//! Several cursors at once: pick matches of a word with Ctrl-N, or add cursors on the lines above
//! and below, then change them together. Insert mode types at every position in
//! `View::block_insert`, the same as Visual block insert.
use crate::editor::{
    state::{EditorMode, View},
    text::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet, next_grapheme_boundary},
    transaction::{EditorOperation, EditorTransaction, UndoPolicy},
    ViewId,
};

use super::super::CommandError;
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
    !source[..start].chars().next_back().is_some_and(is_word_character)
        && !source[end..].chars().next().is_some_and(is_word_character)
}

/// The next place `first`'s text occurs after `from`, going round the end of the file.
fn next_occurrence(source: &str, first: ByteRange, from: usize, marks: &[ByteRange]) -> Option<ByteRange> {
    let needle = &source[first.start.get()..first.end.get()];
    if needle.is_empty() {
        return None;
    }
    let whole_word = needle.chars().all(is_word_character);
    let free = |start: usize| {
        let end = start + needle.len();
        (!whole_word || is_whole_word(source, start, end))
            && !marks.iter().any(|mark| mark.start.get() < end && start < mark.end.get())
    };
    source[from..]
        .match_indices(needle)
        .map(|(index, _)| from + index)
        .chain(source[..from].match_indices(needle).map(|(index, _)| index))
        .find(|start| free(*start))
        .and_then(|start| ByteRange::new(ByteOffset(start), ByteOffset(start + needle.len())).ok())
}

fn set_marks(transaction: &mut EditorTransaction, view_id: ViewId, marks: Vec<ByteRange>, cursor: ByteOffset) {
    transaction.undo_policy = UndoPolicy::Ignore;
    transaction.operations.push(EditorOperation::SetMode { view: view_id, mode: EditorMode::Multi });
    transaction.operations.push(EditorOperation::SetMultiMarks { view: view_id, marks });
    transaction.operations.push(EditorOperation::SetCursor { view: view_id, position: cursor });
}

fn visual_range(view: &View, text: &TextBuffer) -> Option<ByteRange> {
    let selection = view.selection.filter(|_| view.mode == EditorMode::VisualChar)?;
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
            let from = view.cursor.get().saturating_add(first.end.get() - first.start.get());
            next_occurrence(&source, *first, from.min(source.len()), &marks)
        }
        None if marks.is_empty() => visual_range(view, text).or_else(|| word_at(&source, view.cursor.get())),
        None => return Err(failed("Ctrl-N needs a word; these cursors were added by line")),
    }
    .ok_or_else(|| failed(if marks.is_empty() { "no word under the cursor" } else { "no more matches" }))?;
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
) -> Result<(), CommandError> {
    let source = text.to_string();
    let mut marks = view.multi_marks.clone();
    if marks.is_empty() {
        marks.push(ByteRange::empty(view.cursor));
    }
    if marks.iter().any(|mark| !mark.is_empty()) {
        return Err(failed("these cursors are on word matches; press Ctrl-N to add more"));
    }
    let edge = if below { marks.last() } else { marks.first() }
        .map_or(view.cursor.get(), |mark| mark.start.get());
    let line_start = source[..edge].rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..edge].chars().count();
    let target_start = if below {
        source[edge..].find('\n').map(|index| edge + index + 1)
    } else {
        line_start
            .checked_sub(1)
            .map(|end| source[..end].rfind('\n').map_or(0, |index| index + 1))
    }
    .ok_or_else(|| failed("no line there to add a cursor on"))?;
    let line = source[target_start..].split('\n').next().unwrap_or_default();
    let offset = line.char_indices().nth(column).map_or(line.len(), |(index, _)| index);
    let position = ByteOffset(target_start + offset);
    marks.push(ByteRange::empty(position));
    marks.sort_by_key(|mark| mark.start);
    set_marks(transaction, view_id, marks, position);
    Ok(())
}

/// `c`, `i` and `a`: leave the marks for Insert mode, typing at each. `c` removes them first.
pub(super) fn begin_insert(
    transaction: &mut EditorTransaction,
    view_id: ViewId,
    buffer: crate::editor::BufferId,
    view: &View,
    text: &TextBuffer,
    placement: &str,
) -> Result<(), CommandError> {
    let positions: Vec<ByteOffset> = view
        .multi_marks
        .iter()
        .map(|mark| if placement == "end" { mark.end } else { mark.start })
        .collect();
    let Some(first) = positions.first().copied() else {
        return Ok(());
    };
    transaction.undo_policy = UndoPolicy::Record;
    transaction.operations.push(EditorOperation::SetMode { view: view_id, mode: EditorMode::Insert });
    transaction.operations.push(EditorOperation::SetBlockInsert { view: view_id, positions });
    if placement == "change" {
        push_delete(transaction, buffer, view, text);
    }
    transaction.operations.push(EditorOperation::SetCursor { view: view_id, position: first });
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
    transaction.operations.push(EditorOperation::SetMode { view: view_id, mode: EditorMode::Normal });
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
