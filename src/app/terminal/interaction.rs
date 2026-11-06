use crate::app::DockEdge;
use crate::app::App;
use crate::app::NativeSurfaceTarget;
use crate::app::PromptKind;
use crate::app::TerminalUpdate;
use crate::app::TrustedAction;
use crate::app::WindowPlacement;
use crate::app::WindowRect;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::app::PtyId;
use crate::app::RegisterShape;
use crate::app::RegisterValue;
use crate::app::PtyId;
use crate::windows::WindowPlacement;
use crate::editor::RegisterShape;
use crate::editor::RegisterValue;



impl App {

    /// Shows a terminal and gives it the keys, bringing its window back if it was hidden.
    pub(in crate::app) fn reveal_terminal(&mut self, id: PtyId) {
        self.terminal_session.select(id);
        self.terminal_session.display(id);
        if self.terminal_rect(id).is_none()
            && let Err(error) = self.show_native_window(NativeSurfaceTarget::Terminal(id.0))
        {
            self.show_error(error);
            return;
        }
        self.set_terminal_focus(true);
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn close_terminal(&mut self, id: PtyId) {
        if self.terminal_session.stopping(id) {
            self.show_error("terminal stop is already pending");
        } else if self.terminal_session.requires_close_confirmation(id) {
            if self.input.prompt().is_some() {
                self.show_error("close the active prompt before closing a running terminal");
            } else if self.terminal_session.begin_close(id) {
                self.open_confirmation_actions(
                    PromptKind::TerminalClose,
                    format!("Terminal #{} is active", id.0),
                    "Hide this terminal or stop its process?".into(),
                    crate::ui::ConfirmationChoice::No,
                    "Stop",
                    "Hide",
                );
            }
        } else {
            self.terminal_session.cancel_open();
            self.retire_terminal(id, true);
        }
    }
}

impl App {


    pub(in crate::app) fn submit_terminal_close(&mut self, selected: crate::ui::ConfirmationChoice) {
        let source = self.terminal_session.take_close();
        // Restore the pre-prompt focus first; Hide or Stop then performs the
        // accepted final focus transition without the prompt lease undoing it.
        self.close_prompt();
        let id = match source {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match selected {
            crate::ui::ConfirmationChoice::Yes => {
                match self
                    .window_session
                    .require_native_stop(NativeSurfaceTarget::Terminal(id.0))
                {
                    Ok(()) => {
                        self.terminal_session.cancel_open();
                        self.retire_terminal(id, true);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            crate::ui::ConfirmationChoice::No => {
                if let Err(error) = self
                    .window_session
                    .place_native(NativeSurfaceTarget::Terminal(id.0), WindowPlacement::Hidden)
                {
                    self.show_error(error);
                }
            }
        }
    }
}

impl App {

    fn terminal_command_target(&self) -> Option<PtyId> {
        self.keyboard_terminal()
            .or(self.terminal_session.selected())
    }
}

impl App {

    pub(in crate::app) fn reject_terminal_responses(&mut self, reason: &str) {
        self.debugger_session.reject_terminals(reason);
    }
}

impl App {


    /// Shows the terminal as a centered float covering most of the screen, like LazyVim's
    /// lazygit window.
    fn float_terminal(&mut self, id: PtyId) {
        self.terminal_session.display(id);
        let (width, height) = (self.window_session.width(), self.window_session.height());
        let rect = WindowRect {
            x: width / 20,
            y: height / 20,
            width: width - width / 10,
            height: height - height / 10,
        };
        self.window_session
            .prefer_terminal_placement(id.0, WindowPlacement::Floating(rect));
    }
}

impl App {


    /// A terminal running an agent profile's own program, for `agents.connection = "terminal"`.
    pub(in crate::app) fn agent_terminal(&self, id: PtyId) -> bool {
        self.terminal_session.is_agent(id)
    }
}

impl App {


    /// An agent's terminal opens at the agent panel's width on its side, where the ACP panel goes.
    fn dock_agent_terminal(&mut self, id: PtyId) {
        self.terminal_session.set_side(id);
        let width = u32::from(self.window_session.width());
        let share = width * u32::from(self.configuration.settings().agents.panel_width) / 100;
        // A conversation needs about 40 columns, however narrow the terminal is.
        let size = share.max(40).min(width * 7 / 10).max(1);
        self.window_session.prefer_terminal_placement(
            id.0,
            WindowPlacement::Docked {
                edge: match self.configuration.settings().agents.side {
                    crate::config::PanelSide::Left => DockEdge::Left,
                    crate::config::PanelSide::Right => DockEdge::Right,
                },
                size: u16::try_from(size).unwrap_or(u16::MAX),
            },
        );
    }
}

impl App {


    pub(in crate::app) fn retire_terminal(&mut self, id: PtyId, request_close: bool) {
        let focused = self.window_session.focused_terminal() == Some(id.0);
        if request_close {
            match self.terminal_session.request_stop(id) {
                Ok(true) => {
                    if focused {
                        self.set_terminal_focus(false);
                    }
                    self.sync_terminal_panel();
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        } else {
            self.terminal_session.close(id);
        }
        if let Some(reply) = self.debugger_session.take_terminal(&id) {
            let _ = reply.respond(
                false,
                Some("terminal closed before startup response".into()),
            );
        }
        if focused {
            self.set_terminal_focus(self.terminal_session.selected().is_some());
        }
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.ensure_cursor_visible();
    }
}

impl App {

    pub(in crate::app) fn enter_terminal_normal_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_normal_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn enter_terminal_input_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_input_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn move_terminal_normal_cursor(&mut self, motion: &str) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        if let Err(error) = self
            .terminal_session
            .move_terminal_normal_cursor(id, motion)
        {
            self.show_error(error);
        }
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn start_terminal_visual(&mut self, linewise: bool) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.start_terminal_visual(id, linewise);
        }
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn yank_terminal_text(&mut self, line: bool) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        let Some((text, linewise)) = self.terminal_session.yank_terminal_text(id, line) else {
            return;
        };
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text,
                shape: if linewise {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                },
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        self.notify_message(self.message("terminal_yanked", &[]), false);
        self.sync_terminal_panel();
    }
}

impl App {

    pub(in crate::app) fn sync_terminal_panel(&mut self) {
        let focused = self.window_session.focused_terminal();
        let height = self.window_session.terminal_height();
        self.ui.terminal_panel = self.displayed_terminal().and_then(|id| {
            self.terminal_session
                .projection(id, focused == Some(id.0), height)
        });
        if self.ui.terminal_panel.is_none() {
            self.set_terminal_focus(false);
        }
        self.reconcile_editor_windows();
        let active = self.ui.terminal_panel.as_ref().map(|terminal| terminal.id);
        self.ui.other_terminals = self
            .shown_terminal_windows()
            .into_iter()
            .filter(|(id, _)| Some(id.0) != active)
            .filter_map(|(id, window)| {
                Some((window, self.terminal_session.projection(id, false, height)?))
            })
            .collect();
    }
}

impl App {


    /// Every terminal with a window on screen, the active one first.
    pub(in crate::app) fn shown_terminal_windows(&self) -> Vec<(PtyId, crate::windows::WindowId)> {
        let Some(projection) = self.compositor_projection() else {
            return Vec::new();
        };
        self.displayed_terminal()
            .into_iter()
            .chain(self.terminal_session.displayed())
            .chain(self.terminal_session.side_terminals())
            .fold(Vec::new(), |mut ids, id| {
                if !ids.contains(&id) {
                    ids.push(id);
                }
                ids
            })
            .into_iter()
            .filter_map(|id| {
                let window = self.window_session.native_ids().terminal(id.0)?.window;
                projection.window(window).map(|_| (id, window))
            })
            .collect()
    }
}

impl App {

    pub(in crate::app) fn request_terminal_open(
        &mut self,
        profile: &str,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("requesting debugger retired");
            return;
        }
        let file = self.state.active_view.and_then(|view| {
            let view = &self.state.views[view];
            let buffer = &self.state.buffers[view.buffer];
            let line = buffer
                .text
                .byte_to_line(view.cursor)
                .map_or(1, |line| line + 1);
            buffer.path.as_deref().map(|path| (path, line))
        });
        match self.terminal_session.request_open(
            profile,
            &self.workspace,
            file,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
            self.input.capture_epoch(),
        ) {
            Ok(intent) => self.run_trusted(if let Some(reply) = reply {
                TrustedAction::OpenDapTerminal { intent, reply }
            } else {
                TrustedAction::OpenTerminal(intent)
            }),
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }
}

impl App {


    pub(in crate::app) fn open_terminal_with_response(
        &mut self,
        intent: crate::app::terminal::TerminalIntent,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("the debugger that asked for this terminal has stopped");
            return;
        }
        let for_debugger = reply.is_some();
        // A debugged program's output stays docked under the source it is stepping through.
        let placement = if for_debugger {
            crate::config::TerminalPlacement::Bottom
        } else {
            self.configuration.settings().ui.terminal_panel.placement
        };
        match self.terminal_session.prepare(intent, &self.workspace) {
            Ok(open) => {
                let id = self
                    .terminal_session
                    .resolve(&open.key)
                    .expect("new terminal");
                match self.terminal_session.queue_preflight(
                    open.key.clone(),
                    open.request,
                    self.workspace
                        .execution_lane(crate::app::workspace::DiscoveryKind::Language),
                ) {
                    Ok(()) => {
                        if let Some(reply) = reply {
                            self.terminal_session.set_debugger_owned(id);
                            self.debugger_session.register_terminal(id, reply);
                            self.close_debug_terminal();
                            self.debug_terminal = Some(id);
                        }
                        if self.window_session.navigation_current(
                            &open.navigation,
                            &self.state.window_version(),
                            self.state.active_view,
                        ) && self.input.capture_current(&open.capture)
                            && self.input.capture_context().is_none()
                            && self.ui.picker.is_none()
                            && self.ui.tool_menu.is_none()
                            && self.ui.settings.is_none()
                        {
                            self.terminal_session.select(id);
                            self.terminal_session.display(id);
                            if self.agent_terminal(id) {
                                self.dock_agent_terminal(id);
                            } else if open.float
                                || placement == crate::config::TerminalPlacement::Float
                            {
                                self.float_terminal(id);
                            } else if placement == crate::config::TerminalPlacement::Fullscreen {
                                self.window_session
                                    .prefer_terminal_placement(id.0, WindowPlacement::Fullscreen);
                            }
                            // A debugged program's terminal shows, and the user stays on the source.
                            if !for_debugger {
                                self.set_terminal_focus(true);
                            }
                        }
                        self.resize_active_terminal_to_panel();
                        self.sync_terminal_panel();
                        self.sync_explorer_ui();
                        self.ensure_cursor_visible();
                    }
                    Err(error) => {
                        self.terminal_session.reject(&open.key);
                        if let Some(reply) = reply {
                            let _ = reply.respond(false, Some(error.to_string()));
                        }
                        self.show_error(error);
                    }
                }
            }
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }
}

impl App {


    pub(in crate::app) fn process_pty_events(&mut self) {
        self.terminal_session.set_theme(self.configuration.theme());
        let windows = &self.window_session;
        let updates = self
            .terminal_session
            .drain(&self.workspace, |id| windows.terminal_geometry_source(id.0));
        if updates.is_empty() {
            return;
        }
        // What ran there may have committed or switched branch.
        let ended = updates.iter().any(|update| {
            matches!(
                update,
                TerminalUpdate::Exited(..)
                    | TerminalUpdate::IoComplete(_)
                    | TerminalUpdate::Closed(_)
            )
        });
        for update in updates {
            match update {
                TerminalUpdate::Started(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id)
                        && let Err(error) = reply.respond(true, None)
                    {
                        self.show_error(error);
                    }
                }
                // Nothing is left to type to; keys go back to the source.
                TerminalUpdate::Exited(id, _)
                    if self.debug_terminal == Some(id)
                        && self.window_session.focused_terminal() == Some(id.0) =>
                {
                    self.set_terminal_focus(false);
                }
                TerminalUpdate::Exited(_, _) | TerminalUpdate::IoComplete(_) => {}
                TerminalUpdate::StopUnknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ =
                            reply.respond(false, Some("terminal stop outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal stop outcome is unknown: {message}"));
                }
                TerminalUpdate::Failed(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some(message.clone()));
                    }
                    self.show_error(message);
                }
                TerminalUpdate::Unknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal #{} outcome is unknown: {message}", id.0));
                }
                TerminalUpdate::Closed(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal source retired".into()));
                    }
                    if self.window_session.focused_terminal() == Some(id.0) {
                        self.set_terminal_focus(false);
                    }
                }
                TerminalUpdate::Changed => {}
            }
        }
        if ended {
            self.start_git_status_refresh_with_feedback(false);
        }
        self.sync_terminal_panel();
        self.resize_active_terminal_to_panel();
    }
}

impl App {

    pub(in crate::app) fn set_terminal_focus(&mut self, focused: bool) {
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
}

impl App {


    pub(in crate::app) fn displayed_terminal(&self) -> Option<PtyId> {
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
            .or(self.terminal_session.displayed())
    }
}

impl App {

    /// Where the terminal's window is, if it is on screen.
    pub(in crate::app) fn terminal_rect(&self, id: PtyId) -> Option<WindowRect> {
        let window = self.window_session.native_ids().terminal(id.0)?.window;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
}

impl App {

    pub(in crate::app) fn terminal_window_rect(&self) -> Option<WindowRect> {
        let layout = self.window_session.layout()?;
        let (_, window) = layout.surfaces.terminal?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
}

impl App {


    #[cfg(test)]
    pub(in crate::app) fn terminal_body_dimensions(&self) -> Option<(u16, u16)> {
        self.compositor_projection()?;
        let rect = self.window_session.content().terminal?;
        Some((rect.width.max(1), rect.height.max(1)))
    }
}

impl App {


    /// Sizes each terminal on screen to its window.
    pub(in crate::app) fn resize_active_terminal_to_panel(&mut self) {
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
}

impl App {


    pub(in crate::app) fn place_active_terminal(&mut self, placement: WindowPlacement) {
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
}
