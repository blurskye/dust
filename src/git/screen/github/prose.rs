//! Forge reader rows: Markdown bodies, metadata tables, card
//! headers and links as `lines::Line`s that carry their own Git styles. The painter in
//! `paint.rs` draws them on the reader surface.
use super::find;
use super::lines::Line;
use crate::config::Highlight;
use crate::forge::records::LabelChip;
use crate::ui::{Emphasis, GitRole, GitSurface, Style};
use std::ops::Range;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

mod html;
mod table;
pub(in crate::git::screen) use table::configure as configure_tables;

/// Bodies wrap at a comfortable measure, never wider than this.
pub(super) const MEASURE: usize = 100;

pub(super) const fn role(role: GitRole) -> Style {
    Style::Git {
        role,
        surface: GitSurface::Pane,
        emphasis: Emphasis::None,
    }
}
pub(super) const fn strong(role: GitRole) -> Style {
    Style::Git {
        role,
        surface: GitSurface::Pane,
        emphasis: Emphasis::Bold,
    }
}
pub(super) fn label_style(label: &LabelChip) -> Style {
    match label.rgb {
        Some([r, g, b]) => Style::Git {
            role: GitRole::Label(r, g, b),
            surface: GitSurface::Label(r, g, b),
            emphasis: Emphasis::None,
        },
        None => Style::Git {
            role: GitRole::Modified,
            surface: GitSurface::Chip(crate::ui::GitChip::Modified),
            emphasis: Emphasis::None,
        },
    }
}

/// Builds one row out of styled pieces.
#[derive(Default)]
pub(super) struct Rich {
    text: String,
    paint: Vec<(Range<usize>, Style)>,
}
impl Rich {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn push(mut self, text: impl AsRef<str>, style: Style) -> Self {
        let start = self.text.len();
        self.text.push_str(text.as_ref());
        self.paint.push((start..self.text.len(), style));
        self
    }
    pub(super) fn plain(mut self, text: &str) -> Self {
        self.text.push_str(text);
        self
    }
    pub(super) fn sep(self) -> Self {
        self.push(" · ", role(GitRole::Muted))
    }
    pub(super) fn chip(self, text: &str, style: Style) -> Self {
        self.push(format!(" {text} "), style)
    }
    /// `+13 −2`; a zero side is no fact and is left out (`+13`, r11), and so is `+0 −0`.
    pub(super) fn diffstat(mut self, additions: u64, deletions: u64) -> Self {
        if additions > 0 {
            self = self.push(format!("+{additions}"), role(GitRole::Added));
        }
        if deletions > 0 {
            if additions > 0 {
                self = self.plain(" ");
            }
            self = self.push(format!("−{deletions}"), role(GitRole::Removed));
        }
        self
    }
    pub(super) fn width(&self) -> usize {
        self.text.width()
    }
    /// Pads with spaces to `column` cells (a table's value column).
    #[allow(clippy::wrong_self_convention)] // a builder step, like `with_*`
    pub(super) fn to_column(mut self, column: usize) -> Self {
        let width = self.text.width();
        if width < column {
            self.text.push_str(&" ".repeat(column - width));
        }
        self
    }
    pub(super) fn line(self) -> Line {
        let mut line = Line::text(self.text, Style::Default);
        line.paint = self.paint;
        line.pan = false;
        line
    }
}

pub(super) fn blank() -> Line {
    let mut line = Line::text("", Style::Default);
    line.pan = false;
    line
}
pub(super) fn muted(text: &str) -> Line {
    Rich::new().push(text, role(GitRole::Muted)).line()
}
pub(super) fn rule(width: usize) -> Line {
    Rich::new()
        .push("─".repeat(width.max(1)), role(GitRole::Border))
        .line()
}
/// A section heading inside a reader tab: `Latest activity`.
pub(super) fn heading(text: &str, note: &str) -> Line {
    let mut rich = Rich::new().push(text, strong(GitRole::Text));
    if !note.is_empty() {
        rich = rich.plain("  ").push(note, role(GitRole::Muted));
    }
    rich.line()
}
/// A metadata row: muted label in a fixed column, then the value pieces.
pub(super) fn field(label: &str, column: usize) -> Rich {
    Rich::new()
        .push(label, role(GitRole::Muted))
        .to_column(column)
}

/// The fence language of each fenced block, by its first body line.
fn fences(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let fence = if trimmed.starts_with("```") {
            "```"
        } else if trimmed.starts_with("~~~") {
            "~~~"
        } else {
            continue;
        };
        let info = trimmed
            .trim_start_matches(fence.chars().next().unwrap())
            .trim();
        let first = lines.next().unwrap_or_default();
        if !first.trim_start().starts_with(fence) {
            found.push((
                info.split_whitespace().next().unwrap_or("").to_owned(),
                first.to_owned(),
            ));
            for rest in lines.by_ref() {
                if rest.trim_start().starts_with(fence) {
                    break;
                }
            }
        }
    }
    found
}

/// A title as one line of text: code spans lose their backticks, and a draft drops the
/// `Draft:`/`WIP:` prefix its Draft chip already says (one draft marker).
pub(super) fn title(raw: &str, draft: bool) -> String {
    let mut text = raw.trim();
    if draft {
        loop {
            let lower = text.to_ascii_lowercase();
            let cut = ["draft:", "[draft]", "(draft)", "wip:", "[wip]", "(wip)"]
                .iter()
                .find(|prefix| lower.starts_with(**prefix))
                .map(|prefix| prefix.len());
            match cut {
                Some(cut) => text = text[cut..].trim_start(),
                None => break,
            }
        }
    }
    // Runs of spaces, tabs or newlines in a title read as one space: `bug:  Runner`.
    text.replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Labels as chips, without one that repeats the Draft chip (`wip`, `draft`).
pub(super) fn chips(labels: &[LabelChip], draft: bool) -> Vec<LabelChip> {
    labels
        .iter()
        .filter(|label| {
            !draft
                || !matches!(
                    label.name.to_ascii_lowercase().as_str(),
                    "wip" | "draft" | "work in progress" | "status::wip" | "status/wip"
                )
        })
        .cloned()
        .collect()
}

/// What a forge body carries that is not prose: HTML comments (release-note bots hide their
/// state in them), `<details>` blocks (collapsed to their summary), base64 blobs and inline
/// HTML (`html::inline`: `<br>`, `<img alt>`, other tags keep their text). Paragraph lines are reflowed: a single newline inside a paragraph is a space, as
/// GitHub's renderer and glamour treat it, so the pane width decides the wrap. Fenced and
/// indented code keep every byte.
pub(super) fn clean(text: &str) -> String {
    use std::sync::LazyLock;
    static COMMENT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)<!--.*?-->").unwrap());
    static DETAILS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?is)<details[^>]*>\s*(?:<summary[^>]*>(.*?)</summary>)?.*?</details>")
            .unwrap()
    });
    // Split into fenced code and the rest; only the rest is cleaned.
    let mut parts: Vec<(bool, String)> = Vec::new();
    let mut fence: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker));
        let code = match (&fence, marker) {
            (None, Some(marker)) => {
                fence = Some(marker.to_owned());
                true
            }
            (Some(open), Some(marker)) if open == marker => {
                fence = None;
                true
            }
            (Some(_), _) => true,
            (None, None) => false,
        };
        match parts.last_mut() {
            Some((last, buffer)) if *last == code => {
                buffer.push('\n');
                buffer.push_str(line);
            }
            _ => parts.push((code, line.to_owned())),
        }
    }
    let mut out = String::new();
    for (code, part) in parts {
        if !out.is_empty() {
            out.push('\n');
        }
        if code {
            out.push_str(&part);
            continue;
        }
        let part = COMMENT.replace_all(&part, "");
        let part = DETAILS.replace_all(&part, |captures: &regex::Captures| {
            let summary = captures.get(1).map_or("Details", |m| m.as_str()).trim();
            format!(
                "\n▸ {} · collapsed\n",
                html::inline(summary).replace('\n', " ")
            )
        });
        out.push_str(&reflow(&html::inline(&part)));
    }
    out
}

/// Joins soft-wrapped paragraph lines; list items, quotes, headings, tables, rules, indented
/// code, base64 blobs and hard breaks (two trailing spaces or `\`) keep their own lines.
fn reflow(text: &str) -> String {
    let blob = |line: &str| {
        let line = line.trim();
        line.len() >= 60
            && !line.contains(' ')
            && line
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
            && line.chars().any(|c| c.is_ascii_digit())
    };
    let block = |line: &str| {
        let trimmed = line.trim_start();
        line.starts_with("    ")
            || line.starts_with('\t')
            || trimmed.starts_with(['#', '>', '|', '-', '*', '+', '▸', '=', '<'])
            || trimmed.split_once(['.', ')']).is_some_and(|(number, _)| {
                !number.is_empty()
                    && number.len() <= 3
                    && number.chars().all(|c| c.is_ascii_digit())
            })
    };
    let mut out: Vec<String> = Vec::new();
    let mut joinable = false;
    for line in text.lines() {
        if blob(line) {
            continue;
        }
        let blank = line.trim().is_empty();
        let continues = joinable && !blank && !block(line);
        if continues && let Some(last) = out.last_mut() {
            last.push(' ');
            last.push_str(line.trim_start());
        } else {
            out.push(line.to_owned());
        }
        let last = out.last().map(String::as_str).unwrap_or_default();
        let trimmed = last.trim_start();
        // Only prose that was wrapped at a column joins: a long line with words in it.
        // Short lines and unbroken tokens keep their own rows.
        joinable = !blank
            && trimmed.contains(' ')
            && trimmed.chars().count() >= 40
            && !last.ends_with("  ")
            && !last.ends_with('\\')
            && !last.starts_with("    ")
            && !last.starts_with('\t')
            && !trimmed.starts_with(['#', '|'])
            && !trimmed.chars().all(|c| "-*_= ".contains(c));
    }
    // Collapse the blank runs removed blocks leave behind.
    let mut collapsed = String::new();
    let mut blanks = 0;
    for line in out {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        if !collapsed.is_empty() {
            collapsed.push('\n');
        }
        collapsed.push_str(&line);
    }
    collapsed
}

/// Markdown as reader rows: accent headings without their marks, list and checklist glyphs,
/// quotes with a bar, links in the link colour and fenced code on the code surface with its
/// language. Every row keeps a find source for its logical line.
pub(super) fn markdown(rows: &mut Vec<Line>, text: &str, width: usize, indent: &str) {
    // A body reads the same at the same width until the table settings change, and the
    // reader is rebuilt every frame: each one is laid out once and copied after.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (text, width, indent, table::generation()).hash(&mut hasher);
    let key = (hasher.finish(), text.len());
    let remembered = RENDERED.with(|rendered| rendered.borrow().get(&key).cloned());
    if let Some(remembered) = remembered {
        rows.extend(remembered.iter().cloned());
        return;
    }
    let start = rows.len();
    laid_out(rows, text, width, indent);
    let made = std::rc::Rc::new(rows[start..].to_vec());
    RENDERED.with(|rendered| {
        let mut rendered = rendered.borrow_mut();
        // A bound, not an eviction policy: a long session starts over now and then.
        if rendered.len() >= 512 {
            rendered.clear();
        }
        rendered.insert(key, made);
    });
}

thread_local! {
    /// Laid-out bodies by (hash of text, width, indent and table settings; text length).
    #[allow(clippy::type_complexity)]
    static RENDERED: std::cell::RefCell<std::collections::HashMap<(u64, usize), std::rc::Rc<Vec<Line>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn laid_out(rows: &mut Vec<Line>, text: &str, width: usize, indent: &str) {
    let indent_width = indent.width();
    let width = width.saturating_sub(indent_width).clamp(10, MEASURE);
    if text.trim().is_empty() {
        let mut row = muted("No description provided.");
        row.gutter = indent.into();
        rows.push(row);
        return;
    }
    let cleaned = clean(text);
    let text = cleaned.as_str();
    if text.trim().is_empty() {
        let mut row = muted("No description provided.");
        row.gutter = indent.into();
        rows.push(row);
        return;
    }
    for block in table::split(text) {
        match block {
            table::Block::Text(text) => blocks(rows, &text, width, indent),
            table::Block::Table(lines) => table::render(rows, &lines, width, indent),
        }
    }
}

/// Markdown without tables as reader rows, `width` cells wide after `indent`.
fn blocks(rows: &mut Vec<Line>, text: &str, width: usize, indent: &str) {
    let mut styled = crate::ui::markdown::Styled::default();
    crate::ui::markdown::render_logical(&mut styled, text);
    let mut fences = fences(text).into_iter().peekable();
    let mut logical_start = 0;
    let mut in_code: Option<usize> = None;
    let logicals = styled.text.split_terminator('\n').collect::<Vec<_>>();
    for (index, logical) in logicals.iter().enumerate() {
        let start = logical_start;
        logical_start += logical.len() + 1;
        let block = styled
            .preformatted
            .iter()
            .position(|range| range.contains(&start));
        let spans_at = |from: usize, to: usize| {
            let first = styled.spans.partition_point(|(range, _)| range.end <= from);
            styled.spans[first..]
                .iter()
                .take_while(|(range, _)| range.start < to)
                .map(|(range, role)| {
                    (
                        range.start.max(from) - from..range.end.min(to) - from,
                        *role,
                    )
                })
                .collect::<Vec<_>>()
        };
        let source = Arc::<str>::from(*logical);
        if let Some(block) = block {
            let table = logical.contains(" │ ");
            if !table && in_code != Some(block) {
                in_code = Some(block);
                // A code block opens with a padded row naming its language.
                let language = fences
                    .peek()
                    .filter(|(_, first)| first.trim_end() == logical.trim_end())
                    .map(|(language, _)| language.clone());
                if language.is_some() {
                    fences.next();
                }
                // The fence's row, drawn as the editor draws it: the language's icon and
                // name at its left on the code surface (` ```sh ` reads `<sh icon> sh`), an
                // empty code row when the fence names none. A line wider than the reader
                // pans with h and l and shows `›` at its clipped edge.
                let language = language.unwrap_or_default();
                let mut top = Line::text("", Style::Default);
                if !language.is_empty() {
                    let icon = crate::ui::render::markdown::fence_icon(&language);
                    let mut text = String::new();
                    if let Some((glyph, color)) = icon {
                        text.push_str(glyph);
                        text.push(' ');
                        top.spans.push((
                            0..glyph.len(),
                            crate::ui::render::markdown::icon_group(color),
                        ));
                    }
                    let name = text.len()..text.len() + language.len();
                    text.push_str(&language);
                    top.text = text;
                    top.spans.push((name, Highlight::COMMENT));
                }
                top.pan = false;
                top.band = Some(GitSurface::ProseCode);
                top.gutter = format!("{indent}  ");
                rows.push(top);
            }
            let mut row = Line::text(*logical, Style::Default);
            row.spans = spans_at(start, start + logical.len());
            row.source = Some(find::Source {
                text: source,
                offset: 0,
            });
            row.pan = true;
            if !table {
                row.band = Some(GitSurface::ProseCode);
                row.gutter = format!("{indent}  ");
            } else {
                row.gutter = indent.into();
            }
            rows.push(row);
            let next_in_block = logicals.get(index + 1).is_some_and(|_| {
                styled
                    .preformatted
                    .get(block)
                    .is_some_and(|range| range.contains(&logical_start))
            });
            if !table && !next_in_block {
                let mut bottom = blank();
                bottom.band = Some(GitSurface::ProseCode);
                bottom.gutter = format!("{indent}  ");
                rows.push(bottom);
                in_code = None;
            }
            continue;
        }
        in_code = None;
        let spans = spans_at(start, start + logical.len());
        let is_heading = !logical.is_empty()
            && spans
                .first()
                .is_some_and(|(range, role)| range.start == 0 && *role == Highlight::HEADING);
        // Hanging indent: a list marker or quote bar repeats as a continuation gutter.
        let (lead, repeat) = if logical.starts_with("▎ ") {
            ("▎ ".len(), true)
        } else if let Some((range, _)) = spans
            .first()
            .filter(|(range, role)| range.start == 0 && *role == Highlight::KEYWORD)
        {
            (range.end, false)
        } else {
            (logical.len() - logical.trim_start().len(), false)
        };
        let lead_text = &logical[..lead.min(logical.len())];
        let lead_width = lead_text.width();
        // Headings read as GitHub renders them: no `##`, accent and bold, a blank line above.
        let room = width.saturating_sub(lead_width).max(10);
        let body_text = &logical[lead.min(logical.len())..];
        let fragments = if body_text.is_empty() {
            vec![String::new()]
        } else {
            wrap_words(body_text, room)
        };
        // A heading gets breathing room above it, except at the very top.
        if is_heading
            && rows
                .last()
                .is_some_and(|row: &Line| !row.text.is_empty() || row.band.is_some())
        {
            let mut gap = blank();
            gap.gutter = indent.into();
            rows.push(gap);
        }
        let mut offset = 0;
        for (fragment_index, fragment) in fragments.iter().enumerate() {
            let (text, base) = if fragment_index == 0 {
                (format!("{lead_text}{fragment}"), 0)
            } else {
                (fragment.clone(), lead + offset)
            };
            let row_start = start + base;
            let row_end = row_start
                + if fragment_index == 0 {
                    lead + fragment.len()
                } else {
                    fragment.len()
                };
            let mut row = Line::text(&text, Style::Default);
            row.pan = false;
            row.spans = spans_at(row_start, row_end);
            // Checklists read as boxes (`☐`/`☑`); the bytes match `○`/`✓`.
            if fragment_index == 0 && lead > 0 {
                let marker = lead_text.trim_start();
                let swapped = if marker.starts_with("○ ") {
                    Some("☐")
                } else if marker.starts_with("✓ ") {
                    Some("☑")
                } else {
                    None
                };
                if let Some(swapped) = swapped {
                    let at = lead_text.len() - marker.len();
                    row.text.replace_range(at..at + 3, swapped);
                    if swapped == "☑" {
                        row.paint.push((at..at + 3, role(GitRole::Added)));
                    }
                }
            }
            row.source = Some(find::Source {
                text: source.clone(),
                offset: base,
            });
            row.gutter = if fragment_index == 0 {
                indent.to_owned()
            } else if repeat {
                format!("{indent}▎ ")
            } else {
                format!("{indent}{}", " ".repeat(lead_width))
            };
            if repeat {
                row.gutter_style = role(GitRole::Muted);
            }
            if fragment_index > 0 {
                offset += fragment.len();
            } else {
                offset = fragment.len();
            }
            rows.push(row);
        }
    }
}

/// Word wrap that also breaks a long URL or path: before its last `/`, `?`, `&` or
/// `#` in reach; a part longer than the row before its last `-` in reach, and at
/// the edge only when it has none. Plain prose may break after a
/// `-` that joins two words. The fragments are contiguous byte ranges of `text`, as the
/// reader's find offsets need.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let (mut used, mut end, mut space, mut slash, mut hyphen, mut dash) =
            (0, start, None, None, None, None);
        let mut previous = "";
        for (offset, grapheme) in text[start..].grapheme_indices(true) {
            let cells = grapheme.width();
            if used + cells > width && end > start {
                break;
            }
            let at = start + offset;
            if at > start
                && matches!(grapheme, "/" | "?" | "&" | "#")
                && previous != "/"
                && previous != ":"
            {
                slash = Some(at);
            }
            used += cells;
            end = at + grapheme.len();
            if grapheme == " " {
                space = Some(end);
            } else if grapheme == "-" && previous.chars().all(char::is_alphanumeric) {
                hyphen = Some(end);
                if at > start {
                    dash = Some(at);
                }
            }
            previous = grapheme;
        }
        let cut = if end == text.len() {
            end
        } else {
            // A space wins unless the token after it cannot fit a row of its own anyway;
            // then the last separator inside that token, then a hyphen between the words
            // of plain prose, then the hard edge.
            let token = |from: usize| text[from..].split(' ').next().unwrap_or_default().width();
            // A URL or path breaks at `/ ? & #`. A part longer than the row breaks
            // before its last hyphen that fits, so the hyphen opens the next row and cannot
            // read as one the wrap added (r10 N-21: `…-are-general` / `ly-available/` cut a
            // word in two); only a part with no hyphen is cut at the edge.
            let path = |at: usize| {
                let from = text[..at].rfind(' ').map_or(0, |space| space + 1);
                let to = text[at..].find(' ').map_or(text.len(), |space| at + space);
                text[from..to].contains('/')
            };
            let hyphen = hyphen.filter(|at| !path(*at - 1));
            let dash = dash.filter(|at| path(*at));
            let soft = slash.or(dash).or(hyphen);
            match (space, slash.or(dash), hyphen) {
                (Some(space), ..)
                    if soft.is_none_or(|soft| soft < space) || token(space) <= width =>
                {
                    space
                }
                (_, Some(slash), _) => slash,
                (_, None, Some(hyphen)) => hyphen,
                (Some(space), None, None) => space,
                (None, None, None) => end,
            }
        };
        rows.push(text[start..cut].to_owned());
        start = cut;
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// The style a Markdown highlight takes on the reader.
pub(super) fn highlight_style(highlight: Highlight, surface: GitSurface) -> Style {
    let git = |role, emphasis| Style::Git {
        role,
        surface,
        emphasis,
    };
    if highlight == Highlight::HEADING {
        git(GitRole::Focus, Emphasis::Bold)
    } else if highlight == Highlight::KEYWORD {
        git(GitRole::Focus, Emphasis::None)
    } else if highlight == Highlight::QUOTE || highlight == Highlight::COMMENT {
        git(GitRole::Muted, Emphasis::None)
    } else if highlight == Highlight::LINK {
        git(GitRole::Link, Emphasis::None)
    } else if highlight == Highlight::STRONG {
        git(GitRole::Text, Emphasis::Bold)
    } else if highlight == Highlight::ITALIC {
        git(GitRole::Text, Emphasis::None)
    } else if highlight == Highlight::STRIKETHROUGH {
        git(GitRole::Muted, Emphasis::Strike)
    } else if highlight == Highlight::RAW {
        Style::Git {
            role: GitRole::Modified,
            surface: if matches!(surface, GitSurface::Pane) {
                GitSurface::ProseCode
            } else {
                surface
            },
            emphasis: Emphasis::None,
        }
    } else {
        Style::GitSyntax {
            group: highlight,
            surface,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A remembered body reads exactly as a fresh one, at its own width only.
    #[test]
    fn remembered_bodies_match_a_fresh_layout() {
        let body = "# Title\n\nSome **bold** prose that wraps at narrow widths.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let (mut first, mut again, mut fresh, mut narrow) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        markdown(&mut first, body, 60, "  ");
        markdown(&mut again, body, 60, "  ");
        laid_out(&mut fresh, body, 60, "  ");
        markdown(&mut narrow, body, 24, "  ");
        assert_eq!(texts(&first), texts(&fresh));
        assert_eq!(texts(&again), texts(&fresh));
        assert_ne!(texts(&narrow), texts(&fresh));
    }

    fn texts(rows: &[Line]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", row.gutter, row.text))
            .collect()
    }

    #[test]
    fn headings_lists_checklists_and_code_read_like_the_mockup() {
        let mut rows = Vec::new();
        markdown(
            &mut rows,
            "## Context\nWhen a worker reconnects.\n\n- [ ] Resume\n- [x] Done\n\n```sh\n$ orbit worker\n```\n",
            80,
            "",
        );
        let text = texts(&rows);
        assert_eq!(text[0], "Context");
        assert_eq!(
            rows[0].spans.first().map(|(_, role)| *role),
            Some(Highlight::HEADING),
            "headings keep the accent without their marks"
        );
        assert!(text.iter().any(|row| row.ends_with("☐ Resume")), "{text:?}");
        assert!(text.iter().any(|row| row.ends_with("☑ Done")), "{text:?}");
        let code = rows
            .iter()
            .position(|row| row.text == "$ orbit worker")
            .unwrap();
        assert_eq!(rows[code].band, Some(GitSurface::ProseCode));
        // The fence's row names the language at its left with the editor's icon for it, on
        // the code surface like the rows under it; it is not a code line.
        let (icon, _) = crate::ui::render::markdown::fence_icon("sh").unwrap();
        assert_eq!(rows[code - 1].text, format!("{icon} sh"));
        assert_eq!(rows[code - 1].band, Some(GitSurface::ProseCode));
        assert!(rows[code - 1].source.is_none());
        assert_eq!(rows[code + 1].band, Some(GitSurface::ProseCode));
        assert!(rows.iter().all(|row| row.source.is_some()
            || row.band.is_some()
            || row.text.is_empty()
            || row.text.ends_with(" sh")));
        // r9 R8-13: a line wider than the reader pans rather than wraps. No hint sits in the
        // body copy (R16 gitea-05): a fence without a language opens with an empty code row.
        let line = "could not fetch remote 'origin': fatal: Unable to create '/data/.git/index.lock': File exists";
        let mut rows = Vec::new();
        markdown(&mut rows, &format!("```\n{line}\n```\n"), 60, "");
        let code = rows.iter().position(|row| row.text == line).unwrap();
        assert!(rows[code].pan, "{:?}", texts(&rows));
        assert_eq!(
            (rows[code - 1].text.as_str(), rows[code - 1].band),
            ("", Some(GitSurface::ProseCode))
        );
        assert!(
            !texts(&rows).iter().any(|row| row.contains("scroll")),
            "{:?}",
            texts(&rows)
        );
    }

    #[test]
    fn bodies_drop_bot_markup_and_reflow_soft_wrapped_paragraphs() {
        let body = "A reusable workflow (workflow_call) triggered by a pull\nrequest can write cache\nentries.\n\n<!--start release-notes-->\n<!--URL:x-->\n- PR: <!--number 1784 -->fix: set socket<!--d-->\n<!--end-->\n<details><summary>Logs</summary>\n\nlong\n</details>\nZml4OiBzZXQgY29udGFpbmVyIGRhZW1vbiBzb2NrZXQgdG8gIi0iIGlmIHRoZSBkZWZhdWx0IHNvY2tldA==\n```\nkeep\nthis <!-- -->\n```\n- one\n- two items that were wrapped at a column in the source\n  continued\n\nshort line\nkept apart";
        let cleaned = clean(body);
        assert!(cleaned.starts_with("A reusable workflow (workflow_call) triggered by a pull request can write cache entries.\n"), "{cleaned}");
        assert!(
            !cleaned.contains("<!--start") && !cleaned.contains("Zml4"),
            "{cleaned}"
        );
        assert!(cleaned.contains("- PR: fix: set socket"), "{cleaned}");
        assert!(
            cleaned.contains("▸ Logs · collapsed") && !cleaned.contains("long"),
            "{cleaned}"
        );
        assert!(
            cleaned.contains("keep\nthis <!-- -->"),
            "fenced code is literal: {cleaned}"
        );
        assert!(
            cleaned.contains(
                "- one\n- two items that were wrapped at a column in the source continued"
            ),
            "{cleaned}"
        );
        assert!(cleaned.contains("short line\nkept apart"), "{cleaned}");
    }

    #[test]
    fn titles_lose_code_marks_and_a_reported_draft_prefix() {
        assert_eq!(
            title("fix `TestRunner_Mixed` flake", false),
            "fix TestRunner_Mixed flake"
        );
        assert_eq!(
            title("Draft: Split nested group settings", true),
            "Split nested group settings"
        );
        assert_eq!(title("WIP: feat(jobparser): x", true), "feat(jobparser): x");
        assert_eq!(
            title("WIP: kept when not a draft", false),
            "WIP: kept when not a draft"
        );
        assert_eq!(
            title("bug:  Runner accepts\tsuccessful \n run", false),
            "bug: Runner accepts successful run"
        );
        let labels = [
            LabelChip {
                name: "wip".into(),
                rgb: None,
            },
            LabelChip {
                name: "bug".into(),
                rgb: None,
            },
        ];
        assert_eq!(chips(&labels, true).len(), 1);
        assert_eq!(chips(&labels, false).len(), 2);
    }

    #[test]
    fn long_urls_break_at_their_separators() {
        let text =
            "See https://gitea.com/orbit/orbit/src/branch/main/.gitea/workflow-lockfiles for it";
        let rows = super::wrap_words(text, 40);
        assert_eq!(rows.concat(), text);
        assert!(rows.iter().all(|row| row.width() <= 40), "{rows:?}");
        // Every break inside the URL falls at a separator.
        for pair in rows.windows(2) {
            let next = pair[1].chars().next().unwrap_or(' ');
            let last = pair[0].chars().last().unwrap_or(' ');
            assert!(next == '/' || last == ' ', "{rows:?}");
        }
        // r9 N-21: a URL breaks before a `/ ? & #` when the part after it fits; a part longer
        // than the row breaks before one of its hyphens, never inside a word.
        let url = "https://github.blog/changelog/2026-10-28-github-actions-workflow-lockfiles-are-generally-available/";
        for width in [40, 56, 57, 66, 68, 90] {
            let rows = super::wrap_words(url, width);
            assert_eq!(rows.concat(), url);
            assert!(
                rows.iter().all(|row| row.width() <= width),
                "{width}: {rows:?}"
            );
            for pair in rows.windows(2) {
                assert!(
                    pair[1].starts_with('/') || pair[1].starts_with('-'),
                    "{width}: {rows:?}"
                );
                assert!(
                    !pair[0].ends_with("https:") && !pair[0].ends_with("https:/"),
                    "{width}: {rows:?}"
                );
            }
        }
        assert_eq!(
            super::wrap_words(url, 68),
            [
                "https://github.blog/changelog",
                "/2026-10-28-github-actions-workflow-lockfiles-are-generally",
                "-available/"
            ]
        );
        assert_eq!(
            super::wrap_words(url, 57),
            [
                "https://github.blog/changelog",
                "/2026-10-28-github-actions-workflow-lockfiles-are",
                "-generally-available/"
            ]
        );
        // A part with no hyphen still meets the edge.
        assert_eq!(
            super::wrap_words("https://x.dev/abcdefghijklmnop", 10),
            ["https://x.", "dev", "/abcdefghi", "jklmnop"]
        );
        // Plain prose still breaks after a hyphen between words.
        assert_eq!(
            super::wrap_words("a well-known-thing", 9),
            ["a well-", "known-", "thing"]
        );
        assert_eq!(
            super::wrap_words("short words wrap as before", 11),
            ["short ", "words wrap ", "as before"]
        );
    }

    #[test]
    fn wrapped_list_items_hang_under_their_text() {
        let mut rows = Vec::new();
        markdown(
            &mut rows,
            "- one two three four five six seven eight nine ten",
            20,
            "",
        );
        assert!(rows.len() > 1);
        assert!(rows[1].gutter.chars().all(|c| c == ' ') && !rows[1].gutter.is_empty());
    }
}
