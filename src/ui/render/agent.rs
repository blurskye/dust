//! The agent panel: its session tabs on top, the conversation, the input box and a footer
//! with where the session works and its model. It is drawn in place of the pane that holds
//! it, so the pane still moves, resizes and zooms like any other window.

use std::ops::Range;

use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::{AmbiguousWidth, Config},
    editor::ViewId,
    ui::{CellGrid, Style, geometry::text_width, markdown::Styled},
};

use super::{Frame, visual_rows::syntax_role_style};

/// The input box grows with what is typed up to this many rows, then scrolls.
const INPUT_ROW_CEILING: usize = 8;

/// What the session hands the renderer for the pane that shows `view`.
#[derive(Clone, Debug)]
pub struct AgentPanelUi {
    pub view: ViewId,
    /// Each session's label and whether it is the one shown.
    pub tabs: Vec<(String, bool)>,
    /// The conversation, wrapped to the pane's width less the side padding.
    pub transcript: Styled,
    /// Byte ranges of the transcript drawn as the user's messages.
    pub bands: Vec<Range<usize>>,
    /// Rows scrolled up from the end; 0 follows new output.
    pub scroll: usize,
    /// What goes with the next question.
    pub context: Vec<String>,
    pub input: String,
    /// Byte offset of the cursor in `input`.
    pub cursor: usize,
    pub placeholder: String,
    /// Left of the footer: what the session is doing, or where it works.
    pub status: String,
    /// Right of the footer: model, mode and thinking level.
    pub settings: String,
}

/// Where the transcript's lines start, without the trailing empty ones.
#[must_use]
pub fn transcript_lines(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        lines.push(start..start + line.len());
        start += line.len() + 1;
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

pub(super) fn render_agent_panel(
    panel: &AgentPanelUi,
    focused: bool,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if width < 8 || height < 6 {
        return Frame { grid, cursor: None };
    }
    grid.fill_row(0, Style::Status);
    let mut x = 0;
    for (label, active) in &panel.tabs {
        let style = if *active { Style::StatusAccent } else { Style::Status };
        x = grid.put_text(x, 0, &format!(" {label} "), style);
    }

    let inner = width - 2;
    let chips = u16::from(!panel.context.is_empty());
    let (rows, (cursor_row, cursor_column)) = wrap_input(&panel.input, panel.cursor, usize::from(inner));
    let room = usize::from(height.saturating_sub(6 + chips)).max(1);
    let shown = rows.len().min(INPUT_ROW_CEILING).min(room);
    let first = cursor_row.saturating_sub(shown - 1).min(rows.len() - shown);
    let footer = height - 1;
    let bottom = footer - 1;
    let input_top = bottom - u16::try_from(shown).unwrap_or(1);
    let top = input_top - 1;
    let border = if focused { Style::Accent } else { Style::Border };
    for column in 0..width {
        grid.set_cell(column, top, "─", border);
        grid.set_cell(column, bottom, "─", border);
    }
    if panel.input.is_empty() {
        grid.put_text_clipped(1, input_top, &panel.placeholder, Style::Muted, width - 1);
    }
    for (offset, row) in rows.iter().skip(first).take(shown).enumerate() {
        let y = input_top + u16::try_from(offset).unwrap_or(0);
        grid.put_text_clipped(1, y, row, Style::Default, width - 1);
    }
    if chips > 0 {
        let mut x = 1;
        for label in &panel.context {
            x = grid.put_text_clipped(x, top - 1, &format!("+ {label}"), Style::Accent, width - 1);
            x = x.saturating_add(2);
        }
    }
    let settings_width = u16::try_from(text_width(&panel.settings, config.editor.ambiguous_width)).unwrap_or(width);
    let settings_x = width.saturating_sub(settings_width + 1);
    grid.put_text_clipped(1, footer, &panel.status, Style::Muted, settings_x.saturating_sub(1));
    grid.put_text_clipped(settings_x, footer, &panel.settings, Style::Muted, width - 1);

    let area = usize::from(top - chips - 1);
    let lines = transcript_lines(&panel.transcript.text);
    let hidden = lines.len().saturating_sub(area);
    let scroll = panel.scroll.min(hidden);
    for (offset, line) in lines.iter().skip(hidden - scroll).take(area).enumerate() {
        let y = 1 + u16::try_from(offset).unwrap_or(0);
        draw_line(&mut grid, y, &panel.transcript, &panel.bands, line.clone());
    }
    if scroll > 0 {
        let more = format!(" ↓ {scroll} more ");
        let more_width = u16::try_from(text_width(&more, config.editor.ambiguous_width)).unwrap_or(0);
        let y = u16::try_from(area).unwrap_or(1);
        grid.put_text(width.saturating_sub(more_width + 1), y, &more, Style::StatusAccent);
    }

    let cursor = focused.then(|| {
        let row = u16::try_from(cursor_row - first).unwrap_or(0);
        let column = u16::try_from(cursor_column).unwrap_or(0).min(inner);
        (1 + column, input_top + row, SetCursorStyle::SteadyBar)
    });
    Frame { grid, cursor }
}

fn draw_line(grid: &mut CellGrid, y: u16, transcript: &Styled, bands: &[Range<usize>], line: Range<usize>) {
    let banded = bands.iter().any(|band| band.contains(&line.start));
    if banded {
        grid.fill_row(y, Style::Selection);
    }
    let spans = &transcript.spans;
    let mut span = spans.partition_point(|(range, _)| range.end <= line.start);
    let right = grid.width().saturating_sub(1);
    let mut x = 1;
    for (offset, grapheme) in transcript.text[line.clone()].grapheme_indices(true) {
        let at = line.start + offset;
        while span < spans.len() && spans[span].0.end <= at {
            span += 1;
        }
        let style = match spans.get(span) {
            _ if banded => Style::Selection,
            Some((range, role)) if range.start <= at => syntax_role_style(*role),
            _ => Style::Default,
        };
        x = grid.put_text_clipped(x, y, grapheme, style, right);
    }
}

/// The input split into rows of `width` cells, and the cursor's row and column.
fn wrap_input(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut at = None;
    for (index, character) in input.char_indices() {
        if character == '\n' {
            if index == cursor {
                at = Some((rows.len() - 1, column));
            }
            rows.push(String::new());
            column = 0;
            continue;
        }
        let character_width = character.width().unwrap_or(0);
        if column + character_width > width {
            rows.push(String::new());
            column = 0;
        }
        if index == cursor {
            at = Some((rows.len() - 1, column));
        }
        if let Some(row) = rows.last_mut() {
            row.push(character);
        }
        column += character_width;
    }
    let end = (rows.len() - 1, column);
    (rows, at.unwrap_or(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_wraps_and_the_cursor_follows() {
        assert_eq!(wrap_input("", 0, 4), (vec![String::new()], (0, 0)));
        assert_eq!(wrap_input("abcdef", 6, 4), (vec!["abcd".into(), "ef".into()], (1, 2)));
        assert_eq!(wrap_input("abcd", 4, 4), (vec!["abcd".into()], (0, 4)));
        assert_eq!(wrap_input("ab\ncd", 2, 4), (vec!["ab".into(), "cd".into()], (0, 2)));
        assert_eq!(wrap_input("ab\ncd", 3, 4), (vec!["ab".into(), "cd".into()], (1, 0)));
    }

    #[test]
    fn trailing_empty_lines_are_not_shown() {
        assert_eq!(transcript_lines("a\n\nb\n\n"), [0..1, 2..2, 3..4]);
    }
}
