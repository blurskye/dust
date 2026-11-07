//! Private explorer I/O protocol. Public VfsRequest/VfsResult remain unchanged.
use crate::files::ExplorerMutation;
use crate::files::RequestId;
use crate::files::RootIdentity;
use crate::files::VfsError;
use crate::files::vfs::roots_current;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct PreparedExplorerMutation {
    operation: ExplorerMutation,
    roots: Vec<RootIdentity>,
    identities: Vec<(PathBuf, fs::File)>,
    maximum: u64,
}
/// Read-only source inspection retained while the user edits a rename prompt.
#[derive(Clone, Debug)]
pub(crate) struct ExplorerRenameSource {
    inspected: std::sync::Arc<PreparedExplorerMutation>,
    roots: std::sync::Arc<[RootIdentity]>,
}
impl ExplorerRenameSource {
    pub(crate) fn path(&self) -> &Path {
        self.inspected
            .operation
            .source()
            .expect("inspected rename source")
    }
}
impl PreparedExplorerMutation {
    pub(crate) fn operation(&self) -> &ExplorerMutation {
        &self.operation
    }
}
pub(crate) enum ExplorerIo {
    InspectRename {
        path: PathBuf,
        roots: Vec<RootIdentity>,
        maximum: u64,
    },
    PrepareRename {
        source: ExplorerRenameSource,
        target: PathBuf,
    },
    Open {
        path: PathBuf,
        roots: Vec<RootIdentity>,
    },
    Prepare {
        operation: ExplorerMutation,
        roots: Vec<RootIdentity>,
        maximum: u64,
    },
    Apply(PreparedExplorerMutation),
}
#[derive(Debug)]
pub(crate) enum ExplorerIoReply {
    RenameInspected {
        id: RequestId,
        result: Result<ExplorerRenameSource, VfsError>,
    },
    Opened {
        id: RequestId,
        path: PathBuf,
        result: Result<Vec<u8>, VfsError>,
    },
    Prepared {
        id: RequestId,
        result: Result<PreparedExplorerMutation, VfsError>,
    },
    Applied {
        id: RequestId,
        result: Result<(), VfsError>,
    },
}
impl ExplorerIoReply {
    pub(crate) fn id(&self) -> RequestId {
        match self {
            Self::RenameInspected { id, .. }
            | Self::Prepared { id, .. }
            | Self::Applied { id, .. }
            | Self::Opened { id, .. } => *id,
        }
    }
}
fn error(path: &Path, message: impl Into<String>) -> VfsError {
    VfsError::Io {
        operation: "check explorer identity",
        path: path.into(),
        source: io::Error::other(message.into()),
    }
}
fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

fn prepare(
    operation: ExplorerMutation,
    roots: Vec<RootIdentity>,
    maximum: u64,
) -> Result<PreparedExplorerMutation, VfsError> {
    let requested = operation
        .source()
        .into_iter()
        .chain(operation.target())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    let paths: Vec<_> = roots.iter().map(|r| r.canonical_path.clone()).collect();
    let operation = match operation {
        ExplorerMutation::Create { path, directory } => ExplorerMutation::Create {
            path: crate::files::vfs::scoped_missing_target(&path, &paths)?,
            directory,
        },
        ExplorerMutation::Rename { source, target } => ExplorerMutation::Rename {
            source: crate::files::vfs::scoped_existing_path(&source, &paths)?,
            target: crate::files::vfs::scoped_missing_target(&target, &paths)?,
        },
        ExplorerMutation::Copy { source, target } => ExplorerMutation::Copy {
            source: crate::files::vfs::scoped_existing_path(&source, &paths)?,
            target: crate::files::vfs::scoped_missing_target(&target, &paths)?,
        },
        ExplorerMutation::Delete { path } => ExplorerMutation::Delete {
            path: crate::files::vfs::scoped_existing_path(&path, &paths)?,
        },
    };
    let roots = crate::files::vfs::participating_roots(
        &requested.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        &operation
            .source()
            .into_iter()
            .chain(operation.target())
            .collect::<Vec<_>>(),
        &roots,
    )?;
    // Creating a new name may precede the first listing. Observe an unknown
    // root on this worker; this is filesystem scoping, never execution trust.
    let roots = roots
        .into_iter()
        .map(|root| {
            if root.filesystem_identity.is_none()
                && matches!(operation, ExplorerMutation::Create { .. })
            {
                let observed = RootIdentity::capture(&root.canonical_path)
                    .map_err(|e| error(&root.canonical_path, e.to_string()))?;
                if observed.canonical_path != root.canonical_path {
                    return Err(error(&root.canonical_path, "workspace path changed"));
                }
                Ok(observed)
            } else {
                Ok(root)
            }
        })
        .collect::<Result<Vec<_>, VfsError>>()?;
    roots_current(&roots)?;
    let mut watched = roots
        .iter()
        .map(|r| r.canonical_path.clone())
        .collect::<Vec<_>>();
    if let Some(source) = operation.source() {
        watched.push(source.into());
    }
    if let Some(parent) = operation.target().and_then(Path::parent) {
        watched.push(parent.into());
    }
    watched.sort();
    watched.dedup();
    let identities = watched
        .into_iter()
        .map(|path| {
            // Holding the original inode also prevents immediate unlink/recreate inode reuse.
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_PATH | nix::libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|e| error(&path, e.to_string()))?;
            let metadata = file.metadata().map_err(|e| error(&path, e.to_string()))?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(VfsError::NotRegularFile(path));
            }
            Ok((path, file))
        })
        .collect::<Result<_, _>>()?;
    Ok(PreparedExplorerMutation {
        operation,
        roots,
        identities,
        maximum,
    })
}
fn check_identities(intent: &PreparedExplorerMutation) -> Result<(), VfsError> {
    roots_current(&intent.roots)?;
    for (path, original) in &intent.identities {
        let current = fs::symlink_metadata(path).map_err(|e| error(path, e.to_string()))?;
        let original = original
            .metadata()
            .map_err(|e| error(path, e.to_string()))?;
        if current.file_type().is_symlink() || identity(&current) != identity(&original) {
            return Err(error(path, "explorer target filesystem identity changed"));
        }
    }
    Ok(())
}
fn prepare_rename(
    source: ExplorerRenameSource,
    target: PathBuf,
) -> Result<PreparedExplorerMutation, VfsError> {
    check_identities(&source.inspected)?;
    let mut prepared = prepare(
        ExplorerMutation::Rename {
            source: source.path().into(),
            target,
        },
        source.roots.to_vec(),
        source.inspected.maximum,
    )?;
    check_identities(&source.inspected)?;
    // Retain the original open file identities through application, not just a new path lookup.
    for (path, file) in &source.inspected.identities {
        prepared.identities.push((
            path.clone(),
            file.try_clone().map_err(|e| error(path, e.to_string()))?,
        ));
    }
    Ok(prepared)
}
fn apply(intent: PreparedExplorerMutation) -> Result<(), VfsError> {
    check_identities(&intent)?;
    let roots = intent
        .roots
        .iter()
        .map(|r| r.canonical_path.clone())
        .collect::<Vec<_>>();
    crate::files::vfs::apply_explorer_mutation(&intent.operation, &roots, intent.maximum)
}
fn read_scoped_file(path: &Path, roots: &[RootIdentity]) -> Result<Vec<u8>, VfsError> {
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;

    let root = roots
        .iter()
        .filter(|root| path.starts_with(&root.canonical_path))
        .max_by_key(|root| root.canonical_path.components().count())
        .ok_or_else(|| error(path, "file is outside the workspace roots"))?;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(&root.canonical_path)
        .map_err(|e| error(path, e.to_string()))?;
    let metadata = directory
        .metadata()
        .map_err(|e| error(path, e.to_string()))?;
    if root.filesystem_identity != Some(identity(&metadata)) {
        return Err(error(path, "workspace directory identity changed"));
    }
    let relative = path
        .strip_prefix(&root.canonical_path)
        .map_err(|e| error(path, e.to_string()))?;
    let mut file = directory;
    let mut components = relative.components().peekable();
    // Walk from held directory descriptors so intermediate replacements cannot
    // redirect the read through a symlink. No absolute paths or `..` are allowed.
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(error(path, "invalid workspace-relative file path"));
        };
        let flags = if components.peek().is_some() {
            OFlags::PATH | OFlags::DIRECTORY
        } else {
            OFlags::RDONLY | OFlags::NONBLOCK
        } | OFlags::NOFOLLOW
            | OFlags::CLOEXEC;
        file = fs::File::from(
            openat(&file, name, flags, Mode::empty()).map_err(|e| error(path, e.to_string()))?,
        );
    }
    crate::files::vfs::read_open_file_bounded(
        file,
        path,
        crate::files::recovery::DOCUMENT_BYTE_LIMIT,
    )
}

impl ExplorerIo {
    pub(super) fn run(self, id: RequestId, cancelled: bool) -> ExplorerIoReply {
        let cancellation = || VfsError::Io {
            operation: "cancel explorer request",
            path: PathBuf::new(),
            source: io::Error::new(
                io::ErrorKind::Interrupted,
                "request cancelled before execution",
            ),
        };
        match self {
            Self::InspectRename {
                path,
                roots,
                maximum,
            } => ExplorerIoReply::RenameInspected {
                id,
                result: if cancelled {
                    Err(cancellation())
                } else {
                    // Reuse source-only inspection. No Apply request exists for this opaque token.
                    prepare(ExplorerMutation::Delete { path }, roots.clone(), maximum).map(
                        |prepared| ExplorerRenameSource {
                            inspected: std::sync::Arc::new(prepared),
                            roots: roots.into(),
                        },
                    )
                },
            },
            Self::PrepareRename { source, target } => ExplorerIoReply::Prepared {
                id,
                result: if cancelled {
                    Err(cancellation())
                } else {
                    prepare_rename(source, target)
                },
            },
            Self::Open { path, roots } => {
                if cancelled {
                    return ExplorerIoReply::Opened {
                        id,
                        path,
                        result: Err(cancellation()),
                    };
                }
                match crate::files::vfs::canonical_path(&path).and_then(|canonical| {
                    let roots =
                        crate::files::vfs::participating_roots(&[&path], &[&canonical], &roots)?;
                    roots_current(&roots)?;
                    Ok((canonical, roots))
                }) {
                    Ok((path, roots)) => {
                        let result = read_scoped_file(&path, &roots)
                            .and_then(|bytes| roots_current(&roots).map(|()| bytes));
                        ExplorerIoReply::Opened { id, path, result }
                    }
                    Err(error) => ExplorerIoReply::Opened {
                        id,
                        path,
                        result: Err(error),
                    },
                }
            }
            Self::Prepare {
                operation,
                roots,
                maximum,
            } => ExplorerIoReply::Prepared {
                id,
                result: if cancelled {
                    Err(cancellation())
                } else {
                    prepare(operation, roots, maximum)
                },
            },
            Self::Apply(intent) => ExplorerIoReply::Applied {
                id,
                result: if cancelled {
                    Err(cancellation())
                } else {
                    apply(intent)
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::files::VfsReply;
    use crate::files::VfsWorker;
    use crate::files::explorer_intent::*;
    use std::time::Duration;
    use std::time::Instant;
    fn reply(worker: &VfsWorker) -> ExplorerIoReply {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(VfsReply::Explorer(reply)) = worker.try_reply().unwrap() {
                return reply;
            }
            assert!(
                Instant::now() < deadline,
                "explorer worker did not finish its request"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn explorer_read_rejects_intermediate_symlink_replacement_after_canonicalization() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file"), b"inside").unwrap();
        fs::write(outside.path().join("file"), b"outside").unwrap();
        let roots = vec![RootIdentity::capture(root.path()).unwrap()];
        let path = fs::canonicalize(sub.join("file")).unwrap();
        assert_eq!(read_scoped_file(&path, &roots).unwrap(), b"inside");
        fs::rename(&sub, root.path().join("original-sub")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &sub).unwrap();
        assert!(read_scoped_file(&path, &roots).is_err());
    }

    #[test]
    fn explorer_rename_and_delete_do_not_require_source_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, "private").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o0)).unwrap();
        let roots = vec![RootIdentity::capture(root.path()).unwrap()];
        let intent = prepare(
            ExplorerMutation::Rename {
                source: source.clone(),
                target: target.clone(),
            },
            roots.clone(),
            1024,
        )
        .unwrap();
        apply(intent).unwrap();
        assert!(!source.exists());
        assert!(target.exists());
        let intent = prepare(
            ExplorerMutation::Delete {
                path: target.clone(),
            },
            roots,
            1024,
        )
        .unwrap();
        apply(intent).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn participating_roots_isolate_missing_roots_and_refuse_replaced_nested_or_cross_root_targets()
    {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let source = a.join("source");
        fs::write(&source, "source").unwrap();
        let mut roots = vec![
            RootIdentity::capture(&a).unwrap(),
            RootIdentity::capture(&b).unwrap(),
            RootIdentity {
                canonical_path: temp.path().join("missing"),
                filesystem_identity: None,
            },
        ];
        let mut directory = crate::files::DirectoryWorker::default();
        directory
            .list(
                a.clone(),
                roots.iter().map(|r| r.canonical_path.clone()).collect(),
                1,
                true,
                true,
                64,
            )
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        let observed = loop {
            if let Some(result) = directory.poll_result() {
                break result.unwrap().1;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(observed, [roots[0].clone()]);
        let mut worker = VfsWorker::start(2);
        worker.open_explorer(source.clone(), roots.clone()).unwrap();
        assert!(
            matches!(reply(&worker), ExplorerIoReply::Opened { result: Ok(bytes), .. } if bytes == b"source")
        );
        let target = b.join("copy");
        worker
            .prepare_explorer(
                ExplorerMutation::Copy {
                    source: source.clone(),
                    target: target.clone(),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        let ExplorerIoReply::Prepared {
            result: Ok(intent), ..
        } = reply(&worker)
        else {
            panic!("cross-root preparation");
        };
        assert_eq!(intent.roots.len(), 2);
        let parked = temp.path().join("parked-b");
        fs::rename(&b, &parked).unwrap();
        fs::create_dir(&b).unwrap();
        worker.apply_explorer(intent).unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Applied { result: Err(_), .. }
        ));
        assert!(!target.exists());
        assert!(source.exists());
        fs::remove_dir(&b).unwrap();
        fs::rename(&parked, &b).unwrap();
        worker
            .prepare_explorer(
                ExplorerMutation::Copy {
                    source: source.clone(),
                    target: target.clone(),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        let ExplorerIoReply::Prepared {
            result: Ok(intent), ..
        } = reply(&worker)
        else {
            panic!("fresh copy preparation");
        };
        worker.apply_explorer(intent).unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Applied { result: Ok(()), .. }
        ));
        assert_eq!(fs::read(&target).unwrap(), b"source");
        let nested = a.join("nested");
        fs::create_dir(&nested).unwrap();
        roots.push(RootIdentity::capture(&nested).unwrap());
        worker
            .inspect_explorer_rename(source.clone(), roots.clone(), 1024)
            .unwrap();
        let ExplorerIoReply::RenameInspected {
            result: Ok(rename_source),
            ..
        } = reply(&worker)
        else {
            panic!("rename source inspection");
        };
        fs::rename(&nested, a.join("old-nested")).unwrap();
        std::os::unix::fs::symlink(&b, &nested).unwrap();
        worker
            .prepare_explorer_rename(rename_source, nested.join("renamed"))
            .unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Prepared { result: Err(_), .. }
        ));
        assert!(source.exists());
        assert!(!b.join("renamed").exists());
        // Canonicalization points at a valid other root. The invalid lexical
        // participating root must still reject, not fall back to b or ancestor a.
        worker
            .open_explorer(nested.join("copy"), roots.clone())
            .unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Opened { result: Err(_), .. }
        ));
        worker
            .prepare_explorer(
                ExplorerMutation::Delete {
                    path: nested.join("copy"),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Prepared { result: Err(_), .. }
        ));
        assert_eq!(fs::read(&target).unwrap(), b"source");
        worker.open_explorer(source, roots).unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Opened { result: Ok(_), .. }
        ));
        worker.begin_shutdown();
    }
    #[test]
    fn explorer_worker_preserves_replaced_targets_and_cancels_queued_work_exactly() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let parked = root.path().join("original");
        fs::write(&source, "original").unwrap();
        let roots = vec![RootIdentity::capture(root.path()).unwrap()];
        let mut worker = VfsWorker::start(2);
        let id = worker
            .prepare_explorer(
                ExplorerMutation::Delete {
                    path: source.clone(),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        let ExplorerIoReply::Prepared {
            id: got,
            result: Ok(intent),
        } = reply(&worker)
        else {
            panic!("preflight failed");
        };
        assert_eq!(id, got);
        fs::rename(&source, &parked).unwrap();
        fs::write(&source, "replacement").unwrap();
        worker.apply_explorer(intent).unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Applied { result: Err(_), .. }
        ));
        assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
        assert_eq!(fs::read_to_string(&parked).unwrap(), "original");
        worker
            .prepare_explorer(
                ExplorerMutation::Delete {
                    path: source.clone(),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        let ExplorerIoReply::Prepared {
            result: Ok(intent), ..
        } = reply(&worker)
        else {
            panic!("fresh preflight failed");
        };
        worker.apply_explorer(intent).unwrap();
        assert!(matches!(
            reply(&worker),
            ExplorerIoReply::Applied { result: Ok(()), .. }
        ));
        assert!(!source.exists());
        let (mut gated, release) = VfsWorker::start_gated_for_test(1);
        let id = gated
            .prepare_explorer(
                ExplorerMutation::Delete {
                    path: parked.clone(),
                },
                roots.clone(),
                1024,
            )
            .unwrap();
        assert!(matches!(
            gated.open_explorer(parked.clone(), roots.clone()),
            Err(VfsError::QueueFull)
        ));
        assert!(gated.cancel(id));
        release.send(()).unwrap();
        assert!(
            matches!(reply(&gated), ExplorerIoReply::Prepared { id: got, result: Err(_) } if got == id)
        );
        assert!(parked.exists());
        let open = gated.open_explorer(parked.clone(), roots).unwrap();
        assert!(
            matches!(reply(&gated), ExplorerIoReply::Opened { id, result: Ok(bytes), .. } if id == open && bytes == b"original")
        );
        assert!(gated.try_reply().unwrap().is_none());
        gated.begin_shutdown();
        assert!(matches!(
            gated.open_explorer(parked, Vec::new()),
            Err(VfsError::WorkerStopped)
        ));
    }
}
