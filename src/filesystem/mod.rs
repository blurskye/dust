#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::fs::File;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

pub const RECOVERY_VERSION: u32 = 1;
pub const DOCUMENT_BYTE_CEILING: u64 = 256 * 1024 * 1024;
const RECOVERY_JOURNAL_BYTE_CEILING: u64 = 64 * 1024 * 1024;
const RECOVERY_DIRECTORY_ENTRY_CEILING: usize = 10_000;
const RECOVERY_TOML_OVERHEAD_ALLOWANCE: u64 = 1_024;
const RECOVERY_TOML_MAX_ESCAPE_EXPANSION: u64 = 6;
const RECOVERY_PAYLOAD_BYTE_CEILING: u64 = (RECOVERY_JOURNAL_BYTE_CEILING
    - RECOVERY_TOML_OVERHEAD_ALLOWANCE)
    / RECOVERY_TOML_MAX_ESCAPE_EXPANSION;

const fn recovery_journal_size_allowed(bytes: u64) -> bool {
    bytes <= RECOVERY_JOURNAL_BYTE_CEILING
}

const fn recovery_payload_size_allowed(content_bytes: u64, path_bytes: u64) -> bool {
    content_bytes.saturating_add(path_bytes) <= RECOVERY_PAYLOAD_BYTE_CEILING
}

fn read_bounded_bytes(reader: impl Read, maximum_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= maximum_bytes).then_some(bytes))
}

fn read_bounded_utf8(reader: impl Read, maximum_bytes: u64, kind: &str) -> Result<String, String> {
    let bytes = read_bounded_bytes(reader, maximum_bytes)
        .map_err(|error| format!("cannot read {kind}: {error}"))?
        .ok_or_else(|| format!("{kind} exceeds {maximum_bytes} byte safety ceiling"))?;
    String::from_utf8(bytes).map_err(|error| format!("{kind} is not UTF-8: {error}"))
}

fn read_recovery_source(path: &Path) -> Result<String, String> {
    let file =
        fs::File::open(path).map_err(|error| format!("cannot open recovery journal: {error}"))?;
    read_bounded_utf8(file, RECOVERY_JOURNAL_BYTE_CEILING, "recovery journal")
}

fn bounded_recovery_entries<T>(entries: impl IntoIterator<Item = T>) -> (Vec<T>, bool) {
    let mut paths = entries
        .into_iter()
        .take(RECOVERY_DIRECTORY_ENTRY_CEILING.saturating_add(1))
        .collect::<Vec<_>>();
    let truncated = paths.len() > RECOVERY_DIRECTORY_ENTRY_CEILING;
    paths.truncate(RECOVERY_DIRECTORY_ENTRY_CEILING);
    (paths, truncated)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryRecord {
    pub version: u32,
    pub original_path: Option<String>,
    pub revision: u64,
    pub content: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryDiscovery {
    pub records: Vec<(PathBuf, RecoveryRecord)>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryCleanup {
    pub deleted: Vec<PathBuf>,
    pub diagnostics: Vec<String>,
}

impl RecoveryRecord {
    #[must_use]
    pub fn new(original_path: Option<&Path>, revision: u64, content: String) -> Self {
        Self {
            version: RECOVERY_VERSION,
            original_path: original_path.map(|path| path.display().to_string()),
            revision,
            content,
        }
    }
}

pub fn write_recovery(path: &Path, record: &RecoveryRecord) -> Result<(), VfsError> {
    let path_bytes = record.original_path.as_ref().map_or(0, String::len) as u64;
    if !recovery_payload_size_allowed(record.content.len() as u64, path_bytes) {
        return Err(VfsError::Recovery {
            path: path.to_path_buf(),
            reason: format!(
                "payload exceeds {} byte pre-serialization recovery safety ceiling",
                RECOVERY_PAYLOAD_BYTE_CEILING
            ),
        });
    }
    let source = toml::to_string(record).map_err(|source| VfsError::Recovery {
        path: path.to_path_buf(),
        reason: source.to_string(),
    })?;
    if !recovery_journal_size_allowed(source.len() as u64) {
        return Err(VfsError::Recovery {
            path: path.to_path_buf(),
            reason: format!(
                "serialized journal exceeds {} byte recovery safety ceiling",
                RECOVERY_JOURNAL_BYTE_CEILING
            ),
        });
    }
    atomic_write(path, source.as_bytes())
}

#[must_use]
pub fn discover_recovery(directory: &Path) -> RecoveryDiscovery {
    let mut discovery = RecoveryDiscovery::default();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return discovery,
        Err(error) => {
            discovery.diagnostics.push(format!(
                "cannot inspect recovery directory {}: {error}",
                directory.display()
            ));
            return discovery;
        }
    };
    let (scanned, truncated) = bounded_recovery_entries(entries);
    if truncated {
        discovery.diagnostics.push(format!(
            "recovery directory scan stopped at safety ceiling of {RECOVERY_DIRECTORY_ENTRY_CEILING} raw entries"
        ));
    }
    let mut paths = scanned
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let result = read_recovery_source(&path).and_then(|source| {
            toml::from_str::<RecoveryRecord>(&source).map_err(|error| error.to_string())
        });
        match result {
            Ok(record) if record.version == RECOVERY_VERSION => {
                discovery.records.push((path, record));
            }
            Ok(record) => discovery.diagnostics.push(format!(
                "unsupported recovery version {} in {}",
                record.version,
                path.display()
            )),
            Err(error) => discovery.diagnostics.push(format!(
                "invalid recovery journal {}: {error}",
                path.display()
            )),
        }
    }
    discovery
}

/// Removes only valid, current-version journals selected by explicit retention
/// policy. Corrupt and unsupported records remain available for manual
/// inspection and are reported rather than silently discarded.
#[must_use]
pub fn cleanup_recovery(
    directory: &Path,
    maximum_records: usize,
    maximum_age_days: u64,
    now: SystemTime,
) -> RecoveryCleanup {
    let discovery = discover_recovery(directory);
    let mut cleanup = RecoveryCleanup {
        diagnostics: discovery.diagnostics,
        ..RecoveryCleanup::default()
    };
    let maximum_age = (maximum_age_days > 0)
        .then(|| Duration::from_secs(maximum_age_days.saturating_mul(86_400)));
    let mut records = discovery
        .records
        .into_iter()
        .map(|(path, _)| {
            let modified = fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            (path, modified)
        })
        .collect::<Vec<_>>();
    records.sort_by(|(left_path, left_modified), (right_path, right_modified)| {
        right_modified
            .cmp(left_modified)
            .then_with(|| left_path.cmp(right_path))
    });
    for (index, (path, modified)) in records.into_iter().enumerate() {
        let expired = maximum_age.is_some_and(|maximum| {
            modified
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > maximum)
        });
        if !expired && index < maximum_records {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => cleanup.deleted.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => cleanup.diagnostics.push(format!(
                "cannot prune recovery journal {}: {error}",
                path.display()
            )),
        }
    }
    cleanup
}

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

fn map_send_error(error: TrySendError<VfsRequest>) -> VfsError {
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

fn resolve_resource_intent(
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
fn apply_resource_intent(intent: &CheckedResourceIntent) -> Result<(), VfsError> {
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

fn canonical_path(path: &Path) -> Result<PathBuf, VfsError> {
    fs::canonicalize(path).map_err(|source| VfsError::Io {
        operation: "canonicalize",
        path: path.to_path_buf(),
        source,
    })
}

fn list_directory(path: &Path, maximum_entries: usize) -> Result<Vec<PathBuf>, VfsError> {
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

fn read_file_with_ceiling(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, VfsError> {
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

fn checked_atomic_write(
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
fn sync_parent(parent: &Path) -> Result<(), VfsError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| VfsError::Io {
            operation: "sync containing directory",
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), VfsError> {
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryEntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryListing {
    pub generation: u64,
    pub root_revision: u64,
    pub directory: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

#[derive(Default)]
pub struct DirectoryService {
    generation: u64,
    receiver: Option<Receiver<Result<DirectoryListing, VfsError>>>,
}

impl DirectoryService {
    pub fn list(
        &mut self,
        directory: PathBuf,
        canonical_roots: Vec<PathBuf>,
        root_revision: u64,
        include_hidden: bool,
        include_ignored: bool,
        maximum_entries: usize,
    ) -> Result<u64, VfsError> {
        if !(1..=100_000).contains(&maximum_entries) {
            return Err(VfsError::DirectoryLimit);
        }
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_directory = directory.clone();
        thread::Builder::new()
            .name(format!("dust-directory-{generation}"))
            .spawn(move || {
                let result = list_scoped_directory(
                    generation,
                    root_revision,
                    &worker_directory,
                    &canonical_roots,
                    include_hidden,
                    include_ignored,
                    maximum_entries,
                );
                let _ = sender.send(result);
            })
            .map_err(|source| VfsError::Io {
                operation: "start directory worker for",
                path: directory,
                source,
            })?;
        self.receiver = Some(receiver);
        Ok(generation)
    }

    pub fn try_result(&self) -> Option<Result<DirectoryListing, VfsError>> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }
}

fn natural_name_key(value: &str) -> String {
    let lowercase = value.to_lowercase();
    let mut characters = lowercase.chars().peekable();
    let mut key = String::with_capacity(lowercase.len());
    while let Some(character) = characters.next() {
        if character.is_ascii_digit() {
            let mut digits = String::from(character);
            while characters
                .peek()
                .is_some_and(|digit| digit.is_ascii_digit())
            {
                if let Some(digit) = characters.next() {
                    digits.push(digit);
                }
            }
            let significant = digits.trim_start_matches('0');
            let significant = if significant.is_empty() {
                "0"
            } else {
                significant
            };
            key.push('\u{1}');
            key.push_str(&format!("{:09}", significant.len()));
            key.push_str(significant);
            key.push('\u{1}');
            key.push_str(&format!("{:09}", digits.len()));
        } else {
            key.push(character);
        }
    }
    key
}

fn list_scoped_directory(
    generation: u64,
    root_revision: u64,
    directory: &Path,
    roots: &[PathBuf],
    include_hidden: bool,
    include_ignored: bool,
    maximum_entries: usize,
) -> Result<DirectoryListing, VfsError> {
    let directory = canonical_path(directory)?;
    if !roots.iter().any(|root| directory.starts_with(root)) {
        return Err(VfsError::OutsideScope(directory));
    }
    let mut builder = WalkBuilder::new(&directory);
    builder
        .max_depth(Some(1))
        .follow_links(false)
        .hidden(!include_hidden)
        .ignore(!include_ignored)
        .git_ignore(!include_ignored)
        .git_global(!include_ignored)
        .git_exclude(!include_ignored)
        .parents(!include_ignored);
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in builder.build().skip(1) {
        let entry = entry.map_err(|error| VfsError::DirectoryWalk {
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        if entries.len() >= maximum_entries {
            truncated = true;
            break;
        }
        let kind = match entry.file_type() {
            Some(kind) if kind.is_dir() => DirectoryEntryKind::Directory,
            Some(kind) if kind.is_file() => DirectoryEntryKind::File,
            _ => DirectoryEntryKind::Symlink,
        };
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.into_path(),
            kind,
        });
    }
    let mut keyed_entries = entries
        .into_iter()
        .map(|entry| (natural_name_key(&entry.name), entry))
        .collect::<Vec<_>>();
    keyed_entries.sort_by(|(left_key, left), (right_key, right)| {
        let left_rank = usize::from(left.kind != DirectoryEntryKind::Directory);
        let right_rank = usize::from(right.kind != DirectoryEntryKind::Directory);
        left_rank
            .cmp(&right_rank)
            .then_with(|| left_key.cmp(right_key))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
    });
    let entries = keyed_entries.into_iter().map(|(_, entry)| entry).collect();
    Ok(DirectoryListing {
        generation,
        root_revision,
        directory,
        entries,
        truncated,
    })
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExplorerWatchEntryKind {
    Directory,
    File,
    Symlink,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExplorerWatchEntryFingerprint {
    pub name: Vec<u8>,
    pub kind: ExplorerWatchEntryKind,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerDirectoryFingerprint {
    pub directory: PathBuf,
    pub entries: Vec<ExplorerWatchEntryFingerprint>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWatchPolicy {
    pub enabled: bool,
    pub poll_interval: Duration,
    pub debounce: Duration,
    pub maximum_directories: usize,
    pub maximum_entries_per_directory: usize,
    pub maximum_events: usize,
    pub maximum_path_bytes: usize,
    pub queue_capacity: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerWatchGeneration(pub u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWatchBatch {
    pub generation: ExplorerWatchGeneration,
    pub root_revision: u64,
    pub directories: Vec<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplorerWatchEvent {
    Batch(ExplorerWatchBatch),
    Failed {
        generation: ExplorerWatchGeneration,
        root_revision: u64,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ExplorerWatchError {
    #[error("explorer watch policy is invalid")]
    InvalidPolicy,
    #[error("explorer watch set exceeds limits")]
    Limit,
    #[error("explorer watch worker is unavailable")]
    WorkerUnavailable,
}

enum ExplorerWatchCommand {
    Reconcile {
        generation: ExplorerWatchGeneration,
        root_revision: u64,
        roots: Vec<PathBuf>,
        directories: Vec<PathBuf>,
    },
    Cancel,
}
struct ExplorerWatchSet {
    generation: ExplorerWatchGeneration,
    root_revision: u64,
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    fingerprints: BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    pending: BTreeSet<PathBuf>,
    changed_at: Option<std::time::Instant>,
}

#[cfg(unix)]
fn watch_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}
#[cfg(not(unix))]
fn watch_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    name.to_string_lossy().as_bytes().to_vec()
}

pub fn explorer_directory_fingerprint(
    directory: &Path,
    roots: &[PathBuf],
    maximum_entries: usize,
    maximum_path_bytes: usize,
) -> Result<ExplorerDirectoryFingerprint, ExplorerWatchError> {
    if maximum_entries == 0 || maximum_path_bytes == 0 {
        return Err(ExplorerWatchError::InvalidPolicy);
    }
    let metadata =
        fs::symlink_metadata(directory).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ExplorerWatchError::WorkerUnavailable);
    }
    let canonical =
        fs::canonicalize(directory).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(ExplorerWatchError::Limit);
    }
    let mut entries = Vec::new();
    let mut bytes = canonical.as_os_str().len();
    let read = fs::read_dir(&canonical).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    for entry in read {
        let entry = entry.map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        if entries.len() >= maximum_entries {
            return Err(ExplorerWatchError::Limit);
        }
        let name = watch_name_bytes(&entry.file_name());
        bytes = bytes.saturating_add(name.len());
        if bytes > maximum_path_bytes {
            return Err(ExplorerWatchError::Limit);
        }
        let kind = entry
            .file_type()
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        entries.push(ExplorerWatchEntryFingerprint {
            name,
            kind: if kind.is_dir() {
                ExplorerWatchEntryKind::Directory
            } else if kind.is_file() {
                ExplorerWatchEntryKind::File
            } else {
                ExplorerWatchEntryKind::Symlink
            },
        });
    }
    entries.sort();
    Ok(ExplorerDirectoryFingerprint {
        directory: canonical,
        entries,
    })
}

fn collect_explorer_fingerprints(
    directories: &[PathBuf],
    roots: &[PathBuf],
    policy: &ExplorerWatchPolicy,
) -> Result<BTreeMap<PathBuf, ExplorerDirectoryFingerprint>, ExplorerWatchError> {
    let mut snapshots = BTreeMap::new();
    for directory in directories {
        match explorer_directory_fingerprint(
            directory,
            roots,
            policy.maximum_entries_per_directory,
            policy.maximum_path_bytes,
        ) {
            Ok(fingerprint) => {
                snapshots.insert(directory.clone(), fingerprint);
            }
            Err(ExplorerWatchError::WorkerUnavailable) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(snapshots)
}

pub fn diff_explorer_fingerprints(
    previous: &BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    current: &BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    maximum_events: usize,
) -> Result<Vec<PathBuf>, ExplorerWatchError> {
    let mut paths = previous
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths.retain(|path| previous.get(path) != current.get(path));
    if paths.len() > maximum_events {
        return Err(ExplorerWatchError::Limit);
    }
    Ok(paths.into_iter().collect())
}

pub struct ExplorerWatchService {
    policy: ExplorerWatchPolicy,
    sender: Option<SyncSender<ExplorerWatchCommand>>,
    receiver: Option<Receiver<ExplorerWatchEvent>>,
    generation: u64,
    reconcile_count: u64,
}
impl ExplorerWatchService {
    pub fn start(policy: ExplorerWatchPolicy) -> Result<Self, ExplorerWatchError> {
        if policy.maximum_directories == 0
            || policy.maximum_entries_per_directory == 0
            || policy.maximum_events == 0
            || policy.maximum_path_bytes == 0
            || policy.queue_capacity == 0
            || policy.poll_interval.is_zero()
        {
            return Err(ExplorerWatchError::InvalidPolicy);
        }
        if !policy.enabled {
            return Ok(Self {
                policy,
                sender: None,
                receiver: None,
                generation: 0,
                reconcile_count: 0,
            });
        }
        let (sender, commands) = mpsc::sync_channel(policy.queue_capacity);
        let (events, receiver) = mpsc::sync_channel(policy.queue_capacity);
        let worker_policy = policy.clone();
        thread::Builder::new()
            .name("dust-explorer-watch".into())
            .spawn(move || explorer_watch_worker(worker_policy, commands, events))
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        Ok(Self {
            policy,
            sender: Some(sender),
            receiver: Some(receiver),
            generation: 0,
            reconcile_count: 0,
        })
    }
    pub fn reconcile(
        &mut self,
        root_revision: u64,
        roots: Vec<PathBuf>,
        mut directories: Vec<PathBuf>,
    ) -> Result<Option<ExplorerWatchGeneration>, ExplorerWatchError> {
        if !self.policy.enabled {
            return Ok(None);
        }
        directories.sort();
        directories.dedup();
        if directories.len() > self.policy.maximum_directories
            || directories
                .iter()
                .map(|path| path.as_os_str().len())
                .sum::<usize>()
                > self.policy.maximum_path_bytes
        {
            return Err(ExplorerWatchError::Limit);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ExplorerWatchError::Limit)?;
        let generation = ExplorerWatchGeneration(self.generation);
        self.sender
            .as_ref()
            .ok_or(ExplorerWatchError::WorkerUnavailable)?
            .try_send(ExplorerWatchCommand::Reconcile {
                generation,
                root_revision,
                roots,
                directories,
            })
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        self.reconcile_count = self.reconcile_count.saturating_add(1);
        Ok(Some(generation))
    }
    pub fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(ExplorerWatchCommand::Cancel);
        }
        self.reconcile_count = self.reconcile_count.saturating_add(1);
    }
    pub fn poll(&self) -> Vec<ExplorerWatchEvent> {
        let mut events = Vec::new();
        let Some(receiver) = &self.receiver else {
            return events;
        };
        while events.len() < self.policy.maximum_events {
            match receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        events
    }
    #[must_use]
    pub const fn reconcile_count(&self) -> u64 {
        self.reconcile_count
    }
}

fn explorer_watch_worker(
    policy: ExplorerWatchPolicy,
    commands: Receiver<ExplorerWatchCommand>,
    events: SyncSender<ExplorerWatchEvent>,
) {
    let mut active: Option<ExplorerWatchSet> = None;
    loop {
        match commands.recv_timeout(policy.poll_interval) {
            Ok(ExplorerWatchCommand::Cancel) => active = None,
            Ok(ExplorerWatchCommand::Reconcile {
                generation,
                root_revision,
                roots,
                directories,
            }) => {
                let fingerprints = directories
                    .iter()
                    .map(|directory| {
                        explorer_directory_fingerprint(
                            directory,
                            &roots,
                            policy.maximum_entries_per_directory,
                            policy.maximum_path_bytes,
                        )
                        .map(|fingerprint| (directory.clone(), fingerprint))
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>();
                match fingerprints {
                    Ok(fingerprints) => {
                        active = Some(ExplorerWatchSet {
                            generation,
                            root_revision,
                            roots,
                            directories,
                            fingerprints,
                            pending: BTreeSet::new(),
                            changed_at: None,
                        })
                    }
                    Err(_) => {
                        let _ = events.try_send(ExplorerWatchEvent::Failed {
                            generation,
                            root_revision,
                        });
                        active = None
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(set) = active.as_mut() else { continue };
                let current = collect_explorer_fingerprints(&set.directories, &set.roots, &policy);
                let Ok(current) = current else {
                    let _ = events.try_send(ExplorerWatchEvent::Failed {
                        generation: set.generation,
                        root_revision: set.root_revision,
                    });
                    active = None;
                    continue;
                };
                match diff_explorer_fingerprints(&set.fingerprints, &current, policy.maximum_events)
                {
                    Ok(changed) => {
                        set.fingerprints = current;
                        if !changed.is_empty() {
                            set.pending.extend(changed);
                            set.changed_at.get_or_insert_with(std::time::Instant::now);
                        }
                        if !set.pending.is_empty()
                            && set
                                .changed_at
                                .is_some_and(|at| at.elapsed() >= policy.debounce)
                        {
                            let directories = std::mem::take(&mut set.pending)
                                .into_iter()
                                .take(policy.maximum_events)
                                .collect();
                            let _ =
                                events.try_send(ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                                    generation: set.generation,
                                    root_revision: set.root_revision,
                                    directories,
                                }));
                            set.changed_at = None;
                        }
                    }
                    Err(_) => {
                        let _ = events.try_send(ExplorerWatchEvent::Failed {
                            generation: set.generation,
                            root_revision: set.root_revision,
                        });
                        active = None;
                    }
                }
            }
        }
    }
}

const ABSOLUTE_WORKSPACE_SEARCH_FILES: usize = 1_000_000;
const ABSOLUTE_WORKSPACE_SEARCH_RESULTS: usize = 100_000;
const ABSOLUTE_WORKSPACE_SEARCH_FILE_BYTES: u64 = 256 * 1024 * 1024;
const ABSOLUTE_WORKSPACE_SEARCH_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSearchPolicy {
    pub roots: Vec<PathBuf>,
    pub query: String,
    pub regex: bool,
    pub smart_case: bool,
    pub whole_word: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_results: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_preview_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSearchMatch {
    pub id: String,
    pub canonical_path: PathBuf,
    pub relative_path: PathBuf,
    pub root: PathBuf,
    pub range: std::ops::Range<usize>,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub line_text: String,
    pub snapshot: Arc<[u8]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceSearchSummary {
    pub files_considered: usize,
    pub files_searched: usize,
    pub bytes_searched: u64,
    pub skipped_binary: usize,
    pub skipped_too_large: usize,
    pub walk_errors: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceSearchEvent {
    Batch {
        generation: u64,
        matches: Vec<WorkspaceSearchMatch>,
    },
    Complete {
        generation: u64,
        summary: WorkspaceSearchSummary,
    },
    Cancelled {
        generation: u64,
    },
    Failed {
        generation: u64,
        error: String,
    },
}

#[derive(Default)]
pub struct WorkspaceSearchService {
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<WorkspaceSearchEvent>>,
}

impl WorkspaceSearchService {
    pub fn start(&mut self, policy: WorkspaceSearchPolicy) -> Result<u64, WorkspaceSearchError> {
        validate_workspace_search_policy(&policy)?;
        self.cancel();
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::sync_channel(policy.event_queue_capacity);
        thread::Builder::new()
            .name(format!("dust-workspace-search-{generation}"))
            .spawn(move || run_workspace_search(generation, policy, worker_cancellation, sender))
            .map_err(WorkspaceSearchError::Spawn)?;
        self.cancellation = Some(cancellation);
        self.receiver = Some(receiver);
        Ok(generation)
    }

    pub fn cancel(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.store(true, Ordering::Release);
        }
        self.receiver = None;
    }

    pub fn try_event(&self) -> Option<WorkspaceSearchEvent> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for WorkspaceSearchService {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceSearchError {
    #[error("workspace search requires a non-empty query")]
    EmptyQuery,
    #[error("workspace search requires at least one root")]
    NoRoots,
    #[error("workspace search policy exceeds a safety ceiling: {0}")]
    UnsafePolicy(&'static str),
    #[error("failed to start workspace search worker: {0}")]
    Spawn(io::Error),
}

fn validate_workspace_search_policy(
    policy: &WorkspaceSearchPolicy,
) -> Result<(), WorkspaceSearchError> {
    if policy.query.is_empty() {
        return Err(WorkspaceSearchError::EmptyQuery);
    }
    if policy.query.len() > 65_536 {
        return Err(WorkspaceSearchError::UnsafePolicy("query length"));
    }
    if policy.roots.is_empty() {
        return Err(WorkspaceSearchError::NoRoots);
    }
    if policy.maximum_files == 0 || policy.maximum_files > ABSOLUTE_WORKSPACE_SEARCH_FILES {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_files"));
    }
    if policy.maximum_results == 0 || policy.maximum_results > ABSOLUTE_WORKSPACE_SEARCH_RESULTS {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_results"));
    }
    if policy.maximum_file_bytes == 0
        || policy.maximum_file_bytes > ABSOLUTE_WORKSPACE_SEARCH_FILE_BYTES
    {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_file_bytes"));
    }
    if policy.maximum_total_bytes == 0
        || policy.maximum_total_bytes > ABSOLUTE_WORKSPACE_SEARCH_TOTAL_BYTES
    {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_total_bytes"));
    }
    if !(1..=65_536).contains(&policy.maximum_preview_bytes) {
        return Err(WorkspaceSearchError::UnsafePolicy("maximum_preview_bytes"));
    }
    if !(1..=4_096).contains(&policy.batch_size) {
        return Err(WorkspaceSearchError::UnsafePolicy("batch_size"));
    }
    if !(1..=1_024).contains(&policy.event_queue_capacity) {
        return Err(WorkspaceSearchError::UnsafePolicy("event_queue_capacity"));
    }
    Ok(())
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| error.to_string())?);
    }
    builder.build().map_err(|error| error.to_string())
}

fn compile_workspace_pattern(policy: &WorkspaceSearchPolicy) -> Result<Regex, String> {
    let pattern = if policy.regex {
        policy.query.clone()
    } else {
        regex::escape(&policy.query)
    };
    let case_insensitive = policy.smart_case && !policy.query.chars().any(char::is_uppercase);
    RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .unicode(true)
        .build()
        .map_err(|error| error.to_string())
}

fn send_search_event(
    sender: &SyncSender<WorkspaceSearchEvent>,
    cancellation: &AtomicBool,
    mut event: WorkspaceSearchEvent,
) -> bool {
    loop {
        if cancellation.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::yield_now();
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn has_word_boundaries(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].chars().next_back();
    let after = source[end..].chars().next();
    before.is_none_or(|character| !is_word_character(character))
        && after.is_none_or(|character| !is_word_character(character))
}

fn bounded_line_preview(source: &str, start: usize, end: usize, maximum_bytes: usize) -> String {
    let mut preview_end = end.min(start.saturating_add(maximum_bytes));
    while preview_end > start && !source.is_char_boundary(preview_end) {
        preview_end = preview_end.saturating_sub(1);
    }
    source[start..preview_end].to_owned()
}

fn owner_root<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

fn run_workspace_search(
    generation: u64,
    policy: WorkspaceSearchPolicy,
    cancellation: Arc<AtomicBool>,
    sender: SyncSender<WorkspaceSearchEvent>,
) {
    let send_failure = |error: String| {
        let _ = send_search_event(
            &sender,
            &cancellation,
            WorkspaceSearchEvent::Failed { generation, error },
        );
    };
    let pattern = match compile_workspace_pattern(&policy) {
        Ok(pattern) => pattern,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let include = match compile_globs(&policy.include_globs) {
        Ok(globs) => globs,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let exclude = match compile_globs(&policy.exclude_globs) {
        Ok(globs) => globs,
        Err(error) => {
            send_failure(error);
            return;
        }
    };
    let mut roots = policy.roots.clone();
    roots.sort();
    roots.dedup();
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in roots.iter().skip(1) {
        builder.add(root);
    }
    builder
        .follow_links(false)
        .hidden(!policy.include_hidden)
        .ignore(!policy.include_ignored)
        .git_ignore(!policy.include_ignored)
        .git_global(!policy.include_ignored)
        .git_exclude(!policy.include_ignored)
        .parents(!policy.include_ignored);
    let mut summary = WorkspaceSearchSummary::default();
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for entry in builder.build() {
        if cancellation.load(Ordering::Acquire) {
            let _ = send_search_event(
                &sender,
                &cancellation,
                WorkspaceSearchEvent::Cancelled { generation },
            );
            return;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        summary.files_considered = summary.files_considered.saturating_add(1);
        if summary.files_considered > policy.maximum_files {
            summary.truncated = true;
            break;
        }
        let path = entry.into_path();
        let Some(root) = owner_root(&path, &roots) else {
            summary.walk_errors = summary.walk_errors.saturating_add(1);
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if (!policy.include_globs.is_empty() && !include.is_match(relative))
            || exclude.is_match(relative)
        {
            continue;
        }
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    paths.sort();
    let mut matches = Vec::new();
    'files: for path in paths {
        if cancellation.load(Ordering::Acquire) {
            return;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        if metadata.len() > policy.maximum_file_bytes {
            summary.skipped_too_large = summary.skipped_too_large.saturating_add(1);
            continue;
        }
        if summary.bytes_searched.saturating_add(metadata.len()) > policy.maximum_total_bytes {
            summary.truncated = true;
            break;
        }
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        let bytes = match read_bounded_bytes(file, policy.maximum_file_bytes) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                summary.skipped_too_large = summary.skipped_too_large.saturating_add(1);
                continue;
            }
            Err(_) => {
                summary.walk_errors = summary.walk_errors.saturating_add(1);
                continue;
            }
        };
        summary.bytes_searched = summary
            .bytes_searched
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let bytes: Arc<[u8]> = bytes.into();
        if bytes.contains(&0) {
            summary.skipped_binary = summary.skipped_binary.saturating_add(1);
            continue;
        }
        let Ok(text) = crate::document::TextBuffer::from_bytes(&bytes) else {
            summary.skipped_binary = summary.skipped_binary.saturating_add(1);
            continue;
        };
        let source = text.to_string();
        summary.files_searched = summary.files_searched.saturating_add(1);
        let Some(root) = owner_root(&path, &roots) else {
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        for found in pattern.find_iter(&source) {
            if policy.whole_word && !has_word_boundaries(&source, found.start(), found.end()) {
                continue;
            }
            if matches.len() >= policy.maximum_results {
                summary.truncated = true;
                break 'files;
            }
            let line_start = source[..found.start()]
                .rfind('\n')
                .map_or(0, |offset| offset.saturating_add(1));
            let line_end = source[found.end()..]
                .find('\n')
                .map_or(source.len(), |offset| found.end().saturating_add(offset));
            let line = source[..line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let column = found.start().saturating_sub(line_start);
            let end_line_start = source[..found.end()]
                .rfind('\n')
                .map_or(0, |offset| offset.saturating_add(1));
            let end_line = source[..end_line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let end_column = found.end().saturating_sub(end_line_start);
            matches.push(WorkspaceSearchMatch {
                id: format!("{}:{}:{}", path.display(), found.start(), found.end()),
                canonical_path: path.clone(),
                relative_path: relative.clone(),
                root: root.clone(),
                range: found.start()..found.end(),
                line,
                column,
                end_line,
                end_column,
                line_text: bounded_line_preview(
                    &source,
                    line_start,
                    line_end,
                    policy.maximum_preview_bytes,
                ),
                snapshot: Arc::clone(&bytes),
            });
        }
    }
    for batch in matches.chunks(policy.batch_size) {
        if !send_search_event(
            &sender,
            &cancellation,
            WorkspaceSearchEvent::Batch {
                generation,
                matches: batch.to_vec(),
            },
        ) {
            return;
        }
    }
    let _ = send_search_event(
        &sender,
        &cancellation,
        WorkspaceSearchEvent::Complete {
            generation,
            summary,
        },
    );
}

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("failed to {operation} `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("refusing to overwrite externally modified file `{}`; reload or force the write", .0.display())]
    ExternalModification(PathBuf),
    #[error("refusing to overwrite existing save-as destination `{}` without force", .0.display())]
    DestinationExists(PathBuf),
    #[error("path is outside the permitted filesystem scope: `{}`", .0.display())]
    OutsideScope(PathBuf),
    #[error("invalid recovery journal `{}`: {reason}", path.display())]
    Recovery { path: PathBuf, reason: String },
    #[error("refusing to read `{}` because it exceeds the {maximum_bytes} byte safety ceiling", path.display())]
    DocumentTooLarge { path: PathBuf, maximum_bytes: u64 },
    #[error("resource source is not a regular bounded file: `{}`",.0.display())]
    NotRegularFile(PathBuf),
    #[error("resource changed after preview")]
    ChangedOnDisk,
    #[error("cross-filesystem rename is not supported atomically")]
    CrossFilesystem,
    #[error("directory entry limit is outside safe bounds")]
    DirectoryLimit,
    #[error("failed to walk directory `{}`: {reason}", path.display())]
    DirectoryWalk { path: PathBuf, reason: String },
    #[error("filesystem worker queue is full")]
    QueueFull,
    #[error("filesystem worker stopped")]
    WorkerStopped,
}

const ABSOLUTE_WORKSPACE_FILE_PATH_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFilePolicy {
    pub roots: Vec<PathBuf>,
    pub root_revision: u64,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_path_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFileEntry {
    pub id: String,
    pub canonical_path: PathBuf,
    pub relative_path: PathBuf,
    pub root: PathBuf,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceFileSummary {
    pub files: usize,
    pub path_bytes: usize,
    pub truncated: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceFileEvent {
    Batch {
        generation: u64,
        root_revision: u64,
        entries: Vec<WorkspaceFileEntry>,
    },
    Complete {
        generation: u64,
        root_revision: u64,
        summary: WorkspaceFileSummary,
    },
    Cancelled {
        generation: u64,
        root_revision: u64,
    },
    Failed {
        generation: u64,
        root_revision: u64,
        error: String,
    },
}
#[derive(Default)]
pub struct WorkspaceFileService {
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<WorkspaceFileEvent>>,
}
impl WorkspaceFileService {
    pub fn start(&mut self, policy: WorkspaceFilePolicy) -> Result<u64, WorkspaceFileError> {
        validate_workspace_file_policy(&policy)?;
        self.cancel();
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::sync_channel(policy.event_queue_capacity);
        thread::Builder::new()
            .name(format!("dust-workspace-files-{generation}"))
            .spawn(move || run_workspace_files(generation, policy, worker, sender))
            .map_err(WorkspaceFileError::Spawn)?;
        self.cancellation = Some(cancellation);
        self.receiver = Some(receiver);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancellation.take() {
            c.store(true, Ordering::Release)
        }
        self.receiver = None
    }
    pub fn try_event(&self) -> Option<WorkspaceFileEvent> {
        self.receiver.as_ref().and_then(|r| r.try_recv().ok())
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}
impl Drop for WorkspaceFileService {
    fn drop(&mut self) {
        self.cancel()
    }
}
#[derive(Debug, Error)]
pub enum WorkspaceFileError {
    #[error("workspace files require roots")]
    NoRoots,
    #[error("workspace file policy exceeds {0}")]
    Unsafe(&'static str),
    #[error("failed to start workspace file worker: {0}")]
    Spawn(io::Error),
}
fn validate_workspace_file_policy(p: &WorkspaceFilePolicy) -> Result<(), WorkspaceFileError> {
    if p.roots.is_empty() {
        return Err(WorkspaceFileError::NoRoots);
    }
    if !(1..=ABSOLUTE_WORKSPACE_SEARCH_FILES).contains(&p.maximum_files) {
        return Err(WorkspaceFileError::Unsafe("maximum_files"));
    }
    if !(1..=ABSOLUTE_WORKSPACE_FILE_PATH_BYTES).contains(&p.maximum_path_bytes) {
        return Err(WorkspaceFileError::Unsafe("maximum_path_bytes"));
    }
    if !(1..=4096).contains(&p.batch_size) {
        return Err(WorkspaceFileError::Unsafe("batch_size"));
    }
    if !(1..=1024).contains(&p.event_queue_capacity) {
        return Err(WorkspaceFileError::Unsafe("event_queue_capacity"));
    }
    Ok(())
}
fn send_file_event(
    sender: &SyncSender<WorkspaceFileEvent>,
    cancel: &AtomicBool,
    mut event: WorkspaceFileEvent,
) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(v)) => {
                event = v;
                thread::yield_now()
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}
fn run_workspace_files(
    generation: u64,
    policy: WorkspaceFilePolicy,
    cancel: Arc<AtomicBool>,
    sender: SyncSender<WorkspaceFileEvent>,
) {
    let mut roots = policy
        .roots
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        let _ = send_file_event(
            &sender,
            &cancel,
            WorkspaceFileEvent::Failed {
                generation,
                root_revision: policy.root_revision,
                error: "no canonical workspace roots".into(),
            },
        );
        return;
    }
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in roots.iter().skip(1) {
        builder.add(root);
    }
    builder
        .follow_links(false)
        .hidden(!policy.include_hidden)
        .ignore(!policy.include_ignored)
        .git_ignore(!policy.include_ignored)
        .git_global(!policy.include_ignored)
        .git_exclude(!policy.include_ignored)
        .parents(!policy.include_ignored);
    let mut accepted = Vec::new();
    let mut seen = HashSet::new();
    let mut bytes: usize = 0;
    let mut truncated = false;
    for entry in builder.build() {
        if cancel.load(Ordering::Acquire) {
            let _ = send_file_event(
                &sender,
                &cancel,
                WorkspaceFileEvent::Cancelled {
                    generation,
                    root_revision: policy.root_revision,
                },
            );
            return;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(root) = owner_root(&path, &roots).cloned() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(&root).map(Path::to_path_buf) else {
            continue;
        };
        let cost = path
            .as_os_str()
            .len()
            .saturating_add(relative.as_os_str().len());
        if accepted.len() >= policy.maximum_files
            || bytes.saturating_add(cost) > policy.maximum_path_bytes
        {
            truncated = true;
            break;
        }
        bytes += cost;
        accepted.push(WorkspaceFileEntry {
            id: format!("{}:{}", root.display(), relative.display()),
            canonical_path: path,
            relative_path: relative,
            root,
        });
    }
    accepted.sort_by(|a, b| {
        a.root
            .cmp(&b.root)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    for batch in accepted.chunks(policy.batch_size) {
        if !send_file_event(
            &sender,
            &cancel,
            WorkspaceFileEvent::Batch {
                generation,
                root_revision: policy.root_revision,
                entries: batch.to_vec(),
            },
        ) {
            return;
        }
    }
    let _ = send_file_event(
        &sender,
        &cancel,
        WorkspaceFileEvent::Complete {
            generation,
            root_revision: policy.root_revision,
            summary: WorkspaceFileSummary {
                files: accepted.len(),
                path_bytes: bytes,
                truncated,
            },
        },
    );
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    use crate::document::TextBuffer;
    use tempfile::tempdir;

    use super::{
        DirectoryEntryKind, DirectoryService, ExplorerWatchEntryKind, ExplorerWatchError,
        ExplorerWatchPolicy, ExplorerWatchService, RECOVERY_DIRECTORY_ENTRY_CEILING,
        RECOVERY_JOURNAL_BYTE_CEILING, RECOVERY_PAYLOAD_BYTE_CEILING, RecoveryRecord, VfsError,
        VfsResult, VfsWorker, WorkspaceSearchEvent, WorkspaceSearchMatch, WorkspaceSearchPolicy,
        WorkspaceSearchService, WorkspaceSearchSummary, WriteExpectation, atomic_write,
        bounded_recovery_entries, checked_atomic_write, cleanup_recovery,
        diff_explorer_fingerprints, discover_recovery, explorer_directory_fingerprint,
        list_scoped_directory, natural_name_key, read_bounded_bytes, read_bounded_utf8,
        recovery_journal_size_allowed, recovery_payload_size_allowed, write_recovery,
    };

    #[test]
    fn explorer_watch_fingerprints_are_deterministic_metadata_only_and_bounded() {
        let root = tempdir().unwrap();
        let canonical = fs::canonicalize(root.path()).unwrap();
        fs::write(root.path().join("b"), "one").unwrap();
        fs::create_dir(root.path().join("a")).unwrap();
        let first =
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
                .unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        fs::write(
            root.path().join("b"),
            "content changes are intentionally ignored",
        )
        .unwrap();
        let unchanged =
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
                .unwrap();
        assert_eq!(first, unchanged);
        fs::rename(root.path().join("b"), root.path().join("c")).unwrap();
        let renamed =
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
                .unwrap();
        let previous = BTreeMap::from([(canonical.clone(), first)]);
        let current = BTreeMap::from([(canonical.clone(), renamed)]);
        assert_eq!(
            diff_explorer_fingerprints(&previous, &current, 1).unwrap(),
            vec![canonical.clone()]
        );
        assert_eq!(
            diff_explorer_fingerprints(&previous, &current, 0),
            Err(ExplorerWatchError::Limit)
        );
        fs::remove_file(root.path().join("c")).unwrap();
        fs::create_dir(root.path().join("c")).unwrap();
        let type_changed =
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
                .unwrap();
        assert_ne!(current[&canonical], type_changed);
        assert_eq!(
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 1, 1024),
            Err(ExplorerWatchError::Limit)
        );
    }

    #[cfg(unix)]
    #[test]
    fn explorer_watch_preserves_non_utf8_names_and_never_follows_symlinks() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let root = tempdir().unwrap();
        let canonical = fs::canonicalize(root.path()).unwrap();
        let non_utf8 = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
        assert_eq!(super::watch_name_bytes(&non_utf8), vec![b'x', 0xff]);
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("hidden"), "x").unwrap();
        symlink(outside.path(), root.path().join("link")).unwrap();
        let snapshot =
            explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
                .unwrap();
        assert!(snapshot.entries.iter().any(|entry|entry.name==b"link"&&entry.kind==ExplorerWatchEntryKind::Symlink));
        assert!(!snapshot.entries.iter().any(|entry| entry.name == b"hidden"));
    }

    #[test]
    fn disabled_explorer_watch_has_zero_worker_or_snapshot_activity() {
        let policy = ExplorerWatchPolicy {
            enabled: false,
            poll_interval: Duration::from_millis(50),
            debounce: Duration::ZERO,
            maximum_directories: 2,
            maximum_entries_per_directory: 2,
            maximum_events: 2,
            maximum_path_bytes: 256,
            queue_capacity: 1,
        };
        let mut service = ExplorerWatchService::start(policy).unwrap();
        assert_eq!(service.reconcile(1, Vec::new(), Vec::new()).unwrap(), None);
        assert_eq!(service.reconcile_count(), 0);
        assert!(service.poll().is_empty());
    }

    #[test]
    fn atomic_write_replaces_and_preserves_text_format_bytes() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("document.txt");
        fs::write(&path, b"old").expect("seed");
        let buffer = TextBuffer::from_bytes(b"\xEF\xBB\xBFone\r\ntwo\r\n").expect("decode");
        atomic_write(&path, &buffer.to_bytes()).expect("atomic write");
        assert_eq!(
            fs::read(&path).expect("read"),
            b"\xEF\xBB\xBFone\r\ntwo\r\n"
        );
        let names: Vec<_> = fs::read_dir(root.path())
            .expect("directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, vec!["document.txt"]);
    }

    #[test]
    fn checked_write_detects_external_change_and_save_as_collision() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("checked.txt");
        fs::write(&path, b"initial").expect("seed");
        assert!(matches!(
            checked_atomic_write(
                &path,
                b"editor",
                &WriteExpectation::Exact(b"different".to_vec())
            ),
            Err(VfsError::ExternalModification(_))
        ));
        assert_eq!(fs::read(&path).expect("unchanged"), b"initial");
        assert!(matches!(
            checked_atomic_write(&path, b"editor", &WriteExpectation::Missing),
            Err(VfsError::DestinationExists(_))
        ));
        checked_atomic_write(
            &path,
            b"editor",
            &WriteExpectation::Exact(b"initial".to_vec()),
        )
        .expect("matching write");
        assert_eq!(fs::read(path).expect("written"), b"editor");
    }

    #[test]
    fn recovery_journals_are_versioned_atomic_and_corruption_tolerant() {
        let root = tempdir().expect("tempdir");
        let valid = root.path().join("buffer-a.toml");
        let source = root.path().join("source.rs");
        fs::write(&source, b"source stays intact").expect("source");
        let record = RecoveryRecord::new(Some(&source), 42, "unsaved text".into());
        write_recovery(&valid, &record).expect("journal");
        fs::write(root.path().join("broken.toml"), b"not = [valid").expect("broken");
        let discovery = discover_recovery(root.path());
        assert_eq!(discovery.records, vec![(valid, record)]);
        assert_eq!(discovery.diagnostics.len(), 1);
        assert_eq!(fs::read(source).expect("source"), b"source stays intact");
    }

    #[test]
    fn recovery_safety_ceilings_are_testable_without_large_disk_fixtures() {
        assert!(recovery_journal_size_allowed(RECOVERY_JOURNAL_BYTE_CEILING));
        assert!(!recovery_journal_size_allowed(
            RECOVERY_JOURNAL_BYTE_CEILING + 1
        ));
        assert!(recovery_payload_size_allowed(
            RECOVERY_PAYLOAD_BYTE_CEILING,
            0
        ));
        assert!(!recovery_payload_size_allowed(
            RECOVERY_PAYLOAD_BYTE_CEILING,
            1
        ));
        let (paths, truncated) = bounded_recovery_entries(
            (0..=RECOVERY_DIRECTORY_ENTRY_CEILING)
                .map(|index| std::path::PathBuf::from(format!("non-journal-{index}"))),
        );
        assert!(truncated);
        assert_eq!(paths.len(), RECOVERY_DIRECTORY_ENTRY_CEILING);
        assert!(
            paths.iter().all(|path| path.extension().is_none()),
            "raw non-TOML entries consume the same bounded scan budget"
        );
    }

    #[test]
    fn document_reader_accepts_exact_limit_and_rejects_limit_plus_one() {
        assert_eq!(
            read_bounded_bytes(std::io::Cursor::new(b"1234"), 4).expect("read"),
            Some(b"1234".to_vec())
        );
        assert_eq!(
            read_bounded_bytes(std::io::Cursor::new(b"12345"), 4).expect("read"),
            None
        );
    }

    #[test]
    fn recovery_reader_enforces_bytes_and_utf8_from_the_open_stream() {
        let exact = read_bounded_utf8(std::io::Cursor::new(b"1234"), 4, "test journal")
            .expect("exact limit");
        assert_eq!(exact, "1234");
        let overflow = read_bounded_utf8(std::io::Cursor::new(b"12345"), 4, "test journal")
            .expect_err("limit plus one");
        assert_eq!(overflow, "test journal exceeds 4 byte safety ceiling");
        let invalid = read_bounded_utf8(std::io::Cursor::new([0xff]), 4, "test journal")
            .expect_err("invalid UTF-8");
        assert!(invalid.starts_with("test journal is not UTF-8:"));

        let root = tempdir().expect("tempdir");
        let path = root.path().join("invalid.toml");
        fs::write(&path, [0xff]).expect("invalid journal");
        let discovery = discover_recovery(root.path());
        assert!(discovery.records.is_empty());
        assert!(discovery.diagnostics[0].contains("recovery journal is not UTF-8"));
        let cleanup = cleanup_recovery(root.path(), 0, 0, crate::clock::system_time());
        assert!(cleanup.deleted.is_empty());
        assert!(
            path.exists(),
            "invalid journals remain available for inspection"
        );
    }

    #[test]
    fn recovery_cleanup_obeys_count_and_age_without_deleting_corruption() {
        let root = tempdir().expect("tempdir");
        let first = root.path().join("a.toml");
        let second = root.path().join("b.toml");
        let corrupt = root.path().join("corrupt.toml");
        write_recovery(&first, &RecoveryRecord::new(None, 1, "one".into())).expect("first");
        write_recovery(&second, &RecoveryRecord::new(None, 2, "two".into())).expect("second");
        fs::write(&corrupt, "broken=[").expect("corrupt");

        let cleanup = cleanup_recovery(root.path(), 1, 0, crate::clock::system_time());
        assert_eq!(cleanup.deleted.len(), 1);
        assert!(
            corrupt.exists(),
            "corrupt journals require explicit inspection"
        );
        assert_eq!(discover_recovery(root.path()).records.len(), 1);

        let future = crate::clock::system_time() + Duration::from_secs(2 * 86_400);
        let cleanup = cleanup_recovery(root.path(), 10, 1, future);
        assert_eq!(cleanup.deleted.len(), 1, "age policy removes the survivor");
        assert!(corrupt.exists());
    }

    #[test]
    fn recovery_worker_fifo_order_makes_delete_the_revision_barrier() {
        let root = tempdir().expect("tempdir");
        let journal = root.path().join("buffer.toml");
        let mut worker = VfsWorker::start(4);
        worker
            .write_recovery(journal.clone(), RecoveryRecord::new(None, 1, "old".into()))
            .expect("queue old write");
        worker.delete(journal.clone()).expect("queue delete");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut completions = 0;
        while completions < 2 {
            if worker.try_result().expect("worker").is_some() {
                completions += 1;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !journal.exists(),
            "old write followed by delete stays deleted"
        );

        worker.delete(journal.clone()).expect("queue barrier");
        worker
            .write_recovery(journal.clone(), RecoveryRecord::new(None, 2, "new".into()))
            .expect("queue new write");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut completions = 0;
        while completions < 2 {
            if worker.try_result().expect("worker").is_some() {
                completions += 1;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        let recovered = discover_recovery(root.path());
        assert_eq!(recovered.records.len(), 1);
        assert_eq!(recovered.records[0].1.revision, 2);
        assert_eq!(recovered.records[0].1.content, "new");
    }

    fn search_policy(root: &Path, query: &str) -> WorkspaceSearchPolicy {
        WorkspaceSearchPolicy {
            roots: vec![root.to_path_buf()],
            query: query.into(),
            regex: false,
            smart_case: true,
            whole_word: false,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            include_hidden: false,
            include_ignored: false,
            maximum_files: 1_000,
            maximum_results: 1_000,
            maximum_file_bytes: 1024 * 1024,
            maximum_total_bytes: 8 * 1024 * 1024,
            maximum_preview_bytes: 4_096,
            batch_size: 2,
            event_queue_capacity: 2,
        }
    }

    fn collect_search(
        service: &WorkspaceSearchService,
    ) -> (Vec<WorkspaceSearchMatch>, WorkspaceSearchSummary) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut matches = Vec::new();
        loop {
            if let Some(event) = service.try_event() {
                match event {
                    WorkspaceSearchEvent::Batch { matches: batch, .. } => matches.extend(batch),
                    WorkspaceSearchEvent::Complete { summary, .. } => return (matches, summary),
                    WorkspaceSearchEvent::Failed { error, .. } => panic!("search failed: {error}"),
                    WorkspaceSearchEvent::Cancelled { .. } => panic!("search cancelled"),
                }
            }
            assert!(Instant::now() < deadline, "search timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn explorer_natural_key_is_case_folded_numeric_and_stably_zero_aware() {
        let mut names = vec!["file10", "File2", "file01", "file1", "file02"];
        names.sort_by(|left, right| {
            natural_name_key(left)
                .cmp(&natural_name_key(right))
                .then_with(|| left.cmp(right))
        });
        assert_eq!(names, ["file1", "file01", "File2", "file02", "file10"]);
    }

    #[test]
    fn scoped_directory_listing_is_lazy_sorted_hidden_aware_and_bounded() {
        let root = tempdir().expect("root");
        fs::create_dir(root.path().join("z-dir")).expect("dir");
        fs::create_dir(root.path().join("dir10")).expect("dir10");
        fs::create_dir(root.path().join("dir2")).expect("dir2");
        fs::write(root.path().join("a-file"), "a").expect("file");
        fs::write(root.path().join("file10"), "10").expect("file10");
        fs::write(root.path().join("file2"), "2").expect("file2");
        fs::write(root.path().join(".hidden"), "h").expect("hidden");
        let canonical = fs::canonicalize(root.path()).expect("canonical");
        let mut service = DirectoryService::default();
        service
            .list(
                canonical.clone(),
                vec![canonical.clone()],
                7,
                false,
                false,
                10,
            )
            .expect("list");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = service.try_result() {
                let listing = result.expect("listing");
                assert_eq!(listing.root_revision, 7);
                let ordered = listing
                    .entries
                    .iter()
                    .map(|entry| (entry.kind, entry.name.as_str()))
                    .collect::<Vec<_>>();
                assert_eq!(
                    ordered,
                    [
                        (DirectoryEntryKind::Directory, "dir2"),
                        (DirectoryEntryKind::Directory, "dir10"),
                        (DirectoryEntryKind::Directory, "z-dir"),
                        (DirectoryEntryKind::File, "a-file"),
                        (DirectoryEntryKind::File, "file2"),
                        (DirectoryEntryKind::File, "file10"),
                    ]
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[cfg(unix)]
    #[test]
    fn scoped_directory_listing_does_not_traverse_symlink_escape_and_handles_non_utf8_name() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let root = tempdir().expect("root");
        let outside = tempdir().expect("outside");
        symlink(outside.path(), root.path().join("escape")).expect("symlink");
        let invalid = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
        let invalid_created = fs::write(root.path().join(invalid), "x").is_ok();
        let canonical = fs::canonicalize(root.path()).expect("canonical");
        let result = list_scoped_directory(
            1,
            0,
            &canonical,
            std::slice::from_ref(&canonical),
            true,
            true,
            10,
        )
        .expect("list");
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.kind == DirectoryEntryKind::Symlink)
        );
        if invalid_created {
            assert!(result.entries.iter().any(|entry| entry.name.contains('�')));
        }
        assert!(matches!(
            list_scoped_directory(
                2,
                0,
                outside.path(),
                std::slice::from_ref(&canonical),
                true,
                true,
                10
            ),
            Err(VfsError::OutsideScope(_))
        ));
    }

    #[test]
    fn workspace_search_is_unicode_exact_regex_smart_case_and_whole_word() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("a.txt"), "café CAFE cat scatter cat\n").expect("a");
        fs::write(root.path().join("bom.txt"), b"\xef\xbb\xbfcat\r\n").expect("bom");
        let mut service = WorkspaceSearchService::default();
        let mut policy = search_policy(root.path(), "café|cat");
        policy.regex = true;
        policy.whole_word = true;
        service.start(policy).expect("start");
        let (matches, summary) = collect_search(&service);
        assert_eq!(matches.len(), 4);
        assert_eq!(
            &matches[0].line_text[matches[0].column..matches[0].range.end],
            "café"
        );
        let bom = matches
            .iter()
            .find(|item| item.relative_path == Path::new("bom.txt"))
            .expect("BOM match");
        assert_eq!(bom.range, 0..3);
        assert_eq!(summary.files_searched, 2);

        let mut policy = search_policy(root.path(), "CAFE");
        policy.smart_case = true;
        service.start(policy).expect("start");
        let (matches, _) = collect_search(&service);
        assert_eq!(matches.len(), 1, "uppercase smart-case query is sensitive");
    }

    #[test]
    fn workspace_search_obeys_hidden_ignore_globs_binary_and_limits() {
        let root = tempdir().expect("root");
        fs::create_dir(root.path().join(".git")).expect("git");
        fs::write(root.path().join(".gitignore"), "ignored.txt\n").expect("ignore");
        fs::write(root.path().join("visible.rs"), "needle\n").expect("visible");
        fs::write(root.path().join("ignored.txt"), "needle\n").expect("ignored");
        fs::write(root.path().join(".hidden.rs"), "needle\n").expect("hidden");
        fs::write(root.path().join("binary.rs"), b"needle\0binary").expect("binary");
        fs::write(root.path().join("invalid.rs"), b"needle\xff").expect("invalid UTF-8");
        fs::write(root.path().join("other.txt"), "needle\n").expect("other");
        let mut service = WorkspaceSearchService::default();
        let mut policy = search_policy(root.path(), "needle");
        policy.include_globs = vec!["**/*.rs".into()];
        policy.maximum_results = 1;
        service.start(policy).expect("start");
        let (matches, summary) = collect_search(&service);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].relative_path, Path::new("visible.rs"));
        assert_eq!(summary.skipped_binary, 2);
        assert!(!summary.truncated, "the only textual included match fit");

        let mut policy = search_policy(root.path(), "needle");
        policy.include_hidden = true;
        policy.include_ignored = true;
        policy.exclude_globs = vec!["**/*.txt".into()];
        service.start(policy).expect("start");
        let (matches, _) = collect_search(&service);
        assert_eq!(matches.len(), 2);
        assert!(
            matches
                .iter()
                .any(|item| item.relative_path == Path::new(".hidden.rs"))
        );
    }

    #[test]
    fn workspace_search_bounds_picker_preview_text() {
        let root = tempdir().expect("root");
        fs::write(
            root.path().join("long"),
            format!("needle{}", "x".repeat(10_000)),
        )
        .expect("long line");
        let mut service = WorkspaceSearchService::default();
        let mut policy = search_policy(root.path(), "needle");
        policy.maximum_preview_bytes = 16;
        service.start(policy).expect("start");
        let (matches, _) = collect_search(&service);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].line_text.len() <= 16);
    }

    #[test]
    fn workspace_search_obeys_per_file_and_aggregate_byte_ceilings() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("a"), "needle plus").expect("a");
        fs::write(root.path().join("b"), "needle plus").expect("b");
        let mut service = WorkspaceSearchService::default();
        let mut policy = search_policy(root.path(), "needle");
        policy.maximum_file_bytes = 8;
        service.start(policy).expect("start");
        let (matches, summary) = collect_search(&service);
        assert!(matches.is_empty());
        assert_eq!(summary.skipped_too_large, 2);

        let mut policy = search_policy(root.path(), "needle");
        policy.maximum_total_bytes = 12;
        service.start(policy).expect("start");
        let (matches, summary) = collect_search(&service);
        assert_eq!(matches.len(), 1);
        assert!(summary.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_search_does_not_follow_symlink_escapes_and_nested_roots_deduplicate() {
        use std::os::unix::fs::symlink;
        let root = tempdir().expect("root");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested");
        fs::write(nested.join("inside"), "needle").expect("inside");
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("secret"), "needle").expect("secret");
        symlink(outside.path(), root.path().join("escape")).expect("link");
        let mut service = WorkspaceSearchService::default();
        let mut policy = search_policy(root.path(), "needle");
        policy.roots.push(nested.clone());
        service.start(policy).expect("start");
        let (matches, _) = collect_search(&service);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].root, nested);
    }

    #[test]
    fn workspace_search_replacement_cancels_old_generation_and_caps_results() {
        let root = tempdir().expect("root");
        for index in 0..100 {
            fs::write(root.path().join(format!("{index:03}.txt")), "needle needle").expect("file");
        }
        let mut service = WorkspaceSearchService::default();
        let first = service
            .start(search_policy(root.path(), "never"))
            .expect("first");
        let mut policy = search_policy(root.path(), "needle");
        policy.maximum_results = 3;
        let second = service.start(policy).expect("second");
        assert!(second > first);
        let (matches, summary) = collect_search(&service);
        assert_eq!(matches.len(), 3);
        assert!(summary.truncated);
        assert!(matches.windows(2).all(|pair| pair[0].id < pair[1].id));
    }

    #[cfg(unix)]
    #[test]
    fn scoped_read_rejects_symlink_escape_after_canonicalization() {
        use std::os::unix::fs::symlink;
        let root = tempdir().expect("root");
        let outside = tempdir().expect("outside");
        let secret = outside.path().join("secret");
        fs::write(&secret, "secret").expect("secret");
        let link = root.path().join("link");
        symlink(&secret, &link).expect("symlink");
        let mut worker = VfsWorker::start(2);
        let id = worker
            .read_scoped(
                link,
                vec![fs::canonicalize(root.path()).expect("canonical root")],
            )
            .expect("queue");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(VfsResult::Read {
                id: result_id,
                result,
                ..
            }) = worker.try_result().expect("result")
                && result_id == id
            {
                assert!(matches!(result, Err(VfsError::OutsideScope(_))));
                break;
            }
            assert!(Instant::now() < deadline, "scoped read timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn directory_completion_is_bounded_and_runs_on_worker() {
        let root = tempdir().expect("tempdir");
        fs::write(root.path().join("b.txt"), b"").expect("b");
        fs::write(root.path().join("a.txt"), b"").expect("a");
        fs::write(root.path().join("c.txt"), b"").expect("c");
        let mut worker = VfsWorker::start(2);
        let id = worker
            .list_directory(root.path().to_path_buf(), 2)
            .expect("queue list");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(VfsResult::Directory {
                id: result_id,
                result,
                ..
            }) = worker.try_result().expect("worker")
            {
                assert_eq!(result_id, id);
                let paths = result.expect("list");
                assert_eq!(paths.len(), 2);
                assert!(paths[0].ends_with("a.txt"));
                assert!(paths[1].ends_with("b.txt"));
                break;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn worker_reports_write_completion() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("async.txt");
        let mut worker = VfsWorker::start(2);
        let id = worker
            .write(path.clone(), b"saved".to_vec())
            .expect("queue");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(VfsResult::Write {
                id: result_id,
                result,
                ..
            }) = worker.try_result().expect("worker")
            {
                assert_eq!(result_id, id);
                result.expect("write");
                break;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fs::read(path).expect("read"), b"saved");
    }
    #[test]
    fn workspace_files_are_bounded_deterministic_and_ignore_symlink_escapes() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("sub")).unwrap();
        fs::write(root.path().join("b.txt"), "b").unwrap();
        fs::write(root.path().join("sub/a.txt"), "a").unwrap();
        fs::write(root.path().join(".hidden"), "h").unwrap();
        let mut service = super::WorkspaceFileService::default();
        let generation = service
            .start(super::WorkspaceFilePolicy {
                roots: vec![root.path().to_path_buf()],
                root_revision: 7,
                include_hidden: false,
                include_ignored: false,
                maximum_files: 10,
                maximum_path_bytes: 4096,
                batch_size: 1,
                event_queue_capacity: 8,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut entries = Vec::new();
        let summary = loop {
            if let Some(event) = service.try_event() {
                match event {
                    super::WorkspaceFileEvent::Batch {
                        generation: g,
                        root_revision,
                        entries: batch,
                    } => {
                        assert_eq!((g, root_revision), (generation, 7));
                        entries.extend(batch)
                    }
                    super::WorkspaceFileEvent::Complete { summary, .. } => break summary,
                    super::WorkspaceFileEvent::Failed { error, .. } => panic!("{error}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(summary.files, 2);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.relative_path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["b.txt", "sub/a.txt"]
        );
    }
}
