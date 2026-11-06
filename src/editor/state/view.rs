//! Cursor, selection and viewport state.

use crate::editor::state::id::BufferId;
use crate::editor::text::ByteOffset;

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
pub struct ReplaceRecord {
    pub start: ByteOffset,
    pub original: String,
    pub inserted: String,
}

#[derive(Clone, Debug)]
pub struct InsertRepeat {
    pub start: ByteOffset,
    pub count: usize,
    pub new_line: bool,
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
    pub insert_repeat: Option<InsertRepeat>,
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
        self.insert_repeat = None;
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
            insert_repeat: None,
            block_insert: Vec::new(),
            multi_marks: Vec::new(),
            multi_whole_word: true,
            multi_preferred_columns: Vec::new(),
        }
    }
}
