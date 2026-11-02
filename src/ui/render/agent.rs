//! The agent panel: its session tabs on top (drawn like the buffer line), the conversation,
//! the subagents or the slash-command menu, the input box with what the agent is doing in
//! its top border, and a footer with where the session works and its model. It is drawn in
//! place of the pane that holds it, so the pane still moves, resizes and zooms like any
//! other window.

use std::ops::Range;

use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::{AmbiguousWidth, Config},
    editor::ViewId,
    ui::{CellGrid, Style, geometry::text_width, markdown::Styled},
};

use super::{ContentTabEntry, Frame, overlays::render_content_tab_line};

/// The input box grows with what is typed up to this many rows, then scrolls.
const INPUT_ROW_CEILING: usize = 8;
/// Rows of the slash-command menu and of the subagent list.
const LIST_ROW_CEILING: usize = 8;

/// One session of the shown conversation's family: the main session and its subagents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentListRow {
    pub shown: bool,
    pub name: String,
    pub task: String,
    /// A spinner frame while it works, `✓` or `✗` once done.
    pub status: String,
}

/// What the session hands the renderer for the pane that shows `view`.
#[derive(Clone, Debug)]
pub struct AgentPanelUi {
    pub view: ViewId,
    /// The sessions, as buffer-line entries whose identity is the session's index.
    pub tabs: Vec<ContentTabEntry>,
    /// The conversation, wrapped to the pane's width less the side padding.
    pub transcript: Styled,
    /// Byte ranges of the transcript drawn as the user's messages.
    pub bands: Vec<Range<usize>>,
    /// Rows scrolled up from the end; 0 follows new output.
    pub scroll: usize,
    /// What goes with the next question.
    pub context: Vec<String>,
    /// The shown session's family, when it has subagents.
    pub family: Vec<AgentListRow>,
    /// Slash commands matching what is typed, by name and description, and the selected one.
    pub menu: Vec<(String, String)>,
    pub selected: usize,
    /// What the agent is doing, such as `⠋ Working`, shown in the input's top border.
    pub busy: String,
    /// Whether the input is in Vim's Normal mode.
    pub normal: bool,
    pub input: String,
    /// Byte offset of the cursor in `input`.
    pub cursor: usize,
    pub placeholder: String,
    /// Left of the footer: where the session works.
    pub status: String,
    /// Right of the footer: model, mode and thinking level.
    pub settings: String,
}

/// Where the transcript's lines start, without the trailing empty ones outside the bands, so
/// the user's last message keeps its padding while the answer is awaited.
#[must_use]
pub fn transcript_lines(text: &str, bands: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        lines.push(start..start + line.len());
        start += line.len() + 1;
    }
    while lines
        .last()
        .is_some_and(|line| line.is_empty() && !bands.iter().any(|band| band.contains(&line.start)))
    {
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
    status_row: bool,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    // The last row is left for the status line the caller draws there.
    let height = height - u16::from(status_row);
    if width < 8 || height < 6 {
        return Frame { grid, cursor: None };
    }
    render_content_tab_line(&mut grid, &panel.tabs, config, 0);

    let inner = width - 2;
    let chips = u16::from(!panel.context.is_empty());
    let list = if panel.menu.is_empty() { panel.family.len() } else { panel.menu.len() };
    let list = u16::try_from(list.min(LIST_ROW_CEILING)).unwrap_or(0).min(height.saturating_sub(8) / 2);
    let (rows, (cursor_row, cursor_column)) = wrap_input(&panel.input, panel.cursor, usize::from(inner));
    let room = usize::from(height.saturating_sub(6 + chips + list)).max(1);
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
    if !panel.busy.is_empty() {
        let x = grid.put_text(2, top, " ", border);
        let x = grid.put_text_clipped(x, top, &panel.busy, Style::Accent, width - 2);
        grid.put_text(x, top, " ", border);
    }
    let list_top = top - list;
    if panel.menu.is_empty() {
        for (offset, row) in panel.family.iter().take(usize::from(list)).enumerate() {
            let y = list_top + u16::try_from(offset).unwrap_or(0);
            let (mark, style) = if row.shown { ("● ", Style::Syntax(crate::language::syntax::Highlight::STRONG)) } else { ("○ ", Style::Muted) };
            let x = grid.put_text(1, y, mark, style);
            let x = grid.put_text_clipped(x, y, &row.name, style, width - 1);
            let x = grid.put_text_clipped(x.saturating_add(2), y, &row.task, Style::Muted, width - 1);
            let status = match row.status.as_str() {
                "✓" => Style::GitSignAdded,
                "✗" => Style::Error,
                _ => Style::Accent,
            };
            grid.put_text_clipped(x.saturating_add(2), y, &row.status, status, width - 1);
        }
    } else {
        let shown = usize::from(list);
        let first = panel.selected.saturating_sub(shown.saturating_sub(1));
        for (offset, (name, description)) in panel.menu.iter().skip(first).take(shown).enumerate() {
            let y = list_top + u16::try_from(offset).unwrap_or(0);
            let selected = first + offset == panel.selected;
            if selected {
                grid.fill_row(y, Style::Selection);
            }
            let (name_style, text_style) = if selected {
                (Style::Selection, Style::Selection)
            } else {
                (Style::Accent, Style::Muted)
            };
            let x = grid.put_text_clipped(1, y, &format!("/{name}"), name_style, width - 1);
            let start = x.saturating_add(2).max(18);
            let room = usize::from((width - 1).saturating_sub(start));
            let description = super::surfaces::ellipsized(description, room, config.editor.ambiguous_width);
            grid.put_text_clipped(start, y, &description, text_style, width - 1);
        }
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
            x = grid.put_text_clipped(x, list_top - 1, &format!("+ {label}"), Style::Accent, width - 1);
            x = x.saturating_add(2);
        }
    }
    let settings_width = u16::try_from(text_width(&panel.settings, config.editor.ambiguous_width)).unwrap_or(width);
    let settings_x = width.saturating_sub(settings_width + 1);
    grid.put_text_clipped(1, footer, &panel.status, Style::Muted, settings_x.saturating_sub(1));
    grid.put_text_clipped(settings_x, footer, &panel.settings, Style::Muted, width - 1);

    let area = usize::from(list_top - chips - 1);
    let lines = transcript_lines(&panel.transcript.text, &panel.bands);
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
        let shape = if panel.normal { SetCursorStyle::SteadyBlock } else { SetCursorStyle::SteadyBar };
        (1 + column, input_top + row, shape)
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
            Some((range, role)) if range.start <= at => Style::Syntax(*role),
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
        assert_eq!(transcript_lines("a\n\nb\n\n", &[]), [0..1, 2..2, 3..4]);
        assert_eq!(transcript_lines("a\n\nb\n\n", &[2..6]), [0..1, 2..2, 3..4, 5..5]);
    }
}
