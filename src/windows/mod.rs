//! Universal content, view, window, and workspace-tab ownership.
//!
//! W0 defines the validated graph. W1 reconciles editor resources into its
//! bounded tiled topology. W2 projects tiled, docked, floating, fullscreen,
//! overlay, and hidden windows together; the bundled explorer and PTYs are
//! ordinary graph resources, while editor layout mutation remains transitional.

pub(crate) const PANE_DEFAULT_RATIO: u16 = 500;
pub(crate) const PANE_RESIZE_STEP: u16 = 50;
pub(crate) const PANE_MINIMUM_WIDTH: u16 = 8;
pub(crate) const PANE_MINIMUM_HEIGHT: u16 = 3;

const MAXIMUM_CONTENT_TYPE_ID_BYTES: usize = 128;
const MAXIMUM_OWNER_BYTES: usize = 128;
const MAXIMUM_TITLE_BYTES: usize = 4 * 1024;
const MAXIMUM_TILED_DEPTH: usize = crate::UNIVERSAL_LAYOUT_DEPTH_CEILING;
const MAXIMUM_TILED_WINDOWS: usize = crate::UNIVERSAL_LAYOUT_WINDOW_CEILING;
const MAXIMUM_NON_TILED_WINDOWS_PER_TAB: usize = 128;
const MAXIMUM_OVERLAY_WINDOWS_PER_TAB: usize = 32;
const MAXIMUM_PROTECTED_REGIONS: usize = 16;
mod identity;
pub use identity::{ContentId, ContentTypeId, ViewId, WindowId, WindowTarget, WorkspaceTabId};
mod layout;
pub use layout::{
    CompositorLayer, CompositorProjection, CompositorSnapshot, CompositorWindowSnapshot,
    ContentAdapterKind, DockEdge, ProjectedBorder, ProjectedCompositorWindow, ProjectedWindow,
    SpatialDirection, TileAxis, TiledInsets, TiledLayoutNode, TiledLayoutProjection,
    TiledLayoutSnapshot, WindowPlacement, WindowRect,
};
mod model;
pub use model::{
    ContentCapabilities, ContentContract, ContentDescriptor, ContentLifecycle, ContentSnapshot,
    ViewDescriptor, WindowDescriptor, WorkspaceTabDescriptor,
};
mod graph;
pub use graph::WindowGraph;
mod error;
mod projection;
pub use error::WindowError;
pub(crate) use error::validate_title;

#[cfg(test)]
mod tests;
