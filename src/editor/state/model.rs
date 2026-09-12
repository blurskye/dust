use std::{collections::HashMap, path::PathBuf};

use crate::editor::{
    command::CommandInvocation,
    text::{BufferRevision, ByteOffset, TextBuffer},
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
    pub dirty: bool,
    pub saved_revision: BufferRevision,
    pub(crate) saved_text: String,
    persisted_bytes: Vec<u8>,
    pub undo: UndoTree,
}

impl Buffer {
    #[must_use]
    pub fn new(text: TextBuffer, path: Option<PathBuf>) -> Self {
        let saved_text = text.to_string();
        let saved_revision = text.revision();
        let persisted_bytes = text.to_bytes();
        Self {
            undo: UndoTree::new(saved_text.clone()),
            text,
            path,
            read_only: false,
            origin: BufferOrigin::FileOrScratch,
            dirty: false,
            saved_revision,
            saved_text,
            persisted_bytes,
        }
    }

    #[must_use]
    pub fn is_virtual_read_only(&self) -> bool {
        matches!(self.origin, BufferOrigin::VirtualReadOnly { .. })
    }

    pub fn refresh_dirty(&mut self) {
        self.dirty = self.text.to_string() != self.saved_text;
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
        self.persisted_bytes = bytes.clone();
        self.saved_revision = revision;
        if let Ok(saved) = TextBuffer::from_bytes(&bytes) {
            self.saved_text = saved.to_string();
        }
        self.refresh_dirty();
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
}

impl View {
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
}

impl RegisterStore {
    pub fn set(
        &mut self,
        name: char,
        value: RegisterValue,
        enabled: bool,
        mirror_named_to_unnamed: bool,
    ) {
        if !enabled || name == '_' {
            return;
        }
        self.values.insert(name, value.clone());
        if mirror_named_to_unnamed && name != '"' {
            self.values.insert('"', value);
        }
    }

    #[must_use]
    pub fn get(&self, name: char) -> Option<&RegisterValue> {
        self.values.get(&name)
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
        }
    }
}
