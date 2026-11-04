//! Several cursors at once: pick matches of a word with Ctrl-N, or add cursors on the lines above
//! and below, then change them together. Insert mode types at every position in
//! `View::block_insert`, the same as Visual block insert.
use crate::editor::{
    ViewId,
    state::{EditingPolicy, EditorMode, View},
    text::{
        ByteOffset, ByteRange, DisplayWidthPolicy, TextBuffer, TextEdit, TextEditSet,
        insertion_at_display_column, next_grapheme_boundary,
    },
    transaction::{EditorOperation, EditorTransaction, UndoPolicy},
};

use super::super::CommandError;
use super::motion::{
    cursor_display_column, first_nonblank, line_content_end, motion_target, on_last_character,
};
use super::transaction::failed;

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn word_at(source: &str, cursor: usize) -> Option<ByteRange> {
    let character = source[cursor..].chars().next()?;
    if !is_word_character(character) {
        return None;
    }
    let start = source[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_word_character(*character))
        .last()
        .map_or(cursor, |(index, _)| index);
    let end = source[cursor..]
        .char_indices()
        .find(|(_, character)| !is_word_character(*character))
        .map_or(source.len(), |(relative, _)| cursor + relative);
    ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()
}

fn is_whole_word(source: &str, start: usize, end: usize) -> bool {
    !source[..start]
        .chars()
        .next_back()
        .is_some_and(is_word_character)
        && !source[end..].chars().next().is_some_and(is_word_character)
}

/// The next place `first`'s text occurs after `from`, going round the end of the file.
fn next_occurrence(
    source: &str,
    first: ByteRange,
    from: usize,
    marks: &[ByteRange],
) -> Option<ByteRange> {
    let needle = &source[first.start.get()..first.end.get()];
    if needle.is_empty() {
        return None;
    }
    let whole_word = needle.chars().all(is_word_character);
    let free = |start: usize| {
        let end = start + needle.len();
        (!whole_word || is_whole_word(source, start, end))
            && !marks
                .iter()
                .any(|mark| mark.start.get() < end && start < mark.end.get())
    };
    source[from..]
        .match_indices(needle)
        .map(|(index, _)| from + index)
        .chain(source[..from].match_indices(needle).map(|(index, _)| index))
        .find(|start| free(*start))
        .and_then(|start| ByteRange::new(ByteOffset(start), ByteOffset(start + needle.len())).ok())
}

fn set_marks(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    marks: Vec<ByteRange>,
    cursor: ByteOffset,
) {
    transaction.undo_policy = UndoPolicy::Ignore;
    transaction.operations.push(EditorOperation::SetMode {
        view: view_id,
        mode: EditorMode::Multi,
    });
    transaction.operations.push(EditorOperation::SetMultiMarks {
        view: view_id,
        marks,
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view: view_id,
        position: cursor,
    });
}

fn visual_range(view: &View, text: &TextBuffer) -> Option<ByteRange> {
    let selection = view
        .selection
        .filter(|_| view.mode == EditorMode::VisualChar)?;
    let start = selection.anchor.min(selection.head);
    let end = next_grapheme_boundary(text, selection.anchor.max(selection.head));
    ByteRange::new(start, end).ok()
}

/// Ctrl-N: the first press marks the word or selection, each next press its next match.
pub(super) fn add_next(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    text: &TextBuffer,
) -> Result<(), CommandError> {
    let source = text.to_string();
    let mut marks = view.multi_marks.clone();
    let added = match marks.first().filter(|first| !first.is_empty()) {
        Some(first) => {
            let from = view
                .cursor
                .get()
                .saturating_add(first.end.get() - first.start.get());
            next_occurrence(&source, *first, from.min(source.len()), &marks)
        }
        None if marks.is_empty() => {
            visual_range(view, text).or_else(|| word_at(&source, view.cursor.get()))
        }
        None => {
            return Err(failed(
                "Ctrl-N needs a word; these cursors were added by line",
            ));
        }
    }
    .ok_or_else(|| {
        failed(if marks.is_empty() {
            "no word under the cursor"
        } else {
            "no more matches"
        })
    })?;
    marks.push(added);
    marks.sort_by_key(|mark| mark.start);
    set_marks(transaction, view_id, marks, added.start);
    Ok(())
}

/// A cursor on the next line up or down, at the same offset along the line.
pub(super) fn add_line(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    text: &TextBuffer,
    below: bool,
    policy: EditingPolicy,
) -> Result<(), CommandError> {
    let source = text.to_string();
    let mut marks = view.multi_marks.clone();
    if marks.is_empty() {
        marks.push(ByteRange::empty(view.cursor));
    }
    if marks.iter().any(|mark| !mark.is_empty()) {
        return Err(failed(
            "these cursors are on word matches; press Ctrl-N to add more",
        ));
    }
    let edge = if below { marks.last() } else { marks.first() }
        .map_or(view.cursor.get(), |mark| mark.start.get());
    let line_start = source[..edge].rfind('\n').map_or(0, |index| index + 1);
    let column = cursor_display_column(text, ByteOffset(edge), policy)?;
    let target_start = if below {
        source[edge..].find('\n').map(|index| edge + index + 1)
    } else {
        line_start
            .checked_sub(1)
            .map(|end| source[..end].rfind('\n').map_or(0, |index| index + 1))
    }
    .ok_or_else(|| failed("no line there to add a cursor on"))?;
    let line = source[target_start..]
        .split('\n')
        .next()
        .unwrap_or_default();
    let (offset, _) = insertion_at_display_column(
        line,
        column,
        DisplayWidthPolicy {
            tab_width: policy.tab_width,
            ambiguous_wide: policy.ambiguous_width_wide,
        },
    );
    let position = ByteOffset(target_start + offset);
    marks.push(ByteRange::empty(position));
    marks.sort_by_key(|mark| mark.start);
    set_marks(transaction, view_id, marks, position);
    Ok(())
}

/// The mark the cursor is on, else the first one after it.
fn current(marks: &[ByteRange], cursor: ByteOffset) -> usize {
    marks
        .iter()
        .position(|mark| mark.start == cursor || (mark.start < cursor && cursor < mark.end))
        .or_else(|| marks.iter().position(|mark| mark.start > cursor))
        .unwrap_or(0)
}

/// Every match of the word or selection, as Ctrl-Shift-L does in VS Code.
pub(super) fn add_all(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    text: &TextBuffer,
) -> Result<(), CommandError> {
    let source = text.to_string();
    // A visual selection matches as typed; a word under the cursor matches whole words only.
    let selected = visual_range(view, text);
    let first = match view.multi_marks.first() {
        Some(first) if first.is_empty() => {
            return Err(failed(
                "selecting every match needs a word; these cursors were added by line",
            ));
        }
        Some(first) => *first,
        None => selected
            .or_else(|| word_at(&source, view.cursor.get()))
            .ok_or_else(|| failed("no word under the cursor"))?,
    };
    let needle = &source[first.start.get()..first.end.get()];
    let whole_word = selected.is_none() && needle.chars().all(is_word_character);
    let marks: Vec<ByteRange> = source
        .match_indices(needle)
        .map(|(start, _)| start)
        .filter(|start| !whole_word || is_whole_word(&source, *start, start + needle.len()))
        .filter_map(|start| {
            ByteRange::new(ByteOffset(start), ByteOffset(start + needle.len())).ok()
        })
        .collect();
    if marks.is_empty() {
        return Err(failed("no matches"));
    }
    let cursor = marks[current(&marks, view.cursor)].start;
    set_marks(transaction, view_id, marks, cursor);
    Ok(())
}

/// `n` and `N`: the cursor goes to the next or previous mark, round the ends.
pub(super) fn cycle(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    forward: bool,
) {
    let marks = &view.multi_marks;
    if marks.is_empty() {
        return;
    }
    let index = current(marks, view.cursor);
    let next = if forward {
        (index + 1) % marks.len()
    } else {
        (index + marks.len() - 1) % marks.len()
    };
    transaction.undo_policy = UndoPolicy::Ignore;
    transaction.operations.push(EditorOperation::SetCursor {
        view: view_id,
        position: marks[next].start,
    });
}

/// `q`: drops the match the cursor is on and marks the next one instead.
pub(super) fn skip(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    text: &TextBuffer,
) -> Result<(), CommandError> {
    let source = text.to_string();
    let mut marks = view.multi_marks.clone();
    let index = current(&marks, view.cursor);
    let skipped = marks
        .get(index)
        .copied()
        .filter(|mark| !mark.is_empty())
        .ok_or_else(|| failed("q skips a word match; these cursors were added by line"))?;
    // With no match left to take instead, skipping just drops this one.
    let Some(next) = next_occurrence(&source, skipped, skipped.end.get(), &marks) else {
        remove(transaction, view_id, view);
        return Ok(());
    };
    marks.remove(index);
    marks.push(next);
    marks.sort_by_key(|mark| mark.start);
    set_marks(transaction, view_id, marks, next.start);
    Ok(())
}

/// `Q`: removes the cursor the cursor is on; removing the last one goes back to Normal mode.
pub(super) fn remove(transaction: &mut EditorTransaction, view_id: ViewId, view: &View) {
    let mut marks = view.multi_marks.clone();
    if marks.is_empty() {
        return;
    }
    let index = current(&marks, view.cursor);
    marks.remove(index);
    match marks.get(index.saturating_sub(1)).map(|mark| mark.start) {
        Some(cursor) => set_marks(transaction, view_id, marks, cursor),
        None => {
            transaction.undo_policy = UndoPolicy::Ignore;
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::Normal,
            });
        }
    }
}

/// A motion moves every cursor; a word match becomes a cursor at its start first.
pub(super) fn move_all(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    view: &View,
    text: &TextBuffer,
    motion: &str,
    count: usize,
    policy: EditingPolicy,
) -> Result<(), CommandError> {
    let index = current(&view.multi_marks, view.cursor);
    let mut cursor = None;
    let mut marks = Vec::with_capacity(view.multi_marks.len());
    for (at, mark) in view.multi_marks.iter().enumerate() {
        let wanted_column = matches!(motion, "up" | "down")
            .then(|| cursor_display_column(text, mark.start, policy))
            .transpose()?;
        let mut target =
            motion_target(text, mark.start, motion, count, None, policy, wanted_column)?;
        if wanted_column.is_some() {
            target = on_last_character(text, target)?;
        }
        if at == index {
            cursor = Some(target);
        }
        marks.push(ByteRange::empty(target));
    }
    marks.sort_by_key(|mark| mark.start);
    marks.dedup();
    if let Some(cursor) = cursor {
        set_marks(transaction, view_id, marks, cursor);
    }
    Ok(())
}

/// `x`: deletes the character at every cursor and keeps the cursors.
pub(super) fn delete_char(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    buffer: crate::editor::BufferId,
    view: &View,
    text: &TextBuffer,
) -> Result<(), CommandError> {
    let mut edits = Vec::new();
    let mut points = Vec::new();
    for mark in &view.multi_marks {
        let next = next_grapheme_boundary(text, mark.start);
        let range = ByteRange::new(mark.start, next).map_err(failed)?;
        points.push(ByteRange::empty(mark.start));
        if !range.is_empty() && text.slice(range).map_err(failed)? != "\n" {
            edits.push(TextEdit::new(range, ""));
        }
    }
    transaction.undo_policy = UndoPolicy::Record;
    transaction.operations.push(EditorOperation::SetMultiMarks {
        view: view_id,
        marks: points,
    });
    if !edits.is_empty() {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(text.revision(), edits),
        });
    }
    Ok(())
}

/// `c`, `i` and `a` type at each mark, `I` and `A` at the start and end of each mark's line.
/// `c` removes the marked text first.
pub(super) fn begin_insert(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    buffer: crate::editor::BufferId,
    view: &View,
    text: &TextBuffer,
    placement: &str,
    marks: &[ByteRange],
) -> Result<(), CommandError> {
    let mut positions = Vec::with_capacity(marks.len());
    for mark in marks {
        positions.push(match placement {
            "end" if mark.is_empty() => next_grapheme_boundary(text, mark.end).min(
                line_content_end(text, text.byte_to_line(mark.end).map_err(failed)?)?,
            ),
            "end" => mark.end,
            "line_start" => first_nonblank(text, text.byte_to_line(mark.start).map_err(failed)?)?,
            "line_end" => line_content_end(text, text.byte_to_line(mark.start).map_err(failed)?)?,
            _ => mark.start,
        });
    }
    positions.sort();
    positions.dedup();
    let Some(first) = positions.first().copied() else {
        return Ok(());
    };
    // Merge, so the text typed next undoes together with the removal.
    transaction.undo_policy = UndoPolicy::Merge;
    transaction.operations.push(EditorOperation::SetMode {
        view: view_id,
        mode: EditorMode::Insert,
    });
    transaction
        .operations
        .push(EditorOperation::SetBlockInsert {
            view: view_id,
            positions,
        });
    if placement == "change" {
        push_delete(transaction, buffer, view, text);
    }
    transaction.operations.push(EditorOperation::SetCursor {
        view: view_id,
        position: first,
    });
    Ok(())
}

/// `d`: removes every mark and goes back to Normal mode.
pub(super) fn delete(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    buffer: crate::editor::BufferId,
    view: &View,
    text: &TextBuffer,
) {
    transaction.undo_policy = UndoPolicy::Record;
    transaction.operations.push(EditorOperation::SetMode {
        view: view_id,
        mode: EditorMode::Normal,
    });
    push_delete(transaction, buffer, view, text);
}

fn push_delete(
    transaction: &mut EditorTransaction,
    buffer: crate::editor::BufferId,
    view: &View,
    text: &TextBuffer,
) {
    let edits: Vec<TextEdit> = view
        .multi_marks
        .iter()
        .filter(|mark| !mark.is_empty())
        .map(|mark| TextEdit::new(*mark, ""))
        .collect();
    if !edits.is_empty() {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(text.revision(), edits),
        });
    }
}
