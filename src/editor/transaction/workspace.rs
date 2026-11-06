
pub(crate) struct DraftEdit<'a> {
    pub(crate) canonical_path: &'a std::path::Path,
    pub(crate) base_revision: Option<crate::editor::text::BufferRevision>,
    pub(crate) original_bytes: &'a [u8],
    pub(crate) edits: &'a [crate::editor::text::TextEdit],
    pub(crate) preferred_line_ending: crate::editor::text::LineEnding,
}
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub canonical_path: PathBuf,
    pub revision: Option<crate::editor::text::BufferRevision>,
    pub bytes: Vec<u8>,
}
//! Workspace preview source capture and all-or-nothing editor draft preparation.
use std::path::PathBuf;

use crate::editor::state::BufferId;

use crate::editor::state::EditorState;

use crate::editor::state::EditorWindowVersion;

use crate::editor::state::TransactionId;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEditSet;

use crate::editor::text::TextVersion;



use crate::editor::transaction::EditorDraft;

use crate::editor::transaction::EditorOperation;

use crate::editor::transaction::EditorTransaction;

use crate::editor::transaction::Precondition;

#[derive(Clone)]
pub(crate) struct WorkspaceEditSources {
    editor: EditorWindowVersion,
    buffers: Vec<(BufferId, PathBuf, TextVersion)>,
}
impl WorkspaceEditSources {
    pub(crate) fn same_editor(&self, state: &EditorState) -> bool {
        self.editor.same_editor(&state.window_version()) && state.window_version().revision().is_some()
    }
}
impl EditorState {
    pub(crate) fn validate_workspace_sources<'a>(
        &self,
        sources: &WorkspaceEditSources,
        paths: impl Iterator<Item = &'a std::path::Path>,
    ) -> Result<(), String> {
        if !sources.editor.same_editor(&self.window_version())
            || self.window_version().revision().is_none()
        {
            return Err("workspace edit editor source is foreign or exhausted".into());
        }
        for path in paths {
            let before = sources
                .buffers
                .iter()
                .find(|(_, expected, _)| expected == path);
            let current = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_deref() == Some(path));
            match (before, current) {
                (Some((expected, _, version)), Some((id, buffer)))
                    if *expected == id && buffer.text.source_version() == *version => {}
                (None, None) => {}
                _ => {
                    return Err(format!(
                        "{} changed ownership or content during workspace preparation",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn workspace_resource_change(
        &self,
        sources: &WorkspaceEditSources,
        source: &std::path::Path,
    ) -> Result<crate::editor::transaction::BufferPathChange, String> {
        if !sources.editor.same_editor(&self.window_version()) {
            return Err("workspace resource belongs to another editor".into());
        }
        for (id, path, version) in sources
            .buffers
            .iter()
            .filter(|(_, path, _)| path.starts_with(source))
        {
            if self.buffers.get(*id).is_none_or(|buffer| {
                buffer.path.as_ref() != Some(path)
                    || !buffer.text.source_version().same_incarnation(version)
            }) {
                return Err("workspace resource buffer ownership changed after preview".into());
            }
        }
        if self.buffers.iter().any(|(id, buffer)| {
            buffer
                .path
                .as_ref()
                .is_some_and(|path| path.starts_with(source))
                && !sources.buffers.iter().any(|(expected, path, _)| {
                    *expected == id && buffer.path.as_ref() == Some(path)
                })
        }) {
            return Err("a new buffer owns the workspace resource path".into());
        }
        self.capture_path_change(source)
    }

    pub(crate) fn workspace_edit_source(
        &self,
        id: BufferId,
    ) -> Result<WorkspaceEditSources, String> {
        if self.window_version().revision().is_none() {
            return Err("editor source revision is exhausted".into());
        }
        let buffer = self
            .buffers
            .get(id)
            .ok_or("workspace source buffer closed")?;
        let path = buffer
            .path
            .clone()
            .ok_or("workspace source buffer has no path")?;
        Ok(WorkspaceEditSources {
            editor: self.window_version(),
            buffers: vec![(id, path, buffer.text.source_version())],
        })
    }
    pub(crate) fn workspace_edit_sources(&self) -> Result<WorkspaceEditSources, String> {
        if self.window_version().revision().is_none() {
            return Err("editor source revision is exhausted".into());
        }
        Ok(WorkspaceEditSources {
            editor: self.window_version(),
            buffers: self
                .buffers
                .iter()
                .filter_map(|(id, buffer)| {
                    Some((id, buffer.path.clone()?, buffer.text.source_version()))
                })
                .collect(),
        })
    }
    pub(crate) fn prepare_document_edits(
        &self,
        sources: &WorkspaceEditSources,
        label: &str,
        files: &[DraftEdit<'_>],
        transaction: TransactionId,
    ) -> Result<EditorDraft, String> {
        if !sources.editor.same_editor(&self.window_version())
            || self.window_version().revision().is_none()
        {
            return Err("workspace edit belongs to a foreign or exhausted editor".into());
        }
        // Validate every known buffer before decoding snapshots or cloning the editor.
        for file in files.iter() {
            let before = sources
                .buffers
                .iter()
                .find(|(_, path, _)| path == &file.canonical_path);
            let current = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(file.canonical_path));
            match (before, current) {
                (Some((expected, _, version)), Some((id, buffer)))
                    if *expected == id
                        && buffer.text.source_version() == *version
                        && !buffer.read_only
                        && file.base_revision == Some(buffer.text.revision()) => {}
                (None, None) if file.base_revision.is_none() => {}
                _ => {
                    return Err(format!(
                        "{} changed ownership, revision or write policy after preview",
                        file.canonical_path.display()
                    ));
                }
            }
        }
        let mut draft = self.draft();
        let mut edits = EditorTransaction::new(transaction);
        edits.label = Some(label.to_owned());
        let mut created = Vec::new();
        let mut preferences = Vec::new();
        for file in files.iter() {
            let existing = draft
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(file.canonical_path))
                .map(|(id, _)| id);
            let buffer = if let Some(id) = existing {
                id
            } else {
                let text =
                    TextBuffer::from_bytes(file.original_bytes).map_err(|e| e.to_string())?;
                let id = draft.load_document(text, file.canonical_path.to_path_buf());
                created.push(id);
                id
            };
            let text = &mut draft.state.buffers[buffer].text;
            preferences.push((buffer, text.format().preferred_line_ending));
            // Preview construction decodes the snapshot, whose preferred separator may
            // differ from a live buffer's after earlier edits removed some newlines.
            text.set_preferred_line_ending(file.preferred_line_ending);
            let revision = text.revision();
            edits
                .preconditions
                .push(Precondition::BufferRevision { buffer, revision });
            edits
                .preconditions
                .push(Precondition::BufferWritable(buffer));
            edits.operations.push(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(revision, file.edits.to_vec())
                    .with_normalized_line_endings(),
            });
        }
        draft = draft.apply_operations(&edits).map_err(|e| e.to_string())?;
        for (buffer, ending) in preferences {
            draft.state.buffers[buffer]
                .text
                .set_preferred_line_ending(ending);
        }
        for buffer in created {
            draft.state.set_buffer_dirty(buffer, true);
        }
        Ok(draft)
    }
}

/// Borrowed accepted editor projection; EditorDraft does not expose this view.
pub(crate) struct EditorPublication<'a>(&'a EditorState);
pub(crate) struct WorkspaceCommitCheck {
    editor: EditorWindowVersion,
    targets: Vec<(BufferId, PathBuf, TextVersion)>,
}
impl EditorState {
    pub(crate) fn publication(&self) -> EditorPublication<'_> {
        EditorPublication(self)
    }
}
impl EditorDraft {
    pub(crate) fn check_document_edits<'a>(
        &self,
        files: impl Iterator<Item = (&'a std::path::Path, &'a [u8])>,
    ) -> Result<WorkspaceCommitCheck, String> {
        let targets = files
            .map(|(path, bytes)| {
                self.state
                    .buffers
                    .iter()
                    .find(|(_, buffer)| {
                        buffer.path.as_ref() == Some(path)
                            && buffer.text.matches_encoded_text(bytes)
                    })
                    .map(|(id, buffer)| {
                        (
                            id,
                            path.to_path_buf(),
                            buffer.text.source_version(),
                        )
                    })
                    .ok_or_else(|| {
                        "staged workspace target does not match the selected preview".into()
                    })
            })
            .collect::<Result<_, String>>()?;
        Ok(WorkspaceCommitCheck {
            editor: self.state.window_version(),
            targets,
        })
    }
}
impl WorkspaceCommitCheck {
    pub(crate) fn matches(&self, publication: &EditorPublication<'_>) -> bool {
        let editor = publication.0;
        self.editor.same_editor(&editor.window_version())
            && self.targets.iter().all(|(id, path, text)| {
                editor.buffers.get(*id).is_some_and(|buffer| {
                    buffer.path.as_ref() == Some(path) && buffer.text.source_version() == *text
                })
            })
    }
}

/// Only the editor can associate bytes with an admitted open-buffer snapshot.
pub(crate) struct WorkspaceSnapshot {
    editor: EditorWindowVersion,
    buffer: BufferId,
    source: TextVersion,
    file: DocumentSnapshot,
}
impl EditorState {
    pub(crate) fn workspace_snapshot(
        &self,
        sources: &WorkspaceEditSources,
        path: &std::path::Path,
        maximum: usize,
    ) -> Result<Option<WorkspaceSnapshot>, String> {
        self.validate_workspace_sources(sources, std::iter::once(path))?;
        let Some((id, buffer)) = self
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_deref() == Some(path))
        else {
            return Ok(None);
        };
        if buffer.text.len_bytes() > maximum {
            return Err("workspace buffer exceeds the admitted byte limit".into());
        }
        let bytes = buffer.text.to_bytes();
        if bytes.len() > maximum {
            return Err("workspace encoded buffer exceeds the admitted byte limit".into());
        }
        Ok(Some(WorkspaceSnapshot {
            editor: self.window_version(),
            buffer: id,
            source: buffer.text.source_version(),
            file: DocumentSnapshot {
                canonical_path: path.into(),
                revision: Some(buffer.text.revision()),
                bytes,
            },
        }))
    }
}
impl WorkspaceSnapshot {
    pub(crate) fn into_file(
        self,
        sources: &WorkspaceEditSources,
        path: &std::path::Path,
    ) -> Result<DocumentSnapshot, String> {
        if !sources.editor.same_editor(&self.editor)
            || self.file.canonical_path != path
            || !sources.buffers.iter().any(|(id, expected, version)| {
                *id == self.buffer && expected == path && version == &self.source
            })
        {
            return Err(
                "workspace snapshot belongs to a different editor, buffer or preparation".into(),
            );
        }
        Ok(self.file)
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::text::diff::changed_spans;
    use crate::editor::state::EditorState;
    use crate::editor::state::TransactionId;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::editor::text::TextEdit;
    use crate::editor::text::TextEditSet;
    use crate::editor::transaction::EditorOperation;
    use crate::editor::transaction::EditorTransaction;
    use DocumentSnapshot;
    use crate::workspace::ProposalId;
    use crate::workspace::WorkspaceEditDraft;
    
    use crate::workspace::WorkspaceId;
    use crate::workspace::WorkspaceState;

    #[test]
    fn workspace_edits_preserve_preview_bytes_and_undo_mixed_endings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mixed.txt");
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [(
                "root".into(),
                crate::files::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let original = b"\xef\xbb\xbfa\r\nb\n";
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor
            .open_text(
                TextBuffer::from_bytes(original).unwrap(),
                Some(path.clone()),
            )
            .unwrap();
        let revision = editor.buffers[buffer].text.revision();
        let proposal = WorkspaceEditProposal::build(
            ProposalId(1),
            "replace".into(),
            &scope,
            vec![WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(revision),
                edits: vec![TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
                    "A\nb\n",
                )],
            }],
            vec![FileSnapshot {
                canonical_path: path,
                revision: Some(revision),
                bytes: original.to_vec(),
            }],
        )
        .unwrap();
        let sources = editor.workspace_edit_sources().unwrap();
        let draft = crate::app::workspace::edits::prepare_workspace_edit(&editor, &sources, &proposal, TransactionId(1))
            .unwrap();
        crate::app::workspace::edits::workspace_commit_check(&draft, &proposal).unwrap();
        editor = draft.accept(&editor.version()).unwrap();
        assert_eq!(
            editor.buffers[buffer].text.to_bytes(),
            proposal.files[0].proposed_bytes
        );
        assert_eq!(
            editor.buffers[buffer].text.to_bytes(),
            b"\xef\xbb\xbfA\nb\n"
        );
        editor
            .apply_transaction(
                &EditorTransaction::new(TransactionId(2))
                    .with_operation(EditorOperation::Undo { buffer }),
            )
            .unwrap();
        assert_eq!(editor.buffers[buffer].text.to_bytes(), original);

        // A draft with the same normalized text but different separators is not the preview.
        let mut wrong = editor.draft();
        wrong.state.buffers[buffer].text = TextBuffer::from_bytes(b"\xef\xbb\xbfA\r\nb\n").unwrap();
        assert!(crate::app::workspace::edits::workspace_commit_check(&wrong, &proposal).is_err());
    }

    #[test]
    fn workspace_edits_use_preview_separators_and_undo_separator_only_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("endings.txt");
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [(
                "root".into(),
                crate::files::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        for separator_only in [false, true] {
            let mut editor = EditorState::new(root.path().into());
            let original: &[u8] = if separator_only {
                b"a\r\nb\n"
            } else {
                b"a\r\nb\r\nc\n"
            };
            let (buffer, _) = editor
                .open_text(
                    TextBuffer::from_bytes(original).unwrap(),
                    Some(path.clone()),
                )
                .unwrap();
            if !separator_only {
                editor
                    .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(
                        EditorOperation::ApplyEdits {
                            buffer,
                            edits: TextEditSet::new(
                                editor.buffers[buffer].text.revision(),
                                vec![TextEdit::new(
                                    ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
                                    "",
                                )],
                            ),
                        },
                    ))
                    .unwrap();
            }
            let before = editor.buffers[buffer].text.to_bytes();
            let preference = editor.buffers[buffer].text.format().preferred_line_ending;
            let revision = editor.buffers[buffer].text.revision();
            let replacement = if separator_only {
                "a\nb\n"
            } else {
                "C\nmore\n"
            };
            let proposal = WorkspaceEditProposal::build(
                ProposalId(1),
                "replace".into(),
                &scope,
                vec![WorkspaceEditDraft {
                    canonical_path: path.clone(),
                    expected_revision: Some(revision),
                    edits: vec![TextEdit::new(
                        ByteRange::new(
                            ByteOffset(0),
                            ByteOffset(editor.buffers[buffer].text.len_bytes()),
                        )
                        .unwrap(),
                        replacement,
                    )],
                }],
                vec![FileSnapshot {
                    canonical_path: path.clone(),
                    revision: Some(revision),
                    bytes: before.clone(),
                }],
            )
            .unwrap();
            let draft = crate::app::workspace::edits::prepare_workspace_edit(&editor, 
                    &editor.workspace_edit_sources().unwrap(),
                    &proposal,
                    TransactionId(2),
                )
                .unwrap();
            crate::app::workspace::edits::workspace_commit_check(&draft, &proposal).unwrap();
            editor = draft.accept(&editor.version()).unwrap();
            assert_eq!(
                editor.buffers[buffer].text.to_bytes(),
                proposal.files[0].proposed_bytes
            );
            assert_eq!(
                editor.buffers[buffer].text.format().preferred_line_ending,
                preference
            );
            editor
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(3))
                        .with_operation(EditorOperation::Undo { buffer }),
                )
                .unwrap();
            assert_eq!(editor.buffers[buffer].text.to_bytes(), before);
            editor
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(4))
                        .with_operation(EditorOperation::Redo { buffer }),
                )
                .unwrap();
            assert_eq!(
                editor.buffers[buffer].text.to_bytes(),
                proposal.files[0].proposed_bytes
            );
        }
    }

    #[test]
    fn workspace_edits_keep_cursors_between_disjoint_replacements() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("words.txt");
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [(
                "root".into(),
                crate::files::RootIdentity::capture(root.path()).unwrap(),
            )],
        )
        .unwrap();
        let mut editor = EditorState::new(root.path().into());
        let (buffer, view) = editor
            .open_text(TextBuffer::from_text("x keep x\n"), Some(path.clone()))
            .unwrap();
        let sibling = editor
            .split_active(crate::editor::Axis::Horizontal, 500)
            .unwrap();
        editor
            .apply_transaction(
                &EditorTransaction::new(TransactionId(1))
                    .with_operation(EditorOperation::SetCursor {
                        view,
                        position: ByteOffset(3),
                    })
                    .with_operation(EditorOperation::SetCursor {
                        view: sibling,
                        position: ByteOffset(5),
                    }),
            )
            .unwrap();
        let revision = editor.buffers[buffer].text.revision();
        let proposal = WorkspaceEditProposal::build(
            ProposalId(1),
            "replace".into(),
            &scope,
            vec![WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(revision),
                edits: vec![
                    TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                        "name",
                    ),
                    TextEdit::new(
                        ByteRange::new(ByteOffset(7), ByteOffset(8)).unwrap(),
                        "name",
                    ),
                ],
            }],
            vec![FileSnapshot {
                canonical_path: path,
                revision: Some(revision),
                bytes: b"x keep x\n".to_vec(),
            }],
        )
        .unwrap();
        let draft = crate::app::workspace::edits::prepare_workspace_edit(&editor, 
                &editor.workspace_edit_sources().unwrap(),
                &proposal,
                TransactionId(2),
            )
            .unwrap();
        editor = draft.accept(&editor.version()).unwrap();
        assert_eq!(editor.buffers[buffer].text.to_string(), "name keep name\n");
        assert_eq!(editor.views[view].cursor, ByteOffset(6));
        assert_eq!(editor.views[sibling].cursor, ByteOffset(8));
    }

    #[test]
    fn only_what_differs_is_replaced() {
        let before = "let old = 1;\nkeep\nuse(old);\n";
        let after = "let name = 1;\nkeep\nuse(name);\n";
        assert_eq!(
            changed_spans(before, after),
            vec![(4..7, "name".to_owned()), (22..25, "name".to_owned())]
        );
        assert_eq!(
            changed_spans("a\nb\n", "new\na\nb\n"),
            vec![(0..0, "new\n".to_owned())]
        );
        assert_eq!(
            changed_spans("a\nb", "a\nb\nc\n"),
            vec![(3..3, "\nc\n".to_owned())]
        );
        assert_eq!(changed_spans("é\n", "è\n"), vec![(0..2, "è".to_owned())]);
    }

    #[test]
    fn identical_text_needs_no_edit() {
        assert!(changed_spans("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn text_a_line_diff_cannot_describe_is_replaced_whole() {
        assert_eq!(
            changed_spans("a\0b\n", "a\0c\n"),
            vec![(0..4, "a\0c\n".to_owned())]
        );
    }
}
