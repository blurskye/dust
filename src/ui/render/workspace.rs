use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneViewport {
    pub pane: crate::editor::PaneId,
    pub view: crate::editor::ViewId,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy)]
pub(super) struct PaneRect {
    pane: crate::editor::PaneId,
    view: crate::editor::ViewId,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

pub(super) struct WorkspaceProjection {
    tab_line_visible: bool,
    buffer_line_visible: bool,
    status_rows: u16,
    panes: Vec<PaneRect>,
    borders: Vec<crate::ui::window::ProjectedBorder>,
}

pub(super) fn workspace_projection(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<WorkspaceProjection> {
    state.tabs.get(state.active_tab?)?;
    let source_revision = state.window_revision()?;
    let layout = ui.editor_window_layout.as_ref()?;
    let compositor = ui.compositor_layout.as_ref()?;
    if layout.source_revision != source_revision
        || compositor.source_revision != source_revision
        || compositor.projection.workspace_bounds.width != width
        || compositor.projection.workspace_bounds.height != height
    {
        return None;
    }
    let tab_line_visible = config.ui.tab_line.enabled
        && (!config.ui.tab_line.hide_when_single || state.tabs.len() > 1);
    let buffer_line_visible = config.ui.buffer_line.enabled
        && (!config.ui.buffer_line.hide_when_single || ui.content_tabs.len() > 1);
    let top = u16::from(tab_line_visible).saturating_add(u16::from(buffer_line_visible));
    let status_rows = u16::from(config.ui.status_line.enabled && height > top);
    let origin = compositor.projection.workspace_bounds;
    let panes = compositor
        .projection
        .windows
        .iter()
        .filter(|projected| projected.layer == crate::ui::window::CompositorLayer::Tiled)
        .filter_map(|projected| {
            let binding = layout
                .bindings
                .iter()
                .find(|binding| binding.window == projected.window)?;
            Some(PaneRect {
                pane: binding.pane,
                view: binding.view,
                x: projected.rect.x.saturating_sub(origin.x),
                y: projected.rect.y.saturating_sub(origin.y),
                width: projected.rect.width,
                height: projected.rect.height,
            })
        })
        .collect();
    let borders = compositor
        .projection
        .borders
        .iter()
        .map(|border| crate::ui::window::ProjectedBorder {
            x: border.x.saturating_sub(origin.x),
            y: border.y.saturating_sub(origin.y),
            ..*border
        })
        .collect();
    Some(WorkspaceProjection {
        tab_line_visible,
        buffer_line_visible,
        status_rows,
        panes,
        borders,
    })
}

/// Exact generic-window rectangles used by rendering, cursor following, and mouse projection.
#[must_use]
pub fn pane_viewports(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Vec<PaneViewport> {
    workspace_projection(state, ui, config, width, height).map_or_else(Vec::new, |projection| {
        projection
            .panes
            .into_iter()
            .map(|rect| PaneViewport {
                pane: rect.pane,
                view: rect.view,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect()
    })
}

#[must_use]
pub fn active_pane_viewport(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<PaneViewport> {
    let tab = state.tabs.get(state.active_tab?)?;
    pane_viewports(state, ui, config, width, height)
        .into_iter()
        .find(|viewport| viewport.pane == tab.focused)
}

pub(super) fn render_workspace(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let Some(tab_id) = state.active_tab else {
        let mut grid = CellGrid::new_with_policy(
            width,
            height,
            &config.editor.control_character_replacement,
            &config.editor.zero_width_replacement,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        );
        grid.put_text(0, 0, &config.editor.end_of_buffer_marker, Style::Muted);
        return Frame { grid, cursor: None };
    };
    let Some(tab) = state.tabs.get(tab_id) else {
        return Frame {
            grid: CellGrid::new(width, height),
            cursor: None,
        };
    };
    let pane_count = ui
        .editor_window_layout
        .as_ref()
        .map_or(0, |layout| layout.bindings.len());
    let Some(projection) = workspace_projection(state, ui, config, width, height) else {
        return Frame {
            grid: CellGrid::new(width, height),
            cursor: None,
        };
    };
    if pane_count == 1 && !projection.tab_line_visible && !projection.buffer_line_visible {
        let Some(view) = projection.panes.first().map(|pane| pane.view) else {
            return Frame {
                grid: CellGrid::new(width, height),
                cursor: None,
            };
        };
        return render_editor_view(state, view, ui, config, width, height);
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if projection.tab_line_visible {
        grid.fill_row(0, Style::Status);
        let mut x = 0;
        for (candidate, candidate_tab) in &state.tabs {
            let title = state
                .panes
                .get(candidate_tab.focused)
                .and_then(|pane| state.views.get(pane.view))
                .and_then(|view| state.buffers.get(view.buffer))
                .and_then(|buffer| buffer.path.as_ref())
                .and_then(|path| path.file_name())
                .map_or_else(
                    || config.ui.tab_line.scratch_label.clone(),
                    |name| name.to_string_lossy().into_owned(),
                );
            let format = if candidate == tab_id {
                &config.ui.tab_line.active_format
            } else {
                &config.ui.tab_line.inactive_format
            };
            x = grid.put_text(
                x,
                0,
                &format.replace("{title}", &title),
                if candidate == tab_id {
                    Style::StatusAccent
                } else {
                    Style::Status
                },
            );
            x = grid.put_text(x, 0, &config.ui.tab_line.separator, Style::Status);
        }
    }
    if projection.buffer_line_visible {
        render_content_tab_line(
            &mut grid,
            &ui.content_tabs,
            config,
            u16::from(projection.tab_line_visible),
        );
    }
    let mut cursor = None;
    let mut pane_config = config.clone();
    pane_config.ui.status_line.enabled = false;
    for rect in projection.panes {
        let frame = render_editor_view(state, rect.view, ui, &pane_config, rect.width, rect.height);
        for y in 0..rect.height {
            for x in 0..rect.width {
                let Some(cell) = frame.grid.get(x, y) else {
                    continue;
                };
                if !cell.continuation {
                    grid.set_cell(
                        rect.x.saturating_add(x),
                        rect.y.saturating_add(y),
                        &cell.symbol,
                        cell.style,
                    );
                }
            }
        }
        if rect.pane == tab.focused
            && let Some((x, y, shape)) = frame.cursor
        {
            cursor = Some((rect.x.saturating_add(x), rect.y.saturating_add(y), shape));
        }
    }
    let (horizontal, vertical) = match config.ui.panes.border {
        BorderStyle::Double => ('═', '║'),
        BorderStyle::Single | BorderStyle::Rounded => ('─', '│'),
    };
    for border in projection.borders {
        for offset in 0..border.length {
            let (cell_x, cell_y, symbol) = match border.axis {
                TileAxis::Horizontal => (border.x, border.y.saturating_add(offset), vertical),
                TileAxis::Vertical => (border.x.saturating_add(offset), border.y, horizontal),
            };
            grid.set_cell(cell_x, cell_y, &symbol.to_string(), Style::Border);
        }
    }
    if projection.status_rows > 0
        && let Some(view) = state.active_view
    {
        render_status_line(&mut grid, state, view, ui, config, width, height - 1);
    }
    Frame { grid, cursor }
}
