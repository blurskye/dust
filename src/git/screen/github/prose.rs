//! Forge reader rows (DESIGN.md §3.26/§3.33/§3.34): Markdown bodies, metadata tables, card
//! headers and links as `lines::Line`s that carry their own Git styles. The painter in
//! `paint.rs` draws them on the reader surface.
use super::find;
use super::lines::Line;
use crate::config::{AmbiguousWidth, Highlight};
use crate::forge::records::LabelChip;
use crate::ui::{Emphasis, GitRole, GitSurface, Style};
use std::ops::Range;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

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
        self.push(&format!(" {text} "), style)
    }
    pub(super) fn labels(mut self, labels: &[LabelChip]) -> Self {
        for (index, label) in labels.iter().enumerate() {
            if index > 0 {
                self = self.plain(" ");
            }
            self = self.chip(&label.name, label_style(label));
        }
        self
    }
    pub(super) fn width(&self) -> usize {
        self.text.width()
    }
    /// Pads with spaces to `column` cells (a table's value column).
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
    Rich::new().push(&"─".repeat(width.max(1)), role(GitRole::Border)).line()
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
    Rich::new().push(label, role(GitRole::Muted)).to_column(column)
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
        let info = trimmed.trim_start_matches(fence.chars().next().unwrap()).trim();
        let first = lines.next().unwrap_or_default();
        if !first.trim_start().starts_with(fence) {
            found.push((info.split_whitespace().next().unwrap_or("").to_owned(), first.to_owned()));
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
/// `Draft:`/`WIP:` prefix its Draft chip already says (§3.26: one draft marker).
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
    text.replace('`', "")
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
/// state in them), `<details>` blocks (collapsed to their summary), base64 blobs and layout
/// tags. Paragraph lines are reflowed: a single newline inside a paragraph is a space, as
/// GitHub's renderer and glamour treat it, so the pane width decides the wrap. Fenced and
/// indented code keep every byte.
pub(super) fn clean(text: &str) -> String {
    use std::sync::LazyLock;
    static COMMENT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?s)<!--.*?-->").unwrap());
    static DETAILS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?is)<details[^>]*>\s*(?:<summary[^>]*>(.*?)</summary>)?.*?</details>").unwrap()
    });
    static TAG: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)</?(?:sub|sup|b|i|em|strong|p|div|span|kbd|picture|source|center|summary|details)(?:\s[^>]*)?/?>")
            .unwrap()
    });
    static BREAK: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)<br\s*/?>").unwrap());
    // Split into fenced code and the rest; only the rest is cleaned.
    let mut parts: Vec<(bool, String)> = Vec::new();
    let mut fence: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker));
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
            format!("\n▸ {} · collapsed\n", TAG.replace_all(summary, ""))
        });
        let part = BREAK.replace_all(&part, "\n");
        let part = TAG.replace_all(&part, "");
        out.push_str(&reflow(&part));
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
            && line.chars().all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
            && line.chars().any(|c| c.is_ascii_digit())
    };
    let block = |line: &str| {
        let trimmed = line.trim_start();
        line.starts_with("    ")
            || line.starts_with('\t')
            || trimmed.starts_with(['#', '>', '|', '-', '*', '+', '▸', '=', '<'])
            || trimmed.split_once(['.', ')']).is_some_and(|(number, _)| {
                !number.is_empty() && number.len() <= 3 && number.chars().all(|c| c.is_ascii_digit())
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
                .map(|(range, role)| (range.start.max(from) - from..range.end.min(to) - from, *role))
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
                let mut top = Rich::new()
                    .push(&language.unwrap_or_default(), role(GitRole::Muted))
                    .line();
                top.band = Some(GitSurface::Code);
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
                row.band = Some(GitSurface::Code);
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
                bottom.band = Some(GitSurface::Code);
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
            crate::ui::render::wrap_plain_text(body_text, room, AmbiguousWidth::Narrow)
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
            let row_end = row_start + if fragment_index == 0 { lead + fragment.len() } else { fragment.len() };
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
                GitSurface::Code
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

    fn texts(rows: &[Line]) -> Vec<String> {
        rows.iter().map(|row| format!("{}{}", row.gutter, row.text)).collect()
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
        let code = rows.iter().position(|row| row.text == "$ orbit worker").unwrap();
        assert_eq!(rows[code].band, Some(GitSurface::Code));
        assert_eq!(rows[code - 1].text, "sh");
        assert_eq!(rows[code + 1].band, Some(GitSurface::Code));
        assert!(rows.iter().all(|row| row.source.is_some() || row.band.is_some() || row.text.is_empty() || row.text == "sh"));
    }

    #[test]
    fn bodies_drop_bot_markup_and_reflow_soft_wrapped_paragraphs() {
        let body = "A reusable workflow (workflow_call) triggered by a pull\nrequest can write cache\nentries.\n\n<!--start release-notes-->\n<!--URL:x-->\n- PR: <!--number 1784 -->fix: set socket<!--d-->\n<!--end-->\n<details><summary>Logs</summary>\n\nlong\n</details>\nZml4OiBzZXQgY29udGFpbmVyIGRhZW1vbiBzb2NrZXQgdG8gIi0iIGlmIHRoZSBkZWZhdWx0IHNvY2tldA==\n```\nkeep\nthis <!-- -->\n```\n- one\n- two items that were wrapped at a column in the source\n  continued\n\nshort line\nkept apart";
        let cleaned = clean(body);
        assert!(cleaned.starts_with("A reusable workflow (workflow_call) triggered by a pull request can write cache entries.\n"), "{cleaned}");
        assert!(!cleaned.contains("<!--start") && !cleaned.contains("Zml4"), "{cleaned}");
        assert!(cleaned.contains("- PR: fix: set socket"), "{cleaned}");
        assert!(cleaned.contains("▸ Logs · collapsed") && !cleaned.contains("long"), "{cleaned}");
        assert!(cleaned.contains("keep\nthis <!-- -->"), "fenced code is literal: {cleaned}");
        assert!(cleaned.contains("- one\n- two items that were wrapped at a column in the source continued"), "{cleaned}");
        assert!(cleaned.contains("short line\nkept apart"), "{cleaned}");
    }

    #[test]
    fn titles_lose_code_marks_and_a_reported_draft_prefix() {
        assert_eq!(title("fix `TestRunner_Mixed` flake", false), "fix TestRunner_Mixed flake");
        assert_eq!(title("Draft: Split nested group settings", true), "Split nested group settings");
        assert_eq!(title("WIP: feat(jobparser): x", true), "feat(jobparser): x");
        assert_eq!(title("WIP: kept when not a draft", false), "WIP: kept when not a draft");
        let labels = [LabelChip { name: "wip".into(), rgb: None }, LabelChip { name: "bug".into(), rgb: None }];
        assert_eq!(chips(&labels, true).len(), 1);
        assert_eq!(chips(&labels, false).len(), 2);
    }

    #[test]
    fn wrapped_list_items_hang_under_their_text() {
        let mut rows = Vec::new();
        markdown(&mut rows, "- one two three four five six seven eight nine ten", 20, "");
        assert!(rows.len() > 1);
        assert!(rows[1].gutter.chars().all(|c| c == ' ') && !rows[1].gutter.is_empty());
    }
}
