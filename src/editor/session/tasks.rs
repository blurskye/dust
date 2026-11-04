//! Task planning, admitted run ownership, bounded output and source projections.
use super::workspace::{WorkspaceSession, WorkspaceVersion};
mod linter;
mod sources;

/// Limits for every task and tool process Dust runs.
pub(crate) const PROCESS_EVENT_CAPACITY: usize = 256;
const MAXIMUM_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_LINE_BYTES: usize = 1024 * 1024;
const MAXIMUM_STDIN_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_TIMEOUT_MS: u64 = 300_000;
const MAXIMUM_RUNNING: usize = 16;
use crate::{
    config::{Config, TaskConfig},
    editor::{
        EditorState, TransactionId,
        text::{ByteOffset, ByteRange, TextEdit, TextEditSet},
        transaction::{EditorOperation, EditorTransaction, Precondition},
    },
    process::{
        ExecutionPreflightRequest, ExecutionPreflightResult, JobEvent, JobId, JobPolicy,
        JobRequest, JobStream, JobSupervisor,
    },
    workspace::{DiagnosticProducer, PickerAction, PickerItem, PickerItemId},
};
pub(super) use linter::{LinterReadLoan, LinterUpdate};
pub(super) use sources::{FormatterSource, TaskDiagnosticRun};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum AdapterContext {
    Plain,
    Formatter {
        source: FormatterSource,
    },
    /// A task whose output a matcher turns into problems. `lint` is a linter's run, where
    /// exit 1 means findings rather than failure; other tasks with a matcher, such as a build,
    /// still fail on any nonzero exit.
    Linter {
        matcher: crate::config::ProblemMatcherConfig,
        lint: bool,
    },
    Test {
        matcher: crate::config::ProblemMatcherConfig,
    },
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TaskOutputRecord {
    pub(in crate::editor::session) stream: JobStream,
    pub(in crate::editor::session) text: String,
    pub(in crate::editor::session) truncated: bool,
}
pub(in crate::editor::session) struct RunningTask {
    output: Vec<u8>,
    key: RunKey,
    diagnostic_source: Option<TaskDiagnosticRun>,
    requested: String,
    name: String,
    adapter: AdapterContext,
    root: PathBuf,
    cwd: PathBuf,
}

impl RunningTask {
    pub(super) fn requested(&self) -> &String {
        &self.requested
    }
    pub(super) fn name(&self) -> &String {
        &self.name
    }
    pub(super) fn adapter(&self) -> &AdapterContext {
        &self.adapter
    }
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TestCaseResult {
    pub(in crate::editor::session) name: String,
    pub(in crate::editor::session) status: String,
    pub(in crate::editor::session) duration: String,
    pub(in crate::editor::session) message: String,
    pub(in crate::editor::session) source: Option<super::search::MatcherSource>,
}

pub(in crate::editor::session) fn task_plan(
    name: &str,
    tasks: &BTreeMap<String, crate::config::TaskConfig>,
) -> Option<Vec<String>> {
    fn visit(
        name: &str,
        tasks: &BTreeMap<String, crate::config::TaskConfig>,
        seen: &mut BTreeSet<String>,
        active: &mut BTreeSet<String>,
        plan: &mut Vec<String>,
    ) -> Option<()> {
        if seen.contains(name) {
            return Some(());
        }
        if active.len() >= 256 || !active.insert(name.into()) {
            return None;
        }
        let task = tasks.get(name)?;
        for dependency in &task.dependencies {
            visit(dependency, tasks, seen, active, plan)?;
        }
        active.remove(name);
        seen.insert(name.into());
        plan.push(name.into());
        Some(())
    }
    let mut seen = BTreeSet::new();
    let mut plan = Vec::new();
    visit(name, tasks, &mut seen, &mut BTreeSet::new(), &mut plan)?;
    Some(plan)
}

#[derive(Clone, Debug)]
struct RunKey {
    epoch: Arc<()>,
    workspace: WorkspaceVersion,
    configuration: u64,
}
#[derive(Clone, Debug)]
pub(super) struct TestKey {
    pub(super) name: String,
    run: Arc<()>,
    pub(super) index: usize,
}
struct Plan {
    remaining: VecDeque<(String, TaskConfig)>,
    adapter: AdapterContext,
    awaiting: bool,
}
pub(super) struct TaskStep {
    request: ExecutionPreflightRequest,
    requested: String,
    name: String,
    task: TaskConfig,
    adapter: AdapterContext,
    key: RunKey,
}
pub(super) enum TaskUpdate {
    Output,
    Finished {
        task: Box<RunningTask>,
        code: Option<i32>,
        truncated: bool,
        next: bool,
    },
    Failed {
        source: Option<DiagnosticProducer>,
        message: String,
    },
}
pub(super) struct TaskSession {
    linters: linter::Linters,
    jobs: JobSupervisor,
    preflights: HashMap<crate::process::PreflightId, TaskStep>,
    running: HashMap<JobId, RunningTask>,
    keys: HashMap<String, RunKey>,
    plans: HashMap<String, Plan>,
    rows: HashMap<String, Vec<TaskOutputRecord>>,
    active_output: Option<String>,
    truncated: HashSet<JobId>,
    statuses: HashMap<String, String>,
    tests: HashMap<String, Vec<TestCaseResult>>,
    matchers: HashMap<String, regex::Regex>,
    events: VecDeque<JobEvent>,
    maximum_output: usize,
    closed: bool,
}
impl TaskSession {
    pub(super) fn new(config: &Config) -> Self {
        let mut owner = Self {
            linters: linter::Linters::default(),
            jobs: JobSupervisor::new(PROCESS_EVENT_CAPACITY),
            preflights: HashMap::new(),
            running: HashMap::new(),
            keys: HashMap::new(),
            plans: HashMap::new(),
            rows: HashMap::new(),
            active_output: None,
            truncated: HashSet::new(),
            statuses: HashMap::new(),
            tests: HashMap::new(),
            matchers: HashMap::new(),
            events: VecDeque::new(),
            maximum_output: 0,
            closed: false,
        };
        owner.configure(config);
        owner
    }
    pub(super) fn configure(&mut self, config: &Config) {
        self.maximum_output = MAXIMUM_OUTPUT_BYTES;
        self.matchers = config
            .tasks
            .iter()
            .filter_map(|(name, task)| {
                Some((
                    name.clone(),
                    regex::Regex::new(&task.matcher.as_ref()?.pattern).ok()?,
                ))
            })
            .collect();
        self.linters
            .retire("task source configuration/workspace retired");
        self.keys.clear();
        self.plans.clear();
        for id in self.running.keys() {
            let _ = self.jobs.cancel(*id);
        }
    }
    pub(super) fn plan(
        &mut self,
        name: &str,
        adapter: &AdapterContext,
        config: &Config,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Result<(), String> {
        workspace
            .execution_allowed()
            .map_err(|_| "tasks run only in a trusted workspace".to_owned())?;
        self.plan_checked(name, adapter, config, workspace, configuration)
    }
    fn plan_checked(
        &mut self,
        name: &str,
        adapter: &AdapterContext,
        config: &Config,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Result<(), String> {
        if self.closed
            || configuration == u64::MAX
            || self.running.len() >= MAXIMUM_RUNNING
            || self.keys.contains_key(name)
                && self.statuses.get(name).is_some_and(|status| {
                    status == "queued"
                        || status.starts_with("running:")
                        || status.starts_with("starting:")
                })
        {
            return Err("task admission is closed, full or already pending".into());
        }
        let plan = task_plan(name, &config.tasks).ok_or("unknown task or dependency")?;
        self.keys.insert(
            name.into(),
            RunKey {
                epoch: Arc::new(()),
                workspace: workspace.version(),
                configuration,
            },
        );
        self.plans.insert(
            name.into(),
            Plan {
                remaining: plan
                    .into_iter()
                    .map(|name| {
                        let task = config.tasks[&name].clone();
                        (name, task)
                    })
                    .collect(),
                adapter: adapter.clone(),
                awaiting: false,
            },
        );
        self.rows.insert(name.into(), Vec::new());
        self.tests.remove(name);
        self.statuses.insert(name.into(), "queued".into());
        Ok(())
    }
    pub(super) fn step(
        &mut self,
        requested: &str,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Result<(TaskStep, ExecutionPreflightRequest), String> {
        let key = self
            .keys
            .get(requested)
            .filter(|key| {
                key.workspace == workspace.version() && key.configuration == configuration
            })
            .ok_or("task plan retired")?;
        let plan = self.plans.get_mut(requested).ok_or("task plan closed")?;
        if self.closed || plan.awaiting {
            return Err("task plan is closed or already awaiting a step".into());
        }
        let (name, task) = plan.remaining.pop_front().ok_or("task plan exhausted")?;
        plan.awaiting = true;
        let adapter = if name == requested {
            plan.adapter.clone()
        } else if let Some(matcher) = &task.matcher {
            AdapterContext::Linter {
                matcher: matcher.clone(),
                lint: false,
            }
        } else if let AdapterContext::Linter { matcher, .. } = &plan.adapter {
            AdapterContext::Linter {
                matcher: matcher.clone(),
                lint: false,
            }
        } else {
            AdapterContext::Plain
        };
        let root = workspace
            .roots()
            .iter()
            .find(|root| Some(root.id) == workspace.active_root())
            .ok_or("task has no active root")?;
        let cwd = task.cwd.clone().map_or_else(
            || root.identity.canonical_path.clone(),
            |p| {
                if p.is_absolute() {
                    p
                } else {
                    root.identity.canonical_path.join(p)
                }
            },
        );
        let request = ExecutionPreflightRequest {
            executable: task.executable.clone(),
            cwd,
            roots: workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: workspace.root_revision(),
        };
        Ok((
            TaskStep {
                requested: requested.into(),
                name,
                request: request.clone(),
                task,
                adapter,
                key: key.clone(),
            },
            request,
        ))
    }
    pub(in crate::editor::session) fn preflight_count(&self) -> usize {
        self.preflights.len()
    }
    pub(in crate::editor::session) fn take_preflight(
        &mut self,
        id: crate::process::PreflightId,
    ) -> Option<TaskStep> {
        self.preflights.remove(&id)
    }
    pub(in crate::editor::session) fn queue_preflight(
        &mut self,
        step: TaskStep,
        mut lane: super::workspace::ExecutionLane<'_>,
    ) -> Result<(), String> {
        let result = (|| {
            if self.closed
                || self.preflights.len() >= 256
                || !lane.current(&step.key.workspace)
                || !self
                    .keys
                    .get(&step.requested)
                    .is_some_and(|key| Arc::ptr_eq(&key.epoch, &step.key.epoch))
                || !lane.preflight_available()?
            {
                return Err("task preflight source retired or full".into());
            }
            lane.preflight(step.request.clone())
                .map_err(|error| error.to_string())
        })();
        match result {
            Ok(id) => {
                self.preflights.insert(id, step);
                Ok(())
            }
            Err(error) => {
                self.reject_step(&step);
                Err(error)
            }
        }
    }
    pub(super) fn reject_step(&mut self, step: &TaskStep) {
        if self
            .keys
            .get(&step.requested)
            .is_some_and(|key| Arc::ptr_eq(&key.epoch, &step.key.epoch))
        {
            self.keys.remove(&step.requested);
            self.plans.remove(&step.requested);
            self.statuses
                .insert(step.requested.clone(), "failed".into());
        }
    }
    pub(super) fn launch(
        &mut self,
        step: TaskStep,
        ready: super::workspace::ExecutionReady,
        editor: &EditorState,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Result<Option<DiagnosticProducer>, String> {
        if !self.key_current(&step.requested, &step.key, workspace, configuration) {
            return Err("task launch retired".into());
        }
        let name = step.requested.clone();
        let result = ready
            .into_execution_result(&step.request, workspace)
            .and_then(|result| self.launch_inner(step, result, editor, workspace, configuration));
        if result.is_err() {
            self.keys.remove(&name);
            self.plans.remove(&name);
            self.statuses.insert(name, "failed".into());
        }
        result
    }
    fn launch_inner(
        &mut self,
        step: TaskStep,
        result: ExecutionPreflightResult,
        editor: &EditorState,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Result<Option<DiagnosticProducer>, String> {
        if !self.key_current(&step.requested, &step.key, workspace, configuration)
            || result.root_revision != workspace.root_revision()
            || self.running.len() >= MAXIMUM_RUNNING
        {
            return Err("task launch source retired or full".into());
        }
        let root = workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == result.root)
            .ok_or("task preflight root retired")?;
        // Reservation lends the actual supervisor before snapshots or request serialization.
        let admission = self.jobs.reserve().map_err(|e| e.to_string())?;
        let stdin = match &step.adapter {
            AdapterContext::Formatter { source } if step.name == step.requested => {
                Some(source.stdin(editor, MAXIMUM_STDIN_BYTES)?)
            }
            _ => None,
        };
        let diagnostic_source = if matches!(step.adapter, AdapterContext::Linter { .. }) {
            Some(TaskDiagnosticRun::capture(
                &step.requested,
                editor,
                workspace,
                configuration,
            )?)
        } else {
            None
        };
        let producer = diagnostic_source
            .as_ref()
            .map(|source| source.producer().clone());
        let cwd = result.cwd.clone();
        let request = JobRequest {
            program: result.executable,
            argv0: Some(step.request.executable.clone()),
            arguments: step.task.arguments,
            cwd: result.cwd,
            // A task runs with what the user's shell gave Dust, as a terminal does, and its own
            // settings win.
            environment: std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .chain(step.task.environment)
                .collect(),
            root_revision: result.root_revision,
            stdin,
            policy: JobPolicy {
                maximum_output_bytes: MAXIMUM_OUTPUT_BYTES,
                maximum_line_bytes: MAXIMUM_LINE_BYTES,
                maximum_stdin_bytes: MAXIMUM_STDIN_BYTES,
                timeout: Duration::from_millis(step.task.timeout_ms.unwrap_or(MAXIMUM_TIMEOUT_MS)),
            },
        };
        let id = admission
            .submit_scoped(request, root.identity.clone())
            .map_err(|e| e.to_string())?;
        self.statuses
            .insert(step.requested.clone(), format!("starting:{}", step.name));
        self.running.insert(
            id,
            RunningTask {
                output: Vec::new(),
                key: step.key,
                requested: step.requested,
                name: step.name,
                adapter: step.adapter,
                root: result.root,
                cwd,
                diagnostic_source,
            },
        );
        Ok(producer)
    }
    fn key_current(
        &self,
        name: &str,
        key: &RunKey,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> bool {
        !self.closed
            && key.workspace == workspace.version()
            && key.configuration == configuration
            && self
                .keys
                .get(name)
                .is_some_and(|current| Arc::ptr_eq(&key.epoch, &current.epoch))
    }
    pub(super) fn current(
        &self,
        task: &RunningTask,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> bool {
        self.key_current(&task.requested, &task.key, workspace, configuration)
    }
    pub(super) fn running_count(&self) -> usize {
        self.running.len()
    }
    pub(super) fn status_count(&self) -> usize {
        self.statuses.len()
    }
    pub(super) fn names(&self) -> impl Iterator<Item = &String> {
        self.statuses.keys()
    }
    pub(super) fn active_output(&self) -> Option<&str> {
        self.active_output.as_deref()
    }
    pub(super) fn select_output(&mut self, name: Option<&str>) {
        self.active_output = name.map(str::to_owned);
    }
    fn launch_pending(&self, name: &str) -> bool {
        self.plans.contains_key(name) || self.preflights.values().any(|step| step.requested == name)
    }
    pub(super) fn is_running(&self, name: &str) -> bool {
        self.launch_pending(name) || self.running.values().any(|task| task.requested == name)
    }
    pub(super) fn stop(&mut self, name: &str) -> Result<Option<DiagnosticProducer>, String> {
        let pending = self.launch_pending(name);
        self.preflights.retain(|_, step| step.requested != name);
        let current = self.keys.remove(name);
        let mut producer = current.as_ref().and_then(|key| self.linters.stop_run(key));
        self.plans.remove(name);
        let mut found = pending || producer.is_some();
        for (id, task) in &self.running {
            if task.requested == name {
                match self.jobs.cancel(*id) {
                    Ok(()) | Err(crate::process::JobError::Unknown(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
                found = true;
                if current
                    .as_ref()
                    .is_some_and(|key| Arc::ptr_eq(&key.epoch, &task.key.epoch))
                {
                    producer = task
                        .diagnostic_source
                        .as_ref()
                        .map(|source| source.producer().clone());
                }
            }
        }
        if found {
            self.statuses.insert(name.into(), "cancelled".into());
            Ok(producer)
        } else {
            Err("task is not running".into())
        }
    }
    pub(super) fn retire_workspace(&mut self) {
        self.preflights.clear();
        self.linters
            .retire("task source configuration/workspace retired");
        self.keys.clear();
        self.plans.clear();
        for id in self.running.keys() {
            let _ = self.jobs.cancel(*id);
        }
        self.rows.clear();
        self.tests.clear();
        self.statuses.clear();
        self.active_output = None;
    }
    pub(super) fn begin_shutdown(&mut self) {
        self.preflights.clear();
        self.closed = true;
        self.linters
            .retire("task source configuration/workspace retired");
        self.keys.clear();
        self.plans.clear();
        self.jobs.begin_shutdown();
    }
    pub(super) fn abandon(&mut self) -> usize {
        let count = self.running.len();
        self.jobs.abandon();
        self.running.clear();
        count
    }
    pub(super) fn next_update(
        &mut self,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) -> Option<TaskUpdate> {
        if self.events.is_empty() {
            self.events.extend(self.jobs.poll());
        }
        while let Some(event) = self.events.pop_front() {
            match event {
                JobEvent::Started { job, root_revision } => {
                    if let Some(task) = self.running.get(&job).filter(|task| {
                        self.current(task, workspace, configuration)
                            && root_revision == workspace.root_revision()
                    }) {
                        self.statuses
                            .insert(task.requested.clone(), format!("running:{}", task.name));
                    }
                }
                JobEvent::Output {
                    job,
                    stream,
                    bytes,
                    truncated,
                } => {
                    if !self
                        .running
                        .get(&job)
                        .is_some_and(|task| self.current(task, workspace, configuration))
                    {
                        continue;
                    }
                    let Some(task) = self.running.get_mut(&job) else {
                        continue;
                    };
                    if truncated {
                        self.truncated.insert(job);
                    }
                    let formatter_stderr = matches!(task.adapter, AdapterContext::Formatter { .. })
                        && stream == JobStream::Stderr;
                    let accepted = &bytes[..bytes
                        .len()
                        .min(self.maximum_output.saturating_sub(task.output.len()))];
                    if accepted.len() != bytes.len() {
                        self.truncated.insert(job);
                    }
                    if !formatter_stderr {
                        task.output.extend_from_slice(accepted);
                    }
                    let rows = self.rows.entry(task.requested.clone()).or_default();
                    for line in String::from_utf8_lossy(accepted).lines().take(1024) {
                        rows.push(TaskOutputRecord {
                            stream,
                            text: line
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(4096)
                                .collect(),
                            truncated,
                        });
                    }
                    if rows.len() > 10_000 {
                        rows.drain(..rows.len() - 10_000);
                    }
                    return Some(TaskUpdate::Output);
                }
                JobEvent::Exited {
                    job,
                    code,
                    timed_out,
                } => {
                    let Some(task) = self.running.remove(&job) else {
                        continue;
                    };
                    let truncated = self.truncated.remove(&job) || timed_out;
                    if !self.current(&task, workspace, configuration) {
                        continue;
                    }
                    // A run with a failing test exits non-zero, and that is the run with results
                    // worth seeing.
                    if !truncated {
                        self.apply_test_result(&task);
                    }
                    let next = if code == Some(0) && !truncated {
                        self.plans.get_mut(&task.requested).is_some_and(|plan| {
                            plan.awaiting = false;
                            !plan.remaining.is_empty()
                        })
                    } else {
                        false
                    };
                    if !next {
                        self.plans.remove(&task.requested);
                    }
                    self.statuses.insert(
                        task.requested.clone(),
                        if next {
                            "queued"
                        } else if !truncated
                            && (code == Some(0)
                                && matches!(task.adapter, AdapterContext::Formatter { .. })
                                || matches!(code, Some(0 | 1))
                                    && matches!(
                                        task.adapter,
                                        AdapterContext::Linter { lint: true, .. }
                                    ))
                        {
                            "process exited"
                        } else if code == Some(0) && !truncated {
                            "succeeded"
                        } else {
                            "failed"
                        }
                        .into(),
                    );
                    return Some(TaskUpdate::Finished {
                        task: Box::new(task),
                        code,
                        truncated,
                        next,
                    });
                }
                JobEvent::Failed { job, message } => {
                    let Some(task) = self.running.remove(&job) else {
                        continue;
                    };
                    self.truncated.remove(&job);
                    if !self.current(&task, workspace, configuration) {
                        // Stopping a task can end its output early; that isn't a failure.
                        if self
                            .statuses
                            .get(&task.requested)
                            .is_some_and(|status| status == "cancelled")
                        {
                            continue;
                        }
                        return Some(TaskUpdate::Failed {
                            source: None,
                            message: format!("retired task {}: {message}", task.requested),
                        });
                    }
                    self.plans.remove(&task.requested);
                    self.statuses
                        .insert(task.requested.clone(), "failed".into());
                    return Some(TaskUpdate::Failed {
                        source: task
                            .diagnostic_source
                            .map(|source| source.producer().clone()),
                        message,
                    });
                }
            }
        }
        None
    }
    pub(super) fn task_items(&self) -> Vec<PickerItem> {
        self.statuses
            .iter()
            .map(|(name, status)| PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: PickerItemId(format!("task:{name}")),
                label: name.clone(),
                detail: status.clone(),
                source: String::new(),
                action: PickerAction::None,
            })
            .collect()
    }
    pub(super) fn output_items(&self, name: &str) -> Vec<PickerItem> {
        self.rows
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, row)| PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: PickerItemId(format!("output:{name}:{index}")),
                label: row.text.clone(),
                detail: format!(
                    "{:?}{}",
                    row.stream,
                    if row.truncated { " · truncated" } else { "" }
                ),
                source: String::new(),
                action: PickerAction::None,
            })
            .collect()
    }
    pub(super) fn test_items(&self, name: &str) -> Vec<(PickerItem, TestKey)> {
        let Some(key) = self.keys.get(name) else {
            return Vec::new();
        };
        self.tests
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, case)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("test:{name}:{index}")),
                        label: case.name.clone(),
                        detail: format!("{} {}", case.status, case.duration),
                        source: case.message.clone(),
                        action: PickerAction::None,
                    },
                    TestKey {
                        name: name.into(),
                        run: key.epoch.clone(),
                        index,
                    },
                )
            })
            .collect()
    }
    /// Failed cases of the current runs that name a source line, for the Problems list.
    pub(super) fn failed_tests(&self) -> Vec<(PathBuf, usize, String, TestKey)> {
        let mut names = self.tests.keys().collect::<Vec<_>>();
        names.sort();
        let mut failed = Vec::new();
        for name in names {
            let Some(key) = self.keys.get(name) else {
                continue;
            };
            for (index, case) in self.tests[name].iter().enumerate() {
                let status = case.status.to_lowercase();
                let Some(source) = &case.source else {
                    continue;
                };
                if status.contains("fail") || status.contains("error") {
                    failed.push((
                        source.path.clone(),
                        source.line,
                        format!("{}: {}", case.name, case.message),
                        TestKey {
                            name: name.clone(),
                            run: key.epoch.clone(),
                            index,
                        },
                    ));
                }
            }
        }
        failed
    }
    pub(super) fn resolve_test(&self, key: &TestKey) -> Option<super::search::MatcherSource> {
        let current = self.keys.get(&key.name)?;
        if !Arc::ptr_eq(&current.epoch, &key.run) {
            return None;
        }
        self.tests.get(&key.name)?.get(key.index)?.source.clone()
    }
    pub(super) fn formatter_transaction(
        &self,
        task: &RunningTask,
        editor: &EditorState,
        workspace: &WorkspaceSession,
        configuration: u64,
        id: TransactionId,
    ) -> Result<Option<EditorTransaction>, String> {
        let AdapterContext::Formatter { source } = &task.adapter else {
            return Ok(None);
        };
        if !self.current(task, workspace, configuration) {
            return Err("formatter run retired".into());
        }
        source.check(editor)?;
        let bytes = &task.output;
        let replacement = crate::editor::text::decode_utf8(bytes)
            .map_err(|_| "formatter output is not UTF-8")?
            .text;
        let text = &editor.buffers[source.buffer()].text;
        // Only what the formatter changed is replaced, so the cursor stays on the code it was on.
        let edits = crate::editor::transaction::changed_spans(&text.to_string(), &replacement)
            .into_iter()
            .map(|(range, inserted)| {
                ByteRange::new(ByteOffset(range.start), ByteOffset(range.end))
                    .map(|range| TextEdit::new(range, inserted))
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(
            EditorTransaction::new(id)
                .with_precondition(Precondition::BufferRevision {
                    buffer: source.buffer(),
                    revision: text.revision(),
                })
                .with_precondition(Precondition::BufferWritable(source.buffer()))
                .with_operation(EditorOperation::ApplyEdits {
                    buffer: source.buffer(),
                    edits: TextEditSet::new(text.revision(), edits),
                }),
        ))
    }
}
#[cfg(test)]
impl TaskSession {
    pub(super) fn fixture_formatter_result(
        &mut self,
        name: &str,
        buffer: crate::editor::BufferId,
        output: &[u8],
        editor: &EditorState,
        workspace: &WorkspaceSession,
        configuration: u64,
    ) {
        let key = RunKey {
            epoch: Arc::new(()),
            workspace: workspace.version(),
            configuration,
        };
        self.keys.insert(name.into(), key.clone());
        let job = JobId(self.running.len() as u64 + 1000);
        self.running.insert(
            job,
            RunningTask {
                output: output.into(),
                key,
                diagnostic_source: None,
                requested: name.into(),
                name: name.into(),
                adapter: AdapterContext::Formatter {
                    source: FormatterSource::capture(editor, buffer).unwrap(),
                },
                root: editor.workspace_root.clone(),
                cwd: editor.workspace_root.clone(),
            },
        );
        self.events.push_back(JobEvent::Exited {
            job,
            code: Some(0),
            timed_out: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::text::TextBuffer,
        workspace::{RootIdentity, WorkspaceId, WorkspaceState},
    };
    #[test]
    fn formatter_stderr_is_visible_without_entering_the_replacement_payload() {
        let root = tempfile::tempdir().unwrap();
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(scope, root.path().join("trust"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor.open_text(TextBuffer::from_text("old"), None);
        let mut tasks = TaskSession::new(&Config::default());
        tasks.fixture_formatter_result("format", buffer, b"formatted", &editor, &workspace, 1);
        tasks.events.push_front(JobEvent::Output {
            job: JobId(1000),
            stream: JobStream::Stderr,
            bytes: b"syntax error at line 4\n".to_vec(),
            truncated: false,
        });
        assert!(matches!(
            tasks.next_update(&workspace, 1),
            Some(TaskUpdate::Output)
        ));
        assert!(
            tasks.output_items("format")[0]
                .label
                .contains("syntax error")
        );
        let Some(TaskUpdate::Finished { task, .. }) = tasks.next_update(&workspace, 1) else {
            panic!("completion missing")
        };
        assert_eq!(task.output, b"formatted");
    }
    #[test]
    fn stopping_with_a_buffered_terminal_event_still_finalizes_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(scope, root.path().join("trust"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor.open_text(TextBuffer::from_text("text"), None);
        let mut tasks = TaskSession::new(&Config::default());
        tasks.fixture_formatter_result("format", buffer, b"text", &editor, &workspace, 1);
        assert!(tasks.stop("format").is_ok());
        assert_eq!(tasks.statuses["format"], "cancelled");
        assert!(tasks.next_update(&workspace, 1).is_none());
    }
    #[test]
    fn identity_formatter_normalizes_bom_and_line_endings() {
        let root = tempfile::tempdir().unwrap();
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(scope, root.path().join("trust"), 1, false, false, 1);
        for bytes in [b"a\r\n".as_slice(), b"\xef\xbb\xbfa\r\n".as_slice()] {
            let mut editor = EditorState::new(root.path().into());
            let (buffer, _) = editor.open_text(TextBuffer::from_bytes(bytes).unwrap(), None);
            let source = FormatterSource::capture(&editor, buffer).unwrap();
            let mut owner = TaskSession::new(&Config::default());
            let key = RunKey {
                epoch: Arc::new(()),
                workspace: workspace.version(),
                configuration: 1,
            };
            owner.keys.insert("format".into(), key.clone());
            let task = RunningTask {
                output: bytes.to_vec(),
                key,
                diagnostic_source: None,
                requested: "format".into(),
                name: "format".into(),
                adapter: AdapterContext::Formatter { source },
                root: root.path().into(),
                cwd: root.path().into(),
            };
            let transaction = owner
                .formatter_transaction(&task, &editor, &workspace, 1, TransactionId(1))
                .unwrap()
                .unwrap();
            editor.apply_transaction(&transaction).unwrap();
            assert_eq!(editor.buffers[buffer].text.to_bytes(), bytes);
            assert_eq!(editor.buffers[buffer].text.to_string(), "a\n");
        }
    }
    #[test]
    fn task_owner_rejects_retired_plans_and_formatter_incarnations_before_materialization() {
        let root = tempfile::tempdir().unwrap();
        let scope = WorkspaceState::new(
            WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = WorkspaceSession::new(scope, root.path().join("trust"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor.open_text(
            TextBuffer::from_text("abc"),
            Some(root.path().join("source")),
        );
        let mut config = Config::default();
        config.tasks.insert(
            "task".into(),
            TaskConfig {
                kind: crate::config::TaskKind::Task,
                executable: PathBuf::from("/bin/sh"),
                arguments: vec![],
                cwd: None,
                environment: BTreeMap::new(),
                dependencies: vec![],
                timeout_ms: None,
                stdin: false,
                matcher: None,
            },
        );
        let mut owner = TaskSession::new(&config);
        owner.configure(&config);
        owner
            .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
            .unwrap();
        assert!(
            owner
                .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
                .is_err()
        );
        let (step, _) = owner.step("task", &workspace, 1).unwrap();
        assert!(owner.step("task", &workspace, 1).is_err());
        owner.retire_workspace();
        let before = TextBuffer::test_materializations();
        assert!(
            owner
                .launch(
                    step,
                    super::super::workspace::ExecutionReady::fixture(
                        &workspace,
                        ExecutionPreflightResult {
                            id: crate::process::PreflightId(1),
                            executable: PathBuf::from("/bin/sh"),
                            cwd: root.path().into(),
                            root: root.path().into(),
                            root_revision: workspace.root_revision()
                        }
                    ),
                    &editor,
                    &workspace,
                    1
                )
                .is_err()
        );
        assert_eq!(TextBuffer::test_materializations(), before);
        let source = FormatterSource::capture(&editor, buffer).unwrap();
        editor.buffers[buffer].text = TextBuffer::from_text("abc");
        assert!(source.stdin(&editor, 1024).is_err());
        assert_eq!(TextBuffer::test_materializations(), before);
        owner
            .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
            .unwrap();
        let (step, _) = owner.step("task", &workspace, 1).unwrap();
        let key = TestKey {
            name: "task".into(),
            run: step.key.epoch.clone(),
            index: 0,
        };
        owner.tests.insert(
            "task".into(),
            vec![TestCaseResult {
                name: "one".into(),
                status: "ok".into(),
                duration: String::new(),
                message: String::new(),
                source: Some(super::super::search::MatcherSource {
                    path: root.path().join("source"), line: 0, column: 0,
                    encoding: crate::editor::text::PositionEncoding::Utf8,
                }),
            }],
        );
        assert!(owner.resolve_test(&key).is_some());
        owner.retire_workspace();
        assert!(owner.resolve_test(&key).is_none());
    }
}
