mod id;

pub use id::BufferId;

pub use id::PaneId;

pub use id::TabId;

pub use id::TransactionId;

pub use id::ViewId;

pub const MAXIMUM_LAYOUT_DEPTH: usize = crate::UNIVERSAL_LAYOUT_DEPTH_CEILING;
pub const MAXIMUM_LAYOUT_PANES: usize = crate::UNIVERSAL_LAYOUT_WINDOW_CEILING;
mod model;
pub use model::Axis;
pub use model::Buffer;
pub use model::BufferOrigin;
pub use model::EditingPolicy;
pub use model::EditorMode;
pub use model::FocusDirection;
pub use model::InsertRepeat;
pub use model::LayoutNode;
pub use model::Pane;
pub use model::RegisterShape;
pub use model::RegisterStore;
pub use model::RegisterValue;
pub use model::ReplaceRecord;
pub use model::Selection;
pub use model::SelectionKind;
pub use model::Tab;
pub use model::View;
mod store;
pub use store::EditorState;
pub(crate) use store::EditorTopology;
pub(crate) use store::EditorVersion;
pub(crate) use store::EditorWindowVersion;
mod layout;

#[cfg(test)]
mod tests;
