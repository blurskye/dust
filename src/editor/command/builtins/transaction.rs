use std::sync::atomic::{AtomicU64, Ordering};

use crate::editor::{
    state::{BufferId, EditingPolicy, EditorMode, TransactionId, ViewId},
    text::{
        BufferRevision, ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet,
        next_grapheme_boundary,
    },
    transaction::{EditorOperation, EditorTransaction, Precondition},
};

use super::{
    super::{CommandError, CommandInvocation},
    motion::{first_nonblank, line_content_end},
};

static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub(super) fn transaction_for(
    view: ViewId,
    buffer: BufferId,
    revision: BufferRevision,
) -> EditorTransaction {
    EditorTransaction::new(TransactionId(
        NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed),
    ))
    .with_precondition(Precondition::ViewDisplays { view, buffer })
    .with_precondition(Precondition::BufferRevision { buffer, revision })
}

pub(super) fn string_argument<'a>(
    invocation: &'a CommandInvocation,
    name: &str,
) -> Result<&'a str, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_str()
}

pub(super) fn integer_argument(
    invocation: &CommandInvocation,
    name: &str,
) -> Result<i64, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_integer()
}

pub(super) fn bool_argument(
    invocation: &CommandInvocation,
    name: &str,
) -> Result<bool, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_bool()
}

pub(super) fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::Failed(error.to_string())
}

pub(super) fn build_insert_transition(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    cursor: ByteOffset,
    placement: &str,
) -> Result<(), CommandError> {
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let line_start = text.line_to_byte(line).map_err(failed)?;
    let content_end = line_content_end(text, line)?;
    let mut target = cursor;
    let mut insertion = None;
    match placement {
        "before" => {}
        "after" => target = next_grapheme_boundary(text, cursor).min(content_end),
        "line_start" => target = first_nonblank(text, line)?,
        "line_end" => target = content_end,
        "open_below" => {
            target = content_end;
            insertion = Some((target, "\n"));
            target = ByteOffset(target.get() + 1);
        }
        "open_above" => {
            target = line_start;
            insertion = Some((target, "\n"));
        }
        _ => {
            return Err(CommandError::InvalidArguments(
                "unknown insert placement".into(),
            ));
        }
    }
    if let Some((at, inserted)) = insertion {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(
                text.revision(),
                vec![TextEdit::new(ByteRange::empty(at), inserted)],
            ),
        });
    }
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: target,
    });
    transaction.operations.push(EditorOperation::SetMode {
        view,
        mode: EditorMode::Insert,
    });
    Ok(())
}

pub(super) fn build_join_lines(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    cursor: ByteOffset,
    count: usize,
) -> Result<(), CommandError> {
    let first_line = text.byte_to_line(cursor).map_err(failed)?;
    let last_line = first_line
        .saturating_add(count)
        .min(text.len_lines().saturating_sub(1));
    if last_line == first_line {
        return Ok(());
    }
    let start = text.line_to_byte(first_line).map_err(failed)?;
    let end = line_content_end(text, last_line)?;
    let mut joined = String::new();
    for line in first_line..=last_line {
        let value = text.line(line).map_err(failed)?;
        let content = value.strip_suffix('\n').unwrap_or(&value);
        if line == first_line {
            joined.push_str(content.trim_end());
        } else {
            let content = content.trim();
            if !joined.is_empty() && !content.is_empty() {
                joined.push(' ');
            }
            joined.push_str(content);
        }
    }
    let target = ByteOffset(cursor.get().min(start.get() + joined.len()));
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            vec![TextEdit::new(
                ByteRange::new(start, end).map_err(failed)?,
                joined,
            )],
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: target,
    });
    Ok(())
}

pub(super) fn build_toggle_case(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    cursor: ByteOffset,
    count: usize,
) -> Result<(), CommandError> {
    let mut end = cursor;
    let mut replacement = String::new();
    for _ in 0..count {
        let next = next_grapheme_boundary(text, end);
        if next == end {
            break;
        }
        let grapheme = text
            .slice(ByteRange::new(end, next).map_err(failed)?)
            .map_err(failed)?;
        if grapheme == "\n" {
            break;
        }
        for character in grapheme.chars() {
            if character.is_lowercase() {
                replacement.extend(character.to_uppercase());
            } else if character.is_uppercase() {
                replacement.extend(character.to_lowercase());
            } else {
                replacement.push(character);
            }
        }
        end = next;
    }
    if end != cursor {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(
                text.revision(),
                vec![TextEdit::new(
                    ByteRange::new(cursor, end).map_err(failed)?,
                    replacement,
                )],
            ),
        });
        transaction.operations.push(EditorOperation::SetCursor {
            view,
            position: cursor,
        });
    }
    Ok(())
}

pub(super) fn build_indent(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    range: ByteRange,
    indent: bool,
    levels: usize,
    policy: EditingPolicy,
) -> Result<(), CommandError> {
    let first_line = text.byte_to_line(range.start).map_err(failed)?;
    let final_byte = range.end.get().saturating_sub(1).max(range.start.get());
    let last_line = text.byte_to_line(ByteOffset(final_byte)).map_err(failed)?;
    let start = text.line_to_byte(first_line).map_err(failed)?;
    let end = if last_line + 1 < text.len_lines() {
        text.line_to_byte(last_line + 1).map_err(failed)?
    } else {
        ByteOffset(text.len_bytes())
    };
    let source = text
        .slice(ByteRange::new(start, end).map_err(failed)?)
        .map_err(failed)?;
    let mut replacement = String::new();
    for line in source.split_inclusive('\n') {
        // Empty lines stay empty, as in Vim.
        if indent && line.trim_end_matches(['\r', '\n']).is_empty() {
            replacement.push_str(line);
        } else if indent {
            for _ in 0..levels {
                if policy.expand_tabs {
                    replacement.push_str(&" ".repeat(policy.indent_width));
                } else {
                    replacement.push('\t');
                }
            }
            replacement.push_str(line);
        } else {
            let mut rest = line;
            for _ in 0..levels {
                rest = match rest.strip_prefix('\t') {
                    Some(rest) => rest,
                    None => {
                        let spaces = rest
                            .bytes()
                            .take_while(|byte| *byte == b' ')
                            .take(policy.indent_width)
                            .count();
                        &rest[spaces..]
                    }
                };
            }
            replacement.push_str(rest);
        }
    }
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            vec![TextEdit::new(
                ByteRange::new(start, end).map_err(failed)?,
                replacement,
            )],
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: start,
    });
    Ok(())
}
