use crate::ui::render::{CONTROL_CHARACTER_REPLACEMENT, ZERO_WIDTH_REPLACEMENT};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::{AmbiguousWidth, Config},
    editor::EditorState,
    ui::{CellGrid, Style, geometry::text_width, window::WindowRect},
};

use super::{
    CompositorLayoutState, Frame, KeyHintUiState, TerminalPanelGeometry, TerminalPanelMode,
    TerminalPanelState, TerminalPhase, TerminalTabState, UiState,
};

pub(crate) const KEY_HINT_MAX_ROWS: u16 = 8;
pub(crate) const KEY_HINT_MAX_COLUMNS: u16 = 3;
pub(crate) const KEY_HINT_MAX_WIDTH: u16 = 96;
pub(crate) const KEY_HINT_PADDING: u16 = 1;
pub(crate) const KEY_HINT_COLUMN_GAP: u16 = 2;
pub(crate) const KEY_HINT_FALLBACK_LABEL: &str = "Action";
pub(crate) const TERMINAL_PANEL_DIVIDER: &str = "─";
pub(crate) const TERMINAL_TAB_SEPARATOR: &str = " │ ";
pub(crate) const TERMINAL_NORMAL_LABEL: &str = "NORMAL";
pub(crate) const TERMINAL_VISUAL_LABEL: &str = "VISUAL";

pub(super) fn active_compositor<'a>(
    state: &EditorState,
    ui: &'a UiState,
    width: u16,
    height: u16,
) -> Option<&'a CompositorLayoutState> {
    let revision = state.window_revision()?;
    ui.compositor_layout.as_ref().filter(|compositor| {
        compositor.source_revision == revision
            && Some(compositor.graph_revision) == ui.window_graph_revision
            && compositor.projection.bounds.width == width
            && compositor.projection.bounds.height == height
    })
}

pub(crate) fn terminal_geometry_from_rect(rect: WindowRect) -> Option<TerminalPanelGeometry> {
    (rect.width > 0 && rect.height >= 3).then_some(TerminalPanelGeometry {
        x: rect.x,
        width: rect.width,
        top: rect.y,
        height: rect.height,
        body_top: rect.y.saturating_add(1),
        body_height: rect.height.saturating_sub(1),
    })
}

/// A tab's name: its profile, numbered when an earlier tab has the same one.
pub(super) fn terminal_tab_label(tabs: &[TerminalTabState], tab: &TerminalTabState) -> String {
    let earlier = tabs
        .iter()
        .take_while(|other| other.id != tab.id)
        .filter(|other| other.profile == tab.profile)
        .count();
    let mut title = if earlier == 0 {
        tab.profile.clone()
    } else {
        format!("{} {}", tab.profile, earlier + 1)
    };
    if let Some(status) = match tab.phase {
        TerminalPhase::Preflight => Some(" [preflight]"),
        TerminalPhase::Starting => Some(" [starting]"),
        TerminalPhase::Stopping => Some(" [stopping]"),
        TerminalPhase::Failed => Some(" [failed]"),
        TerminalPhase::Unknown => Some(" [unknown]"),
        _ => None,
    } {
        title.push_str(status);
    }
    if tab.phase == TerminalPhase::Exited {
        title.push_str(
            &tab.exit_code
                .map_or_else(|| " [signal]".into(), |code| format!(" [exit {code}]")),
        );
    }
    title
}

#[must_use]
pub fn terminal_tab_at_column(
    terminal: &TerminalPanelState,
    config: &Config,
    width: u16,
    column: u16,
) -> Option<u64> {
    let mut x = 1_u16;
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(
                u16::try_from(text_width(
                    &TERMINAL_TAB_SEPARATOR,
                    config.editor.ambiguous_width,
                ))
                .unwrap_or(u16::MAX),
            );
        }
        let label = terminal_tab_label(&terminal.tabs, tab);
        let end = x
            .saturating_add(
                u16::try_from(text_width(&label, config.editor.ambiguous_width))
                    .unwrap_or(u16::MAX),
            )
            .min(width);
        if column >= x && column < end {
            return Some(tab.id);
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
}

pub(super) fn dock_terminal_panel(
    workspace: Frame,
    terminal: &TerminalPanelState,
    geometry: TerminalPanelGeometry,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &CONTROL_CHARACTER_REPLACEMENT,
        &ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    for y in 0..workspace.grid.height().min(height) {
        for x in 0..workspace.grid.width().min(width) {
            let Some(cell) = workspace.grid.get(x, y) else {
                continue;
            };
            if !cell.continuation {
                grid.set_cell(x, y, &cell.symbol, cell.style);
            }
        }
    }
    let right = geometry.x.saturating_add(geometry.width).min(width);
    let bottom = geometry.top.saturating_add(geometry.height).min(height);
    for y in geometry.top..bottom {
        for x in geometry.x..right {
            grid.set_cell(x, y, " ", Style::TerminalBackground);
        }
    }
    for x in geometry.x..right {
        grid.set_cell(
            x,
            geometry.top,
            &TERMINAL_PANEL_DIVIDER,
            Style::TerminalBorder,
        );
    }
    let mut title_x = geometry.x.saturating_add(1);
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            title_x = grid.put_text_clipped(
                title_x,
                geometry.top,
                &TERMINAL_TAB_SEPARATOR,
                Style::TerminalBorder,
                right,
            );
        }
        let title = terminal_tab_label(&terminal.tabs, tab);
        title_x = grid.put_text_clipped(
            title_x,
            geometry.top,
            &title,
            if tab.id == terminal.id {
                Style::TerminalTitle
            } else {
                Style::Muted
            },
            right,
        );
    }
    // Input is the usual mode, so only the others are named, at the end of the tab row.
    let mode = match terminal.mode {
        TerminalPanelMode::Input => None,
        TerminalPanelMode::Normal => Some(TERMINAL_NORMAL_LABEL),
        TerminalPanelMode::Visual => Some(TERMINAL_VISUAL_LABEL),
    };
    let notice = [mode, terminal.truncated.then_some("output truncated")]
        .into_iter()
        .flatten()
        .map(|text| format!(" {text} "))
        .collect::<String>();
    if !notice.is_empty() {
        let notice_width =
            u16::try_from(text_width(&notice, config.editor.ambiguous_width)).unwrap_or(u16::MAX);
        let notice_x = right.saturating_sub(notice_width.saturating_add(1));
        if notice_x > title_x {
            grid.put_text_clipped(notice_x, geometry.top, &notice, Style::TerminalTitle, right);
        }
    }

    let visible_rows = usize::from(geometry.body_height).min(terminal.cells.len());
    let source_start = terminal.cells_start;
    for (screen_row, row) in terminal.cells.iter().take(visible_rows).enumerate() {
        let source_row = source_start.saturating_add(screen_row);
        let y = geometry
            .body_top
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        for (column, cell) in row
            .iter()
            .take(usize::from(geometry.width.min(terminal.columns)))
            .enumerate()
        {
            if cell.continuation {
                continue;
            }
            let selected = terminal.selection.is_some_and(|(first, second, linewise)| {
                let (start, end) = if first <= second {
                    (first, second)
                } else {
                    (second, first)
                };
                if linewise {
                    (start.0..=end.0).contains(&source_row)
                } else {
                    (source_row, column) >= start && (source_row, column) <= end
                }
            });
            grid.set_cell(
                geometry
                    .x
                    .saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
                y,
                &cell.text,
                if selected {
                    Style::Selection
                } else {
                    Style::Terminal(cell.style)
                },
            );
        }
    }

    let cursor = if terminal.focused {
        terminal.cursor.and_then(|(column, source_row)| {
            let projected_row = source_row.checked_sub(source_start)?;
            let y = geometry
                .body_top
                .saturating_add(u16::try_from(projected_row).ok()?);
            (column < usize::from(geometry.width) && y < bottom).then_some((
                geometry.x.saturating_add(u16::try_from(column).ok()?),
                y,
                crossterm::cursor::SetCursorStyle::SteadyBlock,
            ))
        })
    } else {
        workspace.cursor
    };
    Frame { grid, cursor }
}

pub(super) fn render_key_hints(
    frame: &mut Frame,
    hints: &KeyHintUiState,
    editor_left: u16,
    editor_width: u16,
    terminal_height: u16,
) {
    let available_height = terminal_height.saturating_sub(1);
    if editor_width < 12 || available_height < 4 || hints.rows.is_empty() {
        return;
    }
    let row_count = hints
        .rows
        .len()
        .min(usize::from(KEY_HINT_MAX_ROWS).saturating_mul(usize::from(KEY_HINT_MAX_COLUMNS)));
    let columns = usize::from(KEY_HINT_MAX_COLUMNS)
        .min(row_count.div_ceil(usize::from(KEY_HINT_MAX_ROWS)))
        .max(1);
    let rows_per_column = row_count
        .div_ceil(columns)
        .min(usize::from(KEY_HINT_MAX_ROWS));
    let desired_height = 3usize.saturating_add(rows_per_column);
    let box_height = u16::try_from(desired_height)
        .unwrap_or(available_height)
        .min(available_height);
    if box_height < 4 {
        return;
    }
    let widest = hints
        .rows
        .iter()
        .take(row_count)
        .map(|row| {
            UnicodeWidthStr::width(row.next_key.as_str())
                + UnicodeWidthStr::width(row.label.as_str())
                + row
                    .icon
                    .as_deref()
                    .map_or(0, |icon| UnicodeWidthStr::width(icon) + 1)
                + 3
        })
        .max()
        .unwrap_or(1);
    let desired_width = widest
        .saturating_mul(columns)
        .saturating_add(usize::from(KEY_HINT_COLUMN_GAP).saturating_mul(columns.saturating_sub(1)))
        .saturating_add(usize::from(KEY_HINT_PADDING).saturating_mul(2))
        .saturating_add(2);
    let box_width = u16::try_from(desired_width)
        .unwrap_or(KEY_HINT_MAX_WIDTH)
        .min(KEY_HINT_MAX_WIDTH)
        .min(editor_width);
    if box_width < 12 {
        return;
    }
    let left = editor_left.saturating_add(editor_width.saturating_sub(box_width));
    let top = available_height.saturating_sub(box_height);
    let right = left.saturating_add(box_width);
    let bottom = top.saturating_add(box_height).saturating_sub(1);
    for y in top..=bottom {
        for x in left..right {
            frame.grid.set_cell(x, y, " ", Style::KeyHintBackground);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        crate::config::BorderStyle::Rounded.glyphs();
    for x in left.saturating_add(1)..right.saturating_sub(1) {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::KeyHintBorder);
        frame
            .grid
            .set_cell(x, bottom, &horizontal.to_string(), Style::KeyHintBorder);
    }
    for y in top.saturating_add(1)..bottom {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::KeyHintBorder);
        frame.grid.set_cell(
            right.saturating_sub(1),
            y,
            &vertical.to_string(),
            Style::KeyHintBorder,
        );
    }
    frame
        .grid
        .set_cell(left, top, &top_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        top,
        &top_right.to_string(),
        Style::KeyHintBorder,
    );
    frame
        .grid
        .set_cell(left, bottom, &bottom_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        bottom,
        &bottom_right.to_string(),
        Style::KeyHintBorder,
    );
    let inner_left = left
        .saturating_add(1)
        .saturating_add(KEY_HINT_PADDING);
    let inner_right = right.saturating_sub(1 + KEY_HINT_PADDING);
    if inner_left >= inner_right {
        return;
    }
    frame.grid.put_text_clipped(
        inner_left,
        top.saturating_add(1),
        &hints.prefix,
        Style::KeyHintPrefix,
        inner_right,
    );
    let body_top = top.saturating_add(2);
    let gap_total = KEY_HINT_COLUMN_GAP
        .saturating_mul(u16::try_from(columns.saturating_sub(1)).unwrap_or(0));
    let column_width = inner_right
        .saturating_sub(inner_left)
        .saturating_sub(gap_total)
        / u16::try_from(columns).unwrap_or(1);
    if column_width < 3 {
        return;
    }
    for (index, row) in hints.rows.iter().take(row_count).enumerate() {
        let column = index / rows_per_column;
        let row_index = index % rows_per_column;
        let x = inner_left.saturating_add(
            u16::try_from(column)
                .unwrap_or(0)
                .saturating_mul(column_width.saturating_add(KEY_HINT_COLUMN_GAP)),
        );
        let y = body_top.saturating_add(
            u16::try_from(row_index).unwrap_or(u16::MAX),
        );
        if y >= bottom {
            continue;
        }
        let column_right = x.saturating_add(column_width).min(inner_right);
        let mut cursor = frame.grid.put_text_clipped(
            x,
            y,
            &format!("[{}]", row.next_key),
            Style::KeyHintKey,
            column_right,
        );
        if let Some(icon) = &row.icon {
            cursor = frame.grid.put_text_clipped(
                cursor.saturating_add(1),
                y,
                icon,
                Style::KeyHintGroup,
                column_right,
            );
        }
        // A label too long for its column ends in an ellipsis instead of stopping mid-word.
        let room = usize::from(column_right.saturating_sub(cursor.saturating_add(1)));
        let label = super::surfaces::ellipsized(&row.label, room, crate::config::AmbiguousWidth::Narrow);
        frame.grid.put_text_clipped(
            cursor.saturating_add(1),
            y,
            &label,
            if row.group {
                Style::KeyHintGroup
            } else {
                Style::KeyHintCommand
            },
            column_right,
        );
    }
}
