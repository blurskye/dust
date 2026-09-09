use super::*;

pub(in crate::editor::session) struct LspLaunchRequest {
    pub session_key: String,
    pub definition: ToolDefinition,
    pub launch: LspLaunch,
    pub root_id: crate::workspace::RootId,
    pub project_root: PathBuf,
    pub buffer: crate::editor::BufferId,
    pub document: LspDocument,
    pub restart: bool,
}

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
        match self.workspace.discover_project(
            crate::editor::session::workspace::DiscoveryKind::Language,
            document.path.clone(),
            root_id,
            definition.project_root_markers.clone(),
            definition.project_root_strategy,
        ) {
            Ok(generation) => {
                self.pending_lsp_project_root = Some((
                    generation,
                    PendingLspProjectRoot {
                        definition,
                        launch_source,
                        root_id,
                        workspace_root: root_path,
                        buffer,
                        document,
                    },
                ));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn process_lsp_project_root(&mut self) {
        let Some(result) = self
            .workspace
            .project_result(crate::editor::session::workspace::DiscoveryKind::Language)
        else {
            return;
        };
        let Some((generation, pending)) = self.pending_lsp_project_root.take() else {
            return;
        };
        let resolution = match result {
            Ok(resolution) if resolution.generation == generation => resolution,
            Ok(_) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.finish_lsp_project_root(pending, resolution);
    }

    pub(in crate::editor::session) fn queue_nix_lsp_workspace(
        &mut self,
        pending: PendingNixLspWorkspace,
    ) {
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
        {
            self.show_error(error);
            return;
        }
        match self.nix_lsp_workspace.prepare() {
            Ok(generation) => {
                self.pending_nix_lsp_workspace = Some((generation, pending));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn process_nix_lsp_workspace(&mut self) {
        let Some(result) = self.nix_lsp_workspace.try_result() else {
            return;
        };
        let Some((generation, pending)) = self.pending_nix_lsp_workspace.take() else {
            return;
        };
        if result.generation != generation {
            return;
        }
        let workspace_root = match result.result {
            Ok(root) => root,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if self
            .state
            .buffers
            .get(pending.buffer)
            .and_then(|buffer| buffer.path.as_ref())
            .is_none_or(|path| path != &pending.document.path)
            || !self
                .workspace
                .require_path(&pending.document.path)
                .is_ok_and(|root| root == pending.root_id)
            || self
                .workspace
                .authorize_execution(ExecutionSource::ProjectLocal)
                .is_err()
        {
            return;
        }
        if self
            .language
            .running_servers()
            .contains(&pending.session_key)
        {
            self.attach_lsp_document(&pending.session_key, pending.buffer, pending.document);
            return;
        }
        self.queue_lsp_preflight(LspLaunchRequest {
            session_key: pending.session_key,
            definition: pending.definition,
            launch: LspLaunch::NixWorkspace {
                nix: pending.nix,
                workspace_root,
                shell: pending.shell,
            },
            root_id: pending.root_id,
            project_root: pending.project_root,
            buffer: pending.buffer,
            document: pending.document,
            restart: pending.restart,
        });
    }

    pub(in crate::editor::session) fn finish_lsp_project_root(
        &mut self,
        pending: PendingLspProjectRoot,
        resolution: ProjectRootResolution,
    ) {
        if resolution.root_revision != self.workspace.root_revision()
            || resolution.document != pending.document.path
            || resolution.workspace_root != pending.workspace_root
            || self
                .state
                .buffers
                .get(pending.buffer)
                .and_then(|buffer| buffer.path.as_ref())
                != Some(&pending.document.path)
            || !self
                .workspace
                .require_path(&pending.document.path)
                .is_ok_and(|root| root == pending.root_id)
        {
            return;
        }
        let Some(project_root) = resolution.project_root else {
            // An opened source file is not enough evidence to launch a project
            // server. Standalone support must be declared by a future adapter.
            return;
        };
        let session_key = crate::language::lsp::LspSessionKey {
            root_id: pending.root_id.0,
            project_root: project_root.clone(),
            server_id: pending.definition.id.clone(),
        }
        .transport_key();
        let running = self.language.running_servers().contains(&session_key);
        let starting = self.pending_execution_preflight.values().any(|pending| {
            matches!(pending, PendingExecutionPreflight::Lsp { session_key: key, .. } if key == &session_key)
        });
        if running {
            self.attach_lsp_document(&session_key, pending.buffer, pending.document);
            return;
        }
        if starting {
            self.language.select(&session_key);
            // The pending launch owns another document in the same project. A
            // fresh observation after it starts attaches this document exactly.
            self.language.reset_autostart();
            return;
        }
        let restart = self.language.sessions().contains_key(&session_key);
        match pending.launch_source {
            LspLaunchSource::Direct {
                executable,
                managed,
            } => self.queue_lsp_preflight(LspLaunchRequest {
                session_key,
                definition: pending.definition,
                launch: LspLaunch::Direct {
                    executable,
                    managed,
                },
                root_id: pending.root_id,
                project_root,
                buffer: pending.buffer,
                document: pending.document,
                restart,
            }),
            LspLaunchSource::NixWorkspace { nix, shell } => {
                self.queue_nix_lsp_workspace(PendingNixLspWorkspace {
                    session_key,
                    definition: pending.definition,
                    nix,
                    shell,
                    root_id: pending.root_id,
                    project_root,
                    buffer: pending.buffer,
                    document: pending.document,
                    restart,
                });
            }
        };
    }

    pub(in crate::editor::session) fn attach_lsp_document(
        &mut self,
        session_key: &str,
        buffer: crate::editor::BufferId,
        mut document: LspDocument,
    ) {
        let Some(current) = self
            .state
            .buffers
            .get(buffer)
            .filter(|current| current.path.as_ref() == Some(&document.path))
        else {
            return;
        };
        document.version = current.text.revision().0;
        document.text = current.text.to_string();
        let source = current.text.source_version();
        let _ = self
            .language
            .sync_owned(session_key, document, (buffer, source));
        self.language.select(session_key);
    }

    pub(in crate::editor::session) fn queue_lsp_preflight(&mut self, pending: LspLaunchRequest) {
        let request = ExecutionPreflightRequest {
            executable: pending.launch.executable().clone(),
            cwd: pending.project_root.clone(),
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.workspace.request_preflight(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Lsp {
                        session_key: pending.session_key,
                        definition: pending.definition,
                        launch: pending.launch,
                        root_id: pending.root_id,
                        project_root: pending.project_root,
                        buffer: pending.buffer,
                        document: pending.document,
                        restart: pending.restart,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn launch_preflighted_lsp(
        &mut self,
        pending: LspLaunchRequest,
        result: crate::process::ExecutionPreflightResult,
    ) {
        let LspLaunchRequest {
            session_key,
            definition,
            launch,
            root_id,
            project_root,
            buffer,
            mut document,
            restart,
        } = pending;
        if result.root_revision != self.workspace.root_revision() {
            return;
        }
        let Some(current) = self.state.buffers.get(buffer) else {
            return;
        };
        if current.path.as_ref() != Some(&document.path)
            || language_id_for_path(&document.path)
                .is_none_or(|language| language != document.language_id)
            || !definition.supports_language(&document.language_id)
            || !document.path.starts_with(&project_root)
            || result.cwd != project_root
            || !self
                .workspace
                .require_path(&document.path)
                .is_ok_and(|owner| owner == root_id)
        {
            return;
        }
        document.version = current.text.revision().0;
        document.text = current.text.to_string();
        let launch = launch.with_preflighted_executable(result.executable.clone());
        let arguments = launch.arguments(&definition);
        let environment = match launch.environment(&definition) {
            Ok(environment) => environment,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let policy = crate::language::lsp::LspPolicy {
            request_timeout: Duration::from_millis(
                self.configuration.settings().lsp.request_timeout_ms,
            ),
            maximum_log_lines: self.configuration.settings().lsp.maximum_log_lines,
        };
        let server_configuration = if self.configuration.settings().lsp.inlay_hint_level
            == crate::config::InlayHintLevel::Max
        {
            crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&definition.id)
        } else {
            crate::language::lsp::LspServerConfiguration::for_server(
                &definition.id,
                &result.executable,
            )
        };
        match self.language.start(
            crate::language::lsp::LspSessionLaunch {
                session_key: &session_key,
                definition: &definition,
                executable: result.executable.clone(),
                arguments,
                root: result.cwd.clone(),
                document: Some(document.clone()),
                policy,
                configuration: server_configuration,
                environment,
            },
            (buffer, current.text.source_version()),
            (root_id, launch, restart),
            &self.state,
        ) {
            Ok(()) => {
                for prefix in ["lsp.", "lsp-pull:"] {
                    if let Some(producer) = self.language.diagnostic_producer(&session_key, prefix)
                    {
                        self.diagnostics.activate(producer);
                    }
                }
                self.sync_ui_diagnostics();
            }
            Err(error) => self.show_error(error),
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
        self.language.forget_synced(&server);
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
