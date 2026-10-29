//! `gc` line comments. Blank lines are left alone; if every other line in the range is
//! already commented the range is uncommented, otherwise the comment string goes in at
//! the smallest indent, as in Neovim.

use crate::editor::{
    state::{BufferId, EditorMode, ViewId},
    text::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet},
    transaction::{EditorOperation, EditorTransaction},
};

use super::{super::CommandError, transaction::failed};

pub(super) fn build_comment_toggle(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: BufferId,
    range: ByteRange,
    cursor: ByteOffset,
    token: &str,
) -> Result<(), CommandError> {
    let first = text.byte_to_line(range.start).map_err(failed)?;
    let mut last = text.byte_to_line(range.end).map_err(failed)?;
    if last > first && text.line_to_byte(last).map_err(failed)? == range.end {
        last -= 1;
    }
    let mut lines = Vec::new();
    for line in first..=last {
        let start = text.line_to_byte(line).map_err(failed)?.get();
        let content = text.line(line).map_err(failed)?;
        let content = content.trim_end_matches('\n');
        if content.trim().is_empty() {
            continue;
        }
        let indent = content.len() - content.trim_start().len();
        let commented = content[indent..].starts_with(token);
        let spaced = commented && content[indent + token.len()..].starts_with(' ');
        lines.push((start, indent, commented, spaced));
    }
    if lines.is_empty() {
        return Ok(());
    }
    let uncomment = lines.iter().all(|(_, _, commented, _)| *commented);
    let edits = if uncomment {
        lines
            .iter()
            .map(|(start, indent, _, spaced)| {
                let from = start + indent;
                let to = from + token.len() + usize::from(*spaced);
                ByteRange::new(ByteOffset(from), ByteOffset(to))
                    .map(|range| TextEdit::new(range, ""))
                    .map_err(failed)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let indent = lines
            .iter()
            .map(|(_, indent, _, _)| *indent)
            .min()
            .unwrap_or(0);
        lines
            .iter()
            .map(|(start, _, _, _)| {
                TextEdit::new(
                    ByteRange::empty(ByteOffset(start + indent)),
                    format!("{token} "),
                )
            })
            .collect()
    };
    let mut position = cursor.get();
    for edit in &edits {
        let (from, to) = (edit.range().start.get(), edit.range().end.get());
        if to <= cursor.get() {
            position = position + edit.replacement().len() - (to - from);
        } else if from < cursor.get() {
            position = position - (cursor.get() - from);
        }
    }
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(text.revision(), edits),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: ByteOffset(position),
    });
    transaction.operations.push(EditorOperation::SetMode {
        view,
        mode: EditorMode::Normal,
    });
    Ok(())
}
