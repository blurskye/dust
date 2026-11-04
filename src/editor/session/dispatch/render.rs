use super::super::windows::{
    CommandEffect, CommandError, CommandPolicy, WindowCommand, rect_contains_point,
};
use super::{
    AppEffect, CompositorProjection, DockEdge, FocusDirection, NativeSurfaceTarget, PtyId,
    WindowPlacement, WindowRect, explorer_panel_width,
};
use super::{
    BTreeMap, EditorMode, EditorOperation, EditorTransaction, EditorWindowBridge,
    EditorWindowLayoutState, LogLevel, NativeDescriptions, NativeSurfaceSignature,
    NativeSurfaceWindowBinding, Precondition, ProjectionRequest, TransactionId,
    UiEditorWindowBinding, UndoPolicy, ViewId, VisualRowAnchor, VisualRowModel,
    active_editor_pane_viewport, editor_state_content_width, screen_foot_rows, snap_display_column,
    snap_display_column_forward, terminal_panel_geometry, workspace_top_rows,
};
use super::{Duration, InteractiveApplication, LiveCrashSnapshot, PluginStatus};
use super::{PickerOrigin, PickerPayload, PickerSurfaceKind};
use crate::editor::session::search::MAXIMUM_PICKER_ITEMS;
use crate::editor::{RegisterShape, RegisterValue};
use crate::files::ResourceIntentKind;
use crate::ui::ExplorerUiState;
use crate::ui::render::{SCRATCH_LABEL, TERMINAL_PANEL_RESIZE_STEP};
use crate::ui::window::{
    PANE_DEFAULT_RATIO, PANE_MINIMUM_HEIGHT, PANE_MINIMUM_WIDTH, PANE_RESIZE_STEP,
};
use crate::workspace::{
    ExplorerKind, ExplorerLoadState, ExplorerNodeId, PickerAction, PickerItem, PickerItemId,
};

/// How long a notice stays on screen.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(4);

impl InteractiveApplication {
    pub(super) fn set_dashboard_visibility(&mut self, visible: bool) {
        if visible {
            self.window_session.open_dashboard();
        } else {
            self.window_session.close_dashboard();
        }
        self.project_window_activation();
    }
    pub(super) fn set_explorer_focus(&mut self, focused: bool) {
        if focused {
            self.window_session.focus_explorer();
        } else {
            self.window_session.leave_explorer();
        }
        self.project_window_activation();
    }
    pub(super) fn set_terminal_focus(&mut self, focused: bool) {
        if focused {
            if let Some(id) = self.terminal_session.selected() {
                self.terminal_session.display(id);
                self.window_session.focus_terminal(id.0);
            } else {
                self.window_session.leave_terminal();
            }
        } else {
            self.window_session.leave_terminal();
        }
        self.project_window_activation();
    }
    pub(super) fn project_window_activation(&mut self) {
        if let Some(id) = self
            .window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
        {
            self.terminal_session.display(id);
        }
        self.ui.dashboard = self.window_session.dashboard_visible();
        self.ui.explorer_focused = self.window_session.explorer_focused();
        if let Some(view) = self.window_session.focused_explorer() {
            let _ = self
                .explorer
                .activate_view(super::super::explorer::ExplorerViewId(view));
        }
    }
    pub(super) fn project_window_content_tabs(&mut self) {
        self.sync_ui_diagnostics();
        self.window_session.project_content_tabs(
            &(&*self.state).into(),
            self.diagnostics.counts(),
            self.configuration.settings(),
            self.configuration.generation(),
        );
        self.ui.content_tabs = self.window_session.content_tabs().to_vec();
    }
    pub(super) fn project_window_geometry(&mut self) {
        self.project_window_activation();
        self.ui.compositor_layout = self.window_session.layout().cloned();
        self.refresh_window_content();
        self.follow_agent_pane();
    }

    pub(super) fn refresh_window_content(&mut self) {
        self.sync_ui_diagnostics();
        self.retire_stale_picker();
        self.retire_stale_explorer_intents();
        self.window_session
            .project_content(self.ui.picker.as_ref(), self.configuration.settings());
        if let Some(geometry) = self.window_session.content().picker {
            if let Some(scroll) = self.search.picker_scroll(usize::from(geometry.list.height))
                && let Some(ui) = &mut self.ui.picker
            {
                ui.scroll = scroll;
            }
            let preview_rows = usize::from(geometry.preview_rows);
            if let Some(scroll) = self.search.picker_preview_scroll(preview_rows)
                && let Some(ui) = &mut self.ui.picker
            {
                ui.preview_side_scroll = (scroll, scroll);
            }
        }
        self.ui.surface_content = self.window_session.content().clone();
        self.project_input_documentation();
    }
    pub(super) fn project_input_documentation(&mut self) {
        if let Some(popup) = self.input.documentation() {
            let total_rows = crate::ui::render::overlays::documentation_display_rows(
                popup,
                self.configuration.settings(),
                self.window_session.width(),
            );
            self.input.scroll_documentation(
                0,
                0,
                self.window_session.content().documentation_rows.max(1),
                total_rows,
            );
        }
        let popup = self.input.documentation().cloned();
        self.ui.set(popup, |ui| &mut ui.documentation_popup);
    }
    pub(super) fn restore_input_focus(&mut self) {
        let Some(capture) = self.input.released_focus() else {
            return;
        };
        self.reconcile_editor_windows();
        if let Some(plan) = self.window_session.restore_focus_plan(
            capture,
            &self.state.window_version(),
            self.extension.contributions().version(),
        ) {
            let _ = self.apply_window_plan(plan);
        }
        self.project_window_activation();
    }
    pub(super) fn close_documentation(&mut self) {
        self.input.close_documentation();
        self.restore_input_focus();
        self.project_input_documentation();
    }

    pub(super) fn project_input_prompt(&mut self) {
        let prompt = self.input.prompt().cloned();
        self.ui.set(prompt, |ui| &mut ui.prompt);
        self.preview_substitute();
    }
    pub(super) fn prompt_help(&mut self, help: String) {
        self.input.prompt_help(help);
        self.project_input_prompt();
    }
    pub(super) fn prompt_error(&mut self, error: String) {
        self.input.prompt_error(error);
        self.project_input_prompt();
    }

    pub(super) fn project_configuration(&mut self) {
        self.ui.dashboard_actions = self.configuration.dashboard_actions();
    }
    pub(super) fn project_input_hints(&mut self) {
        let hints = self.input.hints().cloned();
        self.ui.set(hints, |ui| &mut ui.key_hints);
    }
    pub(super) fn notify_message(&mut self, message: String, error: bool) {
        self.notices
            .notify(message, error, self.clock.monotonic(), NOTICE_TIMEOUT);
        self.project_notice();
    }
    /// A small yellow notice: something is missing, but what could be shown is shown.
    pub(super) fn notify_warning(&mut self, message: String) {
        self.notices
            .warn(message, self.clock.monotonic(), NOTICE_TIMEOUT);
        self.project_notice();
    }
    pub(super) fn clear_notice(&mut self) {
        self.notices.clear();
        self.project_notice();
    }
    pub(super) fn project_notice(&mut self) {
        let current = self.notices.current();
        self.ui.notification = current.map(|(_, message)| message.to_owned());
        self.ui.notification_error = current.is_some_and(|(error, _)| error);
        self.ui.notification_warning = self.notices.is_warning();
    }
    /// Project the operation bar from the repository's REAL state. It is projected whenever the
    /// status is refreshed rather than per frame, and it is cleared when nothing is paused, so
    /// its presence always means an operation is genuinely in progress.
    /// Project the debug panes from the session's OWN state: what it is attached to or running,
    /// the breakpoints it holds, its watches and its console. Absent when there is no session, so
    /// the whole arrangement appears and disappears as ONE decision.
    pub(in crate::editor::session) fn project_debug_panes(&mut self) {
        self.project_debug_signs();
        let was_open = self.ui.debug_panes.is_some();
        let target = self
            .debugger_session
            .target()
            .filter(|_| self.debugger_session.is_running());
        let Some(target) = target else {
            self.ui.set(None, |ui| &mut ui.debug_panes);
            if was_open {
                self.window_session.set_debugging(false);
                self.sync_terminal_panel();
                // The windows make room for the panes, or take it back.
                self.window_session.invalidate();
                self.reconcile_editor_windows();
            }
            return;
        };
        let selected = self
            .ui
            .debug_panes
            .as_ref()
            .map_or(crate::ui::DebugPane::Sessions, |panes| panes.selected);
        let what = match target.request {
            crate::config::DebugRequestKind::Attach => match &target.target {
                Some(name) => format!("attached to {name}"),
                None => "attached".to_owned(),
            },
            crate::config::DebugRequestKind::Launch => match &target.program {
                Some(program) => format!("launched {}", self.workspace_relative_label(program)),
                None => "launched".to_owned(),
            },
        };
        let adapter = target.adapter.clone();
        let breakpoints = self
            .debugger_session
            .persisted_breakpoints()
            .iter()
            .map(|b| {
                let place = format!("{}:{}", self.workspace_relative_label(&b.path), b.line);
                match &b.condition {
                    Some(condition) => format!("{place} if {condition}"),
                    None => place,
                }
            })
            .collect();
        let threads = self.debugger_session.threads().to_vec();
        let stack = self.debugger_session.stack().to_vec();
        let watches = self
            .debugger_session
            .watches()
            .iter()
            .map(|watch| match self.debugger_session.watch_value(watch) {
                Some(value) => format!("{watch} = {value}"),
                None => watch.clone(),
            })
            .collect();
        let status = match self.debugger_session.stop_reason() {
            Some(reason) => format!("paused: {reason}"),
            None => "running".to_owned(),
        };
        let console = self
            .debugger_session
            .console()
            .map(|entry| entry.text.clone())
            .collect();
        self.ui.debug_panes = Some(crate::ui::DebugPanesUiState {
            selected,
            target: format!("{adapter}: {what}"),
            status,
            threads,
            stack,
            breakpoints,
            variables: self.debugger_session.variables().to_vec(),
            watches,
            console,
        });
        if !was_open {
            self.window_session.set_debugging(true);
            self.sync_terminal_panel();
            // The windows make room for the panes, or take it back.
            self.window_session.invalidate();
            self.reconcile_editor_windows();
        }
    }
    fn project_debug_signs(&mut self) {
        let mut signs: Vec<crate::ui::DebugSign> = self
            .debugger_session
            .persisted_breakpoints()
            .into_iter()
            .map(|breakpoint| crate::ui::DebugSign {
                path: breakpoint.path,
                line: breakpoint.line.saturating_sub(1) as usize,
                kind: if breakpoint.condition.is_some()
                    || breakpoint.hit_condition.is_some()
                    || breakpoint.log_message.is_some()
                {
                    crate::ui::DebugSignKind::ConditionalBreakpoint
                } else {
                    crate::ui::DebugSignKind::Breakpoint
                },
            })
            .collect();
        if let Some((path, line)) = self.debugger_session.execution() {
            // First, so it wins over a breakpoint on the same line.
            signs.insert(
                0,
                crate::ui::DebugSign {
                    path: path.clone(),
                    line: line.saturating_sub(1) as usize,
                    kind: crate::ui::DebugSignKind::Execution,
                },
            );
        }
        self.ui.debug_signs = signs;
    }
    pub(in crate::editor::session) fn project_operation_bar(&mut self) {
        let Some(status) = self.git.status() else {
            self.ui.operation_bar = None;
            return;
        };
        let repository = status.repository.clone();
        let state = match crate::workspace::git::read_conflict_state(&repository) {
            Ok(state) => state,
            Err(_) => {
                self.ui.operation_bar = None;
                return;
            }
        };
        self.ui.operation_bar = crate::workspace::git::GitOperationBar::of(&state).map(|bar| {
            crate::ui::OperationBarState {
                operation: bar.operation,
                phase: bar.phase,
                blockers: bar.blockers,
                actions: bar.actions.iter().map(|a| a.name().to_owned()).collect(),
            }
        });
    }
    pub(super) fn update_notification_timer(&mut self) {
        if self.notices.tick(self.clock.monotonic()) {
            self.project_notice();
        }
    }
    pub(in crate::editor::session) fn update_live_crash_snapshot(&mut self) {
        let now = self.clock.monotonic();
        if !self.notices.health_due(now) {
            return;
        }
        self.notices.submit_health(
            LiveCrashSnapshot {
                timestamp_ms: 0,
                roots: self.workspace.roots().len(),
                buffers: self.state.buffers.len(),
                dirty_buffers: self
                    .state
                    .buffers
                    .values()
                    .filter(|buffer| buffer.dirty)
                    .count(),
                jobs: self.tasks.running_count(),
                ptys: self.terminal_session.len(),
                lsp_sessions: self.language.sessions().len(),
                dap_running: self.debugger_session.is_running(),
                pending_effects: self
                    .documents
                    .pending_count()
                    .saturating_add(self.pending_preflight_count())
                    .saturating_add(self.tasks.linter_count()),
                plugins_failed: self
                    .extension
                    .statuses()
                    .values()
                    .filter(|status| matches!(status, PluginStatus::Failed(_)))
                    .count(),
                safe_mode: self.workspace.safe_mode(),
            },
            now,
            self.clock.system_time(),
        );
    }
}

impl InteractiveApplication {
    pub(super) fn project_large_file_modes(&mut self, buffers: Vec<crate::editor::BufferId>) {
        self.ui.set(buffers, |ui| &mut ui.large_file_buffers);
    }
}

impl InteractiveApplication {
    pub(super) fn sync_plugin_window_ui(&mut self) {
        let projection = self.compositor_projection().cloned();
        self.extension.synchronize_windows(projection.as_ref());
        self.ui.plugin_surfaces = self.extension.window_snapshots();
        self.ui.private_surfaces = self
            .extension
            .private_snapshots(self.window_session.graph());
    }

    pub(super) fn compositor_projection(&self) -> Option<&CompositorProjection> {
        self.window_session.current_for_editor(
            &self.state.window_version(),
            (self.window_session.width(), self.window_session.height()),
        )
    }

    pub(super) fn workspace_bounds(&self) -> Option<WindowRect> {
        self.compositor_projection()
            .map(|projection| projection.workspace_bounds)
    }

    pub(super) fn explorer_window_rect(&self) -> Option<WindowRect> {
        let layout = self.window_session.layout()?;
        let window = layout.surfaces.explorer?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }

    pub(super) fn displayed_terminal(&self) -> Option<PtyId> {
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
            .or(self.terminal_session.displayed())
    }
    /// Where the terminal's window is, if it is on screen.
    pub(super) fn terminal_rect(&self, id: PtyId) -> Option<WindowRect> {
        let window = self.window_session.native_ids().terminal(id.0)?.window;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
    pub(super) fn terminal_window_rect(&self) -> Option<WindowRect> {
        let layout = self.window_session.layout()?;
        let (_, window) = layout.surfaces.terminal?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }

    #[cfg(test)]
    pub(super) fn workspace_render_height(&self) -> u16 {
        self.workspace_bounds()
            .map_or(self.window_session.height(), |bounds| bounds.height)
    }

    #[cfg(test)]
    pub(super) fn terminal_body_dimensions(&self) -> Option<(u16, u16)> {
        self.compositor_projection()?;
        let rect = self.window_session.content().terminal?;
        Some((rect.width.max(1), rect.height.max(1)))
    }

    /// Sizes each terminal on screen to its window.
    pub(super) fn resize_active_terminal_to_panel(&mut self) {
        self.reconcile_editor_windows();
        for (id, window) in self.shown_terminal_windows() {
            let Some(geometry) = self
                .compositor_projection()
                .and_then(|projection| projection.window(window))
                .and_then(crate::ui::terminal_window_geometry)
            else {
                continue;
            };
            let Some(source) = self.window_session.terminal_geometry_source(id.0) else {
                self.show_error("terminal geometry source is missing, stale or foreign");
                return;
            };
            let (columns, rows) = (geometry.width.max(1), geometry.body_height.max(1));
            if let Err(error) =
                self.terminal_session
                    .resize(id, columns, rows, source, &self.workspace)
            {
                self.show_error(error);
            }
        }
    }

    pub(super) fn window_target_error(
        &self,
        error: crate::editor::session::windows::TargetError,
    ) -> String {
        match error {
            crate::editor::session::windows::TargetError::MissingTerminal => {
                self.message("terminal_missing", &[])
            }
            crate::editor::session::windows::TargetError::Rejected(error) => error,
        }
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
        self.window_session.place_native(target, placement)?;
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
        let Some(id) = self.displayed_terminal() else {
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
            size: explorer_panel_width(self.configuration.settings(), self.window_session.width())
                .unwrap_or(1)
                .saturating_add(1),
        };
        let terminal_fallback = WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: self.window_session.terminal_height().max(1),
        };
        self.window_session
            .show_native(target, explorer_fallback, terminal_fallback)?;
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
        self.window_session.native_placement(target)
    }

    pub(super) fn clone_graph_window(
        &mut self,
        target: &str,
        axis: crate::editor::Axis,
    ) -> Result<(), String> {
        let selected_terminal = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected_terminal)
            .map_err(|error| self.window_target_error(error))?;
        let explorer_source = match self.window_session.native_ids().target_for_window(window) {
            Some(NativeSurfaceTarget::Explorer) => Some(1),
            Some(NativeSurfaceTarget::ExplorerView(view)) => Some(view),
            _ => None,
        };
        let Some(source) = explorer_source else {
            return self.run_window_command(WindowCommand::Clone(target, axis, PANE_DEFAULT_RATIO));
        };
        let explorer = self
            .explorer
            .prepare_clone(super::super::explorer::ExplorerViewId(source))?;
        self.explorer.check_clone(&explorer)?;
        let plan = self
            .window_session
            .plan_window_command(
                WindowCommand::CloneExplorer(target, source, explorer.id().0, axis),
                &(&*self.state).into(),
                CommandPolicy {
                    selected_terminal,
                    pane_step: PANE_RESIZE_STEP,
                    terminal_step: TERMINAL_PANEL_RESIZE_STEP,
                },
                self.extension.contributions().version(),
            )
            .map_err(|error| match error {
                CommandError::Dirty => self.message("dirty_close", &[]),
                CommandError::Target(error) => self.window_target_error(error),
                CommandError::Rejected(error) => error,
            })?;
        self.explorer.check_clone(&explorer)?;
        match self.window_session.accept_window_command(
            plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
        )? {
            CommandEffect::ClonedExplorer(view) if view == explorer.id().0 => {}
            _ => return Err("explorer clone returned the wrong window effect".into()),
        }
        self.explorer.accept_clone(explorer)?;
        self.project_window_activation();
        self.reconcile_editor_windows();
        self.sync_explorer_ui();
        Ok(())
    }

    pub(super) fn place_graph_window(
        &mut self,
        target: &str,
        placement: WindowPlacement,
    ) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            self.apply_native_window_placement(native, placement)
        } else {
            self.run_window_command(WindowCommand::Place(target, placement))
        }
    }

    pub(super) fn show_graph_window(&mut self, target: &str) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            self.show_native_window(native)
        } else {
            self.run_window_command(WindowCommand::Show(target))
        }
    }

    pub(super) fn resize_graph_window(&mut self, target: &str, amount: i16) -> Result<(), String> {
        self.run_window_command(WindowCommand::Resize(target, amount))
    }

    pub(super) fn resize_focused_window(
        &mut self,
        direction: FocusDirection,
    ) -> Result<(), String> {
        self.reconcile_editor_windows();
        self.run_window_command(WindowCommand::Direction(direction, None))
    }

    #[cfg(test)]
    pub(super) fn resize_native_window_direction(
        &mut self,
        target: NativeSurfaceTarget,
        direction: FocusDirection,
    ) -> Result<(), String> {
        self.run_window_command(WindowCommand::Direction(direction, Some(target)))
    }

    pub(super) fn close_graph_window(&mut self, target: &str) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if self
            .window_session
            .editor_ids()
            .pane_for_window(window)
            .is_some()
        {
            self.reconcile_editor_windows();
            let window = self
                .window_session
                .resolve_graph_target(target, selected)
                .map_err(|error| self.window_target_error(error))?;
            let step = self
                .window_session
                .begin_window_close(window, &(&*self.state).into())?;
            self.handle_editor_close_step(step);
            Ok(())
        } else {
            self.run_window_command(WindowCommand::Close(target))
        }
    }

    pub(super) fn move_graph_window(&mut self, target: &str, x: u16, y: u16) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            let WindowPlacement::Floating(mut rect) = self.current_native_placement(native)? else {
                return Err("only floating ordinary windows can be moved".into());
            };
            rect.x = x;
            rect.y = y;
            self.apply_native_window_placement(native, WindowPlacement::Floating(rect))
        } else {
            self.run_window_command(WindowCommand::Move(target, x, y))
        }
    }

    pub(super) fn swap_graph_windows(&mut self, first: &str, second: &str) -> Result<(), String> {
        self.run_window_command(WindowCommand::Swap(first, second))
    }
}

impl InteractiveApplication {
    fn run_window_command(&mut self, command: WindowCommand<'_>) -> Result<(), String> {
        let plan = self
            .window_session
            .plan_window_command(
                command,
                &(&*self.state).into(),
                CommandPolicy {
                    // The terminal with the keys, as the one ctrl-h/l moved to need not be selected.
                    selected_terminal: self
                        .window_session
                        .focused_terminal()
                        .or(self.terminal_session.selected().map(|id| id.0)),
                    pane_step: PANE_RESIZE_STEP,
                    terminal_step: TERMINAL_PANEL_RESIZE_STEP,
                },
                self.extension.contributions().version(),
            )
            .map_err(|error| match error {
                CommandError::Dirty => self.message("dirty_close", &[]),
                CommandError::Target(error) => self.window_target_error(error),
                CommandError::Rejected(error) => error,
            })?;
        if !plan.operations().is_empty() {
            return self.apply_window_plan(plan);
        }
        match self.window_session.accept_window_command(
            plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
        )? {
            CommandEffect::None => {}
            CommandEffect::ClonedExplorer(_) => {
                return Err("explorer clone bypassed its source owner".into());
            }
            CommandEffect::ClosedExplorer => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == crate::ui::PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.ui.explorer_views.clear();
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
            }
            CommandEffect::ClosedExplorerView(view) => {
                self.explorer
                    .remove_view(super::super::explorer::ExplorerViewId(view))?;
                self.ui
                    .explorer_views
                    .retain(|candidate| candidate.view != view);
            }
            CommandEffect::ClosedPicker(kind) => {
                if self.ui.picker.as_ref().is_some_and(|p| p.surface == kind) {
                    self.close_picker();
                }
            }
            CommandEffect::CloseTerminal(id) => {
                self.terminal_session.select(PtyId(id));
                self.process_effect(AppEffect::TerminalClose);
            }
        }
        self.reconcile_editor_windows();
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.ensure_cursor_visible();
        Ok(())
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn move_explorer_selection(&mut self, delta: isize) {
        self.explorer.navigate(delta);
        self.sync_explorer_ui();
    }

    pub(in crate::editor::session) fn page_explorer_selection(&mut self, direction: isize) {
        let viewport = self
            .window_session
            .content()
            .explorer
            .map_or(0, |body| usize::from(body.height));
        self.move_explorer_selection(
            direction.saturating_mul(isize::try_from(viewport).unwrap_or(isize::MAX)),
        );
    }

    pub(in crate::editor::session) fn explorer_collapse_or_parent(&mut self) {
        self.explorer.parent();
        self.sync_explorer_ui();
    }

    // EXPLORER_WATCH_FOREGROUND_BEGIN: typed event coordination only.

    pub(in crate::editor::session) fn reconcile_explorer_watch(&mut self) {
        if self.ui.explorer.is_none() {
            self.retire_explorer_intents();
        }
        if let Err(error) = self.explorer.reconcile_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn accept_explorer_watch_batch(
        &mut self,
        batch: crate::files::ExplorerWatchBatch,
    ) {
        self.explorer
            .accept_watch(batch, self.ui.explorer.is_some());
    }

    pub(in crate::editor::session) fn process_explorer_watch_events(&mut self) {
        if let Err(error) = self.explorer.poll_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
        self.start_next_explorer_watch_reload();
    }

    pub(in crate::editor::session) fn start_next_explorer_watch_reload(&mut self) {
        if let Some(request) = self.explorer.next_reload() {
            self.load_explorer_request(request);
        }
    }

    // EXPLORER_WATCH_FOREGROUND_END

    pub(in crate::editor::session) fn sync_explorer_ui(&mut self) {
        self.refresh_window_content();
        if self.ui.explorer.is_none() {
            return;
        }
        let title = self.message("explorer_title", &[]);
        let root_label = crate::ui::workspace_title(&self.state);
        let explorer_windows = self
            .window_session
            .native_ids()
            .explorer_views()
            .filter_map(|(view, ids)| {
                let rect = self
                    .window_session
                    .layout()?
                    .projection
                    .window(ids.window)?
                    .rect;
                Some((view, ids.window, rect))
            })
            .collect::<Vec<_>>();
        let focused = self.window_session.focused_explorer();
        let policy = &self.configuration.settings().ui.explorer;
        let decorations = self.git.decorations();
        let mut primary = None;
        let mut clones = Vec::new();
        for (view, window, rect) in explorer_windows {
            let body = if view == 1 {
                self.window_session
                    .content()
                    .explorer
                    .unwrap_or_else(|| crate::ui::geometry::explorer_body_geometry(rect))
            } else {
                crate::ui::geometry::explorer_body_geometry(rect)
            };
            let Ok(projection) = self.explorer.projection_for(
                super::super::explorer::ExplorerViewId(view),
                policy,
                body,
                decorations,
                title.clone(),
                &root_label,
            ) else {
                continue;
            };
            if view == 1 {
                primary = Some(projection);
            } else {
                clones.push(crate::ui::ExplorerWindowUiState {
                    view,
                    window,
                    content: projection,
                    focused: focused == Some(view),
                });
            }
        }
        if primary.is_none() {
            let body = self.ui.surface_content.explorer.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
            primary = self
                .explorer
                .projection_for(
                    super::super::explorer::PRIMARY_EXPLORER_VIEW,
                    policy,
                    body,
                    decorations,
                    title,
                    &root_label,
                )
                .ok();
        }
        self.ui.explorer = primary;
        self.ui.explorer_views = clones;
        self.reconcile_explorer_watch();
        self.reconcile_editor_windows();
    }

    pub(in crate::editor::session) fn open_explorer(&mut self) {
        self.window_session.allow_explorer();
        if self.explorer.tree().root_revision() != self.workspace.root_revision()
            || self.explorer.tree().visible_ids().is_empty()
        {
            self.explorer
                .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        }
        self.ui.explorer = Some(ExplorerUiState {
            title: String::new(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            matches: 0,
            query: String::new(),
            searching: false,
        });
        self.set_explorer_focus(true);
        self.sync_explorer_ui();
        self.git.retry_status();
        self.start_git_status_refresh_silent();
        let root_to_load = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root_to_load {
            self.load_explorer_node(root);
        }
        self.ensure_cursor_visible();
    }

    pub(in crate::editor::session) fn load_explorer_node(&mut self, node: ExplorerNodeId) {
        match self.explorer.load(
            node,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }

    fn load_explorer_request(&mut self, request: super::super::explorer::DirectoryLoad) {
        match self.explorer.load_request(
            request,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn follow_active_file_in_explorer(&mut self) {
        let Some(path) = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        if self.workspace.root_for_canonical_path(&path).is_none() {
            self.show_error("active file is outside workspace roots");
            return;
        }
        self.open_explorer();
        if self.ui.explorer.is_some() {
            self.explorer.follow(path);
            self.continue_explorer_follow();
        }
    }

    pub(in crate::editor::session) fn continue_explorer_follow(&mut self) {
        match self.explorer.continue_follow() {
            Ok(Some(request)) => self.load_explorer_request(request),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        self.sync_explorer_ui();
    }

    /// Filter the tree by `query`; the folders not read yet are read in the background so
    /// the whole project is searched.
    pub(in crate::editor::session) fn search_explorer(&mut self, query: String) {
        self.explorer.set_search(query);
        self.start_next_explorer_watch_reload();
        self.sync_explorer_ui();
    }

    pub(in crate::editor::session) fn explorer_toggle(&mut self) {
        let Some(node) = self.explorer.tree().selected_node().cloned() else {
            return;
        };
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => match node.load {
                ExplorerLoadState::Loaded => {
                    self.explorer.toggle_loaded(node.id);
                    self.sync_explorer_ui();
                }
                _ => self.load_explorer_node(node.id),
            },
            ExplorerKind::File | ExplorerKind::Symlink => self.explorer_open_file(false),
        }
    }

    pub(in crate::editor::session) fn process_explorer_result(&mut self) {
        let Some(result) = self.explorer.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        if let Err(error) = result {
            self.show_error(error);
        }
        self.explorer.select_search_match();
        self.sync_explorer_ui();
        self.continue_explorer_follow();
        self.start_next_explorer_watch_reload();
    }

    pub(in crate::editor::session) fn yank_explorer_path(&mut self) {
        let Some(node) = self.explorer.tree().selected_node() else {
            self.show_error("select an explorer path first");
            return;
        };
        let node_path = node.path.clone();
        let text = node_path.display().to_string();
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text: text.clone(),
                shape: RegisterShape::Character,
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        let label = self.workspace_relative_label(&node_path);
        self.notify_message(format!("yanked `{label}`"), false);
    }

    pub(in crate::editor::session) fn explorer_mouse_select(&mut self) {
        let Some(id) = self.explorer.take_clicked() else {
            return;
        };
        self.explorer.select(id);
        if self.window_session.focused_explorer().unwrap_or(1) == 1 {
            self.set_explorer_focus(true);
        }
        self.sync_explorer_ui();
        // As in neo-tree and VS Code, a click opens a file and toggles a folder.
        self.explorer_open_file(false);
    }

    /// The tree row under the pointer in the layout now on screen.
    pub(super) fn explorer_row_at(
        &self,
        window: crate::ui::window::WindowId,
        column: u16,
        row: u16,
    ) -> Option<ExplorerNodeId> {
        let view = match self.window_session.native_ids().target_for_window(window)? {
            NativeSurfaceTarget::Explorer => 1,
            NativeSurfaceTarget::ExplorerView(view) => view,
            _ => return None,
        };
        let rect = self.compositor_projection()?.window(window)?.rect;
        let body = crate::ui::geometry::explorer_body_geometry(rect);
        if !rect_contains_point(body, column, row) {
            return None;
        }
        let explorer_ui = if view == 1 {
            self.ui.explorer.as_ref()
        } else {
            self.ui
                .explorer_views
                .iter()
                .find(|candidate| candidate.view == view)
                .map(|candidate| &candidate.content)
        };
        let index = usize::from(row.saturating_sub(body.y));
        explorer_ui?
            .rows
            .get(index)
            .map(|row| ExplorerNodeId(row.node_id))
    }

    pub(in crate::editor::session) fn refresh_explorer(&mut self) {
        self.retire_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.sync_explorer_ui();
        let root = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root {
            self.load_explorer_node(root);
        }
    }
}

impl InteractiveApplication {
    /// A path as the reader knows it in the project, or whole when it lies outside.
    /// A short name for a notification: outside the workspace a full path would fill the toast.
    pub(super) fn notice_file_label(&self, path: &std::path::Path) -> String {
        let label = self.workspace_relative_label(path);
        if std::path::Path::new(&label).is_absolute() {
            path.file_name()
                .map_or(label, |name| name.to_string_lossy().into_owned())
        } else {
            label
        }
    }

    pub(super) fn workspace_relative_label(&self, path: &std::path::Path) -> String {
        self.workspace
            .roots()
            .iter()
            .find_map(|root| path.strip_prefix(&root.identity.canonical_path).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }

    pub(in crate::editor::session) fn show_workspace_proposal(&mut self) {
        let Some(proposal) = self.workspace_edits.proposal() else {
            return;
        };
        let mut items = proposal
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let id = PickerItemId(format!(
                    "proposal-file:{index}:{}",
                    file.canonical_path.display()
                ));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: self.workspace_relative_label(&file.canonical_path),
                        detail: self.message(
                            if file.selected {
                                "workspace_edit_included"
                            } else {
                                "workspace_edit_excluded"
                            },
                            &[("count", file.edit_count.to_string())],
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::ProposalFile(index),
                )
            })
            .collect::<Vec<_>>();
        items.extend(
            self.workspace_edits
                .resources()
                .iter()
                .enumerate()
                .map(|(index, intent)| {
                    let detail = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => self.message(
                            "workspace_resource_rename_row",
                            &[("target", target.display().to_string())],
                        ),
                        ResourceIntentKind::Delete => {
                            self.message("workspace_resource_delete_row", &[])
                        }
                    };
                    (
                        PickerItem {
                            badge: String::new(),
                            kind: crate::workspace::PickerItemKind::Entry,
                            id: PickerItemId(format!("resource-intent:{index}")),
                            label: self.workspace_relative_label(&intent.source),
                            detail,
                            source: self.message("workspace_resource_staged", &[]),
                            action: PickerAction::None,
                        },
                        PickerPayload::Command(String::new()),
                    )
                }),
        );
        let title = proposal.label.clone();
        let id = proposal.id;
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            title,
            items,
            String::new(),
            true,
            PickerOrigin::WorkspaceEdit(id),
        );
    }
}

impl InteractiveApplication {
    pub(super) fn set_mode(&mut self, mode: EditorMode) {
        if let Some(view) = self.state.active_view {
            self.set_specific_mode(view, mode);
        }
    }

    pub(super) fn set_specific_mode(&mut self, view: ViewId, mode: EditorMode) {
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewExists(view))
            .with_operation(EditorOperation::SetMode { view, mode })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    pub(super) fn ensure_cursor_visible(&mut self) {
        self.reconcile_editor_windows();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let Some(viewport) = active_editor_pane_viewport(&self.state, &self.ui) else {
            return;
        };
        if viewport.view != view_id {
            return;
        }
        let body_height = usize::from(viewport.height.max(1));
        let vertical_margin = usize::from(
            self.configuration
                .settings()
                .editor
                .viewport
                .vertical_margin,
        )
        .min(body_height.saturating_sub(1) / 2);
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings().editor.wrap
            && !self.ui.large_file_buffers.contains(&view.buffer);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap_enabled,
        );
        let start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let first_anchor = match visual_rows.screen_row(start_anchor, cursor_anchor, body_height) {
            Some(row) if row < vertical_margin => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            Some(row) if row >= body_height.saturating_sub(vertical_margin) => visual_rows
                .backward(
                    cursor_anchor,
                    body_height
                        .saturating_sub(vertical_margin)
                        .saturating_sub(1),
                ),
            Some(_) => start_anchor,
            None if cursor_anchor < start_anchor => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            None => visual_rows.backward(
                cursor_anchor,
                body_height
                    .saturating_sub(vertical_margin)
                    .saturating_sub(1),
            ),
        };
        let first = first_anchor.line;
        // Without wrap only a markdown table's row takes more than one row.
        let first_wrap_row = first_anchor.wrap_row;
        let first_column = if wrap_enabled || available == 0 {
            0
        } else {
            let source_column = visual_rows.cursor_projection(view.cursor).1;
            let line_start = buffer
                .text
                .line_to_byte(line)
                .unwrap_or(crate::editor::text::ByteOffset(0));
            let mut hints = self
                .ui
                .lsp_inlay_hints
                .iter()
                .chain(&self.ui.substitute_preview)
                .filter(|hint| {
                    hint.buffer == view.buffer
                        && hint.revision == buffer.text.revision()
                        && hint.position >= line_start
                        && hint.position <= view.cursor
                })
                .take(10_000)
                .collect::<Vec<_>>();
            hints.sort_unstable_by_key(|hint| hint.position);
            let virtual_width = hints
                .into_iter()
                .take(32)
                .map(|hint| {
                    crate::ui::geometry::text_width(
                        &hint.label,
                        self.configuration.settings().editor.ambiguous_width,
                    )
                })
                .sum::<usize>();
            let cursor_column = source_column.saturating_add(virtual_width);
            let margin = usize::from(
                self.configuration
                    .settings()
                    .editor
                    .viewport
                    .horizontal_margin,
            )
            .min(available.saturating_sub(1));
            let target = if cursor_column < view.scroll_column.saturating_add(margin) {
                cursor_column.saturating_sub(margin)
            } else if cursor_column
                >= view
                    .scroll_column
                    .saturating_add(available.saturating_sub(margin))
            {
                cursor_column.saturating_sub(available.saturating_sub(margin).saturating_sub(1))
            } else {
                view.scroll_column
            }
            .min(source_column);
            let line_text = buffer.text.line(line).unwrap_or_default();
            let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);
            let snapped = snap_display_column(
                line_text,
                target,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            );
            if cursor_column.saturating_sub(snapped) < available {
                snapped
            } else {
                snap_display_column_forward(
                    line_text,
                    cursor_column.saturating_add(1).saturating_sub(available),
                    self.configuration.settings().editor.tab_width,
                    self.configuration.settings().editor.ambiguous_width,
                )
            }
        };
        if first == view.scroll_line
            && first_wrap_row == view.scroll_wrap_row
            && first_column == view.scroll_column
        {
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row,
            })
            .with_operation(EditorOperation::SetScrollColumn {
                view: view_id,
                first_column,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    pub(super) fn transaction_id(&mut self) -> TransactionId {
        let id = TransactionId(self.next_transaction);
        self.next_transaction = self.next_transaction.saturating_add(1);
        id
    }

    pub(super) fn message(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut message = self
            .configuration
            .settings()
            .messages
            .get(key)
            .expect("resolved configuration validates required messages")
            .clone();
        for (name, value) in replacements {
            message = message.replace(&format!("{{{name}}}"), value);
        }
        message
    }

    pub(super) fn show_error(&mut self, error: impl std::fmt::Display) {
        self.notify_message(error.to_string(), true);
    }
}

impl InteractiveApplication {
    pub(super) fn native_surface_signature(
        &self,
        editor: &EditorWindowBridge,
    ) -> NativeSurfaceSignature {
        let explorer_size = self
            .ui
            .explorer
            .as_ref()
            .and_then(|_| {
                explorer_panel_width(self.configuration.settings(), self.window_session.width())
            })
            .map_or(1, |width| width.saturating_add(1));
        let terminal_size = self
            .displayed_terminal()
            .and_then(|_| {
                terminal_panel_geometry(
                    self.window_session.height(),
                    self.window_session.terminal_height(),
                )
            })
            .map_or(1, |geometry| geometry.height);
        let picker_rect = self.ui.picker.as_ref().and_then(|picker| {
            crate::ui::geometry::picker_window_geometry(
                picker,
                WindowRect {
                    x: 0,
                    y: 0,
                    width: self.window_session.width(),
                    height: self.window_session.height(),
                },
            )
        });
        NativeSurfaceSignature {
            editor_tab: editor.active_tab(&(&*self.state).into()),
            editor_window: editor.focused_window(&(&*self.state).into()),
            explorer_visible: self.ui.explorer.is_some(),
            explorer_views: self
                .explorer
                .view_ids()
                .map(|view| view.0)
                .collect::<Vec<_>>()
                .into(),
            focused_explorer: self
                .ui
                .explorer
                .as_ref()
                .and_then(|_| self.window_session.focused_explorer()),
            explorer_size,
            active_terminal: self.displayed_terminal().map(|id| id.0),
            shown_terminals: self
                .terminal_session
                .displayed()
                .into_iter()
                .chain(self.terminal_session.side_terminals())
                .map(|id| id.0)
                .collect::<Vec<_>>()
                .into(),
            terminal_focused: self.displayed_terminal().is_some_and(|id| {
                self.window_session.focused_terminal() == Some(id.0)
                    && self.window_session.native_ids().terminal_placement(id.0)
                        != Some(WindowPlacement::Hidden)
            }),
            terminal_size,
            terminal_shrunk: self.window_session.terminal_shrunk(),
            terminal_set_revision: self.terminal_session.revision(),
            terminal_width: self.window_session.width(),
            terminal_height: self.window_session.height(),
            dashboard_visible: self.window_session.dashboard_visible()
                || self.state.active_view.is_none(),
            picker_rect,
            picker_surface: self
                .ui
                .picker
                .as_ref()
                .filter(|_| picker_rect.is_some())
                .map(|picker| picker.surface),
            protected_prompt_height: self.input.prompt().map_or(0, |prompt| {
                let (width, height) = crate::ui::prompt_surface_dimensions(
                    &self.state,
                    &self.ui,
                    self.window_session.width(),
                    self.window_session.height(),
                );
                crate::ui::prompt_geometry_for_state(
                    width,
                    height,
                    self.configuration.settings(),
                    prompt,
                )
                .height
            }),
            configuration_generation: self.configuration.generation(),
            screen_bottom: screen_foot_rows(
                &self.state,
                &self.ui,
                self.configuration.settings(),
                self.window_session.height(),
            ),
        }
    }

    pub(super) fn reconcile_editor_windows(&mut self) {
        let scratch_label = &SCRATCH_LABEL;
        let editor_current = self
            .window_session
            .editor_ids()
            .is_current(&(&*self.state).into(), scratch_label);
        let signature = self.native_surface_signature(self.window_session.editor_ids());
        if editor_current
            && self.window_session.native_ids().is_current(&signature)
            && self.window_session.graph().revision() == self.ui.window_graph_revision
            && self
                .window_session
                .content_current(self.configuration.generation())
            && self.compositor_projection().is_some()
        {
            return;
        }
        let result = if editor_current && self.window_session.native_ids().is_current(&signature) {
            Ok(())
        } else {
            let terminals = self.terminal_session.descriptions();
            self.window_session
                .prepare_reconcile(
                    &(&*self.state).into(),
                    scratch_label,
                    NativeDescriptions {
                        signature: self.native_surface_signature(self.window_session.editor_ids()),
                        explorer_title: &self.message("explorer_title", &[]),
                        dashboard_title: &self.configuration.dashboard().identity,
                        picker_title: self
                            .ui
                            .picker
                            .as_ref()
                            .map_or("Picker", |picker| &picker.title),
                        terminals: &terminals,
                    },
                )
                .and_then(|candidate| {
                    self.window_session
                        .accept_current(candidate, &self.state.window_version())
                })
        };
        match result {
            Ok(()) => {
                self.project_window_content_tabs();
                let graph = self.window_session.graph();
                let bridge = self.window_session.editor_ids();
                let surfaces = self.window_session.native_ids();
                self.ui.editor_window_layout = self.state.window_revision().and_then(|revision| {
                    bridge
                        .active_layout(&(&*self.state).into(), graph)
                        .map(|active| EditorWindowLayoutState {
                            source_revision: revision,
                            layout: active.layout,
                            bindings: active
                                .bindings
                                .into_iter()
                                .map(|binding| UiEditorWindowBinding {
                                    window: binding.window,
                                    pane: binding.pane,
                                    view: binding.view,
                                })
                                .collect(),
                        })
                });
                self.ui.window_graph_revision = graph.revision();
                let projection = self.state.window_revision().and_then(|revision| {
                    let graph_revision = graph.revision()?;
                    let tab = bridge
                        .active_tab(&(&*self.state).into())
                        .or_else(|| surfaces.workspace_tab())?;
                    let snapshot = graph.compositor_snapshot(tab)?;
                    let screen_bottom = screen_foot_rows(
                        &self.state,
                        &self.ui,
                        self.configuration.settings(),
                        self.window_session.height(),
                    );
                    let preliminary = snapshot.project_with_tiled_insets(
                        WindowRect {
                            x: 0,
                            y: 0,
                            width: self.window_session.width(),
                            height: self.window_session.height(),
                        },
                        PANE_MINIMUM_WIDTH,
                        PANE_MINIMUM_HEIGHT,
                        crate::ui::window::TiledInsets {
                            screen_bottom,
                            ..Default::default()
                        },
                    );
                    let top =
                        workspace_top_rows(&self.state, &self.ui, self.configuration.settings());
                    let (left, right, bottom_panes) = self
                        .ui
                        .debug_panes
                        .as_ref()
                        .filter(|_| !self.ui.dashboard && self.state.active_view.is_some())
                        .and_then(|_| {
                            crate::ui::debug_pane_insets(
                                preliminary.workspace_bounds.width,
                                preliminary.workspace_bounds.height.saturating_sub(top),
                            )
                        })
                        .unwrap_or_default();
                    let protected = self.input.prompt().map(|prompt| {
                        let prompt_height = preliminary.workspace_bounds.height.saturating_sub(top);
                        let geometry = crate::ui::prompt_geometry_for_state(
                            self.window_session.width(),
                            prompt_height,
                            self.configuration.settings(),
                            prompt,
                        );
                        WindowRect {
                            x: geometry.left,
                            y: geometry.top,
                            width: geometry.width,
                            height: geometry.height,
                        }
                    });
                    Some(ProjectionRequest {
                        tab,
                        source_revision: revision,
                        graph_revision,
                        bounds: preliminary.bounds,
                        minimum_size: (PANE_MINIMUM_WIDTH, PANE_MINIMUM_HEIGHT),
                        insets: crate::ui::window::TiledInsets {
                            screen_bottom,
                            top,
                            bottom: bottom_panes,
                            left,
                            right,
                        },
                        protected,
                        surfaces: NativeSurfaceWindowBinding {
                            explorer: surfaces.explorer().map(|ids| ids.window),
                            terminal: self.displayed_terminal().and_then(|id| {
                                surfaces.terminal(id.0).map(|ids| (id.0, ids.window))
                            }),
                            dashboard: surfaces.dashboard().map(|ids| ids.window),
                            picker: surfaces.picker().map(|ids| ids.window),
                        },
                    })
                });
                if let Some(projection) = projection {
                    self.window_session.project(projection);
                }
                self.project_window_geometry();
                self.sync_explorer_ui();
                self.sync_plugin_window_ui();
            }
            Err(error) => self.notices.record(
                LogLevel::Error,
                "windows",
                "window-reconciliation-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            ),
        }
    }
}
