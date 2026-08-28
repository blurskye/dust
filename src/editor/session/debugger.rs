use super::*;

pub(super) struct PreflightedDapLaunch {
    pub name: String,
    pub adapter: crate::config::DebugAdapterConfig,
    pub session: crate::config::DebugSessionConfig,
    pub adapter_executable: PathBuf,
    pub cwd: PathBuf,
    pub root_revision: u64,
    pub program: Option<PathBuf>,
}

impl InteractiveApplication {
    pub(super) fn start_dap_configuration(&mut self, name: &str) {
        let Some(session) = self
            .configuration
            .settings
            .debug
            .sessions
            .get(name)
            .cloned()
        else {
            self.show_error(format!("unknown debug configuration `{name}`"));
            return;
        };
        let Some(adapter) = self
            .configuration
            .settings
            .debug
            .adapters
            .get(&session.adapter)
            .cloned()
        else {
            self.show_error("debug adapter configuration is missing");
            return;
        };
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::StartDapConfiguration(name.into()));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::DebugProgram)
        {
            self.show_error(error);
            return;
        }
        let Some(root_id) = self.workspace.active_root() else {
            return;
        };
        let Some(root) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let cwd = session.cwd.clone().map_or_else(
            || root.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            },
        );
        let request = ExecutionPreflightRequest {
            executable: adapter.program.clone(),
            cwd,
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::DapAdapter {
                        name: name.into(),
                        adapter,
                        session,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn launch_preflighted_dap(&mut self, launch: PreflightedDapLaunch) {
        let PreflightedDapLaunch {
            name,
            adapter,
            session,
            adapter_executable,
            cwd,
            root_revision,
            program,
        } = launch;
        if root_revision != self.workspace.root_revision() {
            return;
        }
        let request = match session.request {
            crate::config::DebugRequestKind::Launch => "launch",
            crate::config::DebugRequestKind::Attach => "attach",
        };
        let path_maps = session.path_maps.clone();
        let terminal_profile = session.run_in_terminal_profile.clone();
        let mut arguments = serde_json::Map::from_iter(session.arguments);
        arguments.insert(
            "cwd".into(),
            serde_json::Value::String(cwd.display().to_string()),
        );
        if let Some(program) = program {
            arguments.insert(
                "program".into(),
                serde_json::Value::String(program.display().to_string()),
            );
        }
        let environment = adapter.environment.into_iter().collect();
        match self.dap.start_absolute(crate::debugger::DapLaunch {
            adapter: session.adapter,
            executable: adapter_executable,
            arguments: adapter.arguments,
            environment,
            path_maps,
            root: cwd,
            request: request.into(),
            request_arguments: serde_json::Value::Object(arguments),
        }) {
            Ok(()) => {
                self.active_debug_configuration = Some(name.clone());
                self.active_debug_terminal_profile = terminal_profile;
                self.ui.notification = Some(self.message("dap_started", &[("adapter", name)]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn start_dap(&mut self, requested: Option<String>, program: Option<PathBuf>) {
        let Some((_, document)) = self.active_lsp_document() else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        let entry = requested
            .as_ref()
            .and_then(|id| self.tools.iter().find(|entry| entry.definition.id == *id))
            .or_else(|| {
                self.tools.iter().find(|entry| {
                    entry.definition.kind == ToolKind::Dap
                        && entry.definition.languages.contains(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
            });
        let Some(entry) = entry else {
            self.show_error(self.message("dap_no_adapter", &[]));
            return;
        };
        let ToolStatus::Installed(adapter_executable) = &entry.status else {
            self.show_error(self.message("dap_no_adapter", &[]));
            return;
        };
        let definition = entry.definition.clone();
        let adapter_executable = adapter_executable.clone();
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::StartDap {
                requested: Some(definition.id.clone()),
                program,
            });
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::DebugProgram)
        {
            self.show_error(error);
            return;
        }
        let program = match program {
            Some(program) => program,
            None if definition.debug_program_policy == crate::tools::DebugProgramPolicy::Source => {
                document.path.clone()
            }
            None => {
                self.show_error(
                    "this adapter requires a compiled program; use :dap.session.start <program>",
                );
                return;
            }
        };
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        if !self
            .workspace
            .require_path(&program)
            .is_ok_and(|owner| owner == root_id)
        {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        }
        let Some(workspace_root) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        match self.dap_project_roots.resolve(
            document.path.clone(),
            workspace_root.clone(),
            self.workspace.root_revision(),
            definition.project_root_markers.clone(),
            definition.project_root_strategy,
        ) {
            Ok(generation) => {
                self.pending_dap_project_root = Some((
                    generation,
                    PendingDapProjectRoot {
                        definition,
                        adapter_executable,
                        root_id,
                        workspace_root,
                        document,
                        program,
                    },
                ));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn process_dap_events(&mut self) {
        while let Some(event) = self.dap.try_event() {
            match event {
                DapEvent::Starting { .. } => {}
                DapEvent::Running { adapter } => {
                    self.ui.notification =
                        Some(self.message("dap_running", &[("adapter", adapter)]));
                    self.ui.notification_error = false;
                }
                DapEvent::Stopped {
                    adapter, reason, ..
                } => {
                    self.ui.notification = Some(
                        self.message("dap_stopped", &[("adapter", adapter), ("reason", reason)]),
                    );
                    self.ui.notification_error = false;
                    for expression in self.dap_watches.iter().take(100) {
                        let _ = self.dap.control(DapControl::Evaluate {
                            expression: expression.clone(),
                            frame_id: None,
                            context: "watch".into(),
                        });
                    }
                }
                DapEvent::Output { adapter, output } => {
                    self.dap_console
                        .push_back((adapter.clone(), output.clone()));
                    while self.dap_console.len() > 10_000 {
                        self.dap_console.pop_front();
                    }
                    self.ui.notification = Some(self.message(
                        "dap_output",
                        &[("adapter", adapter), ("output", output.replace('\n', " "))],
                    ));
                    self.ui.notification_error = false;
                }
                DapEvent::Response {
                    stop_generation,
                    response,
                    ..
                } => {
                    self.dap_responses
                        .push_back((stop_generation, response.clone()));
                    while self.dap_responses.len() > 256 {
                        self.dap_responses.pop_front();
                    }
                    match response {
                        DapResponse::Threads(rows) => self.open_dap_picker(
                            "threads",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        row.name,
                                        row.id.to_string(),
                                        DapControl::StackTrace {
                                            thread_id: row.id,
                                            start: 0,
                                            levels: 256,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Stack(rows) => self.open_dap_frames_picker(rows),
                        DapResponse::Scopes(rows) => self.open_dap_picker(
                            "scopes",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        row.name,
                                        if row.expensive {
                                            "expensive".into()
                                        } else {
                                            String::new()
                                        },
                                        DapControl::Variables {
                                            reference: row.variables_reference,
                                            start: 0,
                                            count: 1000,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Variables(rows) => self.open_dap_picker(
                            "variables",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        format!("{} = {}", row.name, row.value),
                                        row.type_name.unwrap_or_default(),
                                        DapControl::Variables {
                                            reference: row.variables_reference,
                                            start: 0,
                                            count: 1000,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Evaluate(value) => {
                            self.ui.notification = Some(self.message(
                                "dap_evaluate_result",
                                &[("name", value.name), ("value", value.value)],
                            ));
                            self.ui.notification_error = false;
                        }
                        DapResponse::Breakpoints(rows) => {
                            self.ui.notification = Some(self.message(
                                "dap_breakpoints_synced",
                                &[("count", rows.len().to_string())],
                            ));
                            self.ui.notification_error = false;
                        }
                        DapResponse::Source {
                            name,
                            content,
                            mime_type,
                        } => {
                            let (buffer, _) =
                                self.state.open_text(TextBuffer::from_text(&content), None);
                            self.state.buffers[buffer].read_only = true;
                            self.state.set_buffer_dirty(buffer, false);
                            self.ui.notification = Some(self.message(
                                "dap_source_loaded",
                                &[("name", name), ("mime", mime_type.unwrap_or_default())],
                            ));
                            self.ui.notification_error = false;
                        }
                    }
                }
                DapEvent::Terminated { adapter } => {
                    let _ = self.dap.stop();
                    self.ui.notification = Some(self.message(
                        "dap_stopped",
                        &[("adapter", adapter), ("reason", "terminated".into())],
                    ));
                    self.ui.notification_error = false;
                }
                DapEvent::RunInTerminal {
                    request_seq,
                    cwd,
                    arguments,
                    environment,
                    ..
                } => {
                    let Some(profile) = self.active_debug_terminal_profile.clone() else {
                        let _ = self.dap.control(DapControl::ReverseResponse {
                            request_seq,
                            success: false,
                            message: Some(
                                "runInTerminal is not enabled by the debug configuration".into(),
                            ),
                            process_id: None,
                        });
                        continue;
                    };
                    let Some(config) = self
                        .configuration
                        .settings
                        .terminals
                        .profiles
                        .get(&profile)
                        .cloned()
                    else {
                        continue;
                    };
                    let root = self
                        .workspace
                        .active_root()
                        .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                        .map(|root| root.identity.canonical_path.clone());
                    let expected_cwd = config.cwd.clone().map_or_else(
                        || root.clone().unwrap_or_default(),
                        |path| {
                            if path.is_absolute() {
                                path
                            } else {
                                root.clone().unwrap_or_default().join(path)
                            }
                        },
                    );
                    let valid = arguments.first()
                        == config.program.to_str().map(str::to_owned).as_ref()
                        && arguments.get(1..) == Some(config.arguments.as_slice())
                        && cwd == expected_cwd
                        && environment == config.environment.clone().into_iter().collect();
                    if !valid {
                        let _=self.dap.control(DapControl::ReverseResponse{request_seq,success:false,message:Some("runInTerminal request differs from the configured terminal profile".into()),process_id:None});
                        continue;
                    }
                    self.execution_revalidated = false;
                    self.defer_trusted_execution(DeferredExecution::OpenDapTerminal {
                        profile,
                        request_seq,
                    });
                }
                DapEvent::Failed { adapter, message } => {
                    let _ = self.dap.stop();
                    self.show_error(self.message(
                        "dap_failed",
                        &[("adapter", adapter), ("diagnostic", message)],
                    ));
                }
            }
        }
    }
}
