use std::collections::BTreeMap;
use std::collections::BTreeSet;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::extensions::PluginContributionIndex;

use crate::extensions::PluginPresenterKind;

use crate::extensions::PluginPrivatePatch;

use crate::extensions::PluginPrivateRect;

use crate::extensions::PluginPrivateSnapshot;

use crate::extensions::PluginPrivateStyle;

use crate::extensions::PluginRequestedPlacement;

use crate::extensions::PluginSurfaceGeometry;

use crate::extensions::PluginUiError;

use crate::extensions::PluginUiErrorCode;

use crate::extensions::PluginUiEvent;

use crate::extensions::PluginUiNodeKind;

use crate::extensions::PluginUiRequest;

use crate::extensions::PluginUiRequestKind;

use crate::extensions::PluginUiStyle;

use crate::extensions::PluginUiTree;

use crate::extensions::PluginWindowPlacementKind;

use crate::ui::window::ContentAdapterKind;

use crate::ui::window::ContentCapabilities;

use crate::ui::window::ContentContract;

use crate::ui::window::ContentId;

use crate::ui::window::ContentTypeId;

use crate::ui::window::DockEdge;

use crate::ui::window::ViewId;

use crate::ui::window::WindowError;

use crate::ui::window::WindowGraph;

use crate::ui::window::WindowId;

use crate::ui::window::WindowPlacement;

use crate::ui::window::WindowRect;

use crate::ui::PluginSurfaceRow;

use crate::ui::PluginSurfaceUiState;

use crate::ui::PrivateSurfaceCell;

use crate::ui::PrivateSurfaceUiState;

mod host;
#[cfg(test)]
mod target03;
mod validation;
use validation::apply_private_patch;
use validation::ensure_focused_visible;
use validation::first_focusable_node;
use validation::focusable_nodes;
use validation::graph_placement;
use validation::instance_revisions;
use validation::is_focusable_node;
use validation::map_graph_error;
use validation::placement_kind;
use validation::private_declaration;
use validation::private_style;
use validation::rect_contains;
use validation::require_active_window;
use validation::require_graph_revision;
use validation::tree_rows;
use validation::ui_error;
use validation::validate_private_action;
use validation::validate_private_snapshot;
use validation::validate_tree;

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
    visible_rows: usize,
}

#[cfg_attr(not(test), derive(Clone))]
#[derive(Debug, Default, Eq, PartialEq)]
pub(in crate::app) struct PluginWindowHost {
    next_handle: u64,
    instances: BTreeMap<u64, PluginWindowInstance>,
    configured_placements: BTreeMap<String, PluginRequestedPlacement>,
    ambiguous_wide: bool,
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
            ambiguous_wide: self.ambiguous_wide,
        }
    }
}
#[cfg(test)]
impl PluginWindowHost {
    pub(in crate::app) fn test_clone_count() -> usize {
        HOST_CLONES.with(std::cell::Cell::get)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum PluginUiInteraction {
    Focus(isize),
    Scroll(isize),
    Activate,
}
#[cfg(test)]
pub(in crate::app) mod tests;
