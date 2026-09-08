use super::*;
impl InteractiveApplication {
    pub(super) fn start_dap_configuration(&mut self, name: &str) {
        match self.debugger_session.request_debug(
            Some(name),
            None,
            None,
            None,
            &self.configuration.settings().debug,
            &self.configuration.settings().terminals,
            self.configuration.binding_revision(),
            &self.workspace,
            &self.tools,
        ) {
            Ok(intent) => self.defer_trusted_execution(DeferredExecution::StartDebug(intent)),
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn start_dap(&mut self, requested: Option<String>, program: Option<PathBuf>) {
        let document = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers.get(id))
            .and_then(|buffer| buffer.path.as_deref())
            .and_then(|path| language_id_for_path(path).map(|language| (path, language)));
        let Some(document) = document else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        match self.debugger_session.request_debug(
            None,
            requested.as_deref(),
            program,
            Some(document),
            &self.configuration.settings().debug,
            &self.configuration.settings().terminals,
            self.configuration.binding_revision(),
            &self.workspace,
            &self.tools,
        ) {
            Ok(intent) => self.defer_trusted_execution(DeferredExecution::StartDebug(intent)),
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn authorize_debug_launch(
        &mut self,
        intent: crate::editor::session::debugger::DebugIntent,
        authorization: Option<ExecutionAuthorization>,
    ) {
        let Some(permit) = authorization.and_then(|a| a.debug_permit(&self.workspace, &intent))
        else {
            self.show_error("debug launch requires its original authorization");
            return;
        };
        if let Err(error) = self
            .debugger_session
            .authorize_debug(intent, permit, &self.workspace)
        {
            self.show_error(error);
            return;
        }
        self.advance_debug_launch();
    }
    pub(super) fn advance_debug_launch(&mut self) {
        match self.debugger_session.advance_debug(
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Debugger),
            &self.configuration.settings().debug,
            self.configuration.binding_revision(),
            &self.tools,
        ) {
            Ok(Some(name)) => {
                self.notify_message(self.message("dap_started", &[("adapter", name)]), false)
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn process_dap_project_root(&mut self) {
        if let Some(result) = self
            .workspace
            .project_result(crate::editor::session::workspace::DiscoveryKind::Debugger)
        {
            if let Err(error) = self.debugger_session.debug_root_result(result) {
                let _ = self.debugger_session.stop();
                self.show_error(error);
            }
        }
        self.advance_debug_launch();
    }
    pub(super) fn process_dap_events(&mut self) {
        for error in self.debugger_session.reply_failures() {
            self.show_error(error);
        }
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        for _ in 0..256 {
            let Some(event) = self.debugger_session.try_event() else {
                break;
            };
            match event {
                DapEvent::Starting { .. } => {}
                DapEvent::RequestFailed { message, .. } => self.show_error(message),
                DapEvent::Running { adapter } => {
                    self.notify_message(
                        self.message("dap_running", &[("adapter", adapter)]),
                        false,
                    );
                }
                DapEvent::Stopped {
                    adapter, reason, ..
                } => {
                    self.notify_message(
                        self.message("dap_stopped", &[("adapter", adapter), ("reason", reason)]),
                        false,
                    );
                }
                DapEvent::Output { adapter, output } => {
                    self.notify_message(
                        self.message(
                            "dap_output",
                            &[("adapter", adapter), ("output", output.replace('\n', " "))],
                        ),
                        false,
                    );
                }
                DapEvent::Response { response, .. } => match self
                    .debugger_session
                    .response(response)
                {
                    crate::editor::session::debugger::DebugResponse::Retired => {}
                    crate::editor::session::debugger::DebugResponse::Picker { kind, rows } => {
                        self.install_debug_rows(kind, rows)
                    }
                    crate::editor::session::debugger::DebugResponse::Result(response) => {
                        match response {
                            DapResponse::Evaluate(value) => {
                                self.notify_message(
                                    self.message(
                                        "dap_evaluate_result",
                                        &[("name", value.name), ("value", value.value)],
                                    ),
                                    false,
                                );
                            }
                            DapResponse::Breakpoints(rows) => {
                                self.notify_message(
                                    self.message(
                                        "dap_breakpoints_synced",
                                        &[("count", rows.len().to_string())],
                                    ),
                                    false,
                                );
                            }
                            DapResponse::Threads(_)
                            | DapResponse::Stack(_)
                            | DapResponse::Scopes(_)
                            | DapResponse::Variables(_) => {
                                unreachable!("owner converts response rows")
                            }
                            DapResponse::Source {
                                name,
                                content,
                                mime_type,
                            } => {
                                let Some(target) = self.debugger_session.take_source() else {
                                    continue;
                                };
                                if !self.window_session.navigation_current(
                                    &target.navigation,
                                    &self.state.window_version(),
                                    self.state.active_view,
                                ) || !self.input.capture_current(&target.capture)
                                {
                                    continue;
                                }
                                let mut draft = self.state.draft();
                                draft.open_virtual_read_only(
                                    TextBuffer::from_text(&content),
                                    "dap".into(),
                                    0,
                                    0,
                                    0,
                                );
                                if let Err(error) = self.commit_editor_draft(draft, None, false) {
                                    self.show_error(error);
                                    continue;
                                }
                                self.notify_message(
                                    self.message(
                                        "dap_source_loaded",
                                        &[("name", name), ("mime", mime_type.unwrap_or_default())],
                                    ),
                                    false,
                                );
                            }
                        }
                    }
                },
                DapEvent::Terminated { adapter } => {
                    let _ = self.debugger_session.stop();
                    self.notify_message(
                        self.message(
                            "dap_stopped",
                            &[("adapter", adapter), ("reason", "terminated".into())],
                        ),
                        false,
                    );
                }
                DapEvent::RunInTerminal {
                    reply,
                    cwd,
                    arguments,
                    environment,
                    ..
                } => {
                    let reply = reply.claim();
                    let profile = match self.debugger_session.terminal_request(
                        &cwd,
                        &arguments,
                        &environment,
                        &self.configuration.settings().terminals,
                    ) {
                        Ok(profile) => profile.to_owned(),
                        Err(error) => {
                            let _ = reply.respond(false, Some(error));
                            continue;
                        }
                    };
                    if !reply.current() {
                        continue;
                    }
                    self.request_terminal_open(&profile, Some(reply));
                }
                DapEvent::Failed { adapter, message } => {
                    let _ = self.debugger_session.stop();
                    self.show_error(self.message(
                        "dap_failed",
                        &[("adapter", adapter), ("diagnostic", message)],
                    ));
                }
            }
        }
    }
}
