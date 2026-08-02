use crate::document::{Affinity, BufferRevision, ByteOffset, EditError, TextEditSet};
use thiserror::Error;

use crate::editing::{
    BufferId, EditorMode, EditorState, RegisterShape, RegisterValue, Selection, TransactionId,
    ViewId,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UndoPolicy {
    #[default]
    Record,
    Merge,
    Boundary,
    Ignore,
}

#[derive(Clone, Debug)]
pub enum Precondition {
    BufferRevision {
        buffer: BufferId,
        revision: BufferRevision,
    },
    BufferWritable(BufferId),
    ViewExists(ViewId),
    ViewDisplays {
        view: ViewId,
        buffer: BufferId,
    },
}

#[derive(Clone, Debug)]
pub enum EditorOperation {
    ApplyEdits {
        buffer: BufferId,
        edits: TextEditSet,
    },
    SetCursor {
        view: ViewId,
        position: ByteOffset,
    },
    SetScroll {
        view: ViewId,
        first_line: usize,
    },
    SetScrollWrapRow {
        view: ViewId,
        first_wrap_row: usize,
    },
    SetScrollColumn {
        view: ViewId,
        first_column: usize,
    },
    SetSelection {
        view: ViewId,
        selection: Option<Selection>,
    },
    SetMode {
        view: ViewId,
        mode: EditorMode,
    },
    PushReplaceRecord {
        view: ViewId,
        record: crate::editing::ReplaceRecord,
    },
    PopReplaceRecord {
        view: ViewId,
    },
    SetBlockInsert {
        view: ViewId,
        positions: Vec<ByteOffset>,
    },
    SetRegister {
        name: char,
        text: String,
        shape: RegisterShape,
    },
    Undo {
        buffer: BufferId,
    },
    Redo {
        buffer: BufferId,
    },
    SplitPane {
        axis: crate::editing::Axis,
        ratio: u16,
    },
    NewTab,
    ClosePane,
    ResizePane {
        delta: i16,
    },
    SwapPanes {
        first: crate::editing::PaneId,
        second: crate::editing::PaneId,
    },
    CloseTab,
    FocusPane {
        offset: isize,
        wrap: bool,
    },
    FocusPaneId {
        pane: crate::editing::PaneId,
    },
    FocusTab {
        offset: isize,
        wrap: bool,
    },
}

#[derive(Clone, Debug)]
pub struct EditorTransaction {
    pub id: TransactionId,
    pub preconditions: Vec<Precondition>,
    pub operations: Vec<EditorOperation>,
    pub undo_policy: UndoPolicy,
    pub label: Option<String>,
}

impl EditorTransaction {
    #[must_use]
    pub fn new(id: TransactionId) -> Self {
        Self {
            id,
            preconditions: Vec::new(),
            operations: Vec::new(),
            undo_policy: UndoPolicy::Record,
            label: None,
        }
    }

    #[must_use]
    pub fn with_precondition(mut self, precondition: Precondition) -> Self {
        self.preconditions.push(precondition);
        self
    }

    #[must_use]
    pub fn with_operation(mut self, operation: EditorOperation) -> Self {
        self.operations.push(operation);
        self
    }

    #[must_use]
    pub fn with_undo_policy(mut self, policy: UndoPolicy) -> Self {
        self.undo_policy = policy;
        self
    }
}

#[derive(Debug, Error)]
pub enum TransactionError {
    #[error("buffer does not exist")]
    MissingBuffer,
    #[error("view does not exist")]
    MissingView,
    #[error("transaction precondition failed: {0}")]
    PreconditionFailed(String),
    #[error("buffer is read-only")]
    ReadOnly,
    #[error("cursor position {position} is invalid for buffer: {reason}")]
    InvalidCursor { position: usize, reason: String },
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("text position error: {0}")]
    Position(String),
    #[error("layout operation failed: {0}")]
    Layout(String),
}

impl EditorState {
    /// Apply against a clone and commit only after every operation succeeds.
    pub fn apply_transaction(
        &mut self,
        transaction: &EditorTransaction,
    ) -> Result<(), TransactionError> {
        validate_preconditions(self, &transaction.preconditions)?;
        let mut candidate = self.clone();
        for operation in &transaction.operations {
            apply_operation(&mut candidate, operation, transaction.undo_policy)?;
        }
        if transaction.undo_policy == UndoPolicy::Boundary {
            for (_, buffer) in &mut candidate.buffers {
                buffer.undo.boundary();
            }
        }
        if transaction
            .operations
            .iter()
            .any(|operation| operation.changed_buffer_dirtiness(self, &candidate))
        {
            candidate.mark_window_changed();
        }
        *self = candidate;
        Ok(())
    }
}

impl EditorOperation {
    fn changed_buffer_dirtiness(&self, before: &EditorState, after: &EditorState) -> bool {
        let buffer = match self {
            Self::ApplyEdits { buffer, .. } | Self::Undo { buffer } | Self::Redo { buffer } => {
                *buffer
            }
            _ => return false,
        };
        before.buffers.get(buffer).map(|buffer| buffer.dirty)
            != after.buffers.get(buffer).map(|buffer| buffer.dirty)
    }
}

fn validate_preconditions(
    state: &EditorState,
    preconditions: &[Precondition],
) -> Result<(), TransactionError> {
    for precondition in preconditions {
        match *precondition {
            Precondition::BufferRevision { buffer, revision } => {
                let actual = state
                    .buffers
                    .get(buffer)
                    .ok_or(TransactionError::MissingBuffer)?
                    .text
                    .revision();
                if actual != revision {
                    return Err(TransactionError::PreconditionFailed(format!(
                        "expected revision {revision:?}, found {actual:?}"
                    )));
                }
            }
            Precondition::BufferWritable(buffer) => {
                if state
                    .buffers
                    .get(buffer)
                    .ok_or(TransactionError::MissingBuffer)?
                    .read_only
                {
                    return Err(TransactionError::ReadOnly);
                }
            }
            Precondition::ViewExists(view) => {
                if !state.views.contains_key(view) {
                    return Err(TransactionError::MissingView);
                }
            }
            Precondition::ViewDisplays { view, buffer } => {
                let actual = state
                    .views
                    .get(view)
                    .ok_or(TransactionError::MissingView)?
                    .buffer;
                if actual != buffer {
                    return Err(TransactionError::PreconditionFailed(
                        "view no longer displays the target buffer".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn apply_operation(
    state: &mut EditorState,
    operation: &EditorOperation,
    undo_policy: UndoPolicy,
) -> Result<(), TransactionError> {
    match operation {
        EditorOperation::ApplyEdits { buffer, edits } => {
            let target = state
                .buffers
                .get_mut(*buffer)
                .ok_or(TransactionError::MissingBuffer)?;
            if target.read_only {
                return Err(TransactionError::ReadOnly);
            }
            let delta = target.text.apply_edits(edits)?;
            target.refresh_dirty();
            if matches!(undo_policy, UndoPolicy::Record | UndoPolicy::Merge) {
                target
                    .undo
                    .record(target.text.to_string(), undo_policy == UndoPolicy::Merge);
            }
            for (_, view) in state
                .views
                .iter_mut()
                .filter(|(_, view)| view.buffer == *buffer)
            {
                view.cursor = delta.map_offset(view.cursor, Affinity::After);
                if let Some(selection) = &mut view.selection {
                    selection.anchor = delta.map_offset(selection.anchor, Affinity::Before);
                    selection.head = delta.map_offset(selection.head, Affinity::After);
                }
                for position in &mut view.block_insert {
                    *position = delta.map_offset(*position, Affinity::After);
                }
            }
        }
        EditorOperation::SetCursor { view, position } => {
            let buffer_id = state
                .views
                .get(*view)
                .ok_or(TransactionError::MissingView)?
                .buffer;
            state
                .buffers
                .get(buffer_id)
                .ok_or(TransactionError::MissingBuffer)?
                .text
                .validate_offset(*position)
                .map_err(|error| TransactionError::InvalidCursor {
                    position: position.get(),
                    reason: error.to_string(),
                })?;
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.cursor = *position;
            if let Some(selection) = &mut target.selection {
                selection.head = *position;
            }
        }
        EditorOperation::SetScroll { view, first_line } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.scroll_line = *first_line;
            target.scroll_wrap_row = 0;
        }
        EditorOperation::SetScrollWrapRow {
            view,
            first_wrap_row,
        } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.scroll_wrap_row = *first_wrap_row;
        }
        EditorOperation::SetScrollColumn { view, first_column } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.scroll_column = *first_column;
        }
        EditorOperation::SetSelection { view, selection } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.selection = *selection;
        }
        EditorOperation::SetMode { view, mode } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.mode = *mode;
            match mode {
                EditorMode::VisualChar if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editing::SelectionKind::Character,
                    });
                }
                EditorMode::VisualLine if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editing::SelectionKind::Line,
                    });
                }
                EditorMode::VisualBlock if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editing::SelectionKind::Block,
                    });
                }
                EditorMode::Normal | EditorMode::Insert | EditorMode::Replace => {
                    target.selection = None;
                }
                _ => {}
            }
            if *mode != EditorMode::Replace {
                target.replace_history.clear();
            }
            if *mode != EditorMode::Insert {
                target.block_insert.clear();
            }
        }
        EditorOperation::PushReplaceRecord { view, record } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .replace_history
                .push(record.clone());
        }
        EditorOperation::PopReplaceRecord { view } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .replace_history
                .pop();
        }
        EditorOperation::SetBlockInsert { view, positions } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .block_insert = positions.clone();
        }
        EditorOperation::SetRegister { name, text, shape } => state.registers.set(
            *name,
            RegisterValue {
                text: text.clone(),
                shape: *shape,
            },
            state.policy.registers_enabled,
            state.policy.mirror_named_to_unnamed,
        ),
        EditorOperation::Undo { buffer } => {
            let target = state
                .buffers
                .get_mut(*buffer)
                .ok_or(TransactionError::MissingBuffer)?;
            if target.read_only {
                return Err(TransactionError::ReadOnly);
            }
            let text = target
                .undo
                .undo()
                .map(str::to_owned)
                .ok_or(TransactionError::NothingToUndo)?;
            target
                .text
                .restore_text(&text)
                .map_err(|error| TransactionError::Position(error.to_string()))?;
            target.refresh_dirty();
            clamp_views(state, *buffer);
        }
        EditorOperation::Redo { buffer } => {
            let target = state
                .buffers
                .get_mut(*buffer)
                .ok_or(TransactionError::MissingBuffer)?;
            if target.read_only {
                return Err(TransactionError::ReadOnly);
            }
            let text = target
                .undo
                .redo()
                .map(str::to_owned)
                .ok_or(TransactionError::NothingToRedo)?;
            target
                .text
                .restore_text(&text)
                .map_err(|error| TransactionError::Position(error.to_string()))?;
            target.refresh_dirty();
            clamp_views(state, *buffer);
        }
        EditorOperation::SplitPane { axis, ratio } => {
            state
                .split_active(*axis, *ratio)
                .map_err(|error| TransactionError::Layout(error.into()))?;
        }
        EditorOperation::NewTab => {
            state
                .new_tab()
                .map_err(|error| TransactionError::Layout(error.into()))?;
        }
        EditorOperation::ClosePane => state
            .close_active_pane()
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::ResizePane { delta } => state
            .resize_active_pane(*delta)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::SwapPanes { first, second } => state
            .swap_panes(*first, *second)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::CloseTab => state
            .close_active_tab()
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusPane { offset, wrap } => state
            .focus_pane(*offset, *wrap)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusPaneId { pane } => state
            .focus_pane_id(*pane)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusTab { offset, wrap } => state
            .focus_tab(*offset, *wrap)
            .map_err(|error| TransactionError::Layout(error.into()))?,
    }
    Ok(())
}

fn clamp_views(state: &mut EditorState, buffer: BufferId) {
    let len = state.buffers[buffer].text.len_bytes();
    for (_, view) in state
        .views
        .iter_mut()
        .filter(|(_, view)| view.buffer == buffer)
    {
        if view.cursor.get() > len {
            view.cursor = ByteOffset(len);
        }
        view.selection = None;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::document::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};

    use crate::editing::{
        EditorOperation, EditorState, EditorTransaction, Precondition, TransactionId,
    };

    #[test]
    fn failed_late_operation_rolls_back_earlier_edit() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None);
        let transaction = EditorTransaction::new(TransactionId(1))
            .with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(1)).expect("range"),
                        "X",
                    )],
                ),
            })
            .with_operation(EditorOperation::SetCursor {
                view: crate::editing::ViewId::default(),
                position: ByteOffset(0),
            });
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }

    #[test]
    fn failed_late_operation_rolls_back_scroll_column() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (_, view) = state.open_text(TextBuffer::from_text("abc"), None);
        let transaction = EditorTransaction::new(TransactionId(2))
            .with_operation(EditorOperation::SetScrollColumn {
                view,
                first_column: 19,
            })
            .with_operation(EditorOperation::SetCursor {
                view,
                position: ByteOffset(99),
            });
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.views[view].scroll_column, 0);
    }

    #[test]
    fn virtual_origin_rejects_edits_undo_redo_and_refreshes_only_by_host_revision() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) =
            state.open_virtual_read_only(TextBuffer::from_text("héllo"), "demo".into(), 7, 11, 1);
        let before = state.buffers[buffer].text.to_string();
        let edit =
            EditorTransaction::new(TransactionId(4)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                        "x",
                    )],
                ),
            });
        assert!(matches!(
            state.apply_transaction(&edit),
            Err(super::TransactionError::ReadOnly)
        ));
        for operation in [
            EditorOperation::Undo { buffer },
            EditorOperation::Redo { buffer },
        ] {
            assert!(matches!(
                state.apply_transaction(
                    &EditorTransaction::new(TransactionId(5)).with_operation(operation)
                ),
                Err(super::TransactionError::ReadOnly)
            ));
        }
        assert_eq!(state.buffers[buffer].text.to_string(), before);
        assert!(!state.buffers[buffer].dirty);
        assert!(!state.replace_virtual_read_only(buffer, 0, 2, TextBuffer::from_text("stale")));
        assert!(state.replace_virtual_read_only(
            buffer,
            1,
            2,
            TextBuffer::from_text("héllo refreshed")
        ));
        assert_eq!(state.views[view].buffer, buffer);
        assert_eq!(state.buffers[buffer].text.to_string(), "héllo refreshed");
        assert!(state.buffers[buffer].is_virtual_read_only());
    }

    #[test]
    fn stale_precondition_changes_nothing() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None);
        let transaction = EditorTransaction::new(TransactionId(3)).with_precondition(
            Precondition::BufferRevision {
                buffer,
                revision: crate::document::BufferRevision(9),
            },
        );
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }
}
