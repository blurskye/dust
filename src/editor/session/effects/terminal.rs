use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_terminal_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::TerminalOpen(profile) => {
                let profile = profile.unwrap_or_else(|| {
                    self.configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone()
                });
                self.request_terminal_open(&profile, None);
            }
            AppEffect::TerminalToggle => {
                if self.window_session.terminal_focused() {
                    self.place_active_terminal(WindowPlacement::Hidden);
                } else if let Some(id) = self.terminal_session.selected() {
                    if self.terminal_window_rect().is_none()
                        && let Err(error) =
                            self.show_native_window(NativeSurfaceTarget::Terminal(id.0))
                    {
                        self.show_error(error);
                        return Ok(());
                    }
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    let profile = self
                        .configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone();
                    self.request_terminal_open(&profile, None);
                }
            }
            AppEffect::TerminalList => {
                let items = self
                    .terminal_session
                    .choices()
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, label, detail))| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("terminal:{index}")),
                                label,
                                detail,
                                source: "terminal".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Terminal(key),
                        )
                    })
                    .collect();
                self.install_picker("Terminals".into(), items);
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminal_session.contains(id) {
                    self.terminal_session.select(id);
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalFocusActive => {
                if self.terminal_session.selected().is_some() {
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalCycle(direction) => {
                let focused = self.window_session.terminal_focused();
                if !self.terminal_session.cycle(i64::from(direction)) {
                    self.show_error(self.message("terminal_missing", &[]));
                } else {
                    if focused {
                        self.set_terminal_focus(true);
                    }
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                }
            }
            AppEffect::TerminalResize(rows) => {
                if self.terminal_session.selected().is_none() {
                    self.show_error(self.message("terminal_missing", &[]));
                } else {
                    let result = if self.compositor_projection().is_none() {
                        Err("universal compositor projection is missing or stale".into())
                    } else {
                        self.window_session.resize_terminal(
                            self.terminal_session.selected().unwrap().0,
                            rows,
                            &self.configuration.settings().ui.terminal_panel,
                        )
                    };
                    if let Err(error) = result {
                        self.show_error(error);
                    }
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                    self.sync_explorer_ui();
                    self.ensure_cursor_visible();
                }
            }
            AppEffect::TerminalDock { edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("terminal dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                self.place_active_terminal(WindowPlacement::Docked { edge, size });
            }
            AppEffect::TerminalFloat {
                x,
                y,
                width,
                height,
            } => self.place_active_terminal(WindowPlacement::Floating(WindowRect {
                x,
                y,
                width,
                height,
            })),
            AppEffect::TerminalFullscreen => {
                self.place_active_terminal(WindowPlacement::Fullscreen)
            }
            AppEffect::TerminalHide => self.place_active_terminal(WindowPlacement::Hidden),
            AppEffect::TerminalUnfocus => {
                self.set_terminal_focus(false);
                self.sync_terminal_panel();
                self.ensure_cursor_visible();
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.terminal_session.selected() {
                    if let Err(error) =
                        self.terminal_session
                            .send(id, TerminalInput::Text(&text), &self.workspace)
                    {
                        self.show_error(error);
                    }
                }
            }
            AppEffect::TerminalModeNormal => self.enter_terminal_normal_mode(),
            AppEffect::TerminalModeInput => self.enter_terminal_input_mode(),
            AppEffect::TerminalNormalMove(motion) => {
                self.move_terminal_normal_cursor(&motion);
            }
            AppEffect::TerminalVisualStart(linewise) => {
                self.start_terminal_visual(linewise);
            }
            AppEffect::TerminalYank { line } => self.yank_terminal_text(line),
            AppEffect::TerminalClose => {
                self.terminal_session.cancel_open();
                if let Some(id) = self.terminal_session.selected() {
                    self.retire_terminal(id, true);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
