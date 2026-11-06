//! Split tree operations and spatial navigation.

use crate::windows::{SpatialDirection, TileAxis, TiledLayoutNode, WindowId};

pub(super) fn layout_depth(node: &TiledLayoutNode) -> usize {
    match node {
        TiledLayoutNode::Window(_) => 1,
        TiledLayoutNode::Split { first, second, .. } => {
            1usize.saturating_add(layout_depth(first).max(layout_depth(second)))
        }
    }
}

pub(super) fn window_ids(node: &TiledLayoutNode) -> Vec<WindowId> {
    match node {
        TiledLayoutNode::Window(pane) => vec![*pane],
        TiledLayoutNode::Split { first, second, .. } => {
            let mut panes = window_ids(first);
            panes.extend(window_ids(second));
            panes
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FocusRectangle {
    pub(super) x: u64,
    pub(super) y: u64,
    pub(super) width: u64,
    pub(super) height: u64,
}

pub(super) fn collect_focus_rectangles(
    node: &TiledLayoutNode,
    rectangle: FocusRectangle,
    output: &mut Vec<(WindowId, FocusRectangle)>,
) {
    match node {
        TiledLayoutNode::Window(pane) => output.push((*pane, rectangle)),
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            TileAxis::Horizontal => {
                let first_width = rectangle.width.saturating_mul(u64::from(*ratio)) / 1000;
                collect_focus_rectangles(
                    first,
                    FocusRectangle {
                        width: first_width,
                        ..rectangle
                    },
                    output,
                );
                collect_focus_rectangles(
                    second,
                    FocusRectangle {
                        x: rectangle.x.saturating_add(first_width),
                        width: rectangle.width.saturating_sub(first_width),
                        ..rectangle
                    },
                    output,
                );
            }
            TileAxis::Vertical => {
                let first_height = rectangle.height.saturating_mul(u64::from(*ratio)) / 1000;
                collect_focus_rectangles(
                    first,
                    FocusRectangle {
                        height: first_height,
                        ..rectangle
                    },
                    output,
                );
                collect_focus_rectangles(
                    second,
                    FocusRectangle {
                        y: rectangle.y.saturating_add(first_height),
                        height: rectangle.height.saturating_sub(first_height),
                        ..rectangle
                    },
                    output,
                );
            }
        },
    }
}

pub(super) fn focus_score(
    source: FocusRectangle,
    candidate: FocusRectangle,
    direction: SpatialDirection,
) -> Option<(u8, u64, u64)> {
    let source_right = source.x.saturating_add(source.width);
    let source_bottom = source.y.saturating_add(source.height);
    let candidate_right = candidate.x.saturating_add(candidate.width);
    let candidate_bottom = candidate.y.saturating_add(candidate.height);
    let (primary_gap, source_perpendicular, candidate_perpendicular) = match direction {
        SpatialDirection::Left if candidate_right <= source.x => (
            source.x.saturating_sub(candidate_right),
            (source.y, source_bottom),
            (candidate.y, candidate_bottom),
        ),
        SpatialDirection::Right if candidate.x >= source_right => (
            candidate.x.saturating_sub(source_right),
            (source.y, source_bottom),
            (candidate.y, candidate_bottom),
        ),
        SpatialDirection::Up if candidate_bottom <= source.y => (
            source.y.saturating_sub(candidate_bottom),
            (source.x, source_right),
            (candidate.x, candidate_right),
        ),
        SpatialDirection::Down if candidate.y >= source_bottom => (
            candidate.y.saturating_sub(source_bottom),
            (source.x, source_right),
            (candidate.x, candidate_right),
        ),
        _ => return None,
    };
    let overlaps = source_perpendicular.0 < candidate_perpendicular.1
        && candidate_perpendicular.0 < source_perpendicular.1;
    let source_center = source_perpendicular
        .0
        .saturating_add(source_perpendicular.1);
    let candidate_center = candidate_perpendicular
        .0
        .saturating_add(candidate_perpendicular.1);
    Some((
        u8::from(!overlaps),
        primary_gap,
        source_center.abs_diff(candidate_center),
    ))
}

pub(super) fn contains_window(node: &TiledLayoutNode, target: WindowId) -> bool {
    match node {
        TiledLayoutNode::Window(pane) => *pane == target,
        TiledLayoutNode::Split { first, second, .. } => {
            contains_window(first, target) || contains_window(second, target)
        }
    }
}

/// The split with `first` on its first side and `second` on its second: the one a border
/// between them belongs to.
pub(super) fn split_between(
    node: &mut TiledLayoutNode,
    first_pane: WindowId,
    second_pane: WindowId,
) -> Option<&mut TiledLayoutNode> {
    let TiledLayoutNode::Split { first, second, .. } = node else {
        return None;
    };
    if contains_window(first, first_pane) && contains_window(second, second_pane) {
        return Some(node);
    }
    let TiledLayoutNode::Split { first, second, .. } = node else {
        return None;
    };
    if contains_window(first, first_pane) {
        split_between(first, first_pane, second_pane)
    } else {
        split_between(second, first_pane, second_pane)
    }
}

pub(super) fn resize_parent(
    node: &mut TiledLayoutNode,
    target: WindowId,
    requested_axis: Option<TileAxis>,
    delta: i16,
    minimum: u16,
    maximum: u16,
) -> bool {
    let TiledLayoutNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return false;
    };
    let in_first = contains_window(first, target);
    let in_second = contains_window(second, target);
    if !in_first && !in_second {
        return false;
    }
    let child = if in_first { first } else { second };
    if resize_parent(child, target, requested_axis, delta, minimum, maximum) {
        return true;
    }
    if requested_axis.is_some_and(|requested| *axis != requested) {
        return false;
    }
    let signed = if in_first {
        i32::from(delta)
    } else {
        -i32::from(delta)
    };
    let adjusted = i32::from(*ratio) + signed;
    *ratio =
        u16::try_from(adjusted.clamp(i32::from(minimum), i32::from(maximum))).unwrap_or(*ratio);
    true
}

pub(crate) fn offset_index(current: usize, len: usize, offset: isize, wrap: bool) -> usize {
    let len = isize::try_from(len).unwrap_or(isize::MAX).max(1);
    let candidate = isize::try_from(current).unwrap_or(0).saturating_add(offset);
    let target = if wrap {
        candidate.rem_euclid(len)
    } else {
        candidate.clamp(0, len.saturating_sub(1))
    };
    usize::try_from(target).unwrap_or(0)
}
