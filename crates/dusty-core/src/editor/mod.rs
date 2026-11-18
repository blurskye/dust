//! Buffers, editing state and command execution.

#![forbid(unsafe_code)]

mod command;
pub mod decorations;
pub mod input;
mod state;
pub mod text;
mod transaction;

pub use command::ArgumentField;

pub use command::ArgumentSchema;

pub use command::ArgumentType;

pub use command::{CommandAccess, CommandAuthority, CommandWrapperAction, PendingCommandId};

pub use command::CommandArguments;

pub use command::CommandError;

pub use command::CommandId;

pub use command::CommandInvocation;

pub use command::CommandOutcome;

pub use command::CommandRegistry;

pub use command::CommandSource;

pub use command::CommandSpec;

pub use command::CommandTarget;

pub use command::CommandValue;

pub use command::ContextRequirement;

pub use command::InvocationSource;

pub use command::register_builtin_commands;
pub use state::Buffer;
pub use state::BufferId;
pub use state::BufferOrigin;
pub use state::EditingPolicy;
pub use state::EditorMode;
pub use state::EditorState;
pub use state::EditorTopology;
pub use state::EditorVersion;
pub use state::EditorWindowVersion;
pub use state::RegisterShape;
pub use state::RegisterStore;
pub use state::RegisterValue;
pub use state::ReplaceRecord;
pub use state::Selection;
pub use state::SelectionKind;
pub use state::TransactionId;
pub use state::View;
pub use state::ViewId;
pub use state::{BufferChange, EditorEvent};
pub use transaction::EditorDraft;
pub use transaction::EditorOperation;
pub use transaction::EditorTransaction;
pub use transaction::Precondition;
pub use transaction::TransactionError;
pub use transaction::UndoPolicy;
pub use transaction::UndoTree;
pub use transaction::save_undo_history;

mod objects;
pub use objects::{SyntaxObject, SyntaxObjectKind};

#[cfg(any(test, feature = "test-support"))]
pub use transaction::build_layout_for_test;
pub use transaction::{
    DocumentSnapshot, DraftEdit, RestoredLayout, RestoredPane, RestoredTab, WorkspaceCommitCheck,
    WorkspaceEditSources,
};

pub use transaction::BufferPathChange;

pub use transaction::EditorPublication;

pub use transaction::WorkspaceSnapshot;

pub use command::visual_ranges;

#[cfg(any(test, feature = "test-support"))]
pub use transaction::load_undo_history;
