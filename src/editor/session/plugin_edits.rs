//! Extension-source facet: invocation snapshots and one-way edit proposal lifetimes.
//! Other extension runtime/provider ownership has not yet migrated here.
use super::{
    workspace::{WorkspaceSession, WorkspaceVersion},
    workspace_edits::OriginKey,
};
use crate::{
    editor::{
        BufferId, EditorState, EditorWindowVersion,
        text::{BufferRevision, ByteOffset, ByteRange, TextEdit, TextVersion},
        transaction::WorkspaceEditSources,
    },
    extensions::{
        PluginEditProposal, PluginExecution, PluginExecutionReply, PluginExecutionRequest,
        PluginInvocation, PluginInvocationOrigin, PluginLoadId, PluginManager, PluginSnapshot,
    },
    workspace::WorkspaceEditDraft,
};
use slotmap::Key;
use std::path::PathBuf;

struct Document {
    id: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
    revision: BufferRevision,
}
struct Invocation {
    origin: PluginInvocationOrigin,
    editor: EditorWindowVersion,
    workspace: WorkspaceVersion,
    configuration: u64,
    document: Option<Document>,
    sources: Option<WorkspaceEditSources>,
    maximum: usize,
}
impl Invocation {
    fn current(
        &self,
        manager: &PluginManager,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        self.origin.current(manager)
            && &self.workspace == workspace
            && self.configuration == configuration
    }
    fn document<'a>(
        &self,
        editor: &'a EditorState,
    ) -> Result<&'a crate::editor::text::TextBuffer, String> {
        if !self.editor.same_editor(&editor.window_version())
            || editor.window_version().revision().is_none()
        {
            return Err("plugin edit belongs to a foreign or exhausted editor".into());
        }
        let document = self
            .document
            .as_ref()
            .ok_or("plugin invocation had no document snapshot")?;
        let buffer = editor
            .buffers
            .get(document.id)
            .ok_or("plugin snapshot buffer closed")?;
        if buffer.path != document.path
            || buffer.text.source_version() != document.text
            || buffer.read_only
        {
            return Err(
                "plugin snapshot document incarnation, content or write policy changed".into(),
            );
        }
        Ok(&buffer.text)
    }
}
struct Ready {
    key: OriginKey,
    invocation: Invocation,
    proposals: Option<Vec<PluginEditProposal>>,
}
#[derive(Default)]
pub(super) struct PluginEditSources {
    pending: Vec<Invocation>,
    ready: Option<Ready>,
    closed: bool,
}
pub(super) struct InvocationLoan<'a> {
    pending: &'a mut Vec<Invocation>,
    admission: PluginInvocation<'a>,
    invocation: Invocation,
    editor: &'a EditorState,
    _workspace: &'a WorkspaceSession,
    _configuration: &'a u64,
}
impl InvocationLoan<'_> {
    pub(super) fn submit(self, mut request: PluginExecutionRequest) -> Result<(), String> {
        self.admission.check(&request).map_err(|e| e.to_string())?;
        // Only this source serializes the snapshot attached to its provenance.
        request.snapshot = self
            .invocation
            .document
            .as_ref()
            .map(|document| PluginSnapshot {
                buffer: document.id.data().as_ffi(),
                revision: document.revision.0,
                utf8: self.editor.buffers[document.id].text.to_string(),
            });
        self.admission.submit(request).map_err(|e| e.to_string())?;
        self.pending.push(self.invocation);
        Ok(())
    }
}
pub(super) struct SourceExecution {
    pub execution: PluginExecution,
    pub load: PluginLoadId,
    pub preview: Result<Option<OriginKey>, String>,
}
pub(super) struct PreparedPluginEdit {
    pub sources: WorkspaceEditSources,
    pub draft: WorkspaceEditDraft,
}
impl PluginEditSources {
    pub(super) fn prepare<'a>(
        &'a mut self,
        manager: &'a mut PluginManager,
        invocation: (&str, &str),
        editor: &'a EditorState,
        workspace: &'a WorkspaceSession,
        configuration: &'a u64,
        maximum: usize,
    ) -> Result<InvocationLoan<'a>, String> {
        let (plugin, command) = invocation;
        if self.closed || self.pending.len() >= 64 || *configuration == u64::MAX || maximum == 0 {
            return Err("plugin source admission is closed, full or exhausted".into());
        }
        if editor.window_version().revision().is_none() {
            return Err("plugin editor source exhausted".into());
        }
        let document = editor
            .active_buffer_id()
            .and_then(|id| editor.buffers.get(id).map(|buffer| (id, buffer)));
        if document.is_some_and(|(_, buffer)| buffer.text.len_bytes() > maximum) {
            return Err("plugin snapshot exceeds its admitted byte limit".into());
        }
        // Reserves actual bounded queue capacity before snapshot/metadata materialization.
        let admission = manager
            .prepare_execution(plugin, command)
            .map_err(|e| e.to_string())?;
        let sources = document
            .filter(|(_, buffer)| buffer.path.is_some())
            .map(|(id, _)| editor.workspace_edit_source(id))
            .transpose()?;
        let invocation = Invocation {
            origin: admission.origin().clone(),
            editor: editor.window_version(),
            workspace: workspace.version(),
            configuration: *configuration,
            document: document.map(|(id, buffer)| Document {
                id,
                path: buffer.path.clone(),
                text: buffer.text.source_version(),
                revision: buffer.text.revision(),
            }),
            sources,
            maximum,
        };
        Ok(InvocationLoan {
            pending: &mut self.pending,
            admission,
            invocation,
            editor,
            _workspace: workspace,
            _configuration: configuration,
        })
    }
    pub(super) fn receive(
        &mut self,
        reply: PluginExecutionReply,
        manager: &PluginManager,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<SourceExecution, String> {
        let (mut execution, origin) = reply.into_parts();
        let origin = origin.ok_or("plugin execution has no requesting-source identity")?;
        let index = self
            .pending
            .iter()
            .position(|p| p.origin.same(&origin))
            .ok_or("plugin invocation retired or result duplicated")?;
        let invocation = self.pending.remove(index);
        if self.closed || !invocation.current(manager, workspace, configuration) {
            return Err("plugin invocation load, workspace or configuration retired".into());
        }
        let load = origin.load();
        let proposals = std::mem::take(&mut execution.proposals);
        let preview = if proposals.is_empty() {
            Ok(None)
        } else if self.ready.is_some() {
            Err("another plugin edit preview is pending".into())
        } else if proposals.len() > 1024
            || proposals.iter().map(|p| p.edits.len()).sum::<usize>() > 4096
            || proposals
                .iter()
                .flat_map(|p| &p.edits)
                .try_fold(0usize, |n, e| n.checked_add(e.replacement.len()))
                .is_none_or(|n| n > invocation.maximum)
        {
            Err("plugin edits exceed their source safety limit".into())
        } else {
            let key = OriginKey::new();
            self.ready = Some(Ready {
                key: key.clone(),
                invocation,
                proposals: Some(proposals),
            });
            Ok(Some(key))
        };
        Ok(SourceExecution {
            execution,
            load,
            preview,
        })
    }
    pub(super) fn owns(&self, key: &OriginKey) -> bool {
        self.ready.as_ref().is_some_and(|r| r.key.same(key))
    }
    pub(super) fn check(
        &self,
        key: &OriginKey,
        manager: &PluginManager,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        let ready = self
            .ready
            .as_ref()
            .filter(|r| r.key.same(key))
            .ok_or("plugin preview source retired")?;
        if self.closed || !ready.invocation.current(manager, workspace, configuration) {
            return Err("plugin preview load, workspace or configuration retired".into());
        }
        ready.invocation.document(editor)?;
        Ok(())
    }
    pub(super) fn prepare_preview(
        &mut self,
        key: &OriginKey,
        manager: &PluginManager,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<PreparedPluginEdit, String> {
        self.check(key, manager, editor, workspace, configuration)?;
        let ready = self.ready.as_mut().expect("checked preview");
        let text = ready.invocation.document(editor)?;
        let document = ready
            .invocation
            .document
            .as_ref()
            .expect("checked document");
        let path = document
            .path
            .as_ref()
            .ok_or("plugin edits require a named snapshot document")?;
        let proposals = ready
            .proposals
            .as_ref()
            .ok_or("plugin preview already prepared")?;
        // Validate the entire batch before converting or materializing editor snapshots.
        for proposal in proposals {
            if proposal.buffer != document.id.data().as_ffi()
                || proposal.revision != document.revision.0
            {
                return Err("plugin edit did not target its invocation snapshot".into());
            }
            for edit in &proposal.edits {
                let range = Self::range(edit.start, edit.end)?;
                text.validate_range(range).map_err(|e| e.to_string())?;
            }
        }
        let sources = ready
            .invocation
            .sources
            .take()
            .ok_or("plugin preview sources already consumed")?;
        let edits = ready
            .proposals
            .take()
            .expect("checked batch")
            .into_iter()
            .flat_map(|p| p.edits)
            .map(|edit| {
                Ok(TextEdit::new(
                    Self::range(edit.start, edit.end)?,
                    edit.replacement,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(PreparedPluginEdit {
            sources,
            draft: WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(document.revision),
                edits,
            },
        })
    }
    fn range(start: u64, end: u64) -> Result<ByteRange, String> {
        ByteRange::new(
            ByteOffset(usize::try_from(start).map_err(|e| e.to_string())?),
            ByteOffset(usize::try_from(end).map_err(|e| e.to_string())?),
        )
        .map_err(|e| e.to_string())
    }
    pub(super) fn finish(&mut self, key: &OriginKey) -> Result<(), String> {
        if !self.owns(key) {
            return Err("plugin proposal decision duplicated or retired".into());
        }
        self.ready = None; // One-way proposal: no invented applied/failed wire acknowledgement.
        Ok(())
    }
    pub(super) fn retire(
        &mut self,
        manager: &PluginManager,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> (usize, Option<OriginKey>) {
        let before = self.pending.len();
        self.pending
            .retain(|p| !self.closed && p.current(manager, workspace, configuration));
        let key = self
            .ready
            .as_ref()
            .filter(|r| {
                self.check(&r.key, manager, editor, workspace, configuration)
                    .is_err()
            })
            .map(|r| r.key.clone());
        (before - self.pending.len(), key)
    }
    pub(super) fn close(&mut self) -> usize {
        let discarded = self.pending.len() + usize::from(self.ready.is_some());
        self.closed = true;
        self.pending.clear();
        self.ready = None;
        discarded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::text::TextBuffer,
        extensions::{ExecutionFixture, OwnedPluginEvent, PluginEdit, empty_execution_request},
    };
    fn reply(manager: &mut PluginManager) -> PluginExecutionReply {
        let OwnedPluginEvent::Executed(reply) = manager.poll_owned().unwrap().pop().unwrap() else {
            panic!("execution reply");
        };
        reply
    }
    #[test]
    fn plugin_invocation_owns_snapshot_queue_admission_and_preview_incarnation() {
        let root = tempfile::tempdir().unwrap();
        let scope = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "root".into(),
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let workspace =
            WorkspaceSession::new(scope, root.path().join("trust.json"), 1, false, false, 1);
        let version = workspace.version();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor.open_text(TextBuffer::from_text("éab"), Some(path));
        let (mut manager, endpoint) = ExecutionFixture::new(1);
        let mut source = PluginEditSources::default();
        let submit =
            |source: &mut PluginEditSources, manager: &mut PluginManager, editor: &EditorState| {
                let mut request = empty_execution_request();
                request.snapshot = Some(PluginSnapshot {
                    buffer: 999,
                    revision: 999,
                    utf8: "forged payload".into(),
                });
                source
                    .prepare(manager, ("demo", "run"), editor, &workspace, &1, 1024)
                    .unwrap()
                    .submit(request)
                    .unwrap();
            };
        let mut wrong = empty_execution_request();
        wrong.command = "different".into();
        let before = TextBuffer::test_materializations();
        assert!(
            source
                .prepare(&mut manager, ("demo", "run"), &editor, &workspace, &1, 1024)
                .unwrap()
                .submit(wrong)
                .is_err()
        );
        assert_eq!(
            TextBuffer::test_materializations(),
            before,
            "request mismatch rejected before snapshot materialization"
        );
        submit(&mut source, &mut manager, &editor);
        let before = TextBuffer::test_materializations();
        assert!(
            source
                .prepare(&mut manager, ("demo", "run"), &editor, &workspace, &1, 1024)
                .is_err()
        );
        assert_eq!(
            TextBuffer::test_materializations(),
            before,
            "known full queue refused before snapshot serialization"
        );
        let first = endpoint.take();
        assert_eq!(first.snapshot().unwrap().utf8, "éab");
        assert_eq!(first.snapshot().unwrap().buffer, buffer.data().as_ffi());
        let loan = source
            .prepare(&mut manager, ("demo", "run"), &editor, &workspace, &1, 1024)
            .unwrap();
        drop(loan); // Reservation returned and no pending source record added.
        submit(&mut source, &mut manager, &editor);
        let second = endpoint.take();
        assert!(
            !first.same(&second),
            "same public generation is not invocation identity"
        );
        let proposal = || {
            vec![PluginEditProposal {
                buffer: buffer.data().as_ffi(),
                revision: 0,
                edits: vec![PluginEdit {
                    start: 2,
                    end: 3,
                    replacement: "X".into(),
                }],
            }]
        };
        endpoint.reply(&first, proposal());
        let key = source
            .receive(reply(&mut manager), &manager, &version, 1)
            .unwrap()
            .preview
            .unwrap()
            .unwrap();
        let mut foreign_editor = EditorState::new(root.path().into());
        foreign_editor.open_text(
            editor.buffers[buffer].text.clone(),
            editor.buffers[buffer].path.clone(),
        );
        assert!(
            source
                .prepare_preview(&key, &manager, &foreign_editor, &version, 1)
                .is_err()
        );
        let prepared = source
            .prepare_preview(&key, &manager, &editor, &version, 1)
            .unwrap();
        assert_eq!(prepared.draft.edits[0].range().start, ByteOffset(2));
        assert!(
            source
                .prepare_preview(&key, &manager, &editor, &version, 1)
                .is_err()
        );
        source.finish(&key).unwrap();
        assert!(source.finish(&key).is_err());
        endpoint.reply(&first, proposal());
        assert!(
            source
                .receive(reply(&mut manager), &manager, &version, 1)
                .is_err()
        );
        endpoint.reply(&second, proposal());
        let key = source
            .receive(reply(&mut manager), &manager, &version, 1)
            .unwrap()
            .preview
            .unwrap()
            .unwrap();
        editor.buffers[buffer].text = TextBuffer::from_text("éab"); // Equal numeric revision, different incarnation.
        let before = TextBuffer::test_materializations();
        assert!(
            source
                .prepare_preview(&key, &manager, &editor, &version, 1)
                .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), before);
        source.finish(&key).unwrap();
        submit(&mut source, &mut manager, &editor);
        let fresh = endpoint.take();
        let mut bad = proposal();
        bad[0].edits.push(PluginEdit {
            start: 1,
            end: 2,
            replacement: "bad".into(),
        });
        endpoint.reply(&fresh, bad);
        let key = source
            .receive(reply(&mut manager), &manager, &version, 1)
            .unwrap()
            .preview
            .unwrap()
            .unwrap();
        assert!(
            source
                .prepare_preview(&key, &manager, &editor, &version, 1)
                .is_err()
        );
        assert_eq!(editor.buffers[buffer].text.to_string(), "éab");
        source.finish(&key).unwrap();
        submit(&mut source, &mut manager, &editor);
        let late = endpoint.take();
        endpoint.reply(&late, proposal());
        let held = reply(&mut manager); // Real owned poll envelope collected before disable.
        manager.disable("demo").unwrap();
        assert!(source.receive(held, &manager, &version, 1).is_err());
        let (mut replacement, replacement_endpoint) = ExecutionFixture::new(1); // Same name and numeric load, different manager.
        submit(&mut source, &mut replacement, &editor);
        let new = replacement_endpoint.take();
        replacement_endpoint.reply(&new, proposal());
        let held = reply(&mut replacement);
        let (foreign_manager, _foreign_endpoint) = ExecutionFixture::new(1);
        assert!(source.receive(held, &foreign_manager, &version, 1).is_err());
        submit(&mut source, &mut replacement, &editor);
        let configured = replacement_endpoint.take();
        replacement_endpoint.reply(&configured, proposal());
        assert!(
            source
                .receive(reply(&mut replacement), &replacement, &version, 2)
                .is_err()
        );
        submit(&mut source, &mut replacement, &editor);
        let failed = replacement_endpoint.take();
        replacement_endpoint.fail(&failed);
        replacement.poll_owned().unwrap();
        assert_eq!(source.retire(&replacement, &editor, &version, 1).0, 1);
        source.close();
        assert!(
            source
                .prepare(
                    &mut replacement,
                    ("demo", "run"),
                    &editor,
                    &workspace,
                    &1,
                    1024
                )
                .is_err()
        );
    }
}
