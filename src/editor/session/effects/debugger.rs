use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_debugger_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::DapStart(program) => self.start_dap(None, program),
            AppEffect::DapConfigurationStart(name) => self.start_dap_configuration(&name),
            AppEffect::DapWatchAdd(expression) => {
                if !expression.is_empty()
                    && expression.len() <= 64 * 1024
                    && self.dap_watches.len() < self.configuration.settings.debug.maximum_watches
                    && !self.dap_watches.contains(&expression)
                {
                    self.dap_watches.push(expression);
                }
            }
            AppEffect::DapWatchRemove(expression) => {
                self.dap_watches.retain(|watch| watch != &expression)
            }
            AppEffect::DapWatchList => self.open_dap_picker(
                "watches",
                self.dap_watches
                    .iter()
                    .cloned()
                    .map(|expression| {
                        (
                            expression.clone(),
                            String::new(),
                            DapControl::Evaluate {
                                expression,
                                frame_id: None,
                                context: "watch".into(),
                            },
                        )
                    })
                    .collect(),
            ),
            AppEffect::DapStop => {
                if let Err(error) = self.dap.stop() {
                    self.show_error(error);
                }
            }
            AppEffect::DapContinue => {
                if let Err(error) = self.dap.control(DapControl::Continue) {
                    self.show_error(error);
                }
            }
            AppEffect::DapPause => {
                if let Err(error) = self.dap.control(DapControl::Pause) {
                    self.show_error(error);
                }
            }
            AppEffect::DapNext => {
                if let Err(error) = self.dap.control(DapControl::Next) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepIn => {
                if let Err(error) = self.dap.control(DapControl::StepIn) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepOut => {
                if let Err(error) = self.dap.control(DapControl::StepOut) {
                    self.show_error(error);
                }
            }
            AppEffect::DapConsole => {
                let items = self
                    .dap_console
                    .iter()
                    .enumerate()
                    .map(|(index, (adapter, line))| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("dap-console:{index}")),
                                label: line.clone(),
                                detail: String::new(),
                                source: adapter.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("dap_console_title", &[]), items);
            }
            AppEffect::DapSessions => {
                let adapter = self.dap.active_adapter().unwrap_or("none").to_owned();
                let configuration = self.active_debug_configuration.clone().unwrap_or_default();
                let items = vec![(
                    PickerItem {
                        id: PickerItemId("dap-session:active".into()),
                        label: adapter,
                        detail: configuration,
                        source: if self.dap.is_running() {
                            self.message("dap_session_running", &[])
                        } else {
                            self.message("dap_session_stopped", &[])
                        },
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )];
                self.install_picker(self.message("dap_sessions_title", &[]), items);
            }
            AppEffect::DapThreads => {
                if let Err(error) = self.dap.control(DapControl::Threads) {
                    self.show_error(error)
                }
            }
            AppEffect::DapStack(thread) => {
                if let Err(error) = self.dap.control(DapControl::StackTrace {
                    thread_id: thread,
                    start: 0,
                    levels: 256,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapScopes(frame) => {
                if let Err(error) = self.dap.control(DapControl::Scopes { frame_id: frame }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapVariables(reference) => {
                if let Err(error) = self.dap.control(DapControl::Variables {
                    reference,
                    start: 0,
                    count: 1000,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapEvaluate(expression) => {
                if let Err(error) = self.dap.control(DapControl::Evaluate {
                    expression,
                    frame_id: None,
                    context: "repl".into(),
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapRestart => {
                if let Some(name) = self.active_debug_configuration.clone() {
                    let _ = self.dap.stop();
                    self.start_dap_configuration(&name)
                } else {
                    self.show_error(self.message("dap_no_configuration", &[]));
                }
            }
            AppEffect::DapBreakpointConfigure {
                condition,
                hit_condition,
                log_message,
            } => {
                let Some(view) = self.state.active_view else {
                    return Ok(());
                };
                let buffer = &self.state.buffers[self.state.views[view].buffer];
                let Some(path) = buffer.path.clone() else {
                    return Ok(());
                };
                let Ok(line) = buffer
                    .text
                    .byte_to_line(self.state.views[view].cursor)
                    .map(|line| line as u64 + 1)
                else {
                    return Ok(());
                };
                for value in [
                    condition.as_ref(),
                    hit_condition.as_ref(),
                    log_message.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if value.len() > 64 * 1024 || value.chars().any(|character| character == '\0') {
                        self.show_error(self.message("dap_breakpoint_invalid", &[]));
                        return Ok(());
                    }
                }
                let rows = self.dap_breakpoints.entry(path.clone()).or_default();
                if let Some(row) = rows.iter_mut().find(|row| row.line == line) {
                    row.condition = condition;
                    row.hit_condition = hit_condition;
                    row.log_message = log_message;
                } else if rows.len() < self.configuration.settings.debug.maximum_breakpoints {
                    rows.push(DapSourceBreakpoint {
                        path: path.clone(),
                        line,
                        column: None,
                        condition,
                        hit_condition,
                        log_message,
                    });
                }
                if self.dap.is_running() {
                    let _ = self.dap.control(DapControl::SetBreakpoints {
                        path,
                        breakpoints: rows.clone(),
                    });
                }
            }
            AppEffect::DapBreakpointToggle => {
                let Some(view) = self.state.active_view else {
                    return Ok(());
                };
                let buffer = &self.state.buffers[self.state.views[view].buffer];
                let Some(path) = buffer.path.clone() else {
                    return Ok(());
                };
                let Ok(line) = buffer
                    .text
                    .byte_to_line(self.state.views[view].cursor)
                    .map(|line| line as u64 + 1)
                else {
                    return Ok(());
                };
                let rows = self.dap_breakpoints.entry(path.clone()).or_default();
                let removed = rows.iter().any(|row| row.line == line);
                if removed {
                    rows.retain(|row| row.line != line)
                } else {
                    rows.push(DapSourceBreakpoint {
                        path: path.clone(),
                        line,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    })
                }
                let breakpoints = rows.clone();
                if self.dap.is_running() {
                    let _ = self.dap.control(DapControl::SetBreakpoints {
                        path: path.clone(),
                        breakpoints,
                    });
                }
                self.ui.notification = Some(self.message(
                    "dap_breakpoint_status",
                    &[
                        (
                            "state",
                            if removed {
                                "removed".into()
                            } else {
                                "added".into()
                            },
                        ),
                        ("path", path.display().to_string()),
                        ("line", line.to_string()),
                    ],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::DapStatus => {
                self.ui.notification = Some(self.message(
                    "dap_status",
                    &[(
                        "adapter",
                        self.dap.active_adapter().unwrap_or("none").to_owned(),
                    )],
                ));
                self.ui.notification_error = false;
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
