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

/// A view's collapsed source lines, shared by motions and visual-row layout.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FoldedLines(Vec<(usize, usize)>);

impl FoldedLines {
    #[must_use]
    pub fn new(ranges: impl Iterator<Item = (usize, usize)>, last: usize) -> Self {
        let mut ranges: Vec<_> = ranges
            .map(|(start, end)| (start.min(last), end.min(last)))
            .filter(|&(start, end)| start < end)
            .collect();
        ranges.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            if let Some(previous) = merged.last_mut()
                && start <= previous.1
            {
                previous.1 = previous.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        Self(merged)
    }

    #[must_use]
    pub fn containing(&self, line: usize) -> Option<(usize, usize)> {
        let index = self.0.partition_point(|&(start, _)| start < line);
        self.0[..index]
            .last()
            .copied()
            .filter(|&(_, end)| line <= end)
    }

    #[must_use]
    pub fn header(&self, line: usize) -> usize {
        self.containing(line).map_or(line, |(start, _)| start)
    }

    #[must_use]
    pub fn end(&self, line: usize) -> usize {
        self.0
            .get(self.0.partition_point(|&(start, _)| start < line))
            .filter(|&&(start, _)| start == line)
            .map_or(line, |&(_, end)| end)
    }

    #[must_use]
    pub fn forward(&self, line: usize, count: usize, last: usize) -> usize {
        let line = self.header(line.min(last));
        let last = self.header(last);
        let mut target = line.saturating_add(count).min(last);
        for &(start, end) in &self.0[self.0.partition_point(|&(_, end)| end < line)..] {
            if start >= target {
                break;
            }
            target = target.saturating_add(end - start).min(last);
        }
        target
    }

    /// How many visible lines apart two visible lines are, as `3j` counts them.
    #[must_use]
    pub fn distance(&self, from: usize, to: usize) -> usize {
        let (low, high) = (from.min(to), from.max(to));
        let hidden: usize = self
            .0
            .iter()
            .filter(|&&(start, end)| start >= low && end < high)
            .map(|&(start, end)| end - start)
            .sum();
        (high - low).saturating_sub(hidden)
    }

    #[must_use]
    pub fn backward(&self, line: usize, count: usize) -> usize {
        let line = self.header(line);
        let mut target = line.saturating_sub(count);
        let before = self.0.partition_point(|&(start, _)| start < line);
        for &(start, end) in self.0[..before].iter().rev() {
            if end < target {
                break;
            }
            target = target.saturating_sub(end - start);
        }
        target
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub buffer: BufferId,
    pub cursor: ByteOffset,
    pub selection: Option<Selection>,
    /// Previous cursor and selection for incremental syntax selection, newest last.
    pub syntax_selections: Vec<(ByteOffset, Option<Selection>)>,
    pub scroll_line: usize,
    /// Wrapped visual row within `scroll_line`; runtime viewport state only.
    pub scroll_wrap_row: usize,
    pub scroll_column: usize,
    pub preferred_column: Option<usize>,
    /// Runtime motion projection supplied by the app before command dispatch.
    pub folded_lines: FoldedLines,
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
    pub fn clear_editing_state(&mut self) {
        self.syntax_selections.clear();
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
            syntax_selections: Vec::new(),
            scroll_line: 0,
            scroll_wrap_row: 0,
            scroll_column: 0,
            preferred_column: None,
            folded_lines: FoldedLines::default(),
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
