//! Editor buffer, view, pane and tab identifiers.

use slotmap::new_key_type;

new_key_type! {
    pub struct BufferId;
    pub struct ViewId;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TransactionId(pub u64);
