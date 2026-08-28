use super::*;

impl InteractiveApplication {
    pub(super) fn sync_plugin_window_ui(&mut self) {
        let projection = self.compositor_projection().cloned();
        let events = self
            .plugin_windows
            .synchronize_private_geometry(projection.as_ref());
        for (plugin, load, event) in events {
            let pending = self
                .pending_plugin_ui_events
                .entry((plugin, PluginLoadId(load)))
                .or_default();
            pending.push(event);
            if pending.len() > 64 {
                pending.drain(..pending.len().saturating_sub(64));
            }
        }
        self.ui.plugin_surfaces = self.plugin_windows.snapshots();
        self.ui.private_surfaces = self.plugin_windows.private_snapshots(&self.windows);
    }

    pub(super) fn compositor_projection(&self) -> Option<&CompositorProjection> {
        let revision = self.state.window_revision()?;
        self.ui.compositor_layout.as_ref().and_then(|layout| {
            (layout.source_revision == revision
                && Some(layout.graph_revision) == self.windows.revision()
                && Some(layout.graph_revision) == self.ui.window_graph_revision
                && layout.projection.bounds.width == self.terminal_width
                && layout.projection.bounds.height == self.terminal_height)
                .then_some(&layout.projection)
        })
    }

    pub(super) fn workspace_bounds(&self) -> Option<WindowRect> {
        self.compositor_projection()
            .map(|projection| projection.workspace_bounds)
    }

    pub(super) fn explorer_window_rect(&self) -> Option<WindowRect> {
        let layout = self.ui.compositor_layout.as_ref()?;
        let window = layout.surfaces.explorer?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }

    pub(super) fn terminal_window_rect(&self) -> Option<WindowRect> {
        let layout = self.ui.compositor_layout.as_ref()?;
        let (_, window) = layout.surfaces.terminal?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }

    pub(super) fn workspace_render_height(&self) -> u16 {
        self.workspace_bounds()
            .map_or(self.terminal_height, |bounds| bounds.height)
    }

    pub(super) fn terminal_body_dimensions(&self) -> Option<(u16, u16)> {
        let layout = self.ui.compositor_layout.as_ref()?;
        if Some(layout.source_revision) != self.state.window_revision()
            || Some(layout.graph_revision) != self.windows.revision()
        {
            return None;
        }
        let rect = self.terminal_window_rect()?;
        Some((rect.width.max(1), rect.height.saturating_sub(2).max(1)))
    }

    pub(super) fn resize_active_terminal_to_panel(&mut self) {
        let Some(id) = self.active_terminal else {
            return;
        };
        self.reconcile_editor_windows();
        let Some((columns, rows)) = self.terminal_body_dimensions() else {
            return;
        };
        if self.terminals.get(&id).is_none_or(|terminal| {
            terminal.screen.dimensions() == (usize::from(columns), usize::from(rows))
        }) {
            return;
        }
        if let Err(error) = self.ptys.resize(id, columns, rows) {
            self.show_error(error);
            return;
        }
        if let Some(terminal) = self.terminals.get_mut(&id) {
            terminal.screen.resize(columns, rows);
        }
    }

    pub(super) fn resolve_graph_window_target(
        &self,
        target: &str,
    ) -> Result<crate::ui::window::WindowId, String> {
        if target == "terminal" {
            let terminal = self
                .active_terminal
                .ok_or_else(|| self.message("terminal_missing", &[]))?;
            return self
                .surface_windows
                .terminal(terminal.0)
                .map(|ids| ids.window)
                .ok_or_else(|| "selected terminal window is unavailable".into());
        }
        let target = match target {
            "focused" => WindowTarget::Focused,
            "explorer" => WindowTarget::ContentType(
                ContentTypeId::new("builtin.explorer").map_err(|error| error.to_string())?,
            ),
            value if value.starts_with("window:") => WindowTarget::Id(crate::ui::window::WindowId(
                value["window:".len()..]
                    .parse::<u64>()
                    .map_err(|_| format!("invalid runtime window target `{value}`"))?,
            )),
            value => WindowTarget::ContentType(
                ContentTypeId::new(value).map_err(|error| error.to_string())?,
            ),
        };
        self.windows
            .resolve_target(&target)
            .map_err(|error| error.to_string())
    }

    pub(super) fn resolve_native_window_target(
        &self,
        target: &str,
    ) -> Result<NativeSurfaceTarget, String> {
        let window = self.resolve_graph_window_target(target)?;
        self.surface_windows
            .target_for_window(window)
            .ok_or_else(|| {
                let adapter = self
                    .windows
                    .window(window)
                    .and_then(|window| self.windows.view(window.view))
                    .and_then(|view| self.windows.content(view.content))
                    .map_or("unknown", |content| match content.adapter {
                        crate::ui::window::ContentAdapterKind::TextBuffer => "editor-tiled",
                        crate::ui::window::ContentAdapterKind::TerminalSession => "terminal",
                        crate::ui::window::ContentAdapterKind::VirtualDocument => {
                            "virtual-document"
                        }
                        crate::ui::window::ContentAdapterKind::DeclarativeUi => "declarative-ui",
                        crate::ui::window::ContentAdapterKind::PrivateCellSurface => "private-cell",
                        crate::ui::window::ContentAdapterKind::ExternalTui => "external-tui",
                    });
                format!("window adapter `{adapter}` has no mutable lifecycle hook")
            })
    }

    pub(super) fn editor_pane_for_window(
        &self,
        window: crate::ui::window::WindowId,
    ) -> Option<crate::editor::PaneId> {
        self.editor_windows.pane_for_window(window)
    }

    pub(super) fn apply_native_window_placement(
        &mut self,
        target: NativeSurfaceTarget,
        placement: WindowPlacement,
    ) -> Result<(), String> {
        // Commands operate only against the exact application projection that
        // was accepted for the current editor source revision.
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let mut graph = self.windows.clone();
        let mut surfaces = self.surface_windows.clone();
        if placement == WindowPlacement::Hidden
            && let Some(current) = surfaces
                .ids_for_target(target)
                .and_then(|ids| graph.window(ids.window))
                .map(|window| window.placement)
                .filter(|current| *current != WindowPlacement::Hidden)
        {
            surfaces
                .place_target(target, current)
                .map_err(|error| error.to_string())?;
        }
        let ids = surfaces
            .place_target(target, placement)
            .map_err(|error| error.to_string())?;
        graph
            .place(ids.window, placement)
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        self.windows = graph;
        self.surface_windows = surfaces;
        if placement == WindowPlacement::Hidden {
            match target {
                NativeSurfaceTarget::Explorer => self.ui.explorer_focused = false,
                NativeSurfaceTarget::Terminal(_) => self.terminal_focused = false,
                NativeSurfaceTarget::Dashboard | NativeSurfaceTarget::Picker(_) => {}
            }
        }
        self.reconcile_editor_windows();
        if matches!(target, NativeSurfaceTarget::Terminal(_))
            && placement != WindowPlacement::Hidden
        {
            self.resize_active_terminal_to_panel();
        }
        self.sync_terminal_panel();
        self.ensure_cursor_visible();
        Ok(())
    }

    pub(super) fn place_active_terminal(&mut self, placement: WindowPlacement) {
        let Some(id) = self.active_terminal else {
            self.show_error(self.message("terminal_missing", &[]));
            return;
        };
        if let Err(error) =
            self.apply_native_window_placement(NativeSurfaceTarget::Terminal(id.0), placement)
        {
            self.show_error(error);
        }
    }

    pub(super) fn show_native_window(&mut self, target: NativeSurfaceTarget) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let explorer_fallback = WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: explorer_panel_width(&self.configuration.settings, self.terminal_width)
                .unwrap_or(1)
                .saturating_add(1),
        };
        let terminal_fallback = WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: self.terminal_panel_height.max(1),
        };
        let mut graph = self.windows.clone();
        let mut surfaces = self.surface_windows.clone();
        let ids = surfaces
            .show_target(target, explorer_fallback, terminal_fallback)
            .map_err(|error| error.to_string())?;
        let placement = surfaces
            .placement_for_target(target)
            .ok_or_else(|| "native window has no restorable placement".to_owned())?;
        graph
            .place(ids.window, placement)
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        self.windows = graph;
        self.surface_windows = surfaces;
        self.reconcile_editor_windows();
        if matches!(target, NativeSurfaceTarget::Terminal(_)) {
            self.resize_active_terminal_to_panel();
        }
        self.sync_terminal_panel();
        Ok(())
    }

    pub(super) fn current_native_placement(
        &self,
        target: NativeSurfaceTarget,
    ) -> Result<WindowPlacement, String> {
        let ids = self
            .surface_windows
            .ids_for_target(target)
            .ok_or_else(|| "native window target is not currently instantiated".to_owned())?;
        self.windows
            .window(ids.window)
            .map(|window| window.placement)
            .ok_or_else(|| "native window graph identity is stale".to_owned())
    }

    pub(super) fn resize_native_window(
        &mut self,
        target: NativeSurfaceTarget,
        amount: i16,
    ) -> Result<(), String> {
        let placement = match self.current_native_placement(target)? {
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
        self.apply_native_window_placement(target, placement)
    }

    pub(super) fn resize_graph_window(&mut self, target: &str, amount: i16) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let window = self.resolve_graph_window_target(target)?;
        if let Some(native) = self.surface_windows.target_for_window(window) {
            return self.resize_native_window(native, amount);
        }
        let pane = self.editor_pane_for_window(window).ok_or_else(|| {
            "window adapter has no resize hook; editor dock/float/fullscreen remains unsupported"
                .to_owned()
        })?;
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::FocusPaneId { pane })
            .with_operation(EditorOperation::ResizePane { delta: amount })
            .with_undo_policy(UndoPolicy::Ignore);
        self.apply_authoritative_editor_transaction(&transaction)
    }

    pub(super) fn resize_focused_window(
        &mut self,
        direction: FocusDirection,
    ) -> Result<(), String> {
        self.reconcile_editor_windows();
        if self.terminal_focused {
            let id = self
                .active_terminal
                .ok_or_else(|| "no terminal is selected".to_owned())?;
            return self
                .resize_native_window_direction(NativeSurfaceTarget::Terminal(id.0), direction);
        }
        if self.ui.explorer_focused {
            return self.resize_native_window_direction(NativeSurfaceTarget::Explorer, direction);
        }
        let (axis, grow) = match direction {
            FocusDirection::Left => (Axis::Horizontal, false),
            FocusDirection::Right => (Axis::Horizontal, true),
            FocusDirection::Up => (Axis::Vertical, true),
            FocusDirection::Down => (Axis::Vertical, false),
        };
        let step =
            i16::try_from(self.configuration.settings.ui.panes.resize_step).unwrap_or(i16::MAX);
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::ResizePaneOnAxis {
                axis,
                delta: if grow { step } else { -step },
            })
            .with_undo_policy(UndoPolicy::Ignore);
        self.apply_authoritative_editor_transaction(&transaction)
    }

    pub(super) fn resize_native_window_direction(
        &mut self,
        target: NativeSurfaceTarget,
        direction: FocusDirection,
    ) -> Result<(), String> {
        let placement = self.current_native_placement(target)?;
        let step = i16::try_from(self.configuration.settings.ui.terminal_panel.resize_step)
            .unwrap_or(i16::MAX);
        let placement = match placement {
            WindowPlacement::Docked { edge, size } => {
                let grow = match (edge, direction) {
                    (DockEdge::Bottom, FocusDirection::Up)
                    | (DockEdge::Top, FocusDirection::Down)
                    | (DockEdge::Left, FocusDirection::Right)
                    | (DockEdge::Right, FocusDirection::Left) => true,
                    (DockEdge::Bottom, FocusDirection::Down)
                    | (DockEdge::Top, FocusDirection::Up)
                    | (DockEdge::Left, FocusDirection::Left)
                    | (DockEdge::Right, FocusDirection::Right) => false,
                    _ => return Ok(()),
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
                    FocusDirection::Left => {
                        rect.width = rect.width.saturating_add_signed(-step).max(1)
                    }
                    FocusDirection::Right => {
                        rect.width = rect.width.saturating_add_signed(step).max(1)
                    }
                    FocusDirection::Up => {
                        rect.height = rect.height.saturating_add_signed(step).max(1)
                    }
                    FocusDirection::Down => {
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
        if matches!(target, NativeSurfaceTarget::Terminal(_))
            && let WindowPlacement::Docked { size, .. } = placement
        {
            self.terminal_panel_height = size;
        }
        self.apply_native_window_placement(target, placement)
    }

    pub(super) fn close_graph_window(&mut self, target: &str) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let window = self.resolve_graph_window_target(target)?;
        if let Some(native) = self.surface_windows.target_for_window(window) {
            return match native {
                NativeSurfaceTarget::Explorer => self.close_native_explorer(),
                NativeSurfaceTarget::Terminal(id) => {
                    self.active_terminal = Some(PtyId(id));
                    self.process_effect(AppEffect::TerminalClose);
                    Ok(())
                }
                NativeSurfaceTarget::Dashboard => {
                    if self.state.active_view.is_none() {
                        return Err("cannot close the only dashboard surface".into());
                    }
                    let mut graph = self.windows.clone();
                    let mut surfaces = self.surface_windows.clone();
                    surfaces
                        .close_target(&mut graph, native)
                        .map_err(|error| error.to_string())?;
                    self.ui.dashboard = false;
                    self.windows = graph;
                    self.surface_windows = surfaces;
                    self.reconcile_editor_windows();
                    Ok(())
                }
                NativeSurfaceTarget::Picker(kind) => {
                    let mut graph = self.windows.clone();
                    let mut surfaces = self.surface_windows.clone();
                    surfaces
                        .close_target(&mut graph, native)
                        .map_err(|error| error.to_string())?;
                    if self
                        .ui
                        .picker
                        .as_ref()
                        .is_some_and(|picker| picker.surface == kind)
                    {
                        self.close_picker();
                    }
                    self.windows = graph;
                    self.surface_windows = surfaces;
                    self.reconcile_editor_windows();
                    Ok(())
                }
            };
        }
        let pane = self.editor_pane_for_window(window).ok_or_else(|| {
            "window adapter has no close hook; plugin lifecycle is unavailable".to_owned()
        })?;
        let dirty = self
            .state
            .panes
            .get(pane)
            .and_then(|pane| self.state.views.get(pane.view))
            .and_then(|view| self.state.buffers.get(view.buffer))
            .is_some_and(|buffer| buffer.dirty);
        if dirty {
            return Err(self.message("dirty_close", &[]));
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::FocusPaneId { pane })
            .with_operation(EditorOperation::ClosePane)
            .with_undo_policy(UndoPolicy::Ignore);
        self.apply_authoritative_editor_transaction(&transaction)
    }

    pub(super) fn move_native_window(
        &mut self,
        target: NativeSurfaceTarget,
        x: u16,
        y: u16,
    ) -> Result<(), String> {
        let WindowPlacement::Floating(mut rect) = self.current_native_placement(target)? else {
            return Err("only floating ordinary windows can be moved".into());
        };
        rect.x = x;
        rect.y = y;
        self.apply_native_window_placement(target, WindowPlacement::Floating(rect))
    }

    pub(super) fn swap_graph_windows(&mut self, first: &str, second: &str) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let first_window = self.resolve_graph_window_target(first)?;
        let second_window = self.resolve_graph_window_target(second)?;
        if first_window == second_window {
            return Ok(());
        }
        let first_native = self.surface_windows.target_for_window(first_window);
        let second_native = self.surface_windows.target_for_window(second_window);
        match (first_native, second_native) {
            (Some(first), Some(second)) => self.swap_native_windows(first, second),
            (None, None) => {
                let first_pane = self.editor_pane_for_window(first_window).ok_or_else(|| {
                    "first window adapter has no authoritative swap hook".to_owned()
                })?;
                let second_pane = self.editor_pane_for_window(second_window).ok_or_else(|| {
                    "second window adapter has no authoritative swap hook".to_owned()
                })?;
                let transaction = EditorTransaction::new(self.transaction_id())
                    .with_operation(EditorOperation::SwapPanes {
                        first: first_pane,
                        second: second_pane,
                    })
                    .with_undo_policy(UndoPolicy::Ignore);
                self.apply_authoritative_editor_transaction(&transaction)
            }
            _ => Err("cannot swap editor and non-editor window adapters".into()),
        }
    }

    pub(super) fn swap_native_windows(
        &mut self,
        first: NativeSurfaceTarget,
        second: NativeSurfaceTarget,
    ) -> Result<(), String> {
        if first == second {
            return Ok(());
        }
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let mut graph = self.windows.clone();
        let mut surfaces = self.surface_windows.clone();
        let first_ids = surfaces
            .ids_for_target(first)
            .ok_or_else(|| "first native window is unavailable".to_owned())?;
        let second_ids = surfaces
            .ids_for_target(second)
            .ok_or_else(|| "second native window is unavailable".to_owned())?;
        let first_placement = graph
            .window(first_ids.window)
            .map(|window| window.placement)
            .ok_or_else(|| "first native window identity is stale".to_owned())?;
        let second_placement = graph
            .window(second_ids.window)
            .map(|window| window.placement)
            .ok_or_else(|| "second native window identity is stale".to_owned())?;
        graph
            .swap_non_tiled_placements(first_ids.window, second_ids.window)
            .map_err(|error| error.to_string())?;
        surfaces
            .place_target(first, second_placement)
            .map_err(|error| error.to_string())?;
        surfaces
            .place_target(second, first_placement)
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        self.windows = graph;
        self.surface_windows = surfaces;
        self.reconcile_editor_windows();
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        Ok(())
    }

    pub(super) fn close_native_explorer(&mut self) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let mut graph = self.windows.clone();
        let mut surfaces = self.surface_windows.clone();
        surfaces
            .close_target(&mut graph, NativeSurfaceTarget::Explorer)
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        self.windows = graph;
        self.surface_windows = surfaces;
        self.ui.explorer = None;
        self.ui.explorer_focused = false;
        self.reconcile_explorer_watch();
        self.reconcile_editor_windows();
        Ok(())
    }
}

pub(in crate::editor::session) fn configured_window_placements(
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

pub(in crate::editor::session) fn persisted_window_placement(
    placement: WindowPlacement,
) -> Option<PersistedWindowPlacement> {
    match placement {
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
        WindowPlacement::Tiled | WindowPlacement::Overlay(_) => None,
    }
}

pub(in crate::editor::session) fn restored_window_placement(
    placement: &PersistedWindowPlacement,
) -> Result<WindowPlacement, String> {
    let placement = match placement {
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

pub(in crate::editor::session) fn rect_contains_point(
    rect: WindowRect,
    column: u16,
    row: u16,
) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}
