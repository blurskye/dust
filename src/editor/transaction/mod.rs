//! Checked edits and undo history.

use crate::editor::EditorTopology;
use crate::editor::EditorVersion;
use crate::editor::EditorWindowVersion;
use crate::editor::text::TextBuffer;
mod paths;
mod restore;
#[cfg(test)]
pub(crate) use restore::build_layout_for_test;
mod workspace;
pub(crate) use workspace::EditorPublication;
pub(crate) use workspace::WorkspaceCommitCheck;
pub(crate) use workspace::WorkspaceEditSources;
pub(crate) use workspace::WorkspaceSnapshot;
mod undo;
pub(crate) use paths::BufferPathChange;

pub use undo::UndoTree;
pub(crate) use undo::load_undo_history;
pub(crate) use undo::save_undo_history;

use crate::editor::text::Affinity;

use crate::editor::text::BufferRevision;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::EditError;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;
use thiserror::Error;

use crate::editor::BufferId;

use crate::editor::EditorMode;

use crate::editor::EditorState;

use crate::editor::RegisterShape;

use crate::editor::RegisterValue;

use crate::editor::Selection;

use crate::editor::TransactionId;

use crate::editor::ViewId;

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
    /// The column `j` and `k` aim for; any other cursor move clears it.
    SetPreferredColumn {
        view: ViewId,
        column: usize,
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
    SetInsertRepeat {
        view: ViewId,
        count: usize,
        new_line: bool,
    },
    PushReplaceRecord {
        view: ViewId,
        record: crate::editor::ReplaceRecord,
    },
    PopReplaceRecord {
        view: ViewId,
    },
    SetBlockInsert {
        view: ViewId,
        positions: Vec<ByteOffset>,
    },
    SetMultiMarks {
        view: ViewId,
        marks: Vec<crate::editor::text::ByteRange>,
        whole_word: bool,
        preferred_columns: Vec<usize>,
    },
    SetRegister {
        name: char,
        text: String,
        shape: RegisterShape,
    },
    /// Text that was just yanked, which the editor lights up for a moment as Vim's
    /// `vim.hl.on_yank` does. It changes nothing.
    Yanked {
        view: ViewId,
        ranges: Vec<ByteRange>,
    },
    Undo {
        buffer: BufferId,
    },
    Redo {
        buffer: BufferId,
    },
    SplitPane {
        axis: crate::windows::TileAxis,
        ratio: u16,
    },
    ClonePane {
        pane: crate::windows::WindowId,
        axis: crate::windows::TileAxis,
        ratio: u16,
    },
    NewTab,
    ToggleZoom,
    ClosePane,
    ResizePane {
        delta: i16,
    },
    ResizePaneOnAxis {
        axis: crate::windows::TileAxis,
        delta: i16,
    },
    SetSplitRatio {
        first: crate::windows::WindowId,
        second: crate::windows::WindowId,
        ratio: u16,
    },
    SwapPanes {
        first: crate::windows::WindowId,
        second: crate::windows::WindowId,
    },
    CloseTab,
    DiscardUnreferencedBuffers {
        buffers: Vec<BufferId>,
    },
    FocusPane {
        offset: isize,
        wrap: bool,
    },
    FocusPaneId {
        pane: crate::windows::WindowId,
    },
    FocusTab {
        offset: isize,
        wrap: bool,
    },
    FocusTabId {
        tab: crate::windows::WorkspaceTabId,
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
        let draft = self.prepare_transaction(transaction)?;
        *self = draft.state;
        Ok(())
    }
}

impl EditorOperation {
    fn edited_buffer(&self) -> Option<BufferId> {
        match self {
            Self::ApplyEdits { buffer, .. } | Self::Undo { buffer } | Self::Redo { buffer } => {
                Some(*buffer)
            }
            _ => None,
        }
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

fn view_positions(state: &EditorState) -> Vec<(ViewId, ByteOffset, EditorMode)> {
    state
        .views
        .iter()
        .map(|(id, view)| (id, view.cursor, view.mode))
        .collect()
}

fn record_view_changes(state: &mut EditorState, before: &[(ViewId, ByteOffset, EditorMode)]) {
    for (id, cursor, mode) in before {
        let Some(view) = state.views.get(*id) else {
            continue;
        };
        if view.cursor != *cursor {
            state.events.push(crate::editor::EditorEvent::CursorMoved {
                view: *id,
                position: view.cursor,
            });
        }
        if view.mode != *mode {
            state.events.push(crate::editor::EditorEvent::ModeChanged {
                view: *id,
                mode: view.mode,
            });
        }
    }
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
            // The undo step holds the replaced text, taken before the edit, with each start
            // shifted by the edits before it so the changes replay one after another.
            let mut changes = Vec::new();
            if undo_policy != UndoPolicy::Ignore {
                let mut shift = 0isize;
                for edit in target.text.validate_edits(edits)? {
                    let removed = target
                        .text
                        .slice(edit.range())
                        .map_err(|error| TransactionError::Position(error.to_string()))?;
                    let start = edit.range().start.get().saturating_add_signed(shift);
                    shift += edit.replacement().len() as isize - removed.len() as isize;
                    changes.push(undo::Change {
                        start,
                        removed,
                        inserted: edit.replacement().to_owned(),
                        removed_endings: target.text.line_endings(edit.range()),
                        inserted_endings: vec![
                            target.text.format().preferred_line_ending;
                            edit.replacement()
                                .bytes()
                                .filter(|b| *b == b'\n')
                                .count()
                        ],
                    });
                }
            }
            let marks_before = target.decorations.positions();
            let delta = target.text.apply_edits(edits)?;
            target.decorations.advance(&delta);
            let change = target.record_change(edits.clone(), delta.clone());
            state
                .events
                .push(crate::editor::EditorEvent::BufferChanged {
                    buffer: *buffer,
                    change,
                });
            if let Some(view) = &mut target.saved_view {
                view.cursor = delta.map_offset(view.cursor, Affinity::After);
            }
            target.refresh_dirty();
            if undo_policy == UndoPolicy::Ignore {
                if !edits.edits().is_empty() {
                    target.undo.reset();
                }
            } else {
                target.undo.record_with_marks(
                    changes,
                    matches!(undo_policy, UndoPolicy::Merge | UndoPolicy::Boundary),
                    marks_before,
                    target.decorations.positions(),
                );
            }
            for (_, view) in state
                .views
                .iter_mut()
                .filter(|(_, view)| view.buffer == *buffer)
            {
                if let Some(repeat) = &mut view.insert_repeat {
                    repeat.start = delta.map_offset(repeat.start, Affinity::Before);
                }
                view.replace_history.retain_mut(|record| {
                    let end = ByteOffset(record.start.get() + record.inserted.len());
                    if delta.spans().iter().any(|span| {
                        if span.old.is_empty() {
                            span.old.start > record.start && span.old.start < end
                        } else {
                            span.old.start < end && span.old.end > record.start
                        }
                    }) {
                        return false;
                    }
                    record.start = delta.map_offset(record.start, Affinity::After);
                    true
                });
                view.cursor = delta.map_offset(view.cursor, Affinity::After);
                if let Some(selection) = &mut view.selection {
                    selection.anchor = delta.map_offset(selection.anchor, Affinity::Before);
                    selection.head = delta.map_offset(selection.head, Affinity::After);
                }
                for position in &mut view.block_insert {
                    *position = delta.map_offset(*position, Affinity::After);
                }
                view.block_insert.sort();
                view.block_insert.dedup();
                for mark in &mut view.multi_marks {
                    mark.start = delta.map_offset(mark.start, Affinity::Before);
                    mark.end = delta.map_offset(mark.end, Affinity::After);
                }
                view.multi_marks.sort_by_key(|mark| mark.start);
                view.multi_marks.dedup();
                view.multi_preferred_columns.clear();
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
            if target.cursor != *position
                && target.mode == EditorMode::Replace
                && matches!(undo_policy, UndoPolicy::Ignore | UndoPolicy::Boundary)
            {
                target.replace_history.clear();
            }
            if target.cursor != *position && undo_policy == UndoPolicy::Boundary {
                target.insert_repeat = None;
            }
            target.cursor = *position;
            target.preferred_column = None;
            target.multi_preferred_columns.clear();
            if let Some(selection) = &mut target.selection {
                selection.head = *position;
            }
        }
        EditorOperation::SetPreferredColumn { view, column } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .preferred_column = Some(*column);
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
                EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock => {
                    let kind = match mode {
                        EditorMode::VisualLine => crate::editor::SelectionKind::Line,
                        EditorMode::VisualBlock => crate::editor::SelectionKind::Block,
                        _ => crate::editor::SelectionKind::Character,
                    };
                    let selection = target.selection.get_or_insert(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind,
                    });
                    selection.kind = kind;
                }
                EditorMode::Normal
                | EditorMode::Insert
                | EditorMode::Replace
                | EditorMode::Multi => {
                    target.selection = None;
                }
                _ => {}
            }
            if *mode != EditorMode::Replace {
                target.replace_history.clear();
            }
            if *mode != EditorMode::Insert {
                target.insert_repeat = None;
                target.block_insert.clear();
            }
            if *mode != EditorMode::Multi {
                target.multi_marks.clear();
                target.multi_whole_word = true;
                target.multi_preferred_columns.clear();
            }
            if *mode == EditorMode::Normal {
                target.clear_editing_state();
            }
        }
        EditorOperation::SetInsertRepeat {
            view,
            count,
            new_line,
        } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.insert_repeat = Some(crate::editor::state::InsertRepeat {
                start: target.cursor,
                count: *count,
                new_line: *new_line,
            });
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
        EditorOperation::SetMultiMarks {
            view,
            marks,
            whole_word,
            preferred_columns,
        } => {
            let target = state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?;
            target.multi_marks = marks.clone();
            target.multi_whole_word = *whole_word;
            target.multi_preferred_columns = preferred_columns.clone();
        }
        EditorOperation::SetBlockInsert { view, positions } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .block_insert = positions.clone();
        }
        EditorOperation::Yanked { .. } => {}
        EditorOperation::SetRegister { name, text, shape } => state.registers.set(
            *name,
            RegisterValue {
                text: text.clone(),
                shape: *shape,
            },
            state.policy.registers_enabled,
            state.policy.mirror_named_to_unnamed,
        ),
        EditorOperation::Undo { buffer } | EditorOperation::Redo { buffer } => {
            let undo = matches!(operation, EditorOperation::Undo { .. });
            let target = state
                .buffers
                .get_mut(*buffer)
                .ok_or(TransactionError::MissingBuffer)?;
            if target.read_only {
                return Err(TransactionError::ReadOnly);
            }
            let mark_positions = target.undo.mark_positions(undo);
            let changes = if undo {
                target.undo.undo().ok_or(TransactionError::NothingToUndo)?
            } else {
                target.undo.redo().ok_or(TransactionError::NothingToRedo)?
            };
            let first = changes.iter().map(|change| ByteOffset(change.start)).min();
            for change in changes {
                let range = ByteRange::new(
                    ByteOffset(change.start),
                    ByteOffset(change.start + change.removed.len()),
                )
                .map_err(|error| TransactionError::Position(error.to_string()))?;
                if target.text.slice(range).ok().as_deref() != Some(change.removed.as_str()) {
                    return Err(TransactionError::Position(
                        "undo history no longer matches the text".into(),
                    ));
                }
                let first_line = target.text.byte_to_line(range.start).unwrap_or(0);
                let edits = TextEditSet::new(
                    target.text.revision(),
                    vec![TextEdit::new(range, change.inserted)],
                );
                let delta = target.text.apply_edits(&edits)?;
                target.decorations.advance(&delta);
                let record = target.record_change(edits, delta.clone());
                state
                    .events
                    .push(crate::editor::EditorEvent::BufferChanged {
                        buffer: *buffer,
                        change: record,
                    });
                target
                    .text
                    .set_line_endings(first_line, &change.inserted_endings);
                if let Some(view) = &mut target.saved_view {
                    view.cursor = delta.map_offset(view.cursor, Affinity::Before);
                }
                for (_, view) in state
                    .views
                    .iter_mut()
                    .filter(|(_, view)| view.buffer == *buffer)
                {
                    view.cursor = delta.map_offset(view.cursor, Affinity::Before);
                    for position in &mut view.block_insert {
                        *position = delta.map_offset(*position, Affinity::After);
                    }
                    view.block_insert.sort();
                    view.block_insert.dedup();
                    for mark in &mut view.multi_marks {
                        mark.start = delta.map_offset(mark.start, Affinity::Before);
                        mark.end = delta.map_offset(mark.end, Affinity::After);
                    }
                    view.multi_marks.sort_by_key(|mark| mark.start);
                    view.multi_marks.dedup();
                    view.multi_preferred_columns.clear();
                    view.replace_history.clear();
                    view.insert_repeat = None;
                }
            }
            if let Some(positions) = mark_positions {
                target.decorations.restore_positions(&positions);
            }
            target.refresh_dirty();
            clamp_views(state, *buffer);
            // Like Vim, the cursor lands on the earliest changed text.
            if let (Some(first), Some(view)) = (first, state.active_view())
                && let Some(view) = state.views.get_mut(view).filter(|v| v.buffer == *buffer)
            {
                view.cursor = first.min(ByteOffset(state.buffers[*buffer].text.len_bytes()));
                view.preferred_column = None;
            }
        }
        EditorOperation::SplitPane { axis, ratio } => {
            state
                .split_active(*axis, *ratio)
                .map_err(|error| TransactionError::Layout(error.into()))?;
        }
        EditorOperation::ClonePane { pane, axis, ratio } => {
            state
                .clone_pane(*pane, *axis, *ratio)
                .map_err(|error| TransactionError::Layout(error.into()))?;
        }
        EditorOperation::NewTab => {
            state
                .new_tab()
                .map_err(|error| TransactionError::Layout(error.into()))?;
        }
        EditorOperation::ToggleZoom => state
            .toggle_zoom()
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::ClosePane => state
            .close_active_pane()
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::ResizePane { delta } => state
            .resize_active_pane(*delta)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::ResizePaneOnAxis { axis, delta } => state
            .resize_active_pane_on_axis(*axis, *delta)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::SetSplitRatio {
            first,
            second,
            ratio,
        } => state
            .set_split_ratio_between(*first, *second, *ratio)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::SwapPanes { first, second } => state
            .swap_panes(*first, *second)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::CloseTab => state
            .close_active_tab()
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::DiscardUnreferencedBuffers { buffers } => {
            for buffer in buffers {
                if !state.buffers.contains_key(*buffer)
                    || state.views.values().any(|view| view.buffer == *buffer)
                {
                    return Err(TransactionError::Layout(
                        "discard target still has a view or is closed".into(),
                    ));
                }
            }
            for buffer in buffers {
                state.remove_buffer(*buffer);
            }
        }
        EditorOperation::FocusPane { offset, wrap } => state
            .focus_pane(*offset, *wrap)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusPaneId { pane } => state
            .focus_pane_id(*pane)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusTab { offset, wrap } => state
            .focus_tab(*offset, *wrap)
            .map_err(|error| TransactionError::Layout(error.into()))?,
        EditorOperation::FocusTabId { tab } => state
            .activate_tab(*tab)
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

/// Detached editor-owned proposal. Its origin and resulting stores cannot be
/// replaced or paired independently by the window/extension owner.
pub(crate) struct EditorDraft {
    before: EditorVersion,
    before_graph_revision: Option<u64>,
    state: EditorState,
}
impl EditorState {
    pub(crate) fn prepare_transaction(
        &self,
        transaction: &EditorTransaction,
    ) -> Result<EditorDraft, TransactionError> {
        validate_preconditions(self, &transaction.preconditions)?;
        self.draft().apply_operations(transaction)
    }
    pub(crate) fn draft(&self) -> EditorDraft {
        EditorDraft {
            before: self.version(),
            before_graph_revision: self.graph.revision(),
            state: self.clone(),
        }
    }
}
impl EditorDraft {
    pub(crate) fn before_graph_revision(&self) -> Option<u64> {
        self.before_graph_revision
    }

    pub(crate) fn version(&self) -> EditorVersion {
        self.state.version()
    }
    pub(crate) fn remove_virtual_document(
        &mut self,
        buffer: BufferId,
        owner: &str,
        load_id: u64,
        document: u64,
    ) -> Result<(), String> {
        if !self.state.buffers.get(buffer).is_some_and(|buffer| matches!(&buffer.origin,
            crate::editor::BufferOrigin::VirtualReadOnly { owner: current, load_id: load, document: id, .. }
                if current == owner && *load == load_id && *id == document)) {
            return Err("virtual document removal source changed".into());
        }
        let focus = self.state.focused_window();
        let views = self
            .state
            .views
            .iter()
            .filter_map(|(view, value)| (value.buffer == buffer).then_some(view))
            .collect::<Vec<_>>();
        for view in views {
            self.state.close_view(view).map_err(str::to_owned)?;
        }
        self.state.remove_buffer(buffer);
        if let Some(window) = focus
            && let Some(view) = self.state.window_view(window)
        {
            self.state.focus_view(view).map_err(str::to_owned)?;
        }
        Ok(())
    }
    pub(crate) fn before_window(&self) -> &EditorWindowVersion {
        self.before.window()
    }
    pub(crate) fn topology(&self) -> EditorTopology<'_> {
        (&self.state).into()
    }
    pub(crate) fn window_version(&self) -> EditorWindowVersion {
        self.state.window_version()
    }
    fn apply_operations(
        mut self,
        transaction: &EditorTransaction,
    ) -> Result<Self, TransactionError> {
        let before = self.state.version();
        // A failed operation consumes this detached draft; partial edits cannot
        // escape as an acceptable editor proposal, and live state is untouched.
        let dirty_before = transaction
            .operations
            .iter()
            .filter_map(|operation| operation.edited_buffer())
            .map(|id| (id, self.state.buffers.get(id).map(|b| b.dirty)))
            .collect::<Vec<_>>();
        for operation in &transaction.operations {
            let views = view_positions(&self.state);
            apply_operation(&mut self.state, operation, transaction.undo_policy)?;
            record_view_changes(&mut self.state, &views);
        }
        let views = view_positions(&self.state);
        for (_, view) in self.state.views.iter_mut().filter(|(_, view)| {
            view.mode == EditorMode::Normal
                && dirty_before
                    .iter()
                    .any(|(buffer, _)| *buffer == view.buffer)
        }) {
            let text = &self.state.buffers[view.buffer].text;
            let line = text
                .byte_to_line(view.cursor)
                .map_err(|error| TransactionError::Position(error.to_string()))?;
            let start = text
                .line_to_byte(line)
                .map_err(|error| TransactionError::Position(error.to_string()))?;
            let content = text
                .line(line)
                .map_err(|error| TransactionError::Position(error.to_string()))?;
            let end = ByteOffset(start.get() + content.trim_end_matches('\n').len());
            if view.cursor >= end && end > start {
                view.cursor = crate::editor::text::previous_grapheme_boundary(text, end);
                view.preferred_column = None;
            }
        }
        record_view_changes(&mut self.state, &views);
        if transaction.undo_policy == UndoPolicy::Boundary {
            for (_, buffer) in &mut self.state.buffers {
                buffer.undo.boundary();
            }
        }
        if dirty_before
            .into_iter()
            .any(|(id, dirty)| dirty != self.state.buffers.get(id).map(|b| b.dirty))
        {
            self.state.mark_window_changed();
        }
        if self.state.version() == before {
            self.state.mark_editor_changed();
        }
        Ok(self)
    }
    pub(crate) fn check_current(&self, current: &EditorVersion) -> Result<(), String> {
        if &self.before != current
            || self.before.window().revision().is_none()
            || self.state.window_revision().is_none()
        {
            return Err("editor proposal source changed before acceptance".into());
        }
        Ok(())
    }
    pub(crate) fn accept(self, current: &EditorVersion) -> Result<EditorState, String> {
        self.check_current(current)?;
        Ok(self.state)
    }
    pub(crate) fn open_document(
        &mut self,
        text: TextBuffer,
        path: std::path::PathBuf,
    ) -> Result<(BufferId, ViewId), &'static str> {
        self.state.open_text(text, Some(path))
    }
    pub(crate) fn open_new_document(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<(BufferId, ViewId), &'static str> {
        let existing = self
            .state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&path));
        let (buffer, view) = self
            .state
            .open_text(TextBuffer::from_text(""), Some(path))?;
        if !existing {
            self.state.buffers[buffer].new_file = true;
        }
        Ok((buffer, view))
    }
    pub(crate) fn split_document(&mut self, ratio: u16) -> Result<(), &'static str> {
        self.state
            .split_active(crate::windows::TileAxis::Vertical, ratio)
            .map(|_| ())
    }
    pub(crate) fn load_document(
        &mut self,
        text: TextBuffer,
        path: std::path::PathBuf,
    ) -> Result<BufferId, String> {
        self.state.load_hidden_text(text, path)
    }
    pub(crate) fn reload_document(
        &mut self,
        buffer: BufferId,
        text: TextBuffer,
        path: std::path::PathBuf,
    ) -> bool {
        self.state.replace_buffer_from_disk(buffer, text, path)
    }
    pub(crate) fn save_document(
        &mut self,
        buffer: BufferId,
        expected: &crate::editor::text::TextVersion,
        revision: BufferRevision,
        bytes: Vec<u8>,
        path: std::path::PathBuf,
        snapshot: (std::sync::Arc<str>, crate::editor::text::FileFormat),
    ) -> bool {
        if self
            .state
            .buffers
            .get(buffer)
            .is_none_or(|b| !b.text.source_version().same_instance(expected))
        {
            return false;
        }
        let changed = self
            .state
            .buffers
            .get(buffer)
            .is_some_and(|b| &b.text.source_version() != expected);
        let Ok(saved) = TextBuffer::from_bytes(&bytes) else {
            return false;
        };
        let saved_text = saved.to_string();
        let mut saved_revision = revision;
        let (snapshot, snapshot_format) = snapshot;
        if !changed && saved_text != snapshot.as_ref() {
            // Save policy only changes the trailing newlines. Record that edit so the
            // live text and persistent undo both describe the bytes just written.
            let start = snapshot.trim_end_matches('\n').len();
            if !saved_text.starts_with(&snapshot[..start]) {
                return false;
            }
            let edits = TextEditSet::new(
                revision,
                vec![TextEdit::new(
                    ByteRange::new(ByteOffset(start), ByteOffset(snapshot.len()))
                        .expect("save suffix"),
                    &saved_text[start..],
                )],
            );
            self.state.buffers[buffer].undo.boundary();
            if apply_operation(
                &mut self.state,
                &EditorOperation::ApplyEdits { buffer, edits },
                UndoPolicy::Boundary,
            )
            .is_err()
            {
                return false;
            }
            self.state.buffers[buffer].undo.boundary();
            let text = &self.state.buffers[buffer].text;
            let end = ByteOffset(start);
            for (_, view) in self
                .state
                .views
                .iter_mut()
                .filter(|(_, view)| view.buffer == buffer)
            {
                // Writing a final newline must not move an insertion cursor onto a
                // new blank line, or leave a Normal cursor past the remaining text.
                view.cursor = view.cursor.min(end);
                if view.mode == EditorMode::Normal
                    && view.cursor.get() == text.len_bytes()
                    && !text.is_empty()
                {
                    view.cursor =
                        crate::editor::text::previous_grapheme_boundary(text, view.cursor);
                }
                if let Some(selection) = &mut view.selection {
                    selection.anchor = selection.anchor.min(end);
                    selection.head = selection.head.min(end);
                }
            }
            saved_revision = text.revision();
        }
        if !self
            .state
            .mark_buffer_saved_as(buffer, saved_revision, bytes, Some(path))
        {
            return false;
        }
        // Keep the requested format as the dirty baseline until an explicit line-ending
        // policy is applied, while retaining any final-newline normalization in the text.
        let format = if changed {
            snapshot_format
        } else {
            self.state.buffers[buffer].text.format().clone()
        };
        self.state.buffers[buffer].set_saved_snapshot(Some((saved_text.into(), format)));
        true
    }
    pub(crate) fn set_document_line_ending(
        &mut self,
        buffer: BufferId,
        ending: crate::editor::text::LineEnding,
    ) {
        if let Some(buffer) = self.state.buffers.get_mut(buffer) {
            buffer.text.set_line_ending(ending);
            if let Ok(saved) = TextBuffer::from_bytes(buffer.persisted_bytes()) {
                buffer.set_saved_snapshot(Some((saved.to_string().into(), saved.format().clone())));
            }
        }
    }
    pub(crate) fn close_document(
        &mut self,
        view: ViewId,
        buffer: BufferId,
    ) -> Result<bool, &'static str> {
        if self.state.active_view() != Some(view)
            || self
                .state
                .views
                .get(view)
                .is_none_or(|v| v.buffer != buffer)
        {
            return Err("close target changed");
        }
        // As LazyVim's buffer delete: the window stays and shows the buffer before (or after)
        // this one, or an empty one. Only the last window of a buffer-less editor closes.
        let others = self
            .state
            .buffers
            .keys()
            .filter(|other| *other != buffer && Some(*other) != self.state.kept_buffer)
            .collect::<Vec<_>>();
        let position = self
            .state
            .buffers
            .keys()
            .position(|other| other == buffer)
            .unwrap_or(0);
        let replacement = others
            .iter()
            .rev()
            .find(|other| self.state.buffers.keys().position(|key| key == **other) < Some(position))
            .or_else(|| others.first())
            .copied();
        if replacement.is_none() && self.state.is_last_pane() {
            self.state.close_active_view()?;
        } else {
            self.state.replace_active_view(replacement)?;
        }
        let removed = !self.state.views.values().any(|v| v.buffer == buffer);
        if removed {
            self.state.remove_buffer(buffer);
        }
        if self.state.active_view().is_none()
            && let Some(buffer) = self.state.buffers.keys().next()
        {
            self.state.show_buffer(buffer)?;
        }
        Ok(removed)
    }
    /// Drop a buffer no window shows. One still on screen stays.
    pub(crate) fn remove_hidden_document(&mut self, buffer: BufferId) {
        if !self.state.views.values().any(|view| view.buffer == buffer) {
            self.state.remove_buffer(buffer);
        }
    }
    pub(crate) fn show_document(&mut self, buffer: BufferId) -> Result<ViewId, &'static str> {
        self.state.show_buffer(buffer)
    }
    pub(crate) fn open_virtual_read_only(
        &mut self,
        text: TextBuffer,
        owner: String,
        load_id: u64,
        document: u64,
        revision: u64,
    ) -> Result<(BufferId, ViewId), &'static str> {
        self.state
            .open_virtual_read_only(text, owner, load_id, document, revision)
    }
    pub(crate) fn set_virtual_metadata(
        &mut self,
        buffer: BufferId,
        title: String,
        language: String,
    ) {
        let target = &mut self.state.buffers[buffer];
        target.virtual_title = Some(title);
        target.virtual_language = Some(language);
        self.state.mark_window_changed();
    }
    pub(crate) fn replace_virtual_read_only(
        &mut self,
        buffer: BufferId,
        expected: u64,
        revision: u64,
        text: TextBuffer,
    ) -> bool {
        self.state
            .replace_virtual_read_only(buffer, expected, revision, text)
    }
}

impl EditorState {
    pub(crate) fn prepare_document_location(
        &self,
        buffer: BufferId,
        position: ByteOffset,
        transaction: TransactionId,
    ) -> Result<EditorDraft, String> {
        self.buffers
            .get(buffer)
            .ok_or("navigation buffer closed")?
            .text
            .validate_offset(position)
            .map_err(|e| e.to_string())?;
        if self.window_version().revision().is_none() {
            return Err("navigation editor exhausted".into());
        }
        let mut draft = self.draft();
        let view = draft.show_document(buffer)?;
        draft
            .apply_operations(
                &EditorTransaction::new(transaction)
                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                    .with_operation(EditorOperation::SetCursor { view, position })
                    .with_undo_policy(UndoPolicy::Ignore),
            )
            .map_err(|e| e.to_string())
    }
}

pub use workspace::DocumentSnapshot;
pub(crate) use workspace::DraftEdit;

pub(crate) use restore::{RestoredLayout, RestoredPane, RestoredTab};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::editor::text::ByteOffset;

    use crate::editor::text::ByteRange;

    use crate::editor::text::TextBuffer;

    use crate::editor::text::TextEdit;

    use crate::editor::text::TextEditSet;

    use crate::editor::EditorMode;

    use crate::editor::EditorOperation;

    use crate::editor::EditorState;

    use crate::editor::EditorTransaction;

    use crate::editor::Precondition;

    use crate::editor::TransactionId;

    fn edit(
        state: &mut EditorState,
        buffer: crate::editor::BufferId,
        edits: &[(usize, usize, &str)],
    ) {
        let edits = edits
            .iter()
            .map(|&(start, end, text)| {
                TextEdit::new(
                    ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
                    text,
                )
            })
            .collect();
        let edits = TextEditSet::new(state.buffers[buffer].text.revision(), edits);
        state
            .apply_transaction(
                &EditorTransaction::new(TransactionId(1))
                    .with_operation(EditorOperation::ApplyEdits { buffer, edits }),
            )
            .unwrap();
    }

    fn history(state: &mut EditorState, buffer: crate::editor::BufferId, undo: bool) -> String {
        let operation = if undo {
            EditorOperation::Undo { buffer }
        } else {
            EditorOperation::Redo { buffer }
        };
        state
            .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(operation))
            .unwrap();
        state.buffers[buffer].text.to_string()
    }

    #[test]
    fn multi_edit_steps_undo_redo_and_survive_a_restart_while_the_file_is_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = PathBuf::from("/workspace/notes.txt");
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.undo_directory = Some(directory.path().to_path_buf());
        state.undo_maximum_bytes = 1 << 20;
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("one two three"), Some(path.clone()))
            .unwrap();
        edit(&mut state, buffer, &[(0, 3, "1"), (8, 13, "3!")]);
        edit(&mut state, buffer, &[(2, 6, "")]);
        assert_eq!(state.buffers[buffer].text.to_string(), "1 3!");
        assert_eq!(history(&mut state, buffer, true), "1 two 3!");
        assert_eq!(history(&mut state, buffer, true), "one two three");
        assert!(!state.buffers[buffer].dirty);
        assert_eq!(history(&mut state, buffer, false), "1 two 3!");

        crate::editor::transaction::save_undo_history(
            directory.path().to_path_buf(),
            path.clone(),
            state.buffers[buffer].text.clone(),
            state.buffers[buffer].undo.clone(),
            1 << 20,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_dir(directory.path()).map_or(0, |entries| {
            entries
                .filter(|entry| {
                    entry
                        .as_ref()
                        .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))
                })
                .count()
        }) == 0
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let restart = || {
            let mut state = EditorState::new(PathBuf::from("/workspace"));
            state.undo_directory = Some(directory.path().to_path_buf());
            state.undo_maximum_bytes = 1 << 20;
            state
        };
        let mut changed_on_disk = restart();
        let (changed, _) = changed_on_disk
            .open_text(
                TextBuffer::from_text("edited elsewhere"),
                Some(path.clone()),
            )
            .unwrap();
        assert!(changed_on_disk.buffers[changed].undo.is_empty());
        let mut restarted = restart();
        let (reopened, _) = restarted
            .open_text(TextBuffer::from_text("1 two 3!"), Some(path))
            .unwrap();
        assert_eq!(history(&mut restarted, reopened, true), "one two three");
        assert_eq!(history(&mut restarted, reopened, false), "1 two 3!");
        assert_eq!(history(&mut restarted, reopened, false), "1 3!");
    }

    #[test]
    fn save_acknowledgement_uses_snapshot_contents_after_intervening_edits() {
        for restore in [false, true] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
            let expected = state.buffers[buffer].text.source_version();
            let revision = state.buffers[buffer].text.revision();
            edit(&mut state, buffer, &[(3, 3, "x")]);
            if restore {
                edit(&mut state, buffer, &[(3, 4, "")]);
            }
            assert_ne!(state.buffers[buffer].text.source_version(), expected);
            let mut draft = state.draft();
            assert!(draft.save_document(
                buffer,
                &expected,
                revision,
                b"abc".to_vec(),
                "/workspace/file".into(),
                ("abc".into(), crate::editor::text::FileFormat::default())
            ));
            state = draft.accept(&state.version()).unwrap();
            assert_eq!(state.buffers[buffer].dirty, !restore);
            assert_eq!(state.buffers[buffer].persisted_bytes(), b"abc");
        }
    }

    #[test]
    fn closing_a_buffer_restores_the_previous_buffers_saved_view() {
        let mut state = EditorState::new("/workspace".into());
        let (first, view) = state
            .open_text(TextBuffer::from_text(&"line\n".repeat(120)), None)
            .unwrap();
        state.views[view].cursor = ByteOffset(500);
        state.views[view].scroll_line = 95;
        state.views[view].scroll_wrap_row = 2;
        state.views[view].scroll_column = 3;
        let (second, view) = state
            .open_text(TextBuffer::from_text("other"), None)
            .unwrap();
        let pane = state
            .tab_focused_window(state.active_tab().unwrap())
            .unwrap();
        let mut draft = state.draft();
        assert!(draft.close_document(view, second).unwrap());
        state = draft.accept(&state.version()).unwrap();
        let restored = &state.views[state.active_view().unwrap()];
        assert_eq!(restored.buffer, first);
        assert_eq!(restored.cursor, ByteOffset(500));
        assert_eq!(restored.scroll_line, 95);
        assert_eq!(restored.scroll_wrap_row, 2);
        assert_eq!(restored.scroll_column, 3);
        assert_eq!(
            state
                .tab_focused_window(state.active_tab().unwrap())
                .unwrap(),
            pane
        );
        assert!(!state.buffers.contains_key(second));
        state.validate_layout().unwrap();
    }

    #[test]
    fn failed_late_operation_rolls_back_earlier_edit() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
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
                view: crate::editor::ViewId::default(),
                position: ByteOffset(0),
            });
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }

    #[test]
    fn failed_late_operation_rolls_back_scroll_column() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (_, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
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
        let (buffer, view) = state
            .open_virtual_read_only(TextBuffer::from_text("héllo"), "demo".into(), 7, 11, 1)
            .unwrap();
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
            Err(crate::editor::transaction::TransactionError::ReadOnly)
        ));
        for operation in [
            EditorOperation::Undo { buffer },
            EditorOperation::Redo { buffer },
        ] {
            assert!(matches!(
                state.apply_transaction(
                    &EditorTransaction::new(TransactionId(5)).with_operation(operation)
                ),
                Err(crate::editor::transaction::TransactionError::ReadOnly)
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
    fn save_newline_normalization_preserves_valid_cursor_positions() {
        for (source, saved, mode, cursor, expected_cursor) in [
            ("abc", "abc\n", EditorMode::Insert, 3, 3),
            ("", "\n", EditorMode::Normal, 0, 0),
            ("abc\n\n", "abc", EditorMode::Normal, 4, 2),
            ("abc\n\n", "abc", EditorMode::Insert, 4, 3),
        ] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, view) = state
                .open_text(TextBuffer::from_text(source), None)
                .unwrap();
            state.views[view].mode = mode;
            state.views[view].cursor = ByteOffset(cursor);
            let text = &state.buffers[buffer].text;
            let mut draft = state.draft();
            assert!(draft.save_document(
                buffer,
                &text.source_version(),
                text.revision(),
                saved.as_bytes().to_vec(),
                "/workspace/file".into(),
                (source.into(), text.format().clone())
            ));
            state = draft.accept(&state.version()).unwrap();
            assert_eq!(state.views[view].cursor, ByteOffset(expected_cursor));
        }
    }

    #[test]
    fn unchanged_text_with_changed_line_endings_is_undoable() {
        let mut state = EditorState::new("/workspace".into());
        let original = b"a\r\nb\n";
        let (buffer, _) = state
            .open_text(TextBuffer::from_bytes(original).unwrap(), None)
            .unwrap();
        state
            .apply_transaction(
                &EditorTransaction::new(TransactionId(1)).with_operation(
                    EditorOperation::ApplyEdits {
                        buffer,
                        edits: TextEditSet::new(
                            state.buffers[buffer].text.revision(),
                            vec![TextEdit::new(
                                ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
                                "a\nb\n",
                            )],
                        )
                        .with_normalized_line_endings(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(state.buffers[buffer].text.to_bytes(), b"a\nb\n");
        history(&mut state, buffer, true);
        assert_eq!(state.buffers[buffer].text.to_bytes(), original);
        history(&mut state, buffer, false);
        assert_eq!(state.buffers[buffer].text.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn mixed_line_endings_follow_surviving_text_through_undo() {
        let mut state = EditorState::new("/tmp".into());
        let original = b"one\r\ntwo\nthree\r";
        let (buffer, _) = state
            .open_text(TextBuffer::from_bytes(original).unwrap(), None)
            .unwrap();
        edit(&mut state, buffer, &[(0, 4, "")]);
        assert_eq!(state.buffers[buffer].text.to_bytes(), b"two\nthree\r");
        history(&mut state, buffer, true);
        assert_eq!(state.buffers[buffer].text.to_bytes(), original);
        history(&mut state, buffer, false);
        assert_eq!(state.buffers[buffer].text.to_bytes(), b"two\nthree\r");
    }

    #[test]
    fn history_clears_the_preferred_column_at_the_change_location() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text("abc\ndef"), None)
            .unwrap();
        edit(&mut state, buffer, &[(0, 0, "x")]);
        for undo in [true, false] {
            state.views[view].cursor = ByteOffset(3);
            state.views[view].preferred_column = Some(20);
            history(&mut state, buffer, undo);
            assert_eq!(state.views[view].cursor, ByteOffset(0));
            assert_eq!(state.views[view].preferred_column, None);
        }
    }

    #[test]
    fn history_deduplicates_collapsed_block_insert_positions() {
        for undo in [true, false] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, view) = state.open_text(TextBuffer::from_text(""), None).unwrap();
            edit(&mut state, buffer, &[(0, 0, "a\nb")]);
            if !undo {
                edit(&mut state, buffer, &[(0, 3, "")]);
                history(&mut state, buffer, true);
            }
            state.views[view].mode = crate::editor::EditorMode::Insert;
            state.views[view].block_insert = vec![ByteOffset(0), ByteOffset(2)];
            assert_eq!(history(&mut state, buffer, undo), "");
            assert_eq!(state.views[view].block_insert, [ByteOffset(0)]);
            let mut registry = crate::editor::CommandRegistry::<()>::default();
            crate::editor::register_builtin_commands(&mut registry).unwrap();
            let invocation = crate::editor::CommandInvocation::new(
                "edit.insert_text",
                crate::editor::InvocationSource::Test,
            )
            .unwrap()
            .with_argument("text", crate::editor::CommandValue::String("x".into()));
            let outcome = registry.execute(&state, &invocation).unwrap();
            state
                .apply_transaction(&outcome.transaction.unwrap())
                .unwrap();
            assert_eq!(state.buffers[buffer].text.to_string(), "x");
        }
    }

    #[test]
    fn history_maps_multi_cursor_anchors() {
        let mut state = EditorState::new("/tmp".into());
        let (buffer, view) = state.open_text(TextBuffer::from_text("foo"), None).unwrap();
        edit(&mut state, buffer, &[(0, 0, "xx ")]);
        state.views[view].multi_marks = vec![ByteRange::new(ByteOffset(3), ByteOffset(6)).unwrap()];
        state.views[view].block_insert = vec![ByteOffset(6)];
        history(&mut state, buffer, true);
        assert_eq!(
            state.views[view].multi_marks[0],
            ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap()
        );
        assert_eq!(state.views[view].block_insert, [ByteOffset(3)]);
        history(&mut state, buffer, false);
        assert_eq!(state.views[view].multi_marks[0].end, ByteOffset(6));
        assert_eq!(state.views[view].block_insert, [ByteOffset(6)]);
    }

    #[test]
    fn virtual_refresh_clears_editing_positions_in_every_view() {
        let mut state = EditorState::new("/tmp".into());
        let (buffer, first) = state
            .open_virtual_read_only(TextBuffer::from_text("long text"), "demo".into(), 1, 1, 1)
            .unwrap();
        let second = state
            .split_active(crate::windows::TileAxis::Horizontal, 500)
            .unwrap();
        for id in [first, second] {
            let view = &mut state.views[id];
            view.cursor = ByteOffset(8);
            view.mode = crate::editor::EditorMode::Multi;
            view.multi_marks = vec![ByteRange::new(ByteOffset(5), ByteOffset(9)).unwrap()];
            view.block_insert = vec![ByteOffset(8)];
            view.preferred_column = Some(8);
            view.replace_history
                .push(crate::editor::state::ReplaceRecord {
                    start: ByteOffset(8),
                    original: "t".into(),
                    inserted: "x".into(),
                });
            view.scroll_line = 10;
            view.scroll_wrap_row = 3;
        }
        let mut draft = state.draft();
        assert!(draft.replace_virtual_read_only(buffer, 1, 2, TextBuffer::from_text("x")));
        let state = draft.accept(&state.version()).unwrap();
        for id in [first, second] {
            let view = &state.views[id];
            assert_eq!(view.cursor, ByteOffset(1));
            assert_eq!(view.mode, crate::editor::EditorMode::Normal);
            assert!(view.multi_marks.is_empty());
            assert!(view.block_insert.is_empty());
            assert!(view.replace_history.is_empty());
            assert_eq!(view.preferred_column, None);
            assert_eq!(view.scroll_line, 0);
            assert_eq!(view.scroll_wrap_row, 0);
        }
    }

    #[test]
    fn virtual_refresh_drops_selection_inside_a_character() {
        let mut state = EditorState::new("/tmp".into());
        let (buffer, view) = state
            .open_virtual_read_only(TextBuffer::from_text("ab"), "demo".into(), 1, 1, 1)
            .unwrap();
        state.views[view].cursor = ByteOffset(1);
        state.views[view].selection = Some(crate::editor::Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(1),
            kind: crate::editor::SelectionKind::Character,
        });
        let mut draft = state.draft();
        assert!(draft.replace_virtual_read_only(buffer, 1, 2, TextBuffer::from_text("éx")));
        assert_eq!(draft.state.views[view].cursor, ByteOffset(0));
        assert_eq!(draft.state.views[view].selection, None);
    }

    #[test]
    fn stale_precondition_changes_nothing() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        let transaction = EditorTransaction::new(TransactionId(3)).with_precondition(
            Precondition::BufferRevision {
                buffer,
                revision: crate::editor::text::BufferRevision(9),
            },
        );
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }
}
