#![forbid(unsafe_code)]

mod command;
pub mod input;
mod state;
pub mod text;
mod transaction;

pub use command::{
    ArgumentField, ArgumentSchema, ArgumentType, CommandArguments, CommandError, CommandId,
    CommandInvocation, CommandOutcome, CommandRegistry, CommandSource, CommandSpec, CommandTarget,
    CommandValue, ContextRequirement, InvocationSource, register_builtin_commands,
};
pub use state::{
    Axis, Buffer, BufferId, BufferOrigin, EditingPolicy, EditorMode, EditorState, FocusDirection,
    LayoutNode, MAXIMUM_LAYOUT_DEPTH, MAXIMUM_LAYOUT_PANES, Pane, PaneId, RegisterShape,
    RegisterStore, RegisterValue, ReplaceRecord, Selection, SelectionKind, Tab, TabId,
    TransactionId, View, ViewId,
};
pub(crate) use state::{EditorTopology, EditorVersion, EditorWindowVersion};
pub(crate) use transaction::{EditorDraft, save_undo_history};
pub use transaction::{
    EditorOperation, EditorTransaction, Precondition, TransactionError, UndoPolicy, UndoTree,
};

