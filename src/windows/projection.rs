//! Layout validation and projection into clipped window rectangles.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::windows::MAXIMUM_TILED_DEPTH;

use crate::windows::MAXIMUM_TILED_WINDOWS;

use crate::windows::error::WindowError;

use crate::windows::identity::WindowId;

use crate::windows::identity::WorkspaceTabId;

use crate::windows::layout::CompositorLayer;

use crate::windows::layout::CompositorProjection;

use crate::windows::layout::CompositorWindowSnapshot;

use crate::windows::layout::DockEdge;

use crate::windows::layout::ProjectedBorder;

use crate::windows::layout::ProjectedCompositorWindow;

use crate::windows::layout::ProjectedWindow;

use crate::windows::layout::SpatialDirection;

use crate::windows::layout::TileAxis;

use crate::windows::layout::TiledLayoutNode;

use crate::windows::layout::TiledLayoutProjection;

use crate::windows::layout::WindowPlacement;

use crate::windows::layout::WindowRect;

use crate::windows::content::WindowDescriptor;

pub(super) fn validate_tiled_layout(
    tab: WorkspaceTabId,
    root: Option<&TiledLayoutNode>,
    windows: &BTreeMap<WindowId, WindowDescriptor>,
) -> Result<(), WindowError> {
    fn collect(
        node: &TiledLayoutNode,
        tab: WorkspaceTabId,
        windows: &BTreeMap<WindowId, WindowDescriptor>,
        depth: usize,
        leaves: &mut BTreeSet<WindowId>,
    ) -> Result<(), WindowError> {
        if depth > MAXIMUM_TILED_DEPTH {
            return Err(WindowError::TiledLayoutTooDeep);
        }
        match node {
            TiledLayoutNode::Window(window) => {
                let descriptor = windows.get(window).ok_or(WindowError::StaleTiledWindow)?;
                if descriptor.tab != tab || !matches!(descriptor.placement, WindowPlacement::Tiled)
                {
                    return Err(WindowError::NonTiledWindowInLayout);
                }
                if !leaves.insert(*window) {
                    return Err(WindowError::DuplicateTiledWindow);
                }
                if leaves.len() > MAXIMUM_TILED_WINDOWS {
                    return Err(WindowError::TooManyTiledWindows);
                }
            }
            TiledLayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !(1..=999).contains(ratio) {
                    return Err(WindowError::InvalidPlacement);
                }
                collect(first, tab, windows, depth + 1, leaves)?;
                collect(second, tab, windows, depth + 1, leaves)?;
            }
        }
        Ok(())
    }

    let expected = windows
        .values()
        .filter_map(|window| {
            (window.tab == tab && matches!(window.placement, WindowPlacement::Tiled))
                .then_some(window.id)
        })
        .collect::<BTreeSet<_>>();
    if expected.len() > MAXIMUM_TILED_WINDOWS {
        return Err(WindowError::TooManyTiledWindows);
    }
    let mut leaves = BTreeSet::new();
    if let Some(root) = root {
        collect(root, tab, windows, 1, &mut leaves)?;
    }
    if leaves != expected {
        return Err(WindowError::OrphanTiledWindow);
    }
    Ok(())
}

pub(super) fn tiled_contains(node: &TiledLayoutNode, target: WindowId) -> bool {
    match node {
        TiledLayoutNode::Window(window) => *window == target,
        TiledLayoutNode::Split { first, second, .. } => {
            tiled_contains(first, target) || tiled_contains(second, target)
        }
    }
}

pub(super) fn append_tiled_window(
    root: Option<TiledLayoutNode>,
    window: WindowId,
) -> Option<TiledLayoutNode> {
    fn append(node: TiledLayoutNode, window: WindowId) -> TiledLayoutNode {
        match node {
            TiledLayoutNode::Window(_) => TiledLayoutNode::Split {
                axis: TileAxis::Horizontal,
                ratio: 500,
                first: Box::new(node),
                second: Box::new(TiledLayoutNode::Window(window)),
            },
            TiledLayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                if depth(&first) <= depth(&second) {
                    TiledLayoutNode::Split {
                        axis,
                        ratio,
                        first: Box::new(append(*first, window)),
                        second,
                    }
                } else {
                    TiledLayoutNode::Split {
                        axis,
                        ratio,
                        first,
                        second: Box::new(append(*second, window)),
                    }
                }
            }
        }
    }
    fn depth(node: &TiledLayoutNode) -> usize {
        match node {
            TiledLayoutNode::Window(_) => 1,
            TiledLayoutNode::Split { first, second, .. } => 1 + depth(first).max(depth(second)),
        }
    }
    Some(root.map_or(TiledLayoutNode::Window(window), |root| append(root, window)))
}

pub(super) fn remove_tiled_window(
    node: TiledLayoutNode,
    target: WindowId,
) -> Option<TiledLayoutNode> {
    match node {
        TiledLayoutNode::Window(window) => (window != target).then_some(node),
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            remove_tiled_window(*first, target),
            remove_tiled_window(*second, target),
        ) {
            (Some(first), Some(second)) => Some(TiledLayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(node), None) | (None, Some(node)) => Some(node),
            (None, None) => None,
        },
    }
}

pub(super) fn replace_tiled_window(node: &mut TiledLayoutNode, from: WindowId, to: WindowId) {
    match node {
        TiledLayoutNode::Window(window) if *window == from => *window = to,
        TiledLayoutNode::Window(_) => {}
        TiledLayoutNode::Split { first, second, .. } => {
            replace_tiled_window(first, from, to);
            replace_tiled_window(second, from, to);
        }
    }
}

pub(super) fn resize_tiled_parent(
    node: &mut TiledLayoutNode,
    target: WindowId,
    delta: i16,
) -> bool {
    let TiledLayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    if resize_tiled_parent(first, target, delta) || resize_tiled_parent(second, target, delta) {
        return true;
    }
    let signed = if tiled_contains(first, target) {
        i32::from(delta)
    } else if tiled_contains(second, target) {
        -i32::from(delta)
    } else {
        return false;
    };
    let adjusted = i32::from(*ratio) + signed;
    *ratio = u16::try_from(adjusted.clamp(1, 999)).unwrap_or(*ratio);
    true
}

pub(super) fn push_compositor_window(
    output: &mut CompositorProjection,
    window: CompositorWindowSnapshot,
    rect: WindowRect,
    layer: CompositorLayer,
    focused: Option<WindowId>,
) {
    let z_index = u16::try_from(output.windows.len()).unwrap_or(u16::MAX);
    output.windows.push(ProjectedCompositorWindow {
        window: window.window,
        rect,
        layer,
        z_index,
        focused: focused == Some(window.window),
        focusable: window.focusable,
    });
}

pub(super) fn spatial_score(
    source: WindowRect,
    candidate: WindowRect,
    direction: SpatialDirection,
) -> Option<(u32, u32)> {
    let source_left = u32::from(source.x);
    let source_top = u32::from(source.y);
    let source_right = source_left.saturating_add(u32::from(source.width));
    let source_bottom = source_top.saturating_add(u32::from(source.height));
    let candidate_left = u32::from(candidate.x);
    let candidate_top = u32::from(candidate.y);
    let candidate_right = candidate_left.saturating_add(u32::from(candidate.width));
    let candidate_bottom = candidate_top.saturating_add(u32::from(candidate.height));
    let source_center_x = source_left.saturating_add(u32::from(source.width) / 2);
    let source_center_y = source_top.saturating_add(u32::from(source.height) / 2);
    let candidate_center_x = candidate_left.saturating_add(u32::from(candidate.width) / 2);
    let candidate_center_y = candidate_top.saturating_add(u32::from(candidate.height) / 2);
    match direction {
        SpatialDirection::Left
            if (candidate_right <= source_left
                || (candidate_left <= source_left && candidate_center_x < source_center_x))
                && candidate_top < source_bottom
                && source_top < candidate_bottom =>
        {
            Some((
                source_left.saturating_sub(candidate_right).max(1),
                source_center_y.abs_diff(candidate_center_y),
            ))
        }
        SpatialDirection::Right
            if (candidate_left >= source_right
                || (candidate_right >= source_right && candidate_center_x > source_center_x))
                && candidate_top < source_bottom
                && source_top < candidate_bottom =>
        {
            Some((
                candidate_left.saturating_sub(source_right).max(1),
                source_center_y.abs_diff(candidate_center_y),
            ))
        }
        SpatialDirection::Up
            if (candidate_bottom <= source_top
                || (candidate_top <= source_top && candidate_center_y < source_center_y))
                && candidate_left < source_right
                && source_left < candidate_right =>
        {
            Some((
                source_top.saturating_sub(candidate_bottom).max(1),
                source_center_x.abs_diff(candidate_center_x),
            ))
        }
        SpatialDirection::Down
            if (candidate_top >= source_bottom
                || (candidate_bottom >= source_bottom && candidate_center_y > source_center_y))
                && candidate_left < source_right
                && source_left < candidate_right =>
        {
            Some((
                candidate_top.saturating_sub(source_bottom).max(1),
                source_center_x.abs_diff(candidate_center_x),
            ))
        }
        _ => None,
    }
}

pub(super) fn project_dock(
    bounds: WindowRect,
    edge: DockEdge,
    requested: u16,
    reserve_width: u16,
    reserve_height: u16,
) -> (WindowRect, WindowRect) {
    match edge {
        DockEdge::Left => {
            let size = requested.min(bounds.width.saturating_sub(reserve_width));
            (
                WindowRect {
                    width: size,
                    ..bounds
                },
                WindowRect {
                    x: bounds.x.saturating_add(size),
                    width: bounds.width.saturating_sub(size),
                    ..bounds
                },
            )
        }
        DockEdge::Right => {
            let size = requested.min(bounds.width.saturating_sub(reserve_width));
            let remaining = bounds.width.saturating_sub(size);
            (
                WindowRect {
                    x: bounds.x.saturating_add(remaining),
                    width: size,
                    ..bounds
                },
                WindowRect {
                    width: remaining,
                    ..bounds
                },
            )
        }
        DockEdge::Top => {
            let size = requested.min(bounds.height.saturating_sub(reserve_height));
            (
                WindowRect {
                    height: size,
                    ..bounds
                },
                WindowRect {
                    y: bounds.y.saturating_add(size),
                    height: bounds.height.saturating_sub(size),
                    ..bounds
                },
            )
        }
        DockEdge::Bottom => {
            let size = requested.min(bounds.height.saturating_sub(reserve_height));
            let remaining = bounds.height.saturating_sub(size);
            (
                WindowRect {
                    y: bounds.y.saturating_add(remaining),
                    height: size,
                    ..bounds
                },
                WindowRect {
                    height: remaining,
                    ..bounds
                },
            )
        }
    }
}

pub(super) fn project_float(requested: WindowRect, bounds: WindowRect) -> Option<WindowRect> {
    if bounds.width == 0 || bounds.height == 0 {
        return None;
    }
    let width = requested.width.min(bounds.width);
    let height = requested.height.min(bounds.height);
    Some(WindowRect {
        x: requested.x.clamp(
            bounds.x,
            bounds.x.saturating_add(bounds.width.saturating_sub(width)),
        ),
        y: requested.y.clamp(
            bounds.y,
            bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height)),
        ),
        width,
        height,
    })
}

pub(super) fn intersect_rect(requested: WindowRect, bounds: WindowRect) -> Option<WindowRect> {
    let left = requested.x.max(bounds.x);
    let top = requested.y.max(bounds.y);
    let right = requested
        .x
        .saturating_add(requested.width)
        .min(bounds.x.saturating_add(bounds.width));
    let bottom = requested
        .y
        .saturating_add(requested.height)
        .min(bounds.y.saturating_add(bounds.height));
    (right > left && bottom > top).then_some(WindowRect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    })
}

pub(super) fn rect_contains(rect: WindowRect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub(super) fn project_tiled_node(
    node: &TiledLayoutNode,
    bounds: WindowRect,
    output: &mut TiledLayoutProjection,
) {
    match node {
        TiledLayoutNode::Window(window) => output.windows.push(ProjectedWindow {
            window: *window,
            rect: bounds,
        }),
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            TileAxis::Horizontal => {
                let available = bounds.width.saturating_sub(1);
                // u32: a u16 product saturates on terminals wider than about 130 columns.
                let first_width = u16::try_from(u32::from(available) * u32::from(*ratio) / 1_000)
                    .unwrap_or(available)
                    .min(available);
                let second_width = available.saturating_sub(first_width);
                project_tiled_node(
                    first,
                    WindowRect {
                        width: first_width,
                        ..bounds
                    },
                    output,
                );
                let border_x = bounds.x.saturating_add(first_width);
                output.borders.push(ProjectedBorder {
                    x: border_x,
                    y: bounds.y,
                    length: bounds.height,
                    axis: *axis,
                });
                project_tiled_node(
                    second,
                    WindowRect {
                        x: border_x.saturating_add(1),
                        width: second_width,
                        ..bounds
                    },
                    output,
                );
            }
            TileAxis::Vertical => {
                let available = bounds.height.saturating_sub(1);
                // u32: a u16 product saturates on terminals taller than about 130 rows.
                let first_height = u16::try_from(u32::from(available) * u32::from(*ratio) / 1_000)
                    .unwrap_or(available)
                    .min(available);
                let second_height = available.saturating_sub(first_height);
                project_tiled_node(
                    first,
                    WindowRect {
                        height: first_height,
                        ..bounds
                    },
                    output,
                );
                let border_y = bounds.y.saturating_add(first_height);
                output.borders.push(ProjectedBorder {
                    x: bounds.x,
                    y: border_y,
                    length: bounds.width,
                    axis: *axis,
                });
                project_tiled_node(
                    second,
                    WindowRect {
                        y: border_y.saturating_add(1),
                        height: second_height,
                        ..bounds
                    },
                    output,
                );
            }
        },
    }
}
