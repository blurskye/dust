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
pub use identity::ContentId;
pub use identity::ContentTypeId;
pub use identity::ViewId;
pub use identity::WindowId;
pub use identity::WindowTarget;
pub use identity::WorkspaceTabId;
mod layout;
pub use layout::CompositorLayer;
pub use layout::CompositorProjection;
pub use layout::CompositorSnapshot;
pub use layout::CompositorWindowSnapshot;
pub use layout::ContentAdapterKind;
pub use layout::DockEdge;
pub use layout::ProjectedBorder;
pub use layout::ProjectedCompositorWindow;
pub use layout::ProjectedWindow;
pub use layout::SpatialDirection;
pub use layout::TileAxis;
pub use layout::TiledInsets;
pub use layout::TiledLayoutNode;
pub use layout::TiledLayoutProjection;
pub use layout::TiledLayoutSnapshot;
pub use layout::WindowPlacement;
pub use layout::WindowRect;
mod content;
pub use content::ContentCapabilities;
pub use content::ContentContract;
pub use content::ContentDescriptor;
pub use content::ContentLifecycle;
pub use content::ContentSnapshot;
pub use content::ViewDescriptor;
pub use content::WindowDescriptor;
pub use content::WorkspaceTabDescriptor;
mod graph;
pub use graph::WindowGraph;
mod error;
mod projection;
pub use error::WindowError;
pub(crate) use error::validate_title;

#[cfg(test)]
mod tests;
