//! Markdown drawn in place of its markup, from the marks the syntax worker finds, as
//! render-markdown.nvim draws it. The cursor's line, and every line outside Normal mode, shows
//! its source.
use std::borrow::Cow;

use crate::{
    config::{AmbiguousWidth, MarkdownConfig, MarkdownGlyphs},
    editor::{
        BufferId, EditorMode,
        text::{ByteOffset, ByteRange, TextBuffer},
    },
    files::ExplorerIconColor,
    language::syntax::{Highlight, MarkdownMark},
    ui::{Style, UiMarkdownMark, UiState, geometry::text_width},
};

/// A table's border pieces when the configured list is short, in render-markdown's order:
/// the top, the row under the header, the bottom, then the line between cells and across.
const TABLE: [&str; 11] = ["┌", "┬", "┐", "├", "┼", "┤", "└", "┴", "┘", "│", "─"];

/// The marks of one view's buffer, walked line by line from the top of the window down.
pub(super) struct MarkdownMarks<'a> {
    glyphs: &'a MarkdownGlyphs,
    text: &'a TextBuffer,
    marks: Vec<&'a UiMarkdownMark>,
    next: usize,
    /// Code blocks span lines, so they are looked up apart from the marks within a line.
    blocks: Vec<ByteRange>,
    tables: Vec<Table>,
    cursor_line: usize,
    headings: [Highlight; 6],
    list: Highlight,
    unchecked: Highlight,
    checked: Highlight,
    link: Highlight,
    table: Highlight,
}

/// A table laid out as a whole, so every cell is padded to its column's widest, however its
/// source is aligned.
struct Table {
    first_line: usize,
    last_line: usize,
    /// Each line's cells, without the spaces around them; the second line is the row under the
    /// header.
    rows: Vec<Vec<ByteRange>>,
    /// The drawn width of each cell, as `rows`.
    cell_widths: Vec<Vec<usize>>,
    widths: Vec<usize>,
    alignments: Vec<Alignment>,
}

#[derive(Clone, Copy, PartialEq)]
enum Alignment {
    Default,
    Left,
    Center,
    Right,
}

/// What one line draws in place of its source.
pub(super) struct MarkdownLine<'a> {
    marks: Vec<(ByteRange, Shown<'a>)>,
    /// Drawn before the source at the offset, as a code fence's language icon or a table's
    /// border and padding.
    inserts: Vec<(ByteOffset, Cow<'a, str>, Style)>,
    /// Drawn after the line's text, as the end of a table row or a border on a blank line.
    pub(super) after: Option<(String, Style)>,
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
        text: &'a TextBuffer,
        mode: EditorMode,
        cursor_line: usize,
        config: &'a MarkdownConfig,
        ambiguous: AmbiguousWidth,
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
            .partition(|mark| matches!(mark.mark, MarkdownMark::Code | MarkdownMark::Table));
        if blocks.is_empty() && marks.is_empty() {
            return None;
        }
        let (tables, blocks): (Vec<_>, Vec<_>) = blocks
            .into_iter()
            .partition(|mark| mark.mark == MarkdownMark::Table);
        let tables = tables
            .iter()
            .filter_map(|table| Table::new(text, &marks, table.range, &config.glyphs, ambiguous))
            .collect();
        let group = |name: &str, fallback| Highlight::named(name).unwrap_or(fallback);
        let list = group("@markup.list", Highlight::KEYWORD);
        Some(Self {
            glyphs: &config.glyphs,
            text,
            marks,
            next: 0,
            blocks: blocks.into_iter().map(|mark| mark.range).collect(),
            tables,
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
            inserts: Vec::new(),
            after: None,
            shaded: Vec::new(),
            fill: code.then_some(Style::MarkdownCode(None)),
            rule: None,
        };
        self.table_line(line, start, end, &mut drawn);
        let glyph = |text: &'a str, style| Shown::Glyph(Cow::Borrowed(text), style);
        for mark in &self.marks[first..self.next] {
            let shown = match mark.mark {
                MarkdownMark::Hide | MarkdownMark::Code | MarkdownMark::Table => Shown::Hidden,
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
                        .and_then(|url| link_icon(self.text, glyphs, url))
                        .unwrap_or(&glyphs.link);
                    glyph(icon, Style::Syntax(self.link))
                }
                MarkdownMark::Image => glyph(&glyphs.image, Style::Syntax(self.link)),
                MarkdownMark::InlineCode => {
                    drawn.shaded.push(mark.range);
                    continue;
                }
                MarkdownMark::CodeLanguage => {
                    if let Some((icon, color)) = self.language_icon(mark.range) {
                        drawn.inserts.push((
                            mark.range.start,
                            Cow::Owned(format!("{icon} ")),
                            Style::MarkdownCode(Some(icon_group(color))),
                        ));
                    }
                    continue;
                }
                MarkdownMark::Rule => {
                    drawn.rule = Some(glyphs.rule.as_str()).filter(|rule| !rule.is_empty());
                    Shown::Hidden
                }
                // Drawn with the rest of the table's border by `table_line`.
                MarkdownMark::TablePipe => continue,
            };
            drawn.marks.push((mark.range, shown));
        }
        Some(drawn)
    }

    /// A table row drawn with each cell padded to its column, the row under the header and the
    /// borders above and below the table drawn on the blank lines around it, as render-markdown
    /// draws them.
    fn table_line(
        &self,
        line: usize,
        start: ByteOffset,
        end: ByteOffset,
        drawn: &mut MarkdownLine,
    ) {
        let style = Style::Syntax(self.table);
        let whole = ByteRange { start, end };
        if let Some(table) = self
            .tables
            .iter()
            .find(|table| (table.first_line..=table.last_line).contains(&line))
        {
            let row = line - table.first_line;
            let cells = &table.rows[row];
            if row == 1 || cells.is_empty() {
                drawn.marks.push((whole, Shown::Hidden));
                drawn.after = Some((self.border(table, 3, 4, 5, true), style));
                return;
            }
            let pipe = self.part(9);
            let mut gap_start = start;
            let mut trailing = 0;
            for (column, cell) in cells.iter().enumerate() {
                let spare = table.widths[column].saturating_sub(table.cell_widths[row][column]);
                let leading = match table.alignments[column] {
                    Alignment::Default | Alignment::Left => 0,
                    Alignment::Center => spare / 2,
                    Alignment::Right => spare,
                };
                let border = if column == 0 {
                    format!("{pipe} {}", " ".repeat(leading))
                } else {
                    format!("{} {pipe} {}", " ".repeat(trailing), " ".repeat(leading))
                };
                if gap_start < cell.start {
                    let gap = ByteRange {
                        start: gap_start,
                        end: cell.start,
                    };
                    drawn.marks.push((gap, Shown::Hidden));
                }
                drawn.inserts.push((cell.start, Cow::Owned(border), style));
                trailing = spare - leading;
                gap_start = cell.end;
            }
            if gap_start < end {
                let gap = ByteRange {
                    start: gap_start,
                    end,
                };
                drawn.marks.push((gap, Shown::Hidden));
            }
            // A row with fewer cells than its header is drawn with empty ones.
            let mut after = format!("{} {pipe}", " ".repeat(trailing));
            for width in &table.widths[cells.len()..] {
                after.push_str(&" ".repeat(width + 2));
                after.push_str(pipe);
            }
            drawn.after = Some((after, style));
            return;
        }
        let blank = self
            .text
            .slice(whole)
            .is_ok_and(|text| text.trim().is_empty());
        if !blank {
            return;
        }
        let border =
            if let Some(table) = self.tables.iter().find(|table| table.last_line + 1 == line) {
                self.border(table, 6, 7, 8, false)
            } else if let Some(table) = self
                .tables
                .iter()
                .find(|table| table.first_line == line + 1)
            {
                self.border(table, 0, 1, 2, false)
            } else {
                return;
            };
        if start < end {
            drawn.marks.push((whole, Shown::Hidden));
        }
        drawn.after = Some((border, style));
    }

    /// A border across the table from the pieces at `left`, `cross` and `right`. The row under
    /// the header shows each column's alignment.
    fn border(
        &self,
        table: &Table,
        left: usize,
        cross: usize,
        right: usize,
        aligned: bool,
    ) -> String {
        let line = self.part(10);
        let indicator = self.glyphs.table_alignment.as_str();
        let mut border = self.part(left).to_owned();
        for (column, width) in table.widths.iter().enumerate() {
            if column > 0 {
                border.push_str(self.part(cross));
            }
            let alignment = if aligned {
                table.alignments[column]
            } else {
                Alignment::Default
            };
            let (start, end) = match alignment {
                Alignment::Default => (false, false),
                Alignment::Left => (true, false),
                Alignment::Center => (true, true),
                Alignment::Right => (false, true),
            };
            border.push_str(if start { indicator } else { line });
            border.push_str(&line.repeat(*width));
            border.push_str(if end { indicator } else { line });
        }
        border.push_str(self.part(right));
        border
    }

    fn part(&self, index: usize) -> &str {
        self.glyphs
            .table
            .get(index)
            .map_or(TABLE[index], String::as_str)
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

/// The icon of the longest configured address part the link's address contains.
fn link_icon<'a>(
    text: &TextBuffer,
    glyphs: &'a MarkdownGlyphs,
    (start, end): (usize, usize),
) -> Option<&'a str> {
    let url = text
        .slice(ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()?)
        .ok()?;
    glyphs
        .links
        .iter()
        .filter(|(part, _)| url.contains(part.as_str()))
        .max_by_key(|(part, _)| part.len())
        .map(|(_, icon)| icon.as_str())
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
            Shown::Hidden | Shown::Glyph(..) => Some(Drawn::Hidden),
        }
    }

    /// What is drawn before the grapheme at `at`, whether or not the grapheme itself is.
    pub(super) fn before(&self, at: ByteOffset) -> impl Iterator<Item = (&str, Style)> {
        self.inserts
            .iter()
            .filter(move |(offset, _, _)| *offset == at)
            .map(|(_, text, style)| (text.as_ref(), *style))
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

impl Table {
    fn new(
        text: &TextBuffer,
        marks: &[&UiMarkdownMark],
        range: ByteRange,
        glyphs: &MarkdownGlyphs,
        ambiguous: AmbiguousWidth,
    ) -> Option<Self> {
        let first_line = text.byte_to_line(range.start).ok()?;
        let last_line = text
            .byte_to_line(ByteOffset(
                range.end.get().saturating_sub(1).max(range.start.get()),
            ))
            .ok()?;
        let mut rows = Vec::new();
        let mut cell_widths: Vec<Vec<usize>> = Vec::new();
        for line in first_line..=last_line {
            let start = text.line_to_byte(line).ok()?;
            let source = text.line(line).ok()?;
            let source = source.trim_end_matches(['\n', '\r']);
            let end = ByteOffset(start.get() + source.len());
            let pipes = marks[marks.partition_point(|mark| mark.range.start < start)..]
                .iter()
                .take_while(|mark| mark.range.start < end)
                .filter(|mark| mark.mark == MarkdownMark::TablePipe)
                .map(|mark| mark.range);
            // The text between pipes, and before the first and after the last when a row
            // leaves out its outer pipes.
            let mut segments = Vec::new();
            let mut from = start;
            for pipe in pipes {
                segments.push(ByteRange {
                    start: from,
                    end: pipe.start,
                });
                from = pipe.end;
            }
            segments.push(ByteRange { start: from, end });
            let blank = |segment: &ByteRange| {
                text.slice(*segment)
                    .is_ok_and(|segment| segment.trim().is_empty())
            };
            if segments.len() > 1 && segments.first().is_some_and(blank) {
                segments.remove(0);
            }
            if segments.len() > 1 && segments.last().is_some_and(blank) {
                segments.pop();
            }
            let cells: Vec<ByteRange> = segments
                .into_iter()
                .map(|segment| {
                    let source = text.slice(segment).unwrap_or_default();
                    let leading = source.len() - source.trim_start().len();
                    let trimmed = source.trim().len();
                    let start = ByteOffset(segment.start.get() + leading);
                    ByteRange {
                        start,
                        end: ByteOffset(start.get() + trimmed),
                    }
                })
                .collect();
            cell_widths.push(
                cells
                    .iter()
                    .map(|cell| drawn_width(text, marks, *cell, glyphs, ambiguous))
                    .collect(),
            );
            rows.push(cells);
        }
        let alignments = rows
            .get(1)
            .into_iter()
            .flatten()
            .map(|cell| {
                let source = text.slice(*cell).unwrap_or_default();
                match (
                    source.starts_with(':'),
                    source.len() > 1 && source.ends_with(':'),
                ) {
                    (true, true) => Alignment::Center,
                    (true, false) => Alignment::Left,
                    (false, true) => Alignment::Right,
                    (false, false) => Alignment::Default,
                }
            })
            .collect::<Vec<_>>();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![1; columns];
        for (_, cells) in cell_widths.iter().enumerate().filter(|(row, _)| *row != 1) {
            for (column, width) in cells.iter().enumerate() {
                widths[column] = widths[column].max(*width);
            }
        }
        let mut alignments = alignments;
        alignments.resize(columns, Alignment::Default);
        Some(Self {
            first_line,
            last_line,
            rows,
            cell_widths,
            widths,
            alignments,
        })
    }
}

/// The width a cell's text is drawn at, without the markup it hides and with the icons drawn
/// in place of its links.
fn drawn_width(
    text: &TextBuffer,
    marks: &[&UiMarkdownMark],
    cell: ByteRange,
    glyphs: &MarkdownGlyphs,
    ambiguous: AmbiguousWidth,
) -> usize {
    let width = |range| {
        text.slice(range)
            .map_or(0, |source| text_width(&source, ambiguous))
    };
    let mut drawn = width(cell);
    for mark in marks[marks.partition_point(|mark| mark.range.start < cell.start)..]
        .iter()
        .take_while(|mark| mark.range.start < cell.end)
        .filter(|mark| mark.range.end <= cell.end)
    {
        let glyph = match mark.mark {
            MarkdownMark::Hide => "",
            MarkdownMark::Link { url } => url
                .and_then(|url| link_icon(text, glyphs, url))
                .unwrap_or(&glyphs.link),
            MarkdownMark::Image => &glyphs.image,
            _ => continue,
        };
        drawn = drawn.saturating_sub(width(mark.range)) + text_width(glyph, ambiguous);
    }
    drawn
}
