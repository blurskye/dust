//! Markdown drawn in place of its markup, from the marks the syntax worker finds, as
//! render-markdown.nvim draws it. The cursor's line, and every line outside Normal mode, shows
//! its source.
use crate::{
    config::MarkdownConfig,
    editor::{
        BufferId, EditorMode,
        text::{BufferRevision, ByteOffset, ByteRange},
    },
    language::syntax::{Highlight, MarkdownMark},
    ui::{Style, UiMarkdownMark, UiState},
};

/// The marks of one view's buffer, walked line by line from the top of the window down.
pub(super) struct MarkdownMarks<'a> {
    config: &'a MarkdownConfig,
    marks: Vec<&'a UiMarkdownMark>,
    next: usize,
    /// Code blocks span lines, so they are looked up apart from the marks within a line.
    blocks: Vec<ByteRange>,
    cursor_line: usize,
    headings: [Highlight; 6],
    list: Highlight,
    unchecked: Highlight,
    checked: Highlight,
    link: Highlight,
}

/// What one line draws in place of its source.
pub(super) struct MarkdownLine<'a> {
    marks: Vec<(ByteRange, Option<(&'a str, Style)>)>,
    /// The background of the line's text and of the rest of its row.
    pub(super) fill: Option<Style>,
    /// Repeated across the row in place of a `---` break.
    pub(super) rule: Option<&'a str>,
}

pub(super) enum Drawn<'a> {
    Hidden,
    Glyph(&'a str, Style),
}

impl<'a> MarkdownMarks<'a> {
    /// `None` when the buffer has no marks or rendering is off.
    pub(super) fn new(
        ui: &'a UiState,
        buffer: BufferId,
        revision: BufferRevision,
        mode: EditorMode,
        cursor_line: usize,
        config: &'a MarkdownConfig,
    ) -> Option<Self> {
        let shown = config.render
            && matches!(
                mode,
                EditorMode::Normal
                    | EditorMode::OperatorPending
                    | EditorMode::CommandLine
                    | EditorMode::Prompt
            );
        if !shown {
            return None;
        }
        let (blocks, marks): (Vec<_>, Vec<_>) = ui
            .markdown_marks
            .iter()
            .filter(|mark| mark.buffer == buffer && mark.revision == revision)
            .partition(|mark| mark.mark == MarkdownMark::Code);
        if blocks.is_empty() && marks.is_empty() {
            return None;
        }
        let group = |name: &str, fallback| Highlight::named(name).unwrap_or(fallback);
        let list = group("@markup.list", Highlight::KEYWORD);
        Some(Self {
            config,
            marks,
            next: 0,
            blocks: blocks.into_iter().map(|mark| mark.range).collect(),
            cursor_line,
            headings: [1, 2, 3, 4, 5, 6]
                .map(|level| group(&format!("@markup.heading.{level}.markdown"), Highlight::HEADING)),
            list,
            unchecked: group("@markup.list.unchecked", list),
            checked: group("@markup.list.checked", list),
            link: group("@markup.link.label", Highlight::LINK),
        })
    }

    /// What the line from `start`, `length` bytes long without its newline, draws. Lines are
    /// asked for in order.
    pub(super) fn line(&mut self, line: usize, start: ByteOffset, length: usize) -> Option<MarkdownLine<'a>> {
        let end = ByteOffset(start.get().saturating_add(length));
        while self.marks.get(self.next).is_some_and(|mark| mark.range.start < start) {
            self.next = self.next.saturating_add(1);
        }
        let first = self.next;
        while self.marks.get(self.next).is_some_and(|mark| mark.range.start < end) {
            self.next = self.next.saturating_add(1);
        }
        if line == self.cursor_line {
            return None;
        }
        let config = &self.config.glyphs;
        let code = self
            .blocks
            .iter()
            .any(|block| block.start <= end && block.end > start);
        let mut drawn = MarkdownLine {
            marks: Vec::new(),
            fill: code.then_some(Style::MarkdownCode(None)),
            rule: None,
        };
        for mark in &self.marks[first..self.next] {
            let glyph = match mark.mark {
                MarkdownMark::Hide | MarkdownMark::Code => None,
                MarkdownMark::Heading(level) => {
                    let level = usize::from(level.clamp(1, 6)) - 1;
                    let Some(icon) = config.headings.get(level.min(config.headings.len().saturating_sub(1))) else {
                        continue;
                    };
                    Some((icon.as_str(), Style::MarkdownHeading(self.headings[level])))
                }
                MarkdownMark::HeadingLine(level) => {
                    let level = usize::from(level.clamp(1, 6)) - 1;
                    drawn.fill = Some(Style::MarkdownHeading(self.headings[level]));
                    continue;
                }
                MarkdownMark::Bullet(depth) => {
                    let Some(bullet) = config.bullets.get(depth % config.bullets.len().max(1)) else {
                        continue;
                    };
                    Some((bullet.as_str(), Style::Syntax(self.list)))
                }
                MarkdownMark::Checkbox { checked: false } => {
                    Some((config.unchecked.as_str(), Style::Syntax(self.unchecked)))
                }
                MarkdownMark::Checkbox { checked: true } => {
                    Some((config.checked.as_str(), Style::Syntax(self.checked)))
                }
                MarkdownMark::Quote => Some((config.quote.as_str(), Style::Syntax(Highlight::QUOTE))),
                MarkdownMark::Link => Some((config.link.as_str(), Style::Syntax(self.link))),
                MarkdownMark::Image => Some((config.image.as_str(), Style::Syntax(self.link))),
                MarkdownMark::Rule => {
                    drawn.rule = Some(config.rule.as_str()).filter(|rule| !rule.is_empty());
                    None
                }
            };
            drawn.marks.push((mark.range, glyph));
        }
        Some(drawn)
    }
}

impl<'a> MarkdownLine<'a> {
    /// What the grapheme at `at` draws instead of itself: nothing, or a mark's glyph where the
    /// mark starts.
    pub(super) fn at(&self, at: ByteOffset) -> Option<Drawn<'a>> {
        let (range, glyph) = self.marks.iter().find(|(range, _)| range.contains(at))?;
        Some(match glyph {
            Some((glyph, style)) if range.start == at => Drawn::Glyph(glyph, *style),
            _ => Drawn::Hidden,
        })
    }

    /// The source's own colour, on the line's heading bar or code background.
    pub(super) fn restyle(&self, style: Style) -> Style {
        match (self.fill, style) {
            (Some(Style::MarkdownHeading(group)), Style::Default | Style::Syntax(_)) => {
                Style::MarkdownHeading(group)
            }
            (Some(Style::MarkdownCode(_)), Style::Default) => Style::MarkdownCode(None),
            (Some(Style::MarkdownCode(_)), Style::Syntax(group)) => Style::MarkdownCode(Some(group)),
            _ => style,
        }
    }
}
