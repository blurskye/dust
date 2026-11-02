use crate::ui::render::{CONTROL_CHARACTER_REPLACEMENT, SCRATCH_LABEL, ZERO_WIDTH_REPLACEMENT};
use crate::ui::render::editor::END_OF_BUFFER_MARKER;
use crate::{
    config::{AmbiguousWidth, Config},
    editor::EditorState,
    ui::{CellGrid, Style, window::TileAxis},
};

use super::{
    Frame, UiState,
    editor::render_editor_view,
    overlays::render_content_tab_line,
};

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
    panes: Vec<PaneRect>,
    borders: Vec<crate::ui::window::ProjectedBorder>,
}

fn global_editor_pane_viewports(state: &EditorState, ui: &UiState) -> Vec<PaneViewport> {
    let Some(source_revision) = state.window_revision() else {
        return Vec::new();
    };
    let Some(layout) = ui
        .editor_window_layout
        .as_ref()
        .filter(|layout| layout.source_revision == source_revision)
    else {
        return Vec::new();
    };
    let Some(compositor) = ui
        .compositor_layout
        .as_ref()
        .filter(|compositor| compositor.source_revision == source_revision)
    else {
        return Vec::new();
    };
    compositor
        .projection
        .windows
        .iter()
        .filter_map(|projected| {
            let binding = layout
                .bindings
                .iter()
                .find(|binding| binding.window == projected.window)?;
            Some(PaneViewport {
                pane: binding.pane,
                view: binding.view,
                x: projected.rect.x,
                y: projected.rect.y,
                width: projected.rect.width,
                height: projected.rect.height,
            })
        })
        .collect()
}

#[must_use]
pub fn editor_pane_viewport_at(
    state: &EditorState,
    ui: &UiState,
    column: u16,
    row: u16,
) -> Option<PaneViewport> {
    let layout = ui.editor_window_layout.as_ref()?;
    let projected = ui
        .compositor_layout
        .as_ref()?
        .projection
        .windows
        .iter()
        .rev()
        .find(|projected| {
            layout
                .bindings
                .iter()
                .any(|binding| binding.window == projected.window)
                && column >= projected.rect.x
                && column < projected.rect.x.saturating_add(projected.rect.width)
                && row >= projected.rect.y
                && row < projected.rect.y.saturating_add(projected.rect.height)
        })?;
    let binding = layout
        .bindings
        .iter()
        .find(|binding| binding.window == projected.window)?;
    global_editor_pane_viewports(state, ui)
        .into_iter()
        .find(|viewport| viewport.pane == binding.pane)
}

/// Where `view` is shown, if any pane shows it.
#[must_use]
pub fn view_pane_viewport(
    state: &EditorState,
    ui: &UiState,
    view: crate::editor::ViewId,
) -> Option<PaneViewport> {
    global_editor_pane_viewports(state, ui)
        .into_iter()
        .find(|viewport| viewport.view == view)
}

#[must_use]
pub fn active_editor_pane_viewport(state: &EditorState, ui: &UiState) -> Option<PaneViewport> {
    let tab = state.tabs.get(state.active_tab?)?;
    global_editor_pane_viewports(state, ui)
        .into_iter()
        .find(|viewport| viewport.pane == tab.focused)
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
    let tab_line_visible = state.tabs.len() > 1;
    let buffer_line_visible = config.ui.buffer_line.enabled
        && ui.content_tabs.len() > usize::from(config.ui.buffer_line.hide_when_single);
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

pub(super) fn render_non_tiled_editor_window(
    frame: &mut Frame,
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    projected: &crate::ui::window::ProjectedCompositorWindow,
) -> bool {
    if projected.layer == crate::ui::window::CompositorLayer::Tiled {
        return false;
    }
    let Some(tab) = state.active_tab.and_then(|tab| state.tabs.get(tab)) else {
        return false;
    };
    let Some(source_revision) = state.window_revision() else {
        return false;
    };
    let Some(binding) = ui
        .editor_window_layout
        .as_ref()
        .filter(|layout| layout.source_revision == source_revision)
        .and_then(|layout| {
            layout
                .bindings
                .iter()
                .find(|binding| binding.window == projected.window)
        })
    else {
        return false;
    };
    let rendered = render_editor_view(
        state,
        binding.view,
        ui,
        config,
        projected.rect.width,
        projected.rect.height,
    );
    for y in 0..projected.rect.height {
        for x in 0..projected.rect.width {
            let Some(cell) = rendered.grid.get(x, y) else {
                continue;
            };
            if !cell.continuation {
                frame.grid.set_cell(
                    projected.rect.x.saturating_add(x),
                    projected.rect.y.saturating_add(y),
                    &cell.symbol,
                    cell.style,
                );
            }
        }
    }
    if binding.pane == tab.focused && projected.focused {
        frame.cursor = rendered.cursor.map(|(x, y, shape)| {
            (
                projected.rect.x.saturating_add(x),
                projected.rect.y.saturating_add(y),
                shape,
            )
        });
    }
    true
}

pub(super) fn render_non_tiled_editor_windows(
    frame: &mut Frame,
    state: &EditorState,
    ui: &UiState,
    config: &Config,
) {
    let Some(compositor) = ui.compositor_layout.as_ref() else {
        return;
    };
    for projected in compositor
        .projection
        .windows
        .iter()
        .filter(|window| window.layer != crate::ui::window::CompositorLayer::Floating)
    {
        render_non_tiled_editor_window(frame, state, ui, config, projected);
    }
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
            &CONTROL_CHARACTER_REPLACEMENT,
            &ZERO_WIDTH_REPLACEMENT,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        );
        grid.put_text(0, 0, &END_OF_BUFFER_MARKER, Style::Muted);
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
    // The status line belongs to the screen, not to a pane.
    let mut pane_config = config.clone();
    pane_config.ui.status_line.enabled = false;
    if pane_count == 1
        && !projection.tab_line_visible
        && !projection.buffer_line_visible
        && ui.debug_panes.is_none()
    {
        let Some(view) = projection.panes.first().map(|pane| pane.view) else {
            return Frame {
                grid: CellGrid::new(width, height),
                cursor: None,
            };
        };
        return render_editor_view(state, view, ui, &pane_config, width, height);
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &CONTROL_CHARACTER_REPLACEMENT,
        &ZERO_WIDTH_REPLACEMENT,
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
                    || SCRATCH_LABEL.to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                );
            let label = if candidate == tab_id {
                format!("[{title}]")
            } else {
                format!(" {title} ")
            };
            x = grid.put_text(
                x,
                0,
                &label,
                if candidate == tab_id {
                    Style::StatusAccent
                } else {
                    Style::Status
                },
            );
            x = grid.put_text(x, 0, "│", Style::Status);
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
        if rect.pane == tab.focused {
            if let Some((x, y, shape)) = frame.cursor {
                cursor = Some((rect.x.saturating_add(x), rect.y.saturating_add(y), shape));
            }
        }
    }
    let (horizontal, vertical) = ('─', '│');
    for border in projection.borders {
        for offset in 0..border.length {
            let (cell_x, cell_y, symbol) = match border.axis {
                TileAxis::Horizontal => (border.x, border.y.saturating_add(offset), vertical),
                TileAxis::Vertical => (border.x.saturating_add(offset), border.y, horizontal),
            };
            grid.set_cell(cell_x, cell_y, &symbol.to_string(), Style::Border);
        }
    }
    Frame { grid, cursor }
}
