use super::EditorTopology;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::Arc,
};

use crate::{
    editor::{BufferId, LayoutNode, PaneId, TabId, ViewId as EditorViewId},
    ui::PickerSurfaceKind,
    ui::window::{
        ContentAdapterKind, ContentContract, ContentId, ContentLifecycle, ContentTypeId, TileAxis,
        TiledLayoutNode, TiledLayoutSnapshot, ViewId, WindowError, WindowGraph, WindowId,
        WindowPlacement, WindowRect, WorkspaceTabId,
    },
};

const EDITOR_OWNER: &str = "builtin.editor";
const EDITOR_GENERATION: u64 = 1;
const MAXIMUM_BRIDGE_TITLE_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct EditorWindowBinding {
    pub window: WindowId,
    pub pane: PaneId,
    pub view: EditorViewId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct ActiveEditorWindowLayout {
    pub layout: TiledLayoutSnapshot,
    pub bindings: Vec<EditorWindowBinding>,
}

/// Exact signature plus stable identity correspondence while the legacy editor
/// mutation model is migrated into the universal graph. The graph owns layout
/// validation/projection; the maps adapt existing editor identifiers.
#[derive(Clone, Debug, Default)]
pub(in crate::editor::session) struct EditorWindowBridge {
    buffers: HashMap<BufferId, ContentId>,
    views: HashMap<EditorViewId, ViewId>,
    panes: HashMap<PaneId, WindowId>,
    desired_placements: HashMap<PaneId, WindowPlacement>,
    restore_placements: HashMap<PaneId, WindowPlacement>,
    tabs: HashMap<TabId, WorkspaceTabId>,
    source: Option<crate::editor::EditorWindowVersion>,
    scratch_label: Option<String>,
    #[cfg(test)]
    reconciliation_count: u64,
}

impl EditorWindowBridge {
    pub(super) fn source(&self) -> Option<&crate::editor::EditorWindowVersion> {
        self.source.as_ref()
    }
    pub(in crate::editor::session) fn is_current(
        &self,
        state: &EditorTopology<'_>,
        scratch_label: &str,
    ) -> bool {
        state.window_revision().is_some()
            && self.source.as_ref() == Some(state.version())
            && self.scratch_label.as_deref() == Some(scratch_label)
    }

    pub(super) fn reconcile(
        &mut self,
        state: &EditorTopology<'_>,
        graph: &mut WindowGraph,
        scratch_label: &str,
    ) -> Result<(), WindowError> {
        let source = state.version().clone();
        // Retire absent hosts first so a still-live view may be rehosted by a
        // replacement pane without ever owning two windows.
        let stale_panes = self
            .panes
            .iter()
            .filter_map(|(pane, window)| {
                (!state.panes().contains_key(*pane)).then_some((*pane, *window))
            })
            .collect::<Vec<_>>();
        for (pane, window) in stale_panes {
            graph.close_window(window)?;
            self.panes.remove(&pane);
        }

        let titles = editor_buffer_titles(state, scratch_label);
        let text_type = ContentTypeId::new("builtin.text-buffer")?;
        let virtual_type = ContentTypeId::new("builtin.virtual-document")?;
        for (buffer_id, buffer) in state.buffers() {
            let title = titles
                .get(&buffer_id)
                .cloned()
                .unwrap_or_else(|| sanitize_title(scratch_label));
            if let Some(content) = self.buffers.get(&buffer_id).copied() {
                graph.update_content(content, title, buffer.dirty)?;
            } else {
                let (content_type, adapter) = if buffer.is_virtual_read_only() {
                    (virtual_type.clone(), ContentAdapterKind::VirtualDocument)
                } else {
                    (text_type.clone(), ContentAdapterKind::TextBuffer)
                };
                let content = graph.create_content(
                    content_type,
                    adapter,
                    title,
                    buffer.dirty,
                    EDITOR_OWNER,
                    EDITOR_GENERATION,
                )?;
                self.buffers.insert(buffer_id, content);
            }
        }

        for (tab_id, _) in state.tabs() {
            if let std::collections::hash_map::Entry::Vacant(e) = self.tabs.entry(tab_id) {
                let tab = graph.create_workspace_tab(EDITOR_OWNER, EDITOR_GENERATION)?;
                e.insert(tab);
            }
        }

        for (editor_view, view) in state.views() {
            if !self.views.contains_key(&editor_view) {
                let content = self
                    .buffers
                    .get(&view.buffer)
                    .copied()
                    .ok_or(WindowError::UnknownContent)?;
                let universal_view = graph.create_view(content, EDITOR_OWNER, EDITOR_GENERATION)?;
                self.views.insert(editor_view, universal_view);
            }
        }

        for (tab_id, tab) in state.tabs() {
            let universal_tab = self.tabs[&tab_id];
            for pane_id in editor_panes(&tab.root) {
                let placement = self
                    .desired_placements
                    .get(&pane_id)
                    .copied()
                    .unwrap_or(WindowPlacement::Tiled);
                let pane = state
                    .panes()
                    .get(pane_id)
                    .ok_or(WindowError::UnknownWindow)?;
                let universal_view = self
                    .views
                    .get(&pane.view)
                    .copied()
                    .ok_or(WindowError::UnknownView)?;
                if let Some(window) = self.panes.get(&pane_id).copied() {
                    graph.rehost(window, universal_tab, universal_view, placement, true)?;
                } else {
                    let window = graph.open_window(
                        universal_tab,
                        universal_view,
                        placement,
                        true,
                        EDITOR_OWNER,
                        EDITOR_GENERATION,
                    )?;
                    self.panes.insert(pane_id, window);
                }
            }
        }

        self.desired_placements
            .retain(|pane, _| state.panes().contains_key(*pane));
        self.restore_placements
            .retain(|pane, _| state.panes().contains_key(*pane));
        for (tab_id, tab) in state.tabs() {
            let universal_tab = self.tabs[&tab_id];
            let editor = editor_tiled_layout(&tab.root, &self.panes, &self.desired_placements)?;
            let root = mixed_tiled_layout(graph, universal_tab, editor, &self.panes);
            graph.set_tiled_layout(universal_tab, root)?;
        }

        let stale_views = self
            .views
            .iter()
            .filter_map(|(view, universal)| {
                (!state.views().contains_key(*view)).then_some((*view, *universal))
            })
            .collect::<Vec<_>>();
        for (view, universal) in stale_views {
            graph.remove_view(universal)?;
            self.views.remove(&view);
        }

        let stale_tabs = self
            .tabs
            .iter()
            .filter_map(|(tab, universal)| {
                (!state.tabs().contains_key(*tab)).then_some((*tab, *universal))
            })
            .collect::<Vec<_>>();
        for (tab, universal) in stale_tabs {
            graph.remove_workspace_tab(universal)?;
            self.tabs.remove(&tab);
        }

        let stale_buffers = self
            .buffers
            .iter()
            .filter_map(|(buffer, content)| {
                (!state.buffers().contains_key(*buffer)).then_some((*buffer, *content))
            })
            .collect::<Vec<_>>();
        for (buffer, content) in stale_buffers {
            graph.remove_content(content)?;
            self.buffers.remove(&buffer);
        }

        if let Some(active_tab) = state.active_tab() {
            graph.set_active_tab(
                self.tabs
                    .get(&active_tab)
                    .copied()
                    .ok_or(WindowError::UnknownWorkspaceTab)?,
            )?;
        }
        if let Some(active_tab) = state.active_tab() {
            if let Some(tab) = state.tabs().get(active_tab) {
                if let Some(window) = self.panes.get(&tab.focused).copied() {
                    if graph
                        .window(window)
                        .is_some_and(|descriptor| descriptor.placement != WindowPlacement::Hidden)
                    {
                        graph.focus(window)?;
                    }
                }
            }
        }
        graph.validate()?;
        self.source = Some(source);
        self.scratch_label = Some(scratch_label.to_owned());
        #[cfg(test)]
        {
            self.reconciliation_count = self.reconciliation_count.saturating_add(1);
        }
        Ok(())
    }

    pub(super) fn restore_desired_placements(
        &mut self,
        state: &EditorTopology<'_>,
        graph: &mut WindowGraph,
        descriptors: &[(usize, usize, std::path::PathBuf, WindowPlacement)],
    ) -> Result<(), WindowError> {
        let mut planned = Vec::with_capacity(descriptors.len());
        for (tab_index, pane_index, path, placement) in descriptors {
            let (_, tab) = state
                .tabs()
                .iter()
                .nth(*tab_index)
                .ok_or(WindowError::UnknownWorkspaceTab)?;
            let pane = editor_panes(&tab.root)
                .get(*pane_index)
                .copied()
                .ok_or(WindowError::UnknownWindow)?;
            let editor_pane = state.panes().get(pane).ok_or(WindowError::UnknownWindow)?;
            let buffer = state
                .views()
                .get(editor_pane.view)
                .and_then(|view| state.buffers().get(view.buffer))
                .ok_or(WindowError::UnknownContent)?;
            if buffer.path.as_ref() != Some(path) {
                return Err(WindowError::InvalidContentType(
                    "stale persisted text descriptor".into(),
                ));
            }
            let window = self
                .panes
                .get(&pane)
                .copied()
                .ok_or(WindowError::UnknownWindow)?;
            let content = graph
                .content_for_window(window)
                .ok_or(WindowError::UnknownContent)?;
            graph.require_persist_descriptor(content.id)?;
            // Validate every desired placement before changing the candidate.
            placement.validate()?;
            planned.push((pane, window, *placement));
        }

        self.desired_placements.clear();
        self.restore_placements.clear();
        for (pane, window, placement) in planned {
            if placement != WindowPlacement::Tiled {
                self.desired_placements.insert(pane, placement);
                if placement != WindowPlacement::Hidden {
                    self.restore_placements.insert(pane, placement);
                }
            }
            graph.place(window, placement)?;
        }
        for (tab_id, tab) in state.tabs() {
            let universal_tab = self.tabs[&tab_id];
            let editor = editor_tiled_layout(&tab.root, &self.panes, &self.desired_placements)?;
            let root = mixed_tiled_layout(graph, universal_tab, editor, &self.panes);
            graph.set_tiled_layout(universal_tab, root)?;
        }
        graph.validate()
    }

    pub(in crate::editor::session) fn desired_placement(
        &self,
        pane: PaneId,
    ) -> Option<WindowPlacement> {
        self.desired_placements.get(&pane).copied()
    }

    pub(super) fn place_editor(
        &mut self,
        graph: &mut WindowGraph,
        pane: PaneId,
        expected_window: WindowId,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        if self.panes.get(&pane).copied() != Some(expected_window) {
            return Err(WindowError::UnknownWindow);
        }
        let current = graph
            .window(expected_window)
            .ok_or(WindowError::UnknownWindow)?
            .placement;
        if placement == WindowPlacement::Hidden && current != WindowPlacement::Hidden {
            self.restore_placements.insert(pane, current);
        }
        if placement == WindowPlacement::Tiled {
            self.desired_placements.remove(&pane);
        } else {
            self.desired_placements.insert(pane, placement);
            if placement != WindowPlacement::Hidden {
                self.restore_placements.insert(pane, placement);
            }
        }
        graph.place(expected_window, placement)
    }

    pub(super) fn invalidate_layout(&mut self) {
        // `WindowGraph::place` can only append a returning tile and does not
        // own its original split ratio. Reconciliation rebuilds the filtered
        // topology from the editor-owned logical layout.
        self.source = None;
    }

    pub(super) fn rebuild_tiled_layout(
        &self,
        state: &EditorTopology<'_>,
        graph: &mut WindowGraph,
    ) -> Result<(), WindowError> {
        for (tab_id, tab) in state.tabs() {
            let universal_tab = self
                .tabs
                .get(&tab_id)
                .copied()
                .ok_or(WindowError::UnknownWorkspaceTab)?;
            let editor = editor_tiled_layout(&tab.root, &self.panes, &self.desired_placements)?;
            let root = mixed_tiled_layout(graph, universal_tab, editor, &self.panes);
            graph.set_tiled_layout(universal_tab, root)?;
        }
        Ok(())
    }

    pub(super) fn swap_editors(
        &mut self,
        graph: &mut WindowGraph,
        first: (PaneId, WindowId),
        second: (PaneId, WindowId),
    ) -> Result<(), WindowError> {
        if self.panes.get(&first.0).copied() != Some(first.1)
            || self.panes.get(&second.0).copied() != Some(second.1)
        {
            return Err(WindowError::UnknownWindow);
        }
        let first_placement = graph
            .window(first.1)
            .ok_or(WindowError::UnknownWindow)?
            .placement;
        let second_placement = graph
            .window(second.1)
            .ok_or(WindowError::UnknownWindow)?
            .placement;
        graph.swap_non_tiled_placements(first.1, second.1)?;
        for (pane, previous, placement) in [
            (first.0, first_placement, second_placement),
            (second.0, second_placement, first_placement),
        ] {
            self.desired_placements.insert(pane, placement);
            if placement == WindowPlacement::Hidden {
                if previous != WindowPlacement::Hidden {
                    self.restore_placements.insert(pane, previous);
                }
            } else {
                self.restore_placements.insert(pane, placement);
            }
        }
        Ok(())
    }

    pub(super) fn show_editor(
        &mut self,
        graph: &mut WindowGraph,
        pane: PaneId,
        expected_window: WindowId,
    ) -> Result<(), WindowError> {
        if graph
            .window(expected_window)
            .is_none_or(|window| window.placement != WindowPlacement::Hidden)
        {
            return Err(WindowError::UnknownWindow);
        }
        let placement = self
            .restore_placements
            .get(&pane)
            .copied()
            .unwrap_or(WindowPlacement::Tiled);
        self.place_editor(graph, pane, expected_window, placement)
    }

    pub(in crate::editor::session) fn active_layout(
        &self,
        state: &EditorTopology<'_>,
        graph: &WindowGraph,
    ) -> Option<ActiveEditorWindowLayout> {
        let editor_tab = state.active_tab()?;
        let tab = self.tabs.get(&editor_tab).copied()?;
        let editor = state.tabs().get(editor_tab)?;
        let layout = TiledLayoutSnapshot {
            root: editor_logical_layout(&editor.root, &self.panes).ok()?,
            focused: graph.focused().filter(|window| {
                graph
                    .window(*window)
                    .is_some_and(|descriptor| descriptor.tab == tab)
            }),
        };
        let bindings = editor_panes(&editor.root)
            .into_iter()
            .filter_map(|pane| {
                Some(EditorWindowBinding {
                    window: *self.panes.get(&pane)?,
                    pane,
                    view: state.panes().get(pane)?.view,
                })
            })
            .collect();
        Some(ActiveEditorWindowLayout { layout, bindings })
    }

    #[cfg(test)]
    pub(super) fn reset(&mut self, graph: &mut WindowGraph) -> Result<(), WindowError> {
        graph.remove_owner_generation(EDITOR_OWNER, EDITOR_GENERATION)?;
        *self = Self::default();
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::editor::session) const fn reconciliation_count(&self) -> u64 {
        self.reconciliation_count
    }

    pub(in crate::editor::session) fn active_tab(
        &self,
        state: &EditorTopology,
    ) -> Option<WorkspaceTabId> {
        self.tabs.get(&state.active_tab()?).copied()
    }

    pub(in crate::editor::session) fn focused_window(
        &self,
        state: &EditorTopology,
    ) -> Option<WindowId> {
        let tab = state.tabs().get(state.active_tab()?)?;
        self.panes.get(&tab.focused).copied()
    }

    pub(in crate::editor::session) fn buffer_for_content(
        &self,
        content: ContentId,
    ) -> Option<BufferId> {
        self.buffers
            .iter()
            .find_map(|(buffer, candidate)| (*candidate == content).then_some(*buffer))
    }

    pub(in crate::editor::session) fn content_for(&self, buffer: BufferId) -> Option<ContentId> {
        self.buffers.get(&buffer).copied()
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn view_for(&self, view: EditorViewId) -> Option<ViewId> {
        self.views.get(&view).copied()
    }

    pub(in crate::editor::session) fn window_for(&self, pane: PaneId) -> Option<WindowId> {
        self.panes.get(&pane).copied()
    }

    pub(in crate::editor::session) fn pane_for_window(&self, window: WindowId) -> Option<PaneId> {
        self.panes
            .iter()
            .find_map(|(pane, candidate)| (*candidate == window).then_some(*pane))
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn tab_for(&self, tab: TabId) -> Option<WorkspaceTabId> {
        self.tabs.get(&tab).copied()
    }

    pub(in crate::editor::session) fn editor_tab_for(&self, tab: WorkspaceTabId) -> Option<TabId> {
        self.tabs
            .iter()
            .find_map(|(editor, candidate)| (*candidate == tab).then_some(*editor))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct NativeSurfaceSignature {
    pub editor_tab: Option<WorkspaceTabId>,
    pub editor_window: Option<WindowId>,
    pub explorer_visible: bool,
    pub explorer_views: Arc<[u64]>,
    pub focused_explorer: Option<u64>,
    pub explorer_size: u16,
    pub active_terminal: Option<u64>,
    /// Terminals that keep their windows while another is active: the panel's terminal and side
    /// terminals such as a docked agent.
    pub shown_terminals: Arc<[u64]>,
    pub terminal_focused: bool,
    pub terminal_size: u16,
    pub terminal_set_revision: Option<u64>,
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub dashboard_visible: bool,
    pub picker_rect: Option<crate::ui::window::WindowRect>,
    pub picker_surface: Option<PickerSurfaceKind>,
    pub protected_prompt_height: u16,
    pub configuration_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct NativeSurfaceIds {
    pub content: ContentId,
    pub view: ViewId,
    pub window: WindowId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum NativeSurfaceTarget {
    Explorer,
    ExplorerView(u64),
    Terminal(u64),
    Dashboard,
    Picker(PickerSurfaceKind),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::editor::session) struct NativeSurfaceBridge {
    explorer: Option<NativeSurfaceIds>,
    explorer_views: BTreeMap<u64, NativeSurfaceIds>,
    dashboard: Option<NativeSurfaceIds>,
    pickers: BTreeMap<PickerSurfaceKind, NativeSurfaceIds>,
    terminals: HashMap<u64, NativeSurfaceIds>,
    explorer_placement: Option<WindowPlacement>,
    explorer_restore_placement: Option<WindowPlacement>,
    explorer_view_placements: BTreeMap<u64, WindowPlacement>,
    explorer_view_restore_placements: BTreeMap<u64, WindowPlacement>,
    explorer_closed: bool,
    dashboard_placement: Option<WindowPlacement>,
    dashboard_restore_placement: Option<WindowPlacement>,
    picker_placements: BTreeMap<PickerSurfaceKind, WindowPlacement>,
    picker_restore_placements: BTreeMap<PickerSurfaceKind, WindowPlacement>,
    terminal_default_placement: Option<WindowPlacement>,
    terminal_placements: HashMap<u64, WindowPlacement>,
    terminal_restore_placements: HashMap<u64, WindowPlacement>,
    configured_placements: BTreeMap<String, WindowPlacement>,
    workspace_tab: Option<WorkspaceTabId>,
    signature: Option<NativeSurfaceSignature>,
}

impl NativeSurfaceBridge {
    pub(in crate::editor::session) fn is_current(
        &self,
        signature: &NativeSurfaceSignature,
    ) -> bool {
        signature.terminal_set_revision.is_some() && self.signature.as_ref() == Some(signature)
    }

    pub(super) fn invalidate(&mut self) {
        self.signature = None;
    }

    pub(super) fn reconcile(
        &mut self,
        graph: &mut WindowGraph,
        signature: NativeSurfaceSignature,
        explorer_title: &str,
        dashboard_title: &str,
        picker_title: &str,
        terminals: &[(u64, String, ContentLifecycle)],
    ) -> Result<(), WindowError> {
        let owner = "builtin.surfaces";
        let generation = 1;
        // Hidden retained native windows still need a host while editor tabs
        // are replaced; skipping them leaves stale tabs non-empty.
        if self.workspace_tab.is_none()
            && signature.editor_tab.is_none()
            && !signature.explorer_visible
            && !signature.dashboard_visible
            && signature.picker_rect.is_none()
            && terminals.is_empty()
        {
            self.signature = Some(signature);
            return Ok(());
        }
        if signature.explorer_views.first().copied() != Some(1)
            || !signature
                .explorer_views
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        {
            return Err(WindowError::UnknownView);
        }
        let live_ids = |ids: NativeSurfaceIds, graph: &WindowGraph| {
            graph.content(ids.content).is_some()
                && graph.view(ids.view).is_some()
                && graph.window(ids.window).is_some()
        };
        self.explorer = self.explorer.filter(|ids| live_ids(*ids, graph));
        self.explorer_views.retain(|_, ids| live_ids(*ids, graph));
        let semantic_explorer_views = signature
            .explorer_views
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let retired_explorer_views = self
            .explorer_views
            .iter()
            .filter_map(|(view, ids)| {
                (!semantic_explorer_views.contains(view)).then_some((*view, *ids))
            })
            .collect::<Vec<_>>();
        for (view, ids) in retired_explorer_views {
            graph.close_window(ids.window)?;
            graph.remove_view(ids.view)?;
            self.explorer_views.remove(&view);
            self.explorer_view_placements.remove(&view);
            self.explorer_view_restore_placements.remove(&view);
        }
        self.dashboard = self.dashboard.filter(|ids| live_ids(*ids, graph));
        self.pickers.retain(|_, ids| live_ids(*ids, graph));
        self.terminals.retain(|_, ids| live_ids(*ids, graph));

        let previous_tab = self.workspace_tab;
        let retained_plugin_focus = graph.focused().filter(|window| {
            graph.window(*window).is_some_and(|descriptor| {
                descriptor.owner.starts_with("plugin.")
                    && descriptor.focusable
                    && descriptor.placement != WindowPlacement::Hidden
            })
        });
        let retained_tab = self.workspace_tab.filter(|tab| {
            graph.workspace_tab(*tab).is_some_and(|descriptor| {
                signature.editor_tab.is_some() || descriptor.owner == owner
            })
        });
        let tab = if let Some(tab) = signature.editor_tab.or(retained_tab) {
            tab
        } else {
            let tab = graph.create_workspace_tab(owner, generation)?;
            self.workspace_tab = Some(tab);
            tab
        };
        self.workspace_tab = Some(tab);
        let default_explorer_placement = WindowPlacement::Docked {
            edge: crate::ui::window::DockEdge::Left,
            size: signature.explorer_size.max(1),
        };
        let explorer_placement = if signature.explorer_visible {
            self.explorer_placement
                .or_else(|| self.configured_placements.get("builtin.explorer").copied())
                .unwrap_or(default_explorer_placement)
        } else {
            WindowPlacement::Hidden
        };
        let explorer = if self.explorer_closed {
            None
        } else if let Some(ids) = self.explorer {
            graph.update_content(ids.content, explorer_title.to_owned(), false)?;
            graph.rehost(ids.window, tab, ids.view, explorer_placement, true)?;
            Some(ids)
        } else {
            let mut capabilities = crate::ui::window::ContentCapabilities::for_adapter(
                ContentAdapterKind::DeclarativeUi,
            );
            capabilities.clone_view = true;
            let content = graph.create_content_with_contract(
                ContentTypeId::new("builtin.explorer")?,
                ContentContract::declared(ContentAdapterKind::DeclarativeUi, capabilities),
                explorer_title.to_owned(),
                false,
                owner,
                generation,
            )?;
            let view = graph.create_view(content, owner, generation)?;
            let window =
                graph.open_window(tab, view, explorer_placement, true, owner, generation)?;
            let ids = NativeSurfaceIds {
                content,
                view,
                window,
            };
            self.explorer = Some(ids);
            Some(ids)
        };
        if let Some(explorer) = explorer {
            for view in signature
                .explorer_views
                .iter()
                .copied()
                .filter(|view| *view > 1)
            {
                let placement = self.explorer_view_placements.get(&view).copied().unwrap_or(
                    WindowPlacement::Floating(WindowRect {
                        x: 2,
                        y: 2,
                        width: 40,
                        height: 20,
                    }),
                );
                if let Some(ids) = self.explorer_views.get(&view).copied() {
                    graph.rehost(ids.window, tab, ids.view, placement, true)?;
                } else {
                    let cloned_view = graph.create_view(explorer.content, owner, generation)?;
                    let window =
                        graph.open_window(tab, cloned_view, placement, true, owner, generation)?;
                    self.explorer_views.insert(
                        view,
                        NativeSurfaceIds {
                            content: explorer.content,
                            view: cloned_view,
                            window,
                        },
                    );
                    self.explorer_view_placements.insert(view, placement);
                    self.explorer_view_restore_placements
                        .insert(view, placement);
                }
            }
        }

        let dashboard_placement = if signature.dashboard_visible {
            self.dashboard_placement
                .or_else(|| self.configured_placements.get("builtin.dashboard").copied())
                .unwrap_or(WindowPlacement::Overlay(crate::ui::window::WindowRect {
                    x: 0,
                    y: 0,
                    width: signature.terminal_width.max(1),
                    height: signature.terminal_height.max(1),
                }))
        } else {
            WindowPlacement::Hidden
        };
        let dashboard = if let Some(ids) = self.dashboard {
            graph.update_content(ids.content, dashboard_title.to_owned(), false)?;
            graph.rehost(ids.window, tab, ids.view, dashboard_placement, true)?;
            Some(ids)
        } else if signature.dashboard_visible {
            let content = graph.create_content(
                ContentTypeId::new("builtin.dashboard")?,
                ContentAdapterKind::DeclarativeUi,
                dashboard_title.to_owned(),
                false,
                owner,
                generation,
            )?;
            let view = graph.create_view(content, owner, generation)?;
            let window =
                graph.open_window(tab, view, dashboard_placement, true, owner, generation)?;
            let ids = NativeSurfaceIds {
                content,
                view,
                window,
            };
            self.dashboard = Some(ids);
            Some(ids)
        } else {
            None
        };

        let active_picker = signature.picker_rect.zip(signature.picker_surface);
        for (kind, ids) in &self.pickers {
            if active_picker.is_none_or(|(_, active)| active != *kind) {
                graph.rehost(ids.window, tab, ids.view, WindowPlacement::Hidden, true)?;
            }
        }
        let picker = if let Some((rect, kind)) = active_picker {
            let content_type = match kind {
                PickerSurfaceKind::Generic | PickerSurfaceKind::Choice => "builtin.picker",
                PickerSurfaceKind::Problems => "builtin.problems",
                PickerSurfaceKind::Output => "builtin.output",
                PickerSurfaceKind::Logs => "builtin.logs",
                PickerSurfaceKind::Debug => "builtin.debug",
                PickerSurfaceKind::Theme => "builtin.theme",
                PickerSurfaceKind::Changes => "builtin.changes",
            };
            let placement = self
                .picker_placements
                .get(&kind)
                .copied()
                .or_else(|| self.configured_placements.get(content_type).copied())
                .unwrap_or(WindowPlacement::Overlay(rect));
            if let Some(ids) = self.pickers.get(&kind).copied() {
                graph.update_content(ids.content, picker_title.to_owned(), false)?;
                graph.rehost(ids.window, tab, ids.view, placement, true)?;
                Some(ids)
            } else {
                let content = graph.create_content(
                    ContentTypeId::new(content_type)?,
                    ContentAdapterKind::DeclarativeUi,
                    picker_title.to_owned(),
                    false,
                    owner,
                    generation,
                )?;
                let view = graph.create_view(content, owner, generation)?;
                let window = graph.open_window(tab, view, placement, true, owner, generation)?;
                let ids = NativeSurfaceIds {
                    content,
                    view,
                    window,
                };
                self.pickers.insert(kind, ids);
                Some(ids)
            }
        } else {
            None
        };

        let live = terminals
            .iter()
            .map(|(id, ..)| *id)
            .collect::<std::collections::BTreeSet<_>>();
        let stale = self
            .terminals
            .iter()
            .filter_map(|(id, ids)| (!live.contains(id)).then_some((*id, *ids)))
            .collect::<Vec<_>>();
        for (id, ids) in stale {
            graph.close_window(ids.window)?;
            graph.remove_view(ids.view)?;
            graph.remove_content(ids.content)?;
            self.terminals.remove(&id);
            self.terminal_placements.remove(&id);
            self.terminal_restore_placements.remove(&id);
        }
        for (id, profile, lifecycle) in terminals {
            let placement = if signature.active_terminal == Some(*id)
                || signature.shown_terminals.contains(id)
            {
                self.terminal_placements
                    .get(id)
                    .copied()
                    .or(self.terminal_default_placement)
                    .or_else(|| self.configured_placements.get("builtin.terminal").copied())
                    .unwrap_or(WindowPlacement::Docked {
                        edge: crate::ui::window::DockEdge::Bottom,
                        size: signature.terminal_size.max(1),
                    })
            } else {
                WindowPlacement::Hidden
            };
            if let Some(ids) = self.terminals.get(id).copied() {
                graph.update_content_lifecycle(ids.content, profile.clone(), false, *lifecycle)?;
                graph.rehost(ids.window, tab, ids.view, placement, true)?;
            } else {
                let content = graph.create_content_with_contract(
                    ContentTypeId::new("builtin.terminal")?,
                    ContentContract::adapter_defaults(ContentAdapterKind::TerminalSession)
                        .with_lifecycle(*lifecycle),
                    profile.clone(),
                    false,
                    owner,
                    generation,
                )?;
                let view = graph.create_view(content, owner, generation)?;
                let window = graph.open_window(tab, view, placement, true, owner, generation)?;
                self.terminals.insert(
                    *id,
                    NativeSurfaceIds {
                        content,
                        view,
                        window,
                    },
                );
            }
        }

        let mut focused = false;
        if signature.picker_rect.is_some() {
            if let Some(picker) = picker {
                if graph
                    .window(picker.window)
                    .is_some_and(|window| window.placement != WindowPlacement::Hidden)
                {
                    graph.focus(picker.window)?;
                    focused = true;
                }
            }
        }
        if !focused && signature.terminal_focused {
            if let Some(ids) = signature
                .active_terminal
                .and_then(|id| self.terminals.get(&id))
            {
                graph.focus(ids.window)?;
                focused = true;
            }
        }
        if !focused {
            if let Some(view) = signature.focused_explorer {
                let ids = if view == 1 {
                    explorer
                } else {
                    self.explorer_views.get(&view).copied()
                };
                if let Some(ids) = ids {
                    graph.focus(ids.window)?;
                }
            } else if let Some(editor) = signature.editor_window {
                if graph
                    .window(editor)
                    .is_some_and(|window| window.placement != WindowPlacement::Hidden)
                {
                    graph.focus(editor)?;
                } else if signature.dashboard_visible {
                    if let Some(dashboard) = dashboard {
                        graph.focus(dashboard.window)?;
                    }
                }
            } else if signature.dashboard_visible {
                if let Some(dashboard) = dashboard {
                    graph.focus(dashboard.window)?;
                }
            }
        }
        if signature.picker_rect.is_none()
            && !signature.terminal_focused
            && signature.focused_explorer.is_none()
        {
            if let Some(plugin) = retained_plugin_focus {
                if graph
                    .window(plugin)
                    .is_some_and(|descriptor| descriptor.tab == tab)
                {
                    graph.focus(plugin)?;
                }
            }
        }
        if signature.editor_tab == Some(tab) {
            self.workspace_tab = Some(tab);
        }
        if let Some(previous) = previous_tab.filter(|previous| *previous != tab) {
            if graph
                .workspace_tab(previous)
                .is_some_and(|_| !graph.tab_has_windows(previous))
            {
                graph.remove_workspace_tab(previous)?;
            }
        }
        graph.validate()?;
        self.signature = Some(signature);
        Ok(())
    }

    pub(in crate::editor::session) fn workspace_tab(&self) -> Option<WorkspaceTabId> {
        self.workspace_tab
    }

    pub(in crate::editor::session) fn explorer(&self) -> Option<NativeSurfaceIds> {
        self.explorer
    }

    pub(in crate::editor::session) fn explorer_view(&self, view: u64) -> Option<NativeSurfaceIds> {
        if view == 1 {
            self.explorer
        } else {
            self.explorer_views.get(&view).copied()
        }
    }

    pub(in crate::editor::session) fn explorer_view_count(&self) -> usize {
        usize::from(self.explorer.is_some()).saturating_add(self.explorer_views.len())
    }

    pub(in crate::editor::session) fn explorer_views(
        &self,
    ) -> impl Iterator<Item = (u64, NativeSurfaceIds)> + '_ {
        self.explorer
            .map(|ids| (1, ids))
            .into_iter()
            .chain(self.explorer_views.iter().map(|(view, ids)| (*view, *ids)))
    }

    pub(in crate::editor::session) fn dashboard(&self) -> Option<NativeSurfaceIds> {
        self.dashboard
    }

    pub(in crate::editor::session) fn picker(&self) -> Option<NativeSurfaceIds> {
        self.signature
            .as_ref()
            .and_then(|signature| signature.picker_surface)
            .and_then(|kind| self.pickers.get(&kind).copied())
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn picker_for(
        &self,
        kind: PickerSurfaceKind,
    ) -> Option<NativeSurfaceIds> {
        self.pickers.get(&kind).copied()
    }

    pub(in crate::editor::session) fn terminal(&self, id: u64) -> Option<NativeSurfaceIds> {
        self.terminals.get(&id).copied()
    }

    pub(in crate::editor::session) fn terminal_placement(
        &self,
        id: u64,
    ) -> Option<WindowPlacement> {
        self.terminal_placements
            .get(&id)
            .copied()
            .or(self.terminal_default_placement)
    }

    pub(in crate::editor::session) fn target_for_window(
        &self,
        window: WindowId,
    ) -> Option<NativeSurfaceTarget> {
        if self.explorer.is_some_and(|ids| ids.window == window) {
            return Some(NativeSurfaceTarget::Explorer);
        }
        if let Some(view) = self
            .explorer_views
            .iter()
            .find_map(|(view, ids)| (ids.window == window).then_some(*view))
        {
            return Some(NativeSurfaceTarget::ExplorerView(view));
        }
        if self.dashboard.is_some_and(|ids| ids.window == window) {
            return Some(NativeSurfaceTarget::Dashboard);
        }
        if let Some(target) = self.pickers.iter().find_map(|(kind, ids)| {
            (ids.window == window).then_some(NativeSurfaceTarget::Picker(*kind))
        }) {
            return Some(target);
        }
        self.terminals.iter().find_map(|(id, ids)| {
            (ids.window == window).then_some(NativeSurfaceTarget::Terminal(*id))
        })
    }

    pub(in crate::editor::session) fn terminal_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.terminals.keys().copied()
    }

    pub(in crate::editor::session) fn ids_for_target(
        &self,
        target: NativeSurfaceTarget,
    ) -> Option<NativeSurfaceIds> {
        match target {
            NativeSurfaceTarget::Explorer => self.explorer,
            NativeSurfaceTarget::ExplorerView(view) => self.explorer_views.get(&view).copied(),
            NativeSurfaceTarget::Terminal(id) => self.terminals.get(&id).copied(),
            NativeSurfaceTarget::Dashboard => self.dashboard,
            NativeSurfaceTarget::Picker(kind) => self.pickers.get(&kind).copied(),
        }
    }

    pub(in crate::editor::session) fn placement_for_target(
        &self,
        target: NativeSurfaceTarget,
    ) -> Option<WindowPlacement> {
        match target {
            NativeSurfaceTarget::Explorer => self.explorer_placement,
            NativeSurfaceTarget::ExplorerView(view) => {
                self.explorer_view_placements.get(&view).copied()
            }
            NativeSurfaceTarget::Terminal(id) => self.terminal_placement(id),
            NativeSurfaceTarget::Dashboard => self.dashboard_placement,
            NativeSurfaceTarget::Picker(kind) => self.picker_placements.get(&kind).copied(),
        }
    }

    pub(super) fn place_target(
        &mut self,
        target: NativeSurfaceTarget,
        placement: WindowPlacement,
    ) -> Result<NativeSurfaceIds, WindowError> {
        placement.validate()?;
        let ids = match target {
            NativeSurfaceTarget::Explorer => {
                let ids = self.explorer.ok_or(WindowError::UnknownWindow)?;
                if placement == WindowPlacement::Hidden {
                    if let Some(current) = self.explorer_placement {
                        if current != WindowPlacement::Hidden {
                            self.explorer_restore_placement = Some(current);
                        }
                    }
                } else {
                    self.explorer_restore_placement = Some(placement);
                }
                self.explorer_placement = Some(placement);
                ids
            }
            NativeSurfaceTarget::ExplorerView(view) => {
                let ids = self
                    .explorer_views
                    .get(&view)
                    .copied()
                    .ok_or(WindowError::UnknownWindow)?;
                if placement == WindowPlacement::Hidden {
                    if let Some(current) = self.explorer_view_placements.get(&view).copied() {
                        if current != WindowPlacement::Hidden {
                            self.explorer_view_restore_placements.insert(view, current);
                        }
                    }
                } else {
                    self.explorer_view_restore_placements
                        .insert(view, placement);
                }
                self.explorer_view_placements.insert(view, placement);
                ids
            }
            NativeSurfaceTarget::Terminal(id) => {
                return self.place_terminal(id, placement);
            }
            NativeSurfaceTarget::Dashboard => {
                let ids = self.dashboard.ok_or(WindowError::UnknownWindow)?;
                if placement != WindowPlacement::Hidden {
                    self.dashboard_restore_placement = Some(placement);
                }
                self.dashboard_placement = Some(placement);
                ids
            }
            NativeSurfaceTarget::Picker(kind) => {
                let ids = self
                    .pickers
                    .get(&kind)
                    .copied()
                    .ok_or(WindowError::UnknownWindow)?;
                if placement != WindowPlacement::Hidden {
                    self.picker_restore_placements.insert(kind, placement);
                }
                self.picker_placements.insert(kind, placement);
                ids
            }
        };
        self.signature = None;
        Ok(ids)
    }

    pub(super) fn show_target(
        &mut self,
        target: NativeSurfaceTarget,
        explorer_fallback: WindowPlacement,
        terminal_fallback: WindowPlacement,
    ) -> Result<NativeSurfaceIds, WindowError> {
        let placement = match target {
            NativeSurfaceTarget::Explorer => {
                self.explorer_restore_placement.unwrap_or(explorer_fallback)
            }
            NativeSurfaceTarget::ExplorerView(view) => self
                .explorer_view_restore_placements
                .get(&view)
                .copied()
                .unwrap_or(explorer_fallback),
            NativeSurfaceTarget::Terminal(id) => self
                .terminal_restore_placements
                .get(&id)
                .copied()
                .or(self.terminal_default_placement)
                .unwrap_or(terminal_fallback),
            NativeSurfaceTarget::Dashboard => self
                .dashboard_restore_placement
                .unwrap_or(WindowPlacement::Fullscreen),
            NativeSurfaceTarget::Picker(kind) => self
                .picker_restore_placements
                .get(&kind)
                .copied()
                .unwrap_or(WindowPlacement::Floating(crate::ui::window::WindowRect {
                    x: 1,
                    y: 1,
                    width: 60,
                    height: 20,
                })),
        };
        self.place_target(target, placement)
    }

    pub(super) fn close_target(
        &mut self,
        graph: &mut WindowGraph,
        target: NativeSurfaceTarget,
    ) -> Result<NativeSurfaceIds, WindowError> {
        let ids = match target {
            NativeSurfaceTarget::Explorer => self.explorer.ok_or(WindowError::UnknownWindow)?,
            NativeSurfaceTarget::ExplorerView(view) => self
                .explorer_views
                .get(&view)
                .copied()
                .ok_or(WindowError::UnknownWindow)?,
            NativeSurfaceTarget::Terminal(id) => self
                .terminals
                .get(&id)
                .copied()
                .ok_or(WindowError::UnknownWindow)?,
            NativeSurfaceTarget::Dashboard => self.dashboard.ok_or(WindowError::UnknownWindow)?,
            NativeSurfaceTarget::Picker(kind) => self
                .pickers
                .get(&kind)
                .copied()
                .ok_or(WindowError::UnknownWindow)?,
        };
        graph.close_window(ids.window)?;
        graph.remove_view(ids.view)?;
        if graph
            .content_snapshots()
            .into_iter()
            .find(|content| content.id == ids.content)
            .is_some_and(|content| content.view_count == 0)
        {
            graph.remove_content(ids.content)?;
        }
        match target {
            NativeSurfaceTarget::Explorer => {
                self.explorer = None;
                self.explorer_closed = true;
            }
            NativeSurfaceTarget::ExplorerView(view) => {
                self.explorer_views.remove(&view);
                self.explorer_view_placements.remove(&view);
                self.explorer_view_restore_placements.remove(&view);
            }
            NativeSurfaceTarget::Terminal(id) => {
                self.terminals.remove(&id);
                self.terminal_placements.remove(&id);
                self.terminal_restore_placements.remove(&id);
            }
            NativeSurfaceTarget::Dashboard => {
                self.dashboard = None;
                self.dashboard_placement = None;
                self.dashboard_restore_placement = None;
            }
            NativeSurfaceTarget::Picker(kind) => {
                self.pickers.remove(&kind);
                self.picker_placements.remove(&kind);
                self.picker_restore_placements.remove(&kind);
            }
        }
        self.signature = None;
        Ok(ids)
    }

    pub(super) fn clone_explorer(
        &mut self,
        graph: &mut WindowGraph,
        source: NativeSurfaceTarget,
        view: u64,
        axis: crate::editor::Axis,
    ) -> Result<NativeSurfaceIds, WindowError> {
        if view <= 1 || self.explorer_views.contains_key(&view) {
            return Err(WindowError::UnknownView);
        }
        let source = self
            .ids_for_target(source)
            .ok_or(WindowError::UnknownWindow)?;
        graph.require_clone_view(source.content)?;
        let source_window = graph
            .window(source.window)
            .ok_or(WindowError::UnknownWindow)?;
        let source_tab = source_window.tab;
        let placement = match source_window.placement {
            WindowPlacement::Floating(mut rect) => {
                match axis {
                    crate::editor::Axis::Horizontal => rect.x = rect.x.saturating_add(2),
                    crate::editor::Axis::Vertical => rect.y = rect.y.saturating_add(1),
                }
                WindowPlacement::Floating(rect)
            }
            _ => WindowPlacement::Floating(WindowRect {
                x: 2,
                y: 2,
                width: 40,
                height: 20,
            }),
        };
        let owner = "builtin.surfaces";
        let generation = 1;
        let universal_view = graph.create_view(source.content, owner, generation)?;
        let window = graph.open_window(
            source_tab,
            universal_view,
            placement,
            true,
            owner,
            generation,
        )?;
        let ids = NativeSurfaceIds {
            content: source.content,
            view: universal_view,
            window,
        };
        self.explorer_views.insert(view, ids);
        self.explorer_view_placements.insert(view, placement);
        self.signature = None;
        Ok(ids)
    }

    pub(super) fn allow_explorer(&mut self) {
        self.explorer_closed = false;
        self.signature = None;
    }

    pub(super) fn set_configured_placements(
        &mut self,
        placements: BTreeMap<String, WindowPlacement>,
    ) -> Result<(), WindowError> {
        const SUPPORTED: &[&str] = &[
            "builtin.explorer",
            "builtin.terminal",
            "builtin.dashboard",
            "builtin.picker",
            "builtin.problems",
            "builtin.output",
            "builtin.logs",
            "builtin.debug",
            "builtin.theme",
        ];
        for (content_type, placement) in &placements {
            if !SUPPORTED.contains(&content_type.as_str()) {
                return Err(WindowError::InvalidContentType(content_type.clone()));
            }
            placement.validate()?;
            if matches!(
                placement,
                WindowPlacement::Tiled | WindowPlacement::Overlay(_)
            ) {
                return Err(WindowError::InvalidPlacement);
            }
        }
        if self.configured_placements != placements {
            let previous = std::mem::replace(&mut self.configured_placements, placements);
            if let Err(error) = self.validate_effective_semantic_defaults() {
                self.configured_placements = previous;
                return Err(error);
            }
            self.signature = None;
        }
        Ok(())
    }

    fn validate_effective_semantic_defaults(&self) -> Result<(), WindowError> {
        let fullscreen = [
            self.explorer_placement
                .or_else(|| self.configured_placements.get("builtin.explorer").copied()),
            self.terminal_default_placement
                .or_else(|| self.configured_placements.get("builtin.terminal").copied()),
            self.configured_placements.get("builtin.dashboard").copied(),
            self.configured_placements.get("builtin.picker").copied(),
            self.configured_placements.get("builtin.problems").copied(),
            self.configured_placements.get("builtin.output").copied(),
            self.configured_placements.get("builtin.logs").copied(),
            self.configured_placements.get("builtin.debug").copied(),
            self.configured_placements.get("builtin.theme").copied(),
        ]
        .into_iter()
        .flatten()
        .filter(|placement| *placement == WindowPlacement::Fullscreen)
        .count();
        if fullscreen > 1 {
            Err(WindowError::MultipleFullscreenWindows)
        } else {
            Ok(())
        }
    }

    pub(super) fn set_semantic_default(
        &mut self,
        content_type: &str,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        placement.validate()?;
        let previous_explorer = self.explorer_placement;
        let previous_explorer_restore = self.explorer_restore_placement;
        let previous_terminal = self.terminal_default_placement;
        match content_type {
            "builtin.explorer" => {
                self.explorer_placement = Some(placement);
                if placement != WindowPlacement::Hidden {
                    self.explorer_restore_placement = Some(placement);
                }
            }
            "builtin.terminal" => self.terminal_default_placement = Some(placement),
            value => return Err(WindowError::InvalidContentType(value.to_owned())),
        }
        if let Err(error) = self.validate_effective_semantic_defaults() {
            self.explorer_placement = previous_explorer;
            self.explorer_restore_placement = previous_explorer_restore;
            self.terminal_default_placement = previous_terminal;
            return Err(error);
        }
        self.signature = None;
        Ok(())
    }

    pub(in crate::editor::session) fn semantic_defaults(
        &self,
        selected_terminal: Option<u64>,
    ) -> Vec<(&'static str, WindowPlacement)> {
        let mut rules = Vec::new();
        if let Some(placement) = self.explorer_placement {
            rules.push(("builtin.explorer", placement));
        }
        if let Some(placement) = selected_terminal
            .and_then(|id| self.terminal_placements.get(&id))
            .copied()
            .or(self.terminal_default_placement)
        {
            rules.push(("builtin.terminal", placement));
        }
        rules
    }

    /// Records where a terminal's window goes before the window exists; the next reconcile
    /// creates it there.
    pub(super) fn prefer_terminal_placement(&mut self, id: u64, placement: WindowPlacement) {
        self.terminal_placements.insert(id, placement);
        self.terminal_restore_placements.insert(id, placement);
        self.signature = None;
    }

    pub(super) fn place_terminal(
        &mut self,
        id: u64,
        placement: WindowPlacement,
    ) -> Result<NativeSurfaceIds, WindowError> {
        placement.validate()?;
        let ids = self
            .terminals
            .get(&id)
            .copied()
            .ok_or(WindowError::UnknownWindow)?;
        if placement == WindowPlacement::Hidden {
            if let Some(current) = self.terminal_placements.get(&id).copied() {
                if current != WindowPlacement::Hidden {
                    self.terminal_restore_placements.insert(id, current);
                }
            }
        } else {
            self.terminal_restore_placements.insert(id, placement);
        }
        self.terminal_placements.insert(id, placement);
        self.signature = None;
        Ok(ids)
    }

    #[cfg(test)]
    pub(super) fn reset(&mut self, graph: &mut WindowGraph) -> Result<(), WindowError> {
        graph.remove_owner_generation("builtin.surfaces", 1)?;
        let configured_placements = std::mem::take(&mut self.configured_placements);
        let explorer_placement = self.explorer_placement;
        let explorer_restore_placement = self.explorer_restore_placement;
        let explorer_view_placements = std::mem::take(&mut self.explorer_view_placements);
        let explorer_view_restore_placements =
            std::mem::take(&mut self.explorer_view_restore_placements);
        let explorer_closed = self.explorer_closed;
        let terminal_default_placement = self.terminal_default_placement;
        *self = Self {
            configured_placements,
            explorer_placement,
            explorer_restore_placement,
            explorer_view_placements,
            explorer_view_restore_placements,
            explorer_closed,
            terminal_default_placement,
            ..Self::default()
        };
        Ok(())
    }
}

fn mixed_tiled_layout(
    graph: &WindowGraph,
    tab: WorkspaceTabId,
    editor: Option<TiledLayoutNode>,
    editor_windows: &HashMap<PaneId, WindowId>,
) -> Option<TiledLayoutNode> {
    let mut foreign = graph
        .workspace_tab(tab)
        .and_then(|tab| tab.tiled_root.clone());
    for window in editor_windows.values() {
        foreign = foreign.and_then(|root| remove_layout_window(root, *window));
    }
    match (editor, foreign) {
        (Some(editor), Some(foreign)) => Some(TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 500,
            first: Box::new(editor),
            second: Box::new(foreign),
        }),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

fn remove_layout_window(node: TiledLayoutNode, target: WindowId) -> Option<TiledLayoutNode> {
    match node {
        TiledLayoutNode::Window(window) => {
            (window != target).then_some(TiledLayoutNode::Window(window))
        }
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            remove_layout_window(*first, target),
            remove_layout_window(*second, target),
        ) {
            (Some(first), Some(second)) => Some(TiledLayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

fn editor_logical_layout(
    node: &LayoutNode,
    windows: &HashMap<PaneId, WindowId>,
) -> Result<TiledLayoutNode, WindowError> {
    match node {
        LayoutNode::Pane(pane) => windows
            .get(pane)
            .copied()
            .map(TiledLayoutNode::Window)
            .ok_or(WindowError::UnknownWindow),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => Ok(TiledLayoutNode::Split {
            axis: match axis {
                crate::editor::Axis::Horizontal => TileAxis::Horizontal,
                crate::editor::Axis::Vertical => TileAxis::Vertical,
            },
            ratio: *ratio,
            first: Box::new(editor_logical_layout(first, windows)?),
            second: Box::new(editor_logical_layout(second, windows)?),
        }),
    }
}

fn editor_tiled_layout(
    node: &LayoutNode,
    windows: &HashMap<PaneId, WindowId>,
    desired: &HashMap<PaneId, WindowPlacement>,
) -> Result<Option<TiledLayoutNode>, WindowError> {
    Ok(match node {
        LayoutNode::Pane(pane) => {
            if desired
                .get(pane)
                .is_some_and(|placement| *placement != WindowPlacement::Tiled)
            {
                None
            } else {
                Some(TiledLayoutNode::Window(
                    windows
                        .get(pane)
                        .copied()
                        .ok_or(WindowError::UnknownWindow)?,
                ))
            }
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            editor_tiled_layout(first, windows, desired)?,
            editor_tiled_layout(second, windows, desired)?,
        ) {
            (Some(first), Some(second)) => Some(TiledLayoutNode::Split {
                axis: match axis {
                    crate::editor::Axis::Horizontal => TileAxis::Horizontal,
                    crate::editor::Axis::Vertical => TileAxis::Vertical,
                },
                ratio: *ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    })
}

fn editor_panes(node: &LayoutNode) -> Vec<PaneId> {
    match node {
        LayoutNode::Pane(pane) => vec![*pane],
        LayoutNode::Split { first, second, .. } => {
            let mut panes = editor_panes(first);
            panes.extend(editor_panes(second));
            panes
        }
    }
}

fn editor_buffer_titles(
    state: &EditorTopology<'_>,
    scratch_label: &str,
) -> HashMap<BufferId, String> {
    let mut bases = HashMap::<String, usize>::new();
    for (_, buffer) in state.buffers() {
        let base = editor_buffer_base(
            buffer.path.as_deref(),
            buffer.virtual_owner().unwrap_or(scratch_label),
        );
        *bases.entry(base).or_default() += 1;
    }
    let mut occurrences = HashMap::<String, usize>::new();
    state
        .buffers()
        .iter()
        .map(|(id, buffer)| {
            let base = editor_buffer_base(
                buffer.path.as_deref(),
                buffer.virtual_owner().unwrap_or(scratch_label),
            );
            let duplicate = bases.get(&base).copied().unwrap_or(0) > 1;
            let occurrence = occurrences.entry(base.clone()).or_default();
            *occurrence = occurrence.saturating_add(1);
            let title = if duplicate {
                let context = buffer.path.as_deref().map_or_else(
                    || base.clone(),
                    |path| {
                        path.strip_prefix(state.workspace_root())
                            .unwrap_or(path)
                            .to_string_lossy()
                            .into_owned()
                    },
                );
                format!("{context} ·{}", *occurrence)
            } else {
                base
            };
            (id, sanitize_title(&title))
        })
        .collect()
}

fn editor_buffer_base(path: Option<&Path>, scratch_label: &str) -> String {
    path.map_or_else(
        || scratch_label.to_owned(),
        |path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
    )
}

fn sanitize_title(title: &str) -> String {
    let mut output = String::new();
    for character in title.chars() {
        let character = if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > MAXIMUM_BRIDGE_TITLE_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests;
