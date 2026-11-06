//! Transactions produced by built-in editing commands.

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use unicode_segmentation::UnicodeSegmentation;

use crate::editor::state::BufferId;

use crate::editor::state::EditingPolicy;

use crate::editor::state::EditorMode;

use crate::editor::state::TransactionId;

use crate::editor::state::ViewId;

use crate::editor::text::BufferRevision;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;

use crate::editor::text::next_grapheme_boundary;

use crate::editor::text::previous_grapheme_boundary;

use crate::editor::transaction::EditorOperation;

use crate::editor::transaction::EditorTransaction;

use crate::editor::transaction::Precondition;

use crate::editor::command::CommandError;

use crate::editor::CommandInvocation;

use crate::editor::command::builtins::motion::first_nonblank;

use crate::editor::command::builtins::motion::line_content_end;

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
        .min(text.visible_lines().saturating_sub(1));
    if last_line <= first_line {
        return Ok(());
    }
    let start = text.line_to_byte(first_line).map_err(failed)?;
    let end = line_content_end(text, last_line)?;
    let mut joined = String::new();
    let mut boundary = 0;
    for line in first_line..=last_line {
        let value = text.line(line).map_err(failed)?;
        let content = value.strip_suffix('\n').unwrap_or(&value);
        if line == first_line {
            joined.push_str(content.trim_end());
            boundary = joined.len();
        } else {
            let content = if line == last_line {
                content.trim_start()
            } else {
                content.trim()
            };
            if !joined.is_empty() && !content.is_empty() {
                joined.push(' ');
            }
            joined.push_str(content);
        }
    }
    let last_character = joined
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index);
    let target = ByteOffset(start.get() + boundary.min(last_character));
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
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let line_start = text.line_to_byte(line).map_err(failed)?;
    let source = text.line(line).map_err(failed)?;
    let content = source.strip_suffix('\n').unwrap_or(&source);
    let mut end = cursor;
    let mut replacement = String::new();
    for grapheme in content[cursor.get() - line_start.get()..]
        .graphemes(true)
        .take(count)
    {
        for character in grapheme.chars() {
            if character.is_lowercase() {
                replacement.extend(character.to_uppercase());
            } else if character.is_uppercase() {
                replacement.extend(character.to_lowercase());
            } else {
                replacement.push(character);
            }
        }
        end = ByteOffset(end.get() + grapheme.len());
    }
    if end != cursor {
        let advance = if end.get() < line_start.get() + content.len() {
            replacement.len()
        } else {
            replacement
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(index, _)| index)
        };
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
            position: ByteOffset(cursor.get() + advance),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    let final_byte = if range.is_empty() {
        range.start
    } else {
        previous_grapheme_boundary(text, range.end)
    };
    let last_line = text.byte_to_line(final_byte).map_err(failed)?;
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
        } else {
            let tab_width = policy.tab_width.max(1);
            let mut columns = 0;
            let mut prefix_len = 0;
            for byte in line.bytes() {
                match byte {
                    b' ' => columns += 1,
                    b'\t' => columns += tab_width - columns % tab_width,
                    _ => break,
                }
                prefix_len += 1;
            }
            let shift = policy.indent_width.saturating_mul(levels);
            let columns = if indent {
                columns.saturating_add(shift)
            } else {
                columns.saturating_sub(shift)
            };
            if policy.expand_tabs {
                replacement.push_str(&" ".repeat(columns));
            } else {
                replacement.push_str(&"\t".repeat(columns / tab_width));
                replacement.push_str(&" ".repeat(columns % tab_width));
            }
            replacement.push_str(&line[prefix_len..]);
        }
    }
    let first_line = replacement.split('\n').next().unwrap_or_default();
    let first_nonblank = first_line
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(0, |(index, _)| index);
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
        position: ByteOffset(start.get() + first_nonblank),
    });
    Ok(())
}
