use crate::{
    config::{AmbiguousWidth, Config},
    ui::{CellGrid, Style, geometry::text_width, window::WindowRect},
};

use super::{ExplorerSegmentRole, ExplorerUiState, Frame, editor::cursor_style};
use crate::language::syntax::Highlight;

/// Width of the persistent explorer dock, leaving a usable editor pane.
#[must_use]
pub fn explorer_panel_width(config: &Config, terminal_width: u16) -> Option<u16> {
    let separator = 1;
    let minimum_editor = config.ui.panes.minimum_width.max(20);
    if terminal_width < 3 {
        return None;
    }
    let maximum_panel = terminal_width.saturating_sub(minimum_editor.saturating_add(separator));
    if maximum_panel < 12 {
        return Some(terminal_width.saturating_sub(separator) / 2);
    }
    Some(
        config
            .ui
            .explorer
            .width
            .clamp(
                config.ui.explorer.minimum_width,
                config.ui.explorer.maximum_width,
            )
            .min(maximum_panel),
    )
}

pub(super) fn place_workspace_frame(
    content: Frame,
    bounds: WindowRect,
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
    for y in 0..content.grid.height().min(bounds.height) {
        for x in 0..content.grid.width().min(bounds.width) {
            if let Some(cell) = content.grid.get(x, y) {
                if !cell.continuation {
                    grid.set_cell(
                        bounds.x.saturating_add(x),
                        bounds.y.saturating_add(y),
                        &cell.symbol,
                        cell.style,
                    );
                }
            }
        }
    }
    let cursor = content.cursor.and_then(|(x, y, shape)| {
        (x < bounds.width && y < bounds.height).then_some((
            bounds.x.saturating_add(x),
            bounds.y.saturating_add(y),
            shape,
        ))
    });
    Frame { grid, cursor }
}

pub(super) fn render_explorer_window(
    frame: &mut Frame,
    explorer: &ExplorerUiState,
    config: &Config,
    rect: WindowRect,
    focused: bool,
) {
    if rect.width < 2 || rect.height == 0 {
        return;
    }
    let mut local = CellGrid::new_with_policy(
        rect.width,
        rect.height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let panel_width = rect.width.saturating_sub(1);
    let body = crate::ui::geometry::explorer_body_geometry(rect, config);
    let local_body = WindowRect {
        x: 0,
        y: body.y.saturating_sub(rect.y),
        width: body.width,
        height: body.height,
    };
    render_explorer_dock(
        &mut local,
        explorer,
        config,
        panel_width,
        rect.height,
        local_body,
    );
    for y in 0..rect.height {
        for x in 0..rect.width {
            if let Some(cell) = local.get(x, y) {
                if !cell.continuation {
                    frame.grid.set_cell(
                        rect.x.saturating_add(x),
                        rect.y.saturating_add(y),
                        &cell.symbol,
                        cell.style,
                    );
                }
            }
        }
    }
    if focused && explorer.searching {
        // Typing goes to the search box.
        let typed = text_width(&config.ui.explorer.focus_marker, config.editor.ambiguous_width)
            + 1
            + text_width(&explorer.query, config.editor.ambiguous_width);
        let x = u16::try_from(typed)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(panel_width.saturating_sub(2));
        frame.cursor = (rect.height > 1).then(|| {
            (
                rect.x.saturating_add(x),
                rect.y.saturating_add(1),
                cursor_style(config.ui.cursor.insert),
            )
        });
    } else if focused {
        frame.cursor = explorer
            .rows
            .iter()
            .position(|row| row.selected)
            .and_then(|row| u16::try_from(row).ok())
            .map(|row| {
                (
                    // The margin left of the tree, so the cursor doesn't cover its lines.
                    rect.x,
                    body.y.saturating_add(row),
                    cursor_style(config.ui.cursor.normal),
                )
            })
            .filter(|(_, y, _)| *y < body.y.saturating_add(body.height));
    }
}

pub(super) fn render_explorer_dock(
    grid: &mut CellGrid,
    explorer: &ExplorerUiState,
    config: &Config,
    panel_width: u16,
    height: u16,
    body: WindowRect,
) {
    if panel_width == 0 || height == 0 {
        return;
    }
    let panel_height = height;
    let glyphs = config.ui.explorer.active_glyphs();
    for y in 0..panel_height {
        for x in 0..panel_width {
            grid.set_cell(x, y, " ", Style::ExplorerBackground);
        }
        grid.set_cell(
            panel_width,
            y,
            &glyphs.panel_vertical,
            Style::ExplorerBorder,
        );
    }

    if body.y >= 3 {
        render_explorer_search_box(grid, explorer, config, panel_width);
    }

    let body_top = body.y;
    let available_rows = usize::from(body.height);
    let mut rendered_rows = 0usize;
    for (screen_row, row) in explorer.rows.iter().take(available_rows).enumerate() {
        rendered_rows = rendered_rows.saturating_add(1);
        let y = body_top.saturating_add(u16::try_from(screen_row).unwrap_or(0));
        if row.selected {
            for x in 0..panel_width {
                grid.set_cell(x, y, " ", Style::ExplorerSelection);
            }
        }
        let right_width = row.right_segments.iter().fold(0usize, |width, segment| {
            width.saturating_add(text_width(&segment.text, config.editor.ambiguous_width))
        });
        let content_right = panel_width.saturating_sub(1);
        let right_x =
            content_right.saturating_sub(u16::try_from(right_width).unwrap_or(content_right));
        let main_right = if row.right_segments.is_empty() {
            content_right
        } else {
            right_x
        };
        let mut x = 1;
        for segment in &row.segments {
            x = grid.put_text_clipped(
                x,
                y,
                &segment.text,
                if row.selected {
                    Style::ExplorerSelection
                } else {
                    explorer_segment_style(segment.role)
                },
                main_right,
            );
        }
        let mut right_x = right_x;
        for segment in &row.right_segments {
            right_x = grid.put_text_clipped(
                right_x,
                y,
                &segment.text,
                if row.selected {
                    Style::ExplorerSelection
                } else {
                    explorer_segment_style(segment.role)
                },
                content_right,
            );
        }
    }

    if explorer.loading && rendered_rows < available_rows {
        let y = body_top.saturating_add(u16::try_from(rendered_rows).unwrap_or(0));
        grid.put_text_clipped(
            1,
            y,
            &config.ui.explorer.loading_label,
            Style::ExplorerCount,
            panel_width.saturating_sub(1),
        );
        rendered_rows = rendered_rows.saturating_add(1);
    }
    if !explorer.preview_lines.is_empty() && rendered_rows < available_rows {
        let divider_y = body_top.saturating_add(u16::try_from(rendered_rows).unwrap_or(0));
        for x in 0..panel_width {
            grid.set_cell(
                x,
                divider_y,
                &glyphs.panel_horizontal,
                Style::ExplorerBorder,
            );
        }
        for (index, line) in explorer
            .preview_lines
            .iter()
            .take(
                available_rows
                    .saturating_sub(rendered_rows)
                    .saturating_sub(1),
            )
            .enumerate()
        {
            grid.put_text_clipped(
                1,
                divider_y
                    .saturating_add(1)
                    .saturating_add(u16::try_from(index).unwrap_or(0)),
                line,
                Style::ExplorerDecoration,
                panel_width.saturating_sub(1),
            );
        }
    }
}

/// The titled box at the top of the panel: the prompt, the query and how many rows match.
fn render_explorer_search_box(
    grid: &mut CellGrid,
    explorer: &ExplorerUiState,
    config: &Config,
    panel_width: u16,
) {
    if panel_width < 4 {
        return;
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        config.ui.explorer.search_border.glyphs();
    let last = panel_width - 1;
    for x in 1..last {
        grid.set_cell(x, 0, &horizontal.to_string(), Style::ExplorerBorder);
        grid.set_cell(x, 1, " ", Style::ExplorerHeader);
        grid.set_cell(x, 2, &horizontal.to_string(), Style::ExplorerBorder);
    }
    for (x, top, bottom) in [(0, top_left, bottom_left), (last, top_right, bottom_right)] {
        grid.set_cell(x, 0, &top.to_string(), Style::ExplorerBorder);
        grid.set_cell(x, 1, &vertical.to_string(), Style::ExplorerBorder);
        grid.set_cell(x, 2, &bottom.to_string(), Style::ExplorerBorder);
    }
    let title = format!(" {} ", explorer.title);
    let title_width =
        u16::try_from(text_width(&title, config.editor.ambiguous_width)).unwrap_or(last);
    grid.put_text_clipped(
        panel_width.saturating_sub(title_width) / 2,
        0,
        &title,
        Style::ExplorerHeader,
        last,
    );
    // How many entries are listed, or how many match while a search filters the tree.
    let count = if explorer.query.is_empty() {
        format!("{0}/{0}", explorer.total)
    } else {
        explorer.matches.to_string()
    };
    let count_width =
        u16::try_from(text_width(&count, config.editor.ambiguous_width)).unwrap_or(last);
    let count_x = last.saturating_sub(count_width).saturating_sub(1);
    grid.put_text_clipped(count_x, 1, &count, Style::ExplorerCount, last);
    let x = grid.put_text_clipped(
        2,
        1,
        &config.ui.explorer.focus_marker,
        Style::ExplorerHeader,
        count_x,
    );
    grid.put_text_clipped(
        x.saturating_add(1),
        1,
        &explorer.query,
        Style::ExplorerHeader,
        count_x.saturating_sub(1),
    );
}

pub(super) fn explorer_segment_style(role: ExplorerSegmentRole) -> Style {
    match role {
        ExplorerSegmentRole::Tree => Style::ExplorerTree,
        ExplorerSegmentRole::Icon | ExplorerSegmentRole::IconGrey => Style::ExplorerIcon,
        ExplorerSegmentRole::IconAzure => Style::Accent,
        ExplorerSegmentRole::IconBlue => Style::Syntax(Highlight::FUNCTION),
        ExplorerSegmentRole::IconCyan => Style::Syntax(Highlight::PROPERTY),
        ExplorerSegmentRole::IconGreen => Style::GitSignAdded,
        ExplorerSegmentRole::IconOrange => Style::Syntax(Highlight::NUMBER),
        ExplorerSegmentRole::IconPurple => Style::ExplorerSymlink,
        ExplorerSegmentRole::IconRed => Style::GitSignDeleted,
        ExplorerSegmentRole::IconYellow => Style::GitSignChanged,
        ExplorerSegmentRole::Directory => Style::ExplorerDirectory,
        ExplorerSegmentRole::File => Style::ExplorerFile,
        ExplorerSegmentRole::Symlink => Style::ExplorerSymlink,
        ExplorerSegmentRole::Loading => Style::ExplorerDecoration,
        ExplorerSegmentRole::Git => Style::ExplorerGit,
        ExplorerSegmentRole::Diagnostic => Style::ExplorerDiagnostic,
    }
}
