//! Checked application window changes and resource cleanup.

use crate::app::windows::EditorWindowCandidate;
use crate::app::windows::NativeSurfaceSignature;
use crate::app::windows::NativeSurfaceTarget;
use crate::app::windows::TerminalGeometrySource;
use crate::app::windows::WindowCandidate;
use crate::app::windows::Windows;
use crate::app::windows::native::NativeSurfaces;
use crate::editor::EditorTopology;
use crate::windows::ContentLifecycle;
use crate::windows::DockEdge;
use crate::windows::SpatialDirection;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(in crate::app) struct RestoreWindowPolicy<'a> {
    pub rules: &'a [PersistedWindowRule],
    pub descriptors: &'a [crate::persistence::PersistedContentDescriptor],
}

#[derive(Clone)]
pub(in crate::app) struct NativeDescriptions<'a> {
    pub signature: NativeSurfaceSignature,
    pub explorer_title: &'a str,
    pub dashboard_title: &'a str,
    pub picker_title: &'a str,
    pub terminals: &'a [(u64, String, ContentLifecycle)],
}

impl WindowCandidate {
    pub(in crate::app) fn graph(&self) -> &WindowGraph {
        &self.core.graph
    }
}

impl Windows {
    pub(in crate::app) fn require_editor_source(
        &self,
        current: &crate::editor::EditorWindowVersion,
    ) -> Result<(), String> {
        if current.revision().is_none() || self.core.source.as_ref() != Some(current) {
            return Err("window editor source is stale".into());
        }
        Ok(())
    }
    pub(in crate::app) fn graph(&self) -> &WindowGraph {
        &self.core.graph
    }
    pub(in crate::app) fn native_ids(&self) -> &NativeSurfaces {
        &self.core.native
    }
    pub(in crate::app) fn terminal_geometry_source(
        &self,
        terminal: u64,
    ) -> Option<TerminalGeometrySource> {
        let ids = self.core.native.terminal(terminal)?;
        let window = self.core.graph.window(ids.window)?;
        let view = self.core.graph.view(ids.view)?;
        let content = self.core.graph.content(ids.content)?;
        let tab = self.core.graph.workspace_tab(window.tab)?;
        if window.view != view.id || view.content != content.id {
            return None;
        }
        Some(TerminalGeometrySource {
            origin: self.origin.clone(),
            terminal,
            content: content.id,
            content_owner: content.owner.clone(),
            content_instance_id: content.instance_id,
            view: view.id,
            view_owner: view.owner.clone(),
            view_instance_id: view.instance_id,
            window: window.id,
            window_owner: window.owner.clone(),
            window_instance_id: window.instance_id,
            tab: tab.id,
            tab_owner: tab.owner.clone(),
            tab_instance_id: tab.instance_id,
            placement: window.placement,
        })
    }

    pub(in crate::app) fn configure_initial(
        &mut self,
        windows: &crate::config::WindowsConfig,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        let placements = configured_window_placements(windows)?;
        if self.core.graph.active_tab().is_some() {
            return Err("initial window policy requires an empty session".into());
        }
        let mut candidate = self.candidate()?;
        candidate.core.graph = state.graph.clone();
        candidate.core.source = Some(state.window_version());
        candidate
            .core
            .native
            .set_configured_placements(placements)
            .map_err(|e| e.to_string())?;
        self.commit(candidate, state)
    }
    pub(in crate::app) fn check_transition_capacity(&self) -> Result<(), String> {
        self.epoch
            .checked_add(1)
            .ok_or("window owner revision is exhausted")?;
        self.core
            .graph
            .revision()
            .ok_or("window graph revision is exhausted")?;
        Ok(())
    }
    pub(in crate::app) fn candidate(&self) -> Result<WindowCandidate, String> {
        self.check_transition_capacity()?;
        Ok(WindowCandidate {
            origin: self.origin.clone(),
            epoch: self.epoch,
            core: self.core.clone(),
            dashboard: None,
        })
    }
    pub(in crate::app) fn commit(
        &mut self,
        mut candidate: WindowCandidate,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&self.origin, &candidate.origin) || self.epoch != candidate.epoch {
            return Err("window candidate is stale or belongs to another session".into());
        }
        if candidate
            .core
            .source
            .as_ref()
            .is_some_and(|source| source != &state.window_version())
        {
            return Err("window candidate editor source is stale".into());
        }
        candidate
            .graph()
            .validate()
            .map_err(|error| error.to_string())?;
        candidate
            .graph()
            .revision()
            .ok_or("window graph revision is exhausted")?;
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or("window owner revision is exhausted")?;
        if let Some(dashboard) = candidate.dashboard {
            self.dashboard = dashboard;
        }
        let focus_content = |graph: &crate::windows::WindowGraph| {
            graph
                .focused()
                .and_then(|w| graph.window(w))
                .and_then(|w| graph.view(w.view))
                .map(|v| v.content)
        };
        if self.core.graph.focused() != candidate.core.graph.focused()
            || focus_content(&self.core.graph) != focus_content(&candidate.core.graph)
        {
            self.focus_stamp = Arc::new(());
        }
        if state.graph != candidate.core.graph {
            state.graph = candidate.core.graph.clone();
        }
        candidate.core.source = Some(state.window_version());
        self.core = candidate.core;
        self.accept_graph_focus();
        self.epoch = epoch;
        self.invalidate();
        Ok(())
    }
    pub(in crate::app) fn accept_current(
        &mut self,
        candidate: WindowCandidate,
        current: &crate::editor::EditorWindowVersion,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        if candidate
            .core
            .source
            .as_ref()
            .map_or(current.revision() == Some(0), |source| {
                source == current && current.revision().is_some()
            })
        {
            self.commit(candidate, state)
        } else {
            Err("window candidate editor source is stale".into())
        }
    }
    pub(in crate::app) fn prepare_editor_transition(
        &self,
        proposal: crate::editor::EditorDraft,
        current: &crate::editor::EditorVersion,
        catalog: crate::plugins::PluginCatalogVersion,
        scratch: &str,
        descriptions: Option<NativeDescriptions<'_>>,
    ) -> Result<EditorWindowCandidate, String> {
        proposal.check_current(current)?;
        let candidate = match descriptions {
            Some(descriptions) => {
                self.prepare_reconcile(&proposal.topology(), scratch, descriptions)?
            }
            None => self.prepare_editor(&proposal.topology(), scratch)?,
        };
        self.stage_editor_transition(proposal, candidate, catalog)
    }
    pub(in crate::app) fn prepare_restore_transition(
        &self,
        proposal: crate::editor::EditorDraft,
        source: (
            &crate::editor::EditorVersion,
            crate::plugins::PluginCatalogVersion,
        ),
        scratch: &str,
        mut descriptions: NativeDescriptions<'_>,
        plugin_loads: &[(String, u64)],
        policy: RestoreWindowPolicy<'_>,
    ) -> Result<crate::app::windows::EditorWindowCandidate, String> {
        descriptions.signature.dashboard_visible = false;
        let restored_descriptions = descriptions.clone();
        proposal.check_current(source.0)?;
        self.check_editor_source(&proposal.topology())?;
        let mut candidate = self.prepare_editor(&proposal.topology(), scratch)?;
        for (plugin, load) in plugin_loads {
            candidate
                .core
                .graph
                .remove_owner_instance(&format!("plugin.{plugin}"), *load)
                .map_err(|error| error.to_string())?;
        }
        let mut transition = self.stage_editor_transition(proposal, candidate, source.1)?;
        validate_window_rules(policy.rules).map_err(|error| error.to_string())?;
        let mut rules = policy.rules.iter().collect::<Vec<_>>();
        rules.sort_by_key(|rule| matches!(rule.placement, PersistedWindowPlacement::Fullscreen));
        for rule in rules {
            transition
                .candidate
                .core
                .native
                .set_semantic_default(
                    &rule.content_type,
                    restored_window_placement(&rule.placement)?,
                )
                .map_err(|error| error.to_string())?;
        }
        let topology = transition.proposal.topology();
        let mut signature = restored_descriptions.signature;
        signature.editor_tab = topology.state().active_tab();
        signature.editor_window = topology.state().focused_window();
        transition
            .candidate
            .core
            .native
            .reconcile(
                &mut transition.candidate.core.graph,
                signature,
                restored_descriptions.explorer_title,
                restored_descriptions.dashboard_title,
                restored_descriptions.picker_title,
                restored_descriptions.terminals,
            )
            .map_err(|error| error.to_string())?;
        let placements = policy
            .descriptors
            .iter()
            .filter(|descriptor| descriptor.adapter == "text-buffer")
            .map(|descriptor| {
                let mut parts = descriptor.semantic_id.split(':');
                let (Some("text"), Some(tab), Some(pane), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err("invalid persisted text semantic identity".to_owned());
                };
                Ok((
                    tab.parse::<usize>()
                        .map_err(|_| "invalid persisted text tab identity".to_owned())?,
                    pane.parse::<usize>()
                        .map_err(|_| "invalid persisted text pane identity".to_owned())?,
                    descriptor
                        .path
                        .clone()
                        .ok_or("persisted text descriptor has no path")?,
                    restored_window_placement(&descriptor.desired_placement)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (tab_index, pane_index, path, placement) in placements {
            let tab = topology
                .state()
                .editor_tabs()
                .nth(tab_index)
                .ok_or("unknown restored tab")?
                .id;
            let window = *topology
                .state()
                .tab_windows(tab)
                .get(pane_index)
                .ok_or("unknown restored window")?;
            let view = topology
                .state()
                .window_view(window)
                .ok_or("unknown restored view")?;
            if topology.buffers()[topology.views()[view].buffer]
                .path
                .as_ref()
                != Some(&path)
            {
                return Err("stale persisted text descriptor".into());
            }
            transition
                .candidate
                .core
                .graph
                .place(window, placement)
                .map_err(|error| error.to_string())?;
        }
        transition.candidate.core.native.invalidate();
        transition
            .candidate
            .core
            .graph
            .validate()
            .map_err(|error| error.to_string())?;
        transition.candidate.dashboard = Some(false);
        Ok(transition)
    }
    pub(in crate::app) fn prepare_document_transition(
        &self,
        proposal: crate::editor::EditorDraft,
        current: &crate::editor::EditorVersion,
        catalog: crate::plugins::PluginCatalogVersion,
        scratch: &str,
        mut descriptions: NativeDescriptions<'_>,
    ) -> Result<EditorWindowCandidate, String> {
        descriptions.signature.dashboard_visible = false;
        let mut transition = self.prepare_editor_transition(
            proposal,
            current,
            catalog,
            scratch,
            Some(descriptions),
        )?;
        transition.candidate.dashboard = Some(false);
        Ok(transition)
    }
    pub(in crate::app) fn stage_editor_transition(
        &self,
        proposal: crate::editor::EditorDraft,
        candidate: WindowCandidate,
        catalog: crate::plugins::PluginCatalogVersion,
    ) -> Result<EditorWindowCandidate, String> {
        if !Arc::ptr_eq(&self.origin, &candidate.origin) || self.epoch != candidate.epoch {
            return Err("window candidate is stale or belongs to another session".into());
        }
        self.epoch
            .checked_add(1)
            .ok_or("window owner revision is exhausted")?;
        if self.core.source.as_ref() == Some(proposal.before_window())
            && proposal.before_graph_revision() != self.core.graph.revision()
        {
            return Err("editor proposal window graph changed before acceptance".into());
        }
        let after = candidate
            .core
            .source
            .as_ref()
            .ok_or("missing proposed editor source")?;
        let before = proposal.before_window();
        if before.revision().is_none()
            || after.revision().is_none()
            || !before.same_editor(after)
            || after != &proposal.window_version()
        {
            return Err("invalid editor transition source".into());
        }
        if candidate
            .core
            .graph
            .active_tab()
            .and_then(|tab| candidate.core.graph.workspace_tab(tab))
            .is_none()
        {
            return Err("editor transition has no projectable workspace".into());
        }
        Ok(EditorWindowCandidate {
            proposal,
            catalog,
            candidate,
        })
    }
    pub(in crate::app) fn accept_editor_transition(
        &mut self,
        transition: EditorWindowCandidate,
        current: &crate::editor::EditorVersion,
        catalog: &crate::plugins::PluginCatalogVersion,
    ) -> Result<crate::editor::EditorState, String> {
        if &transition.catalog != catalog {
            return Err("editor transition source changed before acceptance".into());
        }
        let mut state = transition.proposal.accept(current)?;
        self.commit(transition.candidate, &mut state)?;
        Ok(state)
    }
    pub(in crate::app) fn invalidate_native(&mut self) {
        self.cancel_editor_close();
        self.core.native.invalidate();
        self.invalidate();
        self.epoch = self.epoch.saturating_add(1);
    }
    pub(in crate::app) fn prefer_terminal_placement(
        &mut self,
        id: u64,
        placement: WindowPlacement,
    ) {
        self.core.native.prefer_terminal_placement(id, placement);
        self.invalidate_native();
    }
    pub(in crate::app) fn allow_explorer(&mut self) {
        self.core.native.allow_explorer();
        self.invalidate_native();
    }
    pub(in crate::app) fn prepare_reconcile(
        &self,
        state: &EditorTopology<'_>,
        scratch: &str,
        descriptions: NativeDescriptions<'_>,
    ) -> Result<WindowCandidate, String> {
        let mut candidate = self.prepare_editor(state, scratch)?;
        let mut signature = descriptions.signature;
        signature.editor_tab = state.state().active_tab();
        signature.editor_window = state.state().focused_window();
        candidate
            .core
            .native
            .reconcile(
                &mut candidate.core.graph,
                signature,
                descriptions.explorer_title,
                descriptions.dashboard_title,
                descriptions.picker_title,
                descriptions.terminals,
            )
            .map_err(|error| error.to_string())?;
        candidate
            .core
            .graph
            .validate()
            .map_err(|error| error.to_string())?;
        Ok(candidate)
    }
    pub(in crate::app) fn check_editor_source(
        &self,
        state: &EditorTopology<'_>,
    ) -> Result<(), String> {
        self.check_transition_capacity()?;
        if state.window_revision().is_none() {
            return Err("editor source revision is exhausted".into());
        }
        if self
            .core
            .source
            .as_ref()
            .is_some_and(|source| !source.same_editor(state.version()))
        {
            return Err("editor source belongs to another editor".into());
        }
        Ok(())
    }
    pub(in crate::app) fn prepare_editor(
        &self,
        state: &EditorTopology<'_>,
        scratch: &str,
    ) -> Result<WindowCandidate, String> {
        self.check_editor_source(state)?;
        let mut candidate = self.candidate()?;
        candidate.core.graph = state.graph().clone();
        for (buffer, title) in crate::app::windows::content::editor_buffer_titles(state, scratch) {
            let content = state
                .state()
                .content_for_buffer(buffer)
                .ok_or("buffer has no graph content")?;
            candidate
                .core
                .graph
                .update_content(content, title, state.buffers()[buffer].dirty)
                .map_err(|error| error.to_string())?;
        }
        candidate.core.source = Some(state.version().clone());
        candidate.core.scratch_label = Some(scratch.to_owned());
        Ok(candidate)
    }
    pub(in crate::app) fn focus_window(
        &mut self,
        window: WindowId,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        let mut candidate = self.candidate()?;
        candidate
            .core
            .graph
            .focus(window)
            .map_err(|e| e.to_string())?;
        candidate.core.native.invalidate();
        self.commit(candidate, state)
    }
    pub(in crate::app) fn require_projected_graph(&self) -> Result<(), String> {
        if self
            .layout
            .as_ref()
            .is_some_and(|layout| Some(layout.graph_revision) == self.core.graph.revision())
        {
            Ok(())
        } else {
            Err("universal compositor projection is missing or stale".into())
        }
    }
    pub(in crate::app) fn require_native_stop(
        &self,
        target: NativeSurfaceTarget,
    ) -> Result<(), String> {
        let content = self
            .core
            .native
            .ids_for_target(target)
            .ok_or("native resource is unavailable")?
            .content;
        self.core
            .graph
            .require_stop_resource(content)
            .map_err(|error| error.to_string())
    }

    pub(in crate::app) fn place_native(
        &mut self,
        target: NativeSurfaceTarget,
        placement: WindowPlacement,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        self.require_projected_graph()?;
        let hiding_focused_terminal = placement == WindowPlacement::Hidden
            && matches!(target, NativeSurfaceTarget::Terminal(id) if self.focused_terminal() == Some(id));
        let mut candidate = self.candidate()?;
        let core = &mut candidate.core;
        if placement == WindowPlacement::Hidden
            && let Some(current) = core
                .native
                .ids_for_target(target)
                .and_then(|ids| core.graph.window(ids.window))
                .map(|w| w.placement)
                .filter(|p| *p != WindowPlacement::Hidden)
        {
            core.native
                .place_target(target, current)
                .map_err(|e| e.to_string())?;
        }
        let ids = core
            .native
            .place_target(target, placement)
            .map_err(|e| e.to_string())?;
        core.graph
            .place(ids.window, placement)
            .map_err(|e| e.to_string())?;
        self.commit(candidate, state)?;
        if hiding_focused_terminal {
            self.return_from_terminal();
        }
        if matches!(target, NativeSurfaceTarget::Terminal(_)) {
            match placement {
                WindowPlacement::Docked {
                    edge: DockEdge::Top | DockEdge::Bottom,
                    size,
                } => {
                    self.terminal_height = size;
                }
                WindowPlacement::Floating(rect) => self.terminal_height = rect.height,
                _ => {}
            }
        }
        Ok(())
    }
    pub(in crate::app) fn show_native(
        &mut self,
        target: NativeSurfaceTarget,
        explorer: WindowPlacement,
        terminal: WindowPlacement,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        self.require_projected_graph()?;
        let ids = self
            .core
            .native
            .ids_for_target(target)
            .ok_or("native window is unavailable")?;
        if self
            .core
            .graph
            .window(ids.window)
            .is_some_and(|window| window.placement != WindowPlacement::Hidden)
        {
            return Ok(());
        }
        let mut candidate = self.candidate()?;
        let core = &mut candidate.core;
        let ids = core
            .native
            .show_target(target, explorer, terminal)
            .map_err(|e| e.to_string())?;
        let placement = core
            .native
            .placement_for_target(target)
            .ok_or("native window has no restorable placement")?;
        core.graph
            .place(ids.window, placement)
            .map_err(|e| e.to_string())?;
        self.commit(candidate, state)
    }

    pub(in crate::app) fn swap_native(
        &mut self,
        first: NativeSurfaceTarget,
        second: NativeSurfaceTarget,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        self.require_projected_graph()?;
        let mut candidate = self.candidate()?;
        let core = &mut candidate.core;
        let a = core
            .native
            .ids_for_target(first)
            .ok_or("first native window is unavailable")?;
        let b = core
            .native
            .ids_for_target(second)
            .ok_or("second native window is unavailable")?;
        let ap = core
            .graph
            .window(a.window)
            .ok_or("first native window identity is stale")?
            .placement;
        let bp = core
            .graph
            .window(b.window)
            .ok_or("second native window identity is stale")?
            .placement;
        core.graph
            .swap_non_tiled_placements(a.window, b.window)
            .map_err(|e| e.to_string())?;
        if bp == WindowPlacement::Hidden && ap != WindowPlacement::Hidden {
            core.native
                .place_target(first, ap)
                .map_err(|e| e.to_string())?;
        }
        if ap == WindowPlacement::Hidden && bp != WindowPlacement::Hidden {
            core.native
                .place_target(second, bp)
                .map_err(|e| e.to_string())?;
        }
        core.native
            .place_target(first, bp)
            .map_err(|e| e.to_string())?;
        core.native
            .place_target(second, ap)
            .map_err(|e| e.to_string())?;
        self.commit(candidate, state)
    }
    pub(in crate::app) fn prepare_rules(
        &self,
        windows: &crate::config::WindowsConfig,
        descriptions: NativeDescriptions<'_>,
    ) -> Result<WindowCandidate, String> {
        let mut candidate = self.candidate()?;
        candidate
            .core
            .native
            .set_configured_placements(configured_window_placements(windows)?)
            .map_err(|e| e.to_string())?;
        Self::reconcile_rules(&mut candidate, descriptions)?;
        Ok(candidate)
    }
    pub(in crate::app) fn prepare_semantic_defaults(
        &self,
        rules: &[(&str, WindowPlacement)],
        descriptions: NativeDescriptions<'_>,
    ) -> Result<WindowCandidate, String> {
        let mut candidate = self.candidate()?;
        let mut rules = rules.to_vec();
        rules.sort_by_key(|(_, placement)| *placement == WindowPlacement::Fullscreen);
        for (kind, placement) in rules {
            candidate
                .core
                .native
                .set_semantic_default(kind, placement)
                .map_err(|e| e.to_string())?;
        }
        Self::reconcile_rules(&mut candidate, descriptions)?;
        Ok(candidate)
    }
    fn reconcile_rules(
        candidate: &mut WindowCandidate,
        descriptions: NativeDescriptions<'_>,
    ) -> Result<(), String> {
        let core = &mut candidate.core;
        core.native
            .reconcile(
                &mut core.graph,
                descriptions.signature,
                descriptions.explorer_title,
                descriptions.dashboard_title,
                descriptions.picker_title,
                descriptions.terminals,
            )
            .map_err(|e| e.to_string())?;
        core.graph.validate().map_err(|e| e.to_string())?;
        core.native.invalidate();
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::app) fn reset_runtime(
        &mut self,
        plugin_loads: &[(String, u64)],
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        let semantic_focus = match self.focus {
            crate::app::windows::FocusTarget::Explorer(_)
            | crate::app::windows::FocusTarget::Terminal(_) => Some(self.focus),
            crate::app::windows::FocusTarget::Editor
            | crate::app::windows::FocusTarget::Plugin(_) => None,
        };
        let mut candidate = self.candidate()?;
        let core = &mut candidate.core;
        for (plugin, load) in plugin_loads {
            core.graph
                .remove_owner_instance(&format!("plugin.{plugin}"), *load)
                .map_err(|e| e.to_string())?;
        }
        core.native
            .reset(&mut core.graph)
            .map_err(|e| e.to_string())?;
        self.commit(candidate, state)?;
        if let Some(focus) = semantic_focus {
            self.focus = focus;
            self.core.native.invalidate();
        }
        self.origin = Arc::new(());
        Ok(())
    }
    pub(in crate::app) fn remove_plugin_load(
        &mut self,
        plugin: &str,
        load: u64,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        let mut candidate = self.candidate()?;
        candidate.core.graph = state.graph.clone();
        candidate.core.source = Some(state.window_version());
        candidate
            .core
            .graph
            .remove_owner_instance(&format!("plugin.{plugin}"), load)
            .map_err(|e| e.to_string())?;
        candidate.core.native.invalidate();
        self.commit(candidate, state)
    }
}

impl Windows {
    pub(in crate::app) fn native_placement(
        &self,
        target: NativeSurfaceTarget,
    ) -> Result<WindowPlacement, String> {
        let ids = self
            .native_ids()
            .ids_for_target(target)
            .ok_or_else(|| "native window target is not currently instantiated".to_owned())?;
        self.graph()
            .window(ids.window)
            .map(|window| window.placement)
            .ok_or_else(|| "native window graph identity is stale".to_owned())
    }

    pub(in crate::app) fn resized_native_placement(
        &self,
        target: NativeSurfaceTarget,
        amount: i16,
    ) -> Result<WindowPlacement, String> {
        let placement = match self.native_placement(target)? {
            WindowPlacement::Docked { edge, size } => WindowPlacement::Docked {
                edge,
                size: size.saturating_add_signed(amount).max(1),
            },
            WindowPlacement::Floating(mut rect) => {
                rect.width = rect.width.saturating_add_signed(amount).max(1);
                rect.height = rect.height.saturating_add_signed(amount).max(1);
                WindowPlacement::Floating(rect)
            }
            WindowPlacement::Tiled => {
                return Err("editor tiled mutation is not yet graph-authoritative".into());
            }
            WindowPlacement::Fullscreen | WindowPlacement::Hidden | WindowPlacement::Overlay(_) => {
                return Err("only docked or floating ordinary windows can be resized".into());
            }
        };
        Ok(placement)
    }
    pub(in crate::app) fn directional_native_placement(
        &self,
        target: NativeSurfaceTarget,
        direction: crate::windows::SpatialDirection,
        step: i16,
    ) -> Result<Option<WindowPlacement>, String> {
        let placement = self.native_placement(target)?;
        let placement = match placement {
            WindowPlacement::Docked { edge, size } => {
                let grow = match (edge, direction) {
                    (DockEdge::Bottom, SpatialDirection::Up)
                    | (DockEdge::Top, SpatialDirection::Down)
                    | (DockEdge::Left, SpatialDirection::Right)
                    | (DockEdge::Right, SpatialDirection::Left) => true,
                    (DockEdge::Bottom, SpatialDirection::Down)
                    | (DockEdge::Top, SpatialDirection::Up)
                    | (DockEdge::Left, SpatialDirection::Left)
                    | (DockEdge::Right, SpatialDirection::Right) => false,
                    _ => return Ok(None),
                };
                WindowPlacement::Docked {
                    edge,
                    size: size
                        .saturating_add_signed(if grow { step } else { -step })
                        .max(1),
                }
            }
            WindowPlacement::Floating(mut rect) => {
                match direction {
                    SpatialDirection::Left => {
                        rect.width = rect.width.saturating_add_signed(-step).max(1)
                    }
                    SpatialDirection::Right => {
                        rect.width = rect.width.saturating_add_signed(step).max(1)
                    }
                    SpatialDirection::Up => {
                        rect.height = rect.height.saturating_add_signed(step).max(1)
                    }
                    SpatialDirection::Down => {
                        rect.height = rect.height.saturating_add_signed(-step).max(1)
                    }
                }
                WindowPlacement::Floating(rect)
            }
            WindowPlacement::Tiled => {
                return Err("native tiled window resizing is unsupported".into());
            }
            WindowPlacement::Fullscreen | WindowPlacement::Hidden | WindowPlacement::Overlay(_) => {
                return Err("focused window cannot be directionally resized".into());
            }
        };
        Ok(Some(placement))
    }
}

fn configured_window_placements(
    windows: &crate::config::WindowsConfig,
) -> Result<BTreeMap<String, WindowPlacement>, String> {
    let mut placements = BTreeMap::new();
    for rule in &windows.rules {
        if rule.content_type.starts_with("plugin.") {
            continue;
        }
        let placement = match &rule.placement {
            crate::config::WindowPlacementConfig::Automatic => continue,
            crate::config::WindowPlacementConfig::Docked { edge, size } => {
                WindowPlacement::Docked {
                    edge: match edge.as_str() {
                        "left" => DockEdge::Left,
                        "right" => DockEdge::Right,
                        "top" => DockEdge::Top,
                        "bottom" => DockEdge::Bottom,
                        _ => return Err(format!("unknown configured dock edge `{edge}`")),
                    },
                    size: *size,
                }
            }
            crate::config::WindowPlacementConfig::Floating {
                x,
                y,
                width,
                height,
            } => WindowPlacement::Floating(WindowRect {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            }),
            crate::config::WindowPlacementConfig::Fullscreen => WindowPlacement::Fullscreen,
            crate::config::WindowPlacementConfig::Hidden => WindowPlacement::Hidden,
        };
        placement.validate().map_err(|error| error.to_string())?;
        if placements
            .insert(rule.content_type.clone(), placement)
            .is_some()
        {
            return Err(format!(
                "duplicate configured window target `{}`",
                rule.content_type
            ));
        }
    }
    Ok(placements)
}

use crate::persistence::PersistedWindowPlacement;

use crate::persistence::PersistedWindowRule;

use crate::persistence::validate_window_rules;
impl Windows {
    pub(in crate::app) fn persisted_editor_placement(
        &self,
        pane: crate::windows::WindowId,
        expected_adapter: crate::windows::ContentAdapterKind,
    ) -> Result<PersistedWindowPlacement, String> {
        let window = pane;
        let content = self
            .graph()
            .content_for_window(window)
            .ok_or("editor window content is stale")?;
        if content.adapter != expected_adapter {
            return Err("editor content adapter disagrees with its domain owner".into());
        }
        self.graph()
            .require_persist_descriptor(content.id)
            .map_err(|error| error.to_string())?;
        let placement = self
            .graph()
            .window(window)
            .ok_or("editor window is stale")?
            .placement;
        persisted_window_placement(placement)
            .ok_or_else(|| "editor window placement is not persistable".into())
    }

    pub(in crate::app) fn persisted_rules(
        &self,
        selected: Option<u64>,
    ) -> Vec<PersistedWindowRule> {
        let mut rules = self.native_ids().semantic_defaults(selected);
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.explorer")
            && let Some(ids) = self.native_ids().explorer()
            && let Some(window) = self.graph().window(ids.window)
        {
            rules.push(("builtin.explorer", window.placement));
        }
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.terminal")
            && let Some(ids) = selected.and_then(|id| self.native_ids().terminal(id))
            && let Some(window) = self.graph().window(ids.window)
        {
            rules.push(("builtin.terminal", window.placement));
        }
        rules
            .into_iter()
            // These are semantic placement defaults, never live content descriptors.
            .filter_map(|(content_type, placement)| {
                persisted_window_placement(placement).map(|placement| PersistedWindowRule {
                    content_type: content_type.into(),
                    placement,
                })
            })
            .collect()
    }

    pub(in crate::app) fn prepare_persisted_rules(
        &self,
        rules: &[PersistedWindowRule],
        descriptions: NativeDescriptions<'_>,
    ) -> Result<WindowCandidate, String> {
        validate_window_rules(rules).map_err(|error| error.to_string())?;
        let rules = rules
            .iter()
            .map(|rule| {
                Ok((
                    rule.content_type.as_str(),
                    restored_window_placement(&rule.placement)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.prepare_semantic_defaults(&rules, descriptions)
    }
}
pub(super) fn persisted_window_placement(
    placement: WindowPlacement,
) -> Option<PersistedWindowPlacement> {
    match placement {
        WindowPlacement::Tiled => Some(PersistedWindowPlacement::Tiled),
        WindowPlacement::Docked { edge, size } => Some(PersistedWindowPlacement::Docked {
            edge: match edge {
                DockEdge::Left => "left",
                DockEdge::Right => "right",
                DockEdge::Top => "top",
                DockEdge::Bottom => "bottom",
            }
            .into(),
            size,
        }),
        WindowPlacement::Floating(rect) => Some(PersistedWindowPlacement::Floating {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }),
        WindowPlacement::Fullscreen => Some(PersistedWindowPlacement::Fullscreen),
        WindowPlacement::Hidden => Some(PersistedWindowPlacement::Hidden),
        WindowPlacement::Overlay(_) => None,
    }
}

pub(super) fn restored_window_placement(
    placement: &PersistedWindowPlacement,
) -> Result<WindowPlacement, String> {
    let placement = match placement {
        PersistedWindowPlacement::Tiled => WindowPlacement::Tiled,
        PersistedWindowPlacement::Docked { edge, size } => WindowPlacement::Docked {
            edge: match edge.as_str() {
                "left" => DockEdge::Left,
                "right" => DockEdge::Right,
                "top" => DockEdge::Top,
                "bottom" => DockEdge::Bottom,
                _ => return Err(format!("unknown dock edge `{edge}`")),
            },
            size: *size,
        },
        PersistedWindowPlacement::Floating {
            x,
            y,
            width,
            height,
        } => WindowPlacement::Floating(WindowRect {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        }),
        PersistedWindowPlacement::Fullscreen => WindowPlacement::Fullscreen,
        PersistedWindowPlacement::Hidden => WindowPlacement::Hidden,
    };
    placement.validate().map_err(|error| error.to_string())
}
