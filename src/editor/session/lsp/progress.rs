use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_server_label(&self, server_key: &str) -> String {
        self.lsp_sessions
            .get(server_key)
            .map(|session| session.server_id.clone())
            .or_else(|| {
                server_key
                    .strip_prefix("root:")
                    .and_then(|key| key.rsplit_once(':'))
                    .map(|(_, server)| server.to_owned())
            })
            .unwrap_or_else(|| server_key.to_owned())
    }

    pub(in crate::editor::session) fn update_lsp_progress(
        &mut self,
        server_key: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        if !self.configuration.settings.ui.lsp_progress.enabled {
            self.ui.lsp_progress.clear();
            self.ui.lsp_progress_frame = 0;
            return;
        }
        let server = self.lsp_server_label(&server_key);
        let had_active = self.ui.lsp_progress.iter().any(|item| !item.done);
        if let Some(item) = self
            .ui
            .lsp_progress
            .iter_mut()
            .find(|item| item.server_key == server_key && item.token == token)
        {
            if kind == LspProgressKind::End {
                if !title.is_empty() {
                    item.title = title;
                }
                if !message.is_empty() {
                    item.message = message;
                }
                item.percentage = None;
                item.done = true;
                item.completed_at = Some(Instant::now());
                return;
            }
            if item.done && kind != LspProgressKind::Begin {
                return;
            }
            item.server = server;
            if kind == LspProgressKind::Begin || !title.is_empty() {
                item.title = title;
            }
            if kind == LspProgressKind::Begin || !message.is_empty() {
                item.message = message;
            }
            if kind == LspProgressKind::Begin || percentage.is_some() {
                item.percentage = percentage;
            }
            if kind == LspProgressKind::Begin {
                item.done = false;
                item.completed_at = None;
                if !had_active {
                    self.lsp_progress_epoch = Instant::now();
                    self.ui.lsp_progress_frame = 0;
                }
            }
            return;
        }
        if kind == LspProgressKind::End {
            return;
        }

        let was_empty = self.ui.lsp_progress.is_empty();
        let maximum = usize::from(self.configuration.settings.ui.lsp_progress.max_rows);
        if self.ui.lsp_progress.len() == maximum {
            self.ui.lsp_progress.remove(0);
        }
        self.ui.lsp_progress.push(UiLspProgress {
            server_key,
            token,
            server,
            title,
            message,
            percentage,
            done: false,
            completed_at: None,
        });
        if was_empty {
            self.lsp_progress_epoch = Instant::now();
            self.ui.lsp_progress_frame = 0;
        }
    }

    pub(in crate::editor::session) fn clear_lsp_progress(&mut self, server_key: &str) {
        self.ui
            .lsp_progress
            .retain(|item| item.server_key != server_key);
        if self.ui.lsp_progress.is_empty() {
            self.ui.lsp_progress_frame = 0;
        }
    }

    pub(in crate::editor::session) fn update_lsp_progress_animation(&mut self) {
        if self.ui.lsp_progress.is_empty() {
            return;
        }
        if !self.configuration.settings.ui.lsp_progress.enabled {
            self.ui.lsp_progress.clear();
            self.ui.lsp_progress_frame = 0;
            return;
        }
        let now = Instant::now();
        let policy = &self.configuration.settings.ui.lsp_progress;
        let maximum = usize::from(policy.max_rows);
        if self.ui.lsp_progress.len() > maximum {
            let excess = self.ui.lsp_progress.len().saturating_sub(maximum);
            self.ui.lsp_progress.drain(..excess);
        }
        let completed_timeout = Duration::from_millis(policy.completed_timeout_ms);
        self.ui.lsp_progress.retain(|item| {
            item.completed_at.is_none_or(|completed| {
                now.saturating_duration_since(completed) < completed_timeout
            })
        });
        if self.ui.lsp_progress.is_empty() {
            self.ui.lsp_progress_frame = 0;
            return;
        }
        if !self.ui.lsp_progress.iter().any(|item| !item.done) {
            return;
        }
        let elapsed = now.saturating_duration_since(self.lsp_progress_epoch);
        let phase = elapsed.as_millis() / u128::from(policy.spinner_interval_ms);
        let frame = usize::try_from(phase % policy.spinner_frames.len() as u128).unwrap_or(0);
        if self.ui.lsp_progress_frame != frame {
            self.ui.lsp_progress_frame = frame;
        }
    }

    pub(in crate::editor::session) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        generation: u64,
        method: String,
        message: String,
    ) {
        if method == "textDocument/inlayHint" {
            let owns_current_request = self
                .lsp_inlay_request_owner
                .as_ref()
                .is_some_and(|(owned_generation, _)| *owned_generation == generation);
            if owns_current_request {
                self.lsp_inlay_request_owner = None;
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                if self.lsp_logs.len() == self.configuration.settings.lsp.maximum_log_lines {
                    self.lsp_logs.pop_front();
                }
                self.lsp_logs
                    .push_back(format!("{server}: inlay hint refresh failed: {message}"));
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
        while let Some(event) = self.lsp.try_event() {
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    if let Some(session) = self.lsp_sessions.get_mut(&server) {
                        self.lsp_last_errors.remove(&session.server_id);
                        session.capabilities = Some(capabilities.clone());
                    }
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.lsp_capabilities = Some(capabilities);
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
                    if let Some(session) = self.lsp_sessions.get_mut(&server) {
                        session.capabilities = Some(capabilities.clone());
                    }
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.lsp_capabilities = Some(capabilities);
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::InlayHintsRefresh { server } => {
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let server_label = self.lsp_server_label(&server);
                    let current = self.state.buffers.iter().find_map(|(_, buffer)| {
                        buffer.path.as_ref().and_then(|path| {
                            (crate::language::lsp::file_uri(path) == uri)
                                .then_some(buffer.text.revision())
                        })
                    });
                    let normalized_version = version.map(BufferRevision);
                    let encoding = self
                        .lsp_capabilities
                        .as_ref()
                        .map_or(PositionEncoding::Utf16, |capabilities| {
                            capabilities.position_encoding
                        });
                    let normalized = self
                        .state
                        .buffers
                        .iter()
                        .find_map(|(_, buffer)| {
                            let path = buffer.path.as_ref()?;
                            (crate::language::lsp::file_uri(path) == uri).then(|| {
                                let codec = PositionCodec::new(&buffer.text);
                                diagnostics
                                    .iter()
                                    .filter_map(|diagnostic| {
                                        let start = codec
                                            .lsp_to_byte(
                                                diagnostic.range.start.line,
                                                diagnostic.range.start.character,
                                                encoding,
                                            )
                                            .ok()?;
                                        let end = codec
                                            .lsp_to_byte(
                                                diagnostic.range.end.line,
                                                diagnostic.range.end.character,
                                                encoding,
                                            )
                                            .ok()?;
                                        Some(Diagnostic {
                                            uri: uri.clone(),
                                            version: normalized_version,
                                            range: ByteRange::new(start, end).ok()?,
                                            severity: match diagnostic.severity {
                                                Some(1) => DiagnosticSeverity::Error,
                                                Some(2) => DiagnosticSeverity::Warning,
                                                Some(3) => DiagnosticSeverity::Information,
                                                _ => DiagnosticSeverity::Hint,
                                            },
                                            source: diagnostic
                                                .source
                                                .clone()
                                                .unwrap_or_else(|| server_label.clone()),
                                            code: diagnostic.code.clone(),
                                            message: diagnostic.message.clone(),
                                            related: Vec::new(),
                                            tags: Vec::new(),
                                            fixes: Vec::new(),
                                            timestamp: crate::clock::system_time(),
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .unwrap_or_default();
                    let owner = DiagnosticOwner::new(format!("lsp.{server}"));
                    match owner.and_then(|owner| {
                        self.diagnostics.replace(
                            owner,
                            uri,
                            normalized_version,
                            current,
                            normalized,
                        )
                    }) {
                        Ok(()) => {
                            self.sync_ui_diagnostics();
                        }
                        Err(error) => {
                            self.lsp_logs.push_back(error.to_string());
                        }
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
                    if method == "textDocument/inlayHint" {
                        if !self.lsp_inlay_request_owner.as_ref().is_some_and(
                            |(owned_generation, owner)| {
                                *owned_generation == generation
                                    && Some(owner) == self.lsp_projection_owner.as_ref()
                                    && owner.server == server
                                    && owner.uri == uri
                                    && owner.revision.0 == version
                            },
                        ) {
                            continue;
                        }
                        self.lsp_inlay_request_owner = None;
                        self.present_lsp_result(&server, generation, &method, &uri, result);
                        continue;
                    }
                    if generation != self.lsp_request_generation
                        || !self.lsp_request_owner.as_ref().is_some_and(
                            |(owned_generation, owner)| {
                                *owned_generation == generation
                                    && Some(owner) == self.lsp_projection_owner.as_ref()
                                    && owner.server == server
                                    && owner.uri == uri
                                    && owner.revision.0 == version
                            },
                        )
                    {
                        continue;
                    }
                    self.lsp_request_owner = None;
                    self.present_lsp_result(&server, generation, &method, &uri, result);
                }
                LspEvent::ApplyEditRequest {
                    server,
                    request_id,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        Some((server, request_id)),
                        self.lsp_position_encoding(),
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
                    if self.lsp_logs.len() == 200 {
                        self.lsp_logs.pop_front();
                    }
                    let server = self.lsp_server_label(&server);
                    self.lsp_logs.push_back(format!("{server}: {message}"));
                }
                LspEvent::Health { server, health } => {
                    let server = self.lsp_server_label(&server);
                    self.ui.notification = Some(self.message(
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
                    ));
                    self.ui.notification_error = false;
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
                LspEvent::Stopped { server, .. } => {
                    let desired = self.lsp_sessions.get_mut(&server).is_some_and(|state| {
                        state.capabilities = None;
                        state.desired_running
                    });
                    let _ = self.lsp.stop(&server);
                    self.lsp_session_synced.retain(|(key, _), _| key != &server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.ui.notification =
                        Some(self.message("lsp_stopped", &[("server", server_label)]));
                    self.ui.notification_error = false;
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    let desired = self.lsp_sessions.get_mut(&server).is_some_and(|state| {
                        state.capabilities = None;
                        state.desired_running
                    });
                    let _ = self.lsp.stop(&server);
                    self.lsp_session_synced.retain(|(key, _), _| key != &server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.lsp_last_errors
                        .insert(server_label.clone(), message.clone());
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server_label), ("diagnostic", message)],
                    ));
                    if self.active_lsp_server.as_deref() == Some(&server) {
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
        let limit = self.configuration.settings.lsp.restart_limit;
        let Some(state) = self.lsp_sessions.get_mut(&key) else {
            return;
        };
        if !state.desired_running || state.restart_due.is_some() {
            return;
        }
        if state.restart_count >= limit {
            state.restart_due = None;
            state.circuit_open = true;
            let server = state.server_id.clone();
            let _ = state;
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
            return;
        }
        state.restart_count = state.restart_count.saturating_add(1);
        let exponent = u32::from(state.restart_count.saturating_sub(1)).min(8);
        let delay = self
            .configuration
            .settings
            .lsp
            .restart_backoff_ms
            .saturating_mul(2_u64.saturating_pow(exponent))
            .min(60_000);
        state.restart_due = Some(Instant::now() + Duration::from_millis(delay));
    }

    pub(in crate::editor::session) fn restart_lsp_if_due(&mut self) {
        if self.execution_validation.is_some() {
            return;
        }
        let now = Instant::now();
        let key = self.lsp_sessions.iter().find_map(|(key, state)| {
            (state.desired_running
                && !state.circuit_open
                && state.restart_due.is_some_and(|due| due <= now))
            .then(|| key.clone())
        });
        let Some(key) = key else { return };
        if let Some(state) = self.lsp_sessions.get_mut(&key) {
            state.restart_due = None;
        }
        self.restart_lsp_session(key);
    }
}
