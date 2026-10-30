use std::{collections::HashSet, path::PathBuf};

use slotmap::{Key, SlotMap};

use crate::editor::text::{BufferRevision, ByteOffset, TextBuffer};

use super::{
    MAXIMUM_LAYOUT_DEPTH, MAXIMUM_LAYOUT_PANES,
    id::{BufferId, PaneId, TabId, ViewId},
    layout::{
        FocusRectangle, collect_focus_rectangles, focus_score, layout_depth,
        layout_ratios_are_valid, offset_index, pane_depth, pane_ids, remove_leaf, resize_parent,
        resize_parent_on_axis, split_leaf, swap_layout_leaves,
    },
    model::{
        Axis, Buffer, BufferOrigin, EditingPolicy, EditorMode, FocusDirection, LayoutNode, Pane,
        RegisterStore, Tab, View,
    },
};

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
    workspace_root: &'a std::path::Path,
    buffers: &'a SlotMap<BufferId, Buffer>,
    views: &'a SlotMap<ViewId, View>,
    panes: &'a SlotMap<PaneId, Pane>,
    tabs: &'a SlotMap<TabId, Tab>,
    active_tab: Option<TabId>,
    version: EditorWindowVersion,
}
impl<'a> From<&'a EditorState> for EditorTopology<'a> {
    fn from(state: &'a EditorState) -> Self {
        Self {
            workspace_root: &state.workspace_root,
            buffers: &state.buffers,
            views: &state.views,
            panes: &state.panes,
            tabs: &state.tabs,
            active_tab: state.active_tab,
            version: state.window_version(),
        }
    }
}
impl<'a> EditorTopology<'a> {
    pub(crate) fn workspace_root(&self) -> &'a std::path::Path {
        self.workspace_root
    }
    pub(crate) fn buffers(&self) -> &'a SlotMap<BufferId, Buffer> {
        self.buffers
    }
    pub(crate) fn views(&self) -> &'a SlotMap<ViewId, View> {
        self.views
    }
    pub(crate) fn panes(&self) -> &'a SlotMap<PaneId, Pane> {
        self.panes
    }
    pub(crate) fn tabs(&self) -> &'a SlotMap<TabId, Tab> {
        self.tabs
    }
    pub(crate) fn active_tab(&self) -> Option<TabId> {
        self.active_tab
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
struct Zoom {
    tab: TabId,
    origin_tab: TabId,
    origin_view: ViewId,
}

#[cfg_attr(not(test), derive(Clone))]
#[derive(Debug)]
pub struct EditorState {
    window_identity: std::sync::Arc<()>,
    window_snapshot: std::sync::Arc<()>,
    editor_snapshot: std::sync::Arc<()>,
    pub workspace_root: PathBuf,
    pub buffers: SlotMap<BufferId, Buffer>,
    pub views: SlotMap<ViewId, View>,
    pub panes: SlotMap<PaneId, Pane>,
    pub tabs: SlotMap<TabId, Tab>,
    pub active_tab: Option<TabId>,
    pub active_view: Option<ViewId>,
    pub registers: RegisterStore,
    pub policy: EditingPolicy,
    /// Where undo history is saved next to each file's text, when it is kept.
    pub undo_directory: Option<PathBuf>,
    pub undo_maximum_bytes: usize,
    zoom: Option<Zoom>,
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
            window_identity: self.window_identity.clone(),
            window_snapshot: self.window_snapshot.clone(),
            editor_snapshot: self.editor_snapshot.clone(),
            workspace_root: self.workspace_root.clone(),
            buffers: self.buffers.clone(),
            views: self.views.clone(),
            panes: self.panes.clone(),
            tabs: self.tabs.clone(),
            active_tab: self.active_tab,
            active_view: self.active_view,
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
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            window_identity: std::sync::Arc::new(()),
            window_snapshot: std::sync::Arc::new(()),
            editor_snapshot: std::sync::Arc::new(()),
            workspace_root,
            buffers: SlotMap::with_key(),
            views: SlotMap::with_key(),
            panes: SlotMap::with_key(),
            tabs: SlotMap::with_key(),
            active_tab: None,
            active_view: None,
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
    /// Exhaustion returns `None`; window admission and projections fail closed.
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
        let removed = self.buffers.remove(buffer);
        if removed.is_some() {
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
        if target.path != before_path || target.dirty != before_dirty {
            self.mark_window_changed();
        } else {
            self.mark_editor_changed();
        }
        true
    }

    pub fn load_hidden_text(&mut self, text: TextBuffer, path: PathBuf) -> BufferId {
        if let Some((buffer, _)) = self
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        {
            return buffer;
        }
        let buffer = self.buffers.insert(self.file_buffer(text, path));
        self.mark_window_changed();
        buffer
    }

    pub(crate) fn open_virtual_read_only(
        &mut self,
        text: TextBuffer,
        owner: String,
        generation: u64,
        document: u64,
        revision: u64,
    ) -> (BufferId, ViewId) {
        let (buffer, view) = self.open_text(text, None);
        let target = &mut self.buffers[buffer];
        target.read_only = true;
        target.origin = BufferOrigin::VirtualReadOnly {
            owner,
            generation,
            document,
            revision,
        };
        (buffer, view)
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
            generation,
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
        replacement.origin = BufferOrigin::VirtualReadOnly {
            owner,
            generation,
            document,
            revision,
        };
        *target = replacement;
        self.mark_editor_changed();
        true
    }

    fn file_buffer(&self, text: TextBuffer, path: PathBuf) -> Buffer {
        let mut buffer = Buffer::new(text, Some(path));
        if let Some(directory) = &self.undo_directory {
            buffer.restore_undo_history(directory, self.undo_maximum_bytes);
        }
        buffer
    }

    pub fn open_text(&mut self, text: TextBuffer, path: Option<PathBuf>) -> (BufferId, ViewId) {
        if let Some(path) = path.as_ref() {
            if let Some((buffer, _)) = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(path))
            {
                if let Some((view, _)) = self.views.iter().find(|(_, view)| view.buffer == buffer) {
                    self.show_view(view);
                    return (buffer, view);
                }
                let view = self.views.insert(View::new(buffer));
                self.show_view(view);
                return (buffer, view);
            }
        }
        let buffer = match path {
            Some(path) => self.file_buffer(text, path),
            None => Buffer::new(text, None),
        };
        let buffer = self.buffers.insert(buffer);
        let view = self.views.insert(View::new(buffer));
        self.show_view(view);
        (buffer, view)
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
        let view = existing.unwrap_or_else(|| self.views.insert(View::new(buffer)));
        self.show_view(view);
        Ok(view)
    }

    /// Show `buffer`, or a new empty one, in the focused pane in place of its view.
    pub fn replace_active_view(&mut self, buffer: Option<BufferId>) -> Result<(), &'static str> {
        let tab = self.active_tab.ok_or("no active tab")?;
        let pane = self.tabs.get(tab).ok_or("active tab is stale")?.focused;
        if !self.panes.contains_key(pane) {
            return Err("focused pane is stale");
        }
        let buffer = buffer.unwrap_or_else(|| self.buffers.insert(Buffer::new(TextBuffer::from_text(""), None)));
        let view = self.views.insert(View::new(buffer));
        let old = std::mem::replace(&mut self.panes[pane].view, view);
        self.views.remove(old);
        self.active_view = Some(view);
        self.mark_window_changed();
        Ok(())
    }

    pub fn cycle_buffer(&mut self, offset: isize, wrap: bool) -> Result<ViewId, &'static str> {
        let buffers = self
            .buffers
            .keys()
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
        let target = buffers[offset_index(index, buffers.len(), offset, wrap)];
        self.show_buffer(target)
    }

    fn show_view(&mut self, view: ViewId) {
        if let Some((tab_id, pane_id)) = self.tabs.iter().find_map(|(tab_id, tab)| {
            pane_ids(&tab.root).into_iter().find_map(|pane_id| {
                self.panes
                    .get(pane_id)
                    .is_some_and(|pane| pane.view == view)
                    .then_some((tab_id, pane_id))
            })
        }) {
            self.active_tab = Some(tab_id);
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                tab.focused = pane_id;
            }
            self.active_view = Some(view);
            self.mark_window_changed();
            return;
        }
        self.leave_kept_pane(view);
        if let Some(tab_id) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_id) {
                if let Some(pane) = self.panes.get_mut(tab.focused) {
                    let previous = pane.view;
                    pane.view = view;
                    self.active_view = Some(view);
                    if previous != view
                        && !self
                            .panes
                            .iter()
                            .any(|(_, candidate)| candidate.view == previous)
                        && let Some(old) = self.views.remove(previous)
                    {
                        // An untouched [No Name] buffer gives way to what opens over it, as
                        // in Vim, so `ctrl-t` and a picked file leave one tab.
                        let unused = self.buffers.get(old.buffer).is_some_and(|buffer| {
                            buffer.path.is_none()
                                && !buffer.dirty
                                && buffer.text.len_bytes() == 0
                                && buffer.origin == BufferOrigin::FileOrScratch
                        });
                        if unused && !self.views.values().any(|other| other.buffer == old.buffer) {
                            self.buffers.remove(old.buffer);
                        }
                    }
                    self.mark_window_changed();
                    return;
                }
            }
        }
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.insert(Tab {
            root: LayoutNode::Pane(pane),
            focused: pane,
        });
        self.active_tab = Some(tab);
        self.active_view = Some(view);
        self.mark_window_changed();
    }

    /// When the focused pane holds the kept buffer and something else is to be shown, focus
    /// another pane of the tab, or split one off to the kept pane's left.
    fn leave_kept_pane(&mut self, view: ViewId) {
        let Some(kept) = self.kept_buffer else {
            return;
        };
        let Some(tab_id) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get(tab_id) else {
            return;
        };
        let shows_kept = |state: &Self, pane: PaneId| {
            state
                .panes
                .get(pane)
                .and_then(|pane| state.views.get(pane.view))
                .is_some_and(|view| view.buffer == kept)
        };
        let focused = tab.focused;
        if !shows_kept(self, focused) || self.views.get(view).is_some_and(|view| view.buffer == kept) {
            return;
        }
        if let Some(other) = pane_ids(&tab.root).into_iter().find(|pane| !shows_kept(self, *pane)) {
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                tab.focused = other;
            }
            return;
        }
        // The split's new pane lands on the right: it takes the kept view, and the old pane
        // on the left is left focused for what opens.
        let kept_view = self.panes[focused].view;
        if let Ok(clone) = self.clone_pane(focused, Axis::Horizontal, 500)
            && let Some(right) = self.tabs.get(tab_id).map(|tab| tab.focused)
        {
            self.panes[right].view = kept_view;
            self.views.remove(clone);
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                tab.focused = focused;
            }
        }
    }

    pub fn split_active(&mut self, axis: Axis, ratio: u16) -> Result<ViewId, &'static str> {
        let tab = self.active_tab.ok_or("no active tab")?;
        let pane = self.tabs.get(tab).ok_or("active tab is stale")?.focused;
        self.clone_pane(pane, axis, ratio)
    }

    pub fn clone_pane(
        &mut self,
        source_pane: PaneId,
        axis: Axis,
        ratio: u16,
    ) -> Result<ViewId, &'static str> {
        if !(100..=900).contains(&ratio) {
            return Err("split ratio must be between 100 and 900");
        }
        let tab_id = self
            .tabs
            .iter()
            .find_map(|(id, tab)| pane_ids(&tab.root).contains(&source_pane).then_some(id))
            .ok_or("source pane is absent from the editor")?;
        let tab = self.tabs.get(tab_id).ok_or("source tab is stale")?;
        if self.panes.len() >= MAXIMUM_LAYOUT_PANES {
            return Err("layout pane ceiling reached");
        }
        if pane_depth(&tab.root, source_pane, 1).is_some_and(|depth| depth >= MAXIMUM_LAYOUT_DEPTH)
        {
            return Err("layout depth ceiling reached");
        }
        let source_view = self
            .panes
            .get(source_pane)
            .ok_or("source pane is stale")?
            .view;
        let cloned = self
            .views
            .get(source_view)
            .ok_or("source view is stale")?
            .clone();
        let view = self.views.insert(cloned);
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.get_mut(tab_id).ok_or("source tab is stale")?;
        if !split_leaf(&mut tab.root, source_pane, axis, ratio, pane) {
            self.panes.remove(pane);
            self.views.remove(view);
            return Err("source pane is absent from layout");
        }
        tab.focused = pane;
        self.active_tab = Some(tab_id);
        self.active_view = Some(view);
        self.mark_window_changed();
        Ok(view)
    }

    pub fn new_tab(&mut self) -> Result<TabId, &'static str> {
        if self.panes.len() >= MAXIMUM_LAYOUT_PANES {
            return Err("layout pane ceiling reached");
        }
        let source = self.active_view.ok_or("no active view")?;
        let view = self.views.insert(
            self.views
                .get(source)
                .ok_or("active view is stale")?
                .clone(),
        );
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.insert(Tab {
            root: LayoutNode::Pane(pane),
            focused: pane,
        });
        self.active_tab = Some(tab);
        self.active_view = Some(view);
        self.mark_window_changed();
        Ok(tab)
    }

    /// LazyVim's zoom: show the active view alone in a tab of its own. Toggling again closes
    /// that tab and puts the zoomed view back into the split it came from.
    pub fn toggle_zoom(&mut self) -> Result<(), &'static str> {
        if let Some(zoom) = self.zoom.take() {
            let origin_pane = self.tabs.get(zoom.origin_tab).and_then(|tab| {
                pane_ids(&tab.root)
                    .into_iter()
                    .find(|pane| self.panes.get(*pane).is_some_and(|p| p.view == zoom.origin_view))
            });
            if let (true, Some(pane)) = (self.active_tab == Some(zoom.tab), origin_pane) {
                let zoomed = self.active_view.and_then(|view| self.views.get(view)).cloned();
                self.close_active_tab()?;
                if let Some(zoomed) = zoomed {
                    self.views[zoom.origin_view] = zoomed;
                }
                self.active_tab = Some(zoom.origin_tab);
                return self.set_focused_pane(zoom.origin_tab, pane);
            }
        }
        let origin_tab = self.active_tab.ok_or("no active tab")?;
        let origin_view = self.active_view.ok_or("no active view")?;
        if self
            .tabs
            .get(origin_tab)
            .is_some_and(|tab| matches!(tab.root, LayoutNode::Pane(_)))
        {
            return Ok(());
        }
        let tab = self.new_tab()?;
        self.zoom = Some(Zoom {
            tab,
            origin_tab,
            origin_view,
        });
        Ok(())
    }

    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some_and(|zoom| self.active_tab == Some(zoom.tab))
    }

    #[must_use]
    pub fn active_tab_views(&self) -> Vec<ViewId> {
        self.active_tab
            .and_then(|tab| self.tabs.get(tab))
            .map_or_else(Vec::new, |tab| {
                pane_ids(&tab.root)
                    .into_iter()
                    .filter_map(|pane| self.panes.get(pane).map(|pane| pane.view))
                    .collect()
            })
    }

    pub fn focus_pane_id(&mut self, pane: PaneId) -> Result<(), &'static str> {
        let tab = self.active_tab.ok_or("no active tab")?;
        if !pane_ids(&self.tabs.get(tab).ok_or("active tab is stale")?.root).contains(&pane) {
            return Err("pane is absent from active tab");
        }
        self.set_focused_pane(tab, pane)
    }

    /// Whether the focused pane is the only one left, so closing it would close the editor.
    #[must_use]
    pub fn is_last_pane(&self) -> bool {
        self.tabs.len() == 1
            && self
                .active_tab
                .and_then(|tab| self.tabs.get(tab))
                .is_some_and(|tab| pane_ids(&tab.root).len() == 1)
    }

    pub fn focus_pane(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get(tab_id).ok_or("active tab is stale")?;
        let panes = pane_ids(&tab.root);
        let current = panes
            .iter()
            .position(|pane| *pane == tab.focused)
            .ok_or("focused pane is absent from layout")?;
        let target = offset_index(current, panes.len(), offset, wrap);
        self.set_focused_pane(tab_id, panes[target])
    }

    /// Return the spatially adjacent pane without mutating focus.
    pub fn directional_pane(
        &self,
        direction: FocusDirection,
    ) -> Result<Option<PaneId>, &'static str> {
        const EXTENT: u64 = 1_000_000;
        let tab = self
            .tabs
            .get(self.active_tab.ok_or("no active tab")?)
            .ok_or("active tab is stale")?;
        let mut rectangles = Vec::new();
        collect_focus_rectangles(
            &tab.root,
            FocusRectangle {
                x: 0,
                y: 0,
                width: EXTENT,
                height: EXTENT,
            },
            &mut rectangles,
        );
        let source = rectangles
            .iter()
            .find(|(pane, _)| *pane == tab.focused)
            .map(|(_, rectangle)| *rectangle)
            .ok_or("focused pane is absent from layout")?;
        Ok(rectangles
            .into_iter()
            .filter(|(pane, _)| *pane != tab.focused)
            .filter_map(|(pane, candidate)| {
                focus_score(source, candidate, direction)
                    .map(|score| (score, pane.data().as_ffi(), pane))
            })
            .min_by_key(|(score, stable_id, _)| (*score, *stable_id))
            .map(|(_, _, pane)| pane))
    }

    pub fn activate_tab(&mut self, tab: TabId) -> Result<(), &'static str> {
        let pane = self.tabs.get(tab).ok_or("target tab is stale")?.focused;
        self.active_tab = Some(tab);
        self.set_focused_pane(tab, pane)
    }

    pub fn focus_tab(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tabs = self.tabs.keys().collect::<Vec<_>>();
        let active = self.active_tab.ok_or("no active tab")?;
        let current = tabs
            .iter()
            .position(|tab| *tab == active)
            .ok_or("active tab is stale")?;
        let target = tabs[offset_index(current, tabs.len(), offset, wrap)];
        let pane = self.tabs.get(target).ok_or("target tab is stale")?.focused;
        self.active_tab = Some(target);
        self.set_focused_pane(target, pane)
    }

    pub fn resize_active_pane(&mut self, delta: i16) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        if resize_parent(&mut tab.root, tab.focused, delta) {
            self.mark_window_changed();
            Ok(())
        } else {
            Err("active pane has no resizable split")
        }
    }

    pub fn resize_active_pane_on_axis(
        &mut self,
        axis: Axis,
        delta: i16,
    ) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        if resize_parent_on_axis(&mut tab.root, tab.focused, axis, delta) {
            self.mark_window_changed();
            Ok(())
        } else {
            Err("active pane has no resizable split on that axis")
        }
    }

    /// Swap two pane leaves in the active tab without changing pane, view, or
    /// content identity. Focus follows the focused pane identity.
    pub fn swap_panes(&mut self, first: PaneId, second: PaneId) -> Result<(), &'static str> {
        if first == second {
            return Ok(());
        }
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        let leaves = pane_ids(&tab.root);
        if !leaves.contains(&first) || !leaves.contains(&second) {
            return Err("both panes must belong to the active tab");
        }
        if !swap_layout_leaves(&mut tab.root, first, second) {
            return Err("pane swap could not be applied");
        }
        self.mark_window_changed();
        Ok(())
    }

    pub(crate) fn close_view(&mut self, view: ViewId) -> Result<(), &'static str> {
        let (tab, pane) = self
            .tabs
            .iter()
            .find_map(|(tab_id, tab)| {
                pane_ids(&tab.root)
                    .into_iter()
                    .find(|pane| self.panes.get(*pane).is_some_and(|host| host.view == view))
                    .map(|pane| (tab_id, pane))
            })
            .ok_or("view is not hosted")?;
        self.active_tab = Some(tab);
        self.set_focused_pane(tab, pane)?;
        self.close_active_view().map(|_| ())
    }

    pub fn close_active_view(&mut self) -> Result<ViewId, &'static str> {
        let view = self.active_view.ok_or("no active view")?;
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let pane_count = pane_ids(&self.tabs.get(tab_id).ok_or("active tab is stale")?.root).len();
        if pane_count > 1 {
            self.close_active_pane()?;
        } else if self.tabs.len() > 1 {
            self.close_active_tab()?;
        } else {
            let tab = self.tabs.remove(tab_id).ok_or("active tab is stale")?;
            for pane_id in pane_ids(&tab.root) {
                self.panes.remove(pane_id);
            }
            self.views.remove(view);
            self.active_tab = None;
            self.active_view = None;
            self.mark_window_changed();
        }
        Ok(view)
    }

    pub fn close_active_pane(&mut self) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let focused = self.tabs.get(tab_id).ok_or("active tab is stale")?.focused;
        let pane_count = pane_ids(&self.tabs.get(tab_id).ok_or("active tab is stale")?.root).len();
        if pane_count == 1 {
            return self.close_active_tab();
        }
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        let new_root = remove_leaf(tab.root.clone(), focused).ok_or("cannot close pane")?;
        tab.root = new_root;
        let next = pane_ids(&tab.root)[0];
        tab.focused = next;
        if let Some(pane) = self.panes.remove(focused) {
            self.views.remove(pane.view);
        }
        self.set_focused_pane(tab_id, next)
    }

    pub fn close_active_tab(&mut self) -> Result<(), &'static str> {
        if self.tabs.len() <= 1 {
            return Err("cannot close the final tab");
        }
        let active = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.remove(active).ok_or("active tab is stale")?;
        for pane_id in pane_ids(&tab.root) {
            if let Some(pane) = self.panes.remove(pane_id) {
                self.views.remove(pane.view);
            }
        }
        let next = self.tabs.keys().next().ok_or("no remaining tab")?;
        let pane = self.tabs.get(next).ok_or("remaining tab is stale")?.focused;
        self.active_tab = Some(next);
        self.set_focused_pane(next, pane)
    }

    fn set_focused_pane(&mut self, tab: TabId, pane: PaneId) -> Result<(), &'static str> {
        let view = self.panes.get(pane).ok_or("target pane is stale")?.view;
        self.tabs.get_mut(tab).ok_or("target tab is stale")?.focused = pane;
        self.active_view = Some(view);
        self.mark_window_changed();
        Ok(())
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
            view.scroll_line = view
                .scroll_line
                .min(target.text.len_lines().saturating_sub(1));
            view.scroll_wrap_row = 0;
        }
        self.mark_window_changed();
        true
    }

    pub fn validate_layout(&self) -> Result<(), &'static str> {
        if self.panes.len() > MAXIMUM_LAYOUT_PANES {
            return Err("layout pane ceiling exceeded");
        }
        if self
            .tabs
            .values()
            .any(|tab| layout_depth(&tab.root) > MAXIMUM_LAYOUT_DEPTH)
        {
            return Err("layout depth ceiling exceeded");
        }
        if self
            .tabs
            .values()
            .any(|tab| !layout_ratios_are_valid(&tab.root))
        {
            return Err("layout split ratio is invalid");
        }
        if self.tabs.is_empty() {
            return if self.panes.is_empty()
                && self.views.is_empty()
                && self.active_tab.is_none()
                && self.active_view.is_none()
            {
                Ok(())
            } else {
                Err("empty layout retains pane/view state")
            };
        }
        let active_tab = self.active_tab.ok_or("tabs exist without an active tab")?;
        let active = self.tabs.get(active_tab).ok_or("active tab is stale")?;
        let mut referenced_panes = HashSet::new();
        let mut referenced_views = HashSet::new();
        for (_, tab) in &self.tabs {
            let leaves = pane_ids(&tab.root);
            if !leaves.contains(&tab.focused) {
                return Err("focused pane is absent from tab layout");
            }
            for pane_id in leaves {
                if !referenced_panes.insert(pane_id) {
                    return Err("pane appears more than once in layouts");
                }
                let pane = self.panes.get(pane_id).ok_or("layout pane is stale")?;
                if !referenced_views.insert(pane.view) {
                    return Err("view appears in more than one pane");
                }
                if !self.views.contains_key(pane.view) {
                    return Err("pane view is stale");
                }
            }
        }
        if referenced_panes.len() != self.panes.len() {
            return Err("orphan pane exists");
        }
        if referenced_views.len() != self.views.len() {
            return Err("orphan view exists");
        }
        let focused_view = self
            .panes
            .get(active.focused)
            .ok_or("active focused pane is stale")?
            .view;
        if self.active_view != Some(focused_view) {
            return Err("active view disagrees with active tab focus");
        }
        Ok(())
    }

    #[must_use]
    pub fn active_buffer_id(&self) -> Option<BufferId> {
        self.active_view
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
