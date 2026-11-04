//! Markdown drawn in place of its markup, from the marks the syntax worker finds, as
//! render-markdown.nvim draws it. The cursor's line, and every line outside Normal mode, shows
//! its source.
use std::borrow::Cow;

use crate::{
    config::{MarkdownConfig, MarkdownGlyphs},
    editor::{
        BufferId, EditorMode,
        text::{ByteOffset, ByteRange, TextBuffer},
    },
    files::ExplorerIconColor,
    language::syntax::{Highlight, MarkdownMark, TablePart},
    ui::{Style, UiMarkdownMark, UiState},
};

/// The marks of one view's buffer, walked line by line from the top of the window down.
pub(super) struct MarkdownMarks<'a> {
    glyphs: &'a MarkdownGlyphs,
    text: &'a TextBuffer,
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
    table: Highlight,
}

/// What one line draws in place of its source.
pub(super) struct MarkdownLine<'a> {
    marks: Vec<(ByteRange, Shown<'a>)>,
    /// Code spans, drawn on the code background.
    shaded: Vec<ByteRange>,
    /// The background of the line's text and of the rest of its row.
    pub(super) fill: Option<Style>,
    /// Repeated across the row in place of a `---` break.
    pub(super) rule: Option<&'a str>,
}

enum Shown<'a> {
    Hidden,
    /// Drawn in place of the mark's text.
    Glyph(Cow<'a, str>, Style),
    /// Drawn before the mark's text, which stays.
    Prefix(Cow<'a, str>, Style),
}

pub(super) enum Drawn<'a> {
    Hidden,
    Glyph(&'a str, Style),
    Prefix(&'a str, Style),
}

impl<'a> MarkdownMarks<'a> {
    /// `None` when the buffer has no marks or rendering is off.
    pub(super) fn new(
        ui: &'a UiState,
        buffer: BufferId,
        text: &'a TextBuffer,
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
        let revision = text.revision();
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
            glyphs: &config.glyphs,
            text,
            marks,
            next: 0,
            blocks: blocks.into_iter().map(|mark| mark.range).collect(),
            cursor_line,
            headings: [1, 2, 3, 4, 5, 6].map(|level| {
                group(
                    &format!("@markup.heading.{level}.markdown"),
                    Highlight::HEADING,
                )
            }),
            list,
            unchecked: group("@markup.list.unchecked", list),
            checked: group("@markup.list.checked", list),
            link: group("@markup.link.label", Highlight::LINK),
            table: group("@punctuation.special", Highlight::KEYWORD),
        })
    }

    /// What the line from `start`, `length` bytes long without its newline, draws. Lines are
    /// asked for in order.
    pub(super) fn line(
        &mut self,
        line: usize,
        start: ByteOffset,
        length: usize,
    ) -> Option<MarkdownLine<'a>> {
        let end = ByteOffset(start.get().saturating_add(length));
        while self
            .marks
            .get(self.next)
            .is_some_and(|mark| mark.range.start < start)
        {
            self.next = self.next.saturating_add(1);
        }
        let first = self.next;
        while self
            .marks
            .get(self.next)
            .is_some_and(|mark| mark.range.start < end)
        {
            self.next = self.next.saturating_add(1);
        }
        if line == self.cursor_line {
            return None;
        }
        let glyphs = self.glyphs;
        let code = self
            .blocks
            .iter()
            .any(|block| block.start <= end && block.end > start);
        let mut drawn = MarkdownLine {
            marks: Vec::new(),
            shaded: Vec::new(),
            fill: code.then_some(Style::MarkdownCode(None)),
            rule: None,
        };
        let glyph = |text: &'a str, style| Shown::Glyph(Cow::Borrowed(text), style);
        // Cells shrink by the markup they hide, so a table's next border is pushed right by as
        // much, which keeps its columns lined up as render-markdown pads them.
        let mut hidden = 0isize;
        for mark in &self.marks[first..self.next] {
            let shown = match mark.mark {
                MarkdownMark::Hide | MarkdownMark::Code => Shown::Hidden,
                MarkdownMark::Heading(level) => {
                    let level = usize::from(level.clamp(1, 6)) - 1;
                    let Some(icon) = glyphs
                        .headings
                        .get(level.min(glyphs.headings.len().saturating_sub(1)))
                    else {
                        continue;
                    };
                    glyph(icon, Style::MarkdownHeading(self.headings[level]))
                }
                MarkdownMark::HeadingLine(level) => {
                    let level = usize::from(level.clamp(1, 6)) - 1;
                    drawn.fill = Some(Style::MarkdownHeading(self.headings[level]));
                    continue;
                }
                MarkdownMark::Bullet(depth) => {
                    let Some(bullet) = glyphs.bullets.get(depth % glyphs.bullets.len().max(1))
                    else {
                        continue;
                    };
                    glyph(bullet, Style::Syntax(self.list))
                }
                MarkdownMark::Checkbox { checked: false } => {
                    glyph(&glyphs.unchecked, Style::Syntax(self.unchecked))
                }
                MarkdownMark::Checkbox { checked: true } => {
                    glyph(&glyphs.checked, Style::Syntax(self.checked))
                }
                MarkdownMark::Quote { callout } => {
                    let style = callout
                        .and_then(|at| self.callout(at))
                        .map_or(Style::Syntax(Highlight::QUOTE), |(_, style)| style);
                    glyph(&glyphs.quote, style)
                }
                MarkdownMark::Callout => {
                    let Some((title, style)) = self.callout(mark.range.start.get()) else {
                        continue;
                    };
                    glyph(title, style)
                }
                MarkdownMark::Link { url } => {
                    let icon = url
                        .and_then(|url| self.link_icon(url))
                        .unwrap_or(&glyphs.link);
                    glyph(icon, Style::Syntax(self.link))
                }
                MarkdownMark::Image => glyph(&glyphs.image, Style::Syntax(self.link)),
                MarkdownMark::InlineCode => {
                    drawn.shaded.push(mark.range);
                    continue;
                }
                MarkdownMark::CodeLanguage => {
                    let Some((icon, color)) = self.language_icon(mark.range) else {
                        continue;
                    };
                    Shown::Prefix(
                        Cow::Owned(format!("{icon} ")),
                        Style::MarkdownCode(Some(icon_group(color))),
                    )
                }
                MarkdownMark::Rule => {
                    drawn.rule = Some(glyphs.rule.as_str()).filter(|rule| !rule.is_empty());
                    Shown::Hidden
                }
                MarkdownMark::Table(part) => {
                    let index = match part {
                        TablePart::Pipe => 0,
                        TablePart::Line => 1,
                        TablePart::Left => 2,
                        TablePart::Cross => 3,
                        TablePart::Right => 4,
                    };
                    let Some(border) = glyphs.table.get(index) else {
                        continue;
                    };
                    let pad = usize::try_from(std::mem::take(&mut hidden)).unwrap_or(0);
                    let border = if pad == 0 {
                        Cow::Borrowed(border.as_str())
                    } else {
                        Cow::Owned(format!("{}{border}", " ".repeat(pad)))
                    };
                    Shown::Glyph(border, Style::Syntax(self.table))
                }
            };
            let source = isize::try_from(mark.range.len()).unwrap_or(0);
            hidden += match &shown {
                _ if matches!(mark.mark, MarkdownMark::Table(_)) => 0,
                Shown::Hidden => source,
                Shown::Glyph(glyph, _) => {
                    source - isize::try_from(glyph.chars().count()).unwrap_or(0)
                }
                Shown::Prefix(glyph, _) => -isize::try_from(glyph.chars().count()).unwrap_or(0),
            };
            drawn.marks.push((mark.range, shown));
        }
        Some(drawn)
    }

    /// The configured title and colour of the callout whose `[!NAME]` starts at `at`.
    fn callout(&self, at: usize) -> Option<(&'a str, Style)> {
        let end = (at + 32).min(self.text.len_bytes());
        let text = self
            .text
            .slice(ByteRange::new(ByteOffset(at), ByteOffset(end)).ok()?)
            .ok()?;
        let name = text
            .strip_prefix("[!")?
            .split_once(']')?
            .0
            .to_ascii_lowercase();
        let callout = self.glyphs.callouts.get(&name)?;
        let group = Highlight::named(&callout.group).unwrap_or(Highlight::QUOTE);
        Some((callout.text.as_str(), Style::Syntax(group)))
    }

    /// The icon of the longest configured address part the link's address contains.
    fn link_icon(&self, (start, end): (usize, usize)) -> Option<&'a str> {
        let url = self
            .text
            .slice(ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()?)
            .ok()?;
        self.glyphs
            .links
            .iter()
            .filter(|(part, _)| url.contains(part.as_str()))
            .max_by_key(|(part, _)| part.len())
            .map(|(_, icon)| icon.as_str())
    }

    /// A fence's language icon, found as the explorer finds a file's.
    fn language_icon(&self, range: ByteRange) -> Option<(&'static str, ExplorerIconColor)> {
        let name = self.text.slice(range).ok()?.to_ascii_lowercase();
        let extension = match name.as_str() {
            "rust" => "rs",
            "python" => "py",
            "javascript" | "node" => "js",
            "typescript" => "ts",
            "bash" | "shell" | "zsh" | "console" => "sh",
            "golang" => "go",
            "ruby" => "rb",
            "c++" => "cpp",
            "markdown" => "md",
            "kotlin" => "kt",
            "haskell" => "hs",
            "elixir" => "ex",
            other => other,
        };
        crate::files::file_icon(&format!("x.{extension}")).map(|icon| (icon.glyph, icon.color))
    }
}

/// The syntax group an explorer icon colour is drawn with, so it follows the theme.
fn icon_group(color: ExplorerIconColor) -> Highlight {
    match color {
        ExplorerIconColor::Azure | ExplorerIconColor::Blue => Highlight::FUNCTION,
        ExplorerIconColor::Cyan => Highlight::PROPERTY,
        ExplorerIconColor::Green => Highlight::STRING,
        ExplorerIconColor::Grey => Highlight::COMMENT,
        ExplorerIconColor::Orange | ExplorerIconColor::Red => Highlight::NUMBER,
        ExplorerIconColor::Purple => Highlight::KEYWORD,
        ExplorerIconColor::Yellow => Highlight::TYPE,
    }
}

impl MarkdownLine<'_> {
    /// What the grapheme at `at` draws: nothing, a mark's glyph where the mark starts, or a
    /// glyph before itself.
    pub(super) fn at(&self, at: ByteOffset) -> Option<Drawn<'_>> {
        let (range, shown) = self.marks.iter().find(|(range, _)| range.contains(at))?;
        match shown {
            Shown::Glyph(glyph, style) if range.start == at => Some(Drawn::Glyph(glyph, *style)),
            Shown::Prefix(glyph, style) if range.start == at => Some(Drawn::Prefix(glyph, *style)),
            Shown::Prefix(..) => None,
            Shown::Hidden | Shown::Glyph(..) => Some(Drawn::Hidden),
        }
    }

    /// The source's own colour at `at`, on the line's heading bar or code background.
    pub(super) fn restyle(&self, at: ByteOffset, style: Style) -> Style {
        let fill = if self.shaded.iter().any(|range| range.contains(at)) {
            Some(Style::MarkdownCode(None))
        } else {
            self.fill
        };
        match (fill, style) {
            (Some(Style::MarkdownHeading(group)), Style::Default | Style::Syntax(_)) => {
                Style::MarkdownHeading(group)
            }
            (Some(Style::MarkdownCode(_)), Style::Default) => Style::MarkdownCode(None),
            (Some(Style::MarkdownCode(_)), Style::Syntax(group)) => {
                Style::MarkdownCode(Some(group))
            }
            _ => style,
        }
    }
}
