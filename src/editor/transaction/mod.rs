use crate::editor::text::TextBuffer;
use crate::editor::{EditorTopology, EditorVersion, EditorWindowVersion};
mod paths;
mod restore;
#[cfg(test)]
pub(crate) use restore::build_restored_layout;
mod workspace;
pub(crate) use workspace::{
    EditorPublication, WorkspaceCommitCheck, WorkspaceEditSources, WorkspaceSnapshot,
};
mod undo;
pub(crate) use paths::BufferPathChange;

pub use undo::UndoTree;
pub(crate) use undo::{load_undo_history, save_undo_history};

use crate::editor::text::{
    Affinity, BufferRevision, ByteOffset, ByteRange, EditError, TextEdit, TextEditSet,
};
use thiserror::Error;

use crate::editor::{
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
        axis: crate::editor::Axis,
        ratio: u16,
    },
    ClonePane {
        pane: crate::editor::PaneId,
        axis: crate::editor::Axis,
        ratio: u16,
    },
    NewTab,
    ToggleZoom,
    ClosePane,
    ResizePane {
        delta: i16,
    },
    ResizePaneOnAxis {
        axis: crate::editor::Axis,
        delta: i16,
    },
    SwapPanes {
        first: crate::editor::PaneId,
        second: crate::editor::PaneId,
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
        pane: crate::editor::PaneId,
    },
    FocusTab {
        offset: isize,
        wrap: bool,
    },
    FocusTabId {
        tab: crate::editor::TabId,
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
                    });
                }
            }
            let delta = target.text.apply_edits(edits)?;
            target.refresh_dirty();
            if undo_policy == UndoPolicy::Ignore {
                if !edits.edits().is_empty() {
                    target.undo.reset();
                }
            } else {
                target
                    .undo
                    .record(changes, undo_policy == UndoPolicy::Merge);
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
                for mark in &mut view.multi_marks {
                    mark.start = delta.map_offset(mark.start, Affinity::Before);
                    mark.end = delta.map_offset(mark.end, Affinity::After);
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
            target.preferred_column = None;
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
                EditorMode::VisualChar if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editor::SelectionKind::Character,
                    });
                }
                EditorMode::VisualLine if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editor::SelectionKind::Line,
                    });
                }
                EditorMode::VisualBlock if target.selection.is_none() => {
                    target.selection = Some(Selection {
                        anchor: target.cursor,
                        head: target.cursor,
                        kind: crate::editor::SelectionKind::Block,
                    });
                }
                EditorMode::Normal | EditorMode::Insert | EditorMode::Replace | EditorMode::Multi => {
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
            if *mode != EditorMode::Multi {
                target.multi_marks.clear();
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
        EditorOperation::SetMultiMarks { view, marks } => {
            state
                .views
                .get_mut(*view)
                .ok_or(TransactionError::MissingView)?
                .multi_marks = marks.clone();
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
        EditorOperation::Undo { buffer } | EditorOperation::Redo { buffer } => {
            let undo = matches!(operation, EditorOperation::Undo { .. });
            let target = state
                .buffers
                .get_mut(*buffer)
                .ok_or(TransactionError::MissingBuffer)?;
            if target.read_only {
                return Err(TransactionError::ReadOnly);
            }
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
                let delta = target.text.apply_edits(&TextEditSet::new(
                    target.text.revision(),
                    vec![TextEdit::new(range, change.inserted)],
                ))?;
                for (_, view) in state
                    .views
                    .iter_mut()
                    .filter(|(_, view)| view.buffer == *buffer)
                {
                    view.cursor = delta.map_offset(view.cursor, Affinity::Before);
                }
            }
            target.refresh_dirty();
            clamp_views(state, *buffer);
            // Like Vim, the cursor lands on the earliest changed text.
            if let (Some(first), Some(view)) = (first, state.active_view) {
                if let Some(view) = state.views.get_mut(view).filter(|v| v.buffer == *buffer) {
                    view.cursor = first.min(ByteOffset(state.buffers[*buffer].text.len_bytes()));
                }
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
            state: self.clone(),
        }
    }
}
impl EditorDraft {
    pub(crate) fn version(&self) -> EditorVersion {
        self.state.version()
    }
    pub(crate) fn remove_virtual_document(
        &mut self,
        buffer: BufferId,
        owner: &str,
        generation: u64,
        document: u64,
    ) -> Result<(), String> {
        if !self.state.buffers.get(buffer).is_some_and(|buffer| matches!(&buffer.origin,
            crate::editor::BufferOrigin::VirtualReadOnly { owner: current, generation: load, document: id, .. }
                if current == owner && *load == generation && *id == document)) {
            return Err("virtual document removal source changed".into());
        }
        let focus = self
            .state
            .active_tab
            .and_then(|tab| self.state.tabs.get(tab).map(|value| (tab, value.focused)));
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
        if let Some((tab, pane)) = focus {
            if self.state.tabs.contains_key(tab) && self.state.panes.contains_key(pane) {
                self.state.active_tab = Some(tab);
                self.state.focus_pane_id(pane).map_err(str::to_owned)?;
            }
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
            apply_operation(&mut self.state, operation, transaction.undo_policy)?;
        }
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
    ) -> (BufferId, ViewId) {
        self.state.open_text(text, Some(path))
    }
    pub(crate) fn split_document(&mut self, ratio: u16) -> Result<(), &'static str> {
        self.state
            .split_active(crate::editor::Axis::Vertical, ratio)
            .map(|_| ())
    }
    pub(crate) fn load_document(&mut self, text: TextBuffer, path: std::path::PathBuf) -> BufferId {
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
    ) -> bool {
        let changed = self
            .state
            .buffers
            .get(buffer)
            .is_some_and(|b| &b.text.source_version() != expected);
        if !self
            .state
            .mark_buffer_saved_as(buffer, revision, bytes, Some(path))
        {
            return false;
        }
        if changed {
            self.state.set_buffer_dirty(buffer, true);
        }
        true
    }
    pub(crate) fn close_document(
        &mut self,
        view: ViewId,
        buffer: BufferId,
    ) -> Result<bool, &'static str> {
        if self.state.active_view != Some(view)
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
        let position = self.state.buffers.keys().position(|other| other == buffer).unwrap_or(0);
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
        if self.state.active_view.is_none() {
            if let Some(buffer) = self.state.buffers.keys().next() {
                self.state.show_buffer(buffer)?;
            }
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
        generation: u64,
        document: u64,
        revision: u64,
    ) -> (BufferId, ViewId) {
        self.state
            .open_virtual_read_only(text, owner, generation, document, revision)
    }
    pub(crate) fn replace_virtual_read_only(
        &mut self,
        buffer: BufferId,
        expected: u64,
        revision: u64,
        text: TextBuffer,
    ) -> bool {
        // Preserve the same cursor/selection/scroll policy as the virtual bridge,
        // without lending that bridge mutable editor stores.
        let views = self
            .state
            .views
            .iter()
            .filter(|(_, v)| v.buffer == buffer)
            .map(|(id, v)| (id, v.cursor, v.selection, v.scroll_line, v.scroll_column))
            .collect::<Vec<_>>();
        if !self
            .state
            .replace_virtual_read_only(buffer, expected, revision, text)
        {
            return false;
        }
        let text = &self.state.buffers[buffer].text;
        let len = text.len_bytes();
        for (id, cursor, selection, line, column) in views {
            if let Some(view) = self.state.views.get_mut(id) {
                view.cursor = ByteOffset(cursor.get().min(len));
                while !text.is_char_boundary(view.cursor) {
                    view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
                }
                view.selection = selection.filter(|s| s.anchor.get() <= len && s.head.get() <= len);
                view.scroll_line = line.min(text.len_lines().saturating_sub(1));
                view.scroll_column = column;
            }
        }
        true
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::editor::text::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};

    use crate::editor::{
        EditorOperation, EditorState, EditorTransaction, Precondition, TransactionId,
    };

    fn edit(state: &mut EditorState, buffer: crate::editor::BufferId, edits: &[(usize, usize, &str)]) {
        let edits = edits
            .iter()
            .map(|&(start, end, text)| {
                TextEdit::new(ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(), text)
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
        let (buffer, _) = state.open_text(TextBuffer::from_text("one two three"), Some(path.clone()));
        edit(&mut state, buffer, &[(0, 3, "1"), (8, 13, "3!")]);
        edit(&mut state, buffer, &[(2, 6, "")]);
        assert_eq!(state.buffers[buffer].text.to_string(), "1 3!");
        assert_eq!(history(&mut state, buffer, true), "1 two 3!");
        assert_eq!(history(&mut state, buffer, true), "one two three");
        assert!(!state.buffers[buffer].dirty);
        assert_eq!(history(&mut state, buffer, false), "1 two 3!");

        super::save_undo_history(
            directory.path().to_path_buf(),
            path.clone(),
            state.buffers[buffer].text.clone(),
            state.buffers[buffer].undo.clone(),
            1 << 20,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_dir(directory.path())
            .map_or(0, |entries| entries.filter(|entry| entry.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))).count())
            == 0
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
        let (changed, _) =
            changed_on_disk.open_text(TextBuffer::from_text("edited elsewhere"), Some(path.clone()));
        assert!(changed_on_disk.buffers[changed].undo.is_empty());
        let mut restarted = restart();
        let (reopened, _) = restarted.open_text(TextBuffer::from_text("1 two 3!"), Some(path));
        assert_eq!(history(&mut restarted, reopened, true), "one two three");
        assert_eq!(history(&mut restarted, reopened, false), "1 two 3!");
        assert_eq!(history(&mut restarted, reopened, false), "1 3!");
    }

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
                view: crate::editor::ViewId::default(),
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
                revision: crate::editor::text::BufferRevision(9),
            },
        );
        assert!(state.apply_transaction(&transaction).is_err());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }
}
