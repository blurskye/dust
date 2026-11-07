//! Explorer tree rows and viewport drawing.

use crate::config::AmbiguousWidth;
use crate::config::Config;
use crate::ui::CellGrid;
use crate::ui::Style;
use crate::ui::geometry::text_width;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::windows::PANE_MINIMUM_WIDTH;
use crate::windows::WindowRect;

use crate::ui::render::ExplorerSegmentRole;

use crate::ui::render::ExplorerUiState;

use crate::ui::grid::Frame;

use crate::config::Highlight;
use crate::ui::render::editor::cursor_style;

pub(crate) const EXPLORER_MAXIMUM_VISIBLE_ROWS: usize = 200;
pub(crate) const EXPLORER_MINIMUM_WIDTH: u16 = 28;
pub(crate) const EXPLORER_MAXIMUM_WIDTH: u16 = 60;
pub(crate) const EXPLORER_FOCUS_MARKER: &str = ">";
pub(crate) const EXPLORER_LOADING_LABEL: &str = "loading…";

/// Width of the persistent explorer dock, leaving a usable editor pane.
#[must_use]
pub fn explorer_panel_width(config: &Config, terminal_width: u16) -> Option<u16> {
    let separator = 1;
    let minimum_editor = PANE_MINIMUM_WIDTH.max(20);
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
            .clamp(EXPLORER_MINIMUM_WIDTH, EXPLORER_MAXIMUM_WIDTH)
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
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    for y in 0..content.grid.height().min(bounds.height) {
        for x in 0..content.grid.width().min(bounds.width) {
            if let Some(cell) = content.grid.get(x, y)
                && !cell.continuation
            {
                grid.set_cell(
                    bounds.x.saturating_add(x),
                    bounds.y.saturating_add(y),
                    &cell.symbol,
                    cell.style,
                );
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
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let panel_width = rect.width.saturating_sub(1);
    let body = crate::ui::render::explorer::explorer_body_geometry(rect);
    let local_body = WindowRect {
        x: 0,
        y: body.y.saturating_sub(rect.y),
        width: body.width,
        height: body.height,
    };
    let search_cursor = render_explorer_dock(
        &mut local,
        explorer,
        config,
        panel_width,
        rect.height,
        local_body,
    );
    for y in 0..rect.height {
        for x in 0..rect.width {
            if let Some(cell) = local.get(x, y)
                && !cell.continuation
            {
                frame.grid.set_cell(
                    rect.x.saturating_add(x),
                    rect.y.saturating_add(y),
                    &cell.symbol,
                    cell.style,
                );
            }
        }
    }
    if focused && explorer.searching {
        // Typing goes to the search box.
        frame.cursor = search_cursor.map(|x| {
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
) -> Option<u16> {
    if panel_width == 0 || height == 0 {
        return None;
    }
    let panel_height = height;
    let border = if config.ui.explorer.icon_mode == crate::config::ExplorerIconMode::Ascii
        || grid.display_width("│") != 1
    {
        "|"
    } else {
        "│"
    };
    for y in 0..panel_height {
        for x in 0..panel_width {
            grid.set_cell(x, y, " ", Style::ExplorerBackground);
        }
        grid.set_cell(panel_width, y, border, Style::ExplorerBorder);
    }

    let search_cursor = if body.y >= 3 {
        render_explorer_search_box(grid, explorer, config, panel_width)
    } else {
        None
    };

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
            EXPLORER_LOADING_LABEL,
            Style::ExplorerCount,
            panel_width.saturating_sub(1),
        );
    }
    search_cursor
}

/// The titled box at the top of the panel: the prompt, the query and how many rows match.
fn render_explorer_search_box(
    grid: &mut CellGrid,
    explorer: &ExplorerUiState,
    config: &Config,
    panel_width: u16,
) -> Option<u16> {
    if panel_width < 4 {
        return None;
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        if grid.display_width("─") == 1 {
            crate::config::BorderStyle::Rounded.glyphs()
        } else {
            ('-', '|', '+', '+', '+', '+')
        };
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
    let x = grid.put_text_clipped(2, 1, EXPLORER_FOCUS_MARKER, Style::ExplorerHeader, count_x);
    let right = count_x.saturating_sub(1);
    let start = x.saturating_add(1).min(right);
    let (query, cursor) = crate::ui::render::prompt::prompt_viewport(
        &explorer.query,
        explorer.query.len(),
        usize::from(right.saturating_sub(start)),
        grid,
    );
    grid.put_text_clipped(start, 1, query, Style::ExplorerHeader, right);
    Some(start.saturating_add(u16::try_from(cursor).unwrap_or(0)))
}

pub(crate) fn explorer_segment_style(role: ExplorerSegmentRole) -> Style {
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

/// The box around the explorer's search row.
const EXPLORER_SEARCH_ROWS: u16 = 3;

#[must_use]
pub fn explorer_body_geometry(rect: WindowRect) -> WindowRect {
    WindowRect {
        x: rect.x,
        y: rect
            .y
            .saturating_add(EXPLORER_SEARCH_ROWS)
            .min(rect.y.saturating_add(rect.height)),
        width: rect.width.saturating_sub(1),
        height: rect
            .height
            .saturating_sub(EXPLORER_SEARCH_ROWS)
            .min(u16::try_from(EXPLORER_MAXIMUM_VISIBLE_ROWS).unwrap_or(u16::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::render::explorer::*;

    fn explorer(query: &str) -> ExplorerUiState {
        ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 1,
            matches: 1,
            query: query.into(),
            searching: true,
        }
    }

    fn render_search(query: &str, ambiguous: AmbiguousWidth) -> Frame {
        let mut config = Config::default();
        config.editor.ambiguous_width = ambiguous;
        config.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Unicode;
        let mut frame = Frame {
            grid: CellGrid::new_with_policy(
                29,
                8,
                CONTROL_CHARACTER_REPLACEMENT,
                ZERO_WIDTH_REPLACEMENT,
                ambiguous == AmbiguousWidth::Wide,
            ),
            cursor: None,
        };
        render_explorer_window(
            &mut frame,
            &explorer(query),
            &config,
            WindowRect {
                x: 0,
                y: 0,
                width: 29,
                height: 8,
            },
            true,
        );
        frame
    }

    #[test]
    fn wide_explorer_separator_fits_the_final_column() {
        let frame = render_search("", AmbiguousWidth::Wide);
        for y in 0..8 {
            let cell = frame.grid.get(28, y).unwrap();
            assert_eq!(cell.symbol, "|");
            assert!(!cell.continuation);
        }
    }

    #[test]
    fn wide_search_box_borders_stay_within_the_panel() {
        let frame = render_search("", AmbiguousWidth::Wide);
        for y in [0, 2] {
            assert_eq!(frame.grid.get(0, y).unwrap().symbol, "+");
            assert_eq!(frame.grid.get(27, y).unwrap().symbol, "+");
        }
        assert_eq!(frame.grid.row_text(2), format!("+{}+|", "-".repeat(26)));
        assert_eq!(frame.grid.get(27, 1).unwrap().symbol, "|");
        let narrow = render_search("", AmbiguousWidth::Narrow);
        assert_eq!(narrow.grid.get(0, 0).unwrap().symbol, "╭");
        assert_eq!(narrow.grid.get(28, 1).unwrap().symbol, "│");
    }

    #[test]
    fn search_query_scrolls_with_the_cursor_before_the_count() {
        for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
            for tail in ["z", "界", "e\u{301}", "·"] {
                let query = format!("{}{tail}", "a".repeat(40));
                let frame = render_search(&query, ambiguous);
                let line = frame.grid.row_text(1);
                assert!(line.contains(tail), "{line}");
                let (x, y, _) = frame.cursor.unwrap();
                assert_eq!(y, 1);
                assert!(x < 25, "{x}: {line}");
                assert_eq!(frame.grid.get(x, y).unwrap().symbol, " ");
                assert_eq!(frame.grid.get(25, y).unwrap().symbol, "1");
                assert_eq!(
                    frame.grid.get(x - 1, y).unwrap().style,
                    Style::ExplorerHeader
                );
            }
        }
    }
}
