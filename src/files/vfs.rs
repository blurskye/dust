use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(unix)]
use std::fs::File;

use tempfile::NamedTempFile;

use super::{
    error::VfsError,
    explorer_intent::{ExplorerIo, ExplorerIoReply, PreparedExplorerMutation},
    recovery::{
        DOCUMENT_BYTE_CEILING, RecoveryCleanup, RecoveryDiscovery, RecoveryRecord,
        cleanup_recovery, discover_recovery_bounded, read_bounded_bytes, write_recovery,
    },
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

#[derive(Debug)]
pub enum VfsRequest {
    ResolvePaths { id: RequestId, paths: Vec<PathBuf> },
    AgentWrite { id: RequestId, path: PathBuf, root: PathBuf, bytes: Vec<u8> },
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
    ExplorerMutation {
        id: RequestId,
        operation: ExplorerMutation,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
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
pub enum ExplorerMutation {
    Create { path: PathBuf, directory: bool },
    Rename { source: PathBuf, target: PathBuf },
    Copy { source: PathBuf, target: PathBuf },
    Delete { path: PathBuf },
}

impl ExplorerMutation {
    #[must_use]
    pub fn source(&self) -> Option<&Path> {
        match self {
            Self::Rename { source, .. } | Self::Copy { source, .. } => Some(source),
            Self::Delete { path } => Some(path),
            Self::Create { .. } => None,
        }
    }

    #[must_use]
    pub fn target(&self) -> Option<&Path> {
        match self {
            Self::Create { path, .. } => Some(path),
            Self::Rename { target, .. } | Self::Copy { target, .. } => Some(target),
            Self::Delete { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteExpectation {
    Any,
    Missing,
    Exact(Vec<u8>),
}

#[derive(Debug)]
pub enum VfsResult {
    PathsResolved { id: RequestId, paths: Vec<PathBuf> },
    ResourceResolved {
        id: RequestId,
        result: Result<CheckedResourceIntent, VfsError>,
    },
    ResourceApplied {
        id: RequestId,
        intent: CheckedResourceIntent,
        result: Result<(), VfsError>,
    },
    ExplorerMutated {
        id: RequestId,
        operation: ExplorerMutation,
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

impl VfsResult {
    pub(crate) fn request_id(&self) -> RequestId {
        match self {
            Self::PathsResolved { id, .. }
            | Self::ResourceResolved { id, .. }
            | Self::ResourceApplied { id, .. }
            | Self::ExplorerMutated { id, .. }
            | Self::CreateTarget { id, .. }
            | Self::Read { id, .. }
            | Self::Write { id, .. }
            | Self::Directory { id, .. }
            | Self::RecoveryDiscovered { id, .. }
            | Self::RecoveryWritten { id, .. }
            | Self::RecoveryCleaned { id, .. }
            | Self::Deleted { id, .. } => *id,
        }
    }
}

#[derive(Debug)]
pub(crate) enum VfsReply {
    Public(VfsResult),
    Explorer(ExplorerIoReply),
}
struct QueuedRequest {
    read_roots: Option<Vec<crate::workspace::RootIdentity>>,
    explorer: Option<ExplorerIo>,
    document_paths: Option<Vec<PathBuf>>,
    request: VfsRequest,
    state: Arc<std::sync::atomic::AtomicU8>,
}
struct WorkerFinished(Arc<AtomicBool>);
impl Drop for WorkerFinished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl VfsRequest {
    fn id(&self) -> Option<RequestId> {
        match self {
            Self::ResolvePaths { id, .. }
            | Self::AgentWrite { id, .. }
            | Self::Read { id, .. }
            | Self::ReadScoped { id, .. }
            | Self::ReadScopedBounded { id, .. }
            | Self::ResolveCreate { id, .. }
            | Self::ResolveResource { id, .. }
            | Self::ApplyResource { id, .. }
            | Self::ExplorerMutation { id, .. }
            | Self::Write { id, .. }
            | Self::ListDirectory { id, .. }
            | Self::DiscoverRecovery { id, .. }
            | Self::WriteRecovery { id, .. }
            | Self::CleanupRecovery { id, .. }
            | Self::Delete { id, .. } => Some(*id),
            Self::Shutdown => None,
        }
    }
}
#[cfg(test)]
thread_local! { static START_GATE: std::cell::RefCell<Option<Receiver<()>>> = const { std::cell::RefCell::new(None) }; }
pub struct VfsWorker {
    queue_capacity: usize,
    sender: SyncSender<QueuedRequest>,
    stopping: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    requests: std::collections::HashMap<RequestId, (Arc<std::sync::atomic::AtomicU8>, bool)>,
    last_id: Option<u64>,
    receiver: Receiver<VfsReply>,
    thread: Option<JoinHandle<()>>,
    ids: Arc<dyn crate::clock::IdSource>,
}

pub(crate) struct VfsReadAdmission<'a> {
    worker: &'a mut VfsWorker,
    id: RequestId,
}
impl VfsReadAdmission<'_> {
    pub(crate) fn submit(
        self,
        path: PathBuf,
        roots: Vec<crate::workspace::RootIdentity>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        self.worker
            .submit_scoped_read(self.id, path, roots, maximum_bytes)?;
        Ok(self.id)
    }
}
impl VfsWorker {
    pub(crate) fn reserve_read(&mut self) -> Result<VfsReadAdmission<'_>, VfsError> {
        if self.stopping.load(Ordering::Acquire) || self.finished.load(Ordering::Acquire) {
            return Err(VfsError::WorkerStopped);
        }
        // Queued/cancelled-but-not-dequeued requests conservatively occupy slots.
        // Holding &mut self excludes every other producer; only the worker can free space.
        if self
            .requests
            .values()
            .filter(|(state, _)| state.load(Ordering::Acquire) < 2)
            .count()
            >= self.queue_capacity
        {
            return Err(VfsError::QueueFull);
        }
        let id = self.next_request_id()?;
        Ok(VfsReadAdmission { worker: self, id })
    }

    #[cfg(test)]
    pub(crate) fn start_gated_for_test(capacity: usize) -> (Self, mpsc::Sender<()>) {
        let (send, receive) = mpsc::channel();
        START_GATE.with(|gate| *gate.borrow_mut() = Some(receive));
        (Self::start(capacity), send)
    }
    #[must_use]
    pub fn start(queue_capacity: usize) -> Self {
        Self::start_with_ids(queue_capacity, Arc::new(crate::clock::AtomicIds::new(1)))
    }
    pub fn start_with_ids(queue_capacity: usize, ids: Arc<dyn crate::clock::IdSource>) -> Self {
        let (request_sender, request_receiver) =
            mpsc::sync_channel::<QueuedRequest>(queue_capacity.max(1));
        let (result_sender, result_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let worker_stop = stopping.clone();
        let worker_finished = finished.clone();
        #[cfg(test)]
        let gate = START_GATE.with(|gate| gate.borrow_mut().take());
        let thread = thread::Builder::new()
            .name("dust-vfs".into())
            .spawn(move || {
                let _finished = WorkerFinished(worker_finished);
                #[cfg(test)]
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                while let Ok(queued) = request_receiver.recv() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let request = queued.request;
                    let cancelled = queued
                        .state
                        .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                        .is_err();
                    let mut result = if let Some(explorer) = queued.explorer {
                        VfsReply::Explorer(
                            explorer.run(request.id().expect("explorer request ID"), cancelled),
                        )
                    } else if !cancelled && queued.read_roots.is_some() {
                        let roots = queued.read_roots.as_deref().expect("checked scoped read");
                        let VfsRequest::ReadScopedBounded {
                            id,
                            path,
                            maximum_bytes,
                            ..
                        } = request
                        else {
                            unreachable!("private scoped read packet")
                        };
                        let inspected = (|| {
                            let canonical = canonical_path(&path)?;
                            let participating =
                                participating_roots(&[&path], &[&canonical], roots)?;
                            roots_current(&participating)?;
                            let bytes = read_file_with_ceiling(&canonical, maximum_bytes)?;
                            roots_current(&participating)?;
                            Ok((canonical, bytes))
                        })();
                        VfsReply::Public(match inspected {
                            Ok((path, bytes)) => VfsResult::Read {
                                id,
                                path,
                                result: Ok(bytes),
                            },
                            Err(error) => VfsResult::Read {
                                id,
                                path,
                                result: Err(error),
                            },
                        })
                    } else {
                        VfsReply::Public(if cancelled {
                            let error = VfsError::Io {
                                operation: "cancel request",
                                path: PathBuf::new(),
                                source: io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "request cancelled before execution",
                                ),
                            };
                            match request {
                                VfsRequest::Read { id, path }
                                | VfsRequest::ReadScoped { id, path, .. }
                                | VfsRequest::ReadScopedBounded { id, path, .. } => {
                                    VfsResult::Read {
                                        id,
                                        path,
                                        result: Err(error),
                                    }
                                }
                                VfsRequest::Write { id, path, .. } => VfsResult::Write {
                                    id,
                                    path,
                                    result: Err(error),
                                },
                                VfsRequest::ResolveCreate { id, .. } => VfsResult::CreateTarget {
                                    id,
                                    result: Err(error),
                                },
                                VfsRequest::ResolveResource { id, .. } => {
                                    VfsResult::ResourceResolved {
                                        id,
                                        result: Err(error),
                                    }
                                }
                                VfsRequest::ApplyResource { id, intent } => {
                                    VfsResult::ResourceApplied {
                                        id,
                                        intent,
                                        result: Err(error),
                                    }
                                }
                                _ => unreachable!("request kind does not admit cancellation"),
                            }
                        } else {
                            match request {
                                VfsRequest::ResolvePaths { id, paths } => VfsResult::PathsResolved {
                                    id, paths: paths.iter().map(|path| resolve_existing_prefix(path)).collect(),
                                },
                                VfsRequest::AgentWrite { id, path, root, bytes } => {
                                    let result = (|| {
                                        let resolved = resolve_existing_prefix(&path);
                                        let root = canonical_path(&root)?;
                                        if resolved != path || !resolved.starts_with(&root) {
                                            return Err(VfsError::OutsideScope(path.clone()));
                                        }
                                        if path.exists() && !path.is_file() {
                                            return Err(VfsError::Io { operation: "agent write", path: path.clone(), source: io::Error::other("not a regular file") });
                                        }
                                        if let Some(parent) = path.parent() {
                                            fs::create_dir_all(parent).map_err(|source| VfsError::Io { operation: "create directory", path: parent.into(), source })?;
                                        }
                                        atomic_write(&path, &bytes)
                                    })();
                                    VfsResult::Write { id, path, result }
                                }
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
                                        if canonical_roots
                                            .iter()
                                            .any(|root| path.starts_with(root)) =>
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
                                        if canonical_roots
                                            .iter()
                                            .any(|root| path.starts_with(root)) =>
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
                                VfsRequest::ExplorerMutation {
                                    id,
                                    operation,
                                    canonical_roots,
                                    maximum_bytes,
                                } => {
                                    let result = apply_explorer_mutation(
                                        &operation,
                                        &canonical_roots,
                                        maximum_bytes,
                                    );
                                    VfsResult::ExplorerMutated {
                                        id,
                                        operation,
                                        result,
                                    }
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
                                        let parent =
                                            canonical_path(path.parent().ok_or_else(|| {
                                                VfsError::OutsideScope(path.clone())
                                            })?)?;
                                        if !canonical_roots
                                            .iter()
                                            .any(|root| parent.starts_with(root))
                                        {
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
                                            Err(error)
                                                if error.kind() == io::ErrorKind::NotFound =>
                                            {
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
                                } => {
                                    let document = queued.document_paths.is_some();
                                    let result = (|| {
                                        if let Some(paths) = queued.document_paths {
                                            let canonical = fs::canonicalize(&path).ok();
                                            if paths.iter().any(|other| {
                                                other == &path
                                                    || canonical.as_ref().is_some_and(|target| {
                                                        fs::canonicalize(other).ok().as_ref()
                                                            == Some(target)
                                                    })
                                            }) {
                                                return Err(VfsError::DestinationExists(
                                                    path.clone(),
                                                ));
                                            }
                                        }
                                        checked_atomic_write(&path, &bytes, &expectation)
                                    })();
                                    let path = if document && result.is_ok() {
                                        fs::canonicalize(&path).unwrap_or(path)
                                    } else {
                                        path
                                    };
                                    VfsResult::Write { id, path, result }
                                }
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
                                    let discovery = discover_recovery_bounded(
                                        &directory,
                                        maximum_records,
                                        super::recovery::RECOVERY_JOURNAL_BYTE_CEILING,
                                    );
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
                            }
                        })
                    };
                    queued.state.store(3, Ordering::Release);
                    loop {
                        if worker_stop.load(Ordering::Acquire) {
                            return;
                        }
                        match result_sender.try_send(result) {
                            Ok(()) => break,
                            Err(TrySendError::Disconnected(_)) => return,
                            Err(TrySendError::Full(pending)) => {
                                result = pending;
                                thread::park_timeout(Duration::from_millis(1));
                            }
                        }
                    }
                }
            })
            .expect("VFS worker thread creation must succeed");
        Self {
            queue_capacity: queue_capacity.max(1),
            sender: request_sender,
            stopping,
            finished,
            requests: std::collections::HashMap::new(),
            last_id: None,
            receiver: result_receiver,
            thread: Some(thread),
            ids,
        }
    }

    pub fn read(&mut self, path: PathBuf) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::Read { id, path })?;
        Ok(id)
    }

    pub fn read_scoped(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ReadScoped {
            id,
            path,
            canonical_roots,
        })?;
        Ok(id)
    }

    pub fn read_scoped_bounded(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ReadScopedBounded {
            id,
            path,
            canonical_roots,
            maximum_bytes,
        })?;
        Ok(id)
    }

    pub fn resolve_resource(
        &mut self,
        source: PathBuf,
        target: Option<PathBuf>,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ResolveResource {
            id,
            source,
            target,
            canonical_roots,
            maximum_bytes,
        })?;
        Ok(id)
    }
    pub fn apply_resource(&mut self, intent: CheckedResourceIntent) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ApplyResource { id, intent })?;
        Ok(id)
    }

    pub(crate) fn open_explorer(
        &mut self,
        path: PathBuf,
        roots: Vec<crate::workspace::RootIdentity>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit_io(
            VfsRequest::Read {
                id,
                path: path.clone(),
            },
            None,
            Some(ExplorerIo::Open { path, roots }),
        )?;
        Ok(id)
    }
    pub(crate) fn inspect_explorer_rename(
        &mut self,
        path: PathBuf,
        roots: Vec<crate::workspace::RootIdentity>,
        maximum: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit_io(
            VfsRequest::Read {
                id,
                path: path.clone(),
            },
            None,
            Some(ExplorerIo::InspectRename {
                path,
                roots,
                maximum,
            }),
        )?;
        Ok(id)
    }
    pub(crate) fn prepare_explorer_rename(
        &mut self,
        source: crate::files::ExplorerRenameSource,
        target: PathBuf,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit_io(
            VfsRequest::Read {
                id,
                path: source.path().into(),
            },
            None,
            Some(ExplorerIo::PrepareRename { source, target }),
        )?;
        Ok(id)
    }
    pub(crate) fn prepare_explorer(
        &mut self,
        operation: ExplorerMutation,
        roots: Vec<crate::workspace::RootIdentity>,
        maximum: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        let request = VfsRequest::ExplorerMutation {
            id,
            operation: operation.clone(),
            canonical_roots: Vec::new(),
            maximum_bytes: maximum,
        };
        self.submit_io(
            request,
            None,
            Some(ExplorerIo::Prepare {
                operation,
                roots,
                maximum,
            }),
        )?;
        Ok(id)
    }
    pub(crate) fn apply_explorer(
        &mut self,
        intent: PreparedExplorerMutation,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        let request = VfsRequest::ExplorerMutation {
            id,
            operation: intent.operation().clone(),
            canonical_roots: Vec::new(),
            maximum_bytes: 0,
        };
        self.submit_io(request, None, Some(ExplorerIo::Apply(intent)))?;
        Ok(id)
    }

    pub fn explorer_mutate(
        &mut self,
        operation: ExplorerMutation,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ExplorerMutation {
            id,
            operation,
            canonical_roots,
            maximum_bytes,
        })?;
        Ok(id)
    }

    pub fn resolve_create(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ResolveCreate {
            id,
            path,
            canonical_roots,
        })?;
        Ok(id)
    }

    pub(crate) fn resolve_paths(&mut self, paths: Vec<PathBuf>) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ResolvePaths { id, paths })?;
        Ok(id)
    }

    pub(crate) fn agent_write(&mut self, path: PathBuf, root: PathBuf, bytes: Vec<u8>) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::AgentWrite { id, path, root, bytes })?;
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
        let id = self.next_request_id()?;
        self.submit(VfsRequest::Write {
            id,
            path,
            bytes,
            expectation,
        })?;
        Ok(id)
    }

    pub(crate) fn write_document(
        &mut self,
        path: PathBuf,
        bytes: Vec<u8>,
        expectation: WriteExpectation,
        other_paths: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit_document(
            VfsRequest::Write {
                id,
                path,
                bytes,
                expectation,
            },
            Some(other_paths),
        )?;
        Ok(id)
    }
    pub fn list_directory(
        &mut self,
        path: PathBuf,
        maximum_entries: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ListDirectory {
            id,
            path,
            maximum_entries,
        })?;
        Ok(id)
    }

    pub fn discover_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::DiscoverRecovery {
            id,
            directory,
            maximum_records,
        })?;
        Ok(id)
    }

    pub fn write_recovery(
        &mut self,
        path: PathBuf,
        record: RecoveryRecord,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::WriteRecovery { id, path, record })?;
        Ok(id)
    }

    pub fn cleanup_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
        maximum_age_days: u64,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::CleanupRecovery {
            id,
            directory,
            maximum_records,
            maximum_age_days,
        })?;
        Ok(id)
    }

    pub fn delete(&mut self, path: PathBuf) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::Delete { id, path })?;
        Ok(id)
    }

    pub fn try_result(&self) -> Result<Option<VfsResult>, VfsError> {
        match self.try_reply()? {
            Some(VfsReply::Public(result)) => Ok(Some(result)),
            None => Ok(None),
            Some(VfsReply::Explorer(_)) => Err(VfsError::Io {
                operation: "route VFS result",
                path: PathBuf::new(),
                source: io::Error::other("internal explorer result requires its owner route"),
            }),
        }
    }
    pub(crate) fn try_reply(&self) -> Result<Option<VfsReply>, VfsError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(VfsError::WorkerStopped),
        }
    }

    fn next_request_id(&mut self) -> Result<RequestId, VfsError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(VfsError::WorkerStopped);
        }
        let id = self.ids.next();
        if self.last_id.is_some_and(|last| id <= last) {
            return Err(VfsError::Io {
                operation: "allocate request identity",
                path: PathBuf::new(),
                source: io::Error::other("VFS request identity exhausted or not monotonic"),
            });
        }
        self.last_id = Some(id);
        Ok(RequestId(id))
    }
    fn submit(&mut self, request: VfsRequest) -> Result<(), VfsError> {
        self.submit_document(request, None)
    }
    fn submit_document(
        &mut self,
        request: VfsRequest,
        document_paths: Option<Vec<PathBuf>>,
    ) -> Result<(), VfsError> {
        self.submit_io(request, document_paths, None)
    }
    fn submit_io(
        &mut self,
        request: VfsRequest,
        document_paths: Option<Vec<PathBuf>>,
        explorer: Option<ExplorerIo>,
    ) -> Result<(), VfsError> {
        self.submit_packet(request, document_paths, explorer, None)
    }
    fn submit_scoped_read(
        &mut self,
        id: RequestId,
        path: PathBuf,
        roots: Vec<crate::workspace::RootIdentity>,
        maximum_bytes: u64,
    ) -> Result<(), VfsError> {
        self.submit_packet(
            VfsRequest::ReadScopedBounded {
                id,
                path,
                canonical_roots: Vec::new(),
                maximum_bytes,
            },
            None,
            None,
            Some(roots),
        )
    }
    fn submit_packet(
        &mut self,
        request: VfsRequest,
        document_paths: Option<Vec<PathBuf>>,
        explorer: Option<ExplorerIo>,
        read_roots: Option<Vec<crate::workspace::RootIdentity>>,
    ) -> Result<(), VfsError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(VfsError::WorkerStopped);
        }
        self.requests
            .retain(|_, (state, _)| state.load(Ordering::Acquire) != 3);
        let id = request
            .id()
            .expect("ordinary admission has request identity");
        let state = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let cancellable = explorer.is_some()
            || matches!(
                &request,
                VfsRequest::Read { .. }
                    | VfsRequest::ReadScoped { .. }
                    | VfsRequest::ReadScopedBounded { .. }
                    | VfsRequest::Write { .. }
                    | VfsRequest::ResolveCreate { .. }
                    | VfsRequest::ResolveResource { .. }
                    | VfsRequest::ApplyResource { .. }
            );
        match self.sender.try_send(QueuedRequest {
            read_roots,
            explorer,
            document_paths,
            request,
            state: state.clone(),
        }) {
            Ok(()) => {
                self.requests.insert(id, (state, cancellable));
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(VfsError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(VfsError::WorkerStopped),
        }
    }
    /// Cancel a queued read/write or workspace resource request; true only before execution starts.
    /// Other request kinds currently return false.
    /// A running filesystem operation may still finish; its owner must retire
    /// publication independently. Cancellation never rolls back a disk write.
    pub fn cancel(&mut self, id: RequestId) -> bool {
        self.requests.get(&id).is_some_and(|(state, allowed)| {
            *allowed
                && state
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        })
    }
    pub fn begin_shutdown(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.sender.try_send(QueuedRequest {
            read_roots: None,
            explorer: None,
            document_paths: None,
            request: VfsRequest::Shutdown,
            state: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        });
    }
    pub fn has_outstanding(&self) -> bool {
        !self.shutdown_finished()
            && self
                .requests
                .values()
                .any(|(state, _)| state.load(Ordering::Acquire) != 3)
    }
    pub fn shutdown_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}
impl Drop for VfsWorker {
    fn drop(&mut self) {
        self.begin_shutdown();
        self.thread.take();
    }
}

/// Resolve symlinks in the existing prefix of a possibly new file path.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return rest.iter().rev().fold(resolved, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => { rest.push(name.to_owned()); existing = parent; }
            _ => return path.to_path_buf(),
        }
    }
}

pub(crate) fn roots_current(roots: &[crate::workspace::RootIdentity]) -> Result<(), VfsError> {
    let error = |path: &Path, message: String| VfsError::Io {
        operation: "check root identity",
        path: path.into(),
        source: io::Error::other(message),
    };
    if roots.is_empty() {
        return Err(error(
            Path::new(""),
            "read or mutation has no filesystem scope".into(),
        ));
    }
    for root in roots {
        let current = crate::workspace::RootIdentity::capture(&root.canonical_path)
            .map_err(|e| error(&root.canonical_path, e.to_string()))?;
        if root.filesystem_identity.is_none() || &current != root {
            return Err(error(
                &root.canonical_path,
                "workspace filesystem identity changed".into(),
            ));
        }
    }
    Ok(())
}
/// Select the most specific applicable root for both lexical and canonical paths.
/// Retaining the lexical root prevents a replaced root symlink bypassing its identity check.
pub(crate) fn participating_roots(
    requested: &[&Path],
    canonical: &[&Path],
    roots: &[crate::workspace::RootIdentity],
) -> Result<Vec<crate::workspace::RootIdentity>, VfsError> {
    let mut selected = Vec::new();
    for (paths, required) in [(requested, false), (canonical, true)] {
        for path in paths {
            let root = roots
                .iter()
                .filter(|root| path.starts_with(&root.canonical_path))
                .max_by_key(|root| root.canonical_path.components().count());
            match root {
                Some(root) => {
                    if !selected.contains(root) {
                        selected.push(root.clone());
                    }
                }
                None if required => return Err(VfsError::OutsideScope((*path).into())),
                None => {}
            }
        }
    }
    Ok(selected)
}

pub(super) fn scoped_existing_path(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, VfsError> {
    let lexical = fs::symlink_metadata(path).map_err(|source| VfsError::Io {
        operation: "inspect explorer path",
        path: path.to_path_buf(),
        source,
    })?;
    if lexical.file_type().is_symlink() {
        return Err(VfsError::Io {
            operation: "mutate symlink",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "symlinks are not mutable from the explorer",
            ),
        });
    }
    let canonical = canonical_path(path)?;
    if !roots.iter().any(|root| canonical.starts_with(root))
        || roots.iter().any(|root| root == &canonical)
    {
        return Err(VfsError::OutsideScope(canonical));
    }
    Ok(canonical)
}

pub(super) fn scoped_missing_target(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, VfsError> {
    let name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| VfsError::OutsideScope(path.to_path_buf()))?;
    let parent = canonical_path(
        path.parent()
            .ok_or_else(|| VfsError::OutsideScope(path.to_path_buf()))?,
    )?;
    if !roots.iter().any(|root| parent.starts_with(root)) {
        return Err(VfsError::OutsideScope(parent));
    }
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
        Ok(_) => Err(VfsError::DestinationExists(target)),
        Err(source) => Err(VfsError::Io {
            operation: "inspect explorer target",
            path: target,
            source,
        }),
    }
}

pub(super) fn apply_explorer_mutation(
    operation: &ExplorerMutation,
    roots: &[PathBuf],
    maximum_bytes: u64,
) -> Result<(), VfsError> {
    match operation {
        ExplorerMutation::Create { path, directory } => {
            let target = scoped_missing_target(path, roots)?;
            if *directory {
                fs::create_dir(&target).map_err(|source| VfsError::Io {
                    operation: "create explorer directory",
                    path: target,
                    source,
                })
            } else {
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                let file = options.open(&target).map_err(|source| VfsError::Io {
                    operation: "create explorer file",
                    path: target.clone(),
                    source,
                })?;
                file.sync_all().map_err(|source| VfsError::Io {
                    operation: "sync explorer file",
                    path: target,
                    source,
                })
            }
        }
        ExplorerMutation::Rename { source, target } => {
            let source = scoped_existing_path(source, roots)?;
            let target = scoped_missing_target(target, roots)?;
            rename_noreplace(&source, &target)
        }
        ExplorerMutation::Copy { source, target } => {
            let source = scoped_existing_path(source, roots)?;
            let target = scoped_missing_target(target, roots)?;
            let metadata = fs::metadata(&source).map_err(|error| VfsError::Io {
                operation: "inspect explorer copy source",
                path: source.clone(),
                source: error,
            })?;
            if metadata.is_dir() {
                let mut entries = fs::read_dir(&source).map_err(|error| VfsError::Io {
                    operation: "inspect explorer directory",
                    path: source.clone(),
                    source: error,
                })?;
                if entries.next().is_some() {
                    return Err(VfsError::Io {
                        operation: "copy explorer directory",
                        path: source,
                        source: io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "recursive directory copy is not supported; select an empty directory",
                        ),
                    });
                }
                fs::create_dir(&target).map_err(|error| VfsError::Io {
                    operation: "copy explorer directory",
                    path: target,
                    source: error,
                })
            } else if metadata.is_file() {
                if metadata.len() > maximum_bytes {
                    return Err(VfsError::DocumentTooLarge {
                        path: source,
                        maximum_bytes,
                    });
                }
                let mut input = fs::File::open(&source).map_err(|error| VfsError::Io {
                    operation: "open explorer copy source",
                    path: source.clone(),
                    source: error,
                })?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| VfsError::Io {
                        operation: "create explorer copy target",
                        path: target.clone(),
                        source: error,
                    })?;
                let copied = io::copy(
                    &mut std::io::Read::by_ref(&mut input).take(maximum_bytes.saturating_add(1)),
                    &mut output,
                )
                .map_err(|error| VfsError::Io {
                    operation: "copy explorer file",
                    path: source.clone(),
                    source: error,
                });
                match copied {
                    Ok(bytes) if bytes <= maximum_bytes => match output.sync_all() {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            drop(output);
                            let _ = fs::remove_file(&target);
                            Err(VfsError::Io {
                                operation: "sync explorer copy",
                                path: target,
                                source: error,
                            })
                        }
                    },
                    Ok(_) => {
                        drop(output);
                        let _ = fs::remove_file(&target);
                        Err(VfsError::DocumentTooLarge {
                            path: source,
                            maximum_bytes,
                        })
                    }
                    Err(error) => {
                        drop(output);
                        let _ = fs::remove_file(&target);
                        Err(error)
                    }
                }
            } else {
                Err(VfsError::NotRegularFile(source))
            }
        }
        ExplorerMutation::Delete { path } => {
            let path = scoped_existing_path(path, roots)?;
            let metadata = fs::metadata(&path).map_err(|error| VfsError::Io {
                operation: "inspect explorer delete target",
                path: path.clone(),
                source: error,
            })?;
            if metadata.is_dir() {
                fs::remove_dir(&path).map_err(|error| VfsError::Io {
                    operation: "delete explorer directory",
                    path,
                    source: error,
                })
            } else if metadata.is_file() {
                fs::remove_file(&path).map_err(|error| VfsError::Io {
                    operation: "delete explorer file",
                    path,
                    source: error,
                })
            } else {
                Err(VfsError::NotRegularFile(path))
            }
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
    let source = scoped_existing_path(source, roots)?;
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
            if parent_metadata.dev() != *parent_device || parent_metadata.ino() != *parent_inode {
                return Err(VfsError::ChangedOnDisk);
            }
            rename_noreplace(&source, target)
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

fn rename_noreplace(source: &Path, target: &Path) -> Result<(), VfsError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, source, CWD, target, RenameFlags::NOREPLACE).map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            VfsError::DestinationExists(target.to_path_buf())
        } else {
            VfsError::Io {
                operation: "rename",
                path: source.to_path_buf(),
                source: error.into(),
            }
        }
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
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| VfsError::Io {
            operation: "open for reading",
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| VfsError::Io {
        operation: "inspect read target",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(VfsError::NotRegularFile(path.to_path_buf()));
    }
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
    read_file_with_ceiling(path, DOCUMENT_BYTE_CEILING)
}

pub fn checked_atomic_write(
    path: &Path,
    bytes: &[u8],
    expectation: &WriteExpectation,
) -> Result<(), VfsError> {
    let temporary = stage_atomic_write(path, bytes)?;
    commit_atomic_write(path, temporary, expectation)
}

fn commit_atomic_write(
    path: &Path,
    temporary: NamedTempFile,
    expectation: &WriteExpectation,
) -> Result<(), VfsError> {
    match expectation {
        WriteExpectation::Any => {
            temporary.persist(path).map_err(|error| VfsError::Io {
                operation: "replace destination",
                path: path.to_path_buf(),
                source: error.error,
            })?;
        }
        WriteExpectation::Missing => persist_missing(path, temporary)?,
        WriteExpectation::Exact(expected) => {
            // A new file's buffer starts empty, with nothing on disk to compare it with yet.
            let new_file = expected.is_empty()
                && matches!(
                    fs::symlink_metadata(path),
                    Err(error) if error.kind() == io::ErrorKind::NotFound
                );
            if new_file {
                persist_missing(path, temporary)?;
            } else {
                if read_file(path)? != *expected {
                    return Err(VfsError::ExternalModification(path.to_path_buf()));
                }
                replace_exact(path, temporary, expected)?;
            }
        }
    }
    sync_parent(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

fn persist_missing(path: &Path, temporary: NamedTempFile) -> Result<(), VfsError> {
    temporary.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            VfsError::DestinationExists(path.to_path_buf())
        } else {
            VfsError::Io {
                operation: "create destination",
                path: path.to_path_buf(),
                source: error.error,
            }
        }
    })?;
    Ok(())
}

fn replace_exact(path: &Path, temporary: NamedTempFile, expected: &[u8]) -> Result<(), VfsError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    // Keep the displaced entry until its bytes have been checked as well: another
    // writer can replace the destination between final validation and this exchange.
    renameat_with(CWD, temporary.path(), CWD, path, RenameFlags::EXCHANGE).map_err(|source| {
        VfsError::Io {
            operation: "replace destination",
            path: path.to_path_buf(),
            source: source.into(),
        }
    })?;
    if read_file(temporary.path()).is_ok_and(|bytes| bytes == expected) {
        return Ok(());
    }
    let (_, preserved) = temporary.keep().map_err(|error| VfsError::Io {
        operation: "preserve externally modified destination",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_parent(preserved.parent().unwrap_or(Path::new(".")))?;
    Err(VfsError::Io {
        operation: "save",
        path: path.to_path_buf(),
        source: io::Error::other(format!(
            "destination changed during replacement; displaced file preserved at {}",
            preserved.display()
        )),
    })
}

/// Lock a stable sidecar across atomic replacements of the data file.
#[cfg(unix)]
pub(crate) fn lock_file(path: &Path) -> io::Result<nix::fcntl::Flock<File>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock target must be a regular file",
        ));
    }
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, error)| io::Error::from(error))
}

/// Write through a same-directory temporary file, sync it, then atomically
/// replace the destination through tempfile's platform-specific persistence.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), VfsError> {
    checked_atomic_write(path, bytes, &WriteExpectation::Any)
}

fn stage_atomic_write(path: &Path, bytes: &[u8]) -> Result<NamedTempFile, VfsError> {
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
    Ok(temporary)
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    fn result(worker: &VfsWorker) -> VfsResult {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = worker.try_result().unwrap() {
                return result;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn staged_writes_recheck_expectations_and_preserve_displaced_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let temporary = stage_atomic_write(&path, b"editor").unwrap();
        fs::write(&path, b"external").unwrap();
        assert!(matches!(
            commit_atomic_write(&path, temporary, &WriteExpectation::Missing),
            Err(VfsError::DestinationExists(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"external");

        let temporary = stage_atomic_write(&path, b"editor").unwrap();
        fs::write(&path, b"newer").unwrap();
        assert!(matches!(
            commit_atomic_write(
                &path,
                temporary,
                &WriteExpectation::Exact(b"external".to_vec())
            ),
            Err(VfsError::ExternalModification(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"newer");

        // Simulate a replacement after final validation but before the exchange.
        let temporary = stage_atomic_write(&path, b"editor").unwrap();
        let preserved = temporary.path().to_path_buf();
        let error = replace_exact(&path, temporary, b"external").unwrap_err();
        assert!(error.to_string().contains(&preserved.display().to_string()));
        assert_eq!(fs::read(&preserved).unwrap(), b"newer");
        assert_eq!(fs::read(&path).unwrap(), b"editor");

        fs::remove_file(&path).unwrap();
        let dangling = root.path().join("dangling");
        std::os::unix::fs::symlink(&dangling, &path).unwrap();
        assert!(matches!(
            checked_atomic_write(&path, b"editor", &WriteExpectation::Missing),
            Err(VfsError::DestinationExists(_))
        ));
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn rename_noreplace_preserves_a_destination_created_after_validation() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"source").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let target = scoped_missing_target(&target, &roots).unwrap();
        fs::write(&target, b"other writer").unwrap();
        assert!(matches!(
            rename_noreplace(&source, &target),
            Err(VfsError::DestinationExists(_))
        ));
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&target).unwrap(), b"other writer");
        fs::remove_file(&target).unwrap();
        rename_noreplace(&source, &target).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"source");
    }

    #[test]
    fn resource_preparation_rejects_source_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let link = root.path().join("link");
        fs::write(&real, b"keep").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let roots = vec![root.path().to_path_buf()];
        for target in [None, Some(root.path().join("renamed"))] {
            assert!(resolve_resource_intent(&link, target.as_deref(), &roots, 1024).is_err());
        }
        assert_eq!(fs::read(&real).unwrap(), b"keep");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn bounded_worker_read_rejects_fifo_and_keeps_servicing_requests() {
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        let mut worker = VfsWorker::start(2);
        worker
            .read_scoped_bounded(fifo.clone(), vec![root.path().to_path_buf()], 1024)
            .unwrap();
        assert!(matches!(
            result(&worker),
            VfsResult::Read {
                result: Err(VfsError::NotRegularFile(_)),
                ..
            }
        ));
        assert!(matches!(read_file(&fifo), Err(VfsError::NotRegularFile(_))));
        let path = root.path().join("file");
        worker.write(path.clone(), b"saved".to_vec()).unwrap();
        assert!(matches!(
            result(&worker),
            VfsResult::Write { result: Ok(()), .. }
        ));
        assert_eq!(fs::read(path).unwrap(), b"saved");
    }

    #[test]
    fn queued_write_cancellation_is_exact_and_full_admission_never_writes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let (mut worker, gate) = VfsWorker::start_gated_for_test(1);
        let id = worker.write(path.clone(), b"cancelled".to_vec()).unwrap();
        assert!(matches!(
            worker.write(path.clone(), b"full".to_vec()),
            Err(VfsError::QueueFull)
        ));
        assert!(worker.cancel(id));
        assert!(!worker.cancel(id));
        gate.send(()).unwrap();
        assert!(
            matches!(result(&worker),VfsResult::Write {id:actual,result:Err(VfsError::Io {source,..}),..} if actual==id && source.kind()==io::ErrorKind::Interrupted)
        );
        assert!(!path.exists());
        let accepted = worker.write(path.clone(), b"saved".to_vec()).unwrap();
        assert!(matches!(result(&worker),VfsResult::Write{id,result:Ok(()),..} if id==accepted));
        assert_eq!(fs::read(path).unwrap(), b"saved");
    }
    #[test]
    fn non_reader_and_full_queue_shutdown_do_not_join_or_block_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, b"text").unwrap();
        let mut worker = VfsWorker::start(1);
        let admission_deadline = std::time::Instant::now() + Duration::from_secs(2);
        for _ in 0..3 {
            loop {
                assert!(std::time::Instant::now() < admission_deadline);
                match worker.read(path.clone()) {
                    Ok(_) => break,
                    Err(VfsError::QueueFull) => thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("{e}"),
                }
            }
        }
        let start = std::time::Instant::now();
        worker.begin_shutdown();
        assert!(start.elapsed() < Duration::from_millis(50));
        let deadline = start + Duration::from_secs(2);
        while !worker.shutdown_finished() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(worker.read(path), Err(VfsError::WorkerStopped)));
        let (worker, gate) = VfsWorker::start_gated_for_test(1);
        let finished = worker.finished.clone();
        let start = std::time::Instant::now();
        drop(worker);
        assert!(start.elapsed() < Duration::from_millis(50));
        gate.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !finished.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn reused_or_exhausted_request_identity_rejects_before_queue_admission() {
        struct Repeated;
        impl crate::clock::IdSource for Repeated {
            fn next(&self) -> u64 {
                u64::MAX
            }
        }
        let (gate, receive) = mpsc::channel();
        START_GATE.with(|g| *g.borrow_mut() = Some(receive));
        let mut worker = VfsWorker::start_with_ids(2, Arc::new(Repeated));
        worker.read("unused".into()).unwrap();
        assert!(matches!(
            worker.read("unused".into()),
            Err(VfsError::Io {
                operation: "allocate request identity",
                ..
            })
        ));
        worker.begin_shutdown();
        gate.send(()).unwrap();
    }
}
