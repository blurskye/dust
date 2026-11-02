//! Markdown shown as styled text: the markers are dropped and what they meant becomes a
//! style, the way LazyVim's render-markdown shows it. Each source line stays its own line
//! and is wrapped at words. The agent transcript is drawn with it.

use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::language::syntax::{self, Highlight};

/// Code blocks larger than this are shown without highlighting.
const HIGHLIGHT_BYTE_CEILING: usize = 64 * 1024;

/// Text with the styled byte ranges in it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Styled {
    pub text: String,
    pub spans: Vec<(Range<usize>, Highlight)>,
}

impl Styled {
    pub fn push(&mut self, text: &str, role: Option<Highlight>) {
        let start = self.text.len();
        self.text.push_str(text);
        let Some(role) = role.filter(|_| !text.is_empty()) else {
            return;
        };
        match self.spans.last_mut() {
            Some((range, last)) if range.end == start && *last == role => range.end = self.text.len(),
            _ => self.spans.push((start..self.text.len(), role)),
        }
    }

    /// A line of `width` rule characters.
    pub fn rule(&mut self, width: usize) {
        self.push(&"─".repeat(width), Some(Highlight::COMMENT));
        self.push("\n", None);
    }

    /// Plain `text` wrapped at words to `width` cells. Its first line starts with `first`
    /// and every other line with `rest`.
    pub fn wrapped(&mut self, text: &str, role: Option<Highlight>, first: Prefix, rest: Prefix, width: usize) {
        for (index, line) in text.lines().enumerate() {
            let segments = [(line.to_owned(), role)];
            wrap(self, &segments, width, if index == 0 { first } else { rest }, rest);
        }
    }

    /// Styled runs wrapped at words to `width` cells.
    pub fn runs(&mut self, runs: &[(String, Option<Highlight>)], width: usize) {
        wrap(self, runs, width, ("", None), ("", None));
    }
}

/// What starts a line: indentation, a bullet or a quote bar, and its style.
pub type Prefix<'a> = (&'a str, Option<Highlight>);

type Segment = (String, Option<Highlight>);

/// Append `source` rendered for a column `width` cells wide.
pub fn render(out: &mut Styled, source: &str, width: usize) {
    let width = width.max(8);
    let lines = source.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        index += 1;
        if let Some((fence, info)) = fence_open(trimmed) {
            let mut body = Vec::new();
            while let Some(candidate) = lines.get(index) {
                index += 1;
                if is_fence_close(candidate.trim(), fence) {
                    break;
                }
                body.push(*candidate);
            }
            code_block(out, info, &body, width);
        } else if trimmed.starts_with('|') && lines.get(index).is_some_and(|next| is_table_separator(next)) {
            let mut rows = vec![trimmed];
            index += 1;
            while let Some(row) = lines.get(index).map(|row| row.trim()).filter(|row| row.starts_with('|')) {
                rows.push(row);
                index += 1;
            }
            table(out, &rows, width);
        } else if let Some(title) = heading(trimmed) {
            wrap(out, &inline(title, Some(Highlight::HEADING)), width, ("", None), ("", None));
        } else if is_rule(trimmed) {
            out.rule(width);
        } else if trimmed.starts_with('>') {
            let text = trimmed.trim_start_matches(['>', ' ']);
            let bar = ("▎ ", Some(Highlight::QUOTE));
            wrap(out, &inline(text, Some(Highlight::QUOTE)), width, bar, bar);
        } else if let Some((marker, text)) = list_item(trimmed, indent) {
            let continuation = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
            wrap(
                out,
                &inline(text, None),
                width,
                (marker.as_str(), Some(Highlight::KEYWORD)),
                (continuation.as_str(), None),
            );
        } else if trimmed.is_empty() {
            out.push("\n", None);
        } else {
            wrap(out, &inline(trimmed, None), width, (indent, None), (indent, None));
        }
    }
}

/// The fence and the info string after it, for a line that opens a code block.
fn fence_open(line: &str) -> Option<(&str, &str)> {
    let marker = line.chars().next().filter(|marker| matches!(*marker, '`' | '~'))?;
    let length = line.len() - line.trim_start_matches(marker).len();
    let info = line[length..].trim();
    (length >= 3 && !(marker == '`' && info.contains('`'))).then(|| (&line[..length], info))
}

fn is_fence_close(line: &str, fence: &str) -> bool {
    let marker = fence.chars().next().unwrap_or('`');
    line.len() >= fence.len() && line.chars().all(|character| character == marker)
}

fn heading(line: &str) -> Option<&str> {
    let level = line.len() - line.trim_start_matches('#').len();
    let rest = &line[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')))
        .then(|| rest.trim().trim_end_matches('#').trim_end())
}

fn is_rule(line: &str) -> bool {
    let mut marks = line.chars().filter(|character| *character != ' ');
    let Some(first) = marks.next().filter(|first| matches!(*first, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 1;
    for mark in marks {
        if mark != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn is_table_separator(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|')
        && line.contains('-')
        && line.chars().all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

/// The bullet or number that starts a list item, as shown, and the item's text.
fn list_item<'a>(line: &'a str, indent: &str) -> Option<(String, &'a str)> {
    let depth = indent.chars().map(|character| if character == '\t' { 4 } else { 1 }).sum::<usize>() / 2;
    let (marker, text) = if let Some(text) = ["- ", "* ", "+ "].iter().find_map(|bullet| line.strip_prefix(*bullet)) {
        (["•", "◦", "▪"][depth % 3].to_owned(), text)
    } else {
        let digits = line.len() - line.trim_start_matches(|character: char| character.is_ascii_digit()).len();
        let rest = &line[digits..];
        let text = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "));
        ((1..=9).contains(&digits)).then_some(())?;
        (format!("{}.", &line[..digits]), text?)
    };
    let (marker, text) = match text.get(..4) {
        Some("[ ] ") => ("○".to_owned(), &text[4..]),
        Some("[x] " | "[X] ") => ("✓".to_owned(), &text[4..]),
        Some("[~] " | "[-] ") => ("…".to_owned(), &text[4..]),
        _ => (marker, text),
    };
    Some((format!("{}{marker} ", "  ".repeat(depth)), text))
}

fn code_block(out: &mut Styled, info: &str, body: &[&str], width: usize) {
    let name = info.split_whitespace().next().unwrap_or("");
    let label = if name.is_empty() { String::new() } else { format!(" {name} ") };
    let rest = width.saturating_sub(2 + UnicodeWidthStr::width(label.as_str()));
    out.push(&format!("──{label}{}", "─".repeat(rest)), Some(Highlight::COMMENT));
    out.push("\n", None);
    let mut code = body.join("\n");
    code.push('\n');
    let start = out.text.len();
    out.push(&code, None);
    let language = syntax::language_for_fence(name).filter(|_| code.len() <= HIGHLIGHT_BYTE_CEILING);
    if let Some(language) = language {
        let mut spans = syntax::highlight(language, &code);
        // Captures can nest; the renderer takes the first span that covers a byte.
        spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
        let mut end = 0;
        for span in spans {
            if span.start >= end && span.start < span.end {
                out.spans.push((start + span.start..start + span.end, span.role));
                end = span.end;
            }
        }
    }
    out.rule(width);
}

fn table(out: &mut Styled, rows: &[&str], width: usize) {
    let cells = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let row = row.trim().trim_start_matches('|');
            let row = row.strip_suffix('|').unwrap_or(row);
            let role = (index == 0).then_some(Highlight::STRONG);
            row.split('|').map(|cell| inline(cell.trim(), role)).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; columns];
    for row in &cells {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(segments_width(cell));
        }
    }
    let separator = (" │ ", Some(Highlight::COMMENT));
    if widths.iter().sum::<usize>() + 3 * columns.saturating_sub(1) > width {
        // Too wide to line up: one wrapped line per row.
        for row in cells {
            let mut line = Vec::new();
            for (column, cell) in row.into_iter().enumerate() {
                if column > 0 {
                    line.push((separator.0.to_owned(), separator.1));
                }
                line.extend(cell);
            }
            wrap(out, &line, width, ("", None), ("", None));
        }
        return;
    }
    for (index, row) in cells.iter().enumerate() {
        for column in 0..columns {
            if column > 0 {
                out.push(separator.0, separator.1);
            }
            let cell = row.get(column).map_or(&[][..], Vec::as_slice);
            for (text, role) in cell {
                out.push(text, *role);
            }
            if column + 1 < columns {
                out.push(&" ".repeat(widths[column] - segments_width(cell)), None);
            }
        }
        out.push("\n", None);
        if index == 0 {
            let line = widths.iter().map(|width| "─".repeat(*width)).collect::<Vec<_>>().join("─┼─");
            out.push(&line, Some(Highlight::COMMENT));
            out.push("\n", None);
        }
    }
}

fn segments_width(segments: &[Segment]) -> usize {
    segments.iter().map(|(text, _)| UnicodeWidthStr::width(text.as_str())).sum()
}

/// Inline Markdown as styled runs: code spans, bold, italic, links and escapes. Text
/// outside any of them gets `base`.
fn inline(text: &str, base: Option<Highlight>) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut plain = String::new();
    let flush = |segments: &mut Vec<Segment>, plain: &mut String| {
        if !plain.is_empty() {
            segments.push((std::mem::take(plain), base));
        }
    };
    let mut at = 0;
    while let Some(character) = text[at..].chars().next() {
        let rest = &text[at..];
        let before = text[..at].chars().next_back();
        match character {
            '\\' => {
                if let Some(next) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                    plain.push(next);
                    at += 1 + next.len_utf8();
                    continue;
                }
            }
            '`' => {
                let ticks = rest.len() - rest.trim_start_matches('`').len();
                if let Some(end) = rest[ticks..].find(&rest[..ticks]) {
                    flush(&mut segments, &mut plain);
                    let code = &rest[ticks..ticks + end];
                    let code = code
                        .strip_prefix(' ')
                        .and_then(|code| code.strip_suffix(' '))
                        .filter(|inner| !inner.trim().is_empty())
                        .unwrap_or(code);
                    segments.push((code.to_owned(), Some(Highlight::RAW)));
                    at += ticks + end + ticks;
                } else {
                    plain.push_str(&rest[..ticks]);
                    at += ticks;
                }
                continue;
            }
            '*' | '_' | '~' => {
                let run = rest.len() - rest.trim_start_matches(character).len();
                let length = if character == '~' { 2 } else { run.min(2) };
                let marker = &rest[..length.min(run)];
                let opens = run >= length
                    && rest[length..].chars().next().is_some_and(|next| !next.is_whitespace())
                    && (character != '_' || before.is_none_or(|before| !before.is_alphanumeric()));
                if let Some(close) = opens.then(|| closing(text, at + length, marker)).flatten() {
                    flush(&mut segments, &mut plain);
                    let role = match (character, length) {
                        ('~', _) => base,
                        (_, 2) => Some(Highlight::STRONG),
                        _ => Some(Highlight::ITALIC),
                    };
                    segments.extend(inline(&text[at + length..close], role));
                    at = close + length;
                } else {
                    plain.push_str(&rest[..run]);
                    at += run;
                }
                continue;
            }
            '[' | '!' => {
                let label_start = usize::from(character == '!') + 1;
                if (character == '[' || rest.starts_with("!["))
                    && let Some((label, length)) = link(&rest[label_start - 1..])
                {
                    flush(&mut segments, &mut plain);
                    segments.extend(inline(label, Some(Highlight::LINK)));
                    at += label_start - 1 + length;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = rest.find('>')
                    && rest[1..end].starts_with("http")
                    && !rest[1..end].contains(' ')
                {
                    flush(&mut segments, &mut plain);
                    segments.push((rest[1..end].to_owned(), Some(Highlight::LINK)));
                    at += end + 1;
                    continue;
                }
            }
            'h' if (rest.starts_with("https://") || rest.starts_with("http://"))
                && before.is_none_or(|before| before.is_whitespace() || before == '(') =>
            {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let url = rest[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
                flush(&mut segments, &mut plain);
                segments.push((url.to_owned(), Some(Highlight::LINK)));
                at += url.len();
                continue;
            }
            _ => {}
        }
        plain.push(character);
        at += character.len_utf8();
    }
    flush(&mut segments, &mut plain);
    segments
}

/// Where `marker` closes emphasis opened before `from`: after a non-space, and for `_`
/// not inside a word.
fn closing(text: &str, from: usize, marker: &str) -> Option<usize> {
    let mut search = from;
    while let Some(found) = text[search..].find(marker) {
        let close = search + found;
        let before = text[..close].chars().next_back();
        let after = text[close + marker.len()..].chars().next();
        let inside_word = marker.starts_with('_') && after.is_some_and(char::is_alphanumeric);
        // `**` must not be read as two single `*`.
        let doubled = marker.len() == 1 && after == marker.chars().next();
        if close > from && before.is_some_and(|before| !before.is_whitespace()) && !inside_word && !doubled {
            return Some(close);
        }
        search = close + marker.len().max(1);
        if doubled {
            search += 1;
        }
    }
    None
}

/// The label of a `[label](target)` link at the start of `text`, and the link's length.
fn link(text: &str) -> Option<(&str, usize)> {
    let mut depth = 0;
    let mut label_end = None;
    for (index, character) in text.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    label_end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let label_end = label_end?;
    let target = text[label_end + 1..].strip_prefix('(')?;
    let target_end = target.find(')')?;
    let label = &text[1..label_end];
    let label = if label.is_empty() { &target[..target_end] } else { label };
    Some((label, label_end + 2 + target_end + 1))
}

/// Lay `segments` out in lines of at most `width` cells, the first starting with `first`
/// and the rest with `rest`. Words longer than a line are broken between characters.
fn wrap(out: &mut Styled, segments: &[Segment], width: usize, first: Prefix, rest: Prefix) {
    let mut words: Vec<Vec<(&str, Option<Highlight>)>> = vec![Vec::new()];
    for (text, role) in segments {
        for (index, part) in text.split(' ').enumerate() {
            if index > 0 {
                words.push(Vec::new());
            }
            if !part.is_empty() {
                words.last_mut().expect("starts with one word").push((part, *role));
            }
        }
    }
    let start_line = |out: &mut Styled, prefix: Prefix| {
        out.push(prefix.0, prefix.1);
        UnicodeWidthStr::width(prefix.0)
    };
    let mut column = start_line(out, first);
    let mut line_start = true;
    let mut broken = false;
    let mut previous_role = None;
    for word in &words {
        let word_width = word.iter().map(|(text, _)| UnicodeWidthStr::width(*text)).sum::<usize>();
        // Runs of spaces where a line was broken are not carried to the next line.
        if word.is_empty() && line_start && broken {
            continue;
        }
        if !line_start && column + 1 + word_width > width {
            out.push("\n", None);
            column = start_line(out, rest);
            line_start = true;
            broken = true;
        }
        if !line_start {
            let next_role = word.first().and_then(|(_, role)| *role);
            out.push(" ", previous_role.filter(|role| Some(*role) == next_role));
            column += 1;
        }
        let line_width = width.saturating_sub(UnicodeWidthStr::width(rest.0)).max(1);
        for (text, role) in word {
            if word_width <= line_width {
                out.push(text, *role);
                column += UnicodeWidthStr::width(*text);
                continue;
            }
            for character in text.chars() {
                let cells = character.width().unwrap_or(0);
                if column + cells > width && column > UnicodeWidthStr::width(rest.0) {
                    out.push("\n", None);
                    column = start_line(out, rest);
                }
                out.push(character.encode_utf8(&mut [0; 4]), *role);
                column += cells;
            }
        }
        previous_role = word.last().and_then(|(_, role)| *role);
        line_start = false;
    }
    out.push("\n", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(source: &str, width: usize) -> Styled {
        let mut out = Styled::default();
        render(&mut out, source, width);
        out
    }

    fn styled<'a>(out: &'a Styled, role: Highlight) -> Vec<&'a str> {
        out.spans
            .iter()
            .filter(|(_, candidate)| *candidate == role)
            .map(|(range, _)| &out.text[range.clone()])
            .collect()
    }

    #[test]
    fn markers_become_styles() {
        let out = rendered(
            "# Title\n\nSome **bold**, _italic_ and `code`, see [docs](https://x.y).\n\n> quoted\n\n- one\n  - two\n1. first\n- [x] done\n\n---",
            40,
        );
        assert_eq!(
            out.text,
            "Title\n\nSome bold, italic and code, see docs.\n\n▎ quoted\n\n• one\n  ◦ two\n1. first\n✓ done\n\n────────────────────────────────────────\n"
        );
        assert_eq!(styled(&out, Highlight::HEADING), ["Title"]);
        assert_eq!(styled(&out, Highlight::STRONG), ["bold"]);
        assert_eq!(styled(&out, Highlight::ITALIC), ["italic"]);
        assert_eq!(styled(&out, Highlight::RAW), ["code"]);
        assert_eq!(styled(&out, Highlight::LINK), ["docs"]);
    }

    #[test]
    fn words_inside_identifiers_and_lone_stars_stay_as_written() {
        let out = rendered("snake_case_name and 2 * 3 * 4", 80);
        assert_eq!(out.text, "snake_case_name and 2 * 3 * 4\n");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn code_blocks_keep_their_text_and_get_highlighted() {
        let out = rendered("```rust\nfn main() {}\n```\nafter", 20);
        assert_eq!(
            out.text,
            "── rust ────────────\nfn main() {}\n────────────────────\nafter\n"
        );
        assert!(styled(&out, Highlight::KEYWORD).contains(&"fn"));
    }

    #[test]
    fn tables_line_up_and_lines_wrap_at_words() {
        let out = rendered("| a | long |\n|---|---|\n| bb | c |", 40);
        assert_eq!(out.text, "a  │ long\n───┼─────\nbb │ c\n");
        assert_eq!(rendered("aaa bbb ccc", 8).text, "aaa bbb\nccc\n");
        assert_eq!(rendered("- aaa bbb", 7).text, "• aaa\n  bbb\n");
    }
}
