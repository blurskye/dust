//! Workspace preview source capture and all-or-nothing editor draft preparation.
use std::path::PathBuf;

use crate::{
    editor::{
        state::{BufferId, EditorState, EditorWindowVersion, TransactionId},
        text::{ByteOffset, TextBuffer, TextEditSet, TextVersion},
    },
    workspace::WorkspaceEditProposal,
};

use super::{EditorDraft, EditorOperation, EditorTransaction, Precondition};

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
    ) -> Result<super::BufferPathChange, String> {
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
            let revision = draft.state.buffers[buffer].text.revision();
            let replacement = TextBuffer::from_bytes(&file.proposed_bytes)
                .map_err(|e| e.to_string())?
                .to_string();
            let spans = changed_spans(&draft.state.buffers[buffer].text.to_string(), &replacement);
            edits
                .preconditions
                .push(Precondition::BufferRevision { buffer, revision });
            edits
                .preconditions
                .push(Precondition::BufferWritable(buffer));
            edits.operations.push(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    revision,
                    spans
                        .into_iter()
                        .map(|(range, text)| {
                            crate::editor::text::ByteRange::new(
                                ByteOffset(range.start),
                                ByteOffset(range.end),
                            )
                            .map(|range| crate::editor::text::TextEdit::new(range, text))
                            .map_err(|e| e.to_string())
                        })
                        .collect::<Result<_, _>>()?,
                ),
            });
        }
        draft = draft.apply_operations(&edits).map_err(|e| e.to_string())?;
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
        let mut prefix = old.bytes().zip(added.bytes()).take_while(|(a, b)| a == b).count();
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
    use super::changed_spans;

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
