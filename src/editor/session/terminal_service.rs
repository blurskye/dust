use super::*;

impl InteractiveApplication {
    pub(super) fn open_terminal_after_validation_with_response(
        &mut self,
        profile: &str,
        dap_request: Option<i64>,
    ) {
        let Some(config) = self
            .configuration
            .settings
            .terminals
            .profiles
            .get(profile)
            .cloned()
        else {
            self.show_error(self.message("terminal_unknown", &[("profile", profile.into())]));
            return;
        };
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
        let cwd = config.cwd.clone().map_or_else(
            || root.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            },
        );
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        let request = ExecutionPreflightRequest {
            executable: config.program.clone(),
            cwd,
            roots,
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Terminal {
                        profile: profile.into(),
                        config,
                        dap_request,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn process_execution_preflight(&mut self) {
        let Ok(events) = self.execution_preflight.poll() else {
            return;
        };
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(result) => {
                    let Some(action) = self.pending_execution_preflight.remove(&result.id) else {
                        continue;
                    };
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        continue;
                    }
                    match action {
                        PendingExecutionPreflight::Lsp {
                            session_key,
                            definition,
                            launch,
                            root_id,
                            project_root,
                            buffer,
                            document,
                            restart,
                        } => self.launch_preflighted_lsp(
                            super::lsp::LspLaunchRequest {
                                session_key,
                                definition,
                                launch,
                                root_id,
                                project_root,
                                buffer,
                                document,
                                restart,
                            },
                            result,
                        ),
                        PendingExecutionPreflight::Task {
                            requested,
                            name,
                            remaining,
                            task,
                            adapter,
                        } => {
                            let policy = &self.configuration.settings.processes;
                            let request = JobRequest {
                                program: result.executable,
                                arguments: task.arguments,
                                cwd: result.cwd,
                                environment: task.environment,
                                root_revision: result.root_revision,
                                stdin: match &adapter {
                                    AdapterContext::Formatter { source, .. }
                                        if name == requested =>
                                    {
                                        Some(source.clone())
                                    }
                                    _ => None,
                                },
                                policy: JobPolicy {
                                    maximum_output_bytes: policy.maximum_output_bytes,
                                    maximum_line_bytes: policy.maximum_line_bytes,
                                    maximum_stdin_bytes: policy.maximum_stdin_bytes,
                                    timeout: Duration::from_millis(
                                        task.timeout_ms.unwrap_or(policy.maximum_timeout_ms),
                                    ),
                                },
                            };
                            match self.jobs.start(request) {
                                Ok(job) => {
                                    self.task_status
                                        .insert(requested.clone(), format!("running:{name}"));
                                    self.job_tasks.insert(
                                        job,
                                        RunningTask {
                                            requested,
                                            name,
                                            root_revision: result.root_revision,
                                            remaining,
                                            adapter,
                                            root: result.root,
                                        },
                                    );
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        PendingExecutionPreflight::DapToolAdapter {
                            definition,
                            project_root,
                            program,
                        } => {
                            if result.cwd != project_root {
                                continue;
                            }
                            let request = ExecutionPreflightRequest {
                                executable: program,
                                cwd: result.cwd.clone(),
                                roots: self
                                    .workspace
                                    .roots()
                                    .iter()
                                    .map(|root| root.identity.canonical_path.clone())
                                    .collect(),
                                root_revision: result.root_revision,
                            };
                            match self.execution_preflight.request(request) {
                                Ok(id) => {
                                    self.pending_execution_preflight.insert(
                                        id,
                                        PendingExecutionPreflight::DapToolProgram {
                                            definition,
                                            adapter_executable: result.executable,
                                            cwd: result.cwd,
                                            root_revision: result.root_revision,
                                        },
                                    );
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        PendingExecutionPreflight::DapToolProgram {
                            definition,
                            adapter_executable,
                            cwd,
                            root_revision,
                        } => {
                            let name = definition.name.clone();
                            let launch = crate::debugger::DapLaunch {
                                adapter: definition.id,
                                executable: adapter_executable,
                                arguments: definition.launch_args,
                                environment: HashMap::new(),
                                path_maps: BTreeMap::new(),
                                root: cwd.clone(),
                                request: "launch".into(),
                                request_arguments: serde_json::json!({
                                    "program": result.executable,
                                    "cwd": cwd,
                                    "stopOnEntry": false,
                                }),
                            };
                            match self.dap.start_absolute(launch) {
                                Ok(()) => {
                                    self.ui.notification =
                                        Some(self.message("dap_started", &[("adapter", name)]));
                                    self.ui.notification_error = false;
                                }
                                Err(error) => self.show_error(error),
                            }
                            let _ = root_revision;
                        }
                        PendingExecutionPreflight::DapAdapter {
                            name,
                            adapter,
                            session,
                        } => match session.request {
                            crate::config::DebugRequestKind::Attach => {
                                self.launch_preflighted_dap(super::debugger::PreflightedDapLaunch {
                                    name,
                                    adapter,
                                    session,
                                    adapter_executable: result.executable,
                                    cwd: result.cwd,
                                    root_revision: result.root_revision,
                                    program: None,
                                })
                            }
                            crate::config::DebugRequestKind::Launch => {
                                let Some(program) = session.program.clone() else {
                                    self.show_error("launch debug configuration requires program");
                                    continue;
                                };
                                let program = if program.is_absolute() {
                                    program
                                } else {
                                    result.root.join(program)
                                };
                                let request = ExecutionPreflightRequest {
                                    executable: program,
                                    cwd: result.cwd.clone(),
                                    roots: self
                                        .workspace
                                        .roots()
                                        .iter()
                                        .map(|root| root.identity.canonical_path.clone())
                                        .collect(),
                                    root_revision: result.root_revision,
                                };
                                match self.execution_preflight.request(request) {
                                    Ok(id) => {
                                        self.pending_execution_preflight.insert(
                                            id,
                                            PendingExecutionPreflight::DapProgram {
                                                name,
                                                adapter,
                                                session,
                                                adapter_executable: result.executable,
                                                cwd: result.cwd,
                                                root_revision: result.root_revision,
                                            },
                                        );
                                    }
                                    Err(error) => self.show_error(error),
                                }
                            }
                        },
                        PendingExecutionPreflight::DapProgram {
                            name,
                            adapter,
                            session,
                            adapter_executable,
                            cwd,
                            root_revision,
                        } => self.launch_preflighted_dap(super::debugger::PreflightedDapLaunch {
                            name,
                            adapter,
                            session,
                            adapter_executable,
                            cwd,
                            root_revision,
                            program: Some(result.executable),
                        }),
                        PendingExecutionPreflight::Terminal {
                            profile,
                            config,
                            dap_request,
                        } => {
                            let policy = &self.configuration.settings.terminals;
                            let (columns, rows) = terminal_panel_geometry(
                                &self.configuration.settings,
                                self.terminal_height,
                                self.terminal_panel_height,
                            )
                            .map_or(
                                (config.columns, config.rows),
                                |geometry| {
                                    (self.terminal_width.max(1), geometry.body_height.max(1))
                                },
                            );
                            let request = PtyRequest {
                                program: result.executable,
                                arguments: config.arguments,
                                cwd: result.cwd,
                                environment: config.environment,
                                columns,
                                rows,
                                root_revision: result.root_revision,
                                policy: PtyPolicy {
                                    maximum_output_bytes: policy.maximum_output_bytes,
                                    maximum_input_bytes: policy.maximum_input_bytes,
                                    event_capacity: policy.queue_capacity,
                                    scrollback_bytes: policy.scrollback_bytes,
                                },
                            };
                            match self.ptys.open(request) {
                                Ok(id) => {
                                    self.terminals.insert(
                                        id,
                                        TerminalRuntime {
                                            profile,
                                            root_revision: result.root_revision,
                                            screen: PtyScreen::new(
                                                columns,
                                                rows,
                                                self.configuration
                                                    .settings
                                                    .terminals
                                                    .scrollback_bytes
                                                    .saturating_div(usize::from(columns.max(1)))
                                                    .max(1),
                                            ),
                                            running: true,
                                            exit_code: None,
                                            truncated: false,
                                            mode: TerminalPanelMode::Input,
                                            normal_cursor: None,
                                            visual_anchor: None,
                                            visual_linewise: false,
                                            scroll_offset: 0,
                                        },
                                    );
                                    self.terminal_set_revision =
                                        self.terminal_set_revision.saturating_add(1);
                                    self.active_terminal = Some(id);
                                    self.terminal_focused = true;
                                    if let Some(request) = dap_request {
                                        self.dap_terminal_responses.insert(id, request);
                                    }
                                    self.resize_active_terminal_to_panel();
                                    self.sync_terminal_panel();
                                    self.sync_explorer_ui();
                                    self.ensure_cursor_visible();
                                }
                                Err(error) => {
                                    if let Some(request_seq) = dap_request {
                                        let _ = self.dap.control(DapControl::ReverseResponse {
                                            request_seq,
                                            success: false,
                                            message: Some(error.to_string()),
                                            process_id: None,
                                        });
                                    }
                                    self.show_error(error)
                                }
                            }
                        }
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    let action = self.pending_execution_preflight.remove(&id);
                    if let Some(PendingExecutionPreflight::Terminal {
                        dap_request: Some(request_seq),
                        ..
                    }) = &action
                    {
                        let _ = self.dap.control(DapControl::ReverseResponse {
                            request_seq: *request_seq,
                            success: false,
                            message: Some(message.clone()),
                            process_id: None,
                        });
                    }
                    if root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }

    pub(super) fn process_pty_events(&mut self) {
        if let Ok(events) = self.ptys.poll() {
            if events.is_empty() {
                return;
            }
            for event in events {
                match event {
                    PtyEvent::Started { id, root_revision } => {
                        if let Some(request_seq) = self.dap_terminal_responses.remove(&id) {
                            let success = root_revision == self.workspace.root_revision();
                            let _ = self.dap.control(DapControl::ReverseResponse {
                                request_seq,
                                success,
                                message: (!success).then(|| "workspace root changed".into()),
                                process_id: None,
                            });
                        }
                        if root_revision != self.workspace.root_revision() {
                            let _ = self.ptys.close(id);
                        }
                    }
                    PtyEvent::Output {
                        id,
                        bytes,
                        truncated,
                    } => {
                        if let Some(terminal) = self.terminals.get_mut(&id).filter(|terminal| {
                            terminal.root_revision == self.workspace.root_revision()
                        }) {
                            terminal.screen.feed_bytes(&bytes);
                            terminal.truncated |= truncated;
                            if terminal.mode != TerminalPanelMode::Input {
                                if let Some(cursor) = terminal.normal_cursor {
                                    terminal.normal_cursor =
                                        Some(clamp_terminal_cursor(&terminal.screen, cursor));
                                }
                                keep_terminal_cursor_visible(terminal);
                            }
                        }
                    }
                    PtyEvent::Exited { id, code } => {
                        if code == Some(0) {
                            self.retire_terminal(id, false);
                        } else if let Some(terminal) = self.terminals.get_mut(&id) {
                            terminal.screen.finish_bytes();
                            terminal.running = false;
                            terminal.exit_code = code;
                            let diagnostic = code.map_or_else(
                                || "terminal exited by signal".to_owned(),
                                |code| format!("terminal exited with code {code}"),
                            );
                            self.show_error(diagnostic);
                        }
                    }
                    PtyEvent::Failed { id, message } => {
                        if let Some(request_seq) = self.dap_terminal_responses.remove(&id) {
                            let _ = self.dap.control(DapControl::ReverseResponse {
                                request_seq,
                                success: false,
                                message: Some(message.clone()),
                                process_id: None,
                            });
                        }
                        if let Some(terminal) = self.terminals.get_mut(&id) {
                            terminal.screen.finish_bytes();
                            terminal.running = false;
                        }
                        self.show_error(message);
                    }
                }
            }
            self.sync_terminal_panel();
        }
    }
}
