//! Editor buffer, view, pane and tab identifiers.

use slotmap::new_key_type;

new_key_type! {
    pub struct BufferId;
    pub struct ViewId;
    pub struct PaneId;
    pub struct TabId;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TransactionId(pub u64);
