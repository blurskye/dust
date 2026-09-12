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
                    vec![crate::editor::text::TextEdit::new(
                        crate::editor::text::ByteRange::new(
                            ByteOffset(0),
                            ByteOffset(draft.state.buffers[buffer].text.len_bytes()),
                        )
                        .map_err(|e| e.to_string())?,
                        replacement,
                    )],
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
