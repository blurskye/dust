use super::*;

impl InteractiveApplication {
    pub(super) fn stop_task(&mut self, name: &str) {
        match self.tasks.stop(name) {
            Ok(Some(source)) => {
                self.diagnostics.clear_source(&source);
                self.sync_ui_diagnostics();
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn start_task_after_validation(
        &mut self,
        name: &str,
        authorization: Option<super::effects::ExecutionAuthorization>,
    ) {
        self.start_task_plan(name, AdapterContext::Plain, authorization);
    }

    pub(super) fn start_task_plan(
        &mut self,
        name: &str,
        adapter: AdapterContext,
        authorization: Option<super::effects::ExecutionAuthorization>,
    ) {
        let kind = match &adapter {
            AdapterContext::Plain => crate::config::TaskKind::Task,
            AdapterContext::Formatter { .. } => crate::config::TaskKind::Formatter,
            AdapterContext::Linter { .. } => crate::config::TaskKind::Linter,
            AdapterContext::Test { .. } => crate::config::TaskKind::Test,
        };
        let Some(permit) = authorization
            .and_then(|authorization| authorization.task_permit(&self.workspace, name, kind))
        else {
            self.show_error("task launch requires its requesting execution authorization");
            return;
        };
        match self.tasks.plan(
            name,
            &adapter,
            self.configuration.settings(),
            &self.workspace,
            self.configuration.binding_revision(),
            permit,
        ) {
            Ok(()) => {}
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
            self.diagnostics.clear_owner(&owner);
            self.sync_ui_diagnostics();
        }
        self.start_task_step(name.into());
    }
    pub(super) fn start_task_step(&mut self, requested: String) {
        let (step, _) = match self.tasks.step(
            &requested,
            &self.workspace,
            self.configuration.binding_revision(),
        ) {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.tasks.queue_preflight(
            step,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }

    pub(super) fn apply_formatter_result(&mut self, task: &RunningTask) {
        let transaction = self.transaction_id();
        match self.tasks.formatter_transaction(
            task,
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            transaction,
        ) {
            Ok(Some(transaction)) => match self
                .state
                .prepare_transaction(&transaction)
                .map_err(|e| e.to_string())
                .and_then(|draft| self.commit_editor_draft(draft, None, false))
            {
                Ok(()) => self.notify_message(self.message("formatter_applied", &[]), false),
                Err(error) => self.show_error(error),
            },
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn drain_linter_sources(&mut self) {
        self.tasks.retire_linter_sources(
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            &self.diagnostics,
        );
        for key in self.tasks.linter_cancellations() {
            if let Some(cancel) = self.documents.cancel_linter_read(&key) {
                self.tasks.linter_cancelled(cancel);
            }
        }
        for _ in 0..64 {
            let Some(loan) = self.tasks.linter_read(
                &self.state,
                &self.workspace,
                self.configuration.binding_revision_ref(),
                &self.diagnostics,
            ) else {
                break;
            };
            // Full capacity preserves the unread path for retry. Permanent failure retires
            // only this build; already-admitted reads stay owned until a proven terminal.
            if !matches!(self.documents.submit_linter_read(loan), Ok(true)) {
                break;
            }
        }
        while let Some(update) = self.tasks.next_linter_update(
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            &self.diagnostics,
        ) {
            match update {
                crate::editor::session::tasks::LinterUpdate::Publish { producer, batches } => {
                    if let Err(error) =
                        self.diagnostics
                            .replace_source(&producer, batches, &self.state)
                    {
                        self.show_error(error);
                    }
                    self.sync_ui_diagnostics();
                }
                crate::editor::session::tasks::LinterUpdate::Failed(error) => {
                    self.show_error(error)
                }
            }
        }
    }

    pub(super) fn show_task_picker(&mut self) {
        let items = self
            .tasks
            .task_items()
            .into_iter()
            .map(|item| (item, PickerPayload::Command(String::new())))
            .collect();
        self.install_picker(self.message("picker_tasks_title", &[]), items);
    }

    pub(super) fn show_output_picker(&mut self, name: &str) {
        let previous = if self.tasks.active_output() == Some(name) {
            self.search.picker_bookmark()
        } else {
            None
        };
        let follow_tail = previous.is_none() || self.search.picker_follows_tail();
        self.tasks.select_output(Some(name));
        let items = self
            .tasks
            .output_items(name)
            .into_iter()
            .map(|item| (item, PickerPayload::Command(String::new())))
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Output,
            self.message("picker_output_title", &[("name", name.into())]),
            items,
        );
        self.search.restore_picker_view(previous, follow_tail);
        self.sync_picker_ui();
    }

    pub(super) fn show_test_picker(&mut self, name: &str) {
        let items = self
            .tasks
            .test_items(name)
            .into_iter()
            .map(|(item, key)| (item, PickerPayload::TaskTest(key)))
            .collect();
        self.install_picker(
            self.message("picker_tests_title", &[("name", name.into())]),
            items,
        );
    }

    pub(super) fn process_job_events(&mut self) {
        self.drain_job_events(None);
    }
    pub(super) fn drain_job_events(&mut self, deadline: Option<Instant>) {
        for _ in 0..256 {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            let Some(update) = self
                .tasks
                .next_update(&self.workspace, self.configuration.binding_revision())
            else {
                break;
            };
            match update {
                crate::editor::session::tasks::TaskUpdate::Output => {
                    if let Some(name) = self.tasks.active_output().map(str::to_owned)
                        && self.search.picker_surface() == PickerSurfaceKind::Output
                    {
                        self.show_output_picker(&name);
                    }
                }
                crate::editor::session::tasks::TaskUpdate::Finished {
                    task,
                    code,
                    truncated,
                    next,
                } => {
                    if !self.tasks.current(
                        &task,
                        &self.workspace,
                        self.configuration.binding_revision(),
                    ) {
                        continue;
                    }
                    if next {
                        self.start_task_step(task.requested().clone());
                    } else {
                        if code == Some(0) && !truncated {
                            self.apply_formatter_result(&task);
                        } else if truncated
                            && matches!(task.adapter(), AdapterContext::Formatter { .. })
                        {
                            self.show_error(self.message("formatter_truncated", &[]));
                        }
                        if !matches!(
                            task.adapter(),
                            AdapterContext::Formatter { .. } | AdapterContext::Linter { .. }
                        ) || code != Some(0)
                        {
                            self.notify_message(
                                self.message(
                                    "task_exited",
                                    &[
                                        ("name", task.name().clone()),
                                        (
                                            "code",
                                            code.map_or_else(|| "signal".into(), |v| v.to_string()),
                                        ),
                                    ],
                                ),
                                code != Some(0),
                            );
                        }
                        if truncated && matches!(task.adapter(), AdapterContext::Linter { .. }) {
                            self.show_error("linter output truncated; diagnostics not published");
                        }
                        if code == Some(0)
                            && !truncated
                            && matches!(task.adapter(), AdapterContext::Linter { .. })
                        {
                            if let Err(error) = self.tasks.begin_linter(
                                *task,
                                &self.state,
                                &self.workspace,
                                self.configuration.binding_revision(),
                                &self.diagnostics,
                                (
                                    self.configuration.settings().workspace.maximum_diagnostics,
                                    self.configuration
                                        .settings()
                                        .workspace
                                        .search
                                        .maximum_file_bytes,
                                ),
                            ) {
                                self.show_error(error);
                            }
                        }
                    }
                }
                crate::editor::session::tasks::TaskUpdate::Failed { source, message } => {
                    if let Some(source) = source {
                        self.diagnostics.clear_source(&source);
                        self.sync_ui_diagnostics();
                    }
                    self.show_error(message);
                }
            }
        }
        if deadline.is_none_or(|deadline| Instant::now() < deadline) {
            self.drain_linter_sources();
        }
    }
}
