//! Workspace preview source capture and all-or-nothing editor draft preparation.

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
        self.editor.same_editor(&state.window_version())
            && state.window_version().revision().is_some()
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
                    || !buffer.text.source_version().same_instance(version)
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
                    *expected == id && buffer.path.as_deref() == Some(path)
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
                .find(|(_, path, _)| path == file.canonical_path);
            let current = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_deref() == Some(file.canonical_path));
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
                .find(|(_, buffer)| buffer.path.as_deref() == Some(file.canonical_path))
                .map(|(id, _)| id);
            let buffer = if let Some(id) = existing {
                id
            } else {
                let text =
                    TextBuffer::from_bytes(file.original_bytes).map_err(|e| e.to_string())?;
                let id = draft.load_document(text, file.canonical_path.to_path_buf())?;
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
                        buffer.path.as_deref() == Some(path)
                            && buffer.text.matches_encoded_text(bytes)
                    })
                    .map(|(id, buffer)| (id, path.to_path_buf(), buffer.text.source_version()))
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
                    buffer.path.as_deref() == Some(path) && buffer.text.source_version() == *text
                })
            })
    }
}

/// Only the editor can associate bytes with an checked open-buffer snapshot.
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
            return Err("workspace buffer exceeds the requested byte limit".into());
        }
        let bytes = buffer.text.to_bytes();
        if bytes.len() > maximum {
            return Err("workspace encoded buffer exceeds the requested byte limit".into());
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
