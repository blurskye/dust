//! Workspace tabs, buffer lines and status drawing.

use unicode_segmentation::UnicodeSegmentation;

use crate::config::AmbiguousWidth;
use crate::config::Config;
use crate::editor::EditorState;
use crate::ui::CellGrid;
use crate::ui::Style;
use crate::ui::geometry::text_width;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::ui::render::SCRATCH_LABEL;
use crate::ui::render::editor::END_OF_BUFFER_MARKER;
use crate::windows::TileAxis;

use crate::ui::grid::Frame;

use crate::ui::render::UiState;

use crate::ui::render::editor::render_editor_view;

use crate::ui::render::overlays::render_content_tab_line;

use crate::ui::render::visual_rows::one_cell_symbol;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneViewport {
    pub pane: crate::windows::WindowId,
    pub view: crate::editor::ViewId,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy)]
pub(super) struct PaneRect {
    pane: crate::windows::WindowId,
    view: crate::editor::ViewId,
    focused: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

pub(super) struct WorkspaceProjection {
    tab_line_visible: bool,
    buffer_line_visible: bool,
    panes: Vec<PaneRect>,
    borders: Vec<crate::windows::ProjectedBorder>,
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
                pane: binding.window,
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
        .find(|viewport| viewport.pane == binding.window)
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
    let focused = state.focused_window()?;
    global_editor_pane_viewports(state, ui)
        .into_iter()
        .find(|viewport| viewport.pane == focused)
}

pub(super) fn workspace_projection(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<WorkspaceProjection> {
    state.active_tab()?;
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
    let tab_line_visible = state.editor_tabs().count() > 1;
    let buffer_line_visible = config.ui.buffer_line.enabled
        && ui.content_tabs.len() > usize::from(config.ui.buffer_line.hide_when_single);
    let origin = compositor.projection.workspace_bounds;
    let panes = compositor
        .projection
        .windows
        .iter()
        .filter(|projected| projected.layer == crate::windows::CompositorLayer::Tiled)
        .filter_map(|projected| {
            let binding = layout
                .bindings
                .iter()
                .find(|binding| binding.window == projected.window)?;
            Some(PaneRect {
                pane: binding.window,
                view: binding.view,
                focused: projected.focused,
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
        .map(|border| crate::windows::ProjectedBorder {
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
    let focused = state.focused_window()?;
    pane_viewports(state, ui, config, width, height)
        .into_iter()
        .find(|viewport| viewport.pane == focused)
}

pub(super) fn render_non_tiled_editor_window(
    frame: &mut Frame,
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    projected: &crate::windows::ProjectedCompositorWindow,
) -> bool {
    if projected.layer == crate::windows::CompositorLayer::Tiled {
        return false;
    }
    let Some(focused) = state.focused_window() else {
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
    let mut local_config = config.clone();
    local_config.ui.status_line.enabled = false;
    let rendered = render_editor_view(
        state,
        binding.view,
        ui,
        &local_config,
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
    if binding.window == focused && projected.focused {
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
        .filter(|window| window.layer != crate::windows::CompositorLayer::Floating)
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
    let Some(tab_id) = state.active_tab() else {
        let mut grid = CellGrid::new_with_policy(
            width,
            height,
            CONTROL_CHARACTER_REPLACEMENT,
            ZERO_WIDTH_REPLACEMENT,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        );
        grid.put_text(0, 0, END_OF_BUFFER_MARKER, Style::Muted);
        return Frame { grid, cursor: None };
    };
    let Some(focused) = state.tab_focused_window(tab_id) else {
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
        let Some(pane) = projection.panes.first() else {
            return Frame {
                grid: CellGrid::new(width, height),
                cursor: None,
            };
        };
        let mut frame = render_editor_view(state, pane.view, ui, &pane_config, width, height);
        if !pane.focused {
            frame.cursor = None;
        }
        return frame;
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if projection.tab_line_visible {
        grid.fill_row(0, Style::Status);
        let mut labels = Vec::new();
        let mut active = 0;
        for candidate_tab in state.editor_tabs() {
            let candidate = candidate_tab.id;
            let title = state
                .tab_focused_window(candidate)
                .and_then(|window| state.window_view(window))
                .and_then(|view| state.views.get(view))
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
            let style = if candidate == tab_id {
                active = labels.len();
                Style::StatusAccent
            } else {
                Style::Status
            };
            labels.push((label, style));
        }
        // Keep the active label in view, dropping earlier tabs when they do not fit.
        // Match CellGrid's replacement and per-grapheme width policy, including
        // filenames containing controls or standalone zero-width characters.
        let label_width = |label: &str| {
            label
                .graphemes(true)
                .map(|grapheme| {
                    let safe = if grapheme == "\t" {
                        " "
                    } else if grapheme.chars().any(char::is_control) {
                        CONTROL_CHARACTER_REPLACEMENT
                    } else if text_width(grapheme, config.editor.ambiguous_width) == 0 {
                        ZERO_WIDTH_REPLACEMENT
                    } else {
                        grapheme
                    };
                    text_width(safe, config.editor.ambiguous_width).clamp(1, 2)
                })
                .sum::<usize>()
        };
        let mut start = active;
        let mut used = label_width(&labels[active].0);
        while start > 0 {
            let preceding = label_width(&labels[start - 1].0) + 1;
            if used.saturating_add(preceding) > usize::from(width) {
                break;
            }
            used += preceding;
            start -= 1;
        }
        let separator = one_cell_symbol("│", "|", config.editor.ambiguous_width);
        let mut x = 0;
        for (label, style) in &labels[start..] {
            x = grid.put_text(x, 0, label, *style);
            x = grid.put_text(x, 0, separator, Style::Status);
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
        if rect.pane == focused
            && rect.focused
            && let Some((x, y, shape)) = frame.cursor
        {
            cursor = Some((rect.x.saturating_add(x), rect.y.saturating_add(y), shape));
        }
    }
    render_split_borders(
        &mut grid,
        &projection.borders,
        config.editor.ambiguous_width,
    );
    Frame { grid, cursor }
}

fn render_split_borders(
    grid: &mut CellGrid,
    borders: &[crate::windows::ProjectedBorder],
    ambiguous: AmbiguousWidth,
) {
    let mut cells = std::collections::BTreeMap::new();
    const LEFT: u8 = 1;
    const RIGHT: u8 = 2;
    const UP: u8 = 4;
    const DOWN: u8 = 8;
    for border in borders {
        for offset in 0..border.length {
            let (x, y, directions) = match border.axis {
                TileAxis::Horizontal => (border.x, border.y.saturating_add(offset), UP | DOWN),
                TileAxis::Vertical => (border.x.saturating_add(offset), border.y, LEFT | RIGHT),
            };
            *cells.entry((x, y)).or_insert(0) |= directions;
        }
    }
    // Split segments end beside their parent's divider. Connect only projected borders,
    // never an editor's indent guide or a box drawn inside a pane.
    for (&(x, y), &directions) in &cells {
        let mut joined = directions;
        for (neighbor, incoming, outgoing) in [
            (x.checked_sub(1).map(|x| (x, y)), RIGHT, LEFT),
            (x.checked_add(1).map(|x| (x, y)), LEFT, RIGHT),
            (y.checked_sub(1).map(|y| (x, y)), DOWN, UP),
            (y.checked_add(1).map(|y| (x, y)), UP, DOWN),
        ] {
            if neighbor
                .and_then(|position| cells.get(&position))
                .is_some_and(|directions| directions & incoming != 0)
            {
                joined |= outgoing;
            }
        }
        let (symbol, fallback) = match (
            joined & LEFT != 0,
            joined & RIGHT != 0,
            joined & UP != 0,
            joined & DOWN != 0,
        ) {
            (true, true, false, false) => ("─", "-"),
            (false, false, true, true) => ("│", "|"),
            (true, true, true, false) => ("┴", "+"),
            (true, true, false, true) => ("┬", "+"),
            (true, false, true, true) => ("┤", "+"),
            (false, true, true, true) => ("├", "+"),
            _ => ("┼", "+"),
        };
        grid.set_cell(
            x,
            y,
            one_cell_symbol(symbol, fallback, ambiguous),
            Style::Border,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::ProjectedBorder;

    #[test]
    fn nested_split_borders_join_at_both_sides_and_crossings() {
        let vertical = ProjectedBorder {
            x: 5,
            y: 0,
            length: 9,
            axis: TileAxis::Horizontal,
        };
        let left = ProjectedBorder {
            x: 0,
            y: 4,
            length: 5,
            axis: TileAxis::Vertical,
        };
        let right = ProjectedBorder {
            x: 6,
            y: 4,
            length: 5,
            axis: TileAxis::Vertical,
        };
        let above = ProjectedBorder {
            x: 5,
            y: 0,
            length: 4,
            axis: TileAxis::Horizontal,
        };
        let below = ProjectedBorder {
            x: 5,
            y: 5,
            length: 4,
            axis: TileAxis::Horizontal,
        };
        let horizontal = ProjectedBorder {
            x: 0,
            y: 4,
            length: 11,
            axis: TileAxis::Vertical,
        };
        for (borders, expected) in [
            (vec![vertical, left], "┤"),
            (vec![vertical, right], "├"),
            (vec![vertical, left, right], "┼"),
            (vec![horizontal, above], "┴"),
            (vec![horizontal, below], "┬"),
            (vec![horizontal, above, below], "┼"),
        ] {
            for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
                let mut grid = CellGrid::new(11, 9);
                render_split_borders(&mut grid, &borders, ambiguous);
                assert_eq!(
                    grid.get(5, 4).unwrap().symbol,
                    one_cell_symbol(expected, "+", ambiguous)
                );
                assert_eq!(grid.get(5, 4).unwrap().style, Style::Border);
            }
        }
    }
}
