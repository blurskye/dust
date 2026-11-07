//! Task planning, accepted run ownership, bounded output and source projections.
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
mod linter;
mod output;
mod sources;

/// Limits for every task and tool process Dust runs.
pub(in crate::app) const PROCESS_EVENT_CAPACITY: usize = 256;
const MAXIMUM_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_LINE_BYTES: usize = 1024 * 1024;
const MAXIMUM_STDIN_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_TIMEOUT_MS: u64 = 300_000;
const MAXIMUM_RUNNING: usize = 16;
use crate::config::Config;
use crate::config::TaskConfig;
use crate::editor::BufferId;
use crate::editor::EditorOperation;
use crate::editor::EditorState;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::TransactionId;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextEdit;
use crate::editor::text::TextEditSet;
use crate::process::ExecutionPreflightRequest;
use crate::process::ExecutionPreflightResult;
use crate::process::JobEvent;
use crate::process::JobId;
use crate::process::JobPolicy;
use crate::process::JobRequest;
use crate::process::JobStream;
use crate::process::JobSupervisor;
use crate::workspace::DiagnosticProducer;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
pub(super) use linter::LinterReadLoan;
pub(super) use linter::LinterUpdate;
pub(super) use sources::FormatterSource;
pub(super) use sources::TaskDiagnosticRun;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
#[derive(Clone, Debug)]
pub(in crate::app) enum AdapterContext {
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
pub(in crate::app) struct TaskOutputRecord {
    source: Option<crate::app::picker::MatcherSource>,
    pub(in crate::app) stream: JobStream,
    pub(in crate::app) text: String,
    pub(in crate::app) truncated: bool,
}
pub(in crate::app) struct RunningTask {
    output: Vec<u8>,
    cancelled: bool,
    key: RunKey,
    diagnostic_source: Option<TaskDiagnosticRun>,
    requested: String,
    name: String,
    adapter: AdapterContext,
    root: PathBuf,
    cwd: PathBuf,
}

impl RunningTask {
    pub(in crate::app) fn requested(&self) -> &String {
        &self.requested
    }
    pub(in crate::app) fn name(&self) -> &String {
        &self.name
    }
    pub(in crate::app) fn adapter(&self) -> &AdapterContext {
        &self.adapter
    }
}
#[derive(Clone, Debug)]
pub(in crate::app) struct TestCaseResult {
    pub(in crate::app) name: String,
    pub(in crate::app) status: String,
    pub(in crate::app) duration: String,
    pub(in crate::app) message: String,
    pub(in crate::app) source: Option<crate::app::picker::MatcherSource>,
}

pub(in crate::app) fn task_plan(
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
    formatter: Option<crate::tools::ManagedFormatter>,
    adapter: AdapterContext,
    awaiting: bool,
}
pub(super) struct TaskStep {
    request: ExecutionPreflightRequest,
    requested: String,
    name: String,
    task: TaskConfig,
    formatter: Option<crate::tools::ManagedFormatter>,
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
pub(super) struct Tasks {
    linters: linter::Linters,
    jobs: JobSupervisor,
    preflights: HashMap<crate::process::PreflightId, TaskStep>,
    running: HashMap<JobId, RunningTask>,
    keys: HashMap<String, RunKey>,
    plans: HashMap<String, Plan>,
    adapters: HashMap<String, AdapterContext>,
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
impl Tasks {
    pub(in crate::app) fn new(config: &Config) -> Self {
        let mut owner = Self {
            linters: linter::Linters::default(),
            jobs: JobSupervisor::new(PROCESS_EVENT_CAPACITY),
            preflights: HashMap::new(),
            running: HashMap::new(),
            keys: HashMap::new(),
            plans: HashMap::new(),
            adapters: HashMap::new(),
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
    pub(in crate::app) fn configure(&mut self, config: &Config) {
        self.adapters.clear();
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
            .invalidate("task configuration or workspace changed");
        for status in self.statuses.values_mut() {
            if status == "queued"
                || status.starts_with("starting:")
                || status.starts_with("running:")
            {
                *status = "cancelled".into();
            }
        }
        self.keys.clear();
        self.plans.clear();
        self.preflights.clear();
        for (id, task) in &mut self.running {
            task.cancelled = true;
            let _ = self.jobs.cancel(*id);
        }
    }
    pub(in crate::app) fn plan(
        &mut self,
        name: &str,
        adapter: &AdapterContext,
        config: &Config,
        workspace: &Workspace,
        configuration: u64,
        formatter: Option<crate::tools::ManagedFormatter>,
    ) -> Result<(), String> {
        workspace
            .execution_allowed()
            .map_err(|_| "tasks run only in a trusted workspace".to_owned())?;
        self.plan_checked(name, adapter, config, workspace, configuration)?;
        self.plans
            .get_mut(name)
            .expect("accepted task plan")
            .formatter = formatter;
        Ok(())
    }
    pub(in crate::app) fn plan_checked(
        &mut self,
        name: &str,
        adapter: &AdapterContext,
        config: &Config,
        workspace: &Workspace,
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
            return Err("task reservation is closed, full or already pending".into());
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
                formatter: None,
                adapter: adapter.clone(),
                awaiting: false,
            },
        );
        self.adapters.insert(name.into(), adapter.clone());
        self.rows.insert(name.into(), Vec::new());
        self.tests.remove(name);
        self.statuses.insert(name.into(), "queued".into());
        Ok(())
    }
    pub(in crate::app) fn last_adapter(&self, name: &str) -> Option<&AdapterContext> {
        self.adapters.get(name)
    }
    pub(in crate::app) fn step(
        &mut self,
        requested: &str,
        workspace: &Workspace,
        configuration: u64,
    ) -> Result<(TaskStep, ExecutionPreflightRequest), String> {
        let key = self
            .keys
            .get(requested)
            .filter(|key| {
                key.workspace == workspace.version() && key.configuration == configuration
            })
            .ok_or("task plan is stale")?;
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
                formatter: if name == requested {
                    plan.formatter.take()
                } else {
                    None
                },
                name,
                request: request.clone(),
                task,
                adapter,
                key: key.clone(),
            },
            request,
        ))
    }
    pub(in crate::app) fn preflight_count(&self) -> usize {
        self.preflights.len()
    }
    pub(in crate::app) fn take_preflight(
        &mut self,
        id: crate::process::PreflightId,
    ) -> Option<TaskStep> {
        self.preflights.remove(&id)
    }
    pub(in crate::app) fn queue_preflight(
        &mut self,
        step: TaskStep,
        mut lane: crate::app::workspace::ExecutionLane<'_>,
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
                return Err("task preflight source is stale or full".into());
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
    pub(in crate::app) fn reject_step(&mut self, step: &TaskStep) {
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
    pub(in crate::app) fn launch(
        &mut self,
        step: TaskStep,
        ready: crate::app::workspace::ExecutionReady,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
    ) -> Result<Option<DiagnosticProducer>, String> {
        if !self.key_current(&step.requested, &step.key, workspace, configuration) {
            return Err("task launch is stale".into());
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
    pub(in crate::app) fn launch_inner(
        &mut self,
        step: TaskStep,
        result: ExecutionPreflightResult,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
    ) -> Result<Option<DiagnosticProducer>, String> {
        if !self.key_current(&step.requested, &step.key, workspace, configuration)
            || result.root_revision != workspace.root_revision()
            || self.running.len() >= MAXIMUM_RUNNING
        {
            return Err("task launch source is stale or full".into());
        }
        let root = workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == result.root)
            .ok_or("task preflight root is stale")?;
        // Reservation lends the actual supervisor before snapshots or request serialization.
        let reservation = self.jobs.reserve().map_err(|e| e.to_string())?;
        let stdin = match &step.adapter {
            AdapterContext::Formatter { source } if step.name == step.requested => {
                Some(source.stdin(editor, MAXIMUM_STDIN_BYTES)?)
            }
            _ => None,
        };
        let diagnostic_source = if matches!(step.adapter, AdapterContext::Linter { .. }) {
            // Each dependency owns its findings independently of later steps in the plan.
            let diagnostic_name = if step.name == step.requested {
                step.requested.clone()
            } else {
                format!("{}:{}", step.requested, step.name)
            };
            Some(TaskDiagnosticRun::capture(
                &diagnostic_name,
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
            arguments: step.formatter.as_ref().map_or_else(
                || step.task.arguments.into_iter().map(Into::into).collect(),
                |formatter| formatter.arguments.clone(),
            ),
            cwd: result.cwd,
            // A task runs with what the user's shell gave Dust, as a terminal does, and its own
            // settings win.
            environment: std::env::vars_os()
                .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
                .chain(
                    step.task
                        .environment
                        .into_iter()
                        .map(|(key, value)| (key, value.into())),
                )
                .chain(
                    step.formatter
                        .into_iter()
                        .flat_map(|formatter| formatter.environment),
                )
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
        let id = reservation
            .submit_scoped(request, root.identity.clone())
            .map_err(|e| e.to_string())?;
        self.statuses
            .insert(step.requested.clone(), format!("starting:{}", step.name));
        self.running.insert(
            id,
            RunningTask {
                output: Vec::new(),
                cancelled: false,
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
        workspace: &Workspace,
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
    pub(in crate::app) fn current(
        &self,
        task: &RunningTask,
        workspace: &Workspace,
        configuration: u64,
    ) -> bool {
        self.key_current(&task.requested, &task.key, workspace, configuration)
    }
    pub(in crate::app) fn running_count(&self) -> usize {
        self.running.len()
    }
    pub(in crate::app) fn status_count(&self) -> usize {
        self.statuses.len()
    }
    pub(in crate::app) fn names(&self) -> impl Iterator<Item = &String> {
        self.statuses.keys()
    }
    pub(in crate::app) fn active_output(&self) -> Option<&str> {
        self.active_output.as_deref()
    }
    pub(in crate::app) fn select_output(&mut self, name: Option<&str>) {
        self.active_output = name.map(str::to_owned);
    }
    pub(in crate::app) fn launch_pending(&self, name: &str) -> bool {
        self.plans.contains_key(name) || self.preflights.values().any(|step| step.requested == name)
    }
    pub(in crate::app) fn is_running(&self, name: &str) -> bool {
        self.launch_pending(name) || self.running.values().any(|task| task.requested == name)
    }
    pub(in crate::app) fn stop_formatter(&mut self, name: &str, buffer: BufferId) {
        if matches!(self.adapters.get(name), Some(AdapterContext::Formatter { source }) if source.buffer() == buffer)
        {
            // stop invalidates the run key as well as cancelling the process, so already
            // buffered results cannot apply after a formatting-save timeout.
            let _ = self.stop(name);
        }
    }
    pub(in crate::app) fn stop(
        &mut self,
        name: &str,
    ) -> Result<Option<DiagnosticProducer>, String> {
        let pending = self.launch_pending(name);
        let current = self.keys.get(name).cloned();
        let mut producer = current.as_ref().and_then(|key| self.linters.stop_run(key));
        let mut found = pending || producer.is_some();
        let running = current.as_ref().is_some_and(|key| {
            self.running.values().any(|task| {
                task.requested == name
                    && !task.cancelled
                    && Arc::ptr_eq(&key.epoch, &task.key.epoch)
            })
        });
        if !found && !running {
            return Err("task is not running".into());
        }
        self.preflights.retain(|_, step| step.requested != name);
        self.keys.remove(name);
        self.plans.remove(name);
        for (id, task) in &mut self.running {
            if task.requested == name {
                match self.jobs.cancel(*id) {
                    Ok(()) | Err(crate::process::JobError::Unknown(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
                found = true;
                task.cancelled = true;
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
    pub(in crate::app) fn cancel_workspace_tasks(&mut self) {
        self.preflights.clear();
        self.linters
            .invalidate("task configuration or workspace changed");
        self.keys.clear();
        self.plans.clear();
        for (id, task) in &mut self.running {
            task.cancelled = true;
            let _ = self.jobs.cancel(*id);
        }
        self.rows.clear();
        self.tests.clear();
        self.statuses.clear();
        self.active_output = None;
    }
    pub(in crate::app) fn begin_shutdown(&mut self) {
        self.preflights.clear();
        self.closed = true;
        self.linters
            .invalidate("task configuration or workspace changed");
        self.keys.clear();
        self.plans.clear();
        for task in self.running.values_mut() {
            task.cancelled = true;
        }
        self.jobs.begin_shutdown();
    }
    pub(in crate::app) fn abandon(&mut self) -> usize {
        let count = self.running.len();
        self.jobs.abandon();
        self.running.clear();
        count
    }
    pub(in crate::app) fn next_update(
        &mut self,
        workspace: &Workspace,
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
                        // Job output events contain complete records, including an unterminated
                        // last record on each stream. Keep those records separate for matchers.
                        if !matches!(task.adapter, AdapterContext::Formatter { .. })
                            && !accepted.is_empty()
                            && !accepted.ends_with(b"\n")
                        {
                            if task.output.len() < self.maximum_output {
                                task.output.push(b'\n');
                            } else {
                                self.truncated.insert(job);
                            }
                        }
                    }
                    let rows = self.rows.entry(task.requested.clone()).or_default();
                    for line in String::from_utf8_lossy(accepted).lines().take(1024) {
                        let text = output::plain_line(line);
                        rows.push(TaskOutputRecord {
                            source: output::source_location(&text, &task.cwd),
                            stream,
                            text,
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
                    let status = if timed_out {
                        "timed out".into()
                    } else {
                        code.map_or_else(
                            || "terminated by signal".into(),
                            |code| format!("exit {code}"),
                        )
                    };
                    self.append_output_status(
                        &task.requested,
                        format!("[{} · {status}]", task.name),
                        truncated,
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
                        if task.cancelled {
                            continue;
                        }
                        return Some(TaskUpdate::Failed {
                            source: None,
                            message: format!("cancelled task {}: {message}", task.requested),
                        });
                    }
                    self.plans.remove(&task.requested);
                    self.statuses
                        .insert(task.requested.clone(), "failed".into());
                    self.append_output_status(
                        &task.requested,
                        format!("[{} · failed: {message}]", task.name),
                        false,
                    );
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
    pub(in crate::app) fn task_items(&self) -> Vec<PickerItem> {
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
    fn append_output_status(&mut self, name: &str, text: String, truncated: bool) {
        let rows = self.rows.entry(name.into()).or_default();
        rows.push(TaskOutputRecord {
            source: None,
            stream: JobStream::Stdout,
            text,
            truncated,
        });
        if rows.len() > 10_000 {
            rows.drain(..rows.len() - 10_000);
        }
    }
    pub(in crate::app) fn output_items(&self, name: &str) -> Vec<(PickerItem, PickerPayload)> {
        self.rows
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
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
                    },
                    row.source.clone().map_or_else(
                        || PickerPayload::Notice {
                            title: name.to_owned(),
                            text: row.text.clone(),
                        },
                        PickerPayload::MatcherSource,
                    ),
                )
            })
            .collect()
    }
    pub(in crate::app) fn test_items(&self, name: &str) -> Vec<(PickerItem, TestKey)> {
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
    pub(in crate::app) fn failed_tests(&self) -> Vec<(PathBuf, usize, String, TestKey)> {
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
    pub(in crate::app) fn resolve_test(
        &self,
        key: &TestKey,
    ) -> Option<crate::app::picker::MatcherSource> {
        let current = self.keys.get(&key.name)?;
        if !Arc::ptr_eq(&current.epoch, &key.run) {
            return None;
        }
        self.tests.get(&key.name)?.get(key.index)?.source.clone()
    }
    pub(in crate::app) fn formatter_transaction(
        &self,
        task: &RunningTask,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        id: TransactionId,
    ) -> Result<Option<EditorTransaction>, String> {
        let AdapterContext::Formatter { source } = &task.adapter else {
            return Ok(None);
        };
        if !self.current(task, workspace, configuration) {
            return Err("formatter run is stale".into());
        }
        source.check(editor)?;
        let bytes = &task.output;
        let replacement = crate::editor::text::decode_utf8(bytes)
            .map_err(|_| "formatter output is not UTF-8")?
            .text;
        let text = &editor.buffers[source.buffer()].text;
        // Only what the formatter changed is replaced, so the cursor stays on the code it was on.
        let edits = crate::editor::text::diff::changed_spans(&text.to_string(), &replacement)
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
impl Tasks {
    pub(in crate::app) fn fixture_formatter_result(
        &mut self,
        name: &str,
        buffer: crate::editor::BufferId,
        output: &[u8],
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
    ) {
        let key = RunKey {
            epoch: Arc::new(()),
            workspace: workspace.version(),
            configuration,
        };
        self.keys.insert(name.into(), key.clone());
        self.adapters.insert(
            name.into(),
            AdapterContext::Formatter {
                source: FormatterSource::capture(editor, buffer).unwrap(),
            },
        );
        let job = JobId(self.running.len() as u64 + 1000);
        self.running.insert(
            job,
            RunningTask {
                output: output.into(),
                cancelled: false,
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

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS;
use crate::app::picker::PickerPayload;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::tools::managed_formatter;
use crate::ui::PickerSurfaceKind;
use crate::workspace::DiagnosticOwner;
use std::time::Instant;

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
    fn start_task_step(&mut self, requested: String) {
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
    fn apply_formatter_result(&mut self, task: &RunningTask) {
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
        self.tasks.invalidate_linter_sources(
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
            // Full capacity preserves the unread path for retry. Permanent failure discards
            // only this build; already-accepted reads stay owned until a proven terminal.
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
                crate::app::tasks::LinterUpdate::Failed(error) => self.show_error(error),
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
            .map(|item| {
                let payload = PickerPayload::TaskOutput(item.label.clone());
                (item, payload)
            })
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
        let items = self.tasks.output_items(name);
        let title = self.message("picker_output_title", &[("name", name.into())]);
        let title = self
            .tasks
            .statuses
            .get(name)
            .map_or(title.clone(), |status| format!("{title} · {status}"));
        self.install_surface_picker(PickerSurfaceKind::Output, title, items);
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
            if let Some(name) = self.tasks.active_output().map(str::to_owned)
                && self.search.picker_surface() == PickerSurfaceKind::Output
            {
                self.show_output_picker(&name);
            }
            match update {
                crate::app::tasks::TaskUpdate::Output => {}
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
        self.ui.operation_bar =
            crate::git::GitOperationBar::of(&state).map(|bar| crate::ui::OperationBarState {
                operation: bar.operation,
                phase: bar.phase,
                blockers: bar.blockers,
                actions: bar.actions.iter().map(|a| a.name().to_owned()).collect(),
                abort_binding: self
                    .configuration
                    .keymaps()
                    .binding_for("editor.normal", "workspace.git.conflict.abort")
                    .map(crate::app::picker::palette::readable_keys),
            });
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
