//! Markdown tables on the reader, boxed in exactly as the editor draws them:
//! the `[editor.markdown]` border glyphs, closing bottom and optional row lines, each cell a
//! space inside its borders, and the editor's own column narrowing and cell wrapping
//! (`ui::render::markdown`). A table wider than the reader wraps each cell inside its column;
//! one that cannot fit even so reads as stacked `Header: value` records, each in its own box.
use super::super::find;
use super::super::lines::Line;
use super::html::CELL_BREAK;
use super::{Rich, blank, highlight_style, role};
use crate::config::{AmbiguousWidth, Highlight, MarkdownConfig};
use crate::editor::text::ByteOffset;
use crate::ui::render::markdown::{self as editor, Alignment};
use crate::ui::{GitRole, GitSurface, Style};
use std::ops::Range;
use std::sync::{Arc, RwLock};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A column never narrows below its longest word, within these bounds, before the table
/// stacks its records instead.
const FLOOR: (usize, usize) = (4, 14);
/// The reader measures cells as the forge screen draws them.
const AMBIGUOUS: AmbiguousWidth = AmbiguousWidth::Narrow;

/// The editor's `[editor.markdown]` settings, set once from the loaded configuration; the
/// defaults until then (and in tests).
static SETTINGS: RwLock<Option<MarkdownConfig>> = RwLock::new(None);

/// Draws reader tables with the user's editor table settings.
pub(crate) fn configure(config: &MarkdownConfig) {
    if let Ok(mut settings) = SETTINGS.write() {
        *settings = Some(config.clone());
    }
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Bumped by [`configure`]: rendered prose remembered under an older one is redrawn.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

fn settings() -> MarkdownConfig {
    SETTINGS
        .read()
        .ok()
        .and_then(|settings| settings.clone())
        .unwrap_or_default()
}

/// A cleaned body split into prose and the tables in it.
pub(super) enum Block<'a> {
    Text(String),
    Table(Vec<&'a str>),
}

/// The tables of `text`, outside fenced and indented code: a `|` row followed by its
/// delimiter row (`|---|:-:|`) and the `|` rows after it.
pub(super) fn split(text: &str) -> Vec<Block<'_>> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        index += 1;
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            prose.push(line);
            continue;
        }
        if let Some(marker) = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker))
        {
            fence = Some(marker);
            prose.push(line);
            continue;
        }
        let shallow = line.len() - trimmed.len() < 4;
        if shallow
            && trimmed.starts_with('|')
            && lines
                .get(index)
                .is_some_and(|next| crate::ui::markdown::is_table_separator(next))
        {
            if !prose.is_empty() {
                blocks.push(Block::Text(prose.join("\n")));
                prose.clear();
            }
            let mut table = vec![line, lines[index]];
            index += 1;
            while let Some(row) = lines
                .get(index)
                .filter(|row| row.trim_start().starts_with('|'))
            {
                table.push(row);
                index += 1;
            }
            blocks.push(Block::Table(table));
            continue;
        }
        prose.push(line);
    }
    if !prose.is_empty() {
        blocks.push(Block::Text(prose.join("\n")));
    }
    blocks
}

/// One cell's text without its Markdown marks, and the styles over it.
#[derive(Default)]
struct Cell {
    text: String,
    paint: Vec<(Range<usize>, Style)>,
}

impl Cell {
    fn new(raw: &str, header: bool) -> Self {
        let mut cell = Self::default();
        let base = header.then_some(Highlight::STRONG);
        for (text, highlight) in crate::ui::markdown::inline(raw.trim(), base) {
            let start = cell.text.len();
            cell.text.push_str(&text);
            if let Some(highlight) = highlight {
                cell.paint.push((
                    start..cell.text.len(),
                    highlight_style(highlight, GitSurface::Pane),
                ));
            }
        }
        cell
    }

    /// The hard lines (`<br>`) of the cell as byte ranges.
    fn pieces(&self) -> Vec<Range<usize>> {
        let mut pieces = Vec::new();
        let mut start = 0;
        for (at, _) in self.text.match_indices(CELL_BREAK) {
            pieces.push(start..at);
            start = at + CELL_BREAK.len_utf8();
        }
        pieces.push(start..self.text.len());
        pieces
    }

    fn natural(&self) -> usize {
        self.pieces()
            .into_iter()
            .map(|piece| self.text[piece].trim().width())
            .max()
            .unwrap_or(0)
    }

    fn longest_word(&self) -> usize {
        self.text
            .split([' ', CELL_BREAK])
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0)
    }

    /// The cell wrapped to `width` as the editor wraps a table cell: byte ranges of its rows,
    /// the space a row breaks at left out. A `<br>` starts a row of its own.
    fn rows(&self, width: usize) -> Vec<Range<usize>> {
        let mut rows = Vec::new();
        for piece in self.pieces() {
            let text = &self.text[piece.clone()];
            let start = piece.start + (text.len() - text.trim_start().len());
            let text = text.trim();
            let graphemes = text
                .grapheme_indices(true)
                .map(|(at, grapheme)| {
                    let width = if grapheme == "\t" {
                        1
                    } else {
                        grapheme.width()
                    };
                    (ByteOffset(start + at), width, grapheme.trim().is_empty())
                })
                .collect::<Vec<_>>();
            if graphemes.is_empty() {
                rows.push(start..start);
                continue;
            }
            let end_of = |at: ByteOffset| {
                let index = graphemes.partition_point(|(offset, _, _)| *offset <= at);
                graphemes
                    .get(index)
                    .map_or(start + text.len(), |(next, _, _)| next.0)
            };
            let (places, widths) = editor::wrap_cell(&graphemes, width.max(1));
            let mut wrapped: Vec<Option<Range<usize>>> = vec![None; widths.len()];
            for (at, row, _) in places {
                let end = end_of(at);
                let range = wrapped[row].get_or_insert(at.0..end);
                range.start = range.start.min(at.0);
                range.end = range.end.max(end);
            }
            rows.extend(
                wrapped
                    .into_iter()
                    .map(|range| range.unwrap_or(start..start)),
            );
        }
        rows
    }

    /// `range` of the cell onto `rich`, in its styles.
    fn push(&self, mut rich: Rich, range: Range<usize>) -> Rich {
        let mut at = range.start;
        for (span, style) in &self.paint {
            let start = span.start.max(range.start);
            let end = span.end.min(range.end);
            if start >= end {
                continue;
            }
            if at < start {
                rich = rich.plain(&self.text[at..start]);
            }
            rich = rich.push(&self.text[start..end], *style);
            at = end;
        }
        if at < range.end {
            rich = rich.plain(&self.text[at..range.end]);
        }
        rich
    }
}

fn finish(rich: Rich, indent: &str) -> Line {
    let mut row = rich.line();
    // An empty last cell leaves no trailing blanks.
    let kept = row.text.trim_end_matches(' ').len();
    row.text.truncate(kept);
    row.paint.retain_mut(|(range, _)| {
        range.end = range.end.min(kept);
        range.start < range.end
    });
    row.gutter = indent.to_owned();
    row.source = Some(find::Source {
        text: Arc::from(row.text.as_str()),
        offset: 0,
    });
    row.pan = false;
    row
}

/// The editor's table pieces and geometry for one table.
struct Frame {
    config: MarkdownConfig,
    borders: [usize; 3],
}

impl Frame {
    fn new() -> Self {
        let config = settings();
        let borders = editor::table_borders(&config.glyphs, AMBIGUOUS);
        Self { config, borders }
    }

    /// Cells taken by borders and padding around `columns` columns.
    fn overhead(&self, columns: usize) -> usize {
        columns * 2
            + self.borders[0]
            + self.borders[2]
            + columns.saturating_sub(1) * self.borders[1]
    }

    /// A border row from the pieces at `parts` (top `0..3`, under the header `3..6`, bottom
    /// `6..9`), the row under the header marking each column's alignment.
    fn border(
        &self,
        widths: &[usize],
        alignments: &[Alignment],
        parts: [usize; 3],
        aligned: bool,
        indent: &str,
    ) -> Line {
        let text = editor::table_border(
            &self.config.glyphs,
            AMBIGUOUS,
            widths,
            self.borders,
            alignments,
            parts,
            aligned,
        );
        finish(Rich::new().push(&text, role(GitRole::Border)), indent)
    }

    /// The `│` at `border` (0 left, 1 between, 2 right), padded to the border's width.
    fn pipe(&self, border: usize) -> String {
        let pipe = editor::table_part(&self.config.glyphs, 9);
        format!(
            "{pipe}{}",
            " ".repeat(self.borders[border].saturating_sub(pipe.width()))
        )
    }
}

/// `lines` (header, delimiter, body rows) as reader rows `width` cells wide.
pub(super) fn render(rows: &mut Vec<Line>, lines: &[&str], width: usize, indent: &str) {
    let cells = crate::ui::markdown::table_cells;
    let header = cells(lines[0])
        .into_iter()
        .map(|cell| Cell::new(cell, true))
        .collect::<Vec<_>>();
    let columns = header.len().max(1);
    let alignments = (0..columns)
        .map(|column| {
            cells(lines[1])
                .get(column)
                .map_or(Alignment::Default, |cell| editor::table_alignment(cell))
        })
        .collect::<Vec<_>>();
    // A body row has the header's columns: missing cells are empty, extra ones dropped.
    let body = lines[2..]
        .iter()
        .map(|line| {
            let mut row = cells(line)
                .into_iter()
                .map(|cell| Cell::new(cell, false))
                .collect::<Vec<_>>();
            row.resize_with(columns, Cell::default);
            row
        })
        .collect::<Vec<_>>();
    let mut header = header;
    header.resize_with(columns, Cell::default);
    let all = || std::iter::once(&header).chain(body.iter());
    let frame = Frame::new();
    let overhead = frame.overhead(columns);
    let mut widths = (0..columns)
        .map(|column| {
            all()
                .map(|row| row[column].natural())
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect::<Vec<_>>();
    if widths.iter().sum::<usize>() + overhead > width {
        let floors = (0..columns)
            .map(|column| {
                let word = all()
                    .map(|row| row[column].longest_word())
                    .max()
                    .unwrap_or(0);
                widths[column].min(word.clamp(FLOOR.0, FLOOR.1))
            })
            .sum::<usize>();
        if floors + overhead > width {
            stacked(rows, &header, &body, width, indent, &frame);
            return;
        }
        editor::narrow(&mut widths, width - overhead);
    }
    let border = role(GitRole::Border);
    let last = body.len();
    rows.push(frame.border(&widths, &alignments, [0, 1, 2], false, indent));
    for (index, row) in all().enumerate() {
        let wrapped = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| cell.rows(*width))
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for line in 0..height {
            let mut rich = Rich::new().push(frame.pipe(0), border).plain(" ");
            for column in 0..columns {
                let range = wrapped[column].get(line).cloned().unwrap_or(0..0);
                let used = row[column].text[range.clone()].width();
                let spare = widths[column].saturating_sub(used);
                let before = match alignments[column] {
                    Alignment::Default | Alignment::Left => 0,
                    Alignment::Center => spare / 2,
                    Alignment::Right => spare,
                };
                rich = rich.plain(&" ".repeat(before));
                rich = row[column].push(rich, range);
                rich = rich.plain(&" ".repeat(spare - before + 1));
                rich = rich.push(frame.pipe(if column + 1 < columns { 1 } else { 2 }), border);
                if column + 1 < columns {
                    rich = rich.plain(" ");
                }
            }
            rows.push(finish(rich, indent));
        }
        if index == 0 {
            rows.push(frame.border(&widths, &alignments, [3, 4, 5], true, indent));
        } else if frame.config.table_row_lines && index < last {
            rows.push(frame.border(&widths, &alignments, [3, 4, 5], false, indent));
        }
    }
    if frame.config.table_bottom {
        rows.push(frame.border(&widths, &alignments, [6, 7, 8], false, indent));
    }
}

/// Each body row as `Header: value` lines in a box of its own, records apart by a blank row.
fn stacked(
    rows: &mut Vec<Line>,
    header: &[Cell],
    body: &[Vec<Cell>],
    width: usize,
    indent: &str,
    frame: &Frame,
) {
    let overhead = frame.overhead(1);
    // Too narrow even for a box: the records without one.
    let boxed = width > overhead;
    let inner = if boxed { width - overhead } else { width };
    let records = body
        .iter()
        .map(|record| {
            let mut lines = Vec::new();
            for (column, cell) in record.iter().enumerate() {
                let name = header[column].text.replace(CELL_BREAK, " ");
                let name = if name.trim().is_empty() {
                    format!("Column {}", column + 1)
                } else {
                    name
                };
                let label = format!("{name}: ");
                // A long header puts its value on the next rows.
                let beside = label.width() <= inner / 2;
                let hang = if beside { label.width() } else { 2 };
                let values = cell.rows(inner.saturating_sub(hang).max(1));
                let mut rich = Rich::new().push(&label, super::strong(GitRole::Text));
                let mut values = values.into_iter();
                if beside && let Some(first) = values.next() {
                    rich = cell.push(rich, first);
                }
                lines.push(rich);
                for value in values {
                    lines.push(cell.push(Rich::new().plain(&" ".repeat(hang)), value));
                }
            }
            lines
        })
        .collect::<Vec<_>>();
    let content = records
        .iter()
        .flatten()
        .map(Rich::width)
        .max()
        .unwrap_or(1)
        .clamp(1, inner.max(1));
    let border = role(GitRole::Border);
    for (index, record) in records.into_iter().enumerate() {
        if index > 0 {
            let mut gap = blank();
            gap.gutter = indent.to_owned();
            rows.push(gap);
        }
        if boxed {
            rows.push(frame.border(&[content], &[Alignment::Default], [0, 1, 2], false, indent));
        }
        for line in record {
            if !boxed {
                rows.push(finish(line, indent));
                continue;
            }
            let pad = content.saturating_sub(line.width());
            let mut rich = Rich::new().push(frame.pipe(0), border).plain(" ");
            rich = append(rich, line)
                .plain(&" ".repeat(pad + 1))
                .push(frame.pipe(2), border);
            rows.push(finish(rich, indent));
        }
        if boxed {
            rows.push(frame.border(&[content], &[Alignment::Default], [6, 7, 8], false, indent));
        }
    }
}

/// `more` after `rich`, keeping its styles.
fn append(mut rich: Rich, more: Rich) -> Rich {
    let at = rich.text.len();
    rich.text.push_str(&more.text);
    rich.paint.extend(
        more.paint
            .into_iter()
            .map(|(range, style)| (range.start + at..range.end + at, style)),
    );
    rich
}

#[cfg(test)]
mod tests {
    use super::super::markdown;
    use super::*;

    const RESULTS: &str = "Intro\n\n| Suite | Passed | Failed | Duration |\n|:------|-------:|:------:|---------:|\n| unit | 41 | 0 | 2.7s |\n| reconnect-storm | 3 | 0 | 41.0s |\n\nAfter";

    fn texts(rows: &[Line]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", row.gutter, row.text))
            .collect()
    }

    /// Where the column borders fall on a row.
    fn bars(text: &str) -> Vec<usize> {
        text.char_indices()
            .filter(|(_, c)| "│┼┬┴├┤┌┐└┘".contains(*c))
            .map(|(at, _)| text[..at].width())
            .collect()
    }

    /// The table's rows: from its top-left corner to its bottom-right one.
    fn boxed(text: &[String]) -> &[String] {
        let top = text
            .iter()
            .position(|row| row.trim_start().starts_with('┌'))
            .unwrap();
        let bottom = text
            .iter()
            .rposition(|row| row.trim_end().ends_with('┘'))
            .unwrap();
        &text[top..=bottom]
    }

    /// Every side covered: the four corners, `│` down both outer sides, borders in line.
    fn assert_closed(table: &[String]) {
        let (first, last) = (table[0].trim(), table[table.len() - 1].trim());
        assert!(first.starts_with('┌') && first.ends_with('┐'), "{table:#?}");
        assert!(last.starts_with('└') && last.ends_with('┘'), "{table:#?}");
        let (left, right) = (bars(&table[0])[0], *bars(&table[0]).last().unwrap());
        for row in table {
            let bars = bars(row);
            assert_eq!(
                (bars[0], *bars.last().unwrap()),
                (left, right),
                "both outer sides on every row: {table:#?}"
            );
            assert_eq!(row.width(), right + 1, "{table:#?}");
        }
    }

    #[test]
    fn a_table_is_boxed_in_as_the_editor_draws_it() {
        let mut rows = Vec::new();
        markdown(&mut rows, RESULTS, 80, "");
        let text = texts(&rows);
        let table = boxed(&text);
        assert_eq!(
            table,
            [
                "┌─────────────────┬────────┬────────┬──────────┐",
                "│ Suite           │ Passed │ Failed │ Duration │",
                "├━────────────────┼───────━┼━──────━┼─────────━┤",
                "│ unit            │     41 │   0    │     2.7s │",
                "│ reconnect-storm │      3 │   0    │    41.0s │",
                "└─────────────────┴────────┴────────┴──────────┘",
            ]
        );
        assert_closed(table);
        let columns = bars(&table[1]);
        for row in table {
            assert_eq!(bars(row), columns, "{table:#?}");
        }
        // The header is bold; the borders are in the border colour.
        let header = text.iter().position(|row| row.contains("Suite")).unwrap();
        let bold = super::super::strong(GitRole::Text);
        assert!(
            rows[header].paint.iter().any(
                |(range, style)| *style == bold && &rows[header].text[range.clone()] == "Suite"
            )
        );
        assert!(
            rows[header + 1]
                .paint
                .iter()
                .all(|(_, style)| *style == role(GitRole::Border))
        );
        assert!(
            rows[header]
                .paint
                .iter()
                .any(|(_, style)| *style == role(GitRole::Border))
        );
        // Prose around the table reads as before.
        assert_eq!(text[0], "Intro");
        assert_eq!(text.last().map(String::as_str), Some("After"));
    }

    #[test]
    fn the_editor_table_settings_draw_the_forge_table() {
        let mut config = MarkdownConfig {
            table_bottom: false,
            table_row_lines: true,
            ..MarkdownConfig::default()
        };
        config.glyphs.table = ["╭", "┬", "╮", "├", "┼", "┤", "╰", "┴", "╯", "┃", "─"]
            .map(String::from)
            .into();
        let frame = Frame {
            borders: editor::table_borders(&config.glyphs, AMBIGUOUS),
            config,
        };
        assert_eq!(frame.pipe(0), "┃");
        let line = frame.border(
            &[3, 2],
            &[Alignment::Left, Alignment::Right],
            [3, 4, 5],
            true,
            "",
        );
        assert_eq!(line.text, "├━────┼───━┤");
        assert_eq!(
            frame
                .border(&[3, 2], &[Alignment::Default; 2], [0, 1, 2], false, "")
                .text,
            "╭─────┬────╮"
        );
    }

    #[test]
    fn inline_marks_in_cells_lose_their_markup() {
        let mut rows = Vec::new();
        markdown(
            &mut rows,
            "| File | Change |\n|---|---|\n| `orbit/queue.py` | **Bound** the [exponent](https://x) |",
            80,
            "",
        );
        let text = texts(&rows);
        assert_eq!(text[3], "│ orbit/queue.py │ Bound the exponent │");
        assert!(
            rows[3]
                .paint
                .iter()
                .any(|(range, _)| &rows[3].text[range.clone()] == "orbit/queue.py")
        );
    }

    #[test]
    fn a_wide_table_wraps_cells_inside_their_closed_box() {
        let table = "| File | Change summary |\n|------|----------------|\n| `orbit/queue.py` | Bound the exponent to 6 and clamp the delay to 60 seconds so long outages stay cheap. |\n| `tests/test_queue.py` | Renamed the test. |";
        let mut rows = Vec::new();
        markdown(&mut rows, table, 48, "  ");
        let text = texts(&rows);
        assert!(text.iter().all(|row| row.width() <= 48), "{text:#?}");
        let table = boxed(&text);
        assert_closed(table);
        let columns = bars(&table[1]);
        assert_eq!(columns.len(), 3, "{text:#?}");
        for row in table {
            assert_eq!(bars(row), columns, "every row keeps the column: {text:#?}");
        }
        // The long cell continues on rows of its own, under its column, inside the box.
        let first = table
            .iter()
            .position(|row| row.contains("orbit/queue.py"))
            .unwrap();
        assert!(table[first + 1].trim_start().starts_with('│'), "{text:#?}");
        let joined = table
            .iter()
            .map(|row| row.split('│').nth(2).unwrap_or("").trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("clamp the delay to 60 seconds so long outages stay cheap."),
            "{joined}"
        );
    }

    #[test]
    fn a_table_that_cannot_fit_boxes_each_stacked_record() {
        let table = "| Suite | Passed | Failed | Skipped | Duration | Coverage | Runner |\n|---|---|---|---|---|---|---|\n| unit | 41 | 0 | 1 | 2.7s | 96% | ubuntu-latest |\n| storm | 3 | 0 | 0 | 41.0s | 88% | self-hosted |";
        let mut rows = Vec::new();
        markdown(&mut rows, table, 30, "");
        let text = texts(&rows);
        let line = |text: &str| format!("│ {text:<21} │");
        assert_eq!(
            &text[1..4],
            [line("Suite: unit"), line("Passed: 41"), line("Failed: 0")],
            "{text:#?}"
        );
        assert!(text.contains(&line("Runner: ubuntu-latest")), "{text:#?}");
        let gap = text.iter().position(String::is_empty).unwrap();
        assert_closed(&text[..gap]);
        assert_closed(&text[gap + 1..]);
        assert_eq!(text[gap + 2], line("Suite: storm"));
        assert!(text.iter().all(|row| row.width() <= 30), "{text:#?}");
    }

    #[test]
    fn breaks_ragged_rows_and_code_fences() {
        let mut rows = Vec::new();
        markdown(
            &mut rows,
            "| a | b |\n|---|---|\n| one<br>two | x |\n| only |\n\n```\n| not | a table |\n|---|---|\n```",
            80,
            "",
        );
        let text = texts(&rows);
        assert_eq!(
            &text[1..6],
            [
                "│ a    │ b │",
                "├──────┼───┤",
                "│ one  │ x │",
                "│ two  │   │",
                "│ only │   │"
            ],
            "{text:#?}"
        );
        assert!(
            text.iter()
                .any(|row| row.trim_start() == "| not | a table |"),
            "fenced code stays literal: {text:#?}"
        );
    }
}
