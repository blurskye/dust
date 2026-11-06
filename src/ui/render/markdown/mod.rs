//! Markdown drawn in place of its markup, from the marks the syntax worker finds, as
//! render-markdown.nvim draws it. The cursor's line, and every line outside Normal mode, shows
//! its source.
use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
mod tests;

use crate::config::AmbiguousWidth;

use crate::config::EditorConfig;

use crate::config::MarkdownGlyphs;

use crate::editor::EditorMode;

use crate::editor::View;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

use crate::files::ExplorerIconColor;

use crate::config::Highlight;

use crate::language::syntax::MarkdownMark;

use crate::ui::Style;

use crate::ui::UiMarkdownMark;

use crate::ui::UiState;

use crate::ui::geometry::text_width;

/// A table's border pieces when the configured list is short, in render-markdown's order:
/// the top, the row under the header, the bottom, then the line between cells and across.
const TABLE: [&str; 11] = ["┌", "┬", "┐", "├", "┼", "┤", "└", "┴", "┘", "│", "─"];

/// The marks of one view's buffer, walked line by line from the top of the window down.
pub(super) struct MarkdownMarks<'a> {
    glyphs: &'a MarkdownGlyphs,
    text: &'a TextBuffer,
    ambiguous: AmbiguousWidth,
    marks: Vec<&'a UiMarkdownMark>,
    /// Greatest end offset through each mark, for finding spans that started on earlier lines.
    mark_ends: Vec<ByteOffset>,
    /// Code blocks span lines, so they are looked up apart from the marks within a line.
    blocks: Vec<ByteRange>,
    tables: Vec<Table>,
    /// Whether a border is drawn under the table and between its rows.
    table_bottom: bool,
    table_row_lines: bool,
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
    /// Each column's width, narrower than its widest cell when the table is narrowed to fit.
    widths: Vec<usize>,
    /// Display widths shared by the left, inner and right borders on every row.
    borders: [usize; 3],
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
    /// A table row, drawn over as many rows as its tallest cell wraps to.
    pub(super) table: Option<TableRow>,
}

/// A table row laid out as a block: the same borders on each of its rows, with every cell's
/// text wrapped inside its column, and a border above, below or after it on rows of its own.
pub(super) struct TableRow {
    /// Each row's borders and padding, drawn before the text.
    pub(super) frames: Vec<String>,
    pub(super) style: Style,
    /// Where each drawn grapheme of the cells goes: its row, and its column from the frame's
    /// start. Sorted by offset; the rest of the line is not drawn.
    places: Vec<(ByteOffset, usize, usize)>,
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
    /// `None` when the buffer has no marks or rendering is off. Tables are narrowed to fit
    /// `width`, whether or not lines wrap, as Neovim draws them.
    pub(super) fn new(
        ui: &'a UiState,
        view: &View,
        text: &'a TextBuffer,
        editor: &'a EditorConfig,
        width: usize,
    ) -> Option<Self> {
        let config = &editor.markdown;
        let ambiguous = editor.ambiguous_width;
        let shown = config.render
            && matches!(
                view.mode,
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
            .filter(|mark| mark.buffer == view.buffer && mark.revision == revision)
            .partition(|mark| matches!(mark.mark, MarkdownMark::Code | MarkdownMark::Table));
        if blocks.is_empty() && marks.is_empty() {
            return None;
        }
        let (tables, blocks): (Vec<_>, Vec<_>) = blocks
            .into_iter()
            .partition(|mark| mark.mark == MarkdownMark::Table);
        let tables = tables
            .iter()
            .filter_map(|table| {
                Table::new(text, &marks, table.range, &config.glyphs, ambiguous, width)
            })
            .collect();
        let mut end = ByteOffset(0);
        let mark_ends = marks
            .iter()
            .map(|mark| {
                end = end.max(mark.range.end);
                end
            })
            .collect();
        let group = |name: &str, fallback| Highlight::named(name).unwrap_or(fallback);
        let list = group("@markup.list", Highlight::KEYWORD);
        Some(Self {
            glyphs: &config.glyphs,
            text,
            ambiguous,
            marks,
            mark_ends,
            blocks: blocks.into_iter().map(|mark| mark.range).collect(),
            tables,
            table_bottom: config.table_bottom,
            table_row_lines: config.table_row_lines,
            cursor_line: text.byte_to_line(view.cursor).unwrap_or(0),
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

    /// What `line` draws, or `None` when it shows its source.
    pub(super) fn line(&self, line: usize) -> Option<MarkdownLine<'a>> {
        if line == self.cursor_line {
            return None;
        }
        let start = self.text.line_to_byte(line).ok()?;
        let source = self.text.line(line).ok()?;
        let end = ByteOffset(start.get() + source.strip_suffix('\n').unwrap_or(&source).len());
        let first = self.mark_ends.partition_point(|end| *end <= start);
        let last = self.marks.partition_point(|mark| mark.range.start < end);
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
            table: None,
        };
        self.table_line(line, start, end, &mut drawn);
        let glyph = |text: &'a str, style| Shown::Glyph(Cow::Borrowed(text), style);
        for mark in self.marks[first..last]
            .iter()
            .filter(|mark| mark.range.end > start)
        {
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

    /// A table row drawn with each cell padded to its column, as render-markdown draws it. The
    /// borders above and below the table are drawn on the blank lines around it, or on rows of
    /// the first and last row's own when there is no blank line.
    fn table_line(
        &self,
        line: usize,
        start: ByteOffset,
        end: ByteOffset,
        drawn: &mut MarkdownLine,
    ) {
        let style = Style::Syntax(self.table);
        let whole = ByteRange { start, end };
        let Some(table) = self
            .tables
            .iter()
            .find(|table| (table.first_line..=table.last_line).contains(&line))
        else {
            let Some((table, bottom)) = self.blank_border(line) else {
                return;
            };
            let border = if bottom {
                self.border(table, 6, 7, 8, false)
            } else {
                self.border(table, 0, 1, 2, false)
            };
            if start < end {
                drawn.marks.push((whole, Shown::Hidden));
            }
            drawn.after = Some((border, style));
            return;
        };
        let row = line - table.first_line;
        let cells = &table.rows[row];
        let last = line == table.last_line;
        let mut frames = Vec::new();
        let mut places = Vec::new();
        let own_border = |line: Option<usize>, bottom: bool| {
            line.filter(|line| *line != self.cursor_line)
                .and_then(|line| self.blank_border(line))
                .is_none_or(|(other, other_bottom)| {
                    other.first_line != table.first_line || other_bottom != bottom
                })
        };
        if row == 0 && own_border(line.checked_sub(1), false) {
            frames.push(self.border(table, 0, 1, 2, false));
        }
        if row == 1 || cells.is_empty() {
            frames.push(self.border(table, 3, 4, 5, true));
        } else {
            let pipe = self.part(9);
            let pipe_width = text_width(pipe, self.ambiguous);
            let mut frame = format!("{pipe}{} ", " ".repeat(table.borders[0] - pipe_width));
            let mut origin = table.borders[0] + 1;
            let mut rows = 1;
            let top = frames.len();
            for (column, width) in table.widths.iter().enumerate() {
                frame.push_str(&" ".repeat(*width));
                frame.push(' ');
                frame.push_str(pipe);
                let border = if column + 1 < table.widths.len() {
                    1
                } else {
                    2
                };
                frame.push_str(&" ".repeat(table.borders[border] - pipe_width));
                if border == 1 {
                    frame.push(' ');
                }
                let cell_origin = origin;
                origin += width + 2 + table.borders[border];
                // A row with fewer cells than its header is drawn with empty ones.
                let Some(cell) = cells.get(column) else {
                    continue;
                };
                let graphemes =
                    cell_graphemes(self.text, &self.marks, *cell, self.glyphs, self.ambiguous);
                let (cell_places, widths) = wrap_cell(&graphemes, *width);
                rows = rows.max(widths.len());
                places.extend(cell_places.into_iter().map(|(at, row, offset)| {
                    let spare = width.saturating_sub(widths[row]);
                    let leading = match table.alignments[column] {
                        Alignment::Default | Alignment::Left => 0,
                        Alignment::Center => spare / 2,
                        Alignment::Right => spare,
                    };
                    (at, top + row, cell_origin + leading + offset)
                }));
            }
            places.sort_by_key(|(at, _, _)| *at);
            frames.extend(std::iter::repeat_n(frame, rows));
            if self.table_row_lines && row > 1 && !last {
                frames.push(self.border(table, 3, 4, 5, false));
            }
        }
        if last && self.table_bottom && own_border(Some(line + 1), true) {
            frames.push(self.border(table, 6, 7, 8, false));
        }
        // The source is drawn only where `places` puts it.
        drawn.table = Some(TableRow {
            frames,
            style,
            places,
        });
    }

    /// The table whose border `line` draws when it is blank, and whether that is the table's
    /// bottom: the table above's bottom comes before the table below's top.
    fn blank_border(&self, line: usize) -> Option<(&Table, bool)> {
        // The empty line after a final newline isn't drawn, so the table closes itself.
        let blank = line < self.text.visible_lines()
            && self
                .text
                .line(line)
                .is_ok_and(|text| text.trim().is_empty());
        if !blank {
            return None;
        }
        let above = self
            .tables
            .iter()
            .find(|table| self.table_bottom && table.last_line + 1 == line);
        if let Some(table) = above {
            return Some((table, true));
        }
        self.tables
            .iter()
            .find(|table| table.first_line == line + 1)
            .map(|table| (table, false))
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
        border
            .push_str(&" ".repeat(table.borders[0] - text_width(self.part(left), self.ambiguous)));
        for (column, width) in table.widths.iter().enumerate() {
            if column > 0 {
                border.push_str(self.part(cross));
                border.push_str(
                    &" ".repeat(table.borders[1] - text_width(self.part(cross), self.ambiguous)),
                );
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
            let indicator_width = text_width(indicator, self.ambiguous);
            let indicators = usize::from(start) + usize::from(end);
            let room = width + 2;
            let aligned = indicator_width * indicators <= room;
            if start && aligned {
                border.push_str(indicator);
            }
            let fill = room
                - if aligned {
                    indicator_width * indicators
                } else {
                    0
                };
            border.push_str(&table_fill(line, fill, self.ambiguous));
            if end && aligned {
                border.push_str(indicator);
            }
        }
        border.push_str(self.part(right));
        border
            .push_str(&" ".repeat(table.borders[2] - text_width(self.part(right), self.ambiguous)));
        border
    }

    fn part(&self, index: usize) -> &str {
        table_part(self.glyphs, index)
    }

    /// The configured title and colour of the callout whose `[!NAME]` starts at `at`.
    fn callout(&self, at: usize) -> Option<(&'a str, Style)> {
        let mut end = (at + 32).min(self.text.len_bytes());
        while !self.text.is_char_boundary(ByteOffset(end)) {
            end -= 1;
        }
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

impl TableRow {
    /// The row and column the grapheme at `at` is drawn at, `None` when it is not drawn.
    pub(super) fn place(&self, at: ByteOffset) -> Option<(usize, usize)> {
        let index = self
            .places
            .binary_search_by_key(&at, |(offset, _, _)| *offset)
            .ok()?;
        let (_, row, column) = self.places[index];
        Some((row, column))
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

    /// The cells drawn for the grapheme at `at`, which is `source` cells wide in the text,
    /// with what is drawn before it.
    pub(super) fn drawn_width(
        &self,
        at: ByteOffset,
        source: usize,
        ambiguous: AmbiguousWidth,
    ) -> usize {
        let before = self
            .before(at)
            .map(|(text, _)| text_width(text, ambiguous))
            .sum::<usize>();
        before
            + match self.at(at) {
                Some(Drawn::Hidden) => 0,
                Some(Drawn::Glyph(glyph, _)) => text_width(glyph, ambiguous),
                None => source,
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
        fit: usize,
    ) -> Option<Self> {
        let first_line = text.byte_to_line(range.start).ok()?;
        let mut last = ByteOffset(range.end.get().saturating_sub(1).max(range.start.get()));
        while !text.is_char_boundary(last) {
            last = ByteOffset(last.get() - 1);
        }
        let last_line = text.byte_to_line(last).ok()?;
        let mut rows = Vec::new();
        let mut cell_widths: Vec<Vec<usize>> = Vec::new();
        for line in first_line..=last_line {
            let start = text.line_to_byte(line).ok()?;
            let source = text.line(line).ok()?;
            let source = source.trim_end_matches(['\n', '\r']);
            let end = ByteOffset(start.get() + source.len());
            let line_marks = &marks[marks.partition_point(|mark| mark.range.start < start)
                ..marks.partition_point(|mark| mark.range.start < end)];
            // A quoted table's syntax range starts after its first quote prefix; later rows
            // have their own quote marks. Neither prefix is part of a table cell.
            let start = line_marks
                .iter()
                .filter(|mark| matches!(mark.mark, MarkdownMark::Quote { .. }))
                .map(|mark| mark.range.end)
                .max()
                .unwrap_or(start)
                .max(start.max(range.start));
            let pipes = line_marks
                .iter()
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
                    .map(|cell| cell_width(text, marks, *cell, glyphs, ambiguous))
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
        let borders = [[0, 3, 6, 9], [1, 4, 7, 9], [2, 5, 8, 9]].map(|parts| {
            parts
                .into_iter()
                .map(|index| text_width(table_part(glyphs, index), ambiguous))
                .max()
                .unwrap_or(0)
        });
        // Each column draws its text between a border and a space on either side.
        let overhead =
            columns * 2 + borders[0] + borders[2] + columns.saturating_sub(1) * borders[1];
        if overhead + columns > fit {
            return None;
        }
        narrow(&mut widths, fit - overhead);
        let mut alignments = alignments;
        alignments.resize(columns, Alignment::Default);
        Some(Self {
            first_line,
            last_line,
            rows,
            widths,
            borders,
            alignments,
        })
    }
}

fn table_part(glyphs: &MarkdownGlyphs, index: usize) -> &str {
    glyphs.table.get(index).map_or(TABLE[index], String::as_str)
}

/// Repeat a border glyph without exceeding the allotted display cells.
fn table_fill(glyph: &str, cells: usize, ambiguous: AmbiguousWidth) -> String {
    let width = text_width(glyph, ambiguous);
    if width == 0 {
        return " ".repeat(cells);
    }
    format!(
        "{}{}",
        glyph.repeat(cells / width),
        " ".repeat(cells % width)
    )
}

/// Narrows columns to fit `room` cells together, the widest first, so narrow columns keep
/// their text on one row.
fn narrow(widths: &mut [usize], room: usize) {
    let capped = |cap: usize, widths: &[usize]| widths.iter().map(|width| (*width).min(cap)).sum();
    let mut cap = 1;
    let mut upper = widths.iter().copied().max().unwrap_or(1);
    while cap < upper {
        let middle = cap + (upper - cap).div_ceil(2);
        if capped(middle, widths) <= room {
            cap = middle;
        } else {
            upper = middle - 1;
        }
    }
    let mut spare = room.saturating_sub(capped(cap, widths));
    for width in widths {
        let whole = *width;
        *width = whole.min(cap);
        if whole > cap && spare > 0 {
            *width += 1;
            spare -= 1;
        }
    }
}

/// A cell's graphemes with the width each is drawn at: none for the markup it hides, and a
/// link's icon in place of its `[`. Whether each is a space, where its text may wrap.
fn cell_graphemes(
    text: &TextBuffer,
    marks: &[&UiMarkdownMark],
    cell: ByteRange,
    glyphs: &MarkdownGlyphs,
    ambiguous: AmbiguousWidth,
) -> Vec<(ByteOffset, usize, bool)> {
    let source = text.slice(cell).unwrap_or_default();
    let mut marks = marks[marks.partition_point(|mark| mark.range.start < cell.start)
        ..marks.partition_point(|mark| mark.range.start < cell.end)]
        .iter()
        .copied()
        .filter(|mark| {
            matches!(
                mark.mark,
                MarkdownMark::Hide | MarkdownMark::Link { .. } | MarkdownMark::Image
            )
        })
        .peekable();
    source
        .grapheme_indices(true)
        .map(|(offset, grapheme)| {
            let at = ByteOffset(cell.start.get() + offset);
            while marks.peek().is_some_and(|mark| mark.range.end <= at) {
                marks.next();
            }
            let mark = marks.peek().copied().filter(|mark| mark.range.contains(at));
            let width = match mark {
                None if grapheme == "\t" => 1,
                None => text_width(grapheme, ambiguous),
                Some(mark) if mark.range.start != at => 0,
                Some(mark) => match mark.mark {
                    MarkdownMark::Link { url } => text_width(
                        url.and_then(|url| link_icon(text, glyphs, url))
                            .unwrap_or(&glyphs.link),
                        ambiguous,
                    ),
                    MarkdownMark::Image => text_width(&glyphs.image, ambiguous),
                    _ => 0,
                },
            };
            (at, width, mark.is_none() && grapheme.trim().is_empty())
        })
        .collect()
}

/// The width a cell's text is drawn at.
fn cell_width(
    text: &TextBuffer,
    marks: &[&UiMarkdownMark],
    cell: ByteRange,
    glyphs: &MarkdownGlyphs,
    ambiguous: AmbiguousWidth,
) -> usize {
    cell_graphemes(text, marks, cell, glyphs, ambiguous)
        .iter()
        .map(|(_, width, _)| width)
        .sum()
}

/// A cell's text wrapped to `width`, at the last space that fits or, in a word longer than
/// the column, where it reaches the edge: each drawn grapheme's row and column, and each row's
/// width. The space a row breaks at is not drawn.
fn wrap_cell(
    graphemes: &[(ByteOffset, usize, bool)],
    width: usize,
) -> (Vec<(ByteOffset, usize, usize)>, Vec<usize>) {
    let mut placed: Vec<Option<(usize, usize)>> = vec![None; graphemes.len()];
    let (mut row, mut column, mut row_start, mut index) = (0, 0, 0, 0);
    while let Some(&(_, cells, space)) = graphemes.get(index) {
        if space && column == 0 && row > 0 {
            index += 1;
            row_start = index;
            continue;
        }
        if column + cells <= width || column == 0 {
            placed[index] = Some((row, column));
            column += cells;
            index += 1;
            continue;
        }
        if let Some(space) = (row_start + 1..index).rev().find(|&at| graphemes[at].2) {
            for place in &mut placed[space..index] {
                *place = None;
            }
            index = space + 1;
        }
        row += 1;
        column = 0;
        row_start = index;
    }
    let mut widths = vec![0; row + 1];
    let places = graphemes
        .iter()
        .zip(placed)
        .filter_map(|(&(at, cells, _), place)| {
            let (row, column) = place?;
            widths[row] = widths[row].max(column + cells);
            Some((at, row, column))
        })
        .collect();
    (places, widths)
}
