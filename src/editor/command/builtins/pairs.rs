//! Bracket and quote pairs: `gsa`/`gsd`/`gsr` surround edits (mini.surround keys) and
//! Insert-mode autopairs (mini.pairs rules).

use crate::editor::state::BufferId;

use crate::editor::state::EditorMode;

use crate::editor::state::ViewId;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;

use crate::editor::transaction::EditorOperation;

use crate::editor::transaction::EditorTransaction;

use crate::editor::command::CommandError;

use crate::editor::command::builtins::text_object::pair_for;

use crate::editor::command::builtins::text_object::surrounding;

use crate::editor::command::builtins::transaction::failed;

/// An opening bracket adds inner spaces, a closing one or `b`/`B` does not; any other
/// character surrounds with itself.
fn surround_strings(character: char) -> (String, String) {
    match character {
        '(' | '[' | '{' | '<' => {
            let (open, close) = pair_for(character).expect("bracket pair");
            (format!("{open} "), format!(" {close}"))
        }
        _ => pair_for(character).map_or_else(
            || (character.to_string(), character.to_string()),
            |(open, close)| (open.to_string(), close.to_string()),
        ),
    }
}

pub(super) fn build_surround_add(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    range: ByteRange,
    character: char,
) -> Result<(), CommandError> {
    let mut end = range.end;
    if end > range.start && text.slice(range).map_err(failed)?.ends_with('\n') {
        end = ByteOffset(end.get() - 1);
    }
    let (open, close) = surround_strings(character);
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            if range.start == end {
                vec![TextEdit::new(
                    ByteRange::empty(end),
                    format!("{open}{close}"),
                )]
            } else {
                vec![
                    TextEdit::new(ByteRange::empty(range.start), open),
                    TextEdit::new(ByteRange::empty(end), close),
                ]
            },
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: range.start,
    });
    transaction.operations.push(EditorOperation::SetMode {
        view,
        mode: EditorMode::Normal,
    });
    Ok(())
}

/// Deletes the pair named by `character` around the cursor, or replaces it with the pair
/// named by `replacement`. An opening bracket also removes the spaces just inside it.
pub(super) fn build_surround_change(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    cursor: ByteOffset,
    character: char,
    replacement: Option<char>,
) -> Result<(), CommandError> {
    let range = surrounding(text, cursor, character)?.ok_or_else(|| {
        CommandError::Unavailable(format!("no `{character}` pair around the cursor"))
    })?;
    let inside = text.slice(range).map_err(failed)?;
    let (open, close) = pair_for(character).unwrap_or((character, character));
    let mut inner_start = range.start.get() + open.len_utf8();
    let mut inner_end = range.end.get() - close.len_utf8();
    if matches!(character, '(' | '[' | '{' | '<') {
        let body = &inside[open.len_utf8()..inside.len() - close.len_utf8()];
        inner_start += body.len() - body.trim_start_matches(' ').len();
        inner_end -= body.len() - body.trim_end_matches(' ').len();
        inner_end = inner_end.max(inner_start);
    }
    let (new_open, new_close) =
        replacement.map_or_else(|| (String::new(), String::new()), surround_strings);
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            vec![
                TextEdit::new(
                    ByteRange::new(range.start, ByteOffset(inner_start)).map_err(failed)?,
                    new_open,
                ),
                TextEdit::new(
                    ByteRange::new(ByteOffset(inner_end), range.end).map_err(failed)?,
                    new_close,
                ),
            ],
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: range.start,
    });
    Ok(())
}

/// What typing `typed` at `cursor` should do with autopairs on.
pub(super) enum AutoPair {
    /// Insert the character followed by its closing partner.
    Open(char),
    /// Step over the closing character already under the cursor.
    Skip,
}

pub(super) fn auto_pair(text: &TextBuffer, cursor: ByteOffset, typed: &str) -> Option<AutoPair> {
    auto_pair_over(text, ByteRange::empty(cursor), typed)
}

/// `auto_pair` for text that replaces `range`: what counts is around the range, not in it.
pub(super) fn auto_pair_over(text: &TextBuffer, range: ByteRange, typed: &str) -> Option<AutoPair> {
    let mut characters = typed.chars();
    let typed = characters.next()?;
    if characters.next().is_some() {
        return None;
    }
    let (next, _) = neighbours(text, range.end);
    let (_, previous) = neighbours(text, range.start);
    if matches!(typed, '"' | '\'' | '`') {
        let line = text.byte_to_line(range.start).ok()?;
        let start = text.line_to_byte(line).ok()?;
        let before = text.slice(ByteRange::new(start, range.start).ok()?).ok()?;
        if before
            .chars()
            .rev()
            .take_while(|character| *character == '\\')
            .count()
            % 2
            == 1
        {
            return None;
        }
    }
    if matches!(typed, ')' | ']' | '}' | '"' | '\'' | '`') && next == Some(typed) {
        return Some(AutoPair::Skip);
    }
    let close = match typed {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' | '\'' | '`' => typed,
        _ => return None,
    };
    if next.is_some_and(|next| next.is_alphanumeric() || "_%'[\".`$".contains(next)) {
        return None;
    }
    if close == typed && previous.is_some_and(|previous| previous.is_alphanumeric()) {
        return None;
    }
    Some(AutoPair::Open(close))
}

/// Backspace between an empty pair such as `(|)` removes both characters.
pub(super) fn empty_pair_end(text: &TextBuffer, cursor: ByteOffset) -> Option<ByteOffset> {
    let (next, previous) = neighbours(text, cursor);
    let (previous, next) = (previous?, next?);
    matches!(
        (previous, next),
        ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'') | ('`', '`')
    )
    .then(|| ByteOffset(cursor.get() + next.len_utf8()))
}

fn neighbours(text: &TextBuffer, cursor: ByteOffset) -> (Option<char>, Option<char>) {
    let Ok(line) = text.byte_to_line(cursor) else {
        return (None, None);
    };
    let (Ok(start), Ok(content)) = (text.line_to_byte(line), text.line(line)) else {
        return (None, None);
    };
    let relative = cursor.get().saturating_sub(start.get()).min(content.len());
    let next = content[relative..]
        .chars()
        .next()
        .filter(|next| *next != '\n');
    let previous = content[..relative].chars().next_back();
    (next, previous)
}
