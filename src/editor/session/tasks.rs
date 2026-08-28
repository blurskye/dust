use super::*;

impl InteractiveApplication {
    pub(super) fn start_task_after_validation(&mut self, name: &str) {
        self.start_task_plan(name, AdapterContext::Plain);
    }

    pub(super) fn start_task_plan(&mut self, name: &str, adapter: AdapterContext) {
        if self.job_tasks.values().any(|task| task.requested == name) {
            self.show_error(self.message("task_already_running", &[("name", name.into())]));
            return;
        }
        let Some(mut plan) = task_plan(name, &self.configuration.settings.tasks) else {
            self.show_error(self.message("task_unknown", &[("name", name.into())]));
            return;
        };
        self.task_output.insert(name.to_owned(), Vec::new());
        self.task_output_rows.insert(name.to_owned(), Vec::new());
        self.task_status.insert(name.to_owned(), "queued".into());
        self.start_task_step(name.to_owned(), &mut plan, adapter);
    }

    pub(super) fn start_task_step(
        &mut self,
        requested: String,
        plan: &mut Vec<String>,
        adapter: AdapterContext,
    ) {
        if self.job_tasks.len() >= self.configuration.settings.processes.maximum_running
            || plan.is_empty()
        {
            return;
        }
        let name = plan.remove(0);
        let Some(task) = self.configuration.settings.tasks.get(&name).cloned() else {
            return;
        };
        let Some(root_id) = self.workspace.active_root() else {
            self.show_error(self.message("task_no_root", &[]));
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
        let cwd = task.cwd.clone().map_or_else(
            || root_path.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root_path.join(path)
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
            executable: task.executable.clone(),
            cwd,
            roots,
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Task {
                        requested,
                        name,
                        remaining: plan.clone(),
                        task,
                        adapter,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn apply_formatter_result(&mut self, task: &RunningTask) {
        let AdapterContext::Formatter {
            buffer, revision, ..
        } = task.adapter
        else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(replacement) = String::from_utf8(bytes.clone()) else {
            self.show_error(self.message("formatter_invalid_utf8", &[]));
            return;
        };
        let Some(state) = self.state.buffers.get(buffer) else {
            return;
        };
        if state.text.revision() != revision {
            self.show_error(self.message("formatter_stale", &[]));
            return;
        }
        let end = ByteOffset(state.text.len_bytes());
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision { buffer, revision })
            .with_precondition(Precondition::BufferWritable(buffer))
            .with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    revision,
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), end).expect("ordered"),
                        replacement,
                    )],
                ),
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.ui.notification = Some(self.message("formatter_applied", &[]));
            self.ui.notification_error = false;
        }
    }

    pub(super) fn apply_linter_result(&mut self, task: &RunningTask) {
        let AdapterContext::Linter { matcher } = &task.adapter else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some(regex) = self.matcher_cache.get(&task.requested) else {
            return;
        };
        let Ok(owner) = DiagnosticOwner::new(format!("task:{}", task.requested)) else {
            return;
        };
        let mut grouped: BTreeMap<String, Vec<Diagnostic>> = BTreeMap::new();
        for captures in regex
            .captures_iter(text)
            .take(self.configuration.settings.workspace.maximum_diagnostics)
        {
            let Some(path) = captures
                .name(&matcher.path_group)
                .map(|value| PathBuf::from(value.as_str()))
            else {
                continue;
            };
            let path = if path.is_absolute() {
                path
            } else {
                task.root.join(path)
            };
            if !path.starts_with(&task.root) {
                continue;
            }
            let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            else {
                continue;
            };
            let line = captures
                .name(&matcher.line_group)
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let column = matcher
                .column_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let Some(start) = buffer
                .text
                .line_to_byte(line)
                .ok()
                .map(|offset| ByteOffset((offset.get() + column).min(buffer.text.len_bytes())))
            else {
                continue;
            };
            if buffer.text.validate_offset(start).is_err() {
                continue;
            }
            let severity = matcher
                .severity_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .map(|value| value.as_str())
                .map_or(DiagnosticSeverity::Error, |value| {
                    match value.to_ascii_lowercase().as_str() {
                        "warning" | "warn" => DiagnosticSeverity::Warning,
                        "info" | "information" => DiagnosticSeverity::Information,
                        "hint" => DiagnosticSeverity::Hint,
                        _ => DiagnosticSeverity::Error,
                    }
                });
            let message = captures
                .name(&matcher.message_group)
                .map(|value| value.as_str().chars().take(4096).collect())
                .unwrap_or_default();
            let uri = crate::language::lsp::file_uri(&path);
            grouped.entry(uri.clone()).or_default().push(Diagnostic {
                uri,
                version: Some(buffer.text.revision()),
                range: ByteRange::new(start, start).expect("empty range"),
                severity,
                source: task.requested.clone(),
                code: None,
                message,
                related: Vec::new(),
                tags: Vec::new(),
                fixes: Vec::new(),
                timestamp: crate::clock::system_time(),
            });
        }
        let mut unopened = BTreeMap::<PathBuf, Vec<RawLinterDiagnostic>>::new();
        for captures in regex
            .captures_iter(text)
            .take(self.configuration.settings.workspace.maximum_diagnostics)
        {
            let Some(raw_path) = captures
                .name(&matcher.path_group)
                .map(|value| PathBuf::from(value.as_str()))
            else {
                continue;
            };
            let path = if raw_path.is_absolute() {
                raw_path
            } else {
                task.root.join(raw_path)
            };
            if !path.starts_with(&task.root)
                || self
                    .state
                    .buffers
                    .iter()
                    .any(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                continue;
            }
            let line = captures
                .name(&matcher.line_group)
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let column = matcher
                .column_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let severity = matcher
                .severity_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .map(|value| value.as_str().to_ascii_lowercase())
                .map_or(DiagnosticSeverity::Error, |value| match value.as_str() {
                    "warning" | "warn" => DiagnosticSeverity::Warning,
                    "info" | "information" => DiagnosticSeverity::Information,
                    "hint" => DiagnosticSeverity::Hint,
                    _ => DiagnosticSeverity::Error,
                });
            let message = captures
                .name(&matcher.message_group)
                .map(|value| value.as_str().chars().take(4096).collect())
                .unwrap_or_default();
            unopened.entry(path).or_default().push(RawLinterDiagnostic {
                line,
                column,
                severity,
                message,
            });
        }
        if !unopened.is_empty() {
            let generation = self.next_linter_generation;
            self.next_linter_generation = self.next_linter_generation.saturating_add(1);
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect::<Vec<_>>();
            let mut waiting = 0;
            for (path, raw) in unopened {
                match self.vfs.read_scoped(path, roots.clone()) {
                    Ok(id) => {
                        self.pending_vfs
                            .insert(id, PendingVfs::LinterRead { generation, raw });
                        waiting += 1
                    }
                    Err(error) => {
                        self.logs.record(
                            LogLevel::Error,
                            "linter",
                            "source-read-queue-failed",
                            BTreeMap::from([("error".into(), error.to_string())]),
                        );
                        return;
                    }
                }
            }
            self.pending_linter_builds.insert(
                generation,
                PendingLinterBuild {
                    owner,
                    root_revision: self.workspace.root_revision(),
                    waiting,
                    failed: false,
                    grouped,
                },
            );
            return;
        }
        self.diagnostics.clear_owner(&owner);
        for (uri, items) in grouped {
            let current = items.first().and_then(|item| item.version);
            let _ = self
                .diagnostics
                .replace(owner.clone(), uri, current, current, items);
        }
        self.sync_ui_diagnostics();
    }

    pub(super) fn finish_linter_build(&mut self, generation: u64) {
        let Some(build) = self.pending_linter_builds.remove(&generation) else {
            return;
        };
        if build.failed || build.root_revision != self.workspace.root_revision() {
            return;
        }
        self.diagnostics.clear_owner(&build.owner);
        for (uri, items) in build.grouped {
            let current = items.first().and_then(|item| item.version);
            let _ = self
                .diagnostics
                .replace(build.owner.clone(), uri, current, current, items);
        }
        self.sync_ui_diagnostics();
    }

    pub(super) fn apply_test_result(&mut self, task: &RunningTask) {
        let AdapterContext::Test { matcher } = &task.adapter else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some(regex) = self.matcher_cache.get(&task.requested) else {
            return;
        };
        let cases = regex
            .captures_iter(text)
            .take(10_000)
            .map(|captures| TestCaseResult {
                name: matcher
                    .name_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(512).collect())
                    .unwrap_or_else(|| "test".into()),
                status: matcher
                    .status_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(32).collect())
                    .unwrap_or_else(|| "unknown".into()),
                duration: matcher
                    .duration_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(64).collect())
                    .unwrap_or_default(),
                message: captures
                    .name(&matcher.message_group)
                    .map(|value| value.as_str().chars().take(2048).collect())
                    .unwrap_or_default(),
                source: captures
                    .name(&matcher.path_group)
                    .map(|value| PathBuf::from(value.as_str()))
                    .map(|path| MatcherSource {
                        path: if path.is_absolute() {
                            path
                        } else {
                            task.root.join(path)
                        },
                        line: captures
                            .name(&matcher.line_group)
                            .and_then(|value| value.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                        column: matcher
                            .column_group
                            .as_ref()
                            .and_then(|name| captures.name(name))
                            .and_then(|value| value.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                    })
                    .filter(|source| source.path.starts_with(&task.root)),
            })
            .collect();
        self.test_results.insert(task.requested.clone(), cases);
    }

    pub(super) fn show_task_picker(&mut self) {
        let items = self
            .task_status
            .iter()
            .map(|(name, status)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("task:{name}")),
                        label: name.clone(),
                        detail: status.clone(),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_tasks_title", &[]), items);
    }

    pub(super) fn show_output_picker(&mut self, name: &str) {
        let items = self
            .task_output_rows
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("output:{name}:{index}")),
                        label: row.text.clone(),
                        detail: format!(
                            "{:?}{}",
                            row.stream,
                            if row.truncated { " · truncated" } else { "" }
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Output,
            self.message("picker_output_title", &[("name", name.into())]),
            items,
        );
    }

    pub(super) fn show_test_picker(&mut self, name: &str) {
        let items = self
            .test_results
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, case)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("test:{name}:{index}")),
                        label: case.name.clone(),
                        detail: format!("{} {}", case.status, case.duration),
                        source: case.message.clone(),
                        action: PickerAction::None,
                    },
                    case.source.clone().map_or(
                        PickerPayload::Command(String::new()),
                        PickerPayload::MatcherSource,
                    ),
                )
            })
            .collect();
        self.install_picker(
            self.message("picker_tests_title", &[("name", name.into())]),
            items,
        );
    }

    pub(super) fn process_job_events(&mut self) {
        let revision = self.workspace.root_revision();
        for event in self.jobs.poll() {
            match event {
                JobEvent::Output {
                    job,
                    stream,
                    bytes,
                    truncated,
                } => {
                    if truncated {
                        self.task_truncated.insert(job);
                    }
                    if let Some(task) = self
                        .job_tasks
                        .get(&job)
                        .filter(|task| task.root_revision == revision)
                    {
                        if task.name == task.requested
                            && matches!(task.adapter, AdapterContext::Formatter { .. })
                            && stream == JobStream::Stderr
                        {
                            continue;
                        }
                        let output = self.task_output.entry(task.requested.clone()).or_default();
                        let remaining = self
                            .configuration
                            .settings
                            .processes
                            .maximum_output_bytes
                            .saturating_sub(output.len());
                        let accepted = &bytes[..bytes.len().min(remaining)];
                        output.extend_from_slice(accepted);
                        let rows = self
                            .task_output_rows
                            .entry(task.requested.clone())
                            .or_default();
                        for line in String::from_utf8_lossy(accepted).lines().take(1024) {
                            rows.push(TaskOutputRecord {
                                stream,
                                text: line
                                    .chars()
                                    .filter(|character| !character.is_control())
                                    .take(4096)
                                    .collect(),
                                truncated,
                            });
                        }
                        if rows.len() > 10_000 {
                            rows.drain(..rows.len() - 10_000);
                        }
                    }
                }
                JobEvent::Exited { job, code, .. } => {
                    if let Some(task) = self.job_tasks.remove(&job) {
                        let truncated = self.task_truncated.remove(&job);
                        if task.root_revision != revision {
                            continue;
                        }
                        if code == Some(0) && !task.remaining.is_empty() {
                            let mut remaining = task.remaining;
                            self.start_task_step(task.requested, &mut remaining, task.adapter);
                        } else {
                            if code == Some(0) && !truncated {
                                self.apply_formatter_result(&task);
                                self.apply_linter_result(&task);
                                self.apply_test_result(&task);
                            } else if matches!(task.adapter, AdapterContext::Formatter { .. })
                                && truncated
                            {
                                self.show_error(self.message("formatter_truncated", &[]));
                            }
                            self.task_status.insert(
                                task.requested.clone(),
                                if code == Some(0) {
                                    "succeeded".into()
                                } else {
                                    "failed".into()
                                },
                            );
                            self.ui.notification = Some(self.message(
                                "task_exited",
                                &[
                                    ("name", task.name),
                                    (
                                        "code",
                                        code.map_or_else(
                                            || "signal".into(),
                                            |value| value.to_string(),
                                        ),
                                    ),
                                ],
                            ));
                            self.ui.notification_error = code != Some(0);
                        }
                    }
                }
                JobEvent::Started { job, root_revision } => {
                    if root_revision != revision {
                        let _ = self.jobs.cancel(job);
                    }
                }
                JobEvent::Failed { job, message } => {
                    self.job_tasks.remove(&job);
                    self.show_error(self.message("task_failed", &[("diagnostic", message)]));
                }
            }
        }
    }
}

pub(in crate::editor::session) fn task_plan(
    name: &str,
    tasks: &BTreeMap<String, crate::config::TaskConfig>,
) -> Option<Vec<String>> {
    fn visit(
        name: &str,
        tasks: &BTreeMap<String, crate::config::TaskConfig>,
        seen: &mut BTreeSet<String>,
        plan: &mut Vec<String>,
    ) -> Option<()> {
        if !seen.insert(name.into()) {
            return Some(());
        }
        let task = tasks.get(name)?;
        for dependency in &task.dependencies {
            visit(dependency, tasks, seen, plan)?;
        }
        plan.push(name.into());
        Some(())
    }
    let mut seen = BTreeSet::new();
    let mut plan = Vec::new();
    visit(name, tasks, &mut seen, &mut plan)?;
    Some(plan)
}
