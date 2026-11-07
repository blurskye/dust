//! Document read, save and close state transitions.

use crate::app::documents::DocumentEvent;
use crate::app::documents::Documents;
use crate::app::windows::NavigationStamp;
use crate::app::workspace::WorkspaceVersion;
use crate::editor::BufferId;
use crate::editor::EditorVersion;
use crate::editor::EditorWindowVersion;
use crate::editor::text::BufferRevision;
use crate::editor::text::FileFormat;
use crate::editor::text::LineEnding;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextVersion;
use crate::files::RequestId;
use crate::files::VfsError;
use crate::files::VfsResult;
use crate::files::WriteExpectation;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(in crate::app) struct DocumentView<'a> {
    pub id: BufferId,
    pub path: Option<&'a Path>,
    pub text: &'a TextBuffer,
    pub persisted: &'a [u8],
    pub new_file: bool,
    pub dirty: bool,
    pub virtual_read_only: bool,
    pub read_only: bool,
}
#[derive(Clone)]
struct Version {
    id: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
    revision: BufferRevision,
    snapshot: Option<Box<(Arc<str>, FileFormat)>>,
}
impl DocumentView<'_> {
    fn version(&self) -> Version {
        Version {
            id: self.id,
            path: self.path.map(Path::to_path_buf),
            text: self.text.source_version(),
            revision: self.text.revision(),
            snapshot: None,
        }
    }
    fn save_version(&self) -> Version {
        let mut version = self.version();
        version.snapshot = Some(Box::new((
            self.text.to_string().into(),
            self.text.format().clone(),
        )));
        version
    }
}
pub(in crate::app) enum DocumentFailure {
    Policy(&'static str),
    Error(String),
}
impl From<VfsError> for DocumentFailure {
    fn from(e: VfsError) -> Self {
        Self::Error(e.to_string())
    }
}
struct Scope {
    editor: EditorWindowVersion,
    workspace: WorkspaceVersion,
}
pub(super) struct FileIntent {
    scope: Scope,
    operation: Operation,
}
enum Operation {
    Open {
        requested: PathBuf,
        line: Option<usize>,
        navigation: NavigationStamp,
        open_request: Arc<()>,
        closed_paths: Vec<PathBuf>,
    },
    Reload {
        version: Version,
        check_disk: bool,
    },
    Save {
        version: Version,
        bytes: Vec<u8>,
        line_ending: Option<LineEnding>,
        target: PathBuf,
        quit: bool,
        close: Option<crate::app::windows::EditorCloseSave>,
    },
}
pub(in crate::app) struct FileReply {
    id: RequestId,
    intent: FileIntent,
    event: VfsResult,
}
impl FileReply {
    pub(in crate::app) fn id(&self) -> RequestId {
        self.id
    }
    pub(in crate::app) fn check_save_destination<'a>(
        &self,
        paths: impl Iterator<Item = (BufferId, &'a Path)>,
    ) -> Result<(), DocumentFailure> {
        if let (
            Operation::Save { version, .. } | Operation::Reload { version, .. },
            VfsResult::Write {
                path,
                result: Ok(()),
                ..
            }
            | VfsResult::Read {
                path,
                result: Ok(_),
                ..
            },
        ) = (&self.intent.operation, &self.event)
            && paths
                .into_iter()
                .any(|(id, other)| id != version.id && other == path.as_path())
        {
            return Err(DocumentFailure::Error("document result target belongs to another open buffer; source metadata was not replaced".into()));
        }
        Ok(())
    }
    pub(in crate::app) fn close_save(&self) -> Option<&crate::app::windows::EditorCloseSave> {
        match &self.intent.operation {
            Operation::Save { close, .. } => close.as_ref(),
            Operation::Open { .. } | Operation::Reload { .. } => None,
        }
    }
    pub(in crate::app) fn target(&self) -> Option<BufferId> {
        match &self.intent.operation {
            Operation::Open { .. } => None,
            Operation::Reload { version, .. } | Operation::Save { version, .. } => Some(version.id),
        }
    }
}
pub(in crate::app) enum DocumentChange {
    Open {
        line: Option<usize>,
        navigation: NavigationStamp,
        activate: bool,
        text: TextBuffer,
        path: PathBuf,
    },
    Reload {
        buffer: BufferId,
        text: TextBuffer,
        path: PathBuf,
    },
    DiskChecked {
        buffer: BufferId,
        path: PathBuf,
        changed: bool,
    },
    Saved {
        buffer: BufferId,
        text: TextVersion,
        snapshot: Box<(Arc<str>, FileFormat)>,
        revision: BufferRevision,
        bytes: Vec<u8>,
        line_ending: Option<LineEnding>,
        target: PathBuf,
        quit: bool,
        close: Option<crate::app::windows::EditorCloseSave>,
    },
}
pub(in crate::app) struct DocumentCommit {
    source: EditorVersion,
    change: DocumentChange,
}
impl DocumentCommit {
    pub(in crate::app) fn into_change(
        self,
        current: &EditorVersion,
    ) -> Result<DocumentChange, DocumentFailure> {
        if &self.source != current {
            return Err(DocumentFailure::Error(
                "document commit source changed".into(),
            ));
        }
        Ok(self.change)
    }
}
impl Documents {
    pub(in crate::app) fn file_work_pending(&self) -> bool {
        !self.pending_files.is_empty()
    }
    pub(in crate::app) fn request_open(
        &mut self,
        path: PathBuf,
        base: &Path,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
        navigation: NavigationStamp,
    ) -> Result<(), DocumentFailure> {
        self.request_open_at_line(path, base, editor, workspace, navigation, None)
    }
    pub(in crate::app) fn request_open_at_line(
        &mut self,
        path: PathBuf,
        base: &Path,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
        navigation: NavigationStamp,
        line: Option<usize>,
    ) -> Result<(), DocumentFailure> {
        self.check_file_capacity(editor)?;
        let path = if path.is_absolute() {
            path
        } else {
            base.join(path)
        };
        let id = self.vfs.read(path.clone())?;
        self.pending_files.retain(|old, intent| {
            let keep =
                !matches!(&intent.operation,Operation::Open {requested,..} if requested==&path);
            if !keep {
                self.vfs.cancel(*old);
            }
            keep
        });
        self.latest_open = Arc::new(());
        self.pending_files.insert(
            id,
            FileIntent {
                scope: Scope {
                    editor: editor.clone(),
                    workspace,
                },
                operation: Operation::Open {
                    requested: path,
                    line,
                    navigation,
                    open_request: self.latest_open.clone(),
                    closed_paths: Vec::new(),
                },
            },
        );
        Ok(())
    }
    pub(in crate::app) fn request_reload(
        &mut self,
        view: Option<DocumentView<'_>>,
        force: bool,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
    ) -> Result<(), DocumentFailure> {
        self.request_reload_inner(view, force, false, editor, workspace)
    }
    pub(in crate::app) fn request_disk_check(
        &mut self,
        view: DocumentView<'_>,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
    ) -> Result<(), DocumentFailure> {
        self.request_reload_inner(Some(view), true, true, editor, workspace)
    }
    fn request_reload_inner(
        &mut self,
        view: Option<DocumentView<'_>>,
        force: bool,
        check_disk: bool,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
    ) -> Result<(), DocumentFailure> {
        self.check_file_capacity(editor)?;
        let view = view.ok_or(DocumentFailure::Policy("no_active_reload"))?;
        if view.virtual_read_only {
            return Err(DocumentFailure::Error(
                "virtual document is read-only".into(),
            ));
        }
        self.check_buffer_mutation(view.id)?;
        if view.dirty && !force {
            return Err(DocumentFailure::Policy("dirty_reload"));
        }
        let path = view.path.ok_or(DocumentFailure::Policy("scratch_reload"))?;
        let id = self.vfs.read(path.to_path_buf())?;
        self.pending_files.insert(
            id,
            FileIntent {
                scope: Scope {
                    editor: editor.clone(),
                    workspace,
                },
                operation: Operation::Reload {
                    version: view.version(),
                    check_disk,
                },
            },
        );
        Ok(())
    }
    pub(in crate::app) fn request_save(
        &mut self,
        view: Option<DocumentView<'_>>,
        mut request: SaveRequest<'_>,
    ) -> Result<RequestId, DocumentFailure> {
        self.check_file_capacity(request.editor)?;
        let view = view.ok_or(DocumentFailure::Policy("no_active_save"))?;
        if view.virtual_read_only || view.read_only {
            return Err(DocumentFailure::Error(
                "virtual document is read-only".into(),
            ));
        }
        self.check_buffer_mutation(view.id)?;
        let path = request
            .path
            .or_else(|| view.path.map(Path::to_path_buf))
            .ok_or(DocumentFailure::Policy("scratch_no_name"))?;
        let target = if path.is_absolute() {
            path
        } else {
            request.base.join(path)
        };
        request
            .other_open_paths
            .extend(
                self.pending_files
                    .values()
                    .filter_map(|intent| match &intent.operation {
                        Operation::Open { requested, .. } => Some(requested.clone()),
                        Operation::Save { target, .. } => Some(target.clone()),
                        Operation::Reload { .. } => None,
                    }),
            );
        if request.other_open_paths.iter().any(|path| path == &target) {
            return Err(DocumentFailure::Error(
                "save-as target belongs to another open buffer".into(),
            ));
        }
        let bytes = configured_file_bytes(view.text, request.policy);
        let expectation = if request.force {
            WriteExpectation::Any
        } else if view.new_file {
            WriteExpectation::Missing
        } else {
            // The worker resolves aliases before deciding whether this is a save-as.
            WriteExpectation::Exact(view.persisted.to_vec())
        };
        request
            .authority
            .require(crate::editor::CommandAccess::Files)
            .map_err(|error| DocumentFailure::Error(error.to_string()))?;
        let id = self.vfs.write_document(
            target.clone(),
            bytes.clone(),
            expectation,
            view.path.map(Path::to_path_buf),
            request.other_open_paths,
        )?;
        self.pending_files.insert(
            id,
            FileIntent {
                scope: Scope {
                    editor: request.editor.clone(),
                    workspace: request.workspace,
                },
                operation: Operation::Save {
                    version: view.save_version(),
                    bytes,
                    line_ending: configured_line_ending(request.policy),
                    target,
                    quit: request.quit,
                    close: request.close,
                },
            },
        );
        Ok(id)
    }
    fn check_buffer_mutation(&self, buffer: BufferId) -> Result<(), DocumentFailure> {
        if self
            .pending_files
            .values()
            .any(|intent| match &intent.operation {
                Operation::Reload { version, .. } | Operation::Save { version, .. } => {
                    version.id == buffer
                }
                Operation::Open { .. } => false,
            })
        {
            return Err(DocumentFailure::Error(
                "a save or reload for this buffer is already pending".into(),
            ));
        }
        Ok(())
    }
    fn check_file_capacity(&self, editor: &EditorWindowVersion) -> Result<(), DocumentFailure> {
        if editor.revision().is_none() {
            return Err(DocumentFailure::Error("editor source is exhausted".into()));
        }
        self.check_capacity()?;
        Ok(())
    }
    pub(in crate::app) fn file_reply(&mut self, id: RequestId, event: VfsResult) -> DocumentEvent {
        match self.pending_files.remove(&id) {
            Some(intent) => DocumentEvent::File(FileReply { id, intent, event }),
            None => DocumentEvent::Discarded,
        }
    }
    pub(in crate::app) fn prepare_file(
        &self,
        reply: FileReply,
        editor: &EditorVersion,
        workspace: &WorkspaceVersion,
        view: Option<DocumentView<'_>>,
    ) -> Result<DocumentCommit, DocumentFailure> {
        let scope = reply.intent.scope;
        let stale = || {
            DocumentFailure::Error(
                "file result is stale or its document was closed/replaced".into(),
            )
        };
        if !scope.editor.same_editor(editor.window())
            || editor.window().revision().is_none()
            || (&scope.workspace != workspace
                && !matches!(reply.intent.operation, Operation::Save { .. }))
        {
            return Err(stale());
        }
        let same_workspace = &scope.workspace == workspace;
        let change = match (reply.intent.operation, reply.event) {
            (
                Operation::Open {
                    navigation,
                    line,
                    open_request,
                    closed_paths,
                    ..
                },
                VfsResult::Read { path, result, .. },
            ) => {
                if closed_paths.contains(&path) {
                    return Err(stale());
                }
                DocumentChange::Open {
                    path,
                    line,
                    navigation,
                    activate: Arc::ptr_eq(&open_request, &self.latest_open),
                    text: TextBuffer::from_bytes(&result?)
                        .map_err(|e| DocumentFailure::Error(e.to_string()))?,
                }
            }
            (
                Operation::Reload {
                    version,
                    check_disk,
                },
                VfsResult::Read { path, result, .. },
            ) => {
                let current = view.ok_or_else(stale)?;
                if current.id != version.id
                    || current.path != version.path.as_deref()
                    || current.text.source_version() != version.text
                    || current.virtual_read_only
                {
                    return Err(stale());
                }
                let bytes = result?;
                let changed = check_disk && bytes != current.persisted;
                if check_disk && (!changed || current.dirty) {
                    DocumentChange::DiskChecked {
                        buffer: version.id,
                        path,
                        changed,
                    }
                } else {
                    DocumentChange::Reload {
                        buffer: version.id,
                        path,
                        text: TextBuffer::from_bytes(&bytes)
                            .map_err(|e| DocumentFailure::Error(e.to_string()))?,
                    }
                }
            }
            (
                Operation::Save {
                    version,
                    bytes,
                    line_ending,
                    target: _,
                    quit,
                    close,
                },
                VfsResult::Write {
                    result,
                    path: target,
                    ..
                },
            ) => {
                result?;
                let current = view.ok_or_else(stale)?;
                if current.id != version.id
                    || current.path != version.path.as_deref()
                    || !current.text.source_version().same_instance(&version.text)
                    || current.virtual_read_only
                {
                    return Err(stale());
                }
                DocumentChange::Saved {
                    buffer: version.id,
                    text: version.text,
                    snapshot: version.snapshot.expect("save snapshot"),
                    revision: version.revision,
                    bytes,
                    line_ending,
                    target,
                    quit: quit && same_workspace,
                    close: if same_workspace { close } else { None },
                }
            }
            _ => {
                return Err(DocumentFailure::Error(
                    "unexpected document worker result".into(),
                ));
            }
        };
        Ok(DocumentCommit {
            source: editor.clone(),
            change,
        })
    }
    pub(in crate::app) fn cancel_buffer_requests(&mut self, buffer: BufferId, path: Option<&Path>) {
        self.pending_files.retain(|id, intent| {
            let keep = match &mut intent.operation {
                Operation::Reload { version, .. } | Operation::Save { version, .. } => {
                    version.id != buffer
                }
                Operation::Open {
                    requested,
                    closed_paths,
                    ..
                } => match path {
                    None => true,
                    Some(path) if path == requested => false,
                    Some(path) => {
                        // Bound closed-path history to this outstanding read only.
                        if !closed_paths.iter().any(|p| p == path) {
                            closed_paths.push(path.to_path_buf());
                        }
                        closed_paths.len() <= 64
                    }
                },
            };
            if !keep {
                self.vfs.cancel(*id);
            }
            keep
        });
        self.overrides.remove(&buffer);
    }
    pub(in crate::app) fn cancel_old_workspace_requests(&mut self, current: &WorkspaceVersion) {
        self.pending_files.retain(|id, intent| {
            let keep = &intent.scope.workspace == current;
            if !keep && matches!(intent.operation, Operation::Save { .. }) && !self.vfs.cancel(*id)
            {
                return true;
            }
            if !keep {
                self.vfs.cancel(*id);
            }
            keep
        });
    }
}
pub(in crate::app) struct SaveRequest<'a> {
    pub authority: crate::editor::CommandAuthority,
    pub editor: &'a EditorWindowVersion,
    pub workspace: WorkspaceVersion,
    pub base: &'a Path,
    pub path: Option<PathBuf>,
    pub policy: &'a crate::config::EditorConfig,
    pub force: bool,
    pub quit: bool,
    pub close: Option<crate::app::windows::EditorCloseSave>,
    pub other_open_paths: Vec<PathBuf>,
}

pub(in crate::app) fn configured_file_bytes(
    text: &TextBuffer,
    configured: &crate::config::EditorConfig,
) -> Vec<u8> {
    use crate::config::FinalNewlinePolicy;
    let mut source = text.to_string();
    match configured.final_newline {
        FinalNewlinePolicy::Preserve => {}
        FinalNewlinePolicy::Insert if !source.ends_with('\n') => source.push('\n'),
        FinalNewlinePolicy::Remove => {
            while source.ends_with('\n') {
                source.pop();
            }
        }
        FinalNewlinePolicy::Insert => {}
    }
    let mut format = text.format().clone();
    if let Some(ending) = configured_line_ending(configured) {
        format.preferred_line_ending = ending;
        format.line_endings = vec![ending; source.bytes().filter(|byte| *byte == b'\n').count()];
    }
    crate::editor::text::encode_utf8(&source, &format)
}

fn configured_line_ending(configured: &crate::config::EditorConfig) -> Option<LineEnding> {
    use crate::config::LineEndingPolicy;
    match configured.line_endings {
        LineEndingPolicy::Preserve => None,
        LineEndingPolicy::Lf => Some(LineEnding::Lf),
        LineEndingPolicy::CrLf => Some(LineEnding::CrLf),
        LineEndingPolicy::Cr => Some(LineEnding::Cr),
    }
}

#[cfg(test)]
mod tests {
    use crate::app::documents::lifecycle::*;
    use crate::app::startup::StartupTarget;
    use crate::app::workspace::Workspace;
    use crate::editor::EditorState;
    use crate::files::RootIdentity;
    use crate::workspace::WorkspaceId;
    use crate::workspace::WorkspaceState;
    fn workspace() -> Workspace {
        Workspace::new(
            WorkspaceState::new(
                WorkspaceId(1),
                [(
                    "root".into(),
                    RootIdentity {
                        canonical_path: "/workspace".into(),
                        filesystem_identity: Some((1, 2)),
                    },
                )],
            )
            .unwrap(),
            "/unused".into(),
            2,
            false,
            false,
            4,
        )
    }
    pub(in crate::app) fn view(state: &EditorState, id: BufferId) -> DocumentView<'_> {
        let b = &state.buffers[id];
        DocumentView {
            id,
            path: b.path.as_deref(),
            text: &b.text,
            persisted: b.persisted_bytes(),
            new_file: b.new_file,
            dirty: b.dirty,
            virtual_read_only: b.is_virtual_read_only(),
            read_only: b.read_only,
        }
    }
    pub(in crate::app) fn owner(state: &EditorState) -> Documents {
        Documents::new(
            crate::files::VfsWorker::start(2),
            &StartupTarget::Dashboard {
                root: "/workspace".into(),
            },
            state.window_version(),
            0,
            2,
        )
    }
    fn reload(state: &EditorState, workspace: &Workspace, id: BufferId) -> FileReply {
        FileReply {
            id: RequestId(1),
            intent: FileIntent {
                scope: Scope {
                    editor: state.window_version(),
                    workspace: workspace.version(),
                },
                operation: Operation::Reload {
                    version: view(state, id).version(),
                    check_disk: false,
                },
            },
            event: VfsResult::Read {
                id: RequestId(1),
                path: "/workspace/file".into(),
                result: Ok(vec![255]),
            },
        }
    }
    #[test]
    fn save_normalization_keeps_the_requested_snapshot_clean() {
        for (source, policy, disk) in [
            (
                "abc",
                crate::config::FinalNewlinePolicy::Insert,
                b"abc\n".as_slice(),
            ),
            (
                "abc\n\n",
                crate::config::FinalNewlinePolicy::Remove,
                b"abc".as_slice(),
            ),
        ] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state
                .open_text(
                    TextBuffer::from_text(source),
                    Some("/workspace/file".into()),
                )
                .unwrap();
            let mut config = crate::config::load_bundled_config().config;
            config.editor.final_newline = policy;
            let bytes = configured_file_bytes(&state.buffers[buffer].text, &config.editor);
            assert_eq!(bytes, disk);
            let version = view(&state, buffer).save_version();
            let mut draft = state.draft();
            assert!(draft.save_document(
                buffer,
                &version.text,
                version.revision,
                bytes,
                "/workspace/file".into(),
                *version.snapshot.unwrap()
            ));
            state = draft.accept(&state.version()).unwrap();
            assert!(!state.buffers[buffer].dirty);
            assert_eq!(state.buffers[buffer].persisted_bytes(), disk);
            assert_eq!(state.buffers[buffer].text.to_bytes(), disk);
            state
                .apply_transaction(
                    &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                        .with_operation(crate::editor::EditorOperation::Undo { buffer }),
                )
                .unwrap();
            assert_eq!(state.buffers[buffer].text.to_string(), source);
        }
    }

    #[test]
    fn line_ending_save_policy_keeps_the_requested_format_snapshot_clean() {
        for (ending, disk) in [
            (crate::config::LineEndingPolicy::Lf, b"a\nb\n".as_slice()),
            (
                crate::config::LineEndingPolicy::CrLf,
                b"a\r\nb\r\n".as_slice(),
            ),
            (crate::config::LineEndingPolicy::Cr, b"a\rb\r".as_slice()),
        ] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state
                .open_text(
                    TextBuffer::from_bytes(b"a\r\nb\n").unwrap(),
                    Some("/workspace/file".into()),
                )
                .unwrap();
            let mut config = crate::config::load_bundled_config().config;
            config.editor.line_endings = ending;
            let bytes = configured_file_bytes(&state.buffers[buffer].text, &config.editor);
            assert_eq!(bytes, disk);
            let version = view(&state, buffer).save_version();
            let mut draft = state.draft();
            assert!(draft.save_document(
                buffer,
                &version.text,
                version.revision,
                bytes,
                "/workspace/file".into(),
                *version.snapshot.unwrap(),
            ));
            state = draft.accept(&state.version()).unwrap();
            assert!(!state.buffers[buffer].dirty);
            state.buffers[buffer].refresh_dirty();
            assert!(!state.buffers[buffer].dirty);
            assert_eq!(state.buffers[buffer].persisted_bytes(), disk);
        }
    }

    #[test]
    fn read_only_files_reject_save_and_reload_collisions_are_checked() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("a"), Some("/workspace/a".into()))
            .unwrap();
        state.buffers[buffer].read_only = true;
        let workspace = workspace();
        let mut documents = owner(&state);
        let config = crate::config::load_bundled_config().config;
        assert!(
            documents
                .request_save(
                    Some(view(&state, buffer)),
                    SaveRequest {
                        authority: crate::editor::CommandAuthority::Host,
                        editor: &state.window_version(),
                        workspace: workspace.version(),
                        base: std::path::Path::new("/workspace"),
                        path: None,
                        policy: &config.editor,
                        force: true,
                        quit: false,
                        close: None,
                        other_open_paths: Vec::new(),
                    }
                )
                .is_err()
        );
        let mut reply = reload(&state, &workspace, buffer);
        reply.event = VfsResult::Read {
            id: RequestId(1),
            path: "/workspace/b".into(),
            result: Ok(b"b".to_vec()),
        };
        let (other, _) = state
            .open_text(TextBuffer::from_text("b"), Some("/workspace/b".into()))
            .unwrap();
        assert!(
            reply
                .check_save_destination(std::iter::once((
                    other,
                    std::path::Path::new("/workspace/b")
                )))
                .is_err()
        );
    }

    #[test]
    fn completed_saves_are_reconciled_after_a_workspace_switch() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(
                TextBuffer::from_text("saved"),
                Some("/workspace/file".into()),
            )
            .unwrap();
        let old = workspace();
        let current = workspace();
        let version = view(&state, buffer).save_version();
        let reply = FileReply {
            id: RequestId(1),
            intent: FileIntent {
                scope: Scope {
                    editor: state.window_version(),
                    workspace: old.version(),
                },
                operation: Operation::Save {
                    version,
                    bytes: b"saved".to_vec(),
                    line_ending: None,
                    target: "/workspace/file".into(),
                    quit: false,
                    close: None,
                },
            },
            event: VfsResult::Write {
                id: RequestId(1),
                path: "/workspace/file".into(),
                result: Ok(()),
            },
        };
        assert!(
            owner(&state)
                .prepare_file(
                    reply,
                    &state.version(),
                    &current.version(),
                    Some(view(&state, buffer))
                )
                .is_ok()
        );
    }

    #[test]
    fn reload_rejects_edits_reloaded_instances_closed_targets_and_foreign_roots_before_decode() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state
            .open_text(TextBuffer::from_text("old"), Some("/workspace/file".into()))
            .unwrap();
        let owner = owner(&state);
        let workspace = workspace();
        let reply = reload(&state, &workspace, id);
        let revision = state.buffers[id].text.revision();
        state.replace_buffer_from_disk(id, TextBuffer::from_text("new"), "/workspace/file".into());
        assert_eq!(state.buffers[id].text.revision(), revision);
        let count = TextBuffer::test_materializations();
        assert!(
            owner
                .prepare_file(
                    reply,
                    &state.version(),
                    &workspace.version(),
                    Some(view(&state, id))
                )
                .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), count);
        let reply = reload(&state, &workspace, id);
        state.buffers[id].text.restore_text("unsaved").unwrap();
        state.set_buffer_dirty(id, true);
        assert!(
            owner
                .prepare_file(
                    reply,
                    &state.version(),
                    &workspace.version(),
                    Some(view(&state, id))
                )
                .is_err()
        );
        let reply = reload(&state, &workspace, id);
        assert!(
            owner
                .prepare_file(reply, &state.version(), &workspace.version(), None)
                .is_err()
        );
        let reply = reload(&state, &workspace, id);
        let other = crate::app::documents::lifecycle::tests::workspace();
        assert!(
            owner
                .prepare_file(
                    reply,
                    &state.version(),
                    &other.version(),
                    Some(view(&state, id))
                )
                .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), count);
    }
    #[test]
    fn stale_save_cannot_clean_new_edits_or_replace_reload_metadata() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state
            .open_text(TextBuffer::from_text("old"), Some("/workspace/file".into()))
            .unwrap();
        let owner = owner(&state);
        let workspace = workspace();
        let reply = |state: &EditorState| FileReply {
            id: RequestId(1),
            intent: FileIntent {
                scope: Scope {
                    editor: state.window_version(),
                    workspace: workspace.version(),
                },
                operation: Operation::Save {
                    version: view(state, id).save_version(),
                    bytes: b"old".to_vec(),
                    line_ending: None,
                    target: "/workspace/file".into(),
                    quit: true,
                    close: None,
                },
            },
            event: VfsResult::Write {
                id: RequestId(1),
                path: "/workspace/file".into(),
                result: Ok(()),
            },
        };
        let pending = reply(&state);
        state.buffers[id].text.restore_text("new unsaved").unwrap();
        state.set_buffer_dirty(id, true);
        let plan = owner
            .prepare_file(
                pending,
                &state.version(),
                &workspace.version(),
                Some(view(&state, id)),
            )
            .unwrap_or_else(|_| panic!("save ack"));
        let DocumentChange::Saved {
            buffer,
            text,
            revision,
            bytes,
            snapshot,
            target,
            ..
        } = plan
            .into_change(&state.version())
            .unwrap_or_else(|_| panic!("commit"))
        else {
            panic!("save")
        };
        let mut candidate = state.draft();
        assert!(candidate.save_document(buffer, &text, revision, bytes, target, *snapshot));
        state = candidate.accept(&state.version()).unwrap();
        assert!(state.buffers[id].dirty);
        assert_eq!(state.buffers[id].persisted_bytes(), b"old");
        let pending = reply(&state);
        state.replace_buffer_from_disk(
            id,
            TextBuffer::from_text("new disk"),
            "/workspace/file".into(),
        );
        assert!(
            owner
                .prepare_file(
                    pending,
                    &state.version(),
                    &workspace.version(),
                    Some(view(&state, id))
                )
                .is_err()
        );
        assert_eq!(state.buffers[id].persisted_bytes(), b"new disk");
    }
    #[test]
    fn worker_stop_clears_document_and_legacy_bookkeeping_once() {
        let state = EditorState::new("/workspace".into());
        let workspace = workspace();
        let (vfs, gate) = crate::files::VfsWorker::start_gated_for_test(2);
        let mut owner = Documents::new(
            vfs,
            &StartupTarget::Dashboard {
                root: "/workspace".into(),
            },
            state.window_version(),
            0,
            2,
        );
        owner
            .request_open(
                "file".into(),
                std::path::Path::new("/workspace"),
                &state.window_version(),
                workspace.version(),
                crate::app::windows::Windows::default()
                    .navigation_stamp(state.window_version(), state.active_view()),
            )
            .unwrap_or_else(|_| panic!("accept"));
        owner.stop_worker();
        gate.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !owner.worker_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(matches!(
            owner.poll(&state.window_version(), 0).unwrap(),
            Some(DocumentEvent::Stopped { .. })
        ));
        assert!(!owner.file_work_pending());
        assert!(!owner.has_startup_work());
        assert!(owner.poll(&state.window_version(), 0).unwrap().is_none());
        assert!(
            owner
                .request_open(
                    "file".into(),
                    std::path::Path::new("/workspace"),
                    &state.window_version(),
                    workspace.version(),
                    crate::app::windows::Windows::default()
                        .navigation_stamp(state.window_version(), state.active_view())
                )
                .is_err()
        );
    }
}
