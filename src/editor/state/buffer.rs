//! Buffer text, metadata and revision identity.

use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::FileFormat;
use crate::editor::text::TextBuffer;
use crate::editor::transaction::UndoTree;
use std::path::PathBuf;

use crate::editor::state::view::View;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BufferOrigin {
    FileOrScratch,
    VirtualReadOnly {
        owner: String,
        load_id: u64,
        document: u64,
        revision: u64,
    },
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub text: TextBuffer,
    pub(crate) decorations: crate::editor::decorations::Decorations,
    changes: std::collections::VecDeque<std::sync::Arc<crate::editor::BufferChange>>,
    pub path: Option<PathBuf>,
    pub read_only: bool,
    pub origin: BufferOrigin,
    pub(crate) virtual_title: Option<String>,
    pub(crate) virtual_language: Option<String>,
    pub dirty: bool,
    pub saved_revision: BufferRevision,
    saved_text: Option<std::sync::Arc<str>>,
    saved_format: FileFormat,
    pub(crate) saved_view: Option<View>,
    persisted_bytes: std::sync::Arc<[u8]>,
    pub(crate) new_file: bool,
    pub undo: UndoTree,
    /// Tree-sitter text objects from the parse of `revision`, set by the syntax session.
    pub syntax_objects: Option<(
        BufferRevision,
        std::sync::Arc<Vec<crate::editor::SyntaxObject>>,
    )>,
}

impl Buffer {
    /// The owner of a virtual document, shown where a file would show its name.
    #[must_use]
    pub fn virtual_owner(&self) -> Option<&str> {
        match &self.origin {
            BufferOrigin::VirtualReadOnly { owner, .. } => Some(owner),
            BufferOrigin::FileOrScratch => None,
        }
    }

    /// The title of a virtual document, falling back to its owner.
    #[must_use]
    pub fn virtual_title(&self) -> Option<&str> {
        self.virtual_title
            .as_deref()
            .or_else(|| self.virtual_owner())
    }

    /// The tree-sitter text objects, when the last parse matches the current text.
    #[must_use]
    pub fn current_syntax_objects(&self) -> &[crate::editor::SyntaxObject] {
        self.syntax_objects
            .as_ref()
            .filter(|(revision, _)| *revision == self.text.revision())
            .map_or(&[], |(_, objects)| objects.as_slice())
    }

    #[must_use]
    pub fn new(text: TextBuffer, path: Option<PathBuf>) -> Self {
        let saved_text = Some(text.to_string().into());
        let saved_revision = text.revision();
        let persisted_bytes = text.to_bytes().into();
        Self {
            undo: UndoTree::default(),
            decorations: crate::editor::decorations::Decorations::default(),
            changes: std::collections::VecDeque::new(),
            saved_format: text.format().clone(),
            text,
            path,
            read_only: false,
            origin: BufferOrigin::FileOrScratch,
            virtual_title: None,
            virtual_language: None,
            dirty: false,
            saved_revision,
            saved_text,
            saved_view: None,
            persisted_bytes,
            new_file: false,
            syntax_objects: None,
        }
    }

    #[must_use]
    pub fn is_virtual_read_only(&self) -> bool {
        matches!(self.origin, BufferOrigin::VirtualReadOnly { .. })
    }

    pub(crate) fn record_change(
        &mut self,
        edits: crate::editor::text::TextEditSet,
        delta: crate::editor::text::EditDelta,
    ) -> std::sync::Arc<crate::editor::BufferChange> {
        let change = std::sync::Arc::new(crate::editor::BufferChange {
            before: edits.base_revision(),
            after: self.text.revision(),
            edits,
            delta,
        });
        self.changes.push_back(change.clone());
        if self.changes.len() > 64 {
            self.changes.pop_front();
        }
        change
    }

    /// Changes are contiguous within this buffer instance; a gap needs a snapshot.
    pub fn changes_since(
        &self,
        revision: BufferRevision,
    ) -> Option<Vec<std::sync::Arc<crate::editor::BufferChange>>> {
        if revision == self.text.revision() {
            return Some(Vec::new());
        }
        let start = self
            .changes
            .iter()
            .position(|change| change.before == revision)?;
        Some(self.changes.iter().skip(start).cloned().collect())
    }

    pub fn refresh_dirty(&mut self) {
        self.dirty = self
            .saved_text
            .as_ref()
            .is_none_or(|saved| !self.text.text_equals(saved))
            || self.text.format().utf8_bom != self.saved_format.utf8_bom
            || self.text.format().line_endings != self.saved_format.line_endings;
    }

    pub(crate) fn restored_view(&self, buffer: crate::editor::state::BufferId) -> View {
        let mut view = self.saved_view.clone().unwrap_or_else(|| View::new(buffer));
        view.cursor = view.cursor.min(ByteOffset(self.text.len_bytes()));
        while !self.text.is_char_boundary(view.cursor) {
            view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
        }
        view.scroll_line = view
            .scroll_line
            .min(self.text.len_lines().saturating_sub(1));
        view
    }

    pub(crate) fn set_saved_snapshot(
        &mut self,
        snapshot: Option<(std::sync::Arc<str>, FileFormat)>,
    ) {
        self.saved_text = snapshot.map(|(text, format)| {
            self.saved_format = format;
            text
        });
        self.refresh_dirty();
    }

    pub fn mark_saved(&mut self, revision: BufferRevision) {
        if revision == self.text.revision() {
            self.mark_saved_as(revision, self.text.to_bytes(), self.path.clone());
        }
    }

    pub fn mark_saved_as(
        &mut self,
        revision: BufferRevision,
        bytes: Vec<u8>,
        path: Option<PathBuf>,
    ) {
        self.path = path;
        self.saved_revision = revision;
        if let Ok(saved) = TextBuffer::from_bytes(&bytes) {
            self.saved_text = Some(saved.to_string().into());
            self.saved_format = saved.format().clone();
        }
        self.persisted_bytes = bytes.into();
        self.new_file = false;
        self.refresh_dirty();
    }

    /// Pick up the history saved with this file, when the file still holds the saved text.
    pub fn restore_undo_history(&mut self, directory: &std::path::Path, maximum_bytes: usize) {
        if let Some(path) = &self.path
            && let Some(tree) = crate::editor::transaction::load_undo_history(
                directory,
                path,
                &self.text,
                maximum_bytes,
            )
        {
            self.undo = tree;
        }
    }

    #[must_use]
    pub fn persisted_bytes(&self) -> &[u8] {
        &self.persisted_bytes
    }
}
