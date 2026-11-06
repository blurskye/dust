//! Markdown shown as styled text: the markers are dropped and what they meant becomes a
//! style, the way LazyVim's render-markdown shows it. Each source line stays its own line
//! and is wrapped at words. The agent transcript is drawn with it.

use std::ops::Range;

use crate::config::AmbiguousWidth;

use crate::ui::geometry::text_width;
use unicode_segmentation::UnicodeSegmentation;

use crate::config::Highlight;

/// Code blocks larger than this are shown without highlighting.
const HIGHLIGHT_BYTE_LIMIT: usize = 64 * 1024;

/// Text with the styled byte ranges in it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Styled {
    pub text: String,
    pub spans: Vec<(Range<usize>, Highlight)>,
    pub ambiguous_width: AmbiguousWidth,
}

impl Styled {
    fn width(&self, text: &str) -> usize {
        text.graphemes(true)
            .map(|g| text_width(g, self.ambiguous_width).clamp(1, 2))
            .sum()
    }

    pub fn push(&mut self, text: &str, role: Option<Highlight>) {
        let start = self.text.len();
        self.text.push_str(text);
        let Some(role) = role.filter(|_| !text.is_empty()) else {
            return;
        };
        match self.spans.last_mut() {
            Some((range, last)) if range.end == start && *last == role => {
                range.end = self.text.len()
            }
            _ => self.spans.push((start..self.text.len(), role)),
        }
    }

    /// A line of `width` rule characters.
    pub fn rule(&mut self, width: usize) {
        self.push(
            &"─".repeat(width / self.width("─")),
            Some(Highlight::COMMENT),
        );
        self.push("\n", None);
    }

    /// Plain `text` wrapped at words to `width` cells. Its first line starts with `first`
    /// and every other line with `rest`.
    pub fn wrapped(
        &mut self,
        text: &str,
        role: Option<Highlight>,
        first: Prefix,
        rest: Prefix,
        width: usize,
    ) {
        for (index, line) in text.lines().enumerate() {
            let segments = [(line.to_owned(), role)];
            wrap(
                self,
                &segments,
                width,
                if index == 0 { first } else { rest },
                rest,
            );
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
    render_blocks(out, source, width, true);
}

/// Like `render`, for a small popup: code blocks are not framed by rules.
pub fn render_compact(out: &mut Styled, source: &str, width: usize) {
    render_blocks(out, source, width, false);
}

fn render_blocks(out: &mut Styled, source: &str, width: usize, framed: bool) {
    let width = width.max(1);
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
            code_block(out, info, &body, width, framed);
        } else if trimmed.starts_with('|')
            && lines
                .get(index)
                .is_some_and(|next| is_table_separator(next))
        {
            let mut rows = vec![trimmed];
            index += 1;
            while let Some(row) = lines
                .get(index)
                .map(|row| row.trim())
                .filter(|row| row.starts_with('|'))
            {
                rows.push(row);
                index += 1;
            }
            table(out, &rows, width);
        } else if let Some(title) = heading(trimmed) {
            wrap(
                out,
                &inline(title, Some(Highlight::HEADING)),
                width,
                ("", None),
                ("", None),
            );
        } else if is_rule(trimmed) {
            out.rule(width);
        } else if trimmed.starts_with('>') {
            let text = trimmed.trim_start_matches(['>', ' ']);
            let bar = ("▎ ", Some(Highlight::QUOTE));
            wrap(out, &inline(text, Some(Highlight::QUOTE)), width, bar, bar);
        } else if let Some((marker, text)) = list_item(trimmed, indent) {
            let continuation = " ".repeat(out.width(marker.as_str()));
            wrap(
                out,
                &inline(text, None),
                width,
                (marker.as_str(), Some(Highlight::KEYWORD)),
                (continuation.as_str(), None),
            );
        } else if trimmed.is_empty() {
            out.push("\n", None);
        } else if lines
            .get(index)
            .is_some_and(|next| is_setext_underline(next.trim()))
        {
            index += 1;
            wrap(
                out,
                &inline(trimmed, Some(Highlight::HEADING)),
                width,
                ("", None),
                ("", None),
            );
        } else {
            wrap(
                out,
                &inline(trimmed, None),
                width,
                (indent, None),
                (indent, None),
            );
        }
    }
}

/// The fence and the info string after it, for a line that opens a code block.
fn fence_open(line: &str) -> Option<(&str, &str)> {
    let marker = line
        .chars()
        .next()
        .filter(|marker| matches!(*marker, '`' | '~'))?;
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
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' '))).then(|| {
        let title = rest.trim_end();
        let without_hashes = title.trim_end_matches('#');
        if without_hashes.ends_with([' ', '\t']) {
            without_hashes.trim()
        } else {
            title.trim_start()
        }
    })
}

/// The `===` or `---` line under a heading written the other way.
fn is_setext_underline(line: &str) -> bool {
    !line.is_empty() && (line.chars().all(|c| c == '=') || line.chars().all(|c| c == '-'))
}

fn is_rule(line: &str) -> bool {
    let mut marks = line.chars().filter(|character| *character != ' ');
    let Some(first) = marks
        .next()
        .filter(|first| matches!(*first, '-' | '*' | '_'))
    else {
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
        && line
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

/// The bullet or number that starts a list item, as shown, and the item's text.
fn list_item<'a>(line: &'a str, indent: &str) -> Option<(String, &'a str)> {
    let depth = indent
        .chars()
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum::<usize>()
        / 2;
    let (marker, text) = if let Some(text) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|bullet| line.strip_prefix(*bullet))
    {
        (["•", "◦", "▪"][depth % 3].to_owned(), text)
    } else {
        let digits = line.len()
            - line
                .trim_start_matches(|character: char| character.is_ascii_digit())
                .len();
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

fn code_block(out: &mut Styled, info: &str, body: &[&str], width: usize, framed: bool) {
    let name = info.split_whitespace().next().unwrap_or("");
    if framed {
        let label = if name.is_empty() {
            String::new()
        } else {
            format!(" {name} ")
        };
        let rest = width.saturating_sub(out.width("──") + out.width(label.as_str()));
        out.push(
            &format!("──{label}{}", "─".repeat(rest / out.width("─"))),
            Some(Highlight::COMMENT),
        );
        out.push("\n", None);
    }
    let mut code = body.join("\n");
    code.push('\n');
    let language = syntax::language_for_fence(name).filter(|_| code.len() <= HIGHLIGHT_BYTE_LIMIT);
    let mut spans = language
        .map(|language| syntax::highlight(language, &code))
        .unwrap_or_default();
    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut end = 0;
    spans.retain(|span| {
        if span.start < end || span.start >= span.end {
            return false;
        }
        end = span.end;
        true
    });
    let mut span_index = 0;
    let mut column = 0;
    for (at, grapheme) in code.grapheme_indices(true) {
        if grapheme == "\n" {
            out.push("\n", None);
            column = 0;
            continue;
        }
        let cells = out.width(grapheme);
        if column > 0 && column + cells > width {
            out.push("\n", None);
            column = 0;
        }
        while spans.get(span_index).is_some_and(|span| span.end <= at) {
            span_index += 1;
        }
        let role = spans
            .get(span_index)
            .filter(|span| span.start <= at)
            .map(|span| span.role);
        out.push(grapheme, role);
        column += cells;
    }

    if framed {
        out.rule(width);
    }
}

fn table_cells(row: &str) -> Vec<&str> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, character) in row.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '|' {
            cells.push(&row[start..index]);
            start = index + 1;
        }
    }
    if start < row.len() || cells.is_empty() {
        cells.push(&row[start..]);
    }
    cells
}

fn table(out: &mut Styled, rows: &[&str], width: usize) {
    let cells = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let role = (index == 0).then_some(Highlight::STRONG);
            table_cells(row)
                .into_iter()
                .map(|cell| inline(cell.trim(), role))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; columns];
    for row in &cells {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(segments_width(out, cell));
        }
    }
    let separator = (" │ ", Some(Highlight::COMMENT));
    if widths.iter().sum::<usize>() + out.width(separator.0) * columns.saturating_sub(1) > width {
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
        for (column, width) in widths.iter().enumerate().take(columns) {
            if column > 0 {
                out.push(separator.0, separator.1);
            }
            let cell = row.get(column).map_or(&[][..], Vec::as_slice);
            for (text, role) in cell {
                out.push(text, *role);
            }
            if column + 1 < columns {
                out.push(&" ".repeat(width - segments_width(out, cell)), None);
            }
        }
        out.push("\n", None);
        if index == 0 {
            let line = widths
                .iter()
                .map(|width| {
                    let rule_width = out.width("─");
                    format!(
                        "{}{}",
                        "─".repeat(*width / rule_width),
                        " ".repeat(*width % rule_width)
                    )
                })
                .collect::<Vec<_>>()
                .join(if out.width("─") == 1 {
                    "─┼─"
                } else {
                    " ┼ "
                });
            out.push(&line, Some(Highlight::COMMENT));
            out.push("\n", None);
        }
    }
}

fn segments_width(out: &Styled, segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|(text, _)| out.width(text.as_str()))
        .sum()
}

/// Inline Markdown as styled runs: code spans, bold, italic, links and escapes. Text
/// outside any of them gets `base`.
fn inline(text: &str, base: Option<Highlight>) -> Vec<Segment> {
    inline_at_depth(text, base, 0)
}

fn inline_at_depth(text: &str, base: Option<Highlight>, depth: usize) -> Vec<Segment> {
    if depth >= 16 {
        return vec![(text.to_owned(), base)];
    }
    let mut segments = Vec::new();
    let mut plain = String::new();
    let flush = |segments: &mut Vec<Segment>, plain: &mut String| {
        if !plain.is_empty() {
            segments.push((std::mem::take(plain), base));
        }
    };
    // Match labels once: retrying a scan at every unmatched `[` is quadratic.
    let mut brackets = Vec::new();
    let mut label_ends = std::collections::HashMap::new();
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
        } else {
            match character {
                '\\' => escaped = true,
                '[' => brackets.push(index),
                ']' => {
                    if let Some(start) = brackets.pop() {
                        label_ends.insert(start, index);
                    }
                }
                _ => {}
            }
        }
    }
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
                let mut search = ticks;
                let mut close = None;
                while let Some(found) = rest[search..].find('`') {
                    let start = search + found;
                    let run = rest[start..].len() - rest[start..].trim_start_matches('`').len();
                    if run == ticks {
                        close = Some(start);
                        break;
                    }
                    search = start + run;
                }
                if let Some(end) = close {
                    flush(&mut segments, &mut plain);
                    let code = &rest[ticks..end];
                    let code = code
                        .strip_prefix(' ')
                        .and_then(|code| code.strip_suffix(' '))
                        .filter(|inner| !inner.trim().is_empty())
                        .unwrap_or(code);
                    segments.push((code.to_owned(), Some(Highlight::RAW)));
                    at += end + ticks;
                } else {
                    plain.push_str(&rest[..ticks]);
                    at += ticks;
                }
                continue;
            }
            '*' | '_' | '~' => {
                let run = rest.len() - rest.trim_start_matches(character).len();
                let length = if character == '~' { 2 } else { run.min(3) };
                let marker = &rest[..length.min(run)];
                let opens = run >= length
                    && rest[length..]
                        .chars()
                        .next()
                        .is_some_and(|next| !next.is_whitespace())
                    && (character != '_' || before.is_none_or(|before| !before.is_alphanumeric()));
                if let Some(close) = opens.then(|| closing(text, at + length, marker)).flatten() {
                    flush(&mut segments, &mut plain);
                    let role = match (character, length) {
                        ('~', _) => Some(Highlight::STRIKETHROUGH),
                        (_, 2 | 3) => Some(Highlight::STRONG),
                        _ => Some(Highlight::ITALIC),
                    };
                    segments.extend(inline_at_depth(&text[at + length..close], role, depth + 1));
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
                    && let Some(end) = label_ends.get(&(at + label_start - 1))
                    && let Some((label, length)) =
                        link(&rest[label_start - 1..], end - at - label_start + 1)
                {
                    flush(&mut segments, &mut plain);
                    segments.extend(inline_at_depth(label, Some(Highlight::LINK), depth + 1));
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
            '&' => {
                // The entities language servers write in hover text.
                let entity = [
                    ("&lt;", '<'),
                    ("&gt;", '>'),
                    ("&quot;", '"'),
                    ("&#39;", '\''),
                    ("&nbsp;", ' '),
                    ("&amp;", '&'),
                ]
                .into_iter()
                .find(|(name, _)| rest.starts_with(name));
                if let Some((name, decoded)) = entity {
                    plain.push(decoded);
                    at += name.len();
                    continue;
                }
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
        let escaped = text[..close]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count()
            % 2
            == 1;
        if close > from
            && !escaped
            && before.is_some_and(|before| !before.is_whitespace())
            && !inside_word
            && !doubled
        {
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
fn link(text: &str, label_end: usize) -> Option<(&str, usize)> {
    let target = text[label_end + 1..].strip_prefix('(')?;
    let mut depth = 0usize;
    let mut escaped = false;
    let mut angle = target.trim_start().starts_with('<');
    let mut quote = None;
    let mut after_space = false;
    let target_end = target.char_indices().find_map(|(index, character)| {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if angle {
            angle = character != '>';
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
        } else {
            match character {
                '\'' | '"' if depth == 0 && after_space => quote = Some(character),
                '(' => depth += 1,
                ')' if depth == 0 => return Some(index),
                ')' => depth -= 1,
                _ => {}
            }
        }
        after_space = character.is_whitespace();
        None
    })?;
    let label = &text[1..label_end];
    let label = if label.is_empty() {
        &target[..target_end]
    } else {
        label
    };
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
                words
                    .last_mut()
                    .expect("starts with one word")
                    .push((part, *role));
            }
        }
    }
    let width = width.max(1);
    let start_line = |out: &mut Styled, prefix: Prefix| {
        let mut cells = 0;
        for grapheme in prefix.0.graphemes(true) {
            let next = out.width(grapheme);
            if cells + next >= width {
                break;
            }
            out.push(grapheme, prefix.1);
            cells += next;
        }
        cells
    };
    let mut column = start_line(out, first);
    let mut prefix_width = column;
    let mut line_start = true;
    let mut broken = false;
    let mut previous_role = None;
    for word in &words {
        let word_width = word.iter().map(|(text, _)| out.width(text)).sum::<usize>();
        // Runs of spaces where a line was broken are not carried to the next line.
        if word.is_empty() && line_start && broken {
            continue;
        }
        if !line_start && column + 1 + word_width > width {
            out.push("\n", None);
            column = start_line(out, rest);
            prefix_width = column;
            line_start = true;
            broken = true;
        }
        if !line_start {
            let next_role = word.first().and_then(|(_, role)| *role);
            out.push(" ", previous_role.filter(|role| Some(*role) == next_role));
            column += 1;
        }
        let fits = word_width <= width.saturating_sub(column);
        for (text, role) in word {
            if fits {
                out.push(text, *role);
                column += out.width(text);
                continue;
            }
            for grapheme in text.graphemes(true) {
                let cells = out.width(grapheme);
                if column + cells > width && column > prefix_width {
                    out.push("\n", None);
                    column = start_line(out, rest);
                    prefix_width = column;
                }
                if column + cells > width {
                    out.push("?", *role);
                    column += 1;
                } else {
                    out.push(grapheme, *role);
                    column += cells;
                }
            }
        }
        previous_role = word.last().and_then(|(_, role)| *role);
        line_start = false;
    }
    out.push("\n", None);
}

use crate::language::syntax;

#[cfg(test)]
mod tests {
    use crate::ui::markdown::*;

    fn rendered(source: &str, width: usize) -> Styled {
        let mut out = Styled::default();
        render(&mut out, source, width);
        out
    }

    fn styled(out: &Styled, role: Highlight) -> Vec<&str> {
        out.spans
            .iter()
            .filter(|(_, candidate)| *candidate == role)
            .map(|(range, _)| &out.text[range.clone()])
            .collect()
    }

    #[test]
    fn code_lines_wrap_without_losing_text_or_highlights() {
        let out = rendered("```rust\n    let identifier = 1234567890;\n```", 12);
        assert!(out.text.lines().all(|line| line.chars().count() <= 12));
        assert!(
            out.text
                .replace('\n', "")
                .contains("    let identifier = 1234567890;")
        );
        assert!(styled(&out, Highlight::named("@keyword.rust").unwrap()).contains(&"let"));
    }

    #[test]
    fn wrapping_uses_ambiguous_width_and_joined_graphemes() {
        let mut out = Styled {
            ambiguous_width: AmbiguousWidth::Wide,
            ..Default::default()
        };
        render(&mut out, "······", 10);
        assert_eq!(out.text, "·····\n·\n");
        let out = rendered("aaaaaaaa👩‍💻zzz", 10);
        assert_eq!(out.text, "aaaaaaaa👩‍💻\nzzz\n");
    }

    #[test]
    fn wrapping_measures_each_prefix_and_bounds_exhausted_lines() {
        let mut out = Styled {
            ambiguous_width: AmbiguousWidth::Wide,
            ..Default::default()
        };
        out.wrapped("abcdefgh", None, ("● ", None), ("  ", None), 10);
        assert_eq!(out.text, "● abcdefg\n  h\n");
        assert!(out.text.lines().all(|line| out.width(line) <= 10));
        for width in 1..=4 {
            let mut out = Styled::default();
            out.wrapped("界界 abc", None, ("prefix ", None), ("  ", None), width);
            assert!(
                out.text.lines().all(|line| out.width(line) <= width),
                "{width}: {:?}",
                out.text
            );
            assert!(out.text.ends_with("c\n"));
        }
        let mut out = Styled::default();
        out.wrapped("abcdef", None, ("", None), ("long prefix", None), 3);
        assert!(out.text.lines().all(|line| out.width(line) <= 3));
        assert!(out.text.ends_with("f\n"));
    }

    #[test]
    fn unmatched_link_openers_are_literal_and_do_not_rescan_the_suffix() {
        for source in ["[".repeat(50_000), format!("{}]", "[".repeat(50_000))] {
            let runs = inline(&source, None);
            assert_eq!(runs, vec![(source, None)]);
        }
        assert_eq!(
            inline("[unclosed [ok](url)", None),
            vec![
                ("[unclosed ".into(), None),
                ("ok".into(), Some(Highlight::LINK)),
            ]
        );
    }

    #[test]
    fn deeply_nested_links_are_bounded() {
        let source = format!("{}x{}", "[".repeat(50_000), "](x)".repeat(50_000));
        let runs = inline(&source, None);
        assert!(runs.iter().any(|(text, _)| text.contains('x')));
        assert!(runs.len() <= 17);
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
    fn headings_keep_literal_hashes_and_only_remove_closing_markers() {
        let out = rendered("# C#\n## F##\n# Title ###\n# Title \\###\n# ###", 40);
        assert_eq!(out.text, "C#\nF##\nTitle\nTitle ###\n\n");
        assert!(styled(&out, Highlight::HEADING).contains(&"C#"));
    }

    #[test]
    fn inline_code_requires_a_matching_backtick_run() {
        let out = rendered("`a``b` and ``a`b`` and `unclosed``", 80);
        assert_eq!(out.text, "a``b and a`b and `unclosed``\n");
        assert_eq!(styled(&out, Highlight::RAW), ["a``b", "a`b"]);
    }

    #[test]
    fn triple_emphasis_consumes_both_complete_markers() {
        let out = rendered("***bold*** and ___strong___", 80);
        assert_eq!(out.text, "bold and strong\n");
        assert_eq!(styled(&out, Highlight::STRONG), ["bold", "strong"]);
    }

    #[test]
    fn escaped_emphasis_markers_remain_inside_the_styled_text() {
        let out = rendered(r"*a\*b* and **c\**d** and _e\_f_", 80);
        assert_eq!(out.text, "a*b and c**d and e_f\n");
        assert_eq!(styled(&out, Highlight::ITALIC), ["a*b", "e_f"]);
        assert_eq!(styled(&out, Highlight::STRONG), ["c**d"]);
        assert_eq!(rendered(r"*a\\*", 80).text, "a\\\n");
    }

    #[test]
    fn link_targets_balance_parentheses_and_skip_escaped_delimiters() {
        let out = rendered(
            r"[doc](https://example.test/a_(b)) [next](a\)b) [last](a\(b)",
            100,
        );
        assert_eq!(out.text, "doc next last\n");
        assert_eq!(styled(&out, Highlight::LINK), ["doc next last"]);
    }

    #[test]
    fn link_parentheses_inside_angle_destinations_and_titles_are_literal() {
        let out = rendered(
            r#"[angle](<https://example.test/a_(b>) [double](url "a(b") [single](url 'a)b') [apostrophe](https://example.test/it's_(ok))"#,
            100,
        );
        assert_eq!(out.text, "angle double single apostrophe\n");
        assert_eq!(
            styled(&out, Highlight::LINK),
            ["angle double single apostrophe"]
        );
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
        assert!(styled(&out, Highlight::named("@keyword.rust").unwrap()).contains(&"fn"));
    }

    #[test]
    fn table_cells_preserve_empty_columns_and_escaped_pipes() {
        assert_eq!(table_cells("||Name|"), ["", "Name"]);
        assert_eq!(table_cells(r"|a\|b|c|"), [r"a\|b", "c"]);
        assert_eq!(table_cells(r"|a\\|b|"), [r"a\\", "b"]);
        assert_eq!(table_cells(r"|a\|"), [r"a\|"]);
        let out = rendered("||Name|\n|---|---|\n|1|Alice|", 40);
        assert_eq!(out.text, "  │ Name\n──┼──────\n1 │ Alice\n");
        let out = rendered(
            r"|a\|b|c|
|---|---|
|d|e|",
            40,
        );
        assert_eq!(out.text, "a|b │ c\n────┼──\nd   │ e\n");
    }

    #[test]
    fn table_rules_align_with_wide_ambiguous_separators() {
        let mut out = Styled {
            ambiguous_width: AmbiguousWidth::Wide,
            ..Default::default()
        };
        render(&mut out, "| aa | bb |\n|---|---|\n| cc | dd |", 8);
        assert_eq!(out.text, "aa │ bb\n─ ┼ ─\ncc │ dd\n");
        assert!(out.text.lines().all(|line| out.width(line) == 8));
        let mut out = Styled {
            ambiguous_width: AmbiguousWidth::Wide,
            ..Default::default()
        };
        render(&mut out, "| aaa | bbb |\n|---|---|\n| ccc | ddd |", 10);
        assert!(out.text.lines().all(|line| out.width(line) == 10));
        let header = out.text.lines().next().unwrap();
        let rule = out.text.lines().nth(1).unwrap();
        assert_eq!(
            out.width(header.split('│').next().unwrap()),
            out.width(rule.split('┼').next().unwrap())
        );
    }

    #[test]
    fn tables_line_up_and_lines_wrap_at_words() {
        let out = rendered("| a | long |\n|---|---|\n| bb | c |", 40);
        assert_eq!(out.text, "a  │ long\n───┼─────\nbb │ c\n");
        assert_eq!(rendered("aaa bbb ccc", 8).text, "aaa bbb\nccc\n");
        assert_eq!(rendered("- aaa bbb", 7).text, "• aaa\n  bbb\n");
    }
}
