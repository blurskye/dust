//! Document-owned file/startup intents and policy, with bounded VFS admission/drain.
//! Other feature payloads still live in legacy dispatch. Queued read/write cancellation
//! and shutdown are nonblocking; running filesystem calls are not interruptible.
mod lifecycle;
mod task_io;
use crate::editor::text::TextBuffer;
use crate::editor::{BufferId, EditorWindowVersion, StartupTarget};
use crate::files::{
    CheckedResourceIntent, ExplorerMutation, RecoveryRecord, RequestId, VfsError, VfsResult,
    VfsWorker,
};
#[cfg(test)]
pub(in crate::editor::session) use lifecycle::configured_file_bytes;
pub(in crate::editor::session) use lifecycle::{
    DocumentChange, DocumentFailure, DocumentView, FileReply, SaveRequest,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
pub(in crate::editor::session) use task_io::{
    TaskCancellation, TaskReadKey, TaskReadReply, TaskReadStopped, TaskSubmission,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum LargeFileOverride {
    Auto,
    On,
    Off,
}
pub(in crate::editor::session) fn mode_name_large(mode: LargeFileOverride) -> &'static str {
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
pub(in crate::editor::session) struct StartupReceipt {
    owner: Arc<()>,
    request: RequestId,
}
pub(in crate::editor::session) enum DocumentEvent {
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
        receipt: StartupReceipt,
        path: PathBuf,
        text: Result<TextBuffer, String>,
    },
    Discarded,
}
pub(super) struct DocumentSession {
    vfs: VfsWorker,
    owner: Arc<()>,
    startup: VecDeque<(PathBuf, bool)>,
    pending_startup: HashMap<RequestId, StartupIntent>,
    pending_legacy: HashSet<RequestId>,
    pending_workspace: HashSet<RequestId>,
    pending_tasks: HashSet<RequestId>,
    pending_files: HashMap<RequestId, lifecycle::FileIntent>,
    open_generation: Arc<()>,
    maximum_pending: usize,
    closing: bool,
    stopped: bool,
    primary: Option<BufferId>,
    editor: EditorWindowVersion,
    root_revision: u64,
    policy_editor: EditorWindowVersion,
    overrides: HashMap<BufferId, LargeFileOverride>,
    restored_overrides: HashMap<PathBuf, LargeFileOverride>,
}
impl DocumentSession {
    pub(in crate::editor::session) fn new(
        vfs: VfsWorker,
        startup: &StartupTarget,
        editor: EditorWindowVersion,
        root_revision: u64,
        maximum_pending: usize,
    ) -> Self {
        let startup = match startup {
            StartupTarget::Dashboard { .. } => VecDeque::new(),
            StartupTarget::File {
                path,
                additional_files,
                ..
            } => std::iter::once((path.clone(), true))
                .chain(additional_files.iter().cloned().map(|path| (path, false)))
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
            open_generation: Arc::new(()),
            maximum_pending: maximum_pending.max(1),
            closing: false,
            stopped: false,
            primary: None,
            policy_editor: editor.clone(),
            editor,
            root_revision,
            overrides: HashMap::new(),
            restored_overrides: HashMap::new(),
        }
    }
    fn startup_current(&self, editor: &EditorWindowVersion, root_revision: u64) -> bool {
        editor.revision().is_some()
            && self.editor.same_editor(editor)
            && self.root_revision == root_revision
    }
    pub(in crate::editor::session) fn has_startup_work(&self) -> bool {
        !self.startup.is_empty() || !self.pending_startup.is_empty()
    }
    pub(in crate::editor::session) fn queue_startup(
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
            if self.check_admission().is_err() {
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
    pub(in crate::editor::session) fn poll(
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
                    receipt: StartupReceipt {
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
            VfsResult::ResourceResolved { id, .. }
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
    pub(in crate::editor::session) fn finish_startup(
        &mut self,
        receipt: StartupReceipt,
        buffer: Option<BufferId>,
        editor: &EditorWindowVersion,
        root_revision: u64,
    ) -> bool {
        if !Arc::ptr_eq(&self.owner, &receipt.owner) {
            return false;
        }
        let Some(intent) = self.pending_startup.remove(&receipt.request) else {
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
    pub(in crate::editor::session) fn take_startup_primary(
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
    pub(in crate::editor::session) fn configured_mode(
        &self,
        buffer: BufferId,
    ) -> LargeFileOverride {
        self.overrides
            .get(&buffer)
            .copied()
            .unwrap_or(LargeFileOverride::Auto)
    }
    pub(in crate::editor::session) fn effective_large_file(
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
                    && (text.len_bytes() >= policy.byte_threshold
                        || text.len_lines() >= policy.line_threshold)
            }),
        }
    }
    pub(in crate::editor::session) fn set_large_file_mode(
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
    pub(in crate::editor::session) fn reconcile_policy<'a>(
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
    pub(in crate::editor::session) fn policy_overrides(
        &self,
    ) -> impl Iterator<Item = (BufferId, LargeFileOverride)> + '_ {
        self.overrides.iter().map(|(id, mode)| (*id, *mode))
    }
    pub(in crate::editor::session) fn restore_policy(
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
    pub(in crate::editor::session) fn enqueue_startup_fixture(
        &mut self,
        path: PathBuf,
        primary: bool,
    ) {
        self.startup.push_back((path, primary));
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn startup_pending(&self) -> usize {
        self.pending_startup.len()
    }
    pub(in crate::editor::session) fn retire_startup(&mut self) {
        self.startup.clear();
        self.primary = None;
        for id in self.pending_startup.keys() {
            self.vfs.cancel(*id);
        }
        self.pending_startup.clear();
    }
    pub(in crate::editor::session) fn close_admission(&mut self) {
        self.closing = true;
        self.startup.clear();
    }
    pub(in crate::editor::session) fn has_work(&self) -> bool {
        !self.pending_files.is_empty()
            || !self.pending_legacy.is_empty()
            || !self.pending_workspace.is_empty()
            || !self.pending_tasks.is_empty()
            || self.has_startup_work()
            || self.vfs.has_outstanding()
    }
    pub(in crate::editor::session) fn stop_worker(&mut self) {
        self.closing = true;
        self.vfs.begin_shutdown();
    }
    pub(in crate::editor::session) fn worker_finished(&self) -> bool {
        self.vfs.shutdown_finished()
    }
    pub(in crate::editor::session) fn check_admission(&self) -> Result<(), VfsError> {
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
    pub(in crate::editor::session) fn read(
        &mut self,
        path: PathBuf,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.read(path)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn read_scoped(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.read_scoped(path, canonical_roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    // Unlike general retirement, a failed queued cancellation must retain the
    // routing obligation for an already-running or already-completed resource.
    pub(in crate::editor::session) fn cancel_workspace_queued(
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
    pub(in crate::editor::session) fn cancel_request(&mut self, id: RequestId) -> bool {
        (self.pending_legacy.remove(&id) | self.pending_workspace.remove(&id))
            && self.vfs.cancel(id)
    }
    pub(in crate::editor::session) fn read_scoped_bounded(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self
            .vfs
            .read_scoped_bounded(path, canonical_roots, maximum_bytes)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn resolve_resource(
        &mut self,
        source: PathBuf,
        target: Option<PathBuf>,
        canonical_roots: Vec<PathBuf>,
        maximum_bytes: usize,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self
            .vfs
            .resolve_resource(source, target, canonical_roots, maximum_bytes)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn apply_resource(
        &mut self,
        intent: CheckedResourceIntent,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.apply_resource(intent)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn open_explorer(
        &mut self,
        path: PathBuf,
        roots: Vec<crate::workspace::RootIdentity>,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.open_explorer(path, roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::editor::session) fn prepare_explorer(
        &mut self,
        operation: ExplorerMutation,
        roots: Vec<crate::workspace::RootIdentity>,
        maximum: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.prepare_explorer(operation, roots, maximum)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
    pub(in crate::editor::session) fn apply_explorer(
        &mut self,
        intent: crate::files::PreparedExplorerMutation,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.apply_explorer(intent)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn resolve_create(
        &mut self,
        path: PathBuf,
        canonical_roots: Vec<PathBuf>,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.resolve_create(path, canonical_roots)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn list_directory(
        &mut self,
        path: PathBuf,
        maximum_entries: usize,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.list_directory(path, maximum_entries)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn discover_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.discover_recovery(directory, maximum_records)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn write_recovery(
        &mut self,
        path: PathBuf,
        record: RecoveryRecord,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.write_recovery(path, record)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn cleanup_recovery(
        &mut self,
        directory: PathBuf,
        maximum_records: usize,
        maximum_age_days: u64,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self
            .vfs
            .cleanup_recovery(directory, maximum_records, maximum_age_days)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }

    pub(in crate::editor::session) fn delete(
        &mut self,
        path: PathBuf,
    ) -> Result<RequestId, VfsError> {
        self.check_admission()?;
        let id = self.vfs.delete(path)?;
        self.pending_legacy.insert(id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorState;
    fn fixture() -> (EditorState, DocumentSession) {
        let state = EditorState::new("/workspace".into());
        let startup = StartupTarget::Dashboard {
            root: "/workspace".into(),
        };
        let owner =
            DocumentSession::new(VfsWorker::start(2), &startup, state.window_version(), 7, 2);
        (state, owner)
    }
    #[test]
    fn startup_reply_is_exact_once_and_rejects_root_or_editor_replacement_before_decode() {
        let (state, mut owner) = fixture();
        // Owner-local fake delivery avoids filesystem timing and proves admission
        // precedes UTF-8 decoding: invalid bytes must be discarded, not decoded.
        let id = RequestId(91);
        owner.pending_startup.insert(
            id,
            StartupIntent {
                primary: true,
                delivered: false,
            },
        );
        let stale = owner.route(
            VfsResult::Read {
                id,
                path: "old".into(),
                result: Ok(vec![255]),
            },
            &state.window_version(),
            8,
        );
        assert!(matches!(stale, DocumentEvent::Discarded));
        assert!(!owner.has_startup_work());
        owner.pending_startup.insert(
            id,
            StartupIntent {
                primary: true,
                delivered: false,
            },
        );
        let foreign = EditorState::new("/workspace".into());
        assert!(matches!(
            owner.route(
                VfsResult::Read {
                    id,
                    path: "old".into(),
                    result: Ok(vec![255])
                },
                &foreign.window_version(),
                7
            ),
            DocumentEvent::Discarded
        ));
        owner.pending_startup.insert(
            id,
            StartupIntent {
                primary: true,
                delivered: false,
            },
        );
        owner.retire_startup();
        let materializations = TextBuffer::test_materializations();
        assert!(matches!(
            owner.route(
                VfsResult::Read {
                    id,
                    path: "old".into(),
                    result: Ok(vec![255])
                },
                &state.window_version(),
                7
            ),
            DocumentEvent::Discarded
        ));
        assert_eq!(TextBuffer::test_materializations(), materializations);
        let mut exhausted = state.clone();
        exhausted.exhaust_window_source_for_test();
        owner.pending_startup.insert(
            id,
            StartupIntent {
                primary: true,
                delivered: false,
            },
        );
        let materializations = TextBuffer::test_materializations();
        assert!(matches!(
            owner.route(
                VfsResult::Read {
                    id,
                    path: "exhausted".into(),
                    result: Ok(vec![255])
                },
                &exhausted.window_version(),
                7
            ),
            DocumentEvent::Discarded
        ));
        assert_eq!(TextBuffer::test_materializations(), materializations);
        owner.pending_startup.insert(
            id,
            StartupIntent {
                primary: true,
                delivered: false,
            },
        );
        let event = owner.route(
            VfsResult::Read {
                id,
                path: "first".into(),
                result: Ok(b"hello".to_vec()),
            },
            &state.window_version(),
            7,
        );
        let DocumentEvent::Startup { receipt, text, .. } = event else {
            panic!("startup event");
        };
        assert_eq!(text.unwrap().to_string(), "hello");
        assert!(matches!(
            owner.route(
                VfsResult::Read {
                    id,
                    path: "duplicate".into(),
                    result: Ok(vec![255])
                },
                &state.window_version(),
                7
            ),
            DocumentEvent::Discarded
        ));
        assert!(owner.finish_startup(receipt, None, &state.window_version(), 7));
        assert!(!owner.has_startup_work());
        assert!(matches!(
            owner.route(
                VfsResult::Read {
                    id,
                    path: "late".into(),
                    result: Ok(vec![255])
                },
                &state.window_version(),
                7
            ),
            DocumentEvent::Discarded
        ));
    }
    #[test]
    fn total_outstanding_admission_is_bounded_and_policy_does_not_follow_reused_buffer_ids() {
        let (mut state, mut owner) = fixture();
        owner.pending_legacy.extend([RequestId(1), RequestId(2)]);
        assert!(matches!(
            owner.read("not-submitted".into()),
            Err(VfsError::QueueFull)
        ));
        owner.route(
            VfsResult::Read {
                id: RequestId(1),
                path: "one".into(),
                result: Err(VfsError::QueueFull),
            },
            &state.window_version(),
            7,
        );
        assert!(owner.check_admission().is_ok());
        let (id, _) = state.open_text(TextBuffer::from_text("tiny"), None);
        let policy = crate::config::LargeFileConfig::default();
        owner.set_large_file_mode(id, None, LargeFileOverride::On);
        assert_eq!(
            owner.reconcile_policy(
                &state.window_version(),
                state
                    .buffers
                    .iter()
                    .map(|(id, b)| (id, b.path.as_deref(), &b.text)),
                &policy
            ),
            vec![id]
        );
        let mut foreign = EditorState::new("/workspace".into());
        let (reused, _) = foreign.open_text(TextBuffer::from_text("tiny"), None);
        assert_eq!(id, reused);
        assert!(
            owner
                .reconcile_policy(
                    &foreign.window_version(),
                    foreign
                        .buffers
                        .iter()
                        .map(|(id, b)| (id, b.path.as_deref(), &b.text)),
                    &policy
                )
                .is_empty()
        );
    }
}

// Admissions are constructed only by this bounded router after the VFS result.
// Constructed and consumed inside submission while the source loan is live.
// Callers cannot fabricate an enqueue acknowledgement or detach its permit.
pub(in crate::editor::session) struct WorkspaceSubmission {
    worker: Arc<()>,
    result: Result<RequestId, String>,
}
impl WorkspaceSubmission {
    pub(in crate::editor::session) fn into_parts(self) -> (Arc<()>, Result<RequestId, String>) {
        (self.worker, self.result)
    }
}
pub(in crate::editor::session) struct WorkspaceCancellation {
    worker: Arc<()>,
    id: RequestId,
}
impl WorkspaceCancellation {
    pub(in crate::editor::session) fn id(&self) -> RequestId {
        self.id
    }
    pub(in crate::editor::session) fn is_from(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
}
pub(in crate::editor::session) struct WorkspaceIoReply {
    worker: Arc<()>,
    result: VfsResult,
}
impl WorkspaceIoReply {
    pub(in crate::editor::session) fn id(&self) -> RequestId {
        self.result.request_id()
    }
    pub(in crate::editor::session) fn is_from(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
    pub(in crate::editor::session) fn result(&self) -> &VfsResult {
        &self.result
    }
    pub(in crate::editor::session) fn into_result(self) -> VfsResult {
        self.result
    }
}
impl DocumentSession {
    pub(in crate::editor::session) fn submit_workspace_io(
        &mut self,
        permit: super::workspace_edits::IoPermit<'_>,
    ) -> Result<(), String> {
        let result = (|| {
            self.check_admission().map_err(|e| e.to_string())?;
            let file = permit.file(&self.owner)?;
            use super::workspace_edits::InputFile;
            match file {
                InputFile::Read(path) => {
                    self.read_scoped_bounded(path, permit.roots().to_vec(), permit.maximum() as u64)
                }
                InputFile::Create(path) => self.resolve_create(path, permit.roots().to_vec()),
                InputFile::Resource { source, target } => {
                    self.resolve_resource(source, target, permit.roots().to_vec(), permit.maximum())
                }
            }
            .map_err(|e| e.to_string())
        })();
        if let Ok(id) = &result {
            self.pending_legacy.remove(id);
            self.pending_workspace.insert(*id);
        }
        permit.finish(WorkspaceSubmission {
            worker: self.owner.clone(),
            result,
        })
    }
    pub(in crate::editor::session) fn submit_workspace_resource(
        &mut self,
        mut permit: super::workspace_edits::ResourcePermit<'_>,
    ) -> Result<(), String> {
        let result = (|| {
            self.check_admission().map_err(|e| e.to_string())?;
            let intent = permit.take_intent(&self.owner)?;
            self.apply_resource(intent).map_err(|e| e.to_string())
        })();
        if let Ok(id) = &result {
            self.pending_legacy.remove(id);
            self.pending_workspace.insert(*id);
        }
        permit.finish(WorkspaceSubmission {
            worker: self.owner.clone(),
            result,
        })
    }
}
