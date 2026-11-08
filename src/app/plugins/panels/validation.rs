//! Plugin tree, cell and placement validation.

use crate::app::plugins::panels::MAXIMUM_PLUGIN_UI_CHILDREN;
use crate::app::plugins::panels::MAXIMUM_PLUGIN_UI_DEPTH;
use crate::app::plugins::panels::MAXIMUM_PLUGIN_UI_NODE_TEXT_BYTES;
use crate::app::plugins::panels::MAXIMUM_PLUGIN_UI_NODES;
use crate::app::plugins::panels::MAXIMUM_PLUGIN_UI_TEXT_BYTES;
use crate::app::plugins::panels::PluginPrivateState;
use crate::app::plugins::panels::PluginWindowInstance;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginPresenterKind;
use crate::plugins::PluginPrivatePatch;
use crate::plugins::PluginPrivateRect;
use crate::plugins::PluginPrivateSnapshot;
use crate::plugins::PluginPrivateStyle;
use crate::plugins::PluginRequestedPlacement;
use crate::plugins::PluginUiError;
use crate::plugins::PluginUiErrorCode;
use crate::plugins::PluginUiNodeKind;
use crate::plugins::PluginUiStyle;
use crate::plugins::PluginUiTree;
use crate::plugins::PluginWindowPlacementKind;
use crate::ui::PluginSurfaceRow;
use crate::windows::DockEdge;
use crate::windows::WindowError;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(in crate::app) fn instance_revisions(
    handle: u64,
    instance: &PluginWindowInstance,
) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        Some(handle),
        instance.tree.as_ref().map(|tree| tree.revision),
        instance
            .private
            .as_ref()
            .and_then(|private| private.snapshot.as_ref())
            .map(|snapshot| snapshot.revision),
    )
}

pub(in crate::app) fn ui_error(
    code: PluginUiErrorCode,
    detail: impl std::fmt::Display,
) -> PluginUiError {
    PluginUiError::new(code, detail)
}

pub(in crate::app) fn map_graph_error(error: WindowError) -> PluginUiError {
    let code = match error {
        WindowError::InvalidPlacement | WindowError::Unsupported("placement") => {
            PluginUiErrorCode::InvalidPlacement
        }
        WindowError::NotFocusable | WindowError::Unsupported("focus") => {
            PluginUiErrorCode::HiddenFocus
        }
        WindowError::ViewStillHosted
        | WindowError::ContentStillViewed
        | WindowError::Unsupported("close-view") => PluginUiErrorCode::CloseOrder,
        WindowError::TooManyTiledWindows
        | WindowError::TooManyNonTiledWindows
        | WindowError::TooManyOverlayWindows
        | WindowError::MultipleFullscreenWindows
        | WindowError::TiledLayoutTooDeep
        | WindowError::RevisionExhausted
        | WindowError::IdentifierExhausted => PluginUiErrorCode::GraphLimit,
        WindowError::WindowOutsideActiveTab | WindowError::FocusOutsideActiveTab => {
            PluginUiErrorCode::CrossTab
        }
        _ => PluginUiErrorCode::InternalRejected,
    };
    ui_error(code, error)
}

pub(in crate::app) fn require_active_window(
    graph: &WindowGraph,
    window: WindowId,
) -> Result<(), PluginUiError> {
    let active = graph
        .active_tab()
        .ok_or_else(|| ui_error(PluginUiErrorCode::CrossTab, "no active workspace tab"))?;
    if graph
        .window(window)
        .is_some_and(|window| window.tab == active)
    {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::CrossTab,
            "plugin window operation crosses the active workspace tab",
        ))
    }
}

pub(in crate::app) fn require_graph_revision(
    graph: &WindowGraph,
    expected: u64,
) -> Result<(), PluginUiError> {
    if graph.revision() == Some(expected) {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::StaleGraph,
            "window graph revision is stale or exhausted",
        ))
    }
}

pub(in crate::app) fn placement_kind(
    placement: PluginRequestedPlacement,
) -> PluginWindowPlacementKind {
    match placement {
        PluginRequestedPlacement::DockedLeft { .. }
        | PluginRequestedPlacement::DockedRight { .. }
        | PluginRequestedPlacement::DockedTop { .. }
        | PluginRequestedPlacement::DockedBottom { .. } => PluginWindowPlacementKind::Docked,
        PluginRequestedPlacement::Floating { .. } => PluginWindowPlacementKind::Floating,
        PluginRequestedPlacement::Fullscreen => PluginWindowPlacementKind::Fullscreen,
        PluginRequestedPlacement::Hidden => PluginWindowPlacementKind::Hidden,
    }
}

pub(in crate::app) fn graph_placement(
    placement: PluginRequestedPlacement,
) -> Result<WindowPlacement, PluginUiError> {
    let placement = match placement {
        PluginRequestedPlacement::DockedLeft { size } => WindowPlacement::Docked {
            edge: DockEdge::Left,
            size,
        },
        PluginRequestedPlacement::DockedRight { size } => WindowPlacement::Docked {
            edge: DockEdge::Right,
            size,
        },
        PluginRequestedPlacement::DockedTop { size } => WindowPlacement::Docked {
            edge: DockEdge::Top,
            size,
        },
        PluginRequestedPlacement::DockedBottom { size } => WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size,
        },
        PluginRequestedPlacement::Floating {
            x,
            y,
            width,
            height,
        } => WindowPlacement::Floating(WindowRect {
            x,
            y,
            width,
            height,
        }),
        PluginRequestedPlacement::Fullscreen => WindowPlacement::Fullscreen,
        PluginRequestedPlacement::Hidden => WindowPlacement::Hidden,
    };
    placement
        .validate()
        .map_err(|error| ui_error(PluginUiErrorCode::InvalidPlacement, error))
}

pub(in crate::app) fn private_declaration<'a>(
    index: &'a PluginContributionIndex,
    instance: &PluginWindowInstance,
) -> Result<&'a crate::plugins::PluginContentTypeContribution, PluginUiError> {
    index
        .content_types
        .iter()
        .find(|declaration| {
            declaration.id == instance.content_type
                && declaration.presenter == PluginPresenterKind::PrivateCells
                && declaration.private_cells.is_some()
        })
        .ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::WrongPresenter,
                "content was not declared as private cells",
            )
        })
}

pub(in crate::app) fn validate_private_action(
    index: &PluginContributionIndex,
    action: &str,
) -> Result<(), PluginUiError> {
    let namespace = format!("plugin.{}.", index.owner);
    if action
        .strip_prefix(&namespace)
        .is_some_and(|command| index.commands.contains_key(command))
    {
        Ok(())
    } else {
        Err(ui_error(
            PluginUiErrorCode::UndeclaredAction,
            "private hit action was not statically declared",
        ))
    }
}

pub(in crate::app) fn rect_contains(
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    px: u16,
    py: u16,
) -> bool {
    width > 0
        && height > 0
        && px >= x
        && py >= y
        && px < x.saturating_add(width)
        && py < y.saturating_add(height)
}

pub(in crate::app) fn rects_overlap(left: PluginPrivateRect, right: PluginPrivateRect) -> bool {
    left.x < right.x.saturating_add(right.width)
        && right.x < left.x.saturating_add(left.width)
        && left.y < right.y.saturating_add(right.height)
        && right.y < left.y.saturating_add(left.height)
}

pub(in crate::app) fn rect_in_bounds(rect: PluginPrivateRect, width: u16, height: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect
            .x
            .checked_add(rect.width)
            .is_some_and(|right| right <= width)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|bottom| bottom <= height)
}

pub(in crate::app) fn validate_private_snapshot(
    declaration: &crate::plugins::PluginContentTypeContribution,
    private: &PluginPrivateState,
    snapshot: &PluginPrivateSnapshot,
    previous_revision: Option<u64>,
    ambiguous_wide: bool,
) -> Result<(), PluginUiError> {
    let cell_width = |text: &str| {
        if ambiguous_wide {
            UnicodeWidthStr::width_cjk(text)
        } else {
            UnicodeWidthStr::width(text)
        }
    };
    let policy = declaration.private_cells.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::WrongPresenter,
            "private policy is missing",
        )
    })?;
    let (width, height) = private.dimensions.ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private surface has no writable dimensions",
        )
    })?;
    if snapshot.dimensions_revision != private.dimensions_revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private surface dimensions revision is stale",
        ));
    }
    if snapshot.revision == 0
        || previous_revision.is_some_and(|revision| snapshot.revision <= revision)
    {
        return Err(ui_error(
            PluginUiErrorCode::StaleSurface,
            "private surface revision is stale",
        ));
    }
    if snapshot.cells.len() > usize::from(policy.maximum_cells)
        || snapshot.styles.len() > usize::from(policy.maximum_styles)
        || snapshot.hit_regions.len() > usize::from(policy.maximum_hit_regions)
    {
        return Err(ui_error(
            PluginUiErrorCode::PatchLimit,
            "private snapshot exceeds its declaration",
        ));
    }
    let mut bytes = 0_usize;
    let mut styles = BTreeSet::new();
    for style in &snapshot.styles {
        if style.id == 0 || !styles.insert(style.id) {
            return Err(ui_error(
                PluginUiErrorCode::InvalidStyle,
                "private styles require unique nonzero IDs",
            ));
        }
        bytes = bytes.checked_add(8).ok_or_else(|| {
            ui_error(PluginUiErrorCode::PatchLimit, "private byte count overflow")
        })?;
    }
    let mut cells = BTreeMap::new();
    for cell in &snapshot.cells {
        bytes = bytes.checked_add(cell.grapheme.len()).ok_or_else(|| {
            ui_error(PluginUiErrorCode::PatchLimit, "private byte count overflow")
        })?;
        if cell.x >= width || cell.y >= height {
            return Err(ui_error(
                PluginUiErrorCode::InvalidLocalBounds,
                "private cell leaves local bounds",
            ));
        }
        if !styles.contains(&cell.style) {
            return Err(ui_error(
                PluginUiErrorCode::InvalidStyle,
                "private cell references an unknown style",
            ));
        }
        if cells.insert((cell.x, cell.y), cell).is_some() {
            return Err(ui_error(
                PluginUiErrorCode::CellOverlap,
                "private cell coordinate is duplicated",
            ));
        }
        if cell.continuation {
            if !cell.grapheme.is_empty() {
                return Err(ui_error(
                    PluginUiErrorCode::SplitWideGrapheme,
                    "wide continuation must have empty text",
                ));
            }
        } else {
            let mut graphemes = cell.grapheme.graphemes(true);
            let Some(grapheme) = graphemes.next() else {
                return Err(ui_error(
                    PluginUiErrorCode::InvalidGrapheme,
                    "private lead cell is empty",
                ));
            };
            let width = cell_width(grapheme);
            if graphemes.next().is_some()
                || grapheme.chars().any(char::is_control)
                || !(1..=2).contains(&width)
            {
                return Err(ui_error(
                    PluginUiErrorCode::InvalidGrapheme,
                    "private lead cell must contain one printable grapheme",
                ));
            }
        }
    }
    for ((x, y), cell) in &cells {
        if cell.continuation {
            let lead = x
                .checked_sub(1)
                .and_then(|x| cells.get(&(x, *y)))
                .filter(|lead| !lead.continuation && cell_width(lead.grapheme.as_str()) == 2);
            if lead.is_none() {
                return Err(ui_error(
                    PluginUiErrorCode::SplitWideGrapheme,
                    "private continuation has no width-two lead",
                ));
            }
        } else if cell_width(cell.grapheme.as_str()) == 2
            && !cells
                .get(&(x.saturating_add(1), *y))
                .is_some_and(|continuation| continuation.continuation)
        {
            return Err(ui_error(
                PluginUiErrorCode::SplitWideGrapheme,
                "width-two private lead has no continuation",
            ));
        }
    }
    if let Some(cursor) = snapshot.cursor
        && (!policy.cursor || cursor.x >= width || cursor.y >= height)
    {
        return Err(ui_error(
            PluginUiErrorCode::InvalidCursor,
            "private cursor is disallowed or outside local bounds",
        ));
    }
    let mut hit_rectangles = Vec::new();
    for hit in &snapshot.hit_regions {
        let rect = PluginPrivateRect {
            x: hit.x,
            y: hit.y,
            width: hit.width,
            height: hit.height,
        };
        bytes = bytes.checked_add(hit.action.len()).ok_or_else(|| {
            ui_error(PluginUiErrorCode::PatchLimit, "private byte count overflow")
        })?;
        if !rect_in_bounds(rect, width, height)
            || hit.action != declaration.command
            || hit_rectangles
                .iter()
                .any(|existing| rects_overlap(*existing, rect))
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidHitRegion,
                "private hit region is outside bounds, overlaps, or is undeclared",
            ));
        }
        hit_rectangles.push(rect);
    }
    if bytes > usize::try_from(policy.maximum_bytes).unwrap_or(usize::MAX) {
        return Err(ui_error(
            PluginUiErrorCode::PatchLimit,
            "private snapshot byte limit exceeded",
        ));
    }
    Ok(())
}

pub(in crate::app) fn apply_private_patch(
    declaration: &crate::plugins::PluginContentTypeContribution,
    private: &PluginPrivateState,
    patch: &PluginPrivatePatch,
    ambiguous_wide: bool,
) -> Result<PluginPrivateSnapshot, PluginUiError> {
    let policy = declaration.private_cells.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::WrongPresenter,
            "private policy is missing",
        )
    })?;
    let current = private.snapshot.as_ref().ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleSurface,
            "private surface has no base snapshot",
        )
    })?;
    if patch.expected_surface_revision != current.revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleSurface,
            "private patch base revision is stale",
        ));
    }
    if patch.dimensions_revision != private.dimensions_revision {
        return Err(ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private patch dimensions revision is stale",
        ));
    }
    if patch.cells.len() > usize::from(policy.maximum_patch_cells)
        || patch.styles.len() > usize::from(policy.maximum_styles)
        || patch.hit_regions.len() > usize::from(policy.maximum_hit_regions)
        || patch.dirty.is_empty()
        || patch.dirty.len() > usize::from(policy.maximum_dirty_rectangles)
    {
        return Err(ui_error(
            PluginUiErrorCode::PatchLimit,
            "private patch exceeds its declaration",
        ));
    }
    let (width, height) = private.dimensions.ok_or_else(|| {
        ui_error(
            PluginUiErrorCode::StaleDimensions,
            "private dimensions are missing",
        )
    })?;
    for (index, rect) in patch.dirty.iter().enumerate() {
        if !rect_in_bounds(*rect, width, height)
            || patch.dirty[..index]
                .iter()
                .any(|previous| rects_overlap(*previous, *rect))
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidLocalBounds,
                "private dirty rectangles overlap or leave local bounds",
            ));
        }
    }
    if patch.cells.iter().any(|cell| {
        !patch
            .dirty
            .iter()
            .any(|rect| rect_contains(rect.x, rect.y, rect.width, rect.height, cell.x, cell.y))
    }) {
        return Err(ui_error(
            PluginUiErrorCode::InvalidLocalBounds,
            "private patch cell is outside every dirty rectangle",
        ));
    }
    let mut candidate = current.clone();
    candidate.revision = patch.revision;
    candidate.dimensions_revision = patch.dimensions_revision;
    candidate.cells.retain(|cell| {
        !patch
            .dirty
            .iter()
            .any(|rect| rect_contains(rect.x, rect.y, rect.width, rect.height, cell.x, cell.y))
    });
    candidate.cells.extend(patch.cells.clone());
    for style in &patch.styles {
        candidate.styles.retain(|existing| existing.id != style.id);
        candidate.styles.push(style.clone());
    }
    if patch.replace_cursor {
        candidate.cursor = patch.cursor;
    }
    if patch.replace_hit_regions {
        candidate.hit_regions = patch.hit_regions.clone();
    }
    validate_private_snapshot(
        declaration,
        private,
        &candidate,
        Some(current.revision),
        ambiguous_wide,
    )?;
    Ok(candidate)
}

pub(in crate::app) fn private_style(style: &PluginPrivateStyle) -> crate::ui::Style {
    match style.style {
        PluginUiStyle::Default => crate::ui::Style::Default,
        PluginUiStyle::Muted => crate::ui::Style::Muted,
        PluginUiStyle::Accent | PluginUiStyle::Warning => crate::ui::Style::Accent,
        PluginUiStyle::Error => crate::ui::Style::Error,
        PluginUiStyle::Selected => crate::ui::Style::Selection,
        PluginUiStyle::Border => crate::ui::Style::Border,
    }
}

pub(in crate::app) fn validate_tree(
    index: &PluginContributionIndex,
    tree: &PluginUiTree,
    previous_revision: Option<u64>,
) -> Result<(), PluginUiError> {
    if tree.revision == 0
        || previous_revision.is_some_and(|revision| tree.revision <= revision)
        || tree.nodes.is_empty()
        || tree.nodes.len() > MAXIMUM_PLUGIN_UI_NODES
    {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "invalid retained UI revision or node count",
        ));
    }
    let mut nodes = BTreeMap::new();
    let mut text_bytes = 0_usize;
    let mut child_count = 0_usize;
    let namespace = format!("plugin.{}.", index.owner);
    for node in &tree.nodes {
        text_bytes = text_bytes.checked_add(node.text.len()).ok_or_else(|| {
            ui_error(
                PluginUiErrorCode::InvalidTree,
                "retained UI byte count overflow",
            )
        })?;
        child_count = child_count
            .checked_add(node.children.len())
            .ok_or_else(|| {
                ui_error(
                    PluginUiErrorCode::InvalidTree,
                    "retained UI child count overflow",
                )
            })?;
        let action_valid = node.action.as_ref().is_none_or(|action| {
            action
                .strip_prefix(&namespace)
                .is_some_and(|command| index.commands.contains_key(command))
                && node.kind == PluginUiNodeKind::Button
        });
        if !action_valid {
            return Err(ui_error(
                PluginUiErrorCode::UndeclaredAction,
                "retained UI action is undeclared or not attached to a button",
            ));
        }
        if node.id == 0
            || node.text.len() > MAXIMUM_PLUGIN_UI_NODE_TEXT_BYTES
            || node.text.chars().any(char::is_control)
            || nodes.insert(node.id, node).is_some()
        {
            return Err(ui_error(
                PluginUiErrorCode::InvalidTree,
                "invalid retained UI node",
            ));
        }
    }
    if text_bytes > MAXIMUM_PLUGIN_UI_TEXT_BYTES || child_count > MAXIMUM_PLUGIN_UI_CHILDREN {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "retained UI aggregate limit exceeded",
        ));
    }
    let mut visited = BTreeSet::new();
    let mut stack = BTreeSet::new();
    fn visit(
        id: u64,
        depth: usize,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        visited: &mut BTreeSet<u64>,
        stack: &mut BTreeSet<u64>,
    ) -> Result<(), String> {
        if depth > MAXIMUM_PLUGIN_UI_DEPTH || !stack.insert(id) {
            return Err("retained UI depth or cycle violation".into());
        }
        let node = nodes
            .get(&id)
            .ok_or_else(|| "retained UI child is missing".to_owned())?;
        if !visited.insert(id) {
            return Err("retained UI node has multiple parents".into());
        }
        for child in &node.children {
            visit(*child, depth + 1, nodes, visited, stack)?;
        }
        stack.remove(&id);
        Ok(())
    }
    visit(tree.root, 1, &nodes, &mut visited, &mut stack)
        .map_err(|error| ui_error(PluginUiErrorCode::InvalidTree, error))?;
    if visited.len() != nodes.len() {
        return Err(ui_error(
            PluginUiErrorCode::InvalidTree,
            "retained UI contains unreachable nodes",
        ));
    }
    Ok(())
}

pub(in crate::app) fn focusable_nodes(tree: &PluginUiTree) -> Vec<u64> {
    let nodes = tree
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::new();
    pub(in crate::app) fn collect(
        id: u64,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        ordered: &mut Vec<u64>,
        visited: &mut BTreeSet<u64>,
    ) {
        if !visited.insert(id) || visited.len() > MAXIMUM_PLUGIN_UI_NODES {
            return;
        }
        let Some(node) = nodes.get(&id) else { return };
        if !node.text.is_empty()
            && (matches!(node.kind, PluginUiNodeKind::List | PluginUiNodeKind::Button)
                || node.action.is_some())
        {
            ordered.push(id);
        }
        for child in &node.children {
            collect(*child, nodes, ordered, visited);
        }
    }
    collect(tree.root, &nodes, &mut ordered, &mut BTreeSet::new());
    ordered
}

pub(in crate::app) fn first_focusable_node(tree: &PluginUiTree) -> Option<u64> {
    focusable_nodes(tree).into_iter().next()
}

pub(in crate::app) fn is_focusable_node(tree: &PluginUiTree, id: u64) -> bool {
    focusable_nodes(tree).contains(&id)
}

pub(in crate::app) fn ensure_focused_visible(instance: &mut PluginWindowInstance) {
    let Some(tree) = instance.tree.as_ref() else {
        return;
    };
    let rows = tree_rows(tree, instance.focused_node);
    if let Some(index) = instance
        .focused_node
        .and_then(|node| rows.iter().position(|row| row.node == node))
    {
        instance.scroll = instance.scroll.min(index);
        if instance.visible_rows > 0
            && index >= instance.scroll.saturating_add(instance.visible_rows)
        {
            instance.scroll = index + 1 - instance.visible_rows;
        }
    }
}

pub(in crate::app) fn tree_rows(
    tree: &PluginUiTree,
    focused_node: Option<u64>,
) -> Vec<PluginSurfaceRow> {
    let nodes = tree
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    pub(in crate::app) fn collect(
        id: u64,
        depth: usize,
        nodes: &BTreeMap<u64, &crate::plugins::PluginUiNode>,
        focused_node: Option<u64>,
        rows: &mut Vec<PluginSurfaceRow>,
        visited: &mut BTreeSet<u64>,
    ) {
        if !visited.insert(id) || visited.len() > MAXIMUM_PLUGIN_UI_NODES {
            return;
        }
        let Some(node) = nodes.get(&id) else { return };
        if !node.text.is_empty() {
            rows.push(PluginSurfaceRow {
                node: id,
                depth: u16::try_from(depth).unwrap_or(u16::MAX),
                text: node.text.clone(),
                style: match node.style {
                    PluginUiStyle::Default => crate::ui::Style::Default,
                    PluginUiStyle::Muted => crate::ui::Style::Muted,
                    PluginUiStyle::Accent => crate::ui::Style::Accent,
                    PluginUiStyle::Warning => crate::ui::Style::Accent,
                    PluginUiStyle::Error => crate::ui::Style::Error,
                    PluginUiStyle::Selected => crate::ui::Style::Selection,
                    PluginUiStyle::Border => crate::ui::Style::Border,
                },
                actionable: node.action.is_some(),
                focused: focused_node == Some(id),
            });
        }
        for child in &node.children {
            collect(*child, depth + 1, nodes, focused_node, rows, visited);
        }
    }
    collect(
        tree.root,
        0,
        &nodes,
        focused_node,
        &mut rows,
        &mut BTreeSet::new(),
    );
    rows
}
