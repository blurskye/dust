use std::{collections::HashMap, path::PathBuf};

use crate::editor::{
    command::CommandInvocation,
    text::{BufferRevision, ByteOffset, FileFormat, TextBuffer},
    transaction::UndoTree,
};

use super::id::{BufferId, PaneId, ViewId};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum EditorMode {
    #[default]
    Normal,
    Insert,
    Replace,
    VisualChar,
    VisualLine,
    VisualBlock,
    /// Several cursors marked, waiting for what to do at them.
    Multi,
    OperatorPending,
    CommandLine,
    Prompt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionKind {
    Character,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: ByteOffset,
    pub head: ByteOffset,
    pub kind: SelectionKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BufferOrigin {
    FileOrScratch,
    VirtualReadOnly {
        owner: String,
        generation: u64,
        document: u64,
        revision: u64,
    },
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub text: TextBuffer,
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
        std::sync::Arc<Vec<crate::language::syntax::SyntaxObject>>,
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
        self.virtual_title.as_deref().or_else(|| self.virtual_owner())
    }

    /// The tree-sitter text objects, when the last parse matches the current text.
    #[must_use]
    pub fn current_syntax_objects(&self) -> &[crate::language::syntax::SyntaxObject] {
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

    pub fn refresh_dirty(&mut self) {
        self.dirty = self
            .saved_text
            .as_ref()
            .is_none_or(|saved| !self.text.text_equals(saved))
            || self.text.format().utf8_bom != self.saved_format.utf8_bom
            || self.text.format().line_endings != self.saved_format.line_endings;
    }

    pub(crate) fn restored_view(&self, buffer: super::BufferId) -> View {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaceRecord {
    pub start: ByteOffset,
    pub original: String,
    pub inserted_len: usize,
}

#[derive(Clone, Debug)]
pub struct View {
    pub buffer: BufferId,
    pub cursor: ByteOffset,
    pub selection: Option<Selection>,
    pub scroll_line: usize,
    /// Wrapped visual row within `scroll_line`; runtime viewport state only.
    pub scroll_wrap_row: usize,
    pub scroll_column: usize,
    pub preferred_column: Option<usize>,
    pub mode: EditorMode,
    pub replace_history: Vec<ReplaceRecord>,
    pub block_insert: Vec<ByteOffset>,
    /// The ranges marked in `EditorMode::Multi`, in order. Empty ranges are plain cursors.
    pub multi_marks: Vec<crate::editor::text::ByteRange>,
    pub multi_whole_word: bool,
    /// Wanted display columns, in mark order; empty after nonvertical movement.
    pub multi_preferred_columns: Vec<usize>,
}

impl View {
    pub(crate) fn clear_editing_state(&mut self) {
        self.replace_history.clear();
        self.block_insert.clear();
        self.multi_marks.clear();
        self.multi_whole_word = true;
        self.multi_preferred_columns.clear();
        self.preferred_column = None;
    }

    #[must_use]
    pub fn new(buffer: BufferId) -> Self {
        Self {
            buffer,
            cursor: ByteOffset(0),
            selection: None,
            scroll_line: 0,
            scroll_wrap_row: 0,
            scroll_column: 0,
            preferred_column: None,
            mode: EditorMode::Normal,
            replace_history: Vec::new(),
            block_insert: Vec::new(),
            multi_marks: Vec::new(),
            multi_whole_word: true,
            multi_preferred_columns: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterShape {
    Character,
    Line,
    Block,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterValue {
    pub text: String,
    pub shape: RegisterShape,
}

#[derive(Clone, Debug, Default)]
pub struct RegisterStore {
    values: HashMap<char, RegisterValue>,
    macros: HashMap<char, Vec<CommandInvocation>>,
    /// Bumped on every stored value, so the clipboard sync notices a yank cheaply.
    generation: u64,
}

impl RegisterStore {
    pub fn set(
        &mut self,
        name: char,
        mut value: RegisterValue,
        enabled: bool,
        mirror_named_to_unnamed: bool,
    ) {
        if !enabled || name == '_' {
            return;
        }
        let lower = name.to_ascii_lowercase();
        if name.is_ascii_uppercase()
            && let Some(previous) = self.values.get(&lower)
        {
            let mut text = if previous.shape == RegisterShape::Block
                && value.shape == RegisterShape::Block
            {
                let mut left = previous.text.split('\n');
                let mut right = value.text.split('\n');
                let mut rows = Vec::new();
                loop {
                    match (left.next(), right.next()) {
                        (None, None) => break,
                        (left, right) => rows.push(format!(
                            "{}{}",
                            left.unwrap_or_default(),
                            right.unwrap_or_default()
                        )),
                    }
                }
                rows.join("\n")
            } else {
                let mut text = previous.text.clone();
                if previous.shape == RegisterShape::Line && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&value.text);
                text
            };
            if previous.shape == RegisterShape::Line || value.shape == RegisterShape::Line {
                value.shape = RegisterShape::Line;
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            } else if value.shape != previous.shape {
                value.shape = RegisterShape::Character;
            }
            value.text = text;
        }
        self.generation = self.generation.wrapping_add(1);
        self.values.insert(lower, value.clone());
        if mirror_named_to_unnamed && name != '"' {
            self.values.insert('"', value);
        }
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn get(&self, name: char) -> Option<&RegisterValue> {
        self.values.get(&name.to_ascii_lowercase())
    }

    pub fn set_macro(&mut self, name: char, invocations: Vec<CommandInvocation>) {
        if name.is_ascii_uppercase() {
            self.macros
                .entry(name.to_ascii_lowercase())
                .or_default()
                .extend(invocations);
        } else {
            self.macros.insert(name, invocations);
        }
    }

    #[must_use]
    pub fn get_macro(&self, name: char) -> Option<&[CommandInvocation]> {
        self.macros
            .get(&name.to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub view: ViewId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub root: LayoutNode,
    pub focused: PaneId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingPolicy {
    pub tab_width: usize,
    pub indent_width: usize,
    pub expand_tabs: bool,
    pub undo_break_on_text_commit: bool,
    pub registers_enabled: bool,
    pub mirror_named_to_unnamed: bool,
    pub ambiguous_width_wide: bool,
    pub autopairs: bool,
}

impl Default for EditingPolicy {
    fn default() -> Self {
        Self {
            tab_width: 4,
            indent_width: 4,
            expand_tabs: true,
            undo_break_on_text_commit: false,
            registers_enabled: true,
            mirror_named_to_unnamed: true,
            ambiguous_width_wide: false,
            autopairs: false,
        }
    }
}
