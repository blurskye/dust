use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn active_lsp_document(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.clone()?;
        let language_id = language_id_for_path(&path)?.to_owned();
        Some((
            buffer_id,
            LspDocument {
                path,
                language_id,
                version: buffer.text.revision().0,
                text: buffer.text.to_string(),
            },
        ))
    }

    pub(in crate::editor::session) fn start_lsp(&mut self, requested: Option<String>) {
        self.start_lsp_authorized(requested, None);
    }
    pub(in crate::editor::session) fn start_lsp_authorized(
        &mut self,
        requested: Option<String>,
        authorization: Option<ExecutionAuthorization>,
    ) {
        if !self.configuration.settings().lsp.enabled {
            self.show_error(self.message("lsp_disabled", &[]));
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let entry = if let Some(id) = requested.as_ref() {
            let entry =
                self.tooling.entries().iter().find(|entry| {
                    entry.definition.id == *id && entry.definition.kind == ToolKind::Lsp
                });
            if !entry.is_some_and(|entry| entry.definition.supports_language(&document.language_id))
            {
                self.show_error(format!(
                    "language server `{id}` does not support {}",
                    document.language_id
                ));
                return;
            }
            entry
        } else {
            self.tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
                .or_else(|| {
                    self.tooling.entries().iter().find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry
                                .definition
                                .automatically_supports_language(&document.language_id)
                    })
                })
        };
        let Some(entry) = entry else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        let base_definition = entry.definition.clone();
        let source = self.selected_lsp_runtime_source(&base_definition);
        let definition = if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition.clone()
        };
        let launch_source = match source {
            LspRuntimeSource::Path => {
                let Some(executable) = discover_executable(&definition.executable) else {
                    self.show_error(format!(
                        "{} is not available on PATH; press p to choose another runtime",
                        definition.name
                    ));
                    return;
                };
                if definition.id == "rust-analyzer"
                    && Self::rust_analyzer_is_rustup_proxy(&executable)
                {
                    let key = Self::tool_version_key(
                        &definition,
                        LspRuntimeSource::Path,
                        Some(&executable),
                    );
                    match self.tooling.version(&key) {
                        Some(Ok(_)) => {}
                        Some(Err(_)) => {
                            self.show_error(Self::unavailable_rust_analyzer_message());
                            return;
                        }
                        None => {
                            self.queue_tool_version_probe(
                                &definition,
                                LspRuntimeSource::Path,
                                executable,
                            );
                            self.notify_message(
                                "checking rust-analyzer in the active rustup toolchain…".into(),
                                false,
                            );
                            return;
                        }
                    }
                }
                LspLaunchSource::Direct {
                    executable,
                    managed: false,
                }
            }
            LspRuntimeSource::Managed => {
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust-download version {} is not installed; press i to install it",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                LspLaunchSource::Direct {
                    executable,
                    managed: true,
                }
            }
            LspRuntimeSource::Nix => {
                let Some(shell) = nix_workspace_shell_for_host(
                    &definition,
                    self.configuration.settings().lsp.nix_workspace_fallback,
                    is_nixos_host(),
                ) else {
                    self.show_error("Dust via Nix is unavailable on this host");
                    return;
                };
                if !nix_lsp_profile_installed(&definition) {
                    self.show_error("Dust via Nix is not installed; press i to install it");
                    return;
                }
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust via Nix version {} is not installed; press i",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                if let Err(error) = nix_lsp_launch_environment(&definition) {
                    self.show_error(error);
                    return;
                }
                LspLaunchSource::NixWorkspace {
                    nix: executable,
                    shell: shell.into(),
                }
            }
        };
        self.language.clear_error(&definition.id);
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        if !authorization.is_some_and(|permit| {
            permit.consume(
                &self.workspace,
                ExecutionAdmission::Lsp(definition.id.clone()),
            )
        }) {
            self.defer_trusted_execution(DeferredExecution::StartLsp(Some(definition.id.clone())));
            return;
        }
        if let Err(error) = self.workspace.authorize_execution(
            if matches!(launch_source, LspLaunchSource::NixWorkspace { .. }) {
                ExecutionSource::ProjectLocal
            } else {
                ExecutionSource::SystemTool
            },
        ) {
            self.show_error(error);
            return;
        }
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.discover_launch(
            PendingLspProjectRoot {
                definition,
                launch_source,
                root_id,
                workspace_root: root_path,
                buffer,
                document,
            },
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_lsp_project_root(&mut self) {
        let Some(result) = self
            .workspace
            .project_result(crate::editor::session::workspace::DiscoveryKind::Language)
        else {
            return;
        };
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.accept_project_root(
            result,
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_nix_lsp_workspace(&mut self) {
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.poll_nix_launch(
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn queue_lsp_preflight(&mut self, pending: LspLaunchRequest) {
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.queue_launch(
            pending,
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }

    pub(in crate::editor::session) fn restart_lsp_session(&mut self, key: String) {
        self.restart_lsp_authorized(key, None);
    }
    pub(in crate::editor::session) fn restart_lsp_authorized(
        &mut self,
        key: String,
        authorization: Option<ExecutionAuthorization>,
    ) {
        let Some(state) = self.language.sessions().get(&key) else {
            return;
        };
        if state.circuit_open || !state.desired_running {
            return;
        }
        if !authorization.is_some_and(|permit| {
            permit.consume(&self.workspace, ExecutionAdmission::Restart(key.clone()))
        }) {
            self.defer_trusted_execution(DeferredExecution::RestartLsp(key));
            return;
        }
        let Some((&buffer, document)) = state.documents.iter().next() else {
            return;
        };
        let (definition, launch, root, root_path, document) = (
            state.definition.clone(),
            state.launch.clone(),
            state.root,
            state.root_path.clone(),
            document.clone(),
        );
        self.queue_lsp_preflight(LspLaunchRequest {
            session_key: key,
            definition,
            launch,
            root_id: root,
            project_root: root_path,
            buffer,
            document,
            restart: true,
        });
    }

    pub(in crate::editor::session) fn select_lsp_session(&mut self, key: &str) {
        if self.language.select(key) {
            self.ui.lsp_inlay_hints.clear();
            self.schedule_lsp_inlay_refresh(true);
        }
    }

    pub(in crate::editor::session) fn show_lsp_sessions(&mut self) {
        let active = self.language.active().map(String::as_str);
        let items = self
            .language
            .sessions()
            .iter()
            .map(|(key, session)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("lsp-session:{key}")),
                        label: session.server_id.clone(),
                        detail: format!(
                            "root #{}{} · {} · restarts {}",
                            session.root.0,
                            if active == Some(key) {
                                " · active"
                            } else {
                                ""
                            },
                            if session.circuit_open {
                                "circuit-open"
                            } else if session.restart_due.is_some() {
                                "backoff"
                            } else if session.desired_running {
                                "running"
                            } else {
                                "stopped"
                            },
                            session.restart_count
                        ),
                        source: key.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspSession(key.clone()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_lsp_sessions_title", &[]), items);
    }

    pub(in crate::editor::session) fn clear_lsp_server_ownership(&mut self, server: &str) {
        self.language.forget_synced(server);
        self.language.retire_projection_server(server);
    }

    pub(in crate::editor::session) fn detach_lsp_documents(
        &mut self,
        buffer: crate::editor::BufferId,
        documents: Vec<(String, String)>,
    ) {
        let detached = !documents.is_empty();
        for (server, uri) in documents {
            let _ = self.language.close(&server, uri.clone());
            self.language.invalidate_document(&server, buffer);
            self.language.retire_projection_document(&server, &uri);
            for prefix in ["lsp.", "lsp-pull:"] {
                if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                    self.diagnostics.clear_owner_uri(&owner, &uri);
                }
            }
            self.language.detach(&server, buffer);
        }
        if detached {
            self.sync_ui_diagnostics();
            if self.state.active_buffer_id() == Some(buffer) {
                self.clear_lsp_ui_projection();
            }
        }
    }
}
