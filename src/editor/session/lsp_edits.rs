//! Source-private LSP edit decoding, requesting-session identity and reply obligations.
use super::workspace_edits::{InputFile, OriginKey, PreparedFiles};
use crate::{
    editor::{
        BufferId, TransactionId,
        text::{
            BufferRevision, ByteRange, PositionCodec, PositionEncoding, TextBuffer, TextEdit,
            TextEditSet,
        },
        transaction::{EditorOperation, EditorTransaction, Precondition},
    },
    language::lsp::{
        ApplyEditReply, EditContext, LspDocumentEdit, LspService, LspTextEdit, LspWorkspaceEdit,
        OwnedLspEvent, PendingEditReply, path_from_file_uri,
    },
    workspace::WorkspaceEditDraft,
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

pub(super) struct EditDocument<'a> {
    pub id: BufferId,
    pub path: &'a Path,
    pub text: &'a TextBuffer,
}
pub(super) struct SourcePlan {
    pub key: OriginKey,
    pub files: Vec<InputFile>,
    pub all_files: bool,
}
struct Request {
    key: OriginKey,
    documents: Option<Vec<LspDocumentEdit>>,
    paths: Vec<(String, PathBuf)>,
    encoding: PositionEncoding,
    context: Option<EditContext>,
    reply: Option<ApplyEditReply>,
}
#[derive(Clone, Debug)]
pub(super) struct CodeActionKey {
    set: std::sync::Arc<()>,
    index: usize,
}
pub(super) struct CodeActionChoice {
    pub key: CodeActionKey,
    pub title: String,
    pub detail: String,
}
struct CodeActions {
    key: std::sync::Arc<()>,
    actions: Vec<crate::language::lsp::LspCodeAction>,
    context: Option<EditContext>,
}
#[derive(Default)]
pub(super) struct LspEditSources {
    actions: Option<CodeActions>,
    active: Option<Request>,
    replies: VecDeque<PendingEditReply>,
}
impl LspEditSources {
    pub(super) fn install_actions(
        &mut self,
        actions: Vec<crate::language::lsp::LspCodeAction>,
        context: Option<EditContext>,
        service: &LspService,
        maximum: usize,
    ) -> Result<Vec<CodeActionChoice>, String> {
        if actions.len() > maximum
            || context
                .as_ref()
                .is_some_and(|context| !service.edit_context_current(context))
        {
            return Err("code-action source retired or exceeded its item limit".into());
        }
        let key = std::sync::Arc::new(());
        let rows = actions
            .iter()
            .enumerate()
            .map(|(index, action)| CodeActionChoice {
                key: CodeActionKey {
                    set: key.clone(),
                    index,
                },
                title: action.title.clone(),
                detail: action.kind.clone().unwrap_or_default(),
            })
            .collect();
        self.actions = Some(CodeActions {
            key,
            actions,
            context,
        });
        Ok(rows)
    }
    pub(super) fn retain_actions<'a>(
        &mut self,
        displayed: impl Iterator<Item = &'a CodeActionKey>,
    ) {
        if self.actions.as_ref().is_some_and(|set| {
            !displayed
                .into_iter()
                .any(|key| std::sync::Arc::ptr_eq(&key.set, &set.key))
        }) {
            self.actions = None;
        }
    }
    pub(super) fn clear_actions(&mut self) {
        self.actions = None;
    }
    pub(super) fn take_action(
        &mut self,
        key: &CodeActionKey,
        service: &LspService,
    ) -> Result<(crate::language::lsp::LspCodeAction, Option<EditContext>), String> {
        let set = self.actions.as_ref().ok_or("code actions closed")?;
        if !std::sync::Arc::ptr_eq(&set.key, &key.set)
            || key.index >= set.actions.len()
            || set
                .context
                .as_ref()
                .is_some_and(|context| !service.edit_context_current(context))
        {
            return Err("code-action source retired".into());
        }
        let set = self.actions.take().expect("checked action source");
        Ok((
            set.actions
                .into_iter()
                .nth(key.index)
                .expect("checked action index"),
            set.context,
        ))
    }
    // Reserve space for the active request's eventual response. Stop draining the
    // bounded backend event channel rather than dropping critical reply obligations.
    pub(super) fn poll_event(&self, service: &LspService) -> Option<OwnedLspEvent> {
        if self.replies.len() + usize::from(self.active.as_ref().is_some_and(|r| r.reply.is_some()))
            >= 128
        {
            return None;
        }
        service.try_owned_event()
    }
    pub(super) fn flush(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        for _ in 0..self.replies.len().min(128) {
            let Some(mut reply) = self.replies.pop_front() else {
                break;
            };
            match reply.try_send() {
                Ok(false) => self.replies.push_back(reply),
                Ok(true) => {}
                Err(error) => errors.push(error),
            }
        }
        errors
    }
    pub(super) fn pending_replies(&self) -> bool {
        !self.replies.is_empty()
    }
    pub(super) fn reject_reply(&mut self, reply: Option<ApplyEditReply>, reason: String) {
        if let Some(reply) = reply {
            self.replies.push_back(reply.finish(false, Some(reason)));
        }
    }
    pub(super) fn start(
        &mut self,
        edit: LspWorkspaceEdit,
        encoding: PositionEncoding,
        context: Option<EditContext>,
        reply: Option<ApplyEditReply>,
        current: &[EditDocument<'_>],
        service: &LspService,
    ) -> Result<SourcePlan, String> {
        let context = reply.as_ref().map(|r| r.context().clone()).or(context);
        let encoding = context.as_ref().map_or(encoding, EditContext::encoding);
        let parsed = (|| {
            if self.active.is_some() {
                return Err("another source edit is pending".into());
            }
            if edit.documents.len() + edit.resources.len() > 1024
                || edit
                    .documents
                    .iter()
                    .map(|doc| doc.edits.len())
                    .sum::<usize>()
                    > 100_000
            {
                return Err("workspace edit exceeds its source safety limit".into());
            }
            if reply.is_some() && !edit.resources.is_empty() {
                return Err("server-requested resource operations require explicit user application and are not supported in applyEdit; no changes were applied".into());
            }
            Self::validate_context(context.as_ref(), &[], current, service)?;
            let mut files = Vec::new();
            let mut paths = Vec::new();
            for doc in &edit.documents {
                let path = path_from_file_uri(&doc.uri)
                    .ok_or_else(|| format!("unsupported workspace-edit URI {}", doc.uri))?;
                if let Some(buffer) = current.iter().find(|buffer| buffer.path == path) {
                    if doc
                        .version
                        .is_some_and(|version| version != buffer.text.revision().0)
                    {
                        return Err("workspace edit document revision is stale".into());
                    }
                }
                if let Some(context) = context.as_ref() {
                    let actual = current
                        .iter()
                        .find(|buffer| buffer.path == path)
                        .map(|buffer| (buffer.id, buffer.text.source_version()));
                    if actual.as_ref() != context.document_source(&doc.uri) {
                        return Err("workspace edit document incarnation/content is stale".into());
                    }
                }
                paths.push((doc.uri.clone(), path.clone()));
                files.push(InputFile::Read(path));
            }
            Self::validate_context(context.as_ref(), &paths, current, service)?;
            for resource in &edit.resources {
                use crate::language::lsp::LspResourceOperation as R;
                let path = |uri: &str| {
                    path_from_file_uri(uri)
                        .ok_or_else(|| "unsupported workspace resource URI".to_string())
                };
                let (first, second) = match resource {
                    R::Create { uri, .. } | R::Delete { uri, .. } => (uri.as_str(), None),
                    R::Rename {
                        old_uri, new_uri, ..
                    } => (old_uri.as_str(), Some(new_uri.as_str())),
                };
                for uri in std::iter::once(first).chain(second) {
                    let requested = path(uri)?;
                    if let Some(context) = context.as_ref() {
                        let actual = current
                            .iter()
                            .find(|buffer| buffer.path == requested)
                            .map(|buffer| (buffer.id, buffer.text.source_version()));
                        if actual.as_ref() != context.document_source(uri) {
                            return Err(
                                "workspace resource document incarnation/content is stale".into()
                            );
                        }
                    }
                    paths.push((uri.to_owned(), requested));
                }
                files.push(match resource {
                    R::Create {
                        uri,
                        overwrite: false,
                        ignore_if_exists: false,
                    } => InputFile::Create(path(uri)?),
                    R::Rename {
                        old_uri,
                        new_uri,
                        overwrite: false,
                        ignore_if_exists: false,
                    } => InputFile::Resource {
                        source: path(old_uri)?,
                        target: Some(path(new_uri)?),
                    },
                    R::Delete {
                        uri,
                        recursive: false,
                        ignore_if_not_exists: false,
                    } => InputFile::Resource {
                        source: path(uri)?,
                        target: None,
                    },
                    _ => return Err("unsupported workspace resource operation".into()),
                });
            }
            Ok::<_, String>((files, paths))
        })();
        let (files, paths) = match parsed {
            Ok(value) => value,
            Err(error) => {
                self.reject_reply(reply, error.clone());
                return Err(error);
            }
        };
        let key = OriginKey::new();
        let all_files = reply.is_some();
        self.active = Some(Request {
            key: key.clone(),
            documents: Some(edit.documents),
            paths,
            encoding,
            context,
            reply,
        });
        Ok(SourcePlan {
            key,
            files,
            all_files,
        })
    }
    fn validate_context(
        context: Option<&EditContext>,
        paths: &[(String, PathBuf)],
        current: &[EditDocument<'_>],
        service: &LspService,
    ) -> Result<(), String> {
        let Some(context) = context else {
            return Ok(());
        };
        if !service.edit_context_current(context) {
            return Err("requesting language-server session has retired".into());
        }
        for (uri, path) in paths {
            let expected = context.document_source(uri);
            let actual = current
                .iter()
                .find(|buffer| buffer.path == path)
                .map(|buffer| (buffer.id, buffer.text.source_version()));
            if actual.as_ref() != expected {
                return Err(format!(
                    "{} no longer has the requesting session's document incarnation/content",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    pub(super) fn check(
        &self,
        key: &OriginKey,
        current: &[EditDocument<'_>],
        service: &LspService,
    ) -> Result<(), String> {
        let request = self
            .active
            .as_ref()
            .filter(|r| r.key.same(key))
            .ok_or("workspace edit origin is foreign or retired")?;
        Self::validate_context(request.context.as_ref(), &request.paths, current, service)
    }
    pub(super) fn decode(
        &mut self,
        key: &OriginKey,
        files: PreparedFiles<'_>,
    ) -> Result<Vec<WorkspaceEditDraft>, String> {
        let request = self
            .active
            .as_mut()
            .filter(|r| r.key.same(key))
            .ok_or("workspace edit origin retired before decoding")?;
        let documents = request
            .documents
            .take()
            .ok_or("workspace source was already decoded")?;
        documents
            .into_iter()
            .map(|document| {
                let path =
                    path_from_file_uri(&document.uri).ok_or("unsupported workspace edit URI")?;
                let snapshot = files
                    .snapshot(&path)
                    .ok_or("workspace edit snapshot is missing")?;
                let text = TextBuffer::from_bytes(&snapshot.bytes).map_err(|e| e.to_string())?;
                Ok(WorkspaceEditDraft {
                    canonical_path: snapshot.canonical_path.clone(),
                    // gopls sends version 0 for a file that is not open; only an open buffer has
                    // a version the edit could be out of date against.
                    expected_revision: snapshot
                        .revision
                        .and(document.version.map(BufferRevision))
                        .or(snapshot.revision),
                    edits: decode_edits(document.edits, &text, request.encoding)?,
                })
            })
            .collect()
    }
    pub(super) fn finish(
        &mut self,
        key: &OriginKey,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        if self.active.as_ref().is_none_or(|r| !r.key.same(key)) {
            return Err("workspace edit origin already completed or foreign".into());
        }
        let request = self.active.take().expect("checked source");
        if let Some(reply) = request.reply {
            self.replies.push_back(reply.finish(applied, reason));
        }
        Ok(())
    }
    pub(super) fn completion_transaction(
        edit: LspWorkspaceEdit,
        encoding: PositionEncoding,
        current: &[EditDocument<'_>],
        id: TransactionId,
    ) -> Result<EditorTransaction, String> {
        if !edit.resources.is_empty() {
            return Err("completion resource edits require a workspace preview".into());
        }
        let mut transaction = EditorTransaction::new(id);
        transaction.label = Some("LSP workspace edit".into());
        for document in edit.documents {
            let path = path_from_file_uri(&document.uri).ok_or("unsupported workspace-edit URI")?;
            let buffer = current
                .iter()
                .find(|buffer| buffer.path == path)
                .ok_or_else(|| {
                    format!(
                        "workspace edit targets unopened file {}; preview required",
                        path.display()
                    )
                })?;
            let revision = buffer.text.revision();
            if document
                .version
                .is_some_and(|version| version != revision.0)
            {
                return Err("workspace edit document is stale".into());
            }
            let edits = TextEditSet::new(
                revision,
                decode_edits(document.edits, buffer.text, encoding)?,
            );
            buffer
                .text
                .validate_edits(&edits)
                .map_err(|e| e.to_string())?;
            transaction.preconditions.extend([
                Precondition::BufferRevision {
                    buffer: buffer.id,
                    revision,
                },
                Precondition::BufferWritable(buffer.id),
            ]);
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer: buffer.id,
                edits,
            });
        }
        Ok(transaction)
    }
}
fn decode_edits(
    edits: Vec<LspTextEdit>,
    text: &TextBuffer,
    encoding: PositionEncoding,
) -> Result<Vec<TextEdit>, String> {
    let codec = PositionCodec::new(text);
    edits
        .into_iter()
        .map(|edit| {
            let start = codec
                .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                .map_err(|e| e.to_string())?;
            let end = codec
                .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                .map_err(|e| e.to_string())?;
            Ok(TextEdit::new(
                ByteRange::new(start, end).map_err(|e| e.to_string())?,
                edit.new_text,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::EditorState,
        language::lsp::{LspEvent, LspPosition, LspRange, ServerRequestId},
        workspace::{RootIdentity, WorkspaceId, WorkspaceState},
    };
    fn documents(editor: &EditorState) -> Vec<EditDocument<'_>> {
        editor
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect()
    }
    #[test]
    fn requesting_edit_origin_preserves_encoding_incarnation_and_acknowledges_only_its_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.rs");
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor.open_text(TextBuffer::from_text("éab"), Some(path.clone()));
        let uri = crate::language::lsp::file_uri(&path);
        let edit = || LspWorkspaceEdit {
            documents: vec![LspDocumentEdit {
                uri: uri.clone(),
                version: Some(0),
                edits: vec![LspTextEdit {
                    range: LspRange {
                        start: LspPosition {
                            line: 0,
                            character: 2,
                        },
                        end: LspPosition {
                            line: 0,
                            character: 3,
                        },
                    },
                    new_text: "X".into(),
                }],
            }],
            resources: Vec::new(),
        };
        let mut service = LspService::new();
        let mut endpoint = service.replace_edit_source_fixture(
            uri.clone(),
            (buffer, editor.buffers[buffer].text.source_version()),
            PositionEncoding::Utf8,
        );
        let mut sources = LspEditSources::default();
        assert!(endpoint.request(ServerRequestId::Number(1), edit()));
        assert!(!endpoint.request(ServerRequestId::Number(1), edit()));
        let packet = sources.poll_event(&service).unwrap();
        let LspEvent::ApplyEditRequest { edit, .. } = packet.event else {
            panic!("request");
        };
        // Deliberately supply the wrong ambient encoding; the origin's UTF-8 wins.
        let plan = sources
            .start(
                edit,
                PositionEncoding::Utf16,
                Some(packet.context),
                packet.reply,
                &documents(&editor),
                &service,
            )
            .unwrap();
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = crate::editor::session::workspace::WorkspaceSession::new(
            scope.clone(),
            root.path().join("trust.json"),
            1,
            false,
            false,
            1,
        );
        let version = workspace.version();
        let mut owner = crate::editor::session::workspace_edits::WorkspaceEditSession::default();
        let ticket = owner
            .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
            .unwrap();
        owner
            .start_files(
                ticket,
                "server edit".into(),
                Some(plan.key.clone()),
                plan.all_files,
                plan.files,
                (vec![root.path().into()], 1024),
            )
            .unwrap();
        let snapshot = editor
            .workspace_snapshot(owner.preparing_sources().unwrap(), &path, 1024)
            .unwrap()
            .unwrap();
        owner.open_snapshot(0, snapshot).unwrap();
        let drafts = sources
            .decode(&plan.key, owner.prepared_files().unwrap())
            .unwrap();
        assert_eq!(drafts[0].edits[0].range().start.get(), 2);
        owner
            .finish_preparation(drafts, &scope, &version, 1)
            .unwrap();
        assert!(
            owner.toggle(0).is_err(),
            "cannot report partial server edit as applied"
        );
        let wrong = editor.draft();
        let before = TextBuffer::test_materializations();
        assert!(
            owner.stage_approval(&wrong, Vec::new()).is_err(),
            "an unrelated draft is not this preview"
        );
        assert_eq!(TextBuffer::test_materializations(), before);
        let (captured, preview) = owner.approval(&version, 1).unwrap();
        let draft = editor
            .prepare_workspace_edit(captured, preview, TransactionId(1))
            .unwrap();
        owner.stage_approval(&draft, Vec::new()).unwrap();
        assert!(owner.accepted(editor.publication()).is_err());
        editor = draft.accept(&editor.version()).unwrap();
        let origin = owner.accepted(editor.publication()).unwrap().unwrap();
        assert_eq!(editor.buffers[buffer].text.to_string(), "éXb");
        endpoint.block_responses();
        sources.finish(&origin, true, None).unwrap();
        assert!(sources.finish(&origin, true, None).is_err());
        assert!(sources.flush().is_empty());
        assert!(sources.pending_replies());
        endpoint.unblock_responses();
        assert!(sources.flush().is_empty());
        assert!(
            sources.pending_replies(),
            "queue admission is not a wire acknowledgement"
        );
        assert_eq!(
            endpoint.response(),
            Some((ServerRequestId::Number(1), true))
        );
        assert!(sources.flush().is_empty());
        assert!(!sources.pending_replies());
        assert!(endpoint.response().is_none());

        // Retain an old request across replacement with the same external name/ID.
        let empty = || LspWorkspaceEdit {
            documents: Vec::new(),
            resources: Vec::new(),
        };
        assert!(endpoint.request(ServerRequestId::String("same".into()), empty()));
        let packet = sources.poll_event(&service).unwrap();
        let action = || crate::language::lsp::LspCodeAction {
            title: "owned action".into(),
            kind: None,
            preferred: false,
            edit: Some(empty()),
        };
        let choices = sources
            .install_actions(vec![action()], Some(packet.context.clone()), &service, 4)
            .unwrap();
        let old_action = choices[0].key.clone();
        let mut replacement = service.replace_edit_source_fixture(
            uri.clone(),
            (buffer, editor.buffers[buffer].text.source_version()),
            PositionEncoding::Utf16,
        );
        assert!(!service.owns_event_context(&packet.context));
        assert!(sources.take_action(&old_action, &service).is_err());
        let LspEvent::ApplyEditRequest { edit, .. } = packet.event else {
            panic!("request");
        };
        assert!(
            sources
                .start(
                    edit,
                    PositionEncoding::Utf8,
                    Some(packet.context),
                    packet.reply,
                    &documents(&editor),
                    &service
                )
                .is_err()
        );
        sources.flush();
        assert_eq!(
            endpoint.response(),
            Some((ServerRequestId::String("same".into()), false))
        );
        assert!(replacement.response().is_none());
        sources.flush();

        // Server resource operations are refused before any text/editor/disk effect.
        let resource = LspWorkspaceEdit {
            documents: Vec::new(),
            resources: vec![crate::language::lsp::LspResourceOperation::Delete {
                uri: uri.clone(),
                recursive: false,
                ignore_if_not_exists: false,
            }],
        };
        assert!(replacement.request(ServerRequestId::String("same".into()), resource));
        let packet = sources.poll_event(&service).unwrap();
        let LspEvent::ApplyEditRequest { edit, .. } = packet.event else {
            panic!("request");
        };
        let choices = sources
            .install_actions(vec![action()], Some(packet.context.clone()), &service, 4)
            .unwrap();
        let key = choices[0].key.clone();
        assert!(sources.take_action(&old_action, &service).is_err());
        assert!(sources.take_action(&key, &service).is_ok());
        assert!(sources.take_action(&key, &service).is_err());
        let copies = EditorState::test_clone_count();
        assert!(
            sources
                .start(
                    edit,
                    PositionEncoding::Utf8,
                    Some(packet.context),
                    packet.reply,
                    &documents(&editor),
                    &service
                )
                .is_err()
        );
        assert_eq!(EditorState::test_clone_count(), copies);
        sources.flush();
        assert_eq!(
            replacement.response(),
            Some((ServerRequestId::String("same".into()), false))
        );
        sources.flush();
        assert_eq!(editor.buffers[buffer].text.to_string(), "éXb");

        // Equal numeric revisions and equal bytes do not substitute a new incarnation.
        editor.replace_buffer_from_disk(buffer, TextBuffer::from_text("éXb"), path.clone());
        let stale = LspWorkspaceEdit {
            documents: vec![LspDocumentEdit {
                uri,
                version: Some(0),
                edits: Vec::new(),
            }],
            resources: Vec::new(),
        };
        assert!(replacement.request(ServerRequestId::Number(3), stale));
        let packet = sources.poll_event(&service).unwrap();
        let LspEvent::ApplyEditRequest { edit, .. } = packet.event else {
            panic!("request");
        };
        let materializations = TextBuffer::test_materializations();
        assert!(
            sources
                .start(
                    edit,
                    PositionEncoding::Utf8,
                    Some(packet.context),
                    packet.reply,
                    &documents(&editor),
                    &service
                )
                .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), materializations);
        drop(replacement);
        assert!(
            !sources.flush().is_empty(),
            "stopped original endpoint must terminate as undeliverable"
        );
        assert!(!sources.pending_replies());
    }
}

impl LspEditSources {
    // Protocol conversion stays with the LSP source, not the canonical store.
    pub(super) fn diagnostic_batch(
        context: &EditContext,
        service: &LspService,
        editor: &crate::editor::EditorState,
        publication: (&str, Option<u64>, Vec<crate::language::lsp::LspDiagnostic>),
        pull: bool,
        maximum: usize,
        label: &str,
    ) -> Result<super::diagnostics::DiagnosticBatch, String> {
        let (uri, version, diagnostics) = publication;
        if editor.window_version().revision().is_none()
            || !service.owns_event_context(context)
            || (!pull && context.diagnostic_ambiguous())
        {
            return Err("diagnostic session or document lineage is unavailable/ambiguous".into());
        }
        let (id, text) = context
            .document_source(uri)
            .ok_or("diagnostic transport has no document source")?;
        let buffer = editor
            .buffers
            .get(*id)
            .ok_or("diagnostic document closed")?;
        if buffer.text.source_version() != *text
            || buffer
                .path
                .as_ref()
                .is_none_or(|path| crate::language::lsp::file_uri(path) != uri)
        {
            return Err("diagnostic document changed since its source context".into());
        }
        if version.is_some_and(|v| v != buffer.text.revision().0) {
            return Err("diagnostic reported version is stale".into());
        }
        let producer = context
            .diagnostic_producer(if pull { "lsp-pull:" } else { "lsp." })
            .map_err(|e| e.to_string())?;
        let codec = PositionCodec::new(&buffer.text);
        let version = if pull {
            Some(buffer.text.revision())
        } else {
            version.map(BufferRevision)
        };
        let items = diagnostics
            .into_iter()
            .take(maximum)
            .map(|diagnostic| {
                let start = codec
                    .lsp_to_byte(
                        diagnostic.range.start.line,
                        diagnostic.range.start.character,
                        context.encoding(),
                    )
                    .map_err(|e| e.to_string())?;
                let end = codec
                    .lsp_to_byte(
                        diagnostic.range.end.line,
                        diagnostic.range.end.character,
                        context.encoding(),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(crate::workspace::Diagnostic {
                    uri: uri.into(),
                    version,
                    range: ByteRange::new(start, end).map_err(|e| e.to_string())?,
                    severity: match diagnostic.severity.unwrap_or(if pull { 1 } else { 4 }) {
                        1 => crate::workspace::DiagnosticSeverity::Error,
                        2 => crate::workspace::DiagnosticSeverity::Warning,
                        3 => crate::workspace::DiagnosticSeverity::Information,
                        _ => crate::workspace::DiagnosticSeverity::Hint,
                    },
                    source: diagnostic.source.unwrap_or_else(|| label.into()),
                    code: diagnostic.code,
                    message: diagnostic.message,
                    related: Vec::new(),
                    tags: Vec::new(),
                    fixes: Vec::new(),
                    timestamp: crate::clock::system_time(),
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(super::diagnostics::DiagnosticBatch {
            producer,
            uri: uri.into(),
            document: super::diagnostics::DiagnosticDocument::capture(editor, *id),
            validity: if pull {
                super::diagnostics::DiagnosticValidity::RequestedSnapshot
            } else if version.is_some() {
                super::diagnostics::DiagnosticValidity::ReportedVersion
            } else {
                super::diagnostics::DiagnosticValidity::UnversionedReport
            },
            items,
        })
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use crate::{
        editor::EditorState,
        editor::session::diagnostics::{DiagnosticSession, DiagnosticValidity},
        language::lsp::{DiagnosticLineage, LspDiagnostic, LspEvent, LspPosition, LspRange},
    };
    #[test]
    fn diagnostic_transport_keeps_encoding_and_quarantines_reused_document_lineage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let uri = crate::language::lsp::file_uri(&path);
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor.open_text(TextBuffer::from_text("éab"), Some(path));
        let mut service = LspService::new();
        let mut endpoint = service.replace_edit_source_fixture(
            uri.clone(),
            (buffer, editor.buffers[buffer].text.source_version()),
            PositionEncoding::Utf8,
        );
        let mut lineage = DiagnosticLineage::default();
        endpoint.observe_diagnostic_document(
            &uri,
            (buffer, editor.buffers[buffer].text.source_version()),
            &mut lineage,
        );
        let raw = || {
            vec![LspDiagnostic {
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 2,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 3,
                    },
                },
                severity: Some(1),
                source: None,
                code: None,
                message: "message".into(),
            }]
        };
        let mut owner = DiagnosticSession::new(10, 10);
        endpoint.publish_diagnostics(&uri, Some(0), raw(), &lineage);
        let event = service.try_owned_event().unwrap();
        let LspEvent::Diagnostics {
            uri,
            version,
            diagnostics,
            ..
        } = event.event
        else {
            panic!("diagnostics");
        };
        let batch = LspEditSources::diagnostic_batch(
            &event.context,
            &service,
            &editor,
            (&uri, version, diagnostics),
            false,
            10,
            "fixture",
        )
        .unwrap();
        assert_eq!(
            batch.items[0].range.start.get(),
            2,
            "must not use ambient UTF-16"
        );
        assert_eq!(batch.validity, DiagnosticValidity::ReportedVersion);
        let first = batch.producer.clone();
        owner.activate(first.clone());
        owner.publish(batch, &editor).unwrap();
        assert!(
            LspEditSources::diagnostic_batch(
                &event.context,
                &service,
                &editor,
                (&uri, Some(1), raw()),
                false,
                10,
                "fixture"
            )
            .is_err()
        );
        let batch = LspEditSources::diagnostic_batch(
            &event.context,
            &service,
            &editor,
            (&uri, None, raw()),
            false,
            10,
            "fixture",
        )
        .unwrap();
        assert_eq!(
            batch.validity,
            DiagnosticValidity::UnversionedReport,
            "not proof of computation against current text"
        );
        let pull = LspEditSources::diagnostic_batch(
            &event.context,
            &service,
            &editor,
            (&uri, None, raw()),
            true,
            10,
            "fixture",
        )
        .unwrap();
        assert_eq!(pull.validity, DiagnosticValidity::RequestedSnapshot);
        let activated = service
            .diagnostic_producer("owned-edit-fixture", "lsp-pull:")
            .unwrap();
        assert!(activated.same(&pull.producer));
        editor.buffers[buffer].text = TextBuffer::from_text("éab");
        endpoint.observe_diagnostic_document(
            &uri,
            (buffer, editor.buffers[buffer].text.source_version()),
            &mut lineage,
        );
        assert!(lineage.ambiguous(&uri));
        endpoint.publish_diagnostics(&uri, Some(0), raw(), &lineage);
        let ambiguous = service.try_owned_event().unwrap();
        let before = TextBuffer::test_materializations();
        assert!(
            LspEditSources::diagnostic_batch(
                &ambiguous.context,
                &service,
                &editor,
                (&uri, Some(0), raw()),
                false,
                10,
                "fixture"
            )
            .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), before);
        let mut fresh = service.replace_edit_source_fixture(
            uri.clone(),
            (buffer, editor.buffers[buffer].text.source_version()),
            PositionEncoding::Utf8,
        );
        let mut new_lineage = DiagnosticLineage::default();
        fresh.observe_diagnostic_document(
            &uri,
            (buffer, editor.buffers[buffer].text.source_version()),
            &mut new_lineage,
        );
        fresh.publish_diagnostics(&uri, Some(0), raw(), &new_lineage);
        let event = service.try_owned_event().unwrap();
        let batch = LspEditSources::diagnostic_batch(
            &event.context,
            &service,
            &editor,
            (&uri, Some(0), raw()),
            false,
            10,
            "fixture",
        )
        .unwrap();
        owner.activate(batch.producer.clone());
        owner.publish(batch, &editor).unwrap();
        owner.clear_source(&first);
        assert_eq!(owner.choices().count(), 1);
        assert!(
            LspEditSources::diagnostic_batch(
                &ambiguous.context,
                &service,
                &editor,
                (&uri, Some(0), raw()),
                false,
                10,
                "fixture"
            )
            .is_err()
        );
    }
}
