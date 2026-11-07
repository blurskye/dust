//! Document open, save, reload and startup requests with bounded file I/O.
//! Feature owners consume their own results. Queued cancellation and shutdown are
//! nonblocking; running filesystem calls are not interruptible.

pub(in crate::app) const VFS_QUEUE_CAPACITY: usize = 8;
pub(in crate::app) const LARGE_FILE_BYTES: usize = 16 * 1024 * 1024;
pub(in crate::app) const LARGE_FILE_LINES: usize = 250_000;

mod lifecycle;
pub(in crate::app) mod save_work;
#[cfg(test)]
pub(in crate::app) use lifecycle::configured_file_bytes;
mod task_io;
use crate::app::startup::StartupTarget;
use crate::editor::BufferId;
use crate::editor::EditorWindowVersion;
use crate::editor::text::TextBuffer;
use crate::files::CheckedResourceIntent;
use crate::files::ExplorerMutation;
use crate::files::RecoveryRecord;
use crate::files::RequestId;
use crate::files::VfsError;
use crate::files::VfsResult;
use crate::files::VfsWorker;
pub(in crate::app) use lifecycle::DocumentChange;
pub(in crate::app) use lifecycle::DocumentFailure;
pub(in crate::app) use lifecycle::DocumentView;
pub(in crate::app) use lifecycle::FileReply;
pub(in crate::app) use lifecycle::SaveRequest;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
pub(in crate::app) use task_io::TaskCancellation;
pub(in crate::app) use task_io::TaskReadKey;
pub(in crate::app) use task_io::TaskReadReply;
pub(in crate::app) use task_io::TaskReadStopped;
pub(in crate::app) use task_io::TaskSubmission;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum LargeFileOverride {
    Auto,
    On,
    Off,
}
pub(in crate::app) fn mode_name_large(mode: LargeFileOverride) -> &'static str {
    match mode {
        LargeFileOverride::Auto => "auto",
        LargeFileOverride::On => "on",
        LargeFileOverride::Off => "off",
    }
}
struct StartupIntent {
    primary: bool,
    delivered: bool,
}
pub(in crate::app) struct StartupRead {
    owner: Arc<()>,
    request: RequestId,
}
pub(in crate::app) enum DocumentEvent {
    Legacy(VfsResult),
    WorkspaceIo(WorkspaceIoReply),
    TaskRead(TaskReadReply),
    Explorer(crate::files::ExplorerIoReply),
    File(FileReply),
    Stopped {
        task_reads: TaskReadStopped,
        requests: Vec<RequestId>,
        error: String,
    },
    Startup {
        read: StartupRead,
        path: PathBuf,
        text: Result<TextBuffer, String>,
    },
    Discarded,
}
pub(super) struct Documents {
    vfs: VfsWorker,
    owner: Arc<()>,
    startup: VecDeque<(PathBuf, bool)>,
    pending_startup: HashMap<RequestId, StartupIntent>,
    pending_legacy: HashSet<RequestId>,
    pending_workspace: HashSet<RequestId>,
    pending_tasks: HashSet<RequestId>,
    pending_files: HashMap<RequestId, lifecycle::FileIntent>,
    latest_open: Arc<()>,
    maximum_pending: usize,
    closing: bool,
    stopped: bool,
    primary: Option<BufferId>,
    primary_path: Option<PathBuf>,
    startup_navigation: Option<crate::app::windows::NavigationStamp>,
    startup_activate: bool,
    editor: EditorWindowVersion,
    root_revision: u64,
    policy_editor: EditorWindowVersion,
    overrides: HashMap<BufferId, LargeFileOverride>,
    restored_overrides: HashMap<PathBuf, LargeFileOverride>,
    save_work: save_work::SaveWork,
}
impl Documents {
    pub(in crate::app) fn new(
        vfs: VfsWorker,
        startup: &StartupTarget,
        editor: EditorWindowVersion,
        root_revision: u64,
        maximum_pending: usize,
    ) -> Self {
        let primary_path = match startup {
            StartupTarget::File { path, .. } => Some(path.clone()),
            StartupTarget::Dashboard { .. } => None,
        };
        let startup = match startup {
            StartupTarget::Dashboard { .. } => VecDeque::new(),
            StartupTarget::File {
                path,
                additional_files,
                ..
            } => std::iter::once((path.clone(), true))
                .chain(additional_files.iter().cloned().map(|path| (path, false)))
                // A file that does not exist yet has nothing to read.
                .filter(|(path, _)| path.exists())
                .collect(),
        };
        Self {
            vfs,
            owner: Arc::new(()),
            startup,
            pending_startup: HashMap::new(),
            pending_legacy: HashSet::new(),
            pending_workspace: HashSet::new(),
            pending_tasks: HashSet::new(),
            pending_files: HashMap::new(),
            latest_open: Arc::new(()),
            maximum_pending: maximum_pending.max(1),
            closing: false,
            stopped: false,
            primary: None,
            primary_path,
            startup_navigation: None,
            startup_activate: true,
            policy_editor: editor.clone(),
            editor,
            root_revision,
            overrides: HashMap::new(),
            restored_overrides: HashMap::new(),
            save_work: save_work::SaveWork::default(),
        }
    }
    pub(in crate::app) fn startup_activation(
        &mut self,
        editor: &crate::editor::EditorState,
        windows: &crate::app::windows::Windows,
    ) -> bool {
        if let Some(stamp) = &self.startup_navigation {
            self.startup_activate &=
                windows.navigation_current(stamp, &editor.window_version(), editor.active_view());
        } else {
            // New startup files were opened synchronously and need no read read.
            self.primary = self.primary_path.as_ref().and_then(|path| {
                editor
                    .buffers
                    .iter()
                    .find_map(|(id, buffer)| (buffer.path.as_ref() == Some(path)).then_some(id))
            });
            self.startup_activated(editor, windows);
        }
        self.startup_activate
    }
    pub(in crate::app) fn startup_activated(
        &mut self,
        editor: &crate::editor::EditorState,
        windows: &crate::app::windows::Windows,
    ) {
        self.startup_navigation =
            Some(windows.navigation_stamp(editor.window_version(), editor.active_view()));
    }
    fn startup_current(&self, editor: &EditorWindowVersion, root_revision: u64) -> bool {
        editor.revision().is_some()
            && self.editor.same_editor(editor)
            && self.root_revision == root_revision
    }
    pub(in crate::app) fn has_startup_work(&self) -> bool {
        !self.startup.is_empty() || !self.pending_startup.is_empty()
    }
    pub(in crate::app) fn queue_startup(
        &mut self,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> Vec<VfsError> {
        if !self.startup_current(editor, root_revision) {
            self.startup.clear();
            self.primary = None;
            return Vec::new();
        }
        let mut errors = Vec::new();
        while let Some((path, primary)) = self.startup.front() {
            if self.check_capacity().is_err() {
                break;
            }
            match self.vfs.read(path.clone()) {
                Ok(id) => {
                    self.pending_startup.insert(
                        id,
                        StartupIntent {
                            primary: *primary,
                            delivered: false,
                        },
                    );
                    self.startup.pop_front();
                }
                Err(VfsError::QueueFull) => break,
                Err(error) => {
                    self.startup.pop_front();
                    errors.push(error);
                }
            }
        }
        errors
    }
    pub(in crate::app) fn poll(
        &mut self,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> Result<Option<DocumentEvent>, VfsError> {
        if self.stopped {
            return Ok(None);
        }
        match self.vfs.try_reply() {
            Ok(event) => Ok(event.map(|event| match event {
                crate::files::VfsReply::Public(event) => self.route(event, editor, root_revision),
                crate::files::VfsReply::Explorer(reply) => {
                    if self.pending_legacy.remove(&reply.id()) {
                        DocumentEvent::Explorer(reply)
                    } else {
                        DocumentEvent::Discarded
                    }
                }
            })),
            Err(error) => {
                self.stopped = true;
                let requests = self
                    .pending_legacy
                    .drain()
                    .chain(self.pending_workspace.drain())
                    .collect();
                self.pending_tasks.clear();
                self.pending_files.clear();
                self.pending_startup.clear();
                self.startup.clear();
                self.primary = None;
                Ok(Some(DocumentEvent::Stopped {
                    task_reads: TaskReadStopped::new(self.owner.clone()),
                    requests,
                    error: error.to_string(),
                }))
            }
        }
    }
    fn route(
        &mut self,
        event: VfsResult,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> DocumentEvent {
        if self.pending_tasks.remove(&event.request_id()) {
            return DocumentEvent::TaskRead(TaskReadReply::new(self.owner.clone(), event));
        }
        if self.pending_workspace.remove(&event.request_id()) {
            return DocumentEvent::WorkspaceIo(WorkspaceIoReply {
                worker: self.owner.clone(),
                result: event,
            });
        }
        if let VfsResult::Read { id, .. } | VfsResult::Write { id, .. } = &event
            && self.pending_files.contains_key(id)
        {
            return self.file_reply(*id, event);
        }
        if let VfsResult::Read { id, path, result } = event {
            if self.pending_startup.contains_key(&id) {
                if !self.startup_current(editor, root_revision) {
                    self.pending_startup.remove(&id);
                    self.startup.clear();
                    self.primary = None;
                    return DocumentEvent::Discarded;
                }
                let intent = self
                    .pending_startup
                    .get_mut(&id)
                    .expect("checked startup request");
                if intent.delivered {
                    return DocumentEvent::Discarded;
                }
                intent.delivered = true;
                let text = result
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string()));
                return DocumentEvent::Startup {
                    read: StartupRead {
                        owner: self.owner.clone(),
                        request: id,
                    },
                    path,
                    text,
                };
            }
            return if self.pending_legacy.remove(&id) {
                DocumentEvent::Legacy(VfsResult::Read { id, path, result })
            } else {
                DocumentEvent::Discarded
            };
        }
        let id = match &event {
            VfsResult::WorkspaceFile { id, .. }
            | VfsResult::PathsResolved { id, .. }
            | VfsResult::ResourceResolved { id, .. }
            | VfsResult::ResourceApplied { id, .. }
            | VfsResult::ExplorerMutated { id, .. }
            | VfsResult::CreateTarget { id, .. }
            | VfsResult::Read { id, .. }
            | VfsResult::Write { id, .. }
            | VfsResult::Directory { id, .. }
            | VfsResult::RecoveryDiscovered { id, .. }
            | VfsResult::RecoveryWritten { id, .. }
            | VfsResult::RecoveryCleaned { id, .. }
            | VfsResult::Deleted { id, .. } => *id,
        };
        if self.pending_legacy.remove(&id) {
            DocumentEvent::Legacy(event)
        } else {
            DocumentEvent::Discarded
        }
    }
    pub(in crate::app) fn finish_startup(
        &mut self,
        read: StartupRead,
        buffer: Option<BufferId>,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> bool {
        if !Arc::ptr_eq(&self.owner, &read.owner) {
            return false;
        }
        let Some(intent) = self.pending_startup.remove(&read.request) else {
            return false;
        };
        if !self.startup_current(editor, root_revision) {
            self.primary = None;
            return false;
        }
        if intent.primary {
            self.primary = buffer;
        }
        true
    }
    pub(in crate::app) fn take_startup_primary(
        &mut self,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> Option<BufferId> {
        if !self.startup_current(editor, root_revision) {
            self.primary = None;
            return None;
        }
        if self.has_startup_work() {
            None
        } else {
            self.primary.take()
        }
    }
    pub(in crate::app) fn configured_mode(&self, buffer: BufferId) -> LargeFileOverride {
        self.overrides
            .get(&buffer)
            .copied()
            .unwrap_or(LargeFileOverride::Auto)
    }
    pub(in crate::app) fn effective_large_file(
        &self,
        buffer: BufferId,
        text: Option<&TextBuffer>,
        policy: &crate::config::LargeFileConfig,
    ) -> bool {
        match self.configured_mode(buffer) {
            LargeFileOverride::On => true,
            LargeFileOverride::Off => false,
            LargeFileOverride::Auto => text.is_some_and(|text| {
                policy.enabled
                    && (text.len_bytes() >= LARGE_FILE_BYTES
                        || text.len_lines() >= LARGE_FILE_LINES)
            }),
        }
    }
    pub(in crate::app) fn set_large_file_mode(
        &mut self,
        buffer: BufferId,
        path: Option<&Path>,
        mode: LargeFileOverride,
    ) {
        self.overrides.insert(buffer, mode);
        if let Some(path) = path {
            if mode == LargeFileOverride::Auto {
                self.restored_overrides.remove(path);
            } else {
                self.restored_overrides.insert(path.to_owned(), mode);
            }
        }
    }
    pub(in crate::app) fn reconcile_policy<'a>(
        &mut self,
        editor: &EditorWindowVersion,
        documents: impl Iterator<Item = (BufferId, Option<&'a Path>, &'a TextBuffer)>,
        policy: &crate::config::LargeFileConfig,
    ) -> Vec<BufferId> {
        if !self.policy_editor.same_editor(editor) {
            self.overrides.clear();
            self.policy_editor = editor.clone();
        }
        let mut live = std::collections::HashSet::new();
        let mut large = Vec::new();
        for (id, path, text) in documents {
            live.insert(id);
            if !self.overrides.contains_key(&id)
                && let Some(mode) = path
                    .and_then(|path| self.restored_overrides.get(path))
                    .copied()
            {
                self.overrides.insert(id, mode);
            }
            if self.effective_large_file(id, Some(text), policy) {
                large.push(id);
            }
        }
        self.overrides.retain(|id, _| live.contains(id));
        large
    }
    pub(in crate::app) fn policy_overrides(
        &self,
    ) -> impl Iterator<Item = (BufferId, LargeFileOverride)> + '_ {
        self.overrides.iter().map(|(id, mode)| (*id, *mode))
    }
    pub(in crate::app) fn restore_policy(
        &mut self,
        entries: impl Iterator<Item = (PathBuf, String)>,
    ) {
        self.restored_overrides = entries
            .filter_map(|(path, mode)| {
                Some((
                    path,
                    match mode.as_str() {
                        "on" => LargeFileOverride::On,
                        "off" => LargeFileOverride::Off,
                        "auto" => LargeFileOverride::Auto,
                        _ => return None,
                    },
                ))
            })
            .collect();
    }
    #[cfg(test)]
    pub(in crate::app) fn enqueue_startup_fixture(&mut self, path: PathBuf, primary: bool) {
        self.startup.push_back((path, primary));
    }
    #[cfg(test)]
    pub(in crate::app) fn startup_pending(&self) -> usize {
        self.pending_startup.len()
    }
    pub(in crate::app) fn cancel_startup(&mut self) {
        self.startup.clear();
        self.primary = None;
        for id in self.pending_startup.keys() {
            self.vfs.cancel(*id);
        }
        self.pending_startup.clear();
    }
    pub(in crate::app) fn stop_accepting(&mut self) {
        self.closing = true;
        self.startup.clear();
    }
    pub(in crate::app) fn pending_count(&self) -> usize {
        self.pending_files
            .len()
            .saturating_add(self.pending_legacy.len())
            .saturating_add(self.pending_workspace.len())
            .saturating_add(self.pending_tasks.len())
            .saturating_add(self.pending_startup.len())
    }
    pub(in crate::app) fn legacy_pending(&self) -> bool {
        !self.pending_legacy.is_empty()
    }
    #[cfg(test)]
    pub(in crate::app) fn legacy_count(&self) -> usize {
        self.pending_legacy.len()
    }
    pub(in crate::app) fn has_work(&self) -> bool {
        !self.pending_files.is_empty()
            || !self.pending_legacy.is_empty()
            || !self.pending_workspace.is_empty()
            || !self.pending_tasks.is_empty()
            || self.has_startup_work()
            || self.vfs.has_outstanding()
    }
    pub(in crate::app) fn stop_worker(&mut self) {
        self.closing = true;
        self.vfs.begin_shutdown();
    }
    pub(in crate::app) fn worker_finished(&self) -> bool {
        self.vfs.shutdown_finished()
    }
    pub(in crate::app) fn check_capacity(&self) -> Result<(), VfsError> {
        if self.closing || self.stopped {
            return Err(VfsError::WorkerStopped);
        }
        if self
            .pending_legacy
            .len()
            .saturating_add(self.pending_workspace.len())
            .saturating_add(self.pending_tasks.len())
            .saturating_add(self.pending_startup.len())
            .saturating_add(self.pending_files.len())
            >= self.maximum_pending
        {
            Err(VfsError::QueueFull)
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(in crate::app) fn read(&mut self, path: PathBuf) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.read(path)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn resolve_agent_paths(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.resolve_paths(paths)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn write_agent_file(
        &mut self,
        path: PathBuf,
        root: PathBuf,
        bytes: Vec<u8>,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.agent_write(path, root, bytes)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn read_scoped(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.read_scoped(path, canonical_roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    // If queued cancellation fails, keep routing the result: the resource operation
    // may already be running or complete.
    pub(in crate::app) fn cancel_workspace_queued(
        &mut self,
        id: RequestId,
    ) -> Option<WorkspaceCancellation> {
        if self.pending_workspace.contains(&id) && self.vfs.cancel(id) {
            self.pending_workspace.remove(&id);
            Some(WorkspaceCancellation {
                worker: self.owner.clone(),
                id,
            })
        } else {
            None
        }
    }
    pub(in crate::app) fn cancel_request(&mut self, id: RequestId) -> bool {
        (self.pending_legacy.remove(&id) | self.pending_workspace.remove(&id))
            && self.vfs.cancel(id)
    }
    pub(in crate::app) fn read_scoped_bounded(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self
            .vfs
            .read_scoped_bounded(path, canonical_roots, maximum_bytes)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn resolve_resource(
        &mut self,
        source: PathBuf,
        target: Option<PathBuf>,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: usize,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self
            .vfs
            .resolve_resource(source, target, canonical_roots, maximum_bytes)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn apply_resource(
        &mut self,
        intent: CheckedResourceIntent,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.apply_resource(intent)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn open_explorer(
        &mut self,
        path: PathBuf,
        roots: Vec<crate::files::RootIdentity>,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.open_explorer(path, roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::app) fn inspect_explorer_rename(
        &mut self,
        path: PathBuf,
        roots: Vec<crate::files::RootIdentity>,
        maximum: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.inspect_explorer_rename(path, roots, maximum)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::app) fn prepare_explorer_rename(
        &mut self,
        source: crate::files::ExplorerRenameSource,
        target: PathBuf,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.prepare_explorer_rename(source, target)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::app) fn prepare_explorer(
        &mut self,
        operation: ExplorerMutation,
        roots: Vec<crate::files::RootIdentity>,
        maximum: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.prepare_explorer(operation, roots, maximum)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::app) fn apply_explorer(
        &mut self,
        intent: crate::files::PreparedExplorerMutation,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.apply_explorer(intent)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn resolve_create(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.resolve_create(path, canonical_roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn list_directory(
        &mut self,
        path: PathBuf,
        prefix: String,
        maximum_entries: usize,
    ) -> Result<RequestId, VfsError> {
        self.check_capacity()?;
        let id = self.vfs.list_directory(path, prefix, maximum_entries)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::app) fn recovery_io(&mut self) -> RecoveryIo<'_> {
        let remaining = if self.closing || self.stopped {
            0
        } else {
            self.maximum_pending.saturating_sub(
                self.pending_legacy.len()
                    + self.pending_workspace.len()
                    + self.pending_tasks.len()
                    + self.pending_startup.len()
                    + self.pending_files.len(),
            )
        };
        RecoveryIo {
            vfs: &mut self.vfs,
            pending: &mut self.pending_legacy,
            remaining,
        }
    }
}

/// Borrow of the VFS transport and its remaining document request capacity.
/// Recovery tracks its pending requests; this borrow only routes I/O.
pub(in crate::app) struct RecoveryIo<'a> {
    vfs: &'a mut VfsWorker,
    pending: &'a mut HashSet<RequestId>,
    remaining: usize,
}
impl RecoveryIo<'_> {
    pub(in crate::app) fn submit(
        &mut self,
        send: impl FnOnce(&mut VfsWorker) -> Result<RequestId, VfsError>,
    ) -> Result<RequestId, VfsError> {
        if self.remaining == 0 {
            return Err(VfsError::QueueFull);
        }
        let id = send(self.vfs)?;
        self.remaining -= 1;
        self.pending.insert(id);
        Ok(id)
    }
    pub(in crate::app) fn discover(
        &mut self,
        path: &Path,
        maximum: usize,
    ) -> Result<RequestId, VfsError> {
        self.submit(|vfs| vfs.discover_recovery(path.to_owned(), maximum))
    }
    pub(in crate::app) fn cleanup(
        &mut self,
        path: &Path,
        maximum: usize,
        days: u64,
        protected: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.submit(|vfs| vfs.cleanup_recovery(path.to_owned(), maximum, days, protected))
    }
    pub(in crate::app) fn delete(&mut self, path: &Path) -> Result<RequestId, VfsError> {
        self.submit(|vfs| vfs.delete(path.to_owned()))
    }
    pub(in crate::app) fn write(
        &mut self,
        path: &Path,
        buffer: &crate::editor::Buffer,
    ) -> Result<RequestId, VfsError> {
        self.submit(|vfs| {
            vfs.write_recovery(
                path.to_owned(),
                RecoveryRecord::new(
                    buffer.path.as_deref(),
                    buffer.text.revision().0,
                    buffer.text.to_string(),
                ),
            )
        })
    }
}

// Submission results are constructed only by this router after the VFS result.
// Constructed and consumed inside submission while the source loan is live.
// Callers cannot fabricate an enqueue acknowledgement or detach its prepared request.
pub(in crate::app) struct WorkspaceSubmission {
    worker: Arc<()>,
    result: Result<RequestId, String>,
}
impl WorkspaceSubmission {
    pub(in crate::app) fn into_parts(self) -> (Arc<()>, Result<RequestId, String>) {
        (self.worker, self.result)
    }
}
pub(in crate::app) struct WorkspaceCancellation {
    worker: Arc<()>,
    id: RequestId,
}
impl WorkspaceCancellation {
    pub(in crate::app) fn id(&self) -> RequestId {
        self.id
    }
    pub(in crate::app) fn is_from(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
}
pub(in crate::app) struct WorkspaceIoReply {
    worker: Arc<()>,
    result: VfsResult,
}
impl WorkspaceIoReply {
    pub(in crate::app) fn id(&self) -> RequestId {
        self.result.request_id()
    }
    pub(in crate::app) fn is_from(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
    pub(in crate::app) fn result(&self) -> &VfsResult {
        &self.result
    }
    pub(in crate::app) fn into_result(self) -> VfsResult {
        self.result
    }
}
impl Documents {
    pub(in crate::app) fn submit_workspace_io(
        &mut self,
        prepared: crate::app::workspace::edits::PreparedIo<'_>,
    ) -> Result<bool, String> {
        match self.check_capacity() {
            Err(VfsError::QueueFull) => return Ok(false),
            result => result.map_err(|e| e.to_string())?,
        }
        let file = prepared.file(&self.owner)?;
        use crate::app::workspace::edits::InputFile;
        let result = match file {
            InputFile::Read(path) | InputFile::ReadMatchingDisk { path, .. } => {
                self.read_scoped_bounded(path, prepared.roots().to_vec(), prepared.maximum() as u64)
            }
            InputFile::Create(path) => self.resolve_create(path, prepared.roots().to_vec()),
            InputFile::Resource { source, target } => self.resolve_resource(
                source,
                target,
                prepared.roots().to_vec(),
                prepared.maximum(),
            ),
        };
        if matches!(result, Err(VfsError::QueueFull)) {
            return Ok(false);
        }
        if let Ok(id) = &result {
            self.pending_legacy.remove(id);
            self.pending_workspace.insert(*id);
        }
        prepared.finish(WorkspaceSubmission {
            worker: self.owner.clone(),
            result: result.map_err(|e| e.to_string()),
        })?;
        Ok(true)
    }
    pub(in crate::app) fn submit_workspace_resource(
        &mut self,
        prepared: crate::app::workspace::edits::PreparedResource<'_>,
    ) -> Result<bool, String> {
        match self.check_capacity() {
            Err(VfsError::QueueFull) => return Ok(false),
            result => result.map_err(|e| e.to_string())?,
        }
        // Keep the staged intent until the worker accepts it, including when
        // its own bounded queue is full.
        let result = self.apply_resource(prepared.intent(&self.owner)?.clone());
        if matches!(result, Err(VfsError::QueueFull)) {
            return Ok(false);
        }
        if let Ok(id) = &result {
            self.pending_legacy.remove(id);
            self.pending_workspace.insert(*id);
        }
        prepared.finish(WorkspaceSubmission {
            worker: self.owner.clone(),
            result: result.map_err(|e| e.to_string()),
        })?;
        Ok(true)
    }
}

pub(in crate::app) mod recovery;

mod navigation;

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::InteractiveError;
use crate::app::persistence::RestoreContext;
use crate::app::persistence::RestoreUpdate;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::UndoPolicy;
use crate::editor::text::ByteOffset;
use crate::editor::text::PositionCodec;
use std::ops::ControlFlow::Break;
use std::ops::ControlFlow::Continue;
use std::time::Instant;

use crate::app::language::NavigationRead;

/// Lines of a recovery journal its preview shows.
pub(in crate::app) const RECOVERY_PREVIEW_LINES: usize = 500;
impl App {
    pub(in crate::app) fn effective_large_file(&self, buffer: crate::editor::BufferId) -> bool {
        self.documents.effective_large_file(
            buffer,
            self.state.buffers.get(buffer).map(|buffer| &buffer.text),
            &self.configuration.settings().editor.large_file,
        )
    }
}

impl App {
    pub(in crate::app) fn process_large_file_modes(&mut self) {
        let large = self.documents.reconcile_policy(
            &self.state.window_version(),
            self.state
                .buffers
                .iter()
                .map(|(id, buffer)| (id, buffer.path.as_deref(), &buffer.text)),
            &self.configuration.settings().editor.large_file,
        );
        self.project_large_file_modes(large);
    }
}

impl App {
    pub(in crate::app) fn sync_status_summary_ui(&mut self) {
        let branch = self.git.branch().map(str::to_owned);
        self.ui.set(branch, |ui| &mut ui.git_branch);
        let running = self.tasks.running_count();
        self.ui.set(running, |ui| &mut ui.running_tasks);
    }
}

impl App {
    pub(in crate::app) fn toggle_buffer_pin(&mut self, buffer: crate::editor::BufferId) {
        if !self.state.buffers.contains_key(buffer) {
            return;
        }
        self.window_session.toggle_pin(buffer, &self.state);
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn toggle_active_buffer_pin(&mut self) {
        if let Some(buffer) = self.state.active_buffer_id() {
            self.toggle_buffer_pin(buffer)
        }
    }
}

impl App {
    pub(in crate::app) fn move_active_buffer_line(&mut self, direction: isize) {
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        if let Err(error) = self.window_session.move_content(buffer, direction) {
            self.show_error(error);
            return;
        }
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn schedule_recovery_work(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            false,
            self.documents.recovery_io(),
        );
        if std::mem::take(&mut self.recovery.oversized_notice) {
            self.notify_warning(self.message("recovery_too_large", &[]));
        }
    }
}

impl App {
    pub(in crate::app) fn queue_dirty_recovery_before_exit(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            true,
            self.documents.recovery_io(),
        );
    }
}

impl App {
    pub(in crate::app) fn process_vfs_results(&mut self) {
        self.process_save_work();
        self.process_pending_saves();
        loop {
            self.submit_restore_reads();
            let result = match self
                .documents
                .poll(&self.state.window_version(), self.workspace.root_revision())
            {
                Ok(Some(DocumentEvent::Legacy(result))) => result,
                Ok(Some(DocumentEvent::TaskRead(reply))) => {
                    if let Err(error) = self.tasks.accept_linter_read(
                        reply,
                        &self.state,
                        &self.workspace,
                        self.configuration.binding_revision(),
                        &self.diagnostics,
                    ) {
                        self.show_error(error);
                    }
                    self.drain_linter_sources();
                    continue;
                }
                Ok(Some(DocumentEvent::WorkspaceIo(reply))) => {
                    self.process_workspace_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Explorer(reply))) => {
                    self.process_explorer_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::File(reply))) => {
                    self.process_file_reply(reply);
                    self.process_pending_saves();
                    continue;
                }
                Ok(Some(DocumentEvent::Stopped {
                    requests,
                    error,
                    task_reads,
                })) => {
                    for id in requests {
                        self.input.forget_document_request(id);
                        self.search.forget_document_request(id);
                        self.language.forget_document_request(id);
                        self.persistence.forget_document_request(id);
                        self.navigation_reads.remove(&id);
                        self.recovery.take_read(id, self.workspace.version());
                    }
                    self.fail_agent_files(&error);
                    self.recovery.stopped();
                    self.search.clear_preview();
                    self.explorer.transport_stopped();
                    self.persistence.restore_transport_stopped();
                    if self.workspace_edits.proposal().is_some() || self.workspace_edits.preparing()
                    {
                        self.reject_workspace_proposal();
                    }
                    self.finish_workspace_resources("document worker stopped");
                    self.tasks.linter_worker_stopped(task_reads);
                    self.drain_linter_sources();
                    self.show_error(error);
                    break;
                }
                Ok(Some(DocumentEvent::Discarded)) => continue,
                Ok(Some(DocumentEvent::Startup { read, path, text })) => {
                    let buffer = match text
                        .and_then(|text| self.open_startup_document(text, path.clone()))
                    {
                        Ok(buffer) => Some(buffer),
                        Err(error) => {
                            self.show_error(error);
                            None
                        }
                    };
                    self.documents.finish_startup(
                        read,
                        buffer,
                        &self.state.window_version(),
                        self.workspace.root_revision(),
                    );
                    self.finish_startup_reads_if_complete();
                    continue;
                }
                Ok(None) => break,
                Err(error) => {
                    self.show_error(error);
                    break;
                }
            };
            let result = match self.persistence.accept_restore(
                result,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) {
                Break(RestoreUpdate::Waiting) => continue,
                Break(RestoreUpdate::Ready(ready)) => {
                    self.finish_session_restore(*ready);
                    continue;
                }
                Break(RestoreUpdate::Abandoned) => {
                    self.cancel_session_restore();
                    continue;
                }
                Break(RestoreUpdate::Failed(error)) => {
                    self.cancel_session_restore();
                    self.show_error(error);
                    continue;
                }
                Continue(result) => result,
            };
            let Some(result) = self.accept_agent_file(result) else {
                continue;
            };
            let result = match self.recovery.accept(result) {
                Break(update) => {
                    use crate::app::documents::recovery::RecoveryUpdate;
                    match update {
                        RecoveryUpdate::Quiet | RecoveryUpdate::Pruned(0) => {}
                        RecoveryUpdate::Error(error) => self.show_error(error),
                        RecoveryUpdate::Pruned(count) => self.notify_message(
                            self.message("recovery_pruned", &[("count", count.to_string())]),
                            false,
                        ),
                        RecoveryUpdate::Discarded => {
                            if self.recovery_picker_open() {
                                self.open_recovery_picker();
                            }
                            self.notify_message(self.message("recovery_discarded", &[]), false);
                        }
                        RecoveryUpdate::CrashCleared => {
                            self.notify_message(self.message("crash_cleared", &[]), false)
                        }
                    }
                    continue;
                }
                Continue(result) => result,
            };
            match result {
                VfsResult::WorkspaceFile { .. }
                | VfsResult::PathsResolved { .. }
                | VfsResult::RecoveryDiscovered { .. }
                | VfsResult::RecoveryCleaned { .. }
                | VfsResult::RecoveryWritten { .. } => {
                    self.show_error(self.message("unexpected_completion", &[]))
                }
                VfsResult::Deleted { result, .. } => match result {
                    Err(error) => self.show_error(error),
                    Ok(()) => self.show_error(self.message("unexpected_completion", &[])),
                },
                VfsResult::ExplorerMutated { .. } => {
                    self.show_error("unowned explorer mutation result")
                }
                VfsResult::ResourceApplied { .. }
                | VfsResult::ResourceResolved { .. }
                | VfsResult::CreateTarget { .. } => self.show_error("unowned workspace I/O result"),
                VfsResult::Read { id, path, result } => {
                    if self.search.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_picker_ui();
                        continue;
                    }
                    let pending = self
                        .search
                        .take_matcher_navigation(id)
                        .map(NavigationRead::Matcher)
                        .or_else(|| {
                            self.persistence
                                .take_navigation_read(id)
                                .map(NavigationRead::Persisted)
                        })
                        .or_else(|| {
                            self.language.take_navigation_read(id).map(
                                |(range, encoding, navigation, workspace)| {
                                    NavigationRead::Lsp(range, encoding, navigation, workspace)
                                },
                            )
                        })
                        .or_else(|| {
                            self.search
                                .take_byte_navigation(id)
                                .map(|(range, snapshot)| NavigationRead::ByteRange(range, snapshot))
                        })
                        .or_else(|| {
                            self.recovery.take_read(id, self.workspace.version()).map(
                                |(journal, content)| NavigationRead::Recovered(journal, content),
                            )
                        });
                    if let Some((navigation, capture)) = self.navigation_reads.remove(&id)
                        && (!self.window_session.navigation_current(
                            &navigation,
                            &self.state.window_version(),
                            self.state.active_view(),
                        ) || !self.input.capture_current(&capture))
                    {
                        continue;
                    }
                    if let Some(NavigationRead::Recovered(journal, content)) = &pending {
                        // A read may finish after another command opened this file. Never
                        // replace that buffer, and never bind a rejected path to recovered text.
                        let already_open = self
                            .state
                            .buffers
                            .values()
                            .any(|buffer| buffer.path.as_ref() == Some(&path));
                        let missing = matches!(&result, Err(crate::files::VfsError::Io {
                            operation: "open for reading" | "open scoped path", source, ..
                        }) if source.kind() == std::io::ErrorKind::NotFound);
                        let writable = self.workspace.require_path(&path).is_ok();
                        let (text, destination) = match result {
                            Ok(bytes) if !already_open && writable => {
                                match TextBuffer::from_bytes(&bytes) {
                                    Ok(text) => (text, Some(path)),
                                    Err(_) => (TextBuffer::from_text(""), None),
                                }
                            }
                            Err(_) if missing && !already_open && writable => {
                                (TextBuffer::from_text(""), Some(path))
                            }
                            _ => (TextBuffer::from_text(""), None),
                        };
                        if let Some(path) = &destination {
                            self.record_recent_file(path.clone());
                        }
                        let (buffer, _) = if missing && let Some(path) = destination.as_ref() {
                            let mut candidate = self.state.draft();
                            let opened = match candidate.open_new_document(path.clone()) {
                                Ok(opened) => opened,
                                Err(error) => {
                                    self.show_error(error);
                                    continue;
                                }
                            };
                            if let Err(error) = self.commit_editor_draft(candidate, None, true) {
                                self.show_error(error);
                                continue;
                            }
                            opened
                        } else {
                            match self.state.open_text(text, destination) {
                                Ok(opened) => opened,
                                Err(error) => {
                                    self.show_error(error);
                                    continue;
                                }
                            }
                        };
                        if let Err(error) = self.replace_with_recovered(buffer, content.clone()) {
                            self.show_error(error);
                            continue;
                        }
                        self.finish_recovery(journal, "recovery_restored");
                        continue;
                    }
                    if let Some(NavigationRead::ByteRange(_, Some(snapshot))) = &pending {
                        let current = self
                            .state
                            .buffers
                            .values()
                            .find(|buffer| buffer.path.as_ref() == Some(&path));
                        let unchanged = current.map_or_else(
                            || {
                                result
                                    .as_deref()
                                    .is_ok_and(|bytes| bytes == snapshot.as_ref())
                            },
                            |buffer| buffer.text.matches_encoded_text(snapshot),
                        );
                        if !unchanged {
                            self.show_error(self.message("workspace_search_stale_result", &[]));
                            continue;
                        }
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(NavigationRead::Matcher(source)) => {
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
                                let start = PositionCodec::new(&self.state.buffers[buffer].text)
                                    .lsp_to_byte(source.line, source.column, source.encoding)
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Persisted(location)) => {
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let clamp = |offset: usize| {
                                    let mut offset = offset.min(length);
                                    while !self.state.buffers[buffer]
                                        .text
                                        .is_char_boundary(ByteOffset(offset))
                                    {
                                        offset -= 1;
                                    }
                                    ByteOffset(offset)
                                };
                                let cursor = clamp(location.byte);
                                let anchor = clamp(location.anchor);
                                let selection =
                                    (anchor != cursor).then_some(crate::editor::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editor::SelectionKind::Character,
                                    });
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: cursor,
                                    })
                                    .with_operation(EditorOperation::SetSelection {
                                        view,
                                        selection,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                    continue;
                                }
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].scroll_wrap_row = 0;
                                self.state.views[view].scroll_column = location.scroll_column;
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Lsp(range, encoding, navigation, workspace)) => {
                                if workspace != self.workspace.version() {
                                    continue;
                                }
                                if !self.window_session.navigation_current(
                                    &navigation,
                                    &self.state.window_version(),
                                    self.state.active_view(),
                                ) {
                                    let mut draft = self.state.draft();
                                    let buffer = match draft.load_document(text, path.clone()) {
                                        Ok(buffer) => buffer,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                    if let Err(error) = self.commit_editor_draft(draft, None, false)
                                    {
                                        self.show_error(error);
                                    }
                                    if let Some(buffer) = self.state.buffers.get_mut(buffer) {
                                        buffer.read_only =
                                            self.workspace.require_path(&path).is_err();
                                    }
                                    continue;
                                }
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
                                let position = self.state.buffers.get(buffer).and_then(|buffer| {
                                    PositionCodec::new(&buffer.text)
                                        .lsp_to_byte(
                                            range.start.line,
                                            range.start.character,
                                            encoding,
                                        )
                                        .ok()
                                });
                                if let Some(position) = position {
                                    let transaction = EditorTransaction::new(self.transaction_id())
                                        .with_precondition(Precondition::ViewDisplays {
                                            view,
                                            buffer,
                                        })
                                        .with_operation(EditorOperation::SetCursor {
                                            view,
                                            position,
                                        })
                                        .with_undo_policy(UndoPolicy::Ignore);
                                    if let Err(error) = self.state.apply_transaction(&transaction) {
                                        self.show_error(error);
                                    }
                                } else {
                                    self.show_error(self.message("lsp_invalid_position", &[]));
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::ByteRange(range, snapshot)) => {
                                let start = range.start;
                                let end = range.end;
                                let target = self
                                    .state
                                    .buffers
                                    .values()
                                    .find(|buffer| buffer.path.as_ref() == Some(&path))
                                    .map_or(&text, |buffer| &buffer.text);
                                if snapshot.as_ref().is_some_and(|snapshot| {
                                    !text.matches_encoded_text(snapshot)
                                        || !target.matches_encoded_text(snapshot)
                                }) || target.validate_offset(start).is_err()
                                    || target.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = match self.state.open_text(text, Some(path)) {
                                    Ok(opened) => opened,
                                    Err(error) => {
                                        self.show_error(error);
                                        continue;
                                    }
                                };
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: start,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.input.take_completion_read(id);
                    match (pending, result) {
                        (Some(key), _) if !self.input.completion_current(&key) => {
                            self.refresh_completions();
                        }
                        (Some(key), Ok(paths)) => {
                            self.input.accept_prompt_paths(
                                &key,
                                paths,
                                &self.state.workspace_root,
                                self.configuration.settings(),
                            );
                            self.project_input_prompt();
                        }
                        (Some(_), Err(_)) => {}
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { result, .. } => {
                    if let Err(error) = result {
                        self.show_error(error);
                    } else {
                        self.show_error(self.message("unexpected_completion", &[]));
                    }
                }
            }
        }
        if self.search.preview_needs_retry() && self.documents.check_capacity().is_ok() {
            self.sync_picker_ui();
        }
        if self.workspace_edits.preparing() {
            self.submit_workspace_preparation();
        }
    }
}

impl App {
    fn project_large_file_modes(&mut self, buffers: Vec<crate::editor::BufferId>) {
        self.ui.set(buffers, |ui| &mut ui.large_file_buffers);
    }
}

use crate::app::TrustedAction;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerPayload;
use crate::app::windows::EditorCloseSave;
use crate::language::lsp::LspDocument;
use crate::language::lsp::language_id_for_path;
use crate::tools::managed_formatter;
use crate::ui::render::SCRATCH_LABEL;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    fn document_view(
        id: crate::editor::BufferId,
        buffer: &crate::editor::Buffer,
    ) -> crate::app::documents::DocumentView<'_> {
        crate::app::documents::DocumentView {
            id,
            path: buffer.path.as_deref(),
            text: &buffer.text,
            persisted: buffer.persisted_bytes(),
            new_file: buffer.new_file,
            dirty: buffer.dirty,
            virtual_read_only: buffer.is_virtual_read_only(),
            read_only: buffer.read_only,
        }
    }
}

impl App {
    pub(in crate::app) fn show_document_failure(
        &mut self,
        failure: crate::app::documents::DocumentFailure,
    ) {
        match failure {
            crate::app::documents::DocumentFailure::Policy(key) => {
                self.show_error(self.message(key, &[]))
            }
            crate::app::documents::DocumentFailure::Error(e) => self.show_error(e),
        }
    }
}

impl App {
    /// Open a path the user named. With nothing there it starts a new file, as in Vim, which
    /// the first save creates.
    pub(in crate::app) fn edit_path(&mut self, path: PathBuf) {
        let absolute = self.state.workspace_root.join(&path);
        let missing = matches!(
            std::fs::symlink_metadata(&absolute),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if !missing {
            self.start_open(path);
            return;
        }
        let path = crate::app::startup::new_file_path(&absolute);
        self.record_jump();
        let mut candidate = self.state.draft();
        if let Err(error) = candidate.open_new_document(path.clone()) {
            self.show_error(error);
            return;
        }
        if let Err(error) = self.commit_editor_draft(candidate, None, true) {
            self.show_error(error);
            return;
        }
        let name = self.notice_file_label(&path);
        self.notify_message(self.message("new_file", &[("path", name)]), false);
    }
}

impl App {
    /// The dashboard's Recover Unsaved Work: every journal with the file it was written for.
    /// Enter opens one, ctrl-x (`d` in Normal mode) discards it, Escape leaves them for later.
    pub(in crate::app) fn open_recovery_picker(&mut self) {
        let items = self
            .recovery
            .records()
            .iter()
            .rev()
            .map(|(journal, record)| {
                let label = record.path().as_deref().map_or_else(
                    || SCRATCH_LABEL.to_owned(),
                    |path| self.workspace_relative_label(path),
                );
                let lines = record.content.lines().count();
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("recovery:{}", journal.display())),
                        label,
                        detail: if lines == 1 {
                            "1 line".to_owned()
                        } else {
                            format!("{lines} lines")
                        },
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Recovery(journal.clone()),
                )
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            if self.recovery_picker_open() {
                self.close_picker();
            }
            self.notify_message(self.message("recovery_none", &[]), false);
            return;
        }
        self.install_picker("Recover Unsaved Work".to_owned(), items);
        self.search
            .set_picker_footer("Enter Recover · ctrl-x Discard · Esc Later".to_owned());
        self.sync_picker_ui();
    }
}

impl App {
    pub(in crate::app) fn recovery_picker_open(&self) -> bool {
        matches!(
            self.search.selected_picker_payload(),
            Some(PickerPayload::Recovery(_))
        )
    }
}

impl App {
    /// Recover a journal into its file, as an unsaved change on top of what is on disk, so undo
    /// goes back to the saved version. Work that never had a file, or whose file is already
    /// open, comes back in a new buffer.
    pub(in crate::app) fn recover_journal(&mut self, journal: &Path) {
        let Some(record) = self
            .recovery
            .records()
            .iter()
            .find(|(path, _)| path == journal)
            .map(|(_, record)| record.clone())
        else {
            self.show_error(self.message("recovery_none", &[]));
            return;
        };
        let original = record.path().filter(|path| {
            !self
                .state
                .buffers
                .values()
                .any(|buffer| buffer.path.as_ref() == Some(path))
        });
        if let Some(original) = original {
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            match self.documents.read_scoped(original, roots) {
                Ok(id) => {
                    self.recovery.register_read(
                        id,
                        journal.to_path_buf(),
                        record.content,
                        self.workspace.version(),
                    );
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let (buffer, _) = match self.state.open_text(TextBuffer::from_text(""), None) {
            Ok(opened) => opened,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.replace_with_recovered(buffer, record.content) {
            self.show_error(error);
            return;
        }
        self.finish_recovery(journal, "recovery_opened");
    }
}

impl App {
    pub(in crate::app) fn replace_with_recovered(
        &mut self,
        buffer: crate::editor::BufferId,
        content: String,
    ) -> Result<(), String> {
        let text = &self.state.buffers[buffer].text;
        let revision = text.revision();
        let edits = crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                crate::editor::text::ByteRange::new(ByteOffset(0), ByteOffset(text.len_bytes()))
                    .expect("whole buffer"),
                content,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision { buffer, revision })
            .with_precondition(Precondition::BufferWritable(buffer))
            .with_operation(EditorOperation::ApplyEdits { buffer, edits });
        self.state
            .apply_transaction(&transaction)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl App {
    pub(in crate::app) fn finish_recovery(&mut self, journal: &Path, message: &str) {
        self.set_dashboard_visibility(false);
        self.ensure_cursor_visible();
        self.notify_message(self.message(message, &[]), false);
        if let Some(buffer) = self.state.active_buffer_id() {
            self.recovery.retain_recovered(buffer, journal.to_owned());
        }
    }
}

impl App {
    pub(in crate::app) fn start_open(&mut self, path: PathBuf) {
        self.record_jump();
        match self.documents.request_open(
            path,
            &self.state.workspace_root,
            &self.state.window_version(),
            self.workspace.version(),
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view()),
        ) {
            Ok(()) => self.notify_message(self.message("opening_file", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn start_reload(&mut self, force: bool) {
        let view = self.state.active_buffer_id().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        match self.documents.request_reload(
            view,
            force,
            &self.state.window_version(),
            self.workspace.version(),
        ) {
            Ok(()) => self.notify_message(self.message("reloading", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
}

impl App {
    /// The formatter from the tool manager for the active file, the way conform.nvim comes
    /// before the language server in LazyVim.
    pub(in crate::app) fn active_formatter(&self) -> Option<String> {
        let buffer = &self.state.buffers[self.state.active_buffer_id()?];
        let path = buffer.path.as_deref()?;
        let language = language_id_for_path(path)?;
        Some(managed_formatter(|tool| tool.supports_language(language), path)?.id)
    }
}

impl App {
    /// Ask the language server to format, and save when it answers. False when there is nothing
    /// to wait for, so the caller saves right away.
    pub(in crate::app) fn begin_format_on_save(&mut self, quit_after: bool) -> bool {
        if !self.configuration.settings().lsp.format_on_save {
            return false;
        }
        self.begin_formatting_chain(quit_after, true)
    }

    pub(in crate::app) fn begin_document_format(&mut self) -> bool {
        self.begin_formatting_chain(false, false)
    }

    fn begin_formatting_chain(&mut self, quit_after: bool, save_after: bool) -> bool {
        if let Some(pending) = self
            .state
            .active_buffer_id()
            .and_then(|buffer| self.format_on_save.get_mut(&buffer))
        {
            pending.quit_after |= quit_after;
            pending.authority = pending.authority.restrict(&self.effect_authority);
            return true;
        }
        let Some(buffer) = self.state.active_buffer_id() else {
            return false;
        };
        let chain = self
            .documents
            .formatter_chain(&self.save_filetype(buffer))
            .unwrap_or_else(|| {
                // As in LazyVim, an installed formatter takes precedence over LSP by default.
                if (!save_after || self.workspace.execution_allowed().is_ok())
                    && let Some(tool) = self.active_formatter()
                {
                    return VecDeque::from([save_work::Formatter::Tool(tool)]);
                }
                if self
                    .active_lsp_document_without_text()
                    .is_some_and(|(buffer, document)| {
                        self.language.formatting_server(buffer, &document).is_some()
                    })
                {
                    VecDeque::from([save_work::Formatter::Lsp])
                } else {
                    VecDeque::new()
                }
            });
        if chain.is_empty() {
            return false;
        }
        if !save_after {
            self.documents.save_work.format_only.insert(buffer);
        }
        self.documents
            .save_work
            .remaining_formatters
            .insert(buffer, chain);
        self.format_on_save.insert(
            buffer,
            crate::app::FormatOnSave {
                authority: self.effect_authority.clone(),
                request: None,
                buffer,
                source: self.state.buffers[buffer].text.source_version(),
                path: self.state.buffers[buffer].path.clone(),
                formatter: None,
                started: false,
                quit_after,
                deadline: Instant::now() + std::time::Duration::from_secs(3),
            },
        );
        self.start_next_save_formatter(buffer);
        true
    }
}

impl App {
    pub(in crate::app) fn formatting_save_for_request(
        &self,
        server: &str,
        request_id: u64,
    ) -> Option<crate::editor::BufferId> {
        self.format_on_save.values().find_map(|pending| {
            pending
                .request
                .as_ref()
                .filter(|(key, request)| key == server && *request == request_id)
                .map(|_| pending.buffer)
        })
    }
}

impl App {
    fn start_pending_save_formatters(&mut self) {
        let waiting = self
            .format_on_save
            .values()
            .filter(|pending| !pending.started)
            .filter_map(|pending| Some((pending.buffer, pending.formatter.clone()?)))
            .collect::<Vec<_>>();
        for (buffer, name) in waiting {
            // Task names identify one running formatter. Saves using the same tool wait
            // their turn instead of bypassing formatting or replacing another run.
            if self.tasks.is_running(&name) {
                continue;
            }
            let pending = self.format_on_save.get_mut(&buffer).expect("pending save");
            pending.started = true;
            pending.deadline = Instant::now() + std::time::Duration::from_millis(3000);
            let previous = std::mem::replace(&mut self.effect_authority, pending.authority.clone());
            match crate::app::tasks::FormatterSource::capture(&self.state, buffer) {
                Ok(source) => self.run_trusted(TrustedAction::RunFormatter { name, source }),
                Err(error) => {
                    self.show_error(error);
                    self.finish_format_on_save(buffer);
                }
            }
            self.effect_authority = previous;
        }
    }
}

impl App {
    pub(in crate::app) fn finish_format_on_save(&mut self, buffer: crate::editor::BufferId) {
        let Some(mut pending) = self.format_on_save.remove(&buffer) else {
            return;
        };
        if pending.started
            && let Some(formatter) = &pending.formatter
        {
            self.tasks.stop_formatter(formatter, pending.buffer);
        }
        if let Some((server, request_id)) = pending.request.take() {
            let _ = self.language.cancel(&server, request_id);
            self.language.finish_request(&server, request_id);
        }
        if self
            .state
            .buffers
            .get(pending.buffer)
            .is_some_and(|buffer| {
                buffer.path == pending.path
                    && pending.source.same_instance(&buffer.text.source_version())
            })
        {
            if self
                .documents
                .save_work
                .remaining_formatters
                .get(&buffer)
                .is_some_and(|steps| !steps.is_empty())
            {
                pending.source = self.state.buffers[buffer].text.source_version();
                self.format_on_save.insert(buffer, pending);
                self.start_next_save_formatter(buffer);
                return;
            }
            self.documents
                .save_work
                .remaining_formatters
                .remove(&buffer);
            if self.documents.save_work.format_only.remove(&buffer) {
                return;
            }
            let previous = std::mem::replace(&mut self.effect_authority, pending.authority);
            match self.request_save_buffer(pending.buffer, pending.quit_after, None, false, None) {
                Ok(_) => self.notify_message(self.message("saving", &[]), false),
                Err(error) => self.show_document_failure(error),
            }
            self.effect_authority = previous;
        } else {
            self.show_error(self.message("format_on_save_skipped", &[]));
        }
    }
}

impl App {
    /// A commit or stage made outside Dust, such as in its terminal, moves the index or HEAD.
    fn follow_outside_git_changes(&mut self) {
        let Some(status) = self.git.status() else {
            return;
        };
        let Ok(head) = crate::git::git_head_id(&status.repository) else {
            return;
        };
        let metadata = ["index", "HEAD"].map(|name| {
            std::fs::metadata(status.git_dir.join(name))
                .and_then(|file| file.modified())
                .ok()
        });
        let stamp = (metadata, head);
        if self
            .git_stamp
            .replace(stamp)
            .is_some_and(|previous| previous != stamp)
        {
            self.start_git_status_refresh_with_feedback(false);
        }
    }
}

impl App {
    /// Once a second, notice open files that changed outside Dust: lazygit, a hunk reset, a
    /// formatter. A clean buffer takes the new text; a changed one keeps its text and says so.
    pub(in crate::app) fn process_disk_changes(&mut self) {
        let now = self.clock.monotonic();
        if self.next_disk_check.is_some_and(|next| now < next) || self.documents.file_work_pending()
        {
            return;
        }
        self.next_disk_check = Some(now + std::time::Duration::from_secs(1));
        self.follow_outside_git_changes();
        let open: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.virtual_owner().is_none())
            .filter_map(|(id, buffer)| Some((id, buffer.path.clone()?)))
            .collect();
        self.disk_stamps
            .retain(|id, _| open.iter().any(|(open, _)| open == id));
        for (id, path) in open {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let stamp = (metadata.modified().ok(), metadata.len());
            if !metadata.is_file() {
                continue;
            }
            if self.disk_stamps.get(&id) == Some(&stamp) {
                continue;
            }
            if matches!(
                self.documents.check_capacity(),
                Err(crate::files::VfsError::QueueFull)
            ) {
                break;
            }
            let editor = self.state.window_version();
            let workspace = self.workspace.version();
            let view = Self::document_view(id, &self.state.buffers[id]);
            match self.documents.request_disk_check(view, &editor, workspace) {
                Ok(()) => {
                    self.reload_stamps.insert(id, stamp);
                }
                Err(error) => self.show_document_failure(error),
            }
        }
    }
}

impl App {
    pub(in crate::app) fn process_format_on_save_deadline(&mut self) {
        let expired = self
            .format_on_save
            .values()
            .filter(|pending| {
                pending.started
                    && Instant::now() >= pending.deadline
                    && !self.documents.save_work_pending(pending.buffer)
            })
            .map(|pending| pending.buffer)
            .collect::<Vec<_>>();
        for buffer in expired {
            self.finish_format_on_save(buffer);
        }
        self.start_pending_save_formatters();
    }
}

impl App {
    pub(in crate::app) fn start_save(
        &mut self,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
    ) {
        let Some(buffer) = self.state.active_buffer_id() else {
            self.show_document_failure(crate::app::documents::DocumentFailure::Policy(
                "no_active_save",
            ));
            return;
        };
        // Said up front, because the write would only report the temporary file it could not make.
        if let Some(requested) = requested_path.as_deref() {
            let folder = requested
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty());
            if let Some(folder) = folder.filter(|f| !self.state.workspace_root.join(f).is_dir()) {
                let name = requested.file_name().unwrap_or_default().to_string_lossy();
                self.show_error(format!(
                    "can't save {name}: the folder {} doesn't exist",
                    folder.display()
                ));
                return;
            }
        }
        match self.request_save_buffer(buffer, quit_after, requested_path, force, None) {
            Ok(_) => self.notify_message(self.message("saving", &[]), false),
            Err(error) => self.show_document_failure(error),
        }
    }
}

impl App {
    /// Every changed file, without waiting on formatters. Quitting happens when the last save
    /// lands, and only if nothing is left unsaved by then.
    pub(in crate::app) fn save_all(&mut self, quit_after: bool) {
        let changed: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(id, buffer)| {
                buffer.dirty
                    && (self.documents.editable_document(*id)
                        || buffer.path.is_some() && buffer.virtual_owner().is_none())
            })
            .map(|(id, _)| id)
            .collect();
        let Some(last) = changed.last().copied() else {
            if quit_after {
                self.process_effect(AppEffect::Quit { force: false });
            } else {
                self.notify_message(self.message("save_all_nothing", &[]), false);
            }
            return;
        };
        self.notify_message(self.message("saving", &[]), false);
        for buffer in changed {
            let quit = quit_after && buffer == last;
            if let Some((_, pending_quit, authority)) = self
                .pending_saves
                .iter_mut()
                .find(|(id, _, _)| *id == buffer)
            {
                *pending_quit |= quit;
                *authority = authority.restrict(&self.effect_authority);
            } else {
                self.pending_saves
                    .push_back((buffer, quit, self.effect_authority.clone()));
            }
        }
        self.process_pending_saves();
    }
}

impl App {
    pub(in crate::app) fn process_pending_saves(&mut self) {
        while !self.pending_saves.is_empty() {
            match self.documents.check_capacity() {
                Ok(()) => {}
                Err(crate::files::VfsError::QueueFull) => break,
                Err(error) => {
                    self.pending_saves.clear();
                    self.show_error(error);
                    break;
                }
            }
            let (buffer, quit, authority) = self.pending_saves.pop_front().expect("pending save");
            let previous = std::mem::replace(&mut self.effect_authority, authority);
            if let Err(error) = self.request_save_buffer(buffer, quit, None, false, None) {
                self.show_document_failure(error);
            }
            self.effect_authority = previous;
        }
    }
}

impl App {
    pub(in crate::app) fn request_save_buffer(
        &mut self,
        buffer: crate::editor::BufferId,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
        close: Option<EditorCloseSave>,
    ) -> Result<Option<crate::files::RequestId>, crate::app::documents::DocumentFailure> {
        if self.begin_save_work(
            buffer,
            quit_after,
            requested_path.clone(),
            force,
            close.clone(),
        )? {
            return Ok(None);
        }
        self.request_disk_save(buffer, quit_after, requested_path, force, close)
            .map(Some)
    }

    fn request_disk_save(
        &mut self,
        buffer: crate::editor::BufferId,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
        close: Option<EditorCloseSave>,
    ) -> Result<crate::files::RequestId, crate::app::documents::DocumentFailure> {
        let view = self
            .state
            .buffers
            .get(buffer)
            .map(|candidate| Self::document_view(buffer, candidate));
        let request = crate::app::documents::SaveRequest {
            authority: self.effect_authority.clone(),
            editor: &self.state.window_version(),
            workspace: self.workspace.version(),
            base: &self.state.workspace_root,
            path: requested_path,
            policy: &self.configuration.settings().editor,
            force,
            quit: quit_after,
            close,
            other_open_paths: self
                .state
                .buffers
                .iter()
                .filter(|(id, _)| *id != buffer)
                .filter_map(|(_, candidate)| candidate.path.clone())
                .collect(),
        };
        let result = self.documents.request_save(view, request);
        if result.is_err() {
            self.effect_failed = true;
        }
        result
    }
}

impl App {
    pub(in crate::app) fn close_active_buffer(&mut self, force: bool) {
        let Some(view) = self.state.active_view() else {
            self.set_dashboard_visibility(true);
            return;
        };
        let buffer = self.state.views[view].buffer;
        if !force {
            self.reconcile_editor_windows();
            match self
                .window_session
                .begin_buffer_close(&(&*self.state).into(), view, buffer)
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
            return;
        }
        self.close_buffer_target(view, buffer);
    }
}

impl App {
    pub(in crate::app) fn finish_buffer_close(
        &mut self,
        buffer_id: crate::editor::BufferId,
        path: Option<&Path>,
        clean: bool,
    ) {
        self.recovery.forget_buffer(buffer_id, clean);
        self.documents.forget_save_work(buffer_id);
        self.documents.cancel_buffer_requests(buffer_id, path);
        if let Some(path) = path
            && let Some(id) = self.explorer.exclude_buffer_path(path)
        {
            self.documents.cancel_request(id);
        }
        match self.extension.close_virtual_buffer(buffer_id) {
            Ok(Some(request)) => self.route_virtual_open(Ok(request)),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        let documents = self
            .language
            .sessions()
            .iter()
            .filter_map(|(server, state)| {
                state
                    .documents
                    .get(&buffer_id)
                    .map(|document| (server.clone(), document.uri()))
            })
            .collect::<Vec<_>>();
        self.detach_lsp_documents(buffer_id, documents);
    }
}

impl App {
    pub(in crate::app) fn close_buffer_target(
        &mut self,
        view_id: crate::editor::ViewId,
        buffer_id: crate::editor::BufferId,
    ) {
        let clean = self
            .state
            .buffers
            .get(buffer_id)
            .is_some_and(|buffer| !buffer.dirty);
        let closed_path = self
            .state
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.path.clone());
        if let Err(error) = self
            .window_session
            .check_editor_source(&(&*self.state).into())
        {
            self.show_error(error);
            return;
        }
        let mut candidate = self.state.draft();
        let removed = match candidate.close_document(view_id, buffer_id) {
            Ok(removed) => removed,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.commit_editor_draft(candidate, None, false) {
            self.show_error(error);
            return;
        }
        if removed {
            self.finish_buffer_close(buffer_id, closed_path.as_deref(), clean);
        }
        self.set_dashboard_visibility(self.state.active_view().is_none());
        if !self.window_session.dashboard_visible() {
            self.sync_lsp_document();
        } else {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        self.notify_message(
            if self.window_session.dashboard_visible() {
                self.message("returned_dashboard", &[])
            } else {
                self.message("buffer_closed", &[])
            },
            false,
        );
    }
}

impl App {
    pub(in crate::app) fn process_file_reply(&mut self, reply: crate::app::documents::FileReply) {
        use crate::app::documents::DocumentChange;
        let stage = self.input.take_stage_save(reply.id());
        let close_save = reply.close_save().cloned();
        if let Err(error) = self
            .window_session
            .check_editor_source(&(&*self.state).into())
        {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_error(error);
            return;
        }
        if let Err(error) = reply.check_save_destination(
            self.state
                .buffers
                .iter()
                .filter_map(|(id, b)| b.path.as_deref().map(|path| (id, path))),
        ) {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_document_failure(error);
            return;
        }
        let current = self.state.version();
        let view = reply.target().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        let change = match self
            .documents
            .prepare_file(reply, &current, &self.workspace.version(), view)
            .and_then(|plan| plan.into_change(&current))
        {
            Ok(change) => change,
            Err(e) => {
                if let Some(token) = &close_save {
                    self.window_session.fail_editor_close_save(token);
                }
                self.show_document_failure(e);
                return;
            }
        };
        match change {
            DocumentChange::Open {
                text,
                path,
                line,
                navigation,
                activate,
            } => {
                let mut candidate = self.state.draft();
                let activate = activate
                    && self.window_session.navigation_current(
                        &navigation,
                        &self.state.window_version(),
                        self.state.active_view(),
                    );
                if activate {
                    if let Err(error) = candidate.open_document(text, path.clone()) {
                        self.show_error(error);
                        return;
                    }
                } else {
                    if let Err(error) = candidate.load_document(text, path.clone()) {
                        self.show_error(error);
                        return;
                    }
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, activate) {
                    self.show_error(e);
                    return;
                }
                if activate && let (Some(line), Some(view)) = (line, self.state.active_view()) {
                    let text = &self.state.buffers[self.state.views[view].buffer].text;
                    let position = text
                        .line_to_byte(line.min(text.visible_lines().saturating_sub(1)))
                        .unwrap_or(ByteOffset(0));
                    let transaction = EditorTransaction::new(self.transaction_id())
                        .with_operation(EditorOperation::SetCursor { view, position })
                        .with_undo_policy(UndoPolicy::Ignore);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                    }
                    self.ensure_cursor_visible();
                }
                self.record_recent_file(path.clone());
                // "Opening file…" is done once the file is there.
                if self.ui.notification.as_deref()
                    == Some(self.message("opening_file", &[]).as_str())
                {
                    self.clear_notice();
                }
            }
            DocumentChange::DiskChecked {
                buffer,
                path,
                changed,
            } => {
                if let Some(stamp) = self.reload_stamps.remove(&buffer) {
                    self.disk_stamps.insert(buffer, stamp);
                }
                if changed {
                    let shown = self.workspace_relative_label(&path);
                    self.notify_warning(self.message("file_changed_on_disk", &[("path", shown)]));
                }
            }
            DocumentChange::Reload { buffer, text, path } => {
                let mut candidate = self.state.draft();
                let stamp = self.reload_stamps.remove(&buffer);
                if !candidate.reload_document(buffer, text, path.clone()) {
                    self.show_error(self.message("reload_closed", &[]));
                    return;
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(e);
                    return;
                }
                if let Some(stamp) = stamp {
                    self.disk_stamps.insert(buffer, stamp);
                }
                self.git.forget_hunks();
                self.start_git_status_refresh_with_feedback(false);
                let views = self
                    .state
                    .views
                    .iter()
                    .filter(|(_, view)| view.buffer == buffer)
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();
                for view in views {
                    self.keep_view_cursor_off_final_newline(view);
                }
                self.notify_message(
                    self.message("reloaded_file", &[("path", self.notice_file_label(&path))]),
                    false,
                );
            }
            DocumentChange::Saved {
                buffer,
                text,
                revision,
                bytes,
                snapshot,
                line_ending,
                target: path,
                quit: quit_after,
                close,
            } => {
                let mut candidate = self.state.draft();
                if !candidate.save_document(buffer, &text, revision, bytes, path.clone(), *snapshot)
                {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    return;
                }
                if let Some(ending) = line_ending {
                    candidate.set_document_line_ending(buffer, ending);
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    self.show_error(e);
                    return;
                }
                // A staged save that was waiting on this completion replays now, against the
                // saved bytes, using the exact effect that was refused.
                let buffer_state = &self.state.buffers[buffer];
                let still_dirty = buffer_state.dirty;
                if let (Some(directory), false) = (&self.state.undo_directory, still_dirty) {
                    self.persistence.track_undo_save(
                        path.clone(),
                        crate::editor::save_undo_history(
                            directory.clone(),
                            path.clone(),
                            buffer_state.text.clone(),
                            buffer_state.undo.clone(),
                            self.state.undo_maximum_bytes,
                        ),
                    );
                }
                let lsp_document = buffer_state.path.clone().and_then(|path| {
                    Some(LspDocument {
                        language_id: language_id_for_path(&path)?.into(),
                        path,
                        version: revision.0,
                        text: crate::editor::text::decode_utf8(buffer_state.persisted_bytes())
                            .ok()?
                            .text,
                    })
                });
                self.git.forget_hunks();
                self.notify_message(
                    if still_dirty {
                        self.message(
                            "saved_dirty",
                            &[
                                ("revision", revision.0.to_string()),
                                ("path", self.notice_file_label(&path)),
                            ],
                        )
                    } else {
                        self.message("saved", &[("path", self.notice_file_label(&path))])
                    },
                    false,
                );
                if let Some(document) = lsp_document {
                    let current = LspDocument {
                        text: self.state.buffers[buffer].text.to_string(),
                        version: self.state.buffers[buffer].text.revision().0,
                        ..document.clone()
                    };
                    let source = self.state.buffers[buffer].text.source_version();
                    let servers = self
                        .language
                        .sessions()
                        .iter()
                        .filter_map(|(key, state)| {
                            state
                                .documents
                                .get(&buffer)
                                .filter(|owned| owned.uri() == document.uri())
                                .map(|_| key.clone())
                        })
                        .collect::<Vec<_>>();
                    for server in servers {
                        // Formatting and save policies can change text since the last sync.
                        // Publish the current buffer before didSave, never the older disk snapshot.
                        if !self.language.synced(&server, buffer, &source)
                            && let Err(error) = self.language.sync_owned(
                                &server,
                                current.clone(),
                                (buffer, source.clone()),
                            )
                        {
                            self.show_error(error);
                        }
                        if let Err(error) = self.language.save(&server, document.clone()) {
                            self.show_error(error);
                        }
                    }
                }
                if let Some((effect, line)) = stage.filter(|_| !still_dirty) {
                    self.retry_saved_stage(buffer, &path, effect, line);
                } else {
                    self.start_git_status_refresh_with_feedback(false);
                }
                if let Some(token) = close {
                    if still_dirty {
                        self.window_session.fail_editor_close_save(&token);
                        self.show_error(
                            "document changed while its close save was pending; close was cancelled",
                        );
                    } else {
                        match self
                            .window_session
                            .finish_editor_close_save(&token, &(&*self.state).into())
                        {
                            Ok(step) => self.handle_editor_close_step(step),
                            Err(error) => self.show_error(error),
                        }
                    }
                }
                if quit_after && !still_dirty {
                    if self
                        .state
                        .buffers
                        .iter()
                        .any(|(_, candidate)| candidate.dirty)
                    {
                        self.show_error(self.message("saved_others_dirty", &[]));
                    } else {
                        self.should_quit = true;
                    }
                }
            }
        }
    }
}

/// Create a user settings or keymaps file if it is missing or empty. A new file starts with a
/// few commented examples rather than nothing.
pub(in crate::app) fn create_user_file(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    let starter = if path.file_name().is_some_and(|name| name == "keymaps.toml") {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/defaults/keymaps-starter.toml"
        ))
    } else {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/defaults/settings-starter.toml"
        ))
    };
    if std::fs::metadata(path)?.len() == 0 {
        std::fs::write(path, starter)?;
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::app) mod tests;
