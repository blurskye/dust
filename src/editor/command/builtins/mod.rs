use std::sync::atomic::{AtomicU64, Ordering};

use unicode_segmentation::UnicodeSegmentation;

use crate::editor::text::{
    ByteOffset, ByteRange, DisplayWidthPolicy, TextBuffer, TextEdit, TextEditSet,
    byte_range_for_display_columns, display_column_at, insertion_at_display_column,
    next_grapheme_boundary, previous_grapheme_boundary,
};

use crate::editor::{
    ArgumentSchema, ArgumentType, CommandError, CommandInvocation, CommandOutcome, CommandRegistry,
    CommandSpec, ContextRequirement, EditorMode, EditorOperation, EditorState, EditorTransaction,
    Precondition, RegisterShape, SelectionKind, TransactionId, UndoPolicy, View, ViewId,
};

static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);
mod registration;
pub use registration::register_builtin_commands;
mod execute;
use execute::*;
mod transaction;
use transaction::*;
mod motion;
use motion::*;

#[cfg(test)]
mod tests;
