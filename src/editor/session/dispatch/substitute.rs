//! `:s/old/new/g`. Patterns are the regex crate's syntax, the same as `/` search, plus Vim's
//! `\<` and `\>`; replacements take `&`, `\1` to `\9`, `\n` and `\t`.

use super::{
    ByteOffset, ByteRange, EditorMode, EditorOperation, EditorTransaction, InteractiveApplication,
    Precondition, TextEdit, TextEditSet,
};
use crate::editor::text::TextBuffer;

/// A replacement in regex-crate template form.
fn replacement_template(replacement: &str) -> String {
    let mut template = String::new();
    let mut characters = replacement.chars();
    while let Some(character) = characters.next() {
        match character {
            '&' => template.push_str("${0}"),
            '$' => template.push_str("$$"),
            '\\' => match characters.next() {
                Some(digit) if digit.is_ascii_digit() => {
                    template.push_str(&format!("${{{digit}}}"));
                }
                Some('n') => template.push('\n'),
                Some('t') => template.push('\t'),
                Some(other) => template.push(other),
                None => template.push('\\'),
            },
            other => template.push(other),
        }
    }
    template
}

/// The zero-based lines a range covers, in order. `current` is the cursor's line, `selection` the
/// Visual lines and `last` the buffer's final line.
fn range_lines(
    range: &str,
    current: usize,
    selection: Option<(usize, usize)>,
    last: usize,
) -> Result<(usize, usize), String> {
    let address = |text: &str| match text {
        "." => Ok(current),
        "$" => Ok(last),
        number => number
            .parse::<usize>()
            .map(|line| line.saturating_sub(1).min(last))
            .map_err(|_| format!("invalid range `{range}`")),
    };
    let (start, end) = match range {
        "" => (current, current),
        "%" => (0, last),
        "'<,'>" => selection.ok_or("no Visual selection")?,
        range => match range.split_once(',') {
            Some((start, end)) => (address(start)?, address(end)?),
            None => {
                let line = address(range)?;
                (line, line)
            }
        },
    };
    Ok((start.min(end), start.max(end)))
}

/// The edits for lines `start..=end`, and how many lines changed.
fn substitution_edits(
    text: &TextBuffer,
    (start, end): (usize, usize),
    pattern: &str,
    replacement: &str,
    flags: &str,
) -> Result<(Vec<TextEdit>, usize), String> {
    let pattern = pattern.replace(r"\<", r"\b").replace(r"\>", r"\b");
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(flags.contains('i'))
        .build()
        .map_err(|error| error.to_string())?;
    let template = replacement_template(replacement);
    let every = flags.contains('g');
    let mut edits = Vec::new();
    let mut changed = 0;
    for number in start..=end {
        let line = text.line(number).map_err(|error| error.to_string())?;
        let line_start = text.line_to_byte(number).map_err(|error| error.to_string())?.get();
        let body = line.trim_end_matches(['\n', '\r']);
        let mut touched = false;
        for captures in regex.captures_iter(body) {
            let found = captures.get(0).expect("a match has its whole text");
            let mut replaced = String::new();
            captures.expand(&template, &mut replaced);
            let range = ByteRange::new(
                ByteOffset(line_start + found.start()),
                ByteOffset(line_start + found.end()),
            )
            .map_err(|error| error.to_string())?;
            edits.push(TextEdit::new(range, replaced));
            touched = true;
            if !every {
                break;
            }
        }
        changed += usize::from(touched);
    }
    Ok((edits, changed))
}

impl InteractiveApplication {
    pub(super) fn run_substitute(
        &mut self,
        range: &str,
        pattern: &str,
        replacement: &str,
        flags: &str,
    ) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let text = &self.state.buffers[buffer_id].text;
        let revision = text.revision();
        let line_of = |offset| text.byte_to_line(offset).unwrap_or(0);
        let selection = view
            .selection
            .map(|selection| (line_of(selection.anchor), line_of(selection.head)));
        let last = text.visible_lines().saturating_sub(1);
        let result = range_lines(range, line_of(view.cursor), selection, last).and_then(|lines| {
            substitution_edits(text, lines, pattern, replacement, flags)
        });
        let (edits, lines) = match result {
            Ok(found) => found,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if edits.is_empty() {
            self.show_error(format!("pattern not found: {pattern}"));
            return;
        }
        let count = edits.len();
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, edits),
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        if selection.is_some() {
            self.set_mode(EditorMode::Normal);
        }
        self.notify_message(format!("{count} substitutions on {lines} lines"), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(text: &str, lines: (usize, usize), pattern: &str, with: &str, flags: &str) -> String {
        let buffer = TextBuffer::from_text(text);
        let (edits, _) = substitution_edits(&buffer, lines, pattern, with, flags).unwrap();
        let mut out = text.to_owned();
        for edit in edits.into_iter().rev() {
            let range = edit.range();
            out.replace_range(range.start.get()..range.end.get(), edit.replacement());
        }
        out
    }

    #[test]
    fn substitutes_first_per_line_or_every_match_with_captures() {
        assert_eq!(applied("a a\na a\n", (0, 1), "a", "b", ""), "b a\nb a\n");
        assert_eq!(applied("a a\na a\n", (0, 0), "a", "b", "g"), "b b\na a\n");
        assert_eq!(applied("foo bar\n", (0, 0), r"(\w+) (\w+)", r"\2 \1 &", ""), "bar foo foo bar\n");
        assert_eq!(applied("cat scatter\n", (0, 0), r"\<cat\>", "dog", "g"), "dog scatter\n");
        assert_eq!(applied("A\n", (0, 0), "a", "$1", "i"), "$1\n");
    }

    #[test]
    fn ranges_name_lines_from_one_and_the_cursor_and_the_end() {
        assert_eq!(range_lines("", 4, None, 9), Ok((4, 4)));
        assert_eq!(range_lines("%", 4, None, 9), Ok((0, 9)));
        assert_eq!(range_lines("2,3", 4, None, 9), Ok((1, 2)));
        assert_eq!(range_lines(".,$", 4, None, 9), Ok((4, 9)));
        assert_eq!(range_lines("'<,'>", 4, Some((6, 2)), 9), Ok((2, 6)));
        assert!(range_lines("'<,'>", 4, None, 9).is_err());
    }
}
