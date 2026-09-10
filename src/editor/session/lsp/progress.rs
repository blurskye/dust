use super::*;

impl InteractiveApplication {
    fn reprobe_failed_path_runtime(&mut self, server: &str) {
        let Some(definition) = self.language.sessions().get(server).and_then(|state| {
            matches!(&state.launch, LspLaunch::Direct { managed: false, .. })
                .then(|| state.definition.clone())
        }) else {
            return;
        };
        let Some(executable) = discover_executable(&definition.executable) else {
            return;
        };
        let key = Self::tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
        self.tooling.forget_probe(&key);
        self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, executable);
    }

    pub(in crate::editor::session) fn append_lsp_log(&mut self, line: String) {
        self.language
            .append_log(line, self.configuration.settings().lsp.maximum_log_lines);
        if self.search.picker_surface() == PickerSurfaceKind::Logs && self.ui.picker.is_some() {
            self.show_lsp_logs();
        }
    }

    pub(in crate::editor::session) fn lsp_server_label(&self, server_key: &str) -> String {
        self.language.server_label(server_key)
    }

    pub(in crate::editor::session) fn update_lsp_progress(
        &mut self,
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        self.language.update_progress(
            (server, token, kind, title, message, percentage),
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        self.project_lsp_progress();
    }
    pub(in crate::editor::session) fn clear_lsp_progress(&mut self, server: &str) {
        self.language.clear_progress(server);
        self.project_lsp_progress();
    }
    fn project_lsp_progress(&mut self) {
        let (rows, frame) = self.language.progress();
        if self.ui.lsp_progress != rows {
            self.ui.lsp_progress = rows.to_vec();
        }
        self.ui.lsp_progress_frame = frame;
    }
    pub(in crate::editor::session) fn update_lsp_progress_animation(&mut self) {
        self.language.animate_progress(
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        self.project_lsp_progress();
    }
    pub(in crate::editor::session) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        generation: u64,
        method: String,
        message: String,
    ) {
        if method == "textDocument/inlayHint" {
            if self.language.fail_inlay(&server, generation) {
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: inlay hint refresh failed: {message}"));
            }
            // Inlay hints are background, revision-owned projections. A failure
            // for any superseded generation is stale by definition, while a
            // current failure is retried above. Neither is user-actionable.
            return;
        }
        let server = self.lsp_server_label(&server);
        self.show_error(self.message(
            "lsp_request_failed",
            &[
                ("server", server),
                ("method", method),
                ("diagnostic", message),
            ],
        ));
    }

    pub(in crate::editor::session) fn process_lsp_events(&mut self) {
        for error in self.language.flush_edit_replies() {
            self.show_error(error);
        }
        while let Some(owned) = self.language.poll_event() {
            let crate::language::lsp::OwnedLspEvent {
                event,
                context,
                reply,
            } = owned;
            if !self.language.owns_event_context(&context) {
                self.language
                    .reject_reply(reply, "requesting language-server session retired".into());
                continue;
            }
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_startup_refresh();
                    }
                    // Successful lifecycle transitions are reflected by the
                    // status line and bounded progress surface. They must not
                    // leave persistent bottom-left notifications.
                }
                LspEvent::CapabilitiesChanged {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }

                LspEvent::InlayHintsRefresh { server } => {
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let result = self
                        .language
                        .diagnostic_batch(
                            &context,
                            &self.state,
                            (&uri, version, diagnostics),
                            (
                                false,
                                self.configuration
                                    .settings()
                                    .workspace
                                    .maximum_diagnostics_per_owner_uri,
                                &self.lsp_server_label(&server),
                            ),
                        )
                        .and_then(|batch| self.diagnostics.publish(batch, &self.state));
                    match result {
                        Ok(()) => self.sync_ui_diagnostics(),
                        Err(error) => self.append_lsp_log(error),
                    }
                }

                LspEvent::Result {
                    server,
                    generation,
                    method,
                    uri,
                    version,
                    result,
                } => {
                    let current = self.active_lsp_document();
                    if !current.as_ref().is_some_and(|(_, document)| {
                        document.uri() == uri && document.version == version
                    }) {
                        continue;
                    }
                    self.refresh_lsp_projection_owner();
                    if !self
                        .language
                        .accept_result(&server, generation, &method, &uri, version)
                    {
                        continue;
                    }
                    self.present_lsp_result_owned(
                        &server,
                        generation,
                        &method,
                        &uri,
                        result,
                        Some(context),
                    );
                }
                LspEvent::ApplyEditRequest {
                    server: _,
                    request_id: _,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        reply,
                        context.encoding(),
                    );
                }
                LspEvent::Progress {
                    server,
                    token,
                    kind,
                    title,
                    message,
                    percentage,
                } => self.update_lsp_progress(server, token, kind, title, message, percentage),
                LspEvent::Log { server, message } => {
                    let server = self.lsp_server_label(&server);
                    self.append_lsp_log(format!("{server}: {message}"));
                }
                LspEvent::Health { server, health } => {
                    let server = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message(
                            "lsp_health",
                            &[
                                ("server", server),
                                ("documents", health.attached_documents.to_string()),
                                ("pending", health.pending_requests.to_string()),
                                (
                                    "encoding",
                                    health.capabilities.position_encoding.lsp_name().into(),
                                ),
                            ],
                        ),
                        false,
                    );
                }
                LspEvent::Unsupported { server, method } => {
                    let server = self.lsp_server_label(&server);
                    self.show_error(
                        self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                    );
                }
                LspEvent::RequestFailed {
                    server,
                    generation,
                    method,
                    message,
                } => self.handle_lsp_request_failure(server, generation, method, message),
                LspEvent::Stopped { server, success } => {
                    if !success {
                        self.reprobe_failed_path_runtime(&server);
                    }
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message("lsp_stopped", &[("server", server_label)]),
                        false,
                    );
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    self.reprobe_failed_path_runtime(&server);
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.language.failed(server_label.clone(), message.clone());
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server_label), ("diagnostic", message)],
                    ));
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
            }
        }
    }

    pub(in crate::editor::session) fn schedule_lsp_restart(&mut self, key: String) {
        if let Some(server) =
            self.language
                .schedule_restart(&key, &self.configuration.settings().lsp, Instant::now())
        {
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
        }
    }

    pub(in crate::editor::session) fn restart_lsp_if_due(&mut self) {
        if self.workspace.validation_pending() {
            return;
        }
        if let Some(key) = self.language.restart_due(Instant::now()) {
            self.restart_lsp_session(key);
        }
    }
}
