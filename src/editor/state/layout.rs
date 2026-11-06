use crate::editor::state::id::PaneId;

pub(super) fn layout_ratios_are_valid(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane(_) => true,
        LayoutNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            (100..=900).contains(ratio)
                && layout_ratios_are_valid(first)
                && layout_ratios_are_valid(second)
        }
    }
}

pub(super) fn layout_depth(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane(_) => 1,
        LayoutNode::Split { first, second, .. } => {
            1usize.saturating_add(layout_depth(first).max(layout_depth(second)))
        }
    }
}

pub(super) fn pane_depth(node: &LayoutNode, target: PaneId, depth: usize) -> Option<usize> {
    match node {
        LayoutNode::Pane(pane) => (*pane == target).then_some(depth),
        LayoutNode::Split { first, second, .. } => {
            pane_depth(first, target, depth + 1).or_else(|| pane_depth(second, target, depth + 1))
        }
    }
}

pub(super) fn pane_ids(node: &LayoutNode) -> Vec<PaneId> {
    match node {
        LayoutNode::Pane(pane) => vec![*pane],
        LayoutNode::Split { first, second, .. } => {
            let mut panes = pane_ids(first);
            panes.extend(pane_ids(second));
            panes
        }
    }
}

pub(super) fn swap_layout_leaves(node: &mut LayoutNode, first: PaneId, second: PaneId) -> bool {
    let mut found_first = false;
    let mut found_second = false;
    fn visit(
        node: &mut LayoutNode,
        first: PaneId,
        second: PaneId,
        found_first: &mut bool,
        found_second: &mut bool,
    ) {
        match node {
            LayoutNode::Pane(pane) if *pane == first => {
                *pane = second;
                *found_first = true;
            }
            LayoutNode::Pane(pane) if *pane == second => {
                *pane = first;
                *found_second = true;
            }
            LayoutNode::Pane(_) => {}
            LayoutNode::Split {
                first: left,
                second: right,
                ..
            } => {
                visit(left, first, second, found_first, found_second);
                visit(right, first, second, found_first, found_second);
            }
        }
    }
    visit(node, first, second, &mut found_first, &mut found_second);
    found_first && found_second
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FocusRectangle {
    pub(super) x: u64,
    pub(super) y: u64,
    pub(super) width: u64,
    pub(super) height: u64,
}

pub(super) fn collect_focus_rectangles(
    node: &LayoutNode,
    rectangle: FocusRectangle,
    output: &mut Vec<(PaneId, FocusRectangle)>,
) {
    match node {
        LayoutNode::Pane(pane) => output.push((*pane, rectangle)),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            Axis::Horizontal => {
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
            Axis::Vertical => {
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
    direction: FocusDirection,
) -> Option<(u8, u64, u64)> {
    let source_right = source.x.saturating_add(source.width);
    let source_bottom = source.y.saturating_add(source.height);
    let candidate_right = candidate.x.saturating_add(candidate.width);
    let candidate_bottom = candidate.y.saturating_add(candidate.height);
    let (primary_gap, source_perpendicular, candidate_perpendicular) = match direction {
        FocusDirection::Left if candidate_right <= source.x => (
            source.x.saturating_sub(candidate_right),
            (source.y, source_bottom),
            (candidate.y, candidate_bottom),
        ),
        FocusDirection::Right if candidate.x >= source_right => (
            candidate.x.saturating_sub(source_right),
            (source.y, source_bottom),
            (candidate.y, candidate_bottom),
        ),
        FocusDirection::Up if candidate_bottom <= source.y => (
            source.y.saturating_sub(candidate_bottom),
            (source.x, source_right),
            (candidate.x, candidate_right),
        ),
        FocusDirection::Down if candidate.y >= source_bottom => (
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

pub(super) fn split_leaf(
    node: &mut LayoutNode,
    target: PaneId,
    axis: Axis,
    ratio: u16,
    new_pane: PaneId,
) -> bool {
    match node {
        LayoutNode::Pane(pane) if *pane == target => {
            *node = LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(LayoutNode::Pane(target)),
                second: Box::new(LayoutNode::Pane(new_pane)),
            };
            true
        }
        LayoutNode::Pane(_) => false,
        LayoutNode::Split { first, second, .. } => {
            split_leaf(first, target, axis, ratio, new_pane)
                || split_leaf(second, target, axis, ratio, new_pane)
        }
    }
}

pub(super) fn contains_pane(node: &LayoutNode, target: PaneId) -> bool {
    match node {
        LayoutNode::Pane(pane) => *pane == target,
        LayoutNode::Split { first, second, .. } => {
            contains_pane(first, target) || contains_pane(second, target)
        }
    }
}

/// The split with `first` on its first side and `second` on its second: the one a border
/// between them belongs to.
pub(super) fn split_between(
    node: &mut LayoutNode,
    first_pane: PaneId,
    second_pane: PaneId,
) -> Option<&mut LayoutNode> {
    let LayoutNode::Split { first, second, .. } = node else {
        return None;
    };
    if contains_pane(first, first_pane) && contains_pane(second, second_pane) {
        return Some(node);
    }
    let LayoutNode::Split { first, second, .. } = node else {
        return None;
    };
    if contains_pane(first, first_pane) {
        split_between(first, first_pane, second_pane)
    } else {
        split_between(second, first_pane, second_pane)
    }
}

pub(super) fn resize_parent(node: &mut LayoutNode, target: PaneId, delta: i16) -> bool {
    let LayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    if resize_parent(first, target, delta) || resize_parent(second, target, delta) {
        return true;
    }
    let signed = if contains_pane(first, target) {
        i32::from(delta)
    } else if contains_pane(second, target) {
        -i32::from(delta)
    } else {
        return false;
    };
    let adjusted = i32::from(*ratio) + signed;
    *ratio = u16::try_from(adjusted.clamp(100, 900)).unwrap_or(*ratio);
    true
}

pub(super) fn resize_parent_on_axis(
    node: &mut LayoutNode,
    target: PaneId,
    requested_axis: Axis,
    delta: i16,
) -> bool {
    let LayoutNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return false;
    };
    let in_first = contains_pane(first, target);
    let in_second = contains_pane(second, target);
    if !in_first && !in_second {
        return false;
    }
    let child = if in_first { first } else { second };
    if resize_parent_on_axis(child, target, requested_axis, delta) {
        return true;
    }
    if *axis != requested_axis {
        return false;
    }
    let signed = if in_first {
        i32::from(delta)
    } else {
        -i32::from(delta)
    };
    let adjusted = i32::from(*ratio) + signed;
    *ratio = u16::try_from(adjusted.clamp(100, 900)).unwrap_or(*ratio);
    true
}

pub(super) fn remove_leaf(node: LayoutNode, target: PaneId) -> Option<LayoutNode> {
    match node {
        LayoutNode::Pane(pane) => (pane != target).then_some(LayoutNode::Pane(pane)),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (remove_leaf(*first, target), remove_leaf(*second, target)) {
            (Some(first), Some(second)) => Some(LayoutNode::Split {
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

pub(super) fn offset_index(current: usize, len: usize, offset: isize, wrap: bool) -> usize {
    let len = isize::try_from(len).unwrap_or(isize::MAX).max(1);
    let candidate = isize::try_from(current).unwrap_or(0).saturating_add(offset);
    let target = if wrap {
        candidate.rem_euclid(len)
    } else {
        candidate.clamp(0, len.saturating_sub(1))
    };
    usize::try_from(target).unwrap_or(0)
}

use crate::editor::state::id::ViewId;




#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusDirection {
    Left,
    Down,
    Up,
    Right,
}


#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}


#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub view: ViewId,
}


#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub root: LayoutNode,
    pub focused: PaneId,
}