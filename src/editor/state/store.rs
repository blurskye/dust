//! Editor resource storage and active views.

use crate::editor::EditorEvent;
use crate::editor::state::EditingPolicy;
use crate::editor::state::buffer::{Buffer, BufferOrigin};
use crate::editor::state::events::EditorEvents;
use crate::editor::state::id::{BufferId, ViewId};
use crate::editor::state::registers::RegisterStore;
use crate::editor::state::view::{EditorMode, View};
use crate::editor::text::{BufferRevision, ByteOffset, TextBuffer};
use crate::windows::{ContentId, WindowGraph, WorkspaceTabId};
use slotmap::SlotMap;
use std::collections::HashMap;
use std::path::PathBuf;

/// Opaque identity/revision of editor-owned window topology. Transaction clones
/// preserve identity; independently constructed editor states do not.
#[derive(Clone, Debug)]
pub(crate) struct EditorWindowVersion {
    identity: std::sync::Arc<()>,
    snapshot: std::sync::Arc<()>,
    revision: Option<u64>,
}
impl PartialEq for EditorWindowVersion {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.identity, &other.identity)
            && std::sync::Arc::ptr_eq(&self.snapshot, &other.snapshot)
            && self.revision == other.revision
    }
}
impl Eq for EditorWindowVersion {}
impl EditorWindowVersion {
    pub(crate) fn same_editor(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.identity, &other.identity)
    }
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }
}

/// An editor-created immutable source view. Callers cannot pair one editor's
/// revision ticket with another editor's stores, or supply mutable authority.
pub(crate) struct EditorTopology<'a> {
    state: &'a EditorState,
    version: EditorWindowVersion,
}
impl<'a> From<&'a EditorState> for EditorTopology<'a> {
    fn from(state: &'a EditorState) -> Self {
        Self {
            state,
            version: state.window_version(),
        }
    }
}
impl<'a> EditorTopology<'a> {
    pub(crate) fn state(&self) -> &'a EditorState {
        self.state
    }
    pub(crate) fn workspace_root(&self) -> &'a std::path::Path {
        &self.state.workspace_root
    }
    pub(crate) fn buffers(&self) -> &'a SlotMap<BufferId, Buffer> {
        &self.state.buffers
    }
    pub(crate) fn views(&self) -> &'a SlotMap<ViewId, View> {
        &self.state.views
    }
    pub(crate) fn graph(&self) -> &'a WindowGraph {
        &self.state.graph
    }
    pub(crate) fn active_tab(&self) -> Option<WorkspaceTabId> {
        self.state.active_tab()
    }
    pub(crate) fn version(&self) -> &EditorWindowVersion {
        &self.version
    }
    pub(crate) fn window_revision(&self) -> Option<u64> {
        self.version.revision()
    }
}

/// A view zoomed into its own tab, and where it came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Zoom {
    pub(super) tab: WorkspaceTabId,
    pub(super) origin_tab: WorkspaceTabId,
    pub(super) origin_view: ViewId,
}

#[cfg_attr(not(test), derive(Clone))]
#[derive(Debug)]
pub struct EditorState {
    pub(crate) events: EditorEvents,
    window_identity: std::sync::Arc<()>,
    window_snapshot: std::sync::Arc<()>,
    editor_snapshot: std::sync::Arc<()>,
    pub workspace_root: PathBuf,
    /// The repository a linked worktree belongs to, for showing with its name.
    pub workspace_repository: Option<String>,
    pub buffers: SlotMap<BufferId, Buffer>,
    pub views: SlotMap<ViewId, View>,
    pub(crate) graph: WindowGraph,
    pub(in crate::editor) buffer_contents: HashMap<BufferId, ContentId>,
    pub(in crate::editor) graph_views: HashMap<ViewId, crate::windows::ViewId>,
    pub registers: RegisterStore,
    pub policy: EditingPolicy,
    /// Where undo history is saved next to each file's text, when it is kept.
    pub undo_directory: Option<PathBuf>,
    pub undo_maximum_bytes: usize,
    pub(super) zoom: Option<Zoom>,
    /// A buffer that keeps its window, such as the agent panel's: files open beside it,
    /// never over it, and buffer cycling skips it.
    pub kept_buffer: Option<BufferId>,
    pub(super) window_revision: u64,
    window_revision_exhausted: bool,
}

#[cfg(test)]
thread_local! { static EDITOR_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
impl Clone for EditorState {
    fn clone(&self) -> Self {
        EDITOR_CLONES.with(|n| n.set(n.get() + 1));
        Self {
            events: self.events.clone(),
            window_identity: self.window_identity.clone(),
            window_snapshot: self.window_snapshot.clone(),
            editor_snapshot: self.editor_snapshot.clone(),
            workspace_root: self.workspace_root.clone(),
            workspace_repository: self.workspace_repository.clone(),
            buffers: self.buffers.clone(),
            views: self.views.clone(),
            graph: self.graph.clone(),
            buffer_contents: self.buffer_contents.clone(),
            graph_views: self.graph_views.clone(),
            registers: self.registers.clone(),
            policy: self.policy,
            undo_directory: self.undo_directory.clone(),
            undo_maximum_bytes: self.undo_maximum_bytes,
            zoom: self.zoom,
            kept_buffer: self.kept_buffer,
            window_revision: self.window_revision,
            window_revision_exhausted: self.window_revision_exhausted,
        }
    }
}

impl EditorState {
    pub fn set_workspace_root(&mut self, root: PathBuf) {
        if self.workspace_root != root {
            self.workspace_repository = None;
            self.events
                .push(EditorEvent::WorkspaceChanged { root: root.clone() });
            self.workspace_root = root;
            self.mark_window_changed();
        }
    }

    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            events: EditorEvents::default(),
            window_identity: std::sync::Arc::new(()),
            window_snapshot: std::sync::Arc::new(()),
            editor_snapshot: std::sync::Arc::new(()),
            workspace_repository: None,
            workspace_root,
            buffers: SlotMap::with_key(),
            views: SlotMap::with_key(),
            graph: WindowGraph::default(),
            buffer_contents: HashMap::new(),
            graph_views: HashMap::new(),
            registers: RegisterStore::default(),
            policy: EditingPolicy::default(),
            undo_directory: None,
            undo_maximum_bytes: 0,
            zoom: None,
            kept_buffer: None,
            window_revision: 0,
            window_revision_exhausted: false,
        }
    }

    /// Capture the source identity and current topology revision without copying stores.
    #[must_use]
    pub(crate) fn window_version(&self) -> EditorWindowVersion {
        EditorWindowVersion {
            identity: self.window_identity.clone(),
            snapshot: self.window_snapshot.clone(),
            revision: self.window_revision(),
        }
    }

    /// Exact source-owned gate for universal-window reconciliation.
    ///
    /// Exhaustion returns `None`; window creation and projections fail closed.
    #[must_use]
    pub const fn window_revision(&self) -> Option<u64> {
        if self.window_revision_exhausted {
            None
        } else {
            Some(self.window_revision)
        }
    }

    #[cfg(test)]
    pub(crate) fn test_clone_count() -> usize {
        EDITOR_CLONES.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    pub(crate) fn exhaust_window_source_for_test(&mut self) {
        self.window_revision = u64::MAX;
        self.mark_window_changed();
    }

    pub(crate) fn mark_editor_changed(&mut self) {
        self.editor_snapshot = std::sync::Arc::new(());
    }

    pub(crate) fn mark_window_changed(&mut self) {
        self.window_snapshot = std::sync::Arc::new(());
        self.mark_editor_changed();
        match self.window_revision.checked_add(1) {
            Some(revision) => self.window_revision = revision,
            None => self.window_revision_exhausted = true,
        }
    }

    pub fn set_buffer_dirty(&mut self, buffer: BufferId, dirty: bool) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        if target.dirty != dirty {
            target.dirty = dirty;
            self.mark_window_changed();
        }
        true
    }

    pub fn set_buffer_path_and_dirty(
        &mut self,
        buffer: BufferId,
        path: Option<PathBuf>,
        dirty: bool,
    ) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        let changed = target.path != path || target.dirty != dirty;
        target.path = path;
        target.dirty = dirty;
        if changed {
            self.mark_window_changed();
        }
        true
    }

    pub fn remove_buffer(&mut self, buffer: BufferId) -> Option<Buffer> {
        if let Some(content) = self.buffer_contents.get(&buffer).copied() {
            if self.graph.content(content).is_some() && self.graph.remove_content(content).is_err()
            {
                return None;
            }
            self.buffer_contents.remove(&buffer);
        }
        let removed = self.buffers.remove(buffer);
        if removed.is_some() {
            self.events.push(EditorEvent::BufferClosed { buffer });
            self.mark_window_changed();
        }
        removed
    }

    pub fn mark_buffer_saved_as(
        &mut self,
        buffer: BufferId,
        revision: BufferRevision,
        bytes: Vec<u8>,
        path: Option<PathBuf>,
    ) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        let before_path = target.path.clone();
        let before_dirty = target.dirty;
        target.mark_saved_as(revision, bytes, path);
        self.events.push(EditorEvent::BufferSaved {
            buffer,
            revision,
            path: target.path.clone(),
        });
        if target.path != before_path || target.dirty != before_dirty {
            self.mark_window_changed();
        } else {
            self.mark_editor_changed();
        }
        true
    }

    pub fn load_hidden_text(
        &mut self,
        text: TextBuffer,
        path: PathBuf,
    ) -> Result<BufferId, String> {
        if let Some((buffer, _)) = self
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        {
            return Ok(buffer);
        }
        let buffer = self.buffers.insert(self.file_buffer(text, path));
        if let Err(error) = self.register_window_content() {
            self.buffers.remove(buffer);
            return Err(error.to_string());
        }
        self.events.push(EditorEvent::BufferOpened { buffer });
        self.mark_window_changed();
        Ok(buffer)
    }

    pub(crate) fn open_virtual_read_only(
        &mut self,
        text: TextBuffer,
        owner: String,
        load_id: u64,
        document: u64,
        revision: u64,
    ) -> Result<(BufferId, ViewId), &'static str> {
        let mut target = Buffer::new(text, None);
        target.read_only = true;
        target.origin = BufferOrigin::VirtualReadOnly {
            owner,
            load_id,
            document,
            revision,
        };
        let buffer = self.buffers.insert(target);
        let view = self.views.insert(View::new(buffer));
        if let Err(error) = self.show_view(view) {
            self.remove_view(view)?;
            if let Some(content) = self.buffer_contents.remove(&buffer) {
                self.graph
                    .remove_content(content)
                    .map_err(|_| "new buffer content is still viewed")?;
            }
            self.buffers.remove(buffer);
            return Err(error);
        }
        Ok((buffer, view))
    }

    pub(crate) fn replace_virtual_read_only(
        &mut self,
        buffer: BufferId,
        expected_revision: u64,
        revision: u64,
        text: TextBuffer,
    ) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        let BufferOrigin::VirtualReadOnly {
            owner,
            load_id,
            document,
            revision: current,
        } = target.origin.clone()
        else {
            return false;
        };
        if current != expected_revision || revision != expected_revision.saturating_add(1) {
            return false;
        }
        let mut replacement = Buffer::new(text, None);
        replacement.read_only = true;
        replacement.virtual_title = target.virtual_title.clone();
        replacement.virtual_language = target.virtual_language.clone();
        replacement.origin = BufferOrigin::VirtualReadOnly {
            owner,
            load_id,
            document,
            revision,
        };
        *target = replacement;
        self.events.push(EditorEvent::BufferReloaded { buffer });
        let text = &target.text;
        let len = text.len_bytes();
        for (_, view) in self
            .views
            .iter_mut()
            .filter(|(_, view)| view.buffer == buffer)
        {
            view.cursor = ByteOffset(view.cursor.get().min(len));
            while !text.is_char_boundary(view.cursor) {
                view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
            }
            view.selection = view.selection.filter(|selection| {
                selection.anchor.get() <= len
                    && selection.head.get() <= len
                    && text.is_char_boundary(selection.anchor)
                    && text.is_char_boundary(selection.head)
            });
            if let Some(selection) = &mut view.selection {
                selection.head = view.cursor;
            }
            view.clear_editing_state();
            if view.mode == EditorMode::Multi {
                view.mode = EditorMode::Normal;
            }
            view.scroll_line = view.scroll_line.min(text.len_lines().saturating_sub(1));
            view.scroll_wrap_row = 0;
        }
        self.mark_editor_changed();
        true
    }

    pub(super) fn file_buffer(&self, text: TextBuffer, path: PathBuf) -> Buffer {
        let mut buffer = Buffer::new(text, Some(path));
        if let Some(directory) = &self.undo_directory {
            buffer.restore_undo_history(directory, self.undo_maximum_bytes);
        }
        buffer
    }

    pub fn open_text(
        &mut self,
        text: TextBuffer,
        path: Option<PathBuf>,
    ) -> Result<(BufferId, ViewId), &'static str> {
        if let Some(path) = path.as_ref()
            && let Some((buffer, _)) = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(path))
        {
            if let Some((view, _)) = self.views.iter().find(|(_, view)| view.buffer == buffer) {
                self.show_view(view)?;
                return Ok((buffer, view));
            }
            let view = self
                .views
                .insert(self.buffers[buffer].restored_view(buffer));
            if let Err(error) = self.show_view(view) {
                self.remove_view(view)?;
                return Err(error);
            }
            return Ok((buffer, view));
        }
        let buffer = match path {
            Some(path) => self.file_buffer(text, path),
            None => Buffer::new(text, None),
        };
        let buffer = self.buffers.insert(buffer);
        let view = self.views.insert(View::new(buffer));
        if let Err(error) = self.show_view(view) {
            self.remove_view(view)?;
            if let Some(content) = self.buffer_contents.remove(&buffer) {
                self.graph
                    .remove_content(content)
                    .map_err(|_| "new buffer content is still viewed")?;
            }
            self.buffers.remove(buffer);
            return Err(error);
        }
        self.events.push(EditorEvent::BufferOpened { buffer });
        Ok((buffer, view))
    }

    pub fn show_buffer(&mut self, buffer: BufferId) -> Result<ViewId, &'static str> {
        if !self.buffers.contains_key(buffer) {
            return Err("buffer is stale");
        }
        let existing = self
            .views
            .iter()
            .find(|(_, view)| view.buffer == buffer)
            .map(|(view, _)| view);
        let view = existing.unwrap_or_else(|| {
            self.views
                .insert(self.buffers[buffer].restored_view(buffer))
        });
        if let Err(error) = self.show_view(view) {
            if existing.is_none() {
                self.remove_view(view)?;
            }
            return Err(error);
        }
        Ok(view)
    }

    /// Show the buffer `offset` places from the active one in `order`, the buffer line's order.
    /// Buffers missing from `order` follow it.
    pub fn cycle_buffer(
        &mut self,
        order: &[BufferId],
        offset: isize,
        wrap: bool,
    ) -> Result<ViewId, &'static str> {
        let buffers = order
            .iter()
            .copied()
            .filter(|buffer| self.buffers.contains_key(*buffer))
            .chain(self.buffers.keys().filter(|buffer| !order.contains(buffer)))
            .filter(|buffer| Some(*buffer) != self.kept_buffer)
            .collect::<Vec<_>>();
        if buffers.is_empty() {
            return Err("no buffers are open");
        }
        let current = self.active_buffer_id().unwrap_or(buffers[0]);
        let index = buffers
            .iter()
            .position(|buffer| *buffer == current)
            .unwrap_or(0);
        let target = buffers[crate::windows::offset_index(index, buffers.len(), offset, wrap)];
        self.show_buffer(target)
    }

    pub fn replace_buffer_from_disk(
        &mut self,
        buffer: BufferId,
        text: TextBuffer,
        path: PathBuf,
    ) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        let read_only = target.read_only;
        let origin = target.origin.clone();
        *target = Buffer::new(text, Some(path));
        self.events.push(EditorEvent::BufferReloaded { buffer });
        if let Some(directory) = &self.undo_directory {
            target.restore_undo_history(directory, self.undo_maximum_bytes);
        }
        target.read_only = read_only;
        target.origin = origin;
        let maximum = ByteOffset(target.text.len_bytes());
        for (_, view) in self
            .views
            .iter_mut()
            .filter(|(_, view)| view.buffer == buffer)
        {
            view.cursor = view.cursor.min(maximum);
            while !target.text.is_char_boundary(view.cursor) {
                view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
            }
            view.selection = None;
            view.mode = EditorMode::Normal;
            view.clear_editing_state();
            view.scroll_line = view
                .scroll_line
                .min(target.text.len_lines().saturating_sub(1));
            view.scroll_wrap_row = 0;
        }
        self.mark_window_changed();
        true
    }

    #[must_use]
    pub fn active_buffer_id(&self) -> Option<BufferId> {
        self.active_view()
            .and_then(|view| self.views.get(view))
            .map(|view| view.buffer)
    }
}

/// Exact editor publication source, separate from presentation-only revisions.
#[derive(Clone, Debug)]
pub(crate) struct EditorVersion {
    window: EditorWindowVersion,
    snapshot: std::sync::Arc<()>,
}
impl PartialEq for EditorVersion {
    fn eq(&self, other: &Self) -> bool {
        self.window == other.window && std::sync::Arc::ptr_eq(&self.snapshot, &other.snapshot)
    }
}
impl Eq for EditorVersion {}

impl EditorState {
    pub(crate) fn version(&self) -> EditorVersion {
        EditorVersion {
            window: self.window_version(),
            snapshot: self.editor_snapshot.clone(),
        }
    }
}
impl EditorVersion {
    pub(crate) fn window(&self) -> &EditorWindowVersion {
        &self.window
    }
}
