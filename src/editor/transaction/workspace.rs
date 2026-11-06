//! Workspace preview source capture and all-or-nothing editor draft preparation.
use std::path::PathBuf;

use crate::editor::state::BufferId;

use crate::editor::state::EditorState;

use crate::editor::state::EditorWindowVersion;

use crate::editor::state::TransactionId;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEditSet;

use crate::editor::text::TextVersion;

use crate::workspace::WorkspaceEditProposal;

use crate::editor::transaction::EditorDraft;

use crate::editor::transaction::EditorOperation;

use crate::editor::transaction::EditorTransaction;

use crate::editor::transaction::Precondition;

#[derive(Clone)]
pub(crate) struct WorkspaceEditSources {
    editor: EditorWindowVersion,
    buffers: Vec<(BufferId, PathBuf, TextVersion)>,
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
    pub(crate) fn prepare_workspace_edit(
        &self,
        sources: &WorkspaceEditSources,
        proposal: &WorkspaceEditProposal,
        transaction: TransactionId,
    ) -> Result<EditorDraft, String> {
        if !sources.editor.same_editor(&self.window_version())
            || self.window_version().revision().is_none()
        {
            return Err("workspace edit belongs to a foreign or exhausted editor".into());
        }
        if proposal.state != crate::workspace::ProposalState::Preview {
            return Err("workspace preview is closed".into());
        }
        // Validate every known buffer before decoding snapshots or cloning the editor.
        for file in proposal.files.iter().filter(|file| file.selected) {
            let before = sources
                .buffers
                .iter()
                .find(|(_, path, _)| path == &file.canonical_path);
            let current = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&file.canonical_path));
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
        edits.label = Some(proposal.label.clone());
        let mut created = Vec::new();
        let mut preferences = Vec::new();
        for file in proposal.files.iter().filter(|file| file.selected) {
            let existing = draft
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&file.canonical_path))
                .map(|(id, _)| id);
            let buffer = if let Some(id) = existing {
                id
            } else {
                let text =
                    TextBuffer::from_bytes(&file.original_bytes).map_err(|e| e.to_string())?;
                let id = draft.load_document(text, file.canonical_path.clone());
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
                edits: TextEditSet::new(revision, file.edits.clone())
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

/// Only what differs between the texts, so cursors and marks elsewhere stay where they are.
/// Replaces the whole text when a line diff can't describe the change, as with binary content.
pub(crate) fn changed_spans(before: &str, after: &str) -> Vec<(std::ops::Range<usize>, String)> {
    if before == after {
        return Vec::new();
    }
    let spans = line_diff_spans(before, after).filter(|spans| {
        let mut rebuilt = String::with_capacity(after.len());
        let mut position = 0;
        for (range, text) in spans {
            rebuilt.push_str(&before[position..range.start]);
            rebuilt.push_str(text);
            position = range.end;
        }
        rebuilt.push_str(&before[position..]);
        !spans.is_empty() && rebuilt == after
    });
    spans.unwrap_or_else(|| vec![(0..before.len(), after.to_owned())])
}

fn line_diff_spans(before: &str, after: &str) -> Option<Vec<(std::ops::Range<usize>, String)>> {
    let mut options = git2::DiffOptions::new();
    options.context_lines(0);
    let patch = git2::Patch::from_buffers(
        before.as_bytes(),
        None,
        after.as_bytes(),
        None,
        Some(&mut options),
    )
    .ok()?;
    let mut line_starts = vec![0];
    line_starts.extend(before.match_indices('\n').map(|(index, _)| index + 1));
    let line_start = |line: usize| line_starts.get(line).copied().unwrap_or(before.len());
    let mut spans = Vec::new();
    for index in 0..patch.num_hunks() {
        let (hunk, lines) = patch.hunk(index).ok()?;
        let removed = usize::try_from(hunk.old_lines()).ok()?;
        // With nothing removed, the hunk's old line is the one it inserts after.
        let first = usize::try_from(hunk.old_start())
            .ok()?
            .saturating_sub(usize::from(removed > 0));
        let mut added = Vec::new();
        for line in 0..lines {
            let line = patch.line_in_hunk(index, line).ok()?;
            if line.origin() == '+' {
                added.extend_from_slice(line.content());
            }
        }
        let added = String::from_utf8(added).ok()?;
        let (start, end) = (line_start(first), line_start(first.saturating_add(removed)));
        let old = before.get(start..end)?;
        let mut prefix = old
            .bytes()
            .zip(added.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(prefix) || !added.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = old[prefix..]
            .bytes()
            .rev()
            .zip(added[prefix..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - suffix)
            || !added.is_char_boundary(added.len() - suffix)
        {
            suffix -= 1;
        }
        spans.push((
            start + prefix..end - suffix,
            added[prefix..added.len() - suffix].to_owned(),
        ));
    }
    Some(spans)
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
    pub(crate) fn workspace_commit_check(
        &self,
        proposal: &WorkspaceEditProposal,
    ) -> Result<WorkspaceCommitCheck, String> {
        let targets = proposal
            .files
            .iter()
            .filter(|file| file.selected)
            .map(|file| {
                self.state
                    .buffers
                    .iter()
                    .find(|(_, buffer)| {
                        buffer.path.as_ref() == Some(&file.canonical_path)
                            && buffer.text.matches_encoded_text(&file.proposed_bytes)
                    })
                    .map(|(id, buffer)| {
                        (
                            id,
                            file.canonical_path.clone(),
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
    file: crate::workspace::FileSnapshot,
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
            file: crate::workspace::FileSnapshot {
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
    ) -> Result<crate::workspace::FileSnapshot, String> {
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
    use crate::editor::transaction::workspace::changed_spans;
    use crate::editor::state::EditorState;
    use crate::editor::state::TransactionId;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::editor::text::TextEdit;
    use crate::editor::text::TextEditSet;
    use crate::editor::transaction::EditorOperation;
    use crate::editor::transaction::EditorTransaction;
    use crate::workspace::FileSnapshot;
    use crate::workspace::ProposalId;
    use crate::workspace::WorkspaceEditDraft;
    use crate::workspace::WorkspaceEditProposal;
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
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
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
        let draft = editor
            .prepare_workspace_edit(&sources, &proposal, TransactionId(1))
            .unwrap();
        draft.workspace_commit_check(&proposal).unwrap();
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
        assert!(wrong.workspace_commit_check(&proposal).is_err());
    }

    #[test]
    fn workspace_edits_use_preview_separators_and_undo_separator_only_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("endings.txt");
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [(
                "root".into(),
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
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
            let draft = editor
                .prepare_workspace_edit(
                    &editor.workspace_edit_sources().unwrap(),
                    &proposal,
                    TransactionId(2),
                )
                .unwrap();
            draft.workspace_commit_check(&proposal).unwrap();
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
                crate::workspace::RootIdentity::capture(root.path()).unwrap(),
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
        let draft = editor
            .prepare_workspace_edit(
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
