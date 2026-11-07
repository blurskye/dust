//! Checked background file reads, writes and resource mutations.

use std::fs;
use std::io;
#[cfg(unix)]
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use std::fs::File;

use tempfile::NamedTempFile;

use crate::files::error::VfsError;

use crate::files::explorer_intent::ExplorerIo;

use crate::files::explorer_intent::ExplorerIoReply;

use crate::files::explorer_intent::PreparedExplorerMutation;

use crate::files::recovery::DOCUMENT_BYTE_LIMIT;

use crate::files::recovery::RecoveryCleanup;

use crate::files::recovery::RecoveryDiscovery;

use crate::files::recovery::RecoveryRecord;

use crate::files::recovery::cleanup_recovery;

use crate::files::recovery::discover_recovery_bounded;

use crate::files::recovery::read_bounded_bytes;

use crate::files::recovery::write_recovery;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

#[derive(Debug)]
pub enum VfsRequest {
    WorkspaceFile {
        id: RequestId,
        root: crate::files::RootIdentity,
        operation: super::workspace_access::FileOperation,
    },
    ResolvePaths {
        id: RequestId,
        paths: Vec<PathBuf>,
    },
    AgentWrite {
        id: RequestId,
        path: PathBuf,
        root: PathBuf,
        bytes: Vec<u8>,
    },
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
        prefix: String,
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
        protected: Vec<PathBuf>,
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
    pub canonical_roots: Vec<PathBuf>,
    pub maximum_bytes: u64,
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
    WorkspaceFile {
        id: RequestId,
        result: Result<super::workspace_access::FileValue, VfsError>,
    },
    PathsResolved {
        id: RequestId,
        paths: Vec<PathBuf>,
    },
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
            Self::WorkspaceFile { id, .. }
            | Self::PathsResolved { id, .. }
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
    read_roots: Option<Vec<crate::files::RootIdentity>>,
    explorer: Option<ExplorerIo>,
    document_paths: Option<(Option<PathBuf>, Vec<PathBuf>)>,
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
            Self::WorkspaceFile { id, .. }
            | Self::ResolvePaths { id, .. }
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

pub(crate) struct VfsReadReservation<'a> {
    worker: &'a mut VfsWorker,
    id: RequestId,
}
impl VfsReadReservation<'_> {
    pub(crate) fn submit(
        self,
        path: PathBuf,
        roots: Vec<crate::files::RootIdentity>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        self.worker
            .submit_scoped_read(self.id, path, roots, maximum_bytes)?;
        Ok(self.id)
    }
}
impl VfsWorker {
    pub(crate) fn reserve_read(&mut self) -> Result<VfsReadReservation<'_>, VfsError> {
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
        Ok(VfsReadReservation { worker: self, id })
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
                            let paths = participating
                                .iter()
                                .map(|root| root.canonical_path.clone())
                                .collect::<Vec<_>>();
                            let bytes = read_scoped_file(&canonical, &paths, maximum_bytes)?;
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
                                _ => unreachable!("request kind does not support cancellation"),
                            }
                        } else {
                            match request {
                                VfsRequest::WorkspaceFile {
                                    id,
                                    root,
                                    operation,
                                } => VfsResult::WorkspaceFile {
                                    id,
                                    result: super::workspace_access::execute(&root, operation),
                                },
                                VfsRequest::ResolvePaths { id, paths } => {
                                    VfsResult::PathsResolved {
                                        id,
                                        paths: paths
                                            .iter()
                                            .map(|path| resolve_existing_prefix(path))
                                            .collect(),
                                    }
                                }
                                VfsRequest::AgentWrite {
                                    id,
                                    path,
                                    root,
                                    bytes,
                                } => {
                                    let result = agent_atomic_write(&root, &path, &bytes);
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
                                } => match canonical_path(&path).or_else(|error| {
                                    if matches!(&error, VfsError::Io { source, .. }
                                        if source.kind() == io::ErrorKind::NotFound)
                                    {
                                        scoped_missing_target(&path, &canonical_roots)
                                    } else {
                                        Err(error)
                                    }
                                }) {
                                    Ok(path)
                                        if canonical_roots
                                            .iter()
                                            .any(|root| path.starts_with(root)) =>
                                    {
                                        VfsResult::Read {
                                            id,
                                            result: read_scoped_file(
                                                &path,
                                                &canonical_roots,
                                                DOCUMENT_BYTE_LIMIT,
                                            ),
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
                                } => match canonical_path(&path).or_else(|error| {
                                    if matches!(&error, VfsError::Io { source, .. }
                                        if source.kind() == io::ErrorKind::NotFound)
                                    {
                                        scoped_missing_target(&path, &canonical_roots)
                                    } else {
                                        Err(error)
                                    }
                                }) {
                                    Ok(path)
                                        if canonical_roots
                                            .iter()
                                            .any(|root| path.starts_with(root)) =>
                                    {
                                        VfsResult::Read {
                                            id,
                                            result: read_scoped_file(
                                                &path,
                                                &canonical_roots,
                                                maximum_bytes,
                                            ),
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
                                    let mut path = path;
                                    let result = (|| {
                                        let mut expectation = expectation;
                                        if let Some((source, paths)) = queued.document_paths {
                                            let requested = path.clone();
                                            path =
                                                canonical_write_path(&path).unwrap_or(path.clone());
                                            if paths.iter().any(|other| {
                                                other == &requested
                                                    || other == &path
                                                    || canonical_write_path(other).ok().as_ref()
                                                        == Some(&path)
                                            }) {
                                                return Err(VfsError::DestinationExists(
                                                    path.clone(),
                                                ));
                                            }
                                            let current_file =
                                                source.as_ref().is_some_and(|source| {
                                                    source == &requested
                                                        || source == &path
                                                        || canonical_write_path(source)
                                                            .ok()
                                                            .as_ref()
                                                            == Some(&path)
                                                });
                                            if !current_file
                                                && matches!(expectation, WriteExpectation::Exact(_))
                                            {
                                                expectation = WriteExpectation::Missing;
                                            }
                                        }
                                        checked_atomic_write(&path, &bytes, &expectation)
                                    })();
                                    let path = if document && result.is_ok() {
                                        crate::files::canonicalize(&path).unwrap_or(path)
                                    } else {
                                        path
                                    };
                                    VfsResult::Write { id, path, result }
                                }
                                VfsRequest::ListDirectory {
                                    id,
                                    path,
                                    prefix,
                                    maximum_entries,
                                } => VfsResult::Directory {
                                    id,
                                    result: list_directory(&path, &prefix, maximum_entries),
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
                                        crate::files::recovery::RECOVERY_JOURNAL_BYTE_LIMIT,
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
                                    protected,
                                } => VfsResult::RecoveryCleaned {
                                    id,
                                    cleanup: cleanup_recovery(
                                        &directory,
                                        maximum_records,
                                        maximum_age_days,
                                        crate::clock::system_time(),
                                        &protected,
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
        roots: Vec<crate::files::RootIdentity>,
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
        roots: Vec<crate::files::RootIdentity>,
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
        roots: Vec<crate::files::RootIdentity>,
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

    pub(crate) fn workspace_file(
        &mut self,
        root: crate::files::RootIdentity,
        operation: super::workspace_access::FileOperation,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::WorkspaceFile {
            id,
            root,
            operation,
        })?;
        Ok(id)
    }

    pub(crate) fn resolve_paths(&mut self, paths: Vec<PathBuf>) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ResolvePaths { id, paths })?;
        Ok(id)
    }

    pub(crate) fn agent_write(
        &mut self,
        path: PathBuf,
        root: PathBuf,
        bytes: Vec<u8>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::AgentWrite {
            id,
            path,
            root,
            bytes,
        })?;
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
        source_path: Option<PathBuf>,
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
            Some((source_path, other_paths)),
        )?;
        Ok(id)
    }
    pub fn list_directory(
        &mut self,
        path: PathBuf,
        prefix: String,
        maximum_entries: usize,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::ListDirectory {
            id,
            path,
            prefix,
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
        protected: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        let id = self.next_request_id()?;
        self.submit(VfsRequest::CleanupRecovery {
            id,
            directory,
            maximum_records,
            maximum_age_days,
            protected,
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
        document_paths: Option<(Option<PathBuf>, Vec<PathBuf>)>,
    ) -> Result<(), VfsError> {
        self.submit_io(request, document_paths, None)
    }
    fn submit_io(
        &mut self,
        request: VfsRequest,
        document_paths: Option<(Option<PathBuf>, Vec<PathBuf>)>,
        explorer: Option<ExplorerIo>,
    ) -> Result<(), VfsError> {
        self.submit_packet(request, document_paths, explorer, None)
    }
    fn submit_scoped_read(
        &mut self,
        id: RequestId,
        path: PathBuf,
        roots: Vec<crate::files::RootIdentity>,
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
        document_paths: Option<(Option<PathBuf>, Vec<PathBuf>)>,
        explorer: Option<ExplorerIo>,
        read_roots: Option<Vec<crate::files::RootIdentity>>,
    ) -> Result<(), VfsError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(VfsError::WorkerStopped);
        }
        self.requests
            .retain(|_, (state, _)| state.load(Ordering::Acquire) != 3);
        let id = request.id().expect("queued request has an identity");
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
    /// A running filesystem operation may still finish; its owner must reject stale
    /// results independently. Cancellation never rolls back a disk write.
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
        if let Ok(resolved) = crate::files::canonicalize(existing) {
            return rest
                .iter()
                .rev()
                .fold(resolved, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

pub(crate) fn roots_current(roots: &[crate::files::RootIdentity]) -> Result<(), VfsError> {
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
        let current = crate::files::RootIdentity::capture(&root.canonical_path)
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
    roots: &[crate::files::RootIdentity],
) -> Result<Vec<crate::files::RootIdentity>, VfsError> {
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

pub(crate) fn open_path(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        open_no_symlinks(path, path_open_flags())
    }
    #[cfg(windows)]
    {
        open_windows_path(path, true)
    }
}

#[cfg(windows)]
pub(super) fn open_windows_path(path: &Path, allow_delete: bool) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let mut options = fs::OpenOptions::new();
    // Metadata-only handles can also open directories. Denying delete pins a parent
    // while a scoped operation resolves and changes names beneath it.
    options
        .access_mode(0)
        .share_mode(if allow_delete { 7 } else { 3 })
        .custom_flags(0x02000000 | 0x00200000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
    let file = options.open(path)?;
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reparse point in scoped path",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn hold_parents(path: &Path) -> io::Result<Vec<File>> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute path",
        ));
    }
    let mut current = PathBuf::new();
    let mut held = Vec::new();
    for component in path.parent().unwrap_or(path).components() {
        match component {
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "path escapes its directory",
                ));
            }
            Component::CurDir => continue,
            Component::Prefix(_) | Component::RootDir => {
                current.push(component);
                continue;
            }
            Component::Normal(_) => current.push(component),
        }
        let file = open_windows_path(&current, false)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "scoped parent is not a directory",
            ));
        }
        held.push(file);
    }
    Ok(held)
}

#[cfg(windows)]
pub(super) fn read_scoped_file(
    path: &Path,
    roots: &[PathBuf],
    maximum: u64,
) -> Result<Vec<u8>, VfsError> {
    let _parents = hold_scoped_parents(path, roots)?;
    read_file_bounded(path, maximum)
}

#[cfg(windows)]
pub(super) fn hold_scoped_parents(path: &Path, roots: &[PathBuf]) -> Result<Vec<File>, VfsError> {
    if !roots.iter().any(|root| path.starts_with(root)) {
        return Err(VfsError::OutsideScope(path.into()));
    }
    hold_parents(path).map_err(|source| VfsError::Io {
        operation: "open scoped parents",
        path: path.into(),
        source,
    })
}

#[cfg(windows)]
pub(super) fn apply_explorer_mutation(
    operation: &ExplorerMutation,
    roots: &[PathBuf],
    maximum: u64,
) -> Result<(), VfsError> {
    let mut parents = Vec::new();
    for path in operation.source().into_iter().chain(operation.target()) {
        parents.extend(hold_scoped_parents(path, roots)?);
    }
    let error = |path: &Path, source| VfsError::Io {
        operation: "mutate explorer entry",
        path: path.into(),
        source,
    };
    match operation {
        ExplorerMutation::Create { path, directory } => {
            let path = scoped_missing_target(path, roots)?;
            if *directory {
                fs::create_dir(&path).map_err(|e| error(&path, e))
            } else {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(|e| error(&path, e))
            }
        }
        ExplorerMutation::Rename { source, target } => {
            let source = scoped_existing_path(source, roots)?;
            let target = scoped_missing_target(target, roots)?;
            // Move without replacement. Unlike std::fs::rename, persist_noclobber
            // uses the Windows no-replace move primitive for files.
            tempfile::TempPath::try_from_path(&source)
                .map_err(|e| error(&source, e))?
                .persist_noclobber(&target)
                .map_err(|e| {
                    let source_error = e.error;
                    let _ = e.path.keep();
                    if source_error.kind() == io::ErrorKind::AlreadyExists {
                        VfsError::DestinationExists(target.clone())
                    } else {
                        error(&target, source_error)
                    }
                })?;
            Ok(())
        }
        ExplorerMutation::Copy { source, target } => {
            let source = scoped_existing_path(source, roots)?;
            let target = scoped_missing_target(target, roots)?;
            let metadata = fs::symlink_metadata(&source).map_err(|e| error(&source, e))?;
            if metadata.is_dir() {
                let _directory =
                    open_windows_path(&source, false).map_err(|e| error(&source, e))?;
                if fs::read_dir(&source)
                    .map_err(|e| error(&source, e))?
                    .next()
                    .is_some()
                {
                    return Err(error(
                        &source,
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "recursive directory copy is not supported; select an empty directory",
                        ),
                    ));
                }
                fs::create_dir(&target).map_err(|e| error(&target, e))
            } else {
                let bytes = read_file_bounded(&source, maximum)?;
                checked_atomic_write(&target, &bytes, &WriteExpectation::Missing)
            }
        }
        ExplorerMutation::Delete { path } => {
            let path = scoped_existing_path(path, roots)?;
            let metadata = fs::symlink_metadata(&path).map_err(|e| error(&path, e))?;
            if metadata.is_dir() {
                fs::remove_dir(&path).map_err(|e| error(&path, e))
            } else if metadata.is_file() {
                fs::remove_file(&path).map_err(|e| error(&path, e))
            } else {
                Err(VfsError::NotRegularFile(path))
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn resolve_resource_intent(
    source: &Path,
    target: Option<&Path>,
    roots: &[PathBuf],
    maximum_bytes: usize,
) -> Result<CheckedResourceIntent, VfsError> {
    use crate::files::root_identity::file_identity;
    let source = scoped_existing_path(source, roots)?;
    let _parents = hold_scoped_parents(&source, roots)?;
    let identity = |path: &Path| {
        open_path(path)
            .and_then(|file| file_identity(&file))
            .map_err(|source| VfsError::Io {
                operation: "inspect resource identity",
                path: path.into(),
                source,
            })
    };
    let (source_device, source_inode) = identity(&source)?;
    let source_bytes = read_file_bounded(&source, maximum_bytes as u64)?;
    let kind = if let Some(target) = target {
        let target = scoped_missing_target(target, roots)?;
        let _target_parents = hold_scoped_parents(&target, roots)?;
        let (parent_device, parent_inode) = identity(
            target
                .parent()
                .ok_or_else(|| VfsError::OutsideScope(target.clone()))?,
        )?;
        if parent_device != source_device {
            return Err(VfsError::CrossFilesystem);
        }
        ResourceIntentKind::Rename {
            target,
            parent_device,
            parent_inode,
        }
    } else {
        ResourceIntentKind::Delete
    };
    Ok(CheckedResourceIntent {
        source,
        source_device,
        source_inode,
        source_bytes,
        canonical_roots: roots.to_vec(),
        maximum_bytes: maximum_bytes as u64,
        kind,
    })
}

#[cfg(windows)]
pub(super) fn apply_resource_intent(intent: &CheckedResourceIntent) -> Result<(), VfsError> {
    let target = match &intent.kind {
        ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
        ResourceIntentKind::Delete => None,
    };
    let current = resolve_resource_intent(
        &intent.source,
        target,
        &intent.canonical_roots,
        intent.maximum_bytes as usize,
    )?;
    if current.source_device != intent.source_device
        || current.source_inode != intent.source_inode
        || current.source_bytes != intent.source_bytes
        || current.kind != intent.kind
    {
        return Err(VfsError::ChangedOnDisk);
    }
    let operation = match target {
        Some(target) => ExplorerMutation::Rename {
            source: intent.source.clone(),
            target: target.into(),
        },
        None => ExplorerMutation::Delete {
            path: intent.source.clone(),
        },
    };
    apply_explorer_mutation(&operation, &intent.canonical_roots, intent.maximum_bytes)
}

#[cfg(windows)]
fn replace_exact(path: &Path, temporary: NamedTempFile, _expected: &[u8]) -> Result<(), VfsError> {
    // Windows has no rename-exchange. The caller checks the disk revision immediately
    // before this atomic replacement, but cannot preserve a racing writer's displaced file.
    fs::rename(temporary.path(), path).map_err(|source| VfsError::Io {
        operation: "replace destination",
        path: path.into(),
        source,
    })
}

#[cfg(windows)]
pub(super) fn agent_atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), VfsError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| VfsError::OutsideScope(path.into()))?;
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(VfsError::OutsideScope(path.into()));
    }
    let mut current = root.to_path_buf();
    let mut held = hold_scoped_parents(&root.join("entry"), &[root.into()])?;
    for part in relative.parent().unwrap_or(Path::new("")).components() {
        current.push(part);
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(VfsError::Io {
                    operation: "create agent directory",
                    path: current,
                    source,
                });
            }
        }
        held.push(
            open_windows_path(&current, false).map_err(|source| VfsError::Io {
                operation: "open agent directory",
                path: current.clone(),
                source,
            })?,
        );
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(VfsError::NotRegularFile(path.into()));
        }
        Err(source) if source.kind() != io::ErrorKind::NotFound => {
            return Err(VfsError::Io {
                operation: "inspect agent destination",
                path: path.into(),
                source,
            });
        }
        _ => {}
    }
    atomic_write(path, bytes)
}

/// Open a directory for identity checks and relative opens with search permission only.
#[cfg(unix)]
pub(crate) fn directory_open_flags() -> rustix::fs::OFlags {
    #[cfg(target_os = "macos")]
    {
        // O_EVTONLY needs read permission; O_SEARCH is enough to walk through a directory.
        rustix::fs::OFlags::from_bits_retain(nix::libc::O_SEARCH as _)
    }
    #[cfg(not(target_os = "macos"))]
    {
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY
    }
}

/// Open an entry for identity checks without requiring read permission.
#[cfg(unix)]
pub(crate) fn path_open_flags() -> rustix::fs::OFlags {
    #[cfg(target_os = "macos")]
    {
        // macOS has no O_PATH; O_EVTONLY also permits fstat and directory-relative opens.
        rustix::fs::OFlags::from_bits_retain(nix::libc::O_EVTONLY as _)
    }
    #[cfg(not(target_os = "macos"))]
    {
        rustix::fs::OFlags::PATH
    }
}

// Canonicalization alone cannot constrain a later open if an ancestor is swapped
// for a symlink. Resolve from a held root and reject links in every component.
#[cfg(unix)]
pub(super) fn open_scoped(
    path: &Path,
    roots: &[PathBuf],
    flags: rustix::fs::OFlags,
) -> Result<fs::File, VfsError> {
    let root = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| VfsError::OutsideScope(path.into()))?;
    let error = |source: io::Error| VfsError::Io {
        operation: "open scoped path",
        path: path.into(),
        source,
    };
    let directory = open_no_symlinks(root, directory_open_flags()).map_err(error)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| VfsError::OutsideScope(path.into()))?;
    open_beneath(directory, relative, flags).map_err(error)
}

#[cfg(unix)]
pub(crate) fn open_no_symlinks(path: &Path, flags: rustix::fs::OFlags) -> io::Result<fs::File> {
    use rustix::fs::CWD;
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;
    let relative = path
        .strip_prefix("/")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "expected an absolute path"))?;
    let root = fs::File::from(openat(
        CWD,
        "/",
        directory_open_flags() | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    open_beneath(root, relative, flags)
}

#[cfg(unix)]
fn open_beneath(
    mut directory: fs::File,
    relative: &Path,
    flags: rustix::fs::OFlags,
) -> io::Result<fs::File> {
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;
    use std::path::Component;
    let mut components = relative
        .components()
        .filter(|part| *part != Component::CurDir)
        .peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path escapes its directory",
            ));
        };
        let flags = if components.peek().is_some() {
            directory_open_flags()
        } else {
            flags
        };
        directory = fs::File::from(openat(
            &directory,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        // O_PATH | O_NOFOLLOW opens the symlink itself instead of rejecting it.
        if directory.metadata()?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "symlink in scoped path",
            ));
        }
        if components.peek().is_none() {
            return Ok(directory);
        }
    }
    // Reopen even an empty relative path so the requested access flags apply.
    Ok(fs::File::from(openat(
        &directory,
        ".",
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

#[cfg(unix)]
fn scoped_parent(
    path: &Path,
    roots: &[PathBuf],
) -> Result<(fs::File, std::ffi::OsString), VfsError> {
    let parent = path
        .parent()
        .ok_or_else(|| VfsError::OutsideScope(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| VfsError::OutsideScope(path.into()))?;
    let file = open_scoped(parent, roots, directory_open_flags())?;
    Ok((file, name.into()))
}

#[cfg(unix)]
pub(super) fn read_scoped_file(
    path: &Path,
    roots: &[PathBuf],
    maximum_bytes: u64,
) -> Result<Vec<u8>, VfsError> {
    let file = open_scoped(
        path,
        roots,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK,
    )?;
    read_open_file_bounded(file, path, maximum_bytes)
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

#[cfg(unix)]
pub(super) fn apply_explorer_mutation(
    operation: &ExplorerMutation,
    roots: &[PathBuf],
    maximum_bytes: u64,
) -> Result<(), VfsError> {
    use rustix::fs::AtFlags;
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::RenameFlags;
    use rustix::fs::mkdirat;
    use rustix::fs::openat;
    use rustix::fs::renameat_with;
    use rustix::fs::unlinkat;
    let error = |path: &Path, source: rustix::io::Errno| VfsError::Io {
        operation: "mutate explorer entry",
        path: path.into(),
        source: source.into(),
    };
    match operation {
        ExplorerMutation::Create { path, directory } => {
            let target = scoped_missing_target(path, roots)?;
            let (parent, name) = scoped_parent(&target, roots)?;
            if *directory {
                mkdirat(&parent, &name, Mode::from_raw_mode(0o777)).map_err(|e| error(&target, e))
            } else {
                let file = fs::File::from(
                    openat(
                        &parent,
                        &name,
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::CLOEXEC
                            | OFlags::NOFOLLOW,
                        Mode::from_raw_mode(0o666),
                    )
                    .map_err(|e| error(&target, e))?,
                );
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
            let (source_parent, source_name) = scoped_parent(&source, roots)?;
            let (target_parent, target_name) = scoped_parent(&target, roots)?;
            renameat_with(
                &source_parent,
                &source_name,
                &target_parent,
                &target_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|e| {
                if e == rustix::io::Errno::EXIST {
                    VfsError::DestinationExists(target.clone())
                } else {
                    error(&source, e)
                }
            })
        }
        ExplorerMutation::Copy { source, target } => {
            let source = scoped_existing_path(source, roots)?;
            let target = scoped_missing_target(target, roots)?;
            let mut input = open_scoped(&source, roots, OFlags::RDONLY | OFlags::NONBLOCK)?;
            let metadata = input.metadata().map_err(|source_error| VfsError::Io {
                operation: "inspect explorer copy source",
                path: source.clone(),
                source: source_error,
            })?;
            let (parent, name) = scoped_parent(&target, roots)?;
            if metadata.is_dir() {
                let entries = rustix::fs::Dir::read_from(&input).map_err(|e| error(&source, e))?;
                for entry in entries {
                    let entry = entry.map_err(|e| error(&source, e))?;
                    if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                        continue;
                    }
                    return Err(VfsError::Io {
                        operation: "copy explorer directory",
                        path: source,
                        source: io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "recursive directory copy is not supported; select an empty directory",
                        ),
                    });
                }
                mkdirat(&parent, &name, Mode::from_raw_mode(0o777)).map_err(|e| error(&target, e))
            } else if metadata.is_file() {
                if metadata.len() > maximum_bytes {
                    return Err(VfsError::DocumentTooLarge {
                        path: source,
                        maximum_bytes,
                    });
                }
                let permissions = metadata.permissions();
                let mut output = fs::File::from(
                    openat(
                        &parent,
                        &name,
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::CLOEXEC
                            | OFlags::NOFOLLOW,
                        Mode::from_raw_mode(0o600),
                    )
                    .map_err(|e| error(&target, e))?,
                );
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
                    Ok(bytes) if bytes <= maximum_bytes => match output
                        .set_permissions(permissions)
                        .and_then(|()| output.sync_all())
                    {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            drop(output);
                            let _ = unlinkat(&parent, &name, AtFlags::empty());
                            Err(VfsError::Io {
                                operation: "sync explorer copy",
                                path: target,
                                source: error,
                            })
                        }
                    },
                    Ok(_) => {
                        drop(output);
                        let _ = unlinkat(&parent, &name, AtFlags::empty());
                        Err(VfsError::DocumentTooLarge {
                            path: source,
                            maximum_bytes,
                        })
                    }
                    Err(error) => {
                        drop(output);
                        let _ = unlinkat(&parent, &name, AtFlags::empty());
                        Err(error)
                    }
                }
            } else {
                Err(VfsError::NotRegularFile(source))
            }
        }
        ExplorerMutation::Delete { path } => {
            let path = scoped_existing_path(path, roots)?;
            let file = open_scoped(&path, roots, path_open_flags())?;
            let metadata = file.metadata().map_err(|source| VfsError::Io {
                operation: "inspect explorer delete target",
                path: path.clone(),
                source,
            })?;
            let (parent, name) = scoped_parent(&path, roots)?;
            if metadata.is_dir() {
                unlinkat(&parent, &name, AtFlags::REMOVEDIR).map_err(|e| error(&path, e))
            } else if metadata.is_file() {
                unlinkat(&parent, &name, AtFlags::empty()).map_err(|e| error(&path, e))
            } else {
                Err(VfsError::NotRegularFile(path))
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn resolve_resource_intent(
    source: &Path,
    target: Option<&Path>,
    roots: &[PathBuf],
    maximum_bytes: usize,
) -> Result<CheckedResourceIntent, VfsError> {
    use std::os::unix::fs::MetadataExt;
    let source = scoped_existing_path(source, roots)?;
    let input = open_scoped(
        &source,
        roots,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK,
    )?;
    let metadata = input.metadata().map_err(|source_error| VfsError::Io {
        operation: "inspect resource source",
        path: source.clone(),
        source: source_error,
    })?;
    if !metadata.is_file() || metadata.len() > maximum_bytes as u64 {
        return Err(VfsError::NotRegularFile(source));
    }
    let bytes = read_open_file_bounded(input, &source, maximum_bytes as u64)?;
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
        canonical_roots: roots.to_vec(),
        maximum_bytes: maximum_bytes as u64,
        kind,
    })
}
#[cfg(unix)]
pub(super) fn apply_resource_intent(intent: &CheckedResourceIntent) -> Result<(), VfsError> {
    use std::os::unix::fs::MetadataExt;
    let source = canonical_path(&intent.source)?;
    if source != intent.source {
        return Err(VfsError::ChangedOnDisk);
    }
    let input = open_scoped(
        &source,
        &intent.canonical_roots,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK,
    )?;
    let metadata = input.metadata().map_err(|source_error| VfsError::Io {
        operation: "revalidate resource",
        path: source.clone(),
        source: source_error,
    })?;
    if metadata.dev() != intent.source_device
        || metadata.ino() != intent.source_inode
        || intent.source_bytes.len() as u64 > intent.maximum_bytes
        || read_open_file_bounded(input, &source, intent.maximum_bytes)? != intent.source_bytes
    {
        return Err(VfsError::ChangedOnDisk);
    }
    let (source_parent, source_name) = scoped_parent(&source, &intent.canonical_roots)?;
    match &intent.kind {
        ResourceIntentKind::Delete => {
            rustix::fs::unlinkat(&source_parent, &source_name, rustix::fs::AtFlags::empty())
                .map_err(|source_error| VfsError::Io {
                    operation: "delete resource",
                    path: source,
                    source: source_error.into(),
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
            if Some(parent.as_path()) != target.parent() {
                return Err(VfsError::ChangedOnDisk);
            }
            let (target_parent, target_name) = scoped_parent(target, &intent.canonical_roots)?;
            let parent_metadata =
                target_parent
                    .metadata()
                    .map_err(|source_error| VfsError::Io {
                        operation: "revalidate resource parent",
                        path: parent.clone(),
                        source: source_error,
                    })?;
            if parent_metadata.dev() != *parent_device || parent_metadata.ino() != *parent_inode {
                return Err(VfsError::ChangedOnDisk);
            }
            rustix::fs::renameat_with(
                &source_parent,
                &source_name,
                &target_parent,
                &target_name,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|e| {
                if e == rustix::io::Errno::EXIST {
                    VfsError::DestinationExists(target.clone())
                } else {
                    VfsError::Io {
                        operation: "rename resource",
                        path: source,
                        source: e.into(),
                    }
                }
            })
        }
    }
}

fn canonical_write_path(path: &Path) -> io::Result<PathBuf> {
    crate::files::canonicalize(path).or_else(|error| {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
        let name = path.file_name().ok_or(error)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(crate::files::canonicalize(parent)?.join(name))
    })
}

/// The absolute path with every symlink resolved, like `fs::canonicalize`. On Windows it
/// drops the `\\?\` prefix std adds when the path does not need it, so canonical paths
/// compare equal to the ones users, Git and the explorer write.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(plain) = plain_windows_path(&path) {
        return Ok(plain);
    }
    Ok(path)
}

/// `\\?\C:\dir` as `C:\dir` and `\\?\UNC\server\share` as `\\server\share`, when the
/// result is short enough for the ordinary Windows path limit.
#[cfg(windows)]
fn plain_windows_path(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    let plain = if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        let disk = text.strip_prefix(r"\\?\")?;
        let bytes = disk.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != br":\" {
            return None;
        }
        disk.to_owned()
    };
    (plain.len() < 260).then(|| PathBuf::from(plain))
}

pub(super) fn canonical_path(path: &Path) -> Result<PathBuf, VfsError> {
    crate::files::canonicalize(path).map_err(|source| VfsError::Io {
        operation: "canonicalize",
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn list_directory(
    path: &Path,
    prefix: &str,
    maximum_entries: usize,
) -> Result<Vec<PathBuf>, VfsError> {
    let entries = fs::read_dir(path).map_err(|source| VfsError::Io {
        operation: "list directory",
        path: path.to_path_buf(),
        source,
    })?;
    let prefix = prefix.to_lowercase();
    let mut paths = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&prefix)
        })
        .map(|entry| entry.path())
        .take(maximum_entries.saturating_add(1))
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(maximum_entries);
    Ok(paths)
}

/// Open without waiting for a FIFO writer, then validate the opened handle.
pub(crate) fn open_regular_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000); // FILE_FLAG_BACKUP_SEMANTICS
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

pub(crate) fn is_symlink_error(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(nix::libc::ELOOP)
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(1921)
    } // ERROR_CANT_RESOLVE_FILENAME
}

pub(crate) fn open_read_no_symlinks(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000 | 0x00200000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::from_raw_os_error(1921));
        }
    }
    Ok(file)
}

pub(crate) fn read_file_bounded(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, VfsError> {
    let file = open_read_no_symlinks(path).map_err(|source| VfsError::Io {
        operation: "open for reading",
        path: path.to_path_buf(),
        source,
    })?;
    read_open_file_bounded(file, path, maximum_bytes)
}

pub(super) fn read_open_file_bounded(
    file: fs::File,
    path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, VfsError> {
    let metadata = file.metadata().map_err(|source| VfsError::Io {
        operation: "inspect read target",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(VfsError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > maximum_bytes {
        return Err(VfsError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
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
    read_file_bounded(path, DOCUMENT_BYTE_LIMIT)
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
            match read_file(path) {
                Ok(bytes) if bytes == *expected => {}
                Ok(_) => return Err(VfsError::ExternalModification(path.to_path_buf())),
                Err(VfsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    return Err(VfsError::ExternalModification(path.to_path_buf()));
                }
                Err(error) => return Err(error),
            }
            replace_exact(path, temporary, expected)?;
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

#[cfg(unix)]
fn replace_exact(path: &Path, temporary: NamedTempFile, expected: &[u8]) -> Result<(), VfsError> {
    use rustix::fs::CWD;
    use rustix::fs::RenameFlags;
    use rustix::fs::renameat_with;
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

#[cfg(unix)]
pub(crate) type FileLock = nix::fcntl::Flock<File>;
#[cfg(windows)]
pub(crate) type FileLock = File;

pub(crate) fn lock_exclusive(file: File, nonblocking: bool) -> Result<FileLock, (File, io::Error)> {
    #[cfg(unix)]
    {
        let mode = if nonblocking {
            nix::fcntl::FlockArg::LockExclusiveNonblock
        } else {
            nix::fcntl::FlockArg::LockExclusive
        };
        nix::fcntl::Flock::lock(file, mode).map_err(|(file, error)| (file, error.into()))
    }
    #[cfg(windows)]
    {
        let result = if nonblocking {
            file.try_lock().map_err(io::Error::from)
        } else {
            file.lock()
        };
        match result {
            Ok(()) => Ok(file),
            Err(error) => Err((file, error)),
        }
    }
}

/// Lock a stable sidecar across atomic replacements of the data file.
pub(crate) fn lock_file(path: &Path) -> io::Result<FileLock> {
    lock_exclusive(open_lock_file(path)?, false).map_err(|(_, error)| error)
}

pub(crate) fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock target must be a regular file",
        ));
    }
    Ok(file)
}

/// Agent paths are already resolved. Open each directory without following
/// symlinks and keep the parent descriptor for creation, rename and cleanup.
#[cfg(unix)]
pub(super) fn agent_atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), VfsError> {
    use rustix::fs::AtFlags;
    use rustix::fs::FileType;
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::fchmod;
    use rustix::fs::mkdirat;
    use rustix::fs::openat;
    use rustix::fs::renameat;
    use rustix::fs::statat;
    use rustix::fs::unlinkat;
    use std::path::Component;
    use std::sync::atomic::AtomicU64;
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    let relative = path
        .strip_prefix(root)
        .map_err(|_| VfsError::OutsideScope(path.into()))?;
    if !root.is_absolute()
        || root
            .components()
            .chain(relative.components())
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(VfsError::OutsideScope(path.into()));
    }
    let name = relative
        .file_name()
        .ok_or_else(|| VfsError::OutsideScope(path.into()))?;
    let result = (|| -> io::Result<()> {
        // Ancestors need search permission, not directory-listing permission.
        let flags = directory_open_flags() | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut directory = File::from(openat(rustix::fs::CWD, "/", flags, Mode::empty())?);
        // Walk the root too: an ancestor substituted since resolution must not
        // redirect the initial directory open outside the resolved workspace.
        for part in root.components() {
            if let Component::Normal(name) = part {
                directory = openat(&directory, name, flags, Mode::empty())?.into();
            }
        }
        for part in relative.parent().unwrap_or(Path::new("")).components() {
            if let Component::Normal(name) = part {
                match mkdirat(&directory, name, Mode::from_raw_mode(0o777)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                directory = openat(&directory, name, flags, Mode::empty())?.into();
            }
        }
        let directory = File::from(openat(
            &directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let permissions = match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
                Some(Mode::from_raw_mode(stat.st_mode))
            }
            Ok(_) => return Err(io::Error::other("not a regular file")),
            Err(rustix::io::Errno::NOENT) => None,
            Err(error) => return Err(error.into()),
        };
        let mut attempts = 0;
        let (temporary_name, mut temporary) = loop {
            attempts += 1;
            let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
            let temporary_name = format!(".dust-{}-{sequence}.tmp", std::process::id());
            if std::ffi::OsStr::new(&temporary_name) == name {
                continue;
            }
            match openat(
                &directory,
                &temporary_name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => break (temporary_name, File::from(fd)),
                Err(rustix::io::Errno::EXIST) if attempts < 16 => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let result = (|| -> io::Result<()> {
            if let Some(permissions) = permissions {
                fchmod(&temporary, permissions)?;
            }
            temporary.write_all(bytes)?;
            temporary.sync_all()?;
            renameat(&directory, &temporary_name, &directory, name)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(&directory, &temporary_name, AtFlags::empty());
        }
        result?;
        directory.sync_all()
    })();
    result.map_err(|source| VfsError::Io {
        operation: "agent write",
        path: path.into(),
        source,
    })
}

/// Write through a same-directory temporary file, sync it, then atomically
/// replace the destination through tempfile's platform-specific persistence.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), VfsError> {
    checked_atomic_write(path, bytes, &WriteExpectation::Any)
}

/// Apply Git's executable flag before publishing the replacement, retaining access permissions.
pub(crate) fn atomic_write_with_executable(
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), VfsError> {
    let temporary = stage_atomic_write(path, bytes)?;
    #[cfg(windows)]
    let _ = executable;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let set_mode = || -> io::Result<()> {
            let mut permissions = temporary.as_file().metadata()?.permissions();
            let mode = permissions.mode() & !0o111;
            permissions.set_mode(if executable {
                mode | ((mode & 0o444) >> 2)
            } else {
                mode
            });
            temporary.as_file().set_permissions(permissions)?;
            temporary.as_file().sync_all()
        };
        set_mode().map_err(|source| VfsError::Io {
            operation: "set replacement executable flag",
            path: path.to_path_buf(),
            source,
        })?;
    }
    commit_atomic_write(path, temporary, &WriteExpectation::Any)
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
    match fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                use std::os::unix::fs::fchown;
                fchown(
                    temporary.as_file(),
                    Some(metadata.uid()),
                    Some(metadata.gid()),
                )
                .map_err(|source| VfsError::Io {
                    operation: "copy ownership",
                    path: path.to_path_buf(),
                    source,
                })?;
            }
            temporary
                .as_file()
                .set_permissions(metadata.permissions())
                .map_err(|source| VfsError::Io {
                    operation: "copy permissions",
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(VfsError::Io {
                operation: "inspect write target",
                path: path.to_path_buf(),
                source,
            });
        }
    }
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

pub(super) fn sync_parent(parent: &Path) -> Result<(), VfsError> {
    sync_directory(parent).map_err(|source| VfsError::Io {
        operation: "sync containing directory",
        path: parent.to_path_buf(),
        source,
    })
}

/// Flush a directory's entries, such as a rename into it, to disk.
#[cfg(unix)]
pub(crate) fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

/// Windows cannot open a directory as a file and has no directory flush.
#[cfg(not(unix))]
pub(crate) fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use crate::files::vfs::*;
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
    // This test substitutes Unix symlinks.
    #[cfg(unix)]
    #[test]
    fn document_writes_reject_missing_destination_aliases_before_creating_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("sub")).unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("alias")).unwrap();
        let owned = root.path().join("new.txt");
        let mut worker = VfsWorker::start(2);
        for target in [
            root.path().join("sub/../new.txt"),
            root.path().join("alias/new.txt"),
        ] {
            for (target, other) in [(target.clone(), owned.clone()), (owned.clone(), target)] {
                worker
                    .write_document(
                        target,
                        b"not written".to_vec(),
                        WriteExpectation::Any,
                        None,
                        vec![other],
                    )
                    .unwrap();
                assert!(matches!(
                    result(&worker),
                    VfsResult::Write {
                        result: Err(VfsError::DestinationExists(_)),
                        ..
                    }
                ));
                assert!(!owned.exists());
            }
        }
    }

    // This test substitutes Unix symlinks.
    #[cfg(unix)]
    #[test]
    fn agent_writes_reject_symlinks_substituted_after_path_resolution() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = crate::files::canonicalize(workspace.path()).unwrap();
        let parent = root.join("nested");
        fs::create_dir(&parent).unwrap();
        let path = resolve_existing_prefix(&parent.join("new/file"));
        fs::remove_dir(&parent).unwrap();
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
        assert!(agent_atomic_write(&root, &path, b"escape").is_err());
        assert!(!outside.path().join("new").exists());
        fs::remove_file(&parent).unwrap();
        agent_atomic_write(&root, &path, b"inside").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"inside");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let target = outside.path().join("target");
        fs::write(&target, "untouched").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(agent_atomic_write(&root, &path, b"escape").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        assert!(agent_atomic_write(&root, &root.join("../escape"), b"escape").is_err());

        // The root itself may also be replaced after validation.
        let moved = root.with_extension("moved");
        fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        assert!(agent_atomic_write(&root, &root.join("escape"), b"escape").is_err());
        assert!(!outside.path().join("escape").exists());
        fs::remove_file(&root).unwrap();
        fs::rename(&moved, &root).unwrap();
    }

    // Search-only directory permissions are Unix-specific.
    #[cfg(unix)]
    #[test]
    fn agent_writes_traverse_search_only_ancestors() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let ancestor = temporary.path().join("search-only");
        let workspace = ancestor.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let root = crate::files::canonicalize(&workspace).unwrap();
        let path = root.join("file");
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o111)).unwrap();
        let result = agent_atomic_write(&root, &path, b"inside");
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        result.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"inside");
    }

    // Checks Unix permission bits.
    #[cfg(unix)]
    #[test]
    fn agent_writes_replace_regular_files_and_preserve_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempfile::tempdir().unwrap();
        let root = crate::files::canonicalize(workspace.path()).unwrap();
        let path = root.join("file");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        agent_atomic_write(&root, &path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
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

        #[cfg(windows)]
        {
            checked_atomic_write(
                &path,
                b"editor",
                &WriteExpectation::Exact(b"newer".to_vec()),
            )
            .unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"editor");
        }
        // Unix provides atomic exchange and unprivileged symlink creation.
        #[cfg(unix)]
        {
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
    }

    #[test]
    fn rename_noreplace_preserves_a_destination_created_after_validation() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"source").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let target = scoped_missing_target(&target, &roots).unwrap();
        let rename = ExplorerMutation::Rename {
            source: source.clone(),
            target: target.clone(),
        };
        let intent = resolve_resource_intent(&source, Some(&target), &roots, 1024).unwrap();
        fs::write(&target, b"other writer").unwrap();
        assert!(matches!(
            apply_explorer_mutation(&rename, &roots, 1024),
            Err(VfsError::DestinationExists(_))
        ));
        // The prepared resource reaches renameat without another missing-target
        // check, so this also exercises the atomic no-replace guarantee.
        assert!(matches!(
            apply_resource_intent(&intent),
            Err(VfsError::DestinationExists(_))
        ));
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&target).unwrap(), b"other writer");
        fs::remove_file(&target).unwrap();
        apply_explorer_mutation(&rename, &roots, 1024).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"source");
    }

    #[test]
    fn resource_application_rejects_growth_beyond_the_prepared_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"small").unwrap();
        let intent = resolve_resource_intent(&source, None, &[root.path().into()], 5).unwrap();
        fs::write(&source, b"larger than accepted").unwrap();
        assert!(matches!(
            apply_resource_intent(&intent),
            Err(VfsError::DocumentTooLarge {
                maximum_bytes: 5,
                ..
            })
        ));
        assert_eq!(fs::read(&source).unwrap(), b"larger than accepted");
    }

    // This test substitutes Unix symlinks.
    #[cfg(unix)]
    #[test]
    fn resource_rename_rejects_a_parent_moved_outside_scope() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = root.path().join("dest");
        fs::create_dir(&parent).unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"keep").unwrap();
        let intent = resolve_resource_intent(
            &source,
            Some(&parent.join("new")),
            &[root.path().into()],
            1024,
        )
        .unwrap();
        let moved = outside.path().join("dest");
        fs::rename(&parent, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &parent).unwrap();
        assert!(matches!(
            apply_resource_intent(&intent),
            Err(VfsError::ChangedOnDisk)
        ));
        assert_eq!(fs::read(&source).unwrap(), b"keep");
        assert!(!moved.join("new").exists());
    }

    // This test creates Unix symlinks.
    #[cfg(unix)]
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
    fn path_handles_support_identity_checks_and_directory_relative_opens() {
        use crate::files::root_identity::file_identity;
        let root = tempfile::tempdir().unwrap();
        let path = crate::files::canonicalize(root.path()).unwrap();
        fs::write(path.join("file"), b"contents").unwrap();
        #[cfg(unix)]
        let file = open_beneath(
            open_path(&path).unwrap(),
            Path::new("file"),
            path_open_flags(),
        )
        .unwrap();
        #[cfg(windows)]
        let file = open_path(&path.join("file")).unwrap();
        let expected = open_path(&path.join("file")).unwrap();
        assert_eq!(
            file_identity(&file).unwrap(),
            file_identity(&expected).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn scoped_reads_reject_windows_junctions() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = crate::files::canonicalize(root.path()).unwrap();
        let target = crate::files::canonicalize(outside.path()).unwrap();
        let link = path.join("junction");
        fs::write(target.join("secret"), b"outside").unwrap();
        let status = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(read_scoped_file(&link.join("secret"), &[path], 1024).is_err());
        assert!(open_read_no_symlinks(&link).is_err());
        assert_eq!(fs::read(target.join("secret")).unwrap(), b"outside");
        fs::remove_dir(link).unwrap();
    }

    // Tests Unix directory-relative descriptors and symlink replacement.
    #[cfg(unix)]
    #[test]
    fn scoped_descriptors_reject_swapped_ancestors_and_pin_mutation_parents() {
        use rustix::fs::AtFlags;
        use rustix::fs::unlinkat;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file"), b"inside").unwrap();
        fs::write(outside.path().join("file"), b"outside").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let canonical = canonical_path(&sub.join("file")).unwrap();
        let (parent, name) = scoped_parent(&canonical, &roots).unwrap();
        let parked = root.path().join("parked");
        fs::rename(&sub, &parked).unwrap();
        std::os::unix::fs::symlink(outside.path(), &sub).unwrap();
        assert!(read_scoped_file(&canonical, &roots, 1024).is_err());
        assert!(open_scoped(&canonical, &roots, path_open_flags()).is_err());
        assert!(scoped_parent(&canonical, &roots).is_err());
        unlinkat(&parent, &name, AtFlags::empty()).unwrap();
        assert!(!parked.join("file").exists());
        assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"outside");
    }

    // FIFO creation is Unix-only.
    #[cfg(unix)]
    #[test]
    fn explorer_copy_rejects_fifo_sources_and_keeps_the_target_missing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fifo");
        let target = root.path().join("copy");
        nix::unistd::mkfifo(&source, nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert!(matches!(
            apply_explorer_mutation(
                &ExplorerMutation::Copy {
                    source,
                    target: target.clone()
                },
                &[root.path().into()],
                1024,
            ),
            Err(VfsError::NotRegularFile(_))
        ));
        assert!(!target.exists());
    }

    // FIFO creation is Unix-only.
    #[cfg(unix)]
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
    fn queued_write_cancellation_is_exact_and_a_full_queue_never_writes() {
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
        let queue_deadline = std::time::Instant::now() + Duration::from_secs(2);
        for _ in 0..3 {
            loop {
                assert!(std::time::Instant::now() < queue_deadline);
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
    fn reused_or_exhausted_request_identity_rejects_before_queueing() {
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
