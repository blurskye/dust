#![forbid(unsafe_code)]

mod command;
pub mod input;
mod state;
pub mod text;
mod transaction;

pub use command::ArgumentField;

pub use command::ArgumentSchema;

pub use command::ArgumentType;

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
pub use state::Axis;
pub use state::Buffer;
pub use state::BufferId;
pub use state::BufferOrigin;
pub use state::EditingPolicy;
pub use state::EditorMode;
pub use state::EditorState;
pub use state::FocusDirection;
pub use state::LayoutNode;
pub use state::MAXIMUM_LAYOUT_DEPTH;
pub use state::MAXIMUM_LAYOUT_PANES;
pub use state::Pane;
pub use state::PaneId;
pub use state::RegisterShape;
pub use state::RegisterStore;
pub use state::RegisterValue;
pub use state::ReplaceRecord;
pub use state::Selection;
pub use state::SelectionKind;
pub use state::Tab;
pub use state::TabId;
pub use state::TransactionId;
pub use state::View;
pub use state::ViewId;
pub(crate) use state::EditorTopology;
pub(crate) use state::EditorVersion;
pub(crate) use state::EditorWindowVersion;
pub(crate) use transaction::EditorDraft;
pub(crate) use transaction::save_undo_history;
pub use transaction::EditorOperation;
pub use transaction::EditorTransaction;
pub use transaction::Precondition;
pub use transaction::TransactionError;
pub use transaction::UndoPolicy;
pub use transaction::UndoTree;


mod objects;
pub use objects::{SyntaxObject, SyntaxObjectKind};

pub(crate) use transaction::{DraftEdit,DocumentSnapshot,WorkspaceEditSources,WorkspaceCommitCheck,RestoredPane,RestoredLayout,RestoredTab};
#[cfg(test)]
pub(crate) use transaction::build_layout_for_test;

pub(crate) use transaction::BufferPathChange;

pub(crate) use transaction::EditorPublication;

pub(crate) use transaction::WorkspaceSnapshot;

pub(crate) use command::visual_ranges;

pub(crate) use transaction::load_undo_history;
