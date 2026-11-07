//! Exact buffer-path reconciliation after a separately completed filesystem operation.
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::EditorWindowVersion;
use crate::editor::text::TextVersion;
use crate::editor::transaction::EditorDraft;
use std::path::Path;
use std::path::PathBuf;

pub(crate) struct BufferPathChange {
    editor: EditorWindowVersion,
    source: PathBuf,
    buffers: Vec<(BufferId, PathBuf, TextVersion)>,
}
impl BufferPathChange {
    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}
impl EditorState {
    pub(crate) fn capture_path_change(&self, source: &Path) -> Result<BufferPathChange, String> {
        if self.window_version().revision().is_none() {
            return Err("editor source revision is exhausted".into());
        }
        Ok(BufferPathChange {
            editor: self.window_version(),
            source: source.into(),
            buffers: self
                .buffers
                .iter()
                .filter_map(|(id, buffer)| {
                    let path = buffer.path.as_ref()?;
                    path.starts_with(source)
                        .then(|| (id, path.clone(), buffer.text.source_version()))
                })
                .collect(),
        })
    }
    fn validate_path_change(&self, change: &BufferPathChange) -> Result<(), String> {
        let current = self.window_version();
        if current.revision().is_none() || !current.same_editor(&change.editor) {
            return Err("filesystem operation completed, but editor identity changed; paths were not reconciled".into());
        }
        for (id, path, text) in &change.buffers {
            if self.buffers.get(*id).is_none_or(|buffer| {
                buffer.path.as_ref() != Some(path)
                    || !buffer.text.source_version().same_instance(text)
            }) {
                return Err("filesystem operation completed, but an affected buffer was closed, reloaded or renamed; paths were not reconciled".into());
            }
        }
        if self.buffers.iter().any(|(id, buffer)| {
            buffer
                .path
                .as_ref()
                .is_some_and(|path| path.starts_with(&change.source))
                && !change
                    .buffers
                    .iter()
                    .any(|(expected, _, _)| *expected == id)
        }) {
            return Err("filesystem operation completed, but a new buffer owns the affected path; paths were not reconciled".into());
        }
        Ok(())
    }
    pub(crate) fn check_rename_destination(
        &self,
        source: &Path,
        target: &Path,
    ) -> Result<(), String> {
        if self.buffers.values().any(|buffer| {
            buffer
                .path
                .as_ref()
                .is_some_and(|path| path.starts_with(target) && !path.starts_with(source))
        }) {
            return Err("rename destination belongs to an open buffer".into());
        }
        Ok(())
    }

    pub(crate) fn check_resource_destination(&self, target: Option<&Path>) -> Result<(), String> {
        if target.is_some_and(|path| {
            self.buffers
                .iter()
                .any(|(_, buffer)| buffer.path.as_deref() == Some(path))
        }) {
            return Err("resource destination belongs to an open buffer".into());
        }
        Ok(())
    }
    pub(crate) fn validate_resource_change(
        &self,
        change: &BufferPathChange,
        expected: &[u8],
        target: Option<&Path>,
    ) -> Result<(), String> {
        self.validate_path_change(change)?;
        self.check_resource_destination(target)?;
        for (_, buffer) in self.buffers.iter().filter(|(_, b)| {
            b.path
                .as_ref()
                .is_some_and(|path| path.starts_with(&change.source))
        }) {
            if buffer.read_only || buffer.dirty || buffer.text.to_bytes() != expected {
                return Err("resource source buffer is dirty, read-only or changed".into());
            }
        }
        Ok(())
    }
    pub(crate) fn reconcile_paths(
        &self,
        change: BufferPathChange,
        target: Option<&Path>,
    ) -> Result<(Option<EditorDraft>, Vec<PathBuf>), String> {
        self.validate_path_change(&change)?;
        let destinations = change
            .buffers
            .iter()
            .map(|(id, path, _)| {
                (
                    *id,
                    target.map(|target| {
                        let suffix = path
                            .strip_prefix(&change.source)
                            .expect("captured source prefix");
                        if suffix.as_os_str().is_empty() {
                            target.to_path_buf()
                        } else {
                            target.join(suffix)
                        }
                    }),
                )
            })
            .collect::<Vec<_>>();
        for (_, path) in &destinations {
            if let Some(path) = path
                && self.buffers.iter().any(|(id, buffer)| {
                    buffer.path.as_ref() == Some(path)
                        && !destinations.iter().any(|(expected, _)| *expected == id)
                })
            {
                return Err("filesystem operation completed, but destination belongs to another buffer; paths were not reconciled".into());
            }
        }
        if destinations.is_empty() {
            return Ok((None, Vec::new()));
        }
        let recent = destinations
            .iter()
            .filter_map(|(_, path)| path.clone())
            .collect();
        let mut draft = self.draft();
        for (id, path) in destinations {
            if path.is_none() {
                draft.state.buffers[id].set_saved_snapshot(None);
            }
            let dirty = path.is_none() || draft.state.buffers[id].dirty;
            draft.state.set_buffer_path_and_dirty(id, path, dirty);
        }
        Ok((Some(draft), recent))
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::text::TextBuffer;
    use crate::editor::transaction::paths::*;
    #[test]
    fn deleted_files_remain_unsaved_after_edits_are_undone() {
        let mut state = EditorState::new("/workspace".into());
        let path = std::path::PathBuf::from("/workspace/deleted");
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("retained"), Some(path.clone()))
            .unwrap();
        let change = state.capture_path_change(&path).unwrap();
        let (draft, _) = state.reconcile_paths(change, None).unwrap();
        state = draft.unwrap().accept(&state.version()).unwrap();
        state.buffers[buffer].refresh_dirty();
        assert!(state.buffers[buffer].dirty);
        assert_eq!(state.buffers[buffer].text.to_string(), "retained");
    }

    #[test]
    fn path_reconciliation_preserves_edits_and_rejects_replaced_buffer_ownership_before_clone() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        let target = root.path().join("target.txt");
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("before"), Some(source.clone()))
            .unwrap();
        let change = editor.capture_path_change(&source).unwrap();
        editor.buffers[buffer]
            .text
            .restore_text("unsaved edit while renaming")
            .unwrap();
        editor.set_buffer_dirty(buffer, true);
        let (draft, recent) = editor.reconcile_paths(change, Some(&target)).unwrap();
        editor = draft.unwrap().accept(&editor.version()).unwrap();
        assert_eq!(recent.as_slice(), std::slice::from_ref(&target));
        assert_eq!(
            editor.buffers[buffer].path.as_ref().unwrap().as_os_str(),
            target.as_os_str()
        );
        assert_eq!(
            editor.buffers[buffer].text.to_string(),
            "unsaved edit while renaming"
        );
        assert!(editor.buffers[buffer].dirty);
        let change = editor.capture_path_change(&target).unwrap();
        editor.replace_buffer_from_disk(
            buffer,
            TextBuffer::from_text("reloaded instance"),
            target.clone(),
        );
        let before = EditorState::test_clone_count();
        assert!(editor.reconcile_paths(change, None).is_err());
        assert_eq!(EditorState::test_clone_count(), before);
        let change = editor.capture_path_change(&target).unwrap();
        editor.set_buffer_path_and_dirty(buffer, Some(source.clone()), true);
        assert!(editor.reconcile_paths(change, None).is_err());
        let change = editor.capture_path_change(&source).unwrap();
        let other = EditorState::new(root.path().into());
        assert!(other.reconcile_paths(change, None).is_err());
        let change = editor.capture_path_change(&source).unwrap();
        editor.close_active_view().unwrap();
        editor.remove_buffer(buffer).unwrap();
        let (new, _) = editor
            .open_text(TextBuffer::from_text("new owner"), Some(source.clone()))
            .unwrap();
        assert!(editor.reconcile_paths(change, None).is_err());
        assert_eq!(editor.buffers[new].path.as_ref(), Some(&source));
        let change = editor.capture_path_change(&source).unwrap();
        let (draft, _) = editor.reconcile_paths(change, None).unwrap();
        let editor = draft.unwrap().accept(&editor.version()).unwrap();
        assert!(editor.buffers[new].path.is_none());
        assert!(editor.buffers[new].dirty);
        assert_eq!(editor.buffers[new].text.to_string(), "new owner");
    }
}
