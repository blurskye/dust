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
                        .settings
                        .terminals
                        .default_profile
                        .clone()
                });
                self.defer_trusted_execution(DeferredExecution::OpenTerminal(profile));
            }
            AppEffect::TerminalToggle => {
                if self.terminal_focused {
                    self.place_active_terminal(WindowPlacement::Hidden);
                } else if let Some(id) = self.active_terminal {
                    if self.terminal_window_rect().is_none()
                        && let Err(error) =
                            self.show_native_window(NativeSurfaceTarget::Terminal(id.0))
                    {
                        self.show_error(error);
                        return Ok(());
                    }
                    self.terminal_focused = true;
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    let profile = self
                        .configuration
                        .settings
                        .terminals
                        .default_profile
                        .clone();
                    self.defer_trusted_execution(DeferredExecution::OpenTerminal(profile));
                }
            }
            AppEffect::TerminalList => {
                let status = self
                    .terminals
                    .iter()
                    .map(|(id, terminal)| {
                        format!(
                            "#{} {} {}",
                            id.0,
                            terminal.profile,
                            if terminal.running {
                                "running"
                            } else {
                                "exited"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.ui.notification = Some(self.message("terminal_list", &[("status", status)]));
                self.ui.notification_error = false;
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminals.contains_key(&id) {
                    self.active_terminal = Some(id);
                    self.terminal_focused = true;
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalFocusActive => {
                if self.active_terminal.is_some() {
                    self.terminal_focused = true;
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalCycle(direction) => {
                let mut ids = self.terminals.keys().copied().collect::<Vec<_>>();
                ids.sort_unstable_by_key(|id| id.0);
                if ids.is_empty() {
                    self.show_error(self.message("terminal_missing", &[]));
                } else {
                    let current = self
                        .active_terminal
                        .and_then(|active| ids.iter().position(|id| *id == active))
                        .unwrap_or(0);
                    let next = if direction < 0 {
                        current.checked_sub(1).unwrap_or(ids.len() - 1)
                    } else {
                        (current + 1) % ids.len()
                    };
                    self.active_terminal = Some(ids[next]);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                }
            }
            AppEffect::TerminalResize(rows) => {
                if self.active_terminal.is_none() {
                    self.show_error(self.message("terminal_missing", &[]));
                } else {
                    let step = i32::from(rows).saturating_mul(i32::from(
                        self.configuration.settings.ui.terminal_panel.resize_step,
                    ));
                    let policy = &self.configuration.settings.ui.terminal_panel;
                    self.terminal_panel_height = u16::try_from(
                        i32::from(self.terminal_panel_height)
                            .saturating_add(step)
                            .clamp(
                                i32::from(policy.minimum_height),
                                i32::from(policy.maximum_height),
                            ),
                    )
                    .unwrap_or(policy.height);
                    if let Some(id) = self.active_terminal {
                        let target = NativeSurfaceTarget::Terminal(id.0);
                        let placement = self.current_native_placement(target).ok();
                        let updated = match placement {
                            Some(WindowPlacement::Docked { edge, .. }) => {
                                Some(WindowPlacement::Docked {
                                    edge,
                                    size: self.terminal_panel_height,
                                })
                            }
                            Some(WindowPlacement::Floating(mut rect)) => {
                                rect.height = self.terminal_panel_height;
                                Some(WindowPlacement::Floating(rect))
                            }
                            _ => None,
                        };
                        if let Some(placement) = updated
                            && let Err(error) =
                                self.apply_native_window_placement(target, placement)
                        {
                            self.show_error(error);
                        }
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
                self.terminal_focused = false;
                self.sync_terminal_panel();
                self.ensure_cursor_visible();
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.active_terminal {
                    if let Err(error) = self.ptys.input(
                        id,
                        text.into_bytes(),
                        self.configuration.settings.terminals.maximum_input_bytes,
                    ) {
                        self.show_error(error)
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
                if let Some(id) = self.active_terminal {
                    self.retire_terminal(id, true);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
