use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_debugger_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        match effect {
            AppEffect::DapStart(program) => self.start_dap(None, program),
            AppEffect::DapConfigurationStart(name) => self.start_dap_configuration(&name),
            AppEffect::DapWatchAdd(expression) => self.debugger_session.add_watch(expression),
            AppEffect::DapWatchRemove(expression) => {
                self.debugger_session.remove_watch(&expression)
            }
            AppEffect::DapWatchList => self.open_dap_picker(
                "watches",
                self.debugger_session
                    .watches()
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
                if let Err(error) = self.debugger_session.stop() {
                    self.show_error(error);
                }
            }
            AppEffect::DapContinue => {
                if let Err(error) = self.debugger_session.control(DapControl::Continue) {
                    self.show_error(error);
                }
            }
            AppEffect::DapPause => {
                if let Err(error) = self.debugger_session.control(DapControl::Pause) {
                    self.show_error(error);
                }
            }
            AppEffect::DapNext => {
                if let Err(error) = self.debugger_session.control(DapControl::Next) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepIn => {
                if let Err(error) = self.debugger_session.control(DapControl::StepIn) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepOut => {
                if let Err(error) = self.debugger_session.control(DapControl::StepOut) {
                    self.show_error(error);
                }
            }
            AppEffect::DapConsole => {
                let items = self
                    .debugger_session
                    .console()
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
                let adapter = self
                    .debugger_session
                    .active_adapter()
                    .unwrap_or("none")
                    .to_owned();
                let configuration = self
                    .debugger_session
                    .configuration()
                    .map(str::to_owned)
                    .unwrap_or_default();
                let items = vec![(
                    PickerItem {
                        id: PickerItemId("dap-session:active".into()),
                        label: adapter,
                        detail: configuration,
                        source: if self.debugger_session.is_running() {
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
                if let Err(error) = self.debugger_session.control(DapControl::Threads) {
                    self.show_error(error)
                }
            }
            AppEffect::DapStack(thread) => {
                if let Err(error) = self.debugger_session.control(DapControl::StackTrace {
                    thread_id: thread,
                    start: 0,
                    levels: 256,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapScopes(frame) => {
                if let Err(error) = self
                    .debugger_session
                    .control(DapControl::Scopes { frame_id: frame })
                {
                    self.show_error(error)
                }
            }
            AppEffect::DapVariables(reference) => {
                if let Err(error) = self.debugger_session.control(DapControl::Variables {
                    reference,
                    start: 0,
                    count: 1000,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapEvaluate(expression) => {
                if let Err(error) = self.debugger_session.control(DapControl::Evaluate {
                    expression,
                    frame_id: None,
                    context: "repl".into(),
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapRestart => {
                if let Some(name) = self.debugger_session.configuration().map(str::to_owned) {
                    let _ = self.debugger_session.stop();
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
                if let Err(error) = self.debugger_session.breakpoint(
                    DapSourceBreakpoint {
                        path,
                        line,
                        column: None,
                        condition,
                        hit_condition,
                        log_message,
                    },
                    false,
                ) {
                    self.show_error(error);
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
                match self.debugger_session.breakpoint(
                    DapSourceBreakpoint {
                        path: path.clone(),
                        line,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    },
                    true,
                ) {
                    Ok(removed) => self.notify_message(
                        self.message(
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
                        ),
                        false,
                    ),
                    Err(error) => self.show_error(error),
                }
            }

            AppEffect::DapStatus => {
                self.notify_message(
                    self.message(
                        "dap_status",
                        &[(
                            "adapter",
                            self.debugger_session
                                .active_adapter()
                                .unwrap_or("none")
                                .to_owned(),
                        )],
                    ),
                    false,
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
