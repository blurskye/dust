mod id;

pub use id::{BufferId, PaneId, TabId, TransactionId, ViewId};

pub const MAXIMUM_LAYOUT_DEPTH: usize = crate::UNIVERSAL_LAYOUT_DEPTH_CEILING;
pub const MAXIMUM_LAYOUT_PANES: usize = crate::UNIVERSAL_LAYOUT_WINDOW_CEILING;
mod model;
pub use model::{
    Axis, Buffer, BufferOrigin, EditingPolicy, EditorMode, FocusDirection, LayoutNode, Pane,
    RegisterShape, RegisterStore, RegisterValue, ReplaceRecord, Selection, SelectionKind, Tab,
    View,
};
mod store;
pub use store::EditorState;
pub(crate) use store::{EditorTopology, EditorVersion, EditorWindowVersion};
mod layout;

#[cfg(test)]
mod tests;
