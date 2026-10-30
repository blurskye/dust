use crate::editor::{
    state::{EditingPolicy, RegisterShape, SelectionKind, View},
    text::{
        ByteOffset, ByteRange, DisplayWidthPolicy, TextBuffer, byte_range_for_display_columns,
        display_column_at, insertion_at_display_column, next_grapheme_boundary,
        previous_grapheme_boundary,
    },
};

use super::{
    super::{CommandError, CommandInvocation},
    text_object::text_object,
    transaction::{failed, integer_argument, string_argument},
};

pub(super) fn motion_target(
    text: &TextBuffer,
    cursor: ByteOffset,
    motion: &str,
    count: usize,
    character: Option<char>,
    policy: EditingPolicy,
    wanted_column: Option<usize>,
) -> Result<ByteOffset, CommandError> {
    let mut target = cursor;
    match motion {
        "find_forward"
        | "till_forward"
        | "till_forward_again"
        | "find_backward"
        | "till_backward"
        | "till_backward_again" => {
            let character = character.ok_or_else(|| {
                CommandError::InvalidArguments(format!("motion `{motion}` needs a character"))
            })?;
            target = find_on_line(text, cursor, motion, character, count)?;
        }
        "left" => {
            for _ in 0..count {
                let previous = previous_grapheme_boundary(text, target);
                if text
                    .slice(ByteRange::new(previous, target).map_err(failed)?)
                    .map_err(failed)?
                    == "\n"
                {
                    break;
                }
                target = previous;
            }
        }
        "right" => {
            for _ in 0..count {
                let next = next_grapheme_boundary(text, target);
                if next == target
                    || text
                        .slice(ByteRange::new(target, next).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                target = next;
            }
        }
        "up" | "down" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let column = match wanted_column {
                Some(column) => column,
                None => cursor_display_column(text, cursor, policy)?,
            };
            let destination = if motion == "up" {
                line.saturating_sub(count)
            } else {
                line.saturating_add(count)
                    .min(text.visible_lines().saturating_sub(1).max(line))
            };
            let destination_start = text.line_to_byte(destination).map_err(failed)?;
            let destination_line = text.line(destination).map_err(failed)?;
            let destination_content = destination_line
                .strip_suffix('\n')
                .unwrap_or(&destination_line);
            let (relative, _) =
                insertion_at_display_column(destination_content, column, width_policy);
            target = ByteOffset(destination_start.get().saturating_add(relative));
        }
        "line_start" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            target = text.line_to_byte(line).map_err(failed)?;
        }
        "first_nonblank" => {
            target = first_nonblank(text, text.byte_to_line(cursor).map_err(failed)?)?;
        }
        "line_end" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            let end = line_content_end(text, line)?;
            target = if end == text.line_to_byte(line).map_err(failed)? {
                end
            } else {
                previous_grapheme_boundary(text, end)
            };
        }
        "word_forward" => {
            for _ in 0..count {
                target = word_forward(text, target);
            }
        }
        "word_backward" => {
            for _ in 0..count {
                target = word_backward(text, target);
            }
        }
        "big_word_forward" => {
            for _ in 0..count {
                target = big_word_forward(text, target);
            }
        }
        "big_word_backward" => {
            for _ in 0..count {
                target = big_word_backward(text, target);
            }
        }
        "word_end" => {
            for index in 0..count {
                if index > 0 {
                    target = next_grapheme_boundary(text, target);
                }
                target = word_end(text, target);
            }
        }
        "big_word_end" => {
            for index in 0..count {
                if index > 0 {
                    target = next_grapheme_boundary(text, target);
                }
                target = big_word_end(text, target);
            }
        }
        "matching_pair" => target = matching_pair(text, target)?,
        "document_start" => target = ByteOffset(0),
        "line_number" => {
            let last = text.visible_lines().saturating_sub(1);
            target = first_nonblank(text, count.saturating_sub(1).min(last))?;
        }
        // `G` lands on the last line's first non-blank character, as in Vim.
        "document_end" => {
            target = first_nonblank(text, text.visible_lines().saturating_sub(1))?;
        }
        _ => {
            return Err(CommandError::InvalidArguments(format!(
                "unknown motion `{motion}`"
            )));
        }
    }
    Ok(target)
}

pub(super) fn block_start_column(
    text: &TextBuffer,
    view: &View,
    policy: EditingPolicy,
) -> Result<usize, CommandError> {
    let width_policy = DisplayWidthPolicy {
        tab_width: policy.tab_width,
        ambiguous_wide: policy.ambiguous_width_wide,
    };
    let column_at = |offset: ByteOffset| -> Result<usize, CommandError> {
        let line = text.byte_to_line(offset).map_err(failed)?;
        let start = text.line_to_byte(line).map_err(failed)?;
        let line_text = text.line(line).map_err(failed)?;
        let content = line_text.strip_suffix('\n').unwrap_or(&line_text);
        Ok(display_column_at(
            content,
            offset.get().saturating_sub(start.get()),
            width_policy,
        ))
    };
    if let Some(selection) = view
        .selection
        .filter(|selection| selection.kind == SelectionKind::Block)
    {
        Ok(column_at(selection.anchor)?.min(column_at(selection.head)?))
    } else {
        column_at(view.cursor)
    }
}

pub(super) fn repeat_visual_ranges(
    text: &TextBuffer,
    view: &View,
    invocation: &CommandInvocation,
    policy: EditingPolicy,
) -> Result<(Vec<ByteRange>, RegisterShape), CommandError> {
    let shape = string_argument(invocation, "shape")?;
    let width = usize::try_from(integer_argument(invocation, "width")?)
        .map_err(|_| CommandError::InvalidArguments("visual repeat width is invalid".into()))?;
    let height = usize::try_from(integer_argument(invocation, "height")?)
        .map_err(|_| CommandError::InvalidArguments("visual repeat height is invalid".into()))?;
    if width == 0 || height == 0 {
        return Err(CommandError::InvalidArguments(
            "visual repeat dimensions must be positive".into(),
        ));
    }
    match shape {
        "character" => {
            let mut end = view.cursor;
            for _ in 0..width {
                let next = next_grapheme_boundary(text, end);
                if next == end {
                    break;
                }
                end = next;
            }
            Ok((
                vec![ByteRange::new(view.cursor, end).map_err(failed)?],
                RegisterShape::Character,
            ))
        }
        "line" => {
            let first_line = text.byte_to_line(view.cursor).map_err(failed)?;
            let start = text.line_to_byte(first_line).map_err(failed)?;
            let after = first_line.saturating_add(height);
            let end = if after < text.len_lines() {
                text.line_to_byte(after).map_err(failed)?
            } else {
                ByteOffset(text.len_bytes())
            };
            Ok((
                vec![ByteRange::new(start, end).map_err(failed)?],
                RegisterShape::Line,
            ))
        }
        "block" => {
            let first_line = text.byte_to_line(view.cursor).map_err(failed)?;
            let line_start = text.line_to_byte(first_line).map_err(failed)?;
            let line = text.line(first_line).map_err(failed)?;
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let first_column = display_column_at(
                content,
                view.cursor.get().saturating_sub(line_start.get()),
                width_policy,
            );
            let last_column = first_column.saturating_add(width.saturating_sub(1));
            let mut ranges = Vec::new();
            for line_index in first_line..first_line.saturating_add(height).min(text.len_lines()) {
                let start = text.line_to_byte(line_index).map_err(failed)?;
                let line = text.line(line_index).map_err(failed)?;
                let content = line.strip_suffix('\n').unwrap_or(&line);
                if let Some((relative_start, relative_end)) =
                    byte_range_for_display_columns(content, first_column, last_column, width_policy)
                {
                    ranges.push(
                        ByteRange::new(
                            ByteOffset(start.get().saturating_add(relative_start)),
                            ByteOffset(start.get().saturating_add(relative_end)),
                        )
                        .map_err(failed)?,
                    );
                } else {
                    ranges.push(ByteRange::empty(ByteOffset(
                        start.get().saturating_add(content.len()),
                    )));
                }
            }
            Ok((ranges, RegisterShape::Block))
        }
        _ => Err(CommandError::InvalidArguments(format!(
            "unknown visual repeat shape `{shape}`"
        ))),
    }
}

pub(super) fn visual_ranges(
    text: &TextBuffer,
    view: &View,
    policy: EditingPolicy,
) -> Result<(Vec<ByteRange>, RegisterShape), CommandError> {
    let selection = view
        .selection
        .ok_or_else(|| CommandError::Unavailable("visual selection is missing".into()))?;
    let low = selection.anchor.min(selection.head);
    let high = selection.anchor.max(selection.head);
    match selection.kind {
        SelectionKind::Character => {
            let end = next_grapheme_boundary(text, high);
            Ok((
                vec![ByteRange::new(low, end).map_err(failed)?],
                RegisterShape::Character,
            ))
        }
        SelectionKind::Line => {
            let first_line = text.byte_to_line(low).map_err(failed)?;
            let last_line = text.byte_to_line(high).map_err(failed)?;
            let start = text.line_to_byte(first_line).map_err(failed)?;
            let end = if last_line + 1 < text.len_lines() {
                text.line_to_byte(last_line + 1).map_err(failed)?
            } else {
                ByteOffset(text.len_bytes())
            };
            Ok((
                vec![ByteRange::new(start, end).map_err(failed)?],
                RegisterShape::Line,
            ))
        }
        SelectionKind::Block => {
            let anchor_line = text.byte_to_line(selection.anchor).map_err(failed)?;
            let head_line = text.byte_to_line(selection.head).map_err(failed)?;
            let anchor_start = text.line_to_byte(anchor_line).map_err(failed)?;
            let head_start = text.line_to_byte(head_line).map_err(failed)?;
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let anchor_text = text.line(anchor_line).map_err(failed)?;
            let anchor_content = anchor_text.strip_suffix('\n').unwrap_or(&anchor_text);
            let head_text = text.line(head_line).map_err(failed)?;
            let head_content = head_text.strip_suffix('\n').unwrap_or(&head_text);
            let anchor_column = display_column_at(
                anchor_content,
                selection.anchor.get().saturating_sub(anchor_start.get()),
                width_policy,
            );
            let head_column = display_column_at(
                head_content,
                selection.head.get().saturating_sub(head_start.get()),
                width_policy,
            );
            let first_column = anchor_column.min(head_column);
            let last_column = anchor_column.max(head_column);
            let mut ranges = Vec::new();
            for line_index in anchor_line.min(head_line)..=anchor_line.max(head_line) {
                let line_start = text.line_to_byte(line_index).map_err(failed)?;
                let line = text.line(line_index).map_err(failed)?;
                let content = line.strip_suffix('\n').unwrap_or(&line);
                if let Some((relative_start, relative_end)) =
                    byte_range_for_display_columns(content, first_column, last_column, width_policy)
                {
                    ranges.push(
                        ByteRange::new(
                            ByteOffset(line_start.get().saturating_add(relative_start)),
                            ByteOffset(line_start.get().saturating_add(relative_end)),
                        )
                        .map_err(failed)?,
                    );
                } else {
                    ranges.push(ByteRange::empty(ByteOffset(
                        line_start.get().saturating_add(content.len()),
                    )));
                }
            }
            Ok((ranges, RegisterShape::Block))
        }
    }
}

pub(super) fn operator_range(
    text: &TextBuffer,
    objects: &[crate::language::syntax::SyntaxObject],
    cursor: ByteOffset,
    motion: &str,
    count: usize,
    linewise: bool,
    character: Option<char>,
    policy: EditingPolicy,
) -> Result<ByteRange, CommandError> {
    if let Some(range) = text_object(text, objects, cursor, motion, count)? {
        return Ok(range);
    }
    if linewise {
        // `dd` covers `count` lines; `dj`, `dk`, `dG`… cover every line up to the target.
        let cursor_line = text.byte_to_line(cursor).map_err(failed)?;
        let (line, last) = if motion == "line" {
            (cursor_line, cursor_line + count.saturating_sub(1))
        } else {
            let target = motion_target(text, cursor, motion, count, character, policy, None)?;
            let target_line = text.byte_to_line(target).map_err(failed)?;
            (cursor_line.min(target_line), cursor_line.max(target_line))
        };
        let start = text.line_to_byte(line).map_err(failed)?;
        let after = last.saturating_add(1);
        let end = if after < text.len_lines() {
            text.line_to_byte(after).map_err(failed)?
        } else {
            ByteOffset(text.len_bytes())
        };
        return ByteRange::new(start, end).map_err(failed);
    }
    if motion == "right" {
        // `l` is exclusive, but `dl` on the last character still takes it, as in Vim.
        let mut end = cursor;
        for _ in 0..count {
            let next = next_grapheme_boundary(text, end);
            if next == end
                || text
                    .slice(ByteRange::new(end, next).map_err(failed)?)
                    .map_err(failed)?
                    == "\n"
            {
                break;
            }
            end = next;
        }
        return ByteRange::new(cursor, end).map_err(failed);
    }
    let target = motion_target(text, cursor, motion, count, character, policy, None)?;
    let (start, mut end) = if target < cursor {
        (target, cursor)
    } else {
        (cursor, target)
    };
    if target >= cursor
        && matches!(
            motion,
            "word_end"
                | "big_word_end"
                | "line_end"
                | "matching_pair"
                | "find_forward"
                | "till_forward"
                | "till_forward_again"
        )
    {
        end = next_grapheme_boundary(text, end);
    }
    ByteRange::new(start, end).map_err(failed)
}

/// `f`/`t` search the cursor line only, like Vim. `till_*_again` is the `;` repeat of a
/// till motion, which skips the character the cursor already stands next to.
fn find_on_line(
    text: &TextBuffer,
    cursor: ByteOffset,
    motion: &str,
    character: char,
    count: usize,
) -> Result<ByteOffset, CommandError> {
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let start = text.line_to_byte(line).map_err(failed)?.get();
    let line_text = text.line(line).map_err(failed)?;
    let content = line_text.strip_suffix('\n').unwrap_or(&line_text);
    let relative = cursor.get().saturating_sub(start).min(content.len());
    let forward = motion.starts_with("find_forward") || motion.starts_with("till_forward");
    let till = motion.starts_with("till");
    let candidates: Vec<usize> = if forward {
        content
            .char_indices()
            .filter(|(index, found)| *index > relative && *found == character)
            .map(|(index, _)| index)
            .collect()
    } else {
        content[..relative]
            .char_indices()
            .rev()
            .filter(|(_, found)| *found == character)
            .map(|(index, _)| index)
            .collect()
    };
    let neighbour = if forward {
        content[relative..]
            .chars()
            .next()
            .map(|current| relative + current.len_utf8())
    } else {
        content[..relative]
            .chars()
            .next_back()
            .map(|previous| relative - previous.len_utf8())
    };
    let skip = usize::from(motion.ends_with("_again") && candidates.first() == neighbour.as_ref());
    let index = candidates
        .get(skip + count.max(1) - 1)
        .copied()
        .ok_or_else(|| CommandError::Unavailable(format!("`{character}` not found on the line")))?;
    let at = ByteOffset(start + index);
    Ok(match (till, forward) {
        (false, _) => at,
        (true, true) => previous_grapheme_boundary(text, at),
        (true, false) => next_grapheme_boundary(text, at),
    })
}

pub(super) fn line_content_end(text: &TextBuffer, line: usize) -> Result<ByteOffset, CommandError> {
    let start = text.line_to_byte(line).map_err(failed)?;
    let value = text.line(line).map_err(failed)?;
    let content_len = value.strip_suffix('\n').map_or(value.len(), str::len);
    Ok(ByteOffset(start.get() + content_len))
}

pub(super) fn first_nonblank(text: &TextBuffer, line: usize) -> Result<ByteOffset, CommandError> {
    let start = text.line_to_byte(line).map_err(failed)?;
    let value = text.line(line).map_err(failed)?;
    let relative = value
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(0, |(index, _)| index);
    Ok(ByteOffset(start.get() + relative))
}

pub(super) fn word_class(character: char) -> u8 {
    if character.is_whitespace() {
        0
    } else if character.is_alphanumeric() || character == '_' {
        1
    } else {
        2
    }
}

/// In Normal mode the cursor sits on a character, so a line shorter than the wanted column
/// puts it on its last one.
pub(super) fn on_last_character(text: &TextBuffer, offset: ByteOffset) -> Result<ByteOffset, CommandError> {
    let line = text.byte_to_line(offset).map_err(failed)?;
    let start = text.line_to_byte(line).map_err(failed)?;
    let end = line_content_end(text, line)?;
    Ok(if offset >= end && end > start {
        previous_grapheme_boundary(text, end)
    } else {
        offset
    })
}

/// The cursor's screen column within its line, which `j` and `k` try to keep.
pub(super) fn cursor_display_column(
    text: &TextBuffer,
    cursor: ByteOffset,
    policy: EditingPolicy,
) -> Result<usize, CommandError> {
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let start = text.line_to_byte(line).map_err(failed)?;
    let content = text.line(line).map_err(failed)?;
    let content = content.strip_suffix('\n').unwrap_or(&content);
    Ok(display_column_at(
        content,
        cursor.get().saturating_sub(start.get()),
        DisplayWidthPolicy {
            tab_width: policy.tab_width,
            ambiguous_wide: policy.ambiguous_width_wide,
        },
    ))
}

pub(super) fn word_forward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    if cursor.get() >= source.len() {
        return cursor;
    }
    let mut iter = source[cursor.get()..].char_indices();
    let current = iter
        .next()
        .map_or(0, |(_, character)| word_class(character));
    let mut seen_other = false;
    for (relative, character) in iter {
        let class = word_class(character);
        if !seen_other && class != current {
            seen_other = true;
        }
        if seen_other && class != 0 {
            return ByteOffset(cursor.get() + relative);
        }
    }
    ByteOffset(source.len())
}

pub(super) fn big_word_forward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let starts_in_space = source[cursor.get()..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace);
    let mut separator_seen = starts_in_space;
    for (relative, character) in source[cursor.get()..].char_indices() {
        if character.is_whitespace() {
            separator_seen = true;
        } else if separator_seen {
            return ByteOffset(cursor.get() + relative);
        }
    }
    ByteOffset(source.len())
}

pub(super) fn big_word_backward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut target = 0;
    let mut seen_nonspace = false;
    for (index, character) in source[..cursor.get()].char_indices().rev() {
        if character.is_whitespace() {
            if seen_nonspace {
                break;
            }
        } else {
            target = index;
            seen_nonspace = true;
        }
    }
    ByteOffset(target)
}

pub(super) fn word_backward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let before = &source[..cursor.get()];
    let mut iter = before.char_indices().rev();
    let Some((mut target, mut character)) = iter.next() else {
        return ByteOffset(0);
    };
    while character.is_whitespace() {
        let Some((index, next)) = iter.next() else {
            return ByteOffset(0);
        };
        target = index;
        character = next;
    }
    let class = word_class(character);
    for (index, previous) in iter {
        if word_class(previous) != class {
            break;
        }
        target = index;
    }
    ByteOffset(target)
}

pub(super) fn big_word_end(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut end = cursor.get();
    let mut seen_nonspace = false;
    for (relative, character) in source[cursor.get()..].char_indices() {
        if character.is_whitespace() {
            if seen_nonspace {
                break;
            }
        } else {
            seen_nonspace = true;
            end = cursor.get() + relative;
        }
    }
    ByteOffset(end)
}

pub(super) fn matching_pair(
    text: &TextBuffer,
    cursor: ByteOffset,
) -> Result<ByteOffset, CommandError> {
    let source = text.to_string();
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let line_end = line_content_end(text, line)?.get();
    let (at, character) = source[cursor.get()..line_end]
        .char_indices()
        .find_map(|(relative, character)| {
            "()[]{}"
                .contains(character)
                .then_some((cursor.get() + relative, character))
        })
        .ok_or_else(|| CommandError::Unavailable("no matching pair on line".into()))?;
    let (mate, forward) = match character {
        '(' => (')', true),
        '[' => (']', true),
        '{' => ('}', true),
        ')' => ('(', false),
        ']' => ('[', false),
        '}' => ('{', false),
        _ => unreachable!(),
    };
    let mut depth = 0_usize;
    if forward {
        for (relative, candidate) in source[at..].char_indices() {
            if candidate == character {
                depth += 1;
            } else if candidate == mate {
                depth -= 1;
                if depth == 0 {
                    return Ok(ByteOffset(at + relative));
                }
            }
        }
    } else {
        for (index, candidate) in source[..=at].char_indices().rev() {
            if candidate == character {
                depth += 1;
            } else if candidate == mate {
                depth -= 1;
                if depth == 0 {
                    return Ok(ByteOffset(index));
                }
            }
        }
    }
    Err(CommandError::Unavailable("matching pair not found".into()))
}

pub(super) fn word_end(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut iter = source[cursor.get()..].char_indices().peekable();
    while iter
        .peek()
        .is_some_and(|(_, character)| character.is_whitespace())
    {
        iter.next();
    }
    let Some((first, character)) = iter.next() else {
        return ByteOffset(source.len());
    };
    let class = word_class(character);
    let mut end = cursor.get() + first;
    for (relative, next) in iter {
        if word_class(next) != class {
            break;
        }
        end = cursor.get() + relative;
    }
    ByteOffset(end)
}
