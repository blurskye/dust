use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_lsp_status_ui(&mut self) {
        let running = self.lsp.running_servers();
        self.ui.lsp_server = self
            .active_lsp_server
            .as_ref()
            .and_then(|server| {
                self.lsp_sessions
                    .get(server)
                    .map(|session| (server, session))
            })
            .map(|(server, session)| {
                if session.capabilities.is_some() {
                    session.server_id.clone()
                } else if session.circuit_open {
                    format!("failed {}", session.server_id)
                } else if session.restart_due.is_some() {
                    format!("backoff {}", session.server_id)
                } else if running.contains(server) {
                    format!("starting {}", session.server_id)
                } else {
                    format!("stopped {}", session.server_id)
                }
            });
        let refresh = self.ui.tool_menu.as_ref().is_some_and(|menu| {
            menu.rows.iter().any(|row| {
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.id == row.id && entry.definition.kind == ToolKind::Lsp
                    })
                    .is_some_and(|entry| {
                        !matches!(entry.status, ToolStatus::Installing { .. })
                            && self.lsp_menu_lifecycle(&row.id).0 != row.lifecycle
                    })
            })
        });
        if refresh {
            let category = self
                .ui
                .tool_menu
                .as_ref()
                .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
            let selected = self.selected_tool_id();
            self.rebuild_tool_menu(&category, selected.as_deref());
        }
    }

    pub(in crate::editor::session) fn reconcile_lsp_autostart(
        &mut self,
        buffer: crate::editor::BufferId,
        document: &LspDocument,
        matching_session: bool,
    ) {
        let launchable_server = self
            .tooling
            .entries()
            .iter()
            .find(|entry| {
                entry.definition.kind == ToolKind::Lsp
                    && entry
                        .definition
                        .automatically_supports_language(&document.language_id)
                    && self.selected_lsp_runtime_available(&entry.definition)
            })
            .map(|entry| entry.definition.id.clone());
        let observation = LspAutostartObservation {
            buffer,
            path: document.path.clone(),
            language: document.language_id.clone(),
            root_revision: self.workspace.root_revision(),
            trust: self.workspace.trust(),
            enabled: self.configuration.settings().lsp.enabled,
            auto_start: self.configuration.settings().lsp.auto_start,
            nix_workspace_fallback: self.configuration.settings().lsp.nix_workspace_fallback,
            execution_busy: self.execution_validation.is_some(),
            launchable_server,
        };
        if self.lsp_autostart_observation.as_ref() == Some(&observation) {
            return;
        }
        let server = observation.launchable_server.clone();
        let should_start = !matching_session
            && observation.enabled
            && observation.auto_start
            && !observation.execution_busy
            && observation.trust == WorkspaceTrust::Trusted
            && !self.workspace.safe_mode()
            && server.is_some();
        self.lsp_autostart_observation = Some(observation);
        if should_start {
            self.start_lsp(server);
        }
    }

    pub(in crate::editor::session) fn sync_lsp_document(&mut self) {
        let Some(buffer) = self.state.active_buffer_id() else {
            self.lsp_autostart_observation = None;
            return;
        };
        let Some((_, document)) = self.active_lsp_document() else {
            let stale = self
                .lsp_sessions
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .documents
                        .get(&buffer)
                        .map(|document| (key.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer, stale);
            self.clear_lsp_ui_projection();
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            self.lsp_autostart_observation = None;
            return;
        };
        let revision = self.state.buffers[buffer].text.source_version();
        let mut sessions = Vec::new();
        let mut detach = Vec::new();
        for (key, state) in &self.lsp_sessions {
            // A lexical `starts_with` check would let a session for an outer
            // project absorb a nested, independently resolved project. Only
            // the asynchronous resolver may attach a new document to a server.
            let owns = state.documents.contains_key(&buffer);
            let supports = state.definition.supports_language(&document.language_id);
            if owns && supports {
                sessions.push(key.clone());
                if let Some(previous) = state.documents.get(&buffer)
                    && (previous.path != document.path
                        || previous.language_id != document.language_id)
                {
                    detach.push((key.clone(), previous.uri()));
                }
            } else if let Some(previous) = state.documents.get(&buffer) {
                detach.push((key.clone(), previous.uri()));
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            self.clear_lsp_ui_projection();
        }
        if !sessions.is_empty()
            && !sessions
                .iter()
                .any(|key| self.active_lsp_server.as_ref() == Some(key))
        {
            self.select_lsp_session(&sessions[0]);
        }
        self.detach_lsp_documents(buffer, detach);
        self.refresh_lsp_projection_owner();
        let mut changed = false;
        for server in &sessions {
            if self.lsp_session_synced.get(&(server.clone(), buffer)) == Some(&revision) {
                continue;
            }
            if self
                .lsp
                .sync_owned(server, document.clone(), (buffer, revision.clone()))
                .is_ok()
            {
                changed = true;
                self.lsp_session_synced
                    .insert((server.clone(), buffer), revision.clone());
                if let Some(state) = self.lsp_sessions.get_mut(server) {
                    state.documents.insert(buffer, document.clone());
                }
            }
        }
        if changed
            && !(self.configuration.settings().lsp.inlay_hint_level
                == crate::config::InlayHintLevel::Max
                && self.ui.lsp_inlay_hints.is_empty()
                && self.lsp_inlay_refresh_due.is_some())
        {
            self.schedule_lsp_inlay_refresh(false);
        }
        self.reconcile_lsp_autostart(buffer, &document, !sessions.is_empty());
        if self.ui.language_completion.is_some()
            && self
                .active_lsp_server
                .as_ref()
                .is_some_and(|active| sessions.contains(active))
        {
            self.request_lsp(LspFeature::Completion);
        }
    }
}
