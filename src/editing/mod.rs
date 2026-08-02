#![forbid(unsafe_code)]

mod builtins;
mod command;
mod ids;
mod state;
mod transaction;
mod undo;

pub use builtins::register_builtin_commands;
pub use command::{
    ArgumentField, ArgumentSchema, ArgumentType, CommandArguments, CommandError, CommandId,
    CommandInvocation, CommandOutcome, CommandRegistry, CommandSource, CommandSpec, CommandTarget,
    CommandValue, ContextRequirement, InvocationSource,
};
pub use ids::{BufferId, PaneId, TabId, TransactionId, ViewId};
pub use state::{
    Axis, Buffer, BufferOrigin, EditingPolicy, EditorMode, EditorState, FocusDirection, LayoutNode,
    MAXIMUM_LAYOUT_DEPTH, MAXIMUM_LAYOUT_PANES, Pane, RegisterShape, RegisterStore, RegisterValue,
    ReplaceRecord, Selection, SelectionKind, Tab, View,
};
pub use transaction::{
    EditorOperation, EditorTransaction, Precondition, TransactionError, UndoPolicy,
};
pub use undo::UndoTree;
