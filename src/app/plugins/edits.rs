//! Plugin invocation snapshots and edit proposal lifetimes.
use crate::app::input::InputState;
use crate::app::windows::NavigationStamp;
use crate::app::windows::Windows;
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::app::workspace::edits::OriginKey;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::EditorWindowVersion;
use crate::editor::WorkspaceEditSources;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextEdit;
use crate::editor::text::TextVersion;
use crate::plugins::PluginEditProposal;
use crate::plugins::PluginExecution;
use crate::plugins::PluginExecutionReply;
use crate::plugins::PluginExecutionRequest;
use crate::plugins::PluginHost;
use crate::plugins::PluginInvocation;
use crate::plugins::PluginInvocationOrigin;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginSnapshot;
use crate::workspace::WorkspaceEditDraft;
use slotmap::Key;
use std::path::PathBuf;

#[derive(Clone)]
struct Document {
    id: BufferId,
    path: Option<PathBuf>,
    text: TextVersion,
    revision: BufferRevision,
}
struct Invocation {
    direct: bool,
    origin: PluginInvocationOrigin,
    editor: EditorWindowVersion,
    workspace: WorkspaceVersion,
    configuration: u64,
    document: Option<Document>,
    sources: Option<WorkspaceEditSources>,
    maximum: usize,
    input_capture: Option<std::sync::Arc<()>>,
    navigation: Option<NavigationStamp>,
    interaction_available: bool,
}
impl Invocation {
    pub(in crate::app) fn current(
        &self,
        manager: &PluginHost,
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
                "plugin snapshot document instance, content or write policy changed".into(),
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
    reservation: PluginInvocation<'a>,
    invocation: Invocation,
    editor: &'a EditorState,
    _workspace: &'a Workspace,
    _configuration: &'a u64,
}
impl InvocationLoan<'_> {
    pub(in crate::app) fn direct(mut self, target: Option<BufferId>) -> Result<Self, String> {
        if let Some(id) = target {
            let buffer = self
                .editor
                .buffers
                .get(id)
                .ok_or("plugin target buffer closed")?;
            if buffer.text.len_bytes() > self.invocation.maximum {
                return Err("plugin snapshot exceeds byte limit".into());
            }
            self.invocation.document = Some(Document {
                id,
                path: buffer.path.clone(),
                text: buffer.text.source_version(),
                revision: buffer.text.revision(),
            });
        }
        self.invocation.direct = true;
        Ok(self)
    }
    pub(in crate::app) fn origin(&self) -> &PluginInvocationOrigin {
        &self.invocation.origin
    }
    pub(in crate::app) fn submit(self, mut request: PluginExecutionRequest) -> Result<(), String> {
        self.reservation
            .check(&request)
            .map_err(|e| e.to_string())?;
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
        self.reservation
            .submit(request)
            .map_err(|e| e.to_string())?;
        self.pending.push(self.invocation);
        Ok(())
    }
}
#[derive(Clone)]
pub(super) struct ConfirmationSource {
    origin: PluginInvocationOrigin,
    editor: EditorWindowVersion,
    workspace: WorkspaceVersion,
    configuration: u64,
    document: Option<Document>,
    input_capture: Option<std::sync::Arc<()>>,
    navigation: Option<NavigationStamp>,
    interaction_available: bool,
}
impl ConfirmationSource {
    pub(in crate::app) fn origin(&self) -> &PluginInvocationOrigin {
        &self.origin
    }
    pub(in crate::app) fn manager_current(&self, manager: &PluginHost) -> bool {
        self.origin.current(manager)
    }
    pub(in crate::app) fn current(
        &self,
        manager: &PluginHost,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        self.manager_current(manager)
            && &self.workspace == workspace
            && self.configuration == configuration
            && self.editor.same_editor(&editor.window_version())
            && editor.window_version().revision().is_some()
            && self.document.as_ref().is_none_or(|document| {
                editor.buffers.get(document.id).is_some_and(|buffer| {
                    buffer.path == document.path
                        && buffer.text.source_version() == document.text
                        && buffer.text.revision() == document.revision
                })
            })
    }
    pub(in crate::app) fn navigation_current(
        &self,
        editor: &EditorState,
        windows: &Windows,
    ) -> bool {
        self.navigation.as_ref().is_none_or(|navigation| {
            windows.navigation_current(navigation, &editor.window_version(), editor.active_view())
        })
    }
    pub(in crate::app) fn presentation_current(
        &self,
        manager: &PluginHost,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &InputState,
        windows: &Windows,
    ) -> bool {
        self.interaction_available
            && self.current(manager, editor, workspace, configuration)
            && self
                .document
                .as_ref()
                .is_none_or(|document| editor.active_buffer_id() == Some(document.id))
            && self
                .input_capture
                .as_ref()
                .is_none_or(|capture| input.capture_current(capture))
            && self.navigation.as_ref().is_none_or(|navigation| {
                windows.navigation_current(
                    navigation,
                    &editor.window_version(),
                    editor.active_view(),
                )
            })
    }
}

pub(super) struct SourceExecution {
    pub execution: PluginExecution,
    pub load: PluginLoadId,
    pub confirmation_source: ConfirmationSource,
    pub confirmation_result: bool,
    pub source_actions_allowed: bool,
    pub preview: Result<Option<OriginKey>, String>,
}
pub(super) struct PreparedPluginEdit {
    pub sources: WorkspaceEditSources,
    pub draft: WorkspaceEditDraft,
}
impl PluginEditSources {
    #[cfg(test)]
    pub(super) fn prepare<'a>(
        &'a mut self,
        manager: &'a mut PluginHost,
        invocation: (&str, &str),
        editor: &'a EditorState,
        workspace: &'a Workspace,
        configuration: &'a u64,
        maximum: usize,
    ) -> Result<InvocationLoan<'a>, String> {
        self.prepare_interactive(
            manager,
            invocation,
            editor,
            workspace,
            configuration,
            (maximum, None, None, true),
        )
    }
    pub(super) fn prepare_interactive<'a>(
        &'a mut self,
        manager: &'a mut PluginHost,
        invocation: (&str, &str),
        editor: &'a EditorState,
        workspace: &'a Workspace,
        configuration: &'a u64,
        interaction: (
            usize,
            Option<std::sync::Arc<()>>,
            Option<NavigationStamp>,
            bool,
        ),
    ) -> Result<InvocationLoan<'a>, String> {
        let (plugin, command) = invocation;
        let (maximum, input_capture, navigation, interaction_available) = interaction;
        if self.closed || self.pending.len() >= 128 || *configuration == u64::MAX || maximum == 0 {
            return Err("plugin request queue is closed, full or exhausted".into());
        }
        if editor.window_version().revision().is_none() {
            return Err("plugin editor source exhausted".into());
        }
        let document = editor
            .active_buffer_id()
            .and_then(|id| editor.buffers.get(id).map(|buffer| (id, buffer)));
        if document.is_some_and(|(_, buffer)| buffer.text.len_bytes() > maximum) {
            return Err("plugin snapshot exceeds its requested byte limit".into());
        }
        // Reserves actual bounded queue capacity before snapshot/metadata materialization.
        let reservation = manager
            .prepare_execution(plugin, command)
            .map_err(|e| e.to_string())?;
        let sources = document
            .filter(|(_, buffer)| buffer.path.is_some())
            .map(|(id, _)| editor.workspace_edit_source(id))
            .transpose()?;
        let invocation = Invocation {
            direct: false,
            origin: reservation.origin().clone(),
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
            input_capture,
            navigation,
            interaction_available,
        };
        Ok(InvocationLoan {
            pending: &mut self.pending,
            reservation,
            invocation,
            editor,
            _workspace: workspace,
            _configuration: configuration,
        })
    }
    pub(in crate::app) fn receive(
        &mut self,
        reply: PluginExecutionReply,
        manager: &PluginHost,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<SourceExecution, String> {
        let (mut execution, origin) = reply.into_parts();
        let origin = origin.ok_or("plugin execution has no requesting-source identity")?;
        let index = self
            .pending
            .iter()
            .position(|p| p.origin.same(&origin))
            .ok_or("plugin invocation discarded or result duplicated")?;
        let invocation = self.pending.remove(index);
        if self.closed || !invocation.origin.current(manager) {
            return Err("plugin invocation load unavailable".into());
        }
        let load = origin.load();
        let confirmation_source = ConfirmationSource {
            origin: invocation.origin.clone(),
            editor: invocation.editor.clone(),
            workspace: invocation.workspace.clone(),
            configuration: invocation.configuration,
            document: invocation.document.clone(),
            input_capture: invocation.input_capture.clone(),
            navigation: invocation.navigation.clone(),
            interaction_available: invocation.interaction_available,
        };
        let source_actions_allowed = invocation.current(manager, workspace, configuration);
        if !source_actions_allowed && execution.confirmation_requests.is_empty() {
            return Err("plugin invocation workspace or configuration changed".into());
        }
        let proposals = if invocation.direct {
            Vec::new()
        } else {
            std::mem::take(&mut execution.proposals)
        };
        let preview = if !source_actions_allowed {
            Err("plugin invocation source changed before its reply".into())
        } else if proposals.is_empty() {
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
            confirmation_source,
            confirmation_result: false,
            source_actions_allowed,
            preview,
        })
    }
    pub(in crate::app) fn owns(&self, key: &OriginKey) -> bool {
        self.ready.as_ref().is_some_and(|r| r.key.same(key))
    }
    pub(in crate::app) fn check(
        &self,
        key: &OriginKey,
        manager: &PluginHost,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        let ready = self
            .ready
            .as_ref()
            .filter(|r| r.key.same(key))
            .ok_or("plugin preview source changed")?;
        if self.closed || !ready.invocation.current(manager, workspace, configuration) {
            return Err("plugin preview load, workspace or configuration changed".into());
        }
        ready.invocation.document(editor)?;
        Ok(())
    }
    pub(in crate::app) fn prepare_preview(
        &mut self,
        key: &OriginKey,
        manager: &PluginHost,
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
    pub(in crate::app) fn range(start: u64, end: u64) -> Result<ByteRange, String> {
        ByteRange::new(
            ByteOffset(usize::try_from(start).map_err(|e| e.to_string())?),
            ByteOffset(usize::try_from(end).map_err(|e| e.to_string())?),
        )
        .map_err(|e| e.to_string())
    }
    pub(in crate::app) fn finish(&mut self, key: &OriginKey) -> Result<(), String> {
        if !self.owns(key) {
            return Err("plugin proposal decision duplicated or discarded".into());
        }
        self.ready = None; // One-way proposal: no invented applied/failed wire acknowledgement.
        Ok(())
    }
    pub(in crate::app) fn discard_stale(
        &mut self,
        manager: &PluginHost,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> (usize, Option<OriginKey>) {
        let before = self.pending.len();
        self.pending
            .retain(|pending| !self.closed && pending.origin.current(manager));
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
    pub(in crate::app) fn close(&mut self) -> usize {
        let discarded = self.pending.len() + usize::from(self.ready.is_some());
        self.closed = true;
        self.pending.clear();
        self.ready = None;
        discarded
    }
}

#[cfg(test)]
mod tests {
    use crate::app::plugins::edits::*;
    use crate::editor::text::TextBuffer;
    use crate::plugins::ExecutionFixture;
    use crate::plugins::OwnedPluginEvent;
    use crate::plugins::PluginEdit;
    use crate::plugins::empty_execution_request;
    pub(in crate::app) fn reply(manager: &mut PluginHost) -> PluginExecutionReply {
        let OwnedPluginEvent::Executed(reply) = manager.poll_owned().unwrap().pop().unwrap() else {
            panic!("execution reply");
        };
        reply
    }
    #[test]
    fn plugin_invocation_owns_snapshot_queue_reservation_and_preview_instance() {
        let root = tempfile::tempdir().unwrap();
        let scope = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "root".into(),
                crate::files::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let workspace = Workspace::new(scope, root.path().join("trust.json"), 1, false, false, 1);
        let version = workspace.version();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("éab"), Some(path))
            .unwrap();
        let (mut manager, endpoint) = ExecutionFixture::new(1);
        let mut source = PluginEditSources::default();
        let submit =
            |source: &mut PluginEditSources, manager: &mut PluginHost, editor: &EditorState| {
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
            "same request ID is not invocation identity"
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
        foreign_editor
            .open_text(
                editor.buffers[buffer].text.clone(),
                editor.buffers[buffer].path.clone(),
            )
            .unwrap();
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
        editor.buffers[buffer].text = TextBuffer::from_text("éab"); // Equal numeric revision, different instance.
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
        assert_eq!(
            source.discard_stale(&replacement, &editor, &version, 1).0,
            1
        );
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
