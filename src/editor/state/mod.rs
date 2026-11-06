//! Buffers, views, registers and editor state.

mod id;

pub use id::BufferId;

pub use id::TransactionId;

pub use id::ViewId;

mod buffer;
pub(crate) mod events;
pub use events::{BufferChange, EditorEvent};
mod registers;
mod view;
pub use buffer::Buffer;
pub use buffer::BufferOrigin;

pub use registers::RegisterShape;
pub use registers::RegisterStore;
pub use registers::RegisterValue;
pub use view::EditorMode;
pub use view::InsertRepeat;
pub use view::ReplaceRecord;
pub use view::Selection;
pub use view::SelectionKind;
pub use view::View;
mod store;
mod windows;
pub use store::EditorState;
pub(crate) use store::EditorTopology;
pub(crate) use store::EditorVersion;
pub(crate) use store::EditorWindowVersion;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingPolicy {
    pub tab_width: usize,
    pub indent_width: usize,
    pub expand_tabs: bool,
    pub undo_break_on_text_commit: bool,
    pub registers_enabled: bool,
    pub mirror_named_to_unnamed: bool,
    pub ambiguous_width_wide: bool,
    pub autopairs: bool,
}
impl Default for EditingPolicy {
    fn default() -> Self {
        Self {
            tab_width: 4,
            indent_width: 4,
            expand_tabs: true,
            undo_break_on_text_commit: false,
            registers_enabled: true,
            mirror_named_to_unnamed: true,
            ambiguous_width_wide: false,
            autopairs: false,
        }
    }
}
