use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::app::AdapterContext;
use crate::app::BTreeMap;
use crate::app::DiagnosticOwner;
use crate::app::Instant;
use crate::app::App;
use crate::app::PickerPayload;
use crate::app::PickerSurfaceKind;
use crate::app::RunningTask;
use crate::app::managed_formatter;



impl App {

    pub(in crate::app) fn rebuild_matcher_cache(&mut self) {
        self.tasks.configure(self.configuration.settings());
    }
}

impl App {

    pub(in crate::app) fn stop_task(&mut self, name: &str) {
        match self.tasks.stop(name) {
            Ok(source) => {
                if let Some(source) = source {
                    self.diagnostics.clear_source(&source);
                    self.sync_ui_diagnostics();
                }
                self.notify_message(
                    self.message("task_stopped", &[("name", name.into())]),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn start_task_plan(&mut self, name: &str, adapter: AdapterContext) {
        let managed = match &adapter {
            AdapterContext::Formatter { source }
                if !self.configuration.settings().tasks.contains_key(name) =>
            {
                let path = self.state.buffers[source.buffer()]
                    .path
                    .clone()
                    .unwrap_or_default();
                let Some(tool) = managed_formatter(|tool| tool.id == name, &path) else {
                    self.show_error(self.message("formatter_missing", &[("name", name.into())]));
                    return;
                };
                let mut config = self.configuration.settings().clone();
                config.tasks.insert(
                    name.into(),
                    crate::config::TaskConfig {
                        kind: crate::config::TaskKind::Formatter,
                        executable: tool.executable.clone(),
                        arguments: Vec::new(),
                        cwd: None,
                        environment: BTreeMap::new(),
                        dependencies: Vec::new(),
                        timeout_ms: Some(10_000),
                        stdin: true,
                        matcher: None,
                    },
                );
                Some((config, tool))
            }
            _ => None,
        };
        match self.tasks.plan(
            name,
            &adapter,
            managed
                .as_ref()
                .map_or(self.configuration.settings(), |(config, _)| config),
            &self.workspace,
            self.configuration.binding_revision(),
            managed.as_ref().map(|(_, tool)| tool.clone()),
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
}

impl App {

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
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
}

impl App {


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
                Ok(()) => {
                    self.notify_message(self.message("formatter_applied", &[]), false);
                }
                Err(error) => self.show_error(error),
            },
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
}

impl App {


    pub(in crate::app) fn drain_linter_sources(&mut self) {
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
                crate::app::tasks::LinterUpdate::Publish { producer, batches } => {
                    if let Err(error) =
                        self.diagnostics
                            .replace_source(&producer, batches, &self.state)
                    {
                        self.show_error(error);
                    }
                    self.sync_ui_diagnostics();
                }
                crate::app::tasks::LinterUpdate::Failed(error) => {
                    self.show_error(error)
                }
            }
        }
    }
}

impl App {


    pub(in crate::app) fn show_task_picker(&mut self) {
        let items = self
            .tasks
            .task_items()
            .into_iter()
            .map(|item| (item, PickerPayload::Command(String::new())))
            .collect();
        self.install_picker(self.message("picker_tasks_title", &[]), items);
    }
}

impl App {


    pub(in crate::app) fn show_output_picker(&mut self, name: &str) {
        let previous = if self.tasks.active_output() == Some(name)
            && self.search.picker_surface() == PickerSurfaceKind::Output
        {
            self.search.picker_bookmark()
        } else {
            None
        };
        let normal = previous.is_some() && self.input.picker_normal();
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
        self.input.set_picker_normal(normal);
        self.sync_picker_ui();
    }
}

impl App {


    pub(in crate::app) fn show_test_picker(&mut self, name: &str) {
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
}

impl App {


    pub(in crate::app) fn process_job_events(&mut self) {
        self.drain_job_events(None);
    }
}

impl App {

    pub(in crate::app) fn drain_job_events(&mut self, deadline: Option<Instant>) {
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
                crate::app::tasks::TaskUpdate::Output => {
                    if let Some(name) = self.tasks.active_output().map(str::to_owned)
                        && self.search.picker_surface() == PickerSurfaceKind::Output
                    {
                        self.show_output_picker(&name);
                    }
                }
                crate::app::tasks::TaskUpdate::Finished {
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
                    let next_step = next.then(|| task.requested().clone());
                    if !next {
                        if code == Some(0) && !truncated {
                            self.apply_formatter_result(&task);
                        } else if truncated
                            && matches!(task.adapter(), AdapterContext::Formatter { .. })
                        {
                            self.show_error(self.message("formatter_truncated", &[]));
                        }
                        // Whatever the formatter did, the save it was holding back goes on.
                        if let AdapterContext::Formatter { source } = task.adapter()
                            && self
                                .format_on_save
                                .get(&source.buffer())
                                .is_some_and(|pending| {
                                    pending.formatter.as_ref() == Some(task.requested())
                                })
                        {
                            self.finish_format_on_save(source.buffer());
                        }
                        let linter_ran =
                            matches!(task.adapter(), AdapterContext::Linter { lint: true, .. })
                                && matches!(code, Some(0 | 1));
                        if !linter_ran
                            && (!matches!(task.adapter(), AdapterContext::Formatter { .. })
                                || code != Some(0))
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
                    }
                    // Interpret dependency output too, before advancing to the next step.
                    let (matched, lint) = match task.adapter() {
                        AdapterContext::Linter { lint, .. } => (true, *lint),
                        _ => (false, false),
                    };
                    let linter_ran = lint && matches!(code, Some(0 | 1));
                    if truncated && matches!(task.adapter(), AdapterContext::Linter { .. }) {
                        self.show_error("linter output truncated; diagnostics not published");
                    }
                    if matched
                        && (linter_ran || !lint)
                        && !truncated
                        && let Err(error) = self.tasks.begin_linter(
                            *task,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                            &self.diagnostics,
                            (MAXIMUM_DIAGNOSTICS, SEARCH_MAXIMUM_FILE_BYTES as u64),
                        )
                    {
                        self.show_error(error);
                    }
                    if let Some(name) = next_step {
                        self.start_task_step(name);
                    }
                }
                crate::app::tasks::TaskUpdate::Failed { source, message } => {
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

impl App {

    pub(in crate::app) fn project_operation_bar(&mut self) {
        let Some(status) = self.git.status() else {
            self.ui.operation_bar = None;
            return;
        };
        let repository = status.repository.clone();
        let state = match crate::git::read_conflict_state(&repository) {
            Ok(state) => state,
            Err(error) => {
                self.ui.operation_bar = Some(crate::ui::OperationBarState {
                    operation: "Git".into(),
                    phase: "operation state unavailable".into(),
                    blockers: Vec::new(),
                    actions: Vec::new(),
                    abort_binding: None,
                });
                self.show_error(error);
                return;
            }
        };
        self.ui.operation_bar = crate::git::GitOperationBar::of(&state).map(|bar| {
            crate::ui::OperationBarState {
                operation: bar.operation,
                phase: bar.phase,
                blockers: bar.blockers,
                actions: bar.actions.iter().map(|a| a.name().to_owned()).collect(),
                abort_binding: self
                    .configuration
                    .keymaps()
                    .binding_for("editor.normal", "workspace.git.conflict.abort")
                    .map(crate::app::picker::palette::readable_keys),
            }
        });
    }
}
