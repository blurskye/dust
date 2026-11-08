//! Vim text objects (`iw`, `a(`, `i"`, `ip`, …). They are motions whose result is a
//! range rather than a cursor target, so operators and Visual mode share them.

use unicode_segmentation::UnicodeSegmentation;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::SyntaxObject;
use crate::editor::SyntaxObjectKind;
use crate::editor::text::TextBuffer;

use crate::editor::command::CommandError;

use crate::editor::command::builtins::motion::word_class;

use crate::editor::command::builtins::transaction::failed;

/// Returns `None` when `motion` is not a text object.
pub(super) fn text_object(
    text: &TextBuffer,
    objects: &[SyntaxObject],
    cursor: ByteOffset,
    motion: &str,
    count: usize,
) -> Result<Option<ByteRange>, CommandError> {
    let (around, kind) = match motion.split_once('_') {
        Some(("inner", kind)) => (false, kind),
        Some(("a", kind)) => (true, kind),
        _ => return Ok(None),
    };
    let mut range = match kind {
        "word" => word(text, cursor, around, false)?,
        "big_word" => word(text, cursor, around, true)?,
        "paragraph" => paragraph(text, cursor, around)?,
        "paren" => pair(text, cursor, '(', ')', around, count)?,
        "bracket" => pair(text, cursor, '[', ']', around, count)?,
        "brace" => pair(text, cursor, '{', '}', around, count)?,
        "angle" => pair(text, cursor, '<', '>', around, count)?,
        "double_quote" => quote(text, cursor, '"', around)?,
        "single_quote" => quote(text, cursor, '\'', around)?,
        "backtick" => quote(text, cursor, '`', around)?,
        "function" => syntax(objects, SyntaxObjectKind::Function, cursor, around, count),
        "class" => syntax(objects, SyntaxObjectKind::Class, cursor, around, count),
        "argument" => syntax(objects, SyntaxObjectKind::Argument, cursor, around, count),
        "comment" => syntax(objects, SyntaxObjectKind::Comment, cursor, around, count),
        _ => return Ok(None),
    };
    if count > 1 && matches!(kind, "word" | "big_word" | "paragraph") {
        let source = text.to_string();
        for _ in 1..count {
            let Some(previous) = range else {
                break;
            };
            let next = source[previous.end.get()..]
                .char_indices()
                .find(|(_, character)| {
                    if around || kind == "paragraph" {
                        !character.is_whitespace()
                    } else {
                        *character != '\n'
                    }
                })
                .map(|(offset, _)| ByteOffset(previous.end.get() + offset));
            let Some(next) = next else {
                break;
            };
            let following = if kind == "paragraph" {
                paragraph(text, next, around)?
            } else {
                word_in(
                    &source[next.get()..],
                    next.get(),
                    next,
                    around,
                    kind == "big_word",
                )?
            };
            let Some(following) = following.filter(|following| following.end > previous.end) else {
                break;
            };
            range = Some(ByteRange::new(previous.start, following.end).map_err(failed)?);
        }
    }
    range
        .map(Some)
        .ok_or_else(|| CommandError::Unavailable("no matching text object at the cursor".into()))
}

/// The `count`-th smallest tree-sitter object of `kind` around the cursor, or the next one
/// after it when none surrounds it, like mini.ai.
fn syntax(
    objects: &[SyntaxObject],
    kind: SyntaxObjectKind,
    cursor: ByteOffset,
    around: bool,
    count: usize,
) -> Option<ByteRange> {
    let cursor = cursor.get();
    let mut enclosing = objects
        .iter()
        .filter(|object| object.kind == kind && object.outer.0 <= cursor && cursor < object.outer.1)
        .collect::<Vec<_>>();
    enclosing.sort_by_key(|object| object.outer.1 - object.outer.0);
    let object = if enclosing.is_empty() {
        objects
            .iter()
            .filter(|object| object.kind == kind && object.outer.0 > cursor)
            .min_by_key(|object| object.outer.0)
    } else {
        enclosing.get(count.max(1) - 1).copied()
    }?;
    let (start, end) = if around { object.outer } else { object.inner };
    ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()
}

/// The pair characters a surround or text-object key names, `b`/`B` included.
pub(super) fn pair_for(character: char) -> Option<(char, char)> {
    match character {
        '(' | ')' | 'b' => Some(('(', ')')),
        '[' | ']' => Some(('[', ']')),
        '{' | '}' | 'B' => Some(('{', '}')),
        '<' | '>' => Some(('<', '>')),
        '"' | '\'' | '`' => Some((character, character)),
        _ => None,
    }
}

/// The range of the pair named by `character` around the cursor, including the pair.
pub(super) fn surrounding(
    text: &TextBuffer,
    cursor: ByteOffset,
    character: char,
) -> Result<Option<ByteRange>, CommandError> {
    match pair_for(character) {
        Some((open, close)) if open == close => quote(text, cursor, open, true),
        Some((open, close)) => pair(text, cursor, open, close, true, 1),
        None => quote(text, cursor, character, true),
    }
}

fn line_bounds(text: &TextBuffer, cursor: ByteOffset) -> Result<(usize, String), CommandError> {
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let start = text.line_to_byte(line).map_err(failed)?.get();
    let mut content = text.line(line).map_err(failed)?;
    if content.ends_with('\n') {
        content.pop();
    }
    Ok((start, content))
}

fn word(
    text: &TextBuffer,
    cursor: ByteOffset,
    around: bool,
    big: bool,
) -> Result<Option<ByteRange>, CommandError> {
    let (start, content) = line_bounds(text, cursor)?;
    word_in(&content, start, cursor, around, big)
}

fn word_in(
    content: &str,
    start: usize,
    cursor: ByteOffset,
    around: bool,
    big: bool,
) -> Result<Option<ByteRange>, CommandError> {
    let class = |grapheme: &str| {
        let character = grapheme.chars().next().unwrap_or(' ');
        if big {
            u8::from(!character.is_whitespace())
        } else {
            word_class(character)
        }
    };
    let relative = cursor.get().saturating_sub(start);
    let mut characters = content
        .grapheme_indices(true)
        .take_while(|(_, grapheme)| *grapheme != "\n")
        .peekable();
    let Some((mut first, grapheme)) = characters.next() else {
        return Ok(Some(ByteRange::empty(cursor)));
    };
    let mut end = first + grapheme.len();
    let mut current = class(grapheme);
    let mut leading = first;
    while characters
        .peek()
        .is_some_and(|(index, _)| *index <= relative)
    {
        let (index, grapheme) = characters.next().expect("peeked grapheme");
        let next = class(grapheme);
        if next != current {
            leading = if current == 0 { first } else { index };
            first = index;
            current = next;
        }
        end = index + grapheme.len();
    }
    while let Some((index, grapheme)) =
        characters.next_if(|(_, grapheme)| class(grapheme) == current)
    {
        end = index + grapheme.len();
    }
    if around {
        if let Some((_, grapheme)) = characters.peek()
            && (current == 0 || class(grapheme) == 0)
        {
            let next = class(grapheme);
            while let Some((index, grapheme)) =
                characters.next_if(|(_, grapheme)| class(grapheme) == next)
            {
                end = index + grapheme.len();
            }
        } else if current != 0 {
            first = leading;
        }
    }
    ByteRange::new(ByteOffset(start + first), ByteOffset(start + end))
        .map(Some)
        .map_err(failed)
}

fn paragraph(
    text: &TextBuffer,
    cursor: ByteOffset,
    around: bool,
) -> Result<Option<ByteRange>, CommandError> {
    let lines = text.len_lines();
    let blank = |line: usize| -> Result<bool, CommandError> {
        Ok(text.line(line).map_err(failed)?.trim().is_empty())
    };
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let kind = blank(line)?;
    let (mut first, mut last) = (line, line);
    while first > 0 && blank(first - 1)? == kind {
        first -= 1;
    }
    while last + 1 < lines && blank(last + 1)? == kind {
        last += 1;
    }
    if around {
        if last + 1 < lines {
            let next = blank(last + 1)?;
            while last + 1 < lines && blank(last + 1)? == next {
                last += 1;
            }
        } else if first > 0 {
            let previous = blank(first - 1)?;
            while first > 0 && blank(first - 1)? == previous {
                first -= 1;
            }
        }
    }
    let start = text.line_to_byte(first).map_err(failed)?;
    let end = if last + 1 < lines {
        text.line_to_byte(last + 1).map_err(failed)?
    } else {
        ByteOffset(text.len_bytes())
    };
    ByteRange::new(start, end).map(Some).map_err(failed)
}

/// Brackets nest and may span lines. When the cursor is not inside a pair, the next pair
/// on the cursor line is used, as in Vim and mini.ai.
fn pair(
    text: &TextBuffer,
    cursor: ByteOffset,
    open: char,
    close: char,
    around: bool,
    count: usize,
) -> Result<Option<ByteRange>, CommandError> {
    let source = text.to_string();
    let at = cursor.get().min(source.len());
    // On a closing bracket the scan below finds its own opening bracket first.
    let mut wanted = count.max(1);
    let mut opening = None;
    if source[at..].starts_with(open) {
        wanted -= 1;
        if wanted == 0 {
            opening = Some(at);
        }
    }
    if opening.is_none() {
        let mut depth = 0_usize;
        for (index, character) in source[..at].char_indices().rev() {
            if character == close {
                depth += 1;
            } else if character == open {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                wanted -= 1;
                if wanted == 0 {
                    opening = Some(index);
                    break;
                }
            }
        }
    }
    let opening = match opening {
        Some(opening) => opening,
        None if wanted < count.max(1) => return Ok(None),
        None => {
            let (start, content) = line_bounds(text, cursor)?;
            let relative = at.saturating_sub(start);
            match content[relative.min(content.len())..].find(open) {
                Some(found) => start + relative + found,
                None => return Ok(None),
            }
        }
    };
    let mut depth = 0_usize;
    let mut closing = None;
    for (index, character) in source[opening..].char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                closing = Some(opening + index);
                break;
            }
        }
    }
    let Some(closing) = closing else {
        return Ok(None);
    };
    if around {
        return ByteRange::new(ByteOffset(opening), ByteOffset(closing + close.len_utf8()))
            .map(Some)
            .map_err(failed);
    }
    let mut start = opening + open.len_utf8();
    let mut end = closing;
    // A block like `{\n    body\n}` keeps its braces on their own lines.
    if source[start..end].contains('\n') {
        if let Some(newline) = source[start..end].find('\n')
            && source[start..start + newline].trim().is_empty()
        {
            start += newline + 1;
        }
        if let Some(newline) = source[start..end].rfind('\n')
            && source[start + newline + 1..end].trim().is_empty()
        {
            end = start + newline + 1;
        }
    }
    ByteRange::new(ByteOffset(start), ByteOffset(end.max(start)))
        .map(Some)
        .map_err(failed)
}

/// Quotes pair up from the start of the line, skipping backslash-escaped quotes.
fn quote(
    text: &TextBuffer,
    cursor: ByteOffset,
    mark: char,
    around: bool,
) -> Result<Option<ByteRange>, CommandError> {
    let (start, content) = line_bounds(text, cursor)?;
    let relative = cursor.get().saturating_sub(start);
    let mut positions = Vec::new();
    let mut escaped = false;
    for (index, character) in content.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && mark != '\\' {
            escaped = true;
        } else if character == mark {
            positions.push(index);
        }
    }
    let pairs = positions
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect::<Vec<_>>();
    let Some((opening, closing)) = pairs
        .iter()
        .find(|(opening, closing)| *opening <= relative && relative <= *closing)
        .or_else(|| pairs.iter().find(|(opening, _)| *opening > relative))
        .copied()
    else {
        return Ok(None);
    };
    let (from, to) = if around {
        (opening, closing + mark.len_utf8())
    } else {
        (opening + mark.len_utf8(), closing)
    };
    ByteRange::new(ByteOffset(start + from), ByteOffset(start + to))
        .map(Some)
        .map_err(failed)
}
