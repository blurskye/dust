use std::collections::{BTreeMap, BTreeSet};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    extensions::{
        PluginContributionIndex, PluginPresenterKind, PluginPrivatePatch, PluginPrivateRect,
        PluginPrivateSnapshot, PluginPrivateStyle, PluginRequestedPlacement, PluginSurfaceGeometry,
        PluginUiError, PluginUiErrorCode, PluginUiEvent, PluginUiNodeKind, PluginUiRequest,
        PluginUiRequestKind, PluginUiStyle, PluginUiTree, PluginWindowPlacementKind,
    },
    ui::window::{
        ContentAdapterKind, ContentId, ContentTypeId, DockEdge, ViewId, WindowError, WindowGraph,
        WindowId, WindowPlacement, WindowRect,
    },
    ui::{PluginSurfaceRow, PluginSurfaceUiState, PrivateSurfaceCell, PrivateSurfaceUiState},
};

mod host;
mod validation;
use validation::{
    apply_private_patch, ensure_focused_visible, first_focusable_node, focusable_nodes,
    graph_placement, instance_revisions, is_focusable_node, map_graph_error, placement_kind,
    private_declaration, private_style, rect_contains, require_active_window,
    require_graph_revision, tree_rows, ui_error, validate_private_action,
    validate_private_snapshot, validate_tree,
};

const MAXIMUM_PLUGIN_UI_NODES: usize = 256;
const MAXIMUM_PLUGIN_UI_DEPTH: usize = 32;
const MAXIMUM_PLUGIN_UI_CHILDREN: usize = 1_024;
const MAXIMUM_PLUGIN_UI_TEXT_BYTES: usize = 64 * 1024;
const MAXIMUM_PLUGIN_UI_NODE_TEXT_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PluginPrivateState {
    dimensions: Option<(u16, u16)>,
    dimensions_revision: u64,
    snapshot: Option<PluginPrivateSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginWindowInstance {
    plugin: String,
    load: u64,
    checksum: String,
    content_type: String,
    content: ContentId,
    view: ViewId,
    window: WindowId,
    presenter: PluginPresenterKind,
    tree: Option<PluginUiTree>,
    private: Option<PluginPrivateState>,
    focused_node: Option<u64>,
    scroll: usize,
}

#[cfg_attr(not(test), derive(Clone))]
#[derive(Debug, Default, Eq, PartialEq)]
pub(in crate::editor::session) struct PluginWindowHost {
    next_handle: u64,
    instances: BTreeMap<u64, PluginWindowInstance>,
    configured_placements: BTreeMap<String, PluginRequestedPlacement>,
}

#[cfg(test)]
thread_local! { static HOST_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
impl Clone for PluginWindowHost {
    fn clone(&self) -> Self {
        HOST_CLONES.with(|count| count.set(count.get() + 1));
        Self {
            next_handle: self.next_handle,
            instances: self.instances.clone(),
            configured_placements: self.configured_placements.clone(),
        }
    }
}
#[cfg(test)]
impl PluginWindowHost {
    pub(in crate::editor::session) fn test_clone_count() -> usize {
        HOST_CLONES.with(std::cell::Cell::get)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum PluginUiInteraction {
    Focus(isize),
    Scroll(isize),
    Activate,
}
#[cfg(test)]
pub(in crate::editor::session) mod tests;
