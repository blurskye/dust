//! `gc` line comments. Blank lines are left alone; if every other line in the range is
//! already commented the range is uncommented, otherwise the comment string goes in at
//! the smallest indent, as in Neovim. A comment string with `%s`, like `<!-- %s -->`, wraps
//! each line in what comes before and after it.

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

use crate::editor::command::builtins::transaction::failed;

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
    let (token, suffix) = match token.split_once("%s") {
        Some((before, after)) => (before.trim(), after.trim()),
        None => (token, ""),
    };
    let mut lines = Vec::new();
    for line in first..=last {
        let start = text.line_to_byte(line).map_err(failed)?.get();
        let content = text.line(line).map_err(failed)?;
        let content = content.trim_end_matches('\n');
        if content.trim().is_empty() {
            continue;
        }
        let indent = content.len() - content.trim_start().len();
        let body = content[indent..].trim_end();
        let commented = body.starts_with(token)
            && body.ends_with(suffix)
            && body.len() >= token.len() + suffix.len();
        let spaced = commented && content[indent + token.len()..].starts_with(' ');
        // Where the closing part starts, with the space before it, when there is one.
        let closing = (commented && !suffix.is_empty()).then(|| {
            let end = indent + body.len() - suffix.len();
            let spaced_end = if body[..body.len() - suffix.len()].ends_with(' ')
                && end > indent + token.len() + usize::from(spaced)
            {
                end - 1
            } else {
                end
            };
            (start + spaced_end, start + indent + body.len())
        });
        lines.push((
            start,
            indent,
            commented,
            spaced,
            closing,
            start + content.len(),
            content[..indent].chars().count(),
        ));
    }
    if lines.is_empty() {
        transaction.operations.push(EditorOperation::SetMode {
            view,
            mode: EditorMode::Normal,
        });
        return Ok(());
    }
    let uncomment = lines.iter().all(|line| line.2);
    let edits = if uncomment {
        let mut edits = Vec::new();
        for (start, indent, _, spaced, closing, _, _) in &lines {
            let from = start + indent;
            let to = from + token.len() + usize::from(*spaced);
            edits.push(TextEdit::new(
                ByteRange::new(ByteOffset(from), ByteOffset(to)).map_err(failed)?,
                "",
            ));
            if let Some((from, to)) = closing {
                edits.push(TextEdit::new(
                    ByteRange::new(ByteOffset(*from), ByteOffset(*to)).map_err(failed)?,
                    "",
                ));
            }
        }
        edits
    } else {
        let indent = lines.iter().map(|line| line.6).min().unwrap_or(0);
        let mut edits = Vec::new();
        for (start, bytes, _, _, _, end, _) in &lines {
            let whitespace = text
                .slice(
                    ByteRange::new(ByteOffset(*start), ByteOffset(start + bytes))
                        .map_err(failed)?,
                )
                .map_err(failed)?;
            let offset = whitespace
                .chars()
                .take(indent)
                .map(char::len_utf8)
                .sum::<usize>();
            edits.push(TextEdit::new(
                ByteRange::empty(ByteOffset(start + offset)),
                format!("{token} "),
            ));
            if !suffix.is_empty() {
                edits.push(TextEdit::new(
                    ByteRange::empty(ByteOffset(*end)),
                    format!(" {suffix}"),
                ));
            }
        }
        edits
    };
    let mut position = cursor.get();
    for edit in &edits {
        let (from, to) = (edit.range().start.get(), edit.range().end.get());
        if to <= cursor.get() {
            position = position + edit.replacement().len() - (to - from);
        } else if from < cursor.get() {
            position -= cursor.get() - from;
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
