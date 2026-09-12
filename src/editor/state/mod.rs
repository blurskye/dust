mod id;

pub use id::{BufferId, PaneId, TabId, TransactionId, ViewId};

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::editor::text::{BufferRevision, ByteOffset, TextBuffer};
use slotmap::{Key, SlotMap};

use crate::editor::{CommandInvocation, UndoTree};

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
use layout::*;

#[cfg(test)]
mod tests;
