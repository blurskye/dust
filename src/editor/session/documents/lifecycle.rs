use super::{DocumentEvent, DocumentSession};
use crate::editor::session::windows::NavigationStamp;
use crate::editor::session::workspace::WorkspaceVersion;
use crate::editor::text::{BufferRevision, TextBuffer, TextVersion};
use crate::editor::{BufferId, EditorVersion, EditorWindowVersion};
use crate::files::{RequestId, VfsError, VfsResult, WriteExpectation};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(in crate::editor::session) struct DocumentView<'a> {
    pub id: BufferId,
    pub path: Option<&'a Path>,
    pub text: &'a TextBuffer,
    pub persisted: &'a [u8],
    pub dirty: bool,
    pub virtual_read_only: bool,
}
#[derive(Clone)]
struct Version {
    id: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
    revision: BufferRevision,
}
impl DocumentView<'_> {
    fn version(&self) -> Version {
        Version {
            id: self.id,
            path: self.path.map(Path::to_path_buf),
            text: self.text.source_version(),
            revision: self.text.revision(),
        }
    }
}
pub(in crate::editor::session) enum DocumentFailure {
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
        navigation: NavigationStamp,
        generation: Arc<()>,
        retired: Vec<PathBuf>,
    },
    Reload(Version),
    Save {
        version: Version,
        bytes: Vec<u8>,
        target: PathBuf,
        quit: bool,
        close: Option<super::super::windows::EditorCloseSave>,
    },
}
pub(in crate::editor::session) struct FileReply {
    intent: FileIntent,
    event: VfsResult,
}
impl FileReply {
    pub(in crate::editor::session) fn check_save_destination<'a>(
        &self,
        paths: impl Iterator<Item = (BufferId, &'a Path)>,
    ) -> Result<(), DocumentFailure> {
        if let (
            Operation::Save { version, .. },
            VfsResult::Write {
                path,
                result: Ok(()),
                ..
            },
        ) = (&self.intent.operation, &self.event)
        {
            if paths
                .into_iter()
                .any(|(id, other)| id != version.id && other == path.as_path())
            {
                return Err(DocumentFailure::Error("save completed on disk but target now belongs to another open buffer; source metadata was not replaced".into()));
            }
        }
        Ok(())
    }
    pub(in crate::editor::session) fn close_save(
        &self,
    ) -> Option<&super::super::windows::EditorCloseSave> {
        match &self.intent.operation {
            Operation::Save { close, .. } => close.as_ref(),
            Operation::Open { .. } | Operation::Reload(_) => None,
        }
    }
    pub(in crate::editor::session) fn target(&self) -> Option<BufferId> {
        match &self.intent.operation {
            Operation::Open { .. } => None,
            Operation::Reload(v) | Operation::Save { version: v, .. } => Some(v.id),
        }
    }
}
pub(in crate::editor::session) enum DocumentChange {
    Open {
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
    Saved {
        buffer: BufferId,
        text: TextVersion,
        revision: BufferRevision,
        bytes: Vec<u8>,
        target: PathBuf,
        quit: bool,
        close: Option<super::super::windows::EditorCloseSave>,
    },
}
pub(in crate::editor::session) struct DocumentCommit {
    source: EditorVersion,
    change: DocumentChange,
}
impl DocumentCommit {
    pub(in crate::editor::session) fn into_change(
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
impl DocumentSession {
    pub(in crate::editor::session) fn file_work_pending(&self) -> bool {
        !self.pending_files.is_empty()
    }
    pub(in crate::editor::session) fn request_open(
        &mut self,
        path: PathBuf,
        base: &Path,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
        navigation: NavigationStamp,
    ) -> Result<(), DocumentFailure> {
        self.check_file_admission(editor)?;
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
        self.open_generation = Arc::new(());
        self.pending_files.insert(
            id,
            FileIntent {
                scope: Scope {
                    editor: editor.clone(),
                    workspace,
                },
                operation: Operation::Open {
                    requested: path,
                    navigation,
                    generation: self.open_generation.clone(),
                    retired: Vec::new(),
                },
            },
        );
        Ok(())
    }
    pub(in crate::editor::session) fn request_reload(
        &mut self,
        view: Option<DocumentView<'_>>,
        force: bool,
        editor: &EditorWindowVersion,
        workspace: WorkspaceVersion,
    ) -> Result<(), DocumentFailure> {
        self.check_file_admission(editor)?;
        let view = view.ok_or(DocumentFailure::Policy("no_active_reload"))?;
        if view.virtual_read_only {
            return Err(DocumentFailure::Error(
                "virtual document is read-only".into(),
            ));
        }
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
                operation: Operation::Reload(view.version()),
            },
        );
        Ok(())
    }
    pub(in crate::editor::session) fn request_save(
        &mut self,
        view: Option<DocumentView<'_>>,
        request: SaveRequest<'_>,
    ) -> Result<(), DocumentFailure> {
        self.check_file_admission(request.editor)?;
        let view = view.ok_or(DocumentFailure::Policy("no_active_save"))?;
        if view.virtual_read_only {
            return Err(DocumentFailure::Error(
                "virtual document is read-only".into(),
            ));
        }
        let path = request
            .path
            .or_else(|| view.path.map(Path::to_path_buf))
            .ok_or(DocumentFailure::Policy("scratch_no_name"))?;
        let target = if path.is_absolute() {
            path
        } else {
            request.base.join(path)
        };
        if request.other_open_paths.iter().any(|path| path == &target) {
            return Err(DocumentFailure::Error(
                "save-as target belongs to another open buffer".into(),
            ));
        }
        if self.pending_files.values().any(|intent|matches!(&intent.operation,Operation::Save {target:pending,..} if pending==&target)) {return Err(DocumentFailure::Error("a save for this target is already pending".into()));}
        let bytes = configured_file_bytes(view.text, request.policy);
        let expectation = if request.force {
            WriteExpectation::Any
        } else if view.path == Some(target.as_path()) {
            WriteExpectation::Exact(view.persisted.to_vec())
        } else {
            WriteExpectation::Missing
        };
        let id = self.vfs.write_document(
            target.clone(),
            bytes.clone(),
            expectation,
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
                    version: view.version(),
                    bytes,
                    target,
                    quit: request.quit,
                    close: request.close,
                },
            },
        );
        Ok(())
    }
    fn check_file_admission(&self, editor: &EditorWindowVersion) -> Result<(), DocumentFailure> {
        if editor.revision().is_none() {
            return Err(DocumentFailure::Error("editor source is exhausted".into()));
        }
        self.check_admission()?;
        Ok(())
    }
    pub(super) fn file_reply(&mut self, id: RequestId, event: VfsResult) -> DocumentEvent {
        match self.pending_files.remove(&id) {
            Some(intent) => DocumentEvent::File(FileReply { intent, event }),
            None => DocumentEvent::Discarded,
        }
    }
    pub(in crate::editor::session) fn prepare_file(
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
            || &scope.workspace != workspace
        {
            return Err(stale());
        }
        let change = match (reply.intent.operation, reply.event) {
            (
                Operation::Open {
                    navigation,
                    generation,
                    retired,
                    ..
                },
                VfsResult::Read { path, result, .. },
            ) => {
                if retired.contains(&path) {
                    return Err(stale());
                }
                DocumentChange::Open {
                    path,
                    navigation,
                    activate: Arc::ptr_eq(&generation, &self.open_generation),
                    text: TextBuffer::from_bytes(&result?)
                        .map_err(|e| DocumentFailure::Error(e.to_string()))?,
                }
            }
            (Operation::Reload(version), VfsResult::Read { path, result, .. }) => {
                let current = view.ok_or_else(stale)?;
                if current.id != version.id
                    || current.path != version.path.as_deref()
                    || current.text.source_version() != version.text
                    || current.virtual_read_only
                {
                    return Err(stale());
                }
                DocumentChange::Reload {
                    buffer: version.id,
                    path,
                    text: TextBuffer::from_bytes(&result?)
                        .map_err(|e| DocumentFailure::Error(e.to_string()))?,
                }
            }
            (
                Operation::Save {
                    version,
                    bytes,
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
                    || !current
                        .text
                        .source_version()
                        .same_incarnation(&version.text)
                    || current.virtual_read_only
                {
                    return Err(stale());
                }
                DocumentChange::Saved {
                    buffer: version.id,
                    text: version.text,
                    revision: version.revision,
                    bytes,
                    target,
                    quit,
                    close,
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
    pub(in crate::editor::session) fn retire_buffer(
        &mut self,
        buffer: BufferId,
        path: Option<&Path>,
    ) {
        self.pending_files.retain(|id, intent| {
            let keep = match &mut intent.operation {
                Operation::Reload(v) | Operation::Save { version: v, .. } => v.id != buffer,
                Operation::Open {
                    requested, retired, ..
                } => match path {
                    None => true,
                    Some(path) if path == requested => false,
                    Some(path) => {
                        // Bound alias-retirement history to this outstanding read only.
                        if !retired.iter().any(|p| p == path) {
                            retired.push(path.to_path_buf());
                        }
                        retired.len() <= 64
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
    pub(in crate::editor::session) fn retire_workspace(&mut self, current: &WorkspaceVersion) {
        self.pending_files.retain(|id, intent| {
            let keep = &intent.scope.workspace == current;
            if !keep {
                self.vfs.cancel(*id);
            }
            keep
        });
    }
}
pub(in crate::editor::session) struct SaveRequest<'a> {
    pub editor: &'a EditorWindowVersion,
    pub workspace: WorkspaceVersion,
    pub base: &'a Path,
    pub path: Option<PathBuf>,
    pub policy: &'a crate::config::EditorConfig,
    pub force: bool,
    pub quit: bool,
    pub close: Option<super::super::windows::EditorCloseSave>,
    pub other_open_paths: Vec<PathBuf>,
}

pub(in crate::editor::session) fn configured_file_bytes(
    text: &TextBuffer,
    configured: &crate::config::EditorConfig,
) -> Vec<u8> {
    use crate::config::{BomPolicy, FinalNewlinePolicy, LineEndingPolicy};
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
    format.utf8_bom = match configured.utf8_bom {
        BomPolicy::Preserve => format.utf8_bom,
        BomPolicy::Always => true,
        BomPolicy::Never => false,
    };
    let configured_ending = match configured.line_endings {
        LineEndingPolicy::Preserve => None,
        LineEndingPolicy::Lf => Some(crate::editor::text::LineEnding::Lf),
        LineEndingPolicy::CrLf => Some(crate::editor::text::LineEnding::CrLf),
        LineEndingPolicy::Cr => Some(crate::editor::text::LineEnding::Cr),
    };
    if let Some(ending) = configured_ending {
        format.preferred_line_ending = ending;
        format.line_endings = vec![ending; source.bytes().filter(|byte| *byte == b'\n').count()];
    }
    crate::editor::text::encode_utf8(&source, &format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::session::workspace::WorkspaceSession;
    use crate::editor::{EditorState, StartupTarget};
    use crate::workspace::{RootIdentity, WorkspaceId, WorkspaceState};
    fn workspace() -> WorkspaceSession {
        WorkspaceSession::new(
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
    fn view(state: &EditorState, id: BufferId) -> DocumentView<'_> {
        let b = &state.buffers[id];
        DocumentView {
            id,
            path: b.path.as_deref(),
            text: &b.text,
            persisted: b.persisted_bytes(),
            dirty: b.dirty,
            virtual_read_only: b.is_virtual_read_only(),
        }
    }
    fn owner(state: &EditorState) -> DocumentSession {
        DocumentSession::new(
            crate::files::VfsWorker::start(2),
            &StartupTarget::Dashboard {
                root: "/workspace".into(),
            },
            state.window_version(),
            0,
            2,
        )
    }
    fn reload(state: &EditorState, workspace: &WorkspaceSession, id: BufferId) -> FileReply {
        FileReply {
            intent: FileIntent {
                scope: Scope {
                    editor: state.window_version(),
                    workspace: workspace.version(),
                },
                operation: Operation::Reload(view(state, id).version()),
            },
            event: VfsResult::Read {
                id: RequestId(1),
                path: "/workspace/file".into(),
                result: Ok(vec![255]),
            },
        }
    }
    #[test]
    fn reload_rejects_edits_reloaded_incarnations_closed_targets_and_foreign_roots_before_decode() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state.open_text(TextBuffer::from_text("old"), Some("/workspace/file".into()));
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
        let other = super::tests::workspace();
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
        let (id, _) = state.open_text(TextBuffer::from_text("old"), Some("/workspace/file".into()));
        let owner = owner(&state);
        let workspace = workspace();
        let reply = |state: &EditorState| FileReply {
            intent: FileIntent {
                scope: Scope {
                    editor: state.window_version(),
                    workspace: workspace.version(),
                },
                operation: Operation::Save {
                    version: view(state, id).version(),
                    bytes: b"old".to_vec(),
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
            target,
            ..
        } = plan
            .into_change(&state.version())
            .unwrap_or_else(|_| panic!("commit"))
        else {
            panic!("save")
        };
        let mut candidate = state.draft();
        assert!(candidate.save_document(buffer, &text, revision, bytes, target));
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
    fn worker_stop_retires_document_and_legacy_bookkeeping_once() {
        let state = EditorState::new("/workspace".into());
        let workspace = workspace();
        let (vfs, gate) = crate::files::VfsWorker::start_gated_for_test(2);
        let mut owner = DocumentSession::new(
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
                crate::editor::session::windows::WindowSession::default()
                    .navigation_stamp(state.window_version(), state.active_view),
            )
            .unwrap_or_else(|_| panic!("admit"));
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
                    crate::editor::session::windows::WindowSession::default()
                        .navigation_stamp(state.window_version(), state.active_view)
                )
                .is_err()
        );
    }
}
