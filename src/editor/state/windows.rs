//! Editor view lifetime and document selection in the window graph.

use crate::editor::state::store::{EditorState, Zoom};
use crate::editor::text::TextBuffer;
use crate::editor::{Buffer, BufferId, BufferOrigin, EditorMode, View, ViewId};
use crate::windows::{
    ContentAdapterKind, ContentId, ContentTypeId, SpatialDirection, TileAxis, TiledLayoutNode,
    WindowError, WindowId, WindowPlacement, WorkspaceTabDescriptor, WorkspaceTabId,
};

const OWNER: &str = "builtin.editor";

fn layout_error(error: WindowError) -> &'static str {
    match error {
        WindowError::TiledLayoutTooDeep => "layout depth limit reached",
        WindowError::TooManyTiledWindows => "layout pane limit reached",
        WindowError::WindowNotInTiledLayout | WindowError::MissingTiledLayout => {
            "active pane has no resizable split"
        }
        _ => "window operation failed",
    }
}

impl EditorState {
    pub fn editor_tabs(&self) -> impl Iterator<Item = &WorkspaceTabDescriptor> {
        self.graph
            .workspace_tabs()
            .filter(|tab| self.graph.last_focused(tab.id, OWNER).is_some())
    }

    pub fn active_tab(&self) -> Option<WorkspaceTabId> {
        self.graph
            .active_tab()
            .filter(|id| self.graph.last_focused(*id, OWNER).is_some())
    }

    pub fn editor_windows(&self) -> impl Iterator<Item = &crate::windows::WindowDescriptor> {
        self.graph.windows().filter(|window| window.owner == OWNER)
    }

    pub fn tab_windows(&self, tab: WorkspaceTabId) -> Vec<WindowId> {
        self.graph
            .ordered_windows(tab)
            .into_iter()
            .filter(|id| self.window_view(*id).is_some())
            .collect()
    }

    pub fn tab_layout(&self, tab: WorkspaceTabId) -> Option<TiledLayoutNode> {
        self.graph
            .logical_layout(tab)?
            .filtered(&|id| self.window_view(id).is_some())
    }

    pub fn active_view(&self) -> Option<ViewId> {
        self.window_view(self.focused_window()?)
    }

    pub fn focused_window(&self) -> Option<WindowId> {
        self.graph.last_focused(self.active_tab()?, OWNER)
    }

    pub fn tab_focused_window(&self, tab: WorkspaceTabId) -> Option<WindowId> {
        self.graph.last_focused(tab, OWNER)
    }

    pub fn window_view(&self, window: WindowId) -> Option<ViewId> {
        let graph_view = self.graph.window(window)?.view;
        self.graph_views
            .iter()
            .find_map(|(view, id)| (*id == graph_view).then_some(*view))
    }

    pub fn view_window(&self, view: ViewId) -> Option<WindowId> {
        let graph_view = self.graph_views.get(&view)?;
        self.graph
            .windows()
            .find(|window| window.view == *graph_view)
            .map(|window| window.id)
    }

    pub(crate) fn content_for_buffer(&self, buffer: BufferId) -> Option<ContentId> {
        self.buffer_contents.get(&buffer).copied()
    }
    pub(crate) fn buffer_for_content(&self, content: ContentId) -> Option<BufferId> {
        self.buffer_contents
            .iter()
            .find_map(|(buffer, id)| (*id == content).then_some(*buffer))
    }

    pub(crate) fn register_window_content(&mut self) -> Result<(), WindowError> {
        let stale = self
            .buffer_contents
            .iter()
            .filter(|(id, _)| !self.buffers.contains_key(**id))
            .map(|(id, content)| (*id, *content))
            .collect::<Vec<_>>();
        for (id, content) in stale {
            self.graph.remove_content(content)?;
            self.buffer_contents.remove(&id);
        }
        for (id, buffer) in &self.buffers {
            if !self.buffer_contents.contains_key(&id) {
                let adapter = if buffer.is_virtual_read_only() {
                    ContentAdapterKind::VirtualDocument
                } else {
                    ContentAdapterKind::TextBuffer
                };
                let content = self.graph.create_content(
                    ContentTypeId::new(if buffer.is_virtual_read_only() {
                        "builtin.virtual-document"
                    } else {
                        "builtin.text-buffer"
                    })?,
                    adapter,
                    String::new(),
                    buffer.dirty,
                    OWNER,
                    1,
                )?;
                self.buffer_contents.insert(id, content);
            }
        }
        Ok(())
    }

    pub(in crate::editor) fn register_view(
        &mut self,
        view: ViewId,
    ) -> Result<crate::windows::ViewId, &'static str> {
        if let Some(id) = self.graph_views.get(&view) {
            return Ok(*id);
        }
        self.register_window_content().map_err(layout_error)?;
        let buffer = self.views.get(view).ok_or("view is stale")?.buffer;
        let content = *self.buffer_contents.get(&buffer).ok_or("buffer is stale")?;
        let id = self
            .graph
            .create_view(content, OWNER, 1)
            .map_err(layout_error)?;
        self.graph_views.insert(view, id);
        Ok(id)
    }

    fn save_view(&mut self, mut view: View) {
        if let Some(buffer) = self.buffers.get_mut(view.buffer) {
            view.mode = EditorMode::Normal;
            view.selection = None;
            view.clear_editing_state();
            buffer.saved_view = Some(view);
        }
    }

    pub(super) fn remove_view(&mut self, view: ViewId) -> Result<(), &'static str> {
        if let Some(id) = self.graph_views.remove(&view) {
            self.graph.remove_view(id).map_err(layout_error)?;
        }
        if let Some(view) = self.views.remove(view)
            && !self.views.values().any(|other| other.buffer == view.buffer)
        {
            self.save_view(view);
        }
        Ok(())
    }

    pub(super) fn show_view(&mut self, view: ViewId) -> Result<(), &'static str> {
        if let Some(window) = self.view_window(view) {
            return self.select_editor_window(window);
        }
        self.leave_kept_pane(view)?;
        let graph_view = self.register_view(view)?;
        if let Some(window) = self.focused_window() {
            let old = self.window_view(window).ok_or("focused view is stale")?;
            let descriptor = self
                .graph
                .window(window)
                .ok_or("focused pane is stale")?
                .clone();
            self.graph
                .rehost(
                    window,
                    descriptor.tab,
                    graph_view,
                    descriptor.placement,
                    true,
                )
                .map_err(layout_error)?;
            let old_buffer = self.views[old].buffer;
            self.remove_view(old)?;
            let unused = self.buffers.get(old_buffer).is_some_and(|buffer| {
                buffer.path.is_none()
                    && !buffer.dirty
                    && buffer.text.len_bytes() == 0
                    && buffer.origin == BufferOrigin::FileOrScratch
            });
            if unused && !self.views.values().any(|other| other.buffer == old_buffer) {
                self.remove_buffer(old_buffer);
            }
            self.select_editor_window(window)?;
        } else {
            let tab = self
                .graph
                .create_workspace_tab(OWNER, 1)
                .map_err(layout_error)?;
            let window = self
                .graph
                .open_window(tab, graph_view, WindowPlacement::Tiled, true, OWNER, 1)
                .map_err(layout_error)?;
            self.select_editor_window(window)?;
        }
        Ok(())
    }

    fn select_editor_window(&mut self, window: WindowId) -> Result<(), &'static str> {
        self.window_view(window).ok_or("target pane is stale")?;
        self.graph.select_window(window).map_err(layout_error)?;
        self.mark_window_changed();
        Ok(())
    }

    pub fn replace_active_view(&mut self, buffer: Option<BufferId>) -> Result<(), &'static str> {
        let window = self.focused_window().ok_or("no active tab")?;
        let old = self.window_view(window).ok_or("focused pane is stale")?;
        let buffer = buffer.unwrap_or_else(|| {
            let buffer = self
                .buffers
                .insert(Buffer::new(TextBuffer::from_text(""), None));
            self.events
                .push(crate::editor::EditorEvent::BufferOpened { buffer });
            buffer
        });
        let view = self
            .views
            .insert(self.buffers[buffer].restored_view(buffer));
        let graph_view = self.register_view(view)?;
        let descriptor = self
            .graph
            .window(window)
            .ok_or("focused pane is stale")?
            .clone();
        self.graph
            .rehost(
                window,
                descriptor.tab,
                graph_view,
                descriptor.placement,
                true,
            )
            .map_err(layout_error)?;
        self.remove_view(old)?;
        self.select_editor_window(window)
    }

    fn leave_kept_pane(&mut self, view: ViewId) -> Result<(), &'static str> {
        let Some(kept) = self.kept_buffer else {
            return Ok(());
        };
        let Some(window) = self.focused_window() else {
            return Ok(());
        };
        let shows_kept = |state: &Self, window| {
            state
                .window_view(window)
                .and_then(|id| state.views.get(id))
                .is_some_and(|view| view.buffer == kept)
        };
        if !shows_kept(self, window) || self.views.get(view).is_some_and(|view| view.buffer == kept)
        {
            return Ok(());
        }
        let tab = self
            .graph
            .window(window)
            .ok_or("focused pane is stale")?
            .tab;
        if let Some(other) = self
            .tab_windows(tab)
            .into_iter()
            .find(|id| !shows_kept(self, *id))
        {
            return self.select_editor_window(other);
        }
        let original = self.window_view(window).ok_or("focused view is stale")?;
        let clone = self.clone_pane(window, TileAxis::Horizontal, 500)?;
        let right = self.view_window(clone).ok_or("cloned view is absent")?;
        // Keep the original editor view in the right-hand window, as before opening a file.
        let original_id = self.graph_views[&original];
        let clone_id = self.graph_views[&clone];
        let mut graph = self.graph.clone();
        graph.close_window(right).map_err(layout_error)?;
        graph
            .rehost(window, tab, clone_id, WindowPlacement::Tiled, true)
            .map_err(layout_error)?;
        let right = graph
            .open_window(tab, original_id, WindowPlacement::Tiled, true, OWNER, 1)
            .map_err(layout_error)?;
        graph
            .tile(right, Some(window), TileAxis::Horizontal, true, 500)
            .map_err(layout_error)?;
        self.graph = graph;
        self.select_editor_window(window)
    }

    pub fn split_active(&mut self, axis: TileAxis, ratio: u16) -> Result<ViewId, &'static str> {
        self.clone_pane(self.focused_window().ok_or("no active tab")?, axis, ratio)
    }

    pub fn clone_pane(
        &mut self,
        source: WindowId,
        axis: TileAxis,
        ratio: u16,
    ) -> Result<ViewId, &'static str> {
        if !(100..=900).contains(&ratio) {
            return Err("split ratio must be between 100 and 900");
        }
        self.graph.check_window_limit(OWNER).map_err(layout_error)?;
        let old = self.window_view(source).ok_or("source pane is stale")?;
        let tab = self.graph.window(source).ok_or("source pane is stale")?.tab;
        let before = self.graph.clone();
        let view = self
            .views
            .insert(self.views.get(old).ok_or("source view is stale")?.clone());
        let result = (|| {
            let graph_view = self.register_view(view)?;
            let window = self
                .graph
                .open_window(tab, graph_view, WindowPlacement::Hidden, true, OWNER, 1)
                .map_err(layout_error)?;
            self.graph
                .tile(window, Some(source), axis, true, ratio)
                .map_err(layout_error)?;
            self.select_editor_window(window)?;
            Ok(view)
        })();
        if result.is_err() {
            self.graph = before;
            self.graph_views.remove(&view);
            self.views.remove(view);
        }
        result
    }

    pub fn new_tab(&mut self) -> Result<WorkspaceTabId, &'static str> {
        self.graph.check_window_limit(OWNER).map_err(layout_error)?;
        let source = self.active_view().ok_or("no active view")?;
        let before = self.graph.clone();
        let view = self.views.insert(
            self.views
                .get(source)
                .ok_or("active view is stale")?
                .clone(),
        );
        let result = (|| {
            let graph_view = self.register_view(view)?;
            let tab = self
                .graph
                .create_workspace_tab(OWNER, 1)
                .map_err(layout_error)?;
            let window = self
                .graph
                .open_window(tab, graph_view, WindowPlacement::Tiled, true, OWNER, 1)
                .map_err(layout_error)?;
            self.select_editor_window(window)?;
            Ok(tab)
        })();
        if result.is_err() {
            self.graph = before;
            self.graph_views.remove(&view);
            self.views.remove(view);
        }
        result
    }

    pub fn toggle_zoom(&mut self) -> Result<(), &'static str> {
        if let Some(zoom) = self.zoom.take() {
            let origin = self.view_window(zoom.origin_view).filter(|window| {
                self.graph
                    .window(*window)
                    .is_some_and(|window| window.tab == zoom.origin_tab)
            });
            if self.active_tab() == Some(zoom.tab)
                && let Some(window) = origin
            {
                let zoomed = self
                    .active_view()
                    .and_then(|id| self.views.get(id))
                    .cloned();
                self.close_active_tab()?;
                if let Some(zoomed) = zoomed {
                    if self.views[zoom.origin_view].buffer == zoomed.buffer {
                        self.views[zoom.origin_view] = zoomed;
                    } else {
                        self.select_editor_window(window)?;
                        self.replace_active_view(Some(zoomed.buffer))?;
                        let view = self.active_view().ok_or("zoom view is absent")?;
                        self.views[view] = zoomed;
                    }
                }
                return self.select_editor_window(window);
            }
        }
        let origin_tab = self.active_tab().ok_or("no active tab")?;
        let origin_view = self.active_view().ok_or("no active view")?;
        if self.tab_windows(origin_tab).len() == 1 {
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

    pub fn is_zoomed(&self) -> bool {
        self.zoom
            .is_some_and(|zoom| self.active_tab() == Some(zoom.tab))
    }
    pub fn active_tab_views(&self) -> Vec<ViewId> {
        self.active_tab().map_or_else(Vec::new, |tab| {
            self.tab_windows(tab)
                .into_iter()
                .filter_map(|window| self.window_view(window))
                .collect()
        })
    }
    pub fn is_last_pane(&self) -> bool {
        self.editor_tabs().count() == 1
            && self
                .active_tab()
                .is_some_and(|tab| self.tab_windows(tab).len() == 1)
    }

    pub fn focus_pane_id(&mut self, window: WindowId) -> Result<(), &'static str> {
        if self
            .graph
            .window(window)
            .is_none_or(|window| Some(window.tab) != self.active_tab())
        {
            return Err("pane is absent from active tab");
        }
        self.select_editor_window(window)
    }
    pub fn focus_pane(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tab = self.active_tab().ok_or("no active tab")?;
        let windows = self.tab_windows(tab);
        let current = windows
            .iter()
            .position(|id| Some(*id) == self.focused_window())
            .ok_or("focused pane is absent from layout")?;
        self.select_editor_window(
            windows[crate::windows::offset_index(current, windows.len(), offset, wrap)],
        )
    }
    pub fn directional_pane(
        &self,
        direction: SpatialDirection,
    ) -> Result<Option<WindowId>, &'static str> {
        Ok(self.graph.directional_window(
            self.focused_window().ok_or("no active tab")?,
            direction,
            OWNER,
        ))
    }
    pub fn activate_tab(&mut self, tab: WorkspaceTabId) -> Result<(), &'static str> {
        self.select_editor_window(self.tab_focused_window(tab).ok_or("target tab is stale")?)
    }
    pub fn focus_tab(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tabs = self.editor_tabs().map(|tab| tab.id).collect::<Vec<_>>();
        let current = tabs
            .iter()
            .position(|id| Some(*id) == self.active_tab())
            .ok_or("active tab is stale")?;
        self.activate_tab(tabs[crate::windows::offset_index(current, tabs.len(), offset, wrap)])
    }
    pub fn resize_active_pane(&mut self, delta: i16) -> Result<(), &'static str> {
        self.graph
            .resize_window_split(self.focused_window().ok_or("no active tab")?, None, delta)
            .map_err(layout_error)?;
        self.mark_window_changed();
        Ok(())
    }
    pub fn resize_active_pane_on_axis(
        &mut self,
        axis: TileAxis,
        delta: i16,
    ) -> Result<(), &'static str> {
        self.graph
            .resize_window_split(
                self.focused_window().ok_or("no active tab")?,
                Some(axis),
                delta,
            )
            .map_err(|_| "active pane has no resizable split on that axis")?;
        self.mark_window_changed();
        Ok(())
    }
    pub fn panes_of_split_between(
        &self,
        first: WindowId,
        second: WindowId,
    ) -> Option<Vec<WindowId>> {
        self.graph.split_windows(first, second)
    }
    pub fn set_split_ratio_between(
        &mut self,
        first: WindowId,
        second: WindowId,
        ratio: u16,
    ) -> Result<(), &'static str> {
        self.graph
            .set_split_ratio(first, second, ratio)
            .map_err(|_| "no split between those panes")?;
        self.mark_window_changed();
        Ok(())
    }
    pub fn swap_panes(&mut self, first: WindowId, second: WindowId) -> Result<(), &'static str> {
        self.graph
            .swap_tiled_windows(self.active_tab().ok_or("no active tab")?, first, second)
            .map_err(|_| "both panes must belong to the active tab")?;
        self.mark_window_changed();
        Ok(())
    }
    pub(crate) fn close_view(&mut self, view: ViewId) -> Result<(), &'static str> {
        self.focus_view(view)?;
        self.close_active_view().map(|_| ())
    }
    pub(crate) fn focus_view(&mut self, view: ViewId) -> Result<(), &'static str> {
        self.select_editor_window(self.view_window(view).ok_or("view is not hosted")?)
    }

    fn close_window(&mut self, window: WindowId) -> Result<(), &'static str> {
        let view = self.window_view(window).ok_or("pane view is stale")?;
        self.graph.close_window(window).map_err(layout_error)?;
        self.remove_view(view)
    }
    fn close_editor_tab(
        &mut self,
        tab: WorkspaceTabId,
        next: Option<WorkspaceTabId>,
    ) -> Result<(), &'static str> {
        for window in self.tab_windows(tab) {
            self.close_window(window)?;
        }
        if let Some(next) = next {
            let native = self
                .graph
                .windows()
                .filter(|window| window.tab == tab)
                .cloned()
                .collect::<Vec<_>>();
            for window in native {
                self.graph
                    .rehost(
                        window.id,
                        next,
                        window.view,
                        window.placement,
                        window.focusable,
                    )
                    .map_err(layout_error)?;
            }
        }
        if !self.graph.tab_has_windows(tab) {
            self.graph.remove_workspace_tab(tab).map_err(layout_error)?;
        }
        Ok(())
    }
    pub fn close_active_view(&mut self) -> Result<ViewId, &'static str> {
        let view = self.active_view().ok_or("no active view")?;
        if !self.is_last_pane() {
            self.close_active_pane()?;
        } else {
            let tab = self.active_tab().ok_or("no active tab")?;
            self.close_editor_tab(tab, None)?;
            self.mark_window_changed();
        }
        Ok(view)
    }
    pub fn close_active_pane(&mut self) -> Result<(), &'static str> {
        let tab = self.active_tab().ok_or("no active tab")?;
        let focused = self.focused_window().ok_or("focused pane is stale")?;
        if self.tab_windows(tab).len() == 1 {
            return self.close_active_tab();
        }
        self.close_window(focused)?;
        self.select_editor_window(self.tab_windows(tab)[0])
    }
    pub fn close_active_tab(&mut self) -> Result<(), &'static str> {
        let tab = self.active_tab().ok_or("no active tab")?;
        let next = self
            .editor_tabs()
            .find(|other| other.id != tab)
            .map(|tab| tab.id)
            .ok_or("cannot close the final tab")?;
        let plugin = self.graph.focused().filter(|id| {
            self.graph
                .window(*id)
                .is_some_and(|window| window.owner.starts_with("plugin."))
        });
        self.close_editor_tab(tab, Some(next))?;
        self.activate_tab(next)?;
        if let Some(plugin) = plugin.filter(|id| self.graph.can_focus(*id)) {
            self.graph.focus(plugin).map_err(layout_error)?;
        }
        Ok(())
    }
    pub fn validate_layout(&self) -> Result<(), &'static str> {
        self.graph.validate().map_err(layout_error)?;
        let mut seen = std::collections::HashSet::new();
        for window in self.editor_windows() {
            let view = self.window_view(window.id).ok_or("pane view is stale")?;
            if !seen.insert(view) {
                return Err("view appears in more than one pane");
            }
            if !self.views.contains_key(view) {
                return Err("pane view is stale");
            }
        }
        if seen.len() != self.views.len() {
            return Err("orphan view exists");
        }
        Ok(())
    }
}
