use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

#[derive(Debug)]
pub enum VfsRequest {
    Read {
        id: RequestId,
        path: PathBuf,
    },
    ReadScoped {
        id: RequestId,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    },
    ReadScopedBounded {
        id: RequestId,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    },
    ResolveCreate {
        id: RequestId,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    },
    ResolveResource {
        id: RequestId,
        source: PathBuf,
        target: Option<PathBuf>,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: usize,
    },
    ApplyResource {
        id: RequestId,
        intent: CheckedResourceIntent,
    },
    Write {
        id: RequestId,
        path: PathBuf,
        bytes: Vec<u8>,
        expectation: WriteExpectation,
    },
    ListDirectory {
        id: RequestId,
        path: PathBuf,
        maximum_entries: usize,
    },
    DiscoverRecovery {
        id: RequestId,
        directory: PathBuf,
        maximum_records: usize,
    },
    WriteRecovery {
        id: RequestId,
        path: PathBuf,
        record: RecoveryRecord,
    },
    CleanupRecovery {
        id: RequestId,
        directory: PathBuf,
        maximum_records: usize,
        maximum_age_days: u64,
    },
    Delete {
        id: RequestId,
        path: PathBuf,
    },
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceIntentKind {
    Rename {
        target: PathBuf,
        parent_device: u64,
        parent_inode: u64,
    },
    Delete,
}
#[derive(Clone, Debug)]
pub struct CheckedResourceIntent {
    pub source: PathBuf,
    pub source_device: u64,
    pub source_inode: u64,
    pub source_bytes: Vec<u8>,
    pub kind: ResourceIntentKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteExpectation {
    Any,
    Missing,
    Exact(Vec<u8>),
}

#[derive(Debug)]
pub enum VfsResult {
    ResourceResolved {
        id: RequestId,
        result: Result<CheckedResourceIntent, VfsError>,
    },
    ResourceApplied {
        id: RequestId,
        intent: CheckedResourceIntent,
        result: Result<(), VfsError>,
    },
    CreateTarget {
        id: RequestId,
        result: Result<PathBuf, VfsError>,
    },
    Read {
        id: RequestId,
        path: PathBuf,
        result: Result<Vec<u8>, VfsError>,
    },
    Write {
        id: RequestId,
        path: PathBuf,
        result: Result<(), VfsError>,
    },
    Directory {
        id: RequestId,
        path: PathBuf,
        result: Result<Vec<PathBuf>, VfsError>,
    },
    RecoveryDiscovered {
        id: RequestId,
        discovery: RecoveryDiscovery,
    },
    RecoveryWritten {
        id: RequestId,
        path: PathBuf,
        result: Result<(), VfsError>,
    },
    RecoveryCleaned {
        id: RequestId,
        cleanup: RecoveryCleanup,
    },
    Deleted {
        id: RequestId,
        path: PathBuf,
        result: Result<(), VfsError>,
    },
}

pub struct VfsWorker {
    sender: SyncSender<VfsRequest>,
    receiver: Receiver<VfsResult>,
    thread: Option<JoinHandle<()>>,
    ids: Arc<dyn crate::clock::IdSource>,
}

impl VfsWorker {
    #[must_use]
    pub fn start(queue_capacity: usize) -> Self {
        Self::start_with_ids(queue_capacity, Arc::new(crate::clock::AtomicIds::new(1)))
    }
    pub fn start_with_ids(queue_capacity: usize, ids: Arc<dyn crate::clock::IdSource>) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let (result_sender, result_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-vfs".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let result = match request {
                        VfsRequest::Read { id, path } => match canonical_path(&path) {
                            Ok(path) => VfsResult::Read {
                                id,
                                result: read_file(&path),
                                path,
                            },
                            Err(error) => VfsResult::Read {
                                id,
                                path,
                                result: Err(error),
                            },
                        },
                        VfsRequest::ReadScoped {
                            id,
                            path,
                            canonical_roots,
                        } => match canonical_path(&path) {
                            Ok(path)
                                if canonical_roots.iter().any(|root| path.starts_with(root)) =>
                            {
                                VfsResult::Read {
                                    id,
                                    result: read_file(&path),
                                    path,
                                }
                            }
                            Ok(path) => VfsResult::Read {
                                id,
                                result: Err(VfsError::OutsideScope(path.clone())),
                                path,
                            },
                            Err(error) => VfsResult::Read {
                                id,
                                path,
                                result: Err(error),
                            },
                        },
                        VfsRequest::ReadScopedBounded {
                            id,
                            path,
                            canonical_roots,
                            maximum_bytes,
                        } => match canonical_path(&path) {
                            Ok(path)
                                if canonical_roots.iter().any(|root| path.starts_with(root)) =>
                            {
                                VfsResult::Read {
                                    id,
                                    result: read_file_with_ceiling(&path, maximum_bytes),
                                    path,
                                }
                            }
                            Ok(path) => VfsResult::Read {
                                id,
                                result: Err(VfsError::OutsideScope(path.clone())),
                                path,
                            },
                            Err(error) => VfsResult::Read {
                                id,
                                path,
                                result: Err(error),
                            },
                        },
                        VfsRequest::ResolveResource {
                            id,
                            source,
                            target,
                            canonical_roots,
                            maximum_bytes,
                        } => VfsResult::ResourceResolved {
                            id,
                            result: resolve_resource_intent(
                                &source,
                                target.as_deref(),
                                &canonical_roots,
                                maximum_bytes,
                            ),
                        },
                        VfsRequest::ApplyResource { id, intent } => {
                            let result = apply_resource_intent(&intent);
                            VfsResult::ResourceApplied { id, intent, result }
                        }
                        VfsRequest::ResolveCreate {
                            id,
                            path,
                            canonical_roots,
                        } => {
                            let result = (|| {
                                let name = path
                                    .file_name()
                                    .ok_or_else(|| VfsError::OutsideScope(path.clone()))?;
                                let parent = canonical_path(
                                    path.parent()
                                        .ok_or_else(|| VfsError::OutsideScope(path.clone()))?,
                                )?;
                                if !canonical_roots.iter().any(|root| parent.starts_with(root)) {
                                    return Err(VfsError::OutsideScope(parent));
                                }
                                let target = parent.join(name);
                                match fs::symlink_metadata(&target) {
                                    Ok(_) => Err(VfsError::Io {
                                        operation: "create target already exists",
                                        path: target,
                                        source: io::Error::new(
                                            io::ErrorKind::AlreadyExists,
                                            "target exists",
                                        ),
                                    }),
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                        Ok(target)
                                    }
                                    Err(source) => Err(VfsError::Io {
                                        operation: "inspect create target",
                                        path: target,
                                        source,
                                    }),
                                }
                            })();
                            VfsResult::CreateTarget { id, result }
                        }
                        VfsRequest::Write {
                            id,
                            path,
                            bytes,
                            expectation,
                        } => VfsResult::Write {
                            id,
                            result: checked_atomic_write(&path, &bytes, &expectation),
                            path,
                        },
                        VfsRequest::ListDirectory {
                            id,
                            path,
                            maximum_entries,
                        } => VfsResult::Directory {
                            id,
                            result: list_directory(&path, maximum_entries),
                            path,
                        },
                        VfsRequest::DiscoverRecovery {
                            id,
                            directory,
                            maximum_records,
                        } => {
                            let mut discovery = discover_recovery(&directory);
                            discovery.records.truncate(maximum_records);
                            VfsResult::RecoveryDiscovered { id, discovery }
                        }
                        VfsRequest::WriteRecovery { id, path, record } => {
                            let result = path
                                .parent()
                                .map_or(Ok(()), fs::create_dir_all)
                                .map_err(|source| VfsError::Io {
                                    operation: "create recovery directory",
                                    path: path.clone(),
                                    source,
                                })
                                .and_then(|()| write_recovery(&path, &record));
                            VfsResult::RecoveryWritten { id, path, result }
                        }
                        VfsRequest::CleanupRecovery {
                            id,
                            directory,
                            maximum_records,
                            maximum_age_days,
                        } => VfsResult::RecoveryCleaned {
                            id,
                            cleanup: cleanup_recovery(
                                &directory,
                                maximum_records,
                                maximum_age_days,
                                crate::clock::system_time(),
                            ),
                        },
                        VfsRequest::Delete { id, path } => {
                            let result = fs::remove_file(&path)
                                .or_else(|error| {
                                    (error.kind() == io::ErrorKind::NotFound)
                                        .then_some(())
                                        .ok_or(error)
                                })
                                .map_err(|source| VfsError::Io {
                                    operation: "delete",
                                    path: path.clone(),
                                    source,
                                });
                            VfsResult::Deleted { id, path, result }
                        }
                        VfsRequest::Shutdown => break,
                    };
                    if result_sender.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("VFS worker thread creation must succeed");
        Self {
            sender: request_sender,
            receiver: result_receiver,
            thread: Some(thread),
            ids,
        }
    }

    pub fn read(&mut self, path: PathBuf) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::Read { id, path })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn read_scoped(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ReadScoped {
                id,
                path,
                canonical_roots,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn read_scoped_bounded(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ReadScopedBounded {
                id,
                path,
                canonical_roots,
                maximum_bytes,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn resolve_resource(
        &mut self,
        source: PathBuf,
        target: Option<PathBuf>,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ResolveResource {
                id,
                source,
                target,
                canonical_roots,
                maximum_bytes,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }
    pub fn apply_resource(&mut self, intent: CheckedResourceIntent) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ApplyResource { id, intent })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn resolve_create(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ResolveCreate {
                id,
                path,
                canonical_roots,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn write(&mut self, path: PathBuf, bytes: Vec<u8>) -> Result<RequestId, VfsError> {
        self.write_checked(path, bytes, WriteExpectation::Any)
    }

    pub fn write_checked(
        &mut self,
        path: PathBuf,
        bytes: Vec<u8>,
        expectation: WriteExpectation,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::Write {
                id,
                path,
                bytes,
                expectation,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn list_directory(
        &mut self,
        path: PathBuf,
        maximum_entries: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::ListDirectory {
                id,
                path,
                maximum_entries,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn discover_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::DiscoverRecovery {
                id,
                directory,
                maximum_records,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn write_recovery(
        &mut self,
        path: PathBuf,
        record: RecoveryRecord,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::WriteRecovery { id, path, record })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn cleanup_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
        maximum_age_days: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::CleanupRecovery {
                id,
                directory,
                maximum_records,
                maximum_age_days,
            })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn delete(&mut self, path: PathBuf) -> Result<RequestId, VfsError> {
        let id = self.next_request_id();
        self.sender
            .try_send(VfsRequest::Delete { id, path })
            .map_err(map_send_error)?;
        Ok(id)
    }

    pub fn try_result(&self) -> Result<Option<VfsResult>, VfsError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(VfsError::WorkerStopped),
        }
    }

    fn next_request_id(&self) -> RequestId {
        RequestId(self.ids.next())
    }
}

pub(super) fn map_send_error(error: TrySendError<VfsRequest>) -> VfsError {
    match error {
        TrySendError::Full(_) => VfsError::QueueFull,
        TrySendError::Disconnected(_) => VfsError::WorkerStopped,
    }
}

impl Drop for VfsWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(VfsRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn resolve_resource_intent(
    source: &Path,
    target: Option<&Path>,
    roots: &[PathBuf],
    maximum_bytes: usize,
) -> Result<CheckedResourceIntent, VfsError> {
    use std::os::unix::fs::MetadataExt;
    let source = canonical_path(source)?;
    if !roots.iter().any(|root| source.starts_with(root)) {
        return Err(VfsError::OutsideScope(source));
    }
    let metadata = fs::metadata(&source).map_err(|source_error| VfsError::Io {
        operation: "inspect resource source",
        path: source.clone(),
        source: source_error,
    })?;
    if !metadata.is_file() || metadata.len() > maximum_bytes as u64 {
        return Err(VfsError::NotRegularFile(source));
    }
    let bytes = read_file(&source)?;
    let kind = if let Some(target) = target {
        let name = target
            .file_name()
            .ok_or_else(|| VfsError::OutsideScope(target.into()))?;
        let parent = canonical_path(
            target
                .parent()
                .ok_or_else(|| VfsError::OutsideScope(target.into()))?,
        )?;
        if !roots.iter().any(|root| parent.starts_with(root)) {
            return Err(VfsError::OutsideScope(parent));
        }
        let parent_metadata = fs::metadata(&parent).map_err(|source_error| VfsError::Io {
            operation: "inspect resource parent",
            path: parent.clone(),
            source: source_error,
        })?;
        if metadata.dev() != parent_metadata.dev() {
            return Err(VfsError::CrossFilesystem);
        }
        let target = parent.join(name);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(VfsError::Io {
                operation: "resource target already exists",
                path: target,
                source: io::Error::new(io::ErrorKind::AlreadyExists, "target exists"),
            });
        }
        ResourceIntentKind::Rename {
            target,
            parent_device: parent_metadata.dev(),
            parent_inode: parent_metadata.ino(),
        }
    } else {
        ResourceIntentKind::Delete
    };
    Ok(CheckedResourceIntent {
        source,
        source_device: metadata.dev(),
        source_inode: metadata.ino(),
        source_bytes: bytes,
        kind,
    })
}
pub(super) fn apply_resource_intent(intent: &CheckedResourceIntent) -> Result<(), VfsError> {
    use std::os::unix::fs::MetadataExt;
    let source = canonical_path(&intent.source)?;
    if source != intent.source {
        return Err(VfsError::ChangedOnDisk);
    }
    let metadata = fs::metadata(&source).map_err(|source_error| VfsError::Io {
        operation: "revalidate resource",
        path: source.clone(),
        source: source_error,
    })?;
    if metadata.dev() != intent.source_device
        || metadata.ino() != intent.source_inode
        || read_file(&source)? != intent.source_bytes
    {
        return Err(VfsError::ChangedOnDisk);
    }
    match &intent.kind {
        ResourceIntentKind::Delete => {
            fs::remove_file(&source).map_err(|source_error| VfsError::Io {
                operation: "delete resource",
                path: source,
                source: source_error,
            })
        }
        ResourceIntentKind::Rename {
            target,
            parent_device,
            parent_inode,
        } => {
            use std::os::unix::fs::MetadataExt;
            let parent = canonical_path(
                target
                    .parent()
                    .ok_or_else(|| VfsError::OutsideScope(target.clone()))?,
            )?;
            let parent_metadata = fs::metadata(&parent).map_err(|source_error| VfsError::Io {
                operation: "revalidate resource parent",
                path: parent.clone(),
                source: source_error,
            })?;
            if parent_metadata.dev() != *parent_device
                || parent_metadata.ino() != *parent_inode
                || fs::symlink_metadata(target).is_ok()
            {
                return Err(VfsError::ChangedOnDisk);
            }
            fs::rename(&source, target).map_err(|source_error| VfsError::Io {
                operation: "rename resource",
                path: source,
                source: source_error,
            })
        }
    }
}

pub(super) fn canonical_path(path: &Path) -> Result<PathBuf, VfsError> {
    fs::canonicalize(path).map_err(|source| VfsError::Io {
        operation: "canonicalize",
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn list_directory(
    path: &Path,
    maximum_entries: usize,
) -> Result<Vec<PathBuf>, VfsError> {
    let entries = fs::read_dir(path).map_err(|source| VfsError::Io {
        operation: "list directory",
        path: path.to_path_buf(),
        source,
    })?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .take(maximum_entries.saturating_add(1))
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(maximum_entries);
    Ok(paths)
}

pub(super) fn read_file_with_ceiling(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, VfsError> {
    let file = fs::File::open(path).map_err(|source| VfsError::Io {
        operation: "open for reading",
        path: path.to_path_buf(),
        source,
    })?;
    read_bounded_bytes(file, maximum_bytes)
        .map_err(|source| VfsError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?
        .ok_or_else(|| VfsError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        })
}

pub fn read_file(path: &Path) -> Result<Vec<u8>, VfsError> {
    let file = fs::File::open(path).map_err(|source| VfsError::Io {
        operation: "open for reading",
        path: path.to_path_buf(),
        source,
    })?;
    read_bounded_bytes(file, DOCUMENT_BYTE_CEILING)
        .map_err(|source| VfsError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?
        .ok_or_else(|| VfsError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes: DOCUMENT_BYTE_CEILING,
        })
}

pub(super) fn checked_atomic_write(
    path: &Path,
    bytes: &[u8],
    expectation: &WriteExpectation,
) -> Result<(), VfsError> {
    match expectation {
        WriteExpectation::Any => {}
        WriteExpectation::Missing => match fs::metadata(path) {
            Ok(_) => return Err(VfsError::DestinationExists(path.to_path_buf())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(VfsError::Io {
                    operation: "inspect destination",
                    path: path.to_path_buf(),
                    source,
                });
            }
        },
        WriteExpectation::Exact(expected) => {
            let actual = read_file(path)?;
            if actual != *expected {
                return Err(VfsError::ExternalModification(path.to_path_buf()));
            }
        }
    }
    atomic_write(path, bytes)
}

/// Write through a same-directory temporary file, sync it, then atomically
/// replace the destination through tempfile's platform-specific persistence.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), VfsError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| VfsError::Io {
        operation: "create temporary file",
        path: parent.to_path_buf(),
        source,
    })?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|source| VfsError::Io {
                operation: "copy permissions",
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary.write_all(bytes).map_err(|source| VfsError::Io {
        operation: "write temporary file",
        path: path.to_path_buf(),
        source,
    })?;
    temporary.flush().map_err(|source| VfsError::Io {
        operation: "flush temporary file",
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| VfsError::Io {
            operation: "sync temporary file",
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| VfsError::Io {
        operation: "replace destination",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_parent(parent: &Path) -> Result<(), VfsError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| VfsError::Io {
            operation: "sync containing directory",
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_parent: &Path) -> Result<(), VfsError> {
    Ok(())
}
