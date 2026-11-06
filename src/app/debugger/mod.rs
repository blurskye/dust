//! Private debugger runtime, controls, breakpoint/watch policy and sourced selections.
mod launch;
use crate::app::windows::NavigationStamp;
use crate::app::workspace::WorkspaceSession;
use crate::app::workspace::WorkspaceVersion;
use crate::config::DebugConfig;
use crate::config::TerminalProfileConfig;
use crate::config::TerminalsConfig;
use crate::debugger::DapContext;
use crate::debugger::DapControl;
use crate::debugger::DapError;
use crate::debugger::DapEvent;
use crate::debugger::DapReplyOutcome;
use crate::debugger::DapReplyReceipt;
use crate::debugger::DapResponse;
use crate::debugger::DapService;
use crate::debugger::DapSourceBreakpoint;
use crate::debugger::DapStackFrame;
use crate::debugger::DapTerminalReply;
use crate::persistence::PersistedBreakpoint;
use crate::persistence::PersistedWatch;
use crate::process::PtyId;

pub(crate) const MAXIMUM_BREAKPOINTS: usize = 10_000;
pub(crate) const MAXIMUM_WATCHES: usize = 1_000;
pub(crate) const MAXIMUM_DEBUG_ARGUMENTS: usize = 4096;
pub(crate) const MAXIMUM_DEBUG_ENVIRONMENT: usize = 4096;

pub(in crate::app) use launch::DebugIntent;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(in crate::app) struct DebugKey {
    epoch: Arc<()>,
    index: usize,
}
pub(in crate::app) enum DebugResponse {
    Picker {
        kind: &'static str,
        rows: Vec<DebugRow>,
    },
    Result(DapResponse),
    Retired,
    /// Part of following a stop: the panes show it, nothing opens.
    Absorbed,
    /// The program stopped here, so the editor goes there.
    StoppedAt(DebugLocation),
    StoppedSource(DapStackFrame),
}
/// What is being fetched on its own after the program stops, so the source, stack and variables
/// are there without asking. Pickers remain for what the user asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopFollow {
    Idle,
    Threads(Option<i64>),
    Stack,
    Scopes,
    Variables,
}
pub(in crate::app) struct DebugRow {
    pub label: String,
    pub detail: String,
    pub key: DebugKey,
}
enum Choice {
    Control(DapControl),
    Frame(DapStackFrame),
    /// A variable with no children: choosing it shows its value.
    Value(String),
}
pub(in crate::app) struct DebugSelection {
    expansion: Vec<i64>,
    source: DapContext,
    choice: Choice,
}
impl DebugSelection {
    pub(in crate::app) fn value(&self) -> Option<&str> {
        match &self.choice {
            Choice::Value(value) => Some(value),
            _ => None,
        }
    }
}
struct Choices {
    epoch: Arc<()>,
    source: DapContext,
    values: Vec<Choice>,
}
pub(in crate::app) struct SourceTarget {
    pub navigation: NavigationStamp,
    pub capture: Arc<()>,
    pub line: u64,
    pub column: u64,
}
struct Source {
    context: DapContext,
    target: SourceTarget,
}
struct PendingRequest {
    epoch: u64,
    submission: Option<u64>,
    frame: Option<i64>,
    automatic: bool,
}
pub(in crate::app) struct DebugLocation {
    pub path: PathBuf,
    pub line: u64,
    pub column: u64,
}
struct TerminalPolicy {
    name: String,
    profile: TerminalProfileConfig,
    cwd: PathBuf,
}
/// How many variables one page asks for: expansion is bounded rather than eager.
const VARIABLE_PAGE: u64 = 200;
/// How deep the inspector expands before it refuses, so a pathological chain cannot recurse away.
const MAX_EXPANSION_DEPTH: usize = 32;
/// WHAT a session is attached to or running. An attach must PRESERVE the target's identity: the
/// editor names the target it was given, keeps no program for it because it did not start that
/// process, and never presents an attached target as one it launched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct DapTargetIdentity {
    pub configuration: Option<String>,
    pub adapter: String,
    pub request: crate::config::DebugRequestKind,
    /// The program a launch runs. An ATTACH has none.
    pub program: Option<std::path::PathBuf>,
    /// Automatic source selection must keep targeting the same package on restart.
    pub automatic_program: bool,
    pub document: Option<std::path::PathBuf>,
    /// The target an ATTACH names, taken from the configuration's own arguments.
    pub target: Option<String>,
}
pub(super) struct DebugSession {
    service: DapService,
    launch: Option<launch::Launch>,
    console: VecDeque<crate::debugger::DapConsoleEntry>,
    breakpoints: BTreeMap<PathBuf, Vec<DapSourceBreakpoint>>,
    /// Settings changed while persistence is loading must win over the saved values.
    changed_breakpoints: Option<std::collections::BTreeSet<(PathBuf, u64)>>,
    changed_watches: Option<std::collections::BTreeSet<String>>,
    /// What is TRUE about each requested breakpoint, keyed by path and line. A request the
    /// adapter has not answered stays unverified; only a real acknowledgement verifies one.
    breakpoint_states: BTreeMap<(PathBuf, u64), crate::debugger::DapBreakpointState>,
    latest_breakpoints: BTreeMap<PathBuf, u64>,
    breakpoint_refresh: std::collections::BTreeSet<PathBuf>,
    /// The visible console row that an unterminated output chunk continues.
    output_line: Option<usize>,
    /// Breakpoints the USER disabled. This is session state rather than a flag a caller passes,
    /// so it survives an adapter answer.
    disabled_breakpoints: std::collections::BTreeSet<(PathBuf, u64)>,
    stop_follow: StopFollow,
    follow_request: Option<u64>,
    answer_request: Option<u64>,
    /// Why the program is paused, while it is.
    stop_reason: Option<String>,
    /// Where the program is paused, while it is.
    execution: Option<(PathBuf, u64)>,
    variables: Vec<String>,
    watch_values: BTreeMap<String, String>,
    /// One entry per evaluation in flight, in order: the watch it answers, or none for the console.
    evaluations: VecDeque<Option<String>>,
    /// How the session ended, when it has: leaving is not killing.
    end_state: Option<crate::debugger::DapSessionEnd>,
    /// Which STOP the session is currently in, incremented whenever execution runs on or stops
    /// again. Data captured at one stop is stamped with it, so a value read at an earlier stop
    /// can never be presented as current at a later one.
    stop_epoch: u64,
    /// The stop at which each stopped-data request was ISSUED. An answer whose request is known to
    /// belong to a DIFFERENT stop belongs to that stop, and is refused rather than presented as
    /// current. A request with no record is presented as before: the session cannot prove it
    /// belongs to another stop, so it does not pretend to know.
    issued_stack: VecDeque<u64>,
    requests: HashMap<u64, PendingRequest>,
    watch_refresh: VecDeque<String>,
    issued_scopes: VecDeque<(i64, u64)>,
    /// Pages of variables asked for and not yet answered, oldest first: the protocol's variables
    /// answer carries no request id, so a page is matched to its answer in the order it was sent.
    pending_variable_pages: VecDeque<(i64, u64, u64)>,
    /// What the adapter last reported about the process: threads, and the STACK of the current
    /// stop. Both are stopped data, so both go when execution runs on.
    threads: Vec<String>,
    stopped_thread: Option<i64>,
    stack: Vec<String>,
    /// The references OPEN on the expansion path, innermost last. A reference already on this path
    /// is shown as a reference and is never expanded again.
    expansion: Vec<i64>,
    /// The adapter the session is talking to, remembered from its events so a console record can
    /// name the actor that produced it.
    actor: Option<String>,
    /// The frame the console is BOUND to: the frame the user selected from the stack.
    console_frame: Option<i64>,
    /// Identity for each console submission, so a late answer updates ITS OWN record.
    next_submission: u64,
    /// Submissions still waiting for an answer: (id, stop, frame, actor) captured at submission.
    pending_submissions: VecDeque<(u64, u64, Option<i64>, Option<String>)>,
    /// Requests awaiting an answer, oldest first. A `setBreakpoints` response carries no request
    /// sequence, so answers are matched to requests in the order they were sent.
    watches: Vec<String>,
    configuration: Option<String>,
    /// What this session is attached to or running, RETAINED across a successful start so an
    /// attached target's identity is not lost the moment the request goes out.
    target: Option<DapTargetIdentity>,
    terminal_profile: Option<TerminalPolicy>,
    /// Started from the active file, with an adapter from the tool catalog. Such a session has no
    /// configuration naming a terminal, so the adapter's program runs in one as it asks.
    adapter_runs_terminal: bool,
    terminal_replies: HashMap<PtyId, DapTerminalReply>,
    reply_receipts: Vec<(String, i64, DapReplyReceipt)>,
    policy: DebugConfig,
    scope: WorkspaceVersion,
    choices: Option<Choices>,
    event_source: Option<DapContext>,
    source: Option<Source>,
}
impl DebugSession {
    pub(in crate::app) fn new(policy: &DebugConfig, scope: WorkspaceVersion) -> Self {
        Self {
            service: DapService::new(),
            launch: None,
            console: VecDeque::new(),
            breakpoints: BTreeMap::new(),
            changed_breakpoints: Default::default(),
            changed_watches: Default::default(),
            breakpoint_states: BTreeMap::new(),
            latest_breakpoints: BTreeMap::new(),
            breakpoint_refresh: std::collections::BTreeSet::new(),
            output_line: None,
            end_state: None,
            stop_epoch: 0,
            issued_stack: VecDeque::new(),
            requests: HashMap::new(),
            watch_refresh: VecDeque::new(),
            issued_scopes: VecDeque::new(),
            pending_variable_pages: VecDeque::new(),
            threads: Vec::new(),
            stopped_thread: None,
            stack: Vec::new(),
            expansion: Vec::new(),
            actor: None,
            console_frame: None,
            next_submission: 0,
            pending_submissions: VecDeque::new(),
            disabled_breakpoints: std::collections::BTreeSet::new(),
            stop_follow: StopFollow::Idle,
            follow_request: None,
            answer_request: None,
            stop_reason: None,
            execution: None,
            variables: Vec::new(),
            watch_values: BTreeMap::new(),
            evaluations: VecDeque::new(),
            watches: Vec::new(),
            configuration: None,
            target: None,
            terminal_profile: None,
            adapter_runs_terminal: false,
            terminal_replies: HashMap::new(),
            reply_receipts: Vec::new(),
            policy: policy.clone(),
            scope,
            choices: None,
            event_source: None,
            source: None,
        }
    }
    pub(in crate::app) fn refresh(
        &mut self,
        policy: &DebugConfig,
        scope: WorkspaceVersion,
    ) {
        if self.policy != *policy || self.scope != scope {
            let _ = self.stop();
            self.policy = policy.clone();
            self.scope = scope;
            self.watches.truncate(MAXIMUM_WATCHES);
        }
    }
    pub(in crate::app) fn stop(&mut self) -> Result<(), DapError> {
        let pending_launch = self.launch.take().is_some();
        self.latest_breakpoints.clear();
        self.breakpoint_refresh.clear();
        self.output_line = None;
        self.clear_paused_state();
        self.stop_reason = None;
        self.choices = None;
        self.event_source = None;
        self.source = None;
        self.reject_terminals("requesting debugger retired");
        self.terminal_profile = None;
        self.adapter_runs_terminal = false;
        match self.service.stop() {
            Err(DapError::NotRunning) if pending_launch => Ok(()),
            result => result,
        }
    }
    pub(in crate::app) fn reset(&mut self) {
        let _ = self.stop();
        self.event_source = None;
        self.console.clear();
        self.configuration = None;
        self.breakpoints.clear();
        self.breakpoint_states.clear();
        self.disabled_breakpoints.clear();
        self.watches.clear();
    }
    pub(in crate::app) fn begin_shutdown(&mut self) {
        let _ = self.stop();
        self.service.begin_shutdown();
    }
    pub(in crate::app) fn pending(&self) -> usize {
        self.service.pending() + self.reply_receipts.len()
    }
    pub(in crate::app) fn is_running(&self) -> bool {
        self.service.is_running()
    }
    pub(in crate::app) fn active_adapter(&self) -> Option<&str> {
        self.service.active_adapter()
    }
    pub(in crate::app) fn configuration(&self) -> Option<&str> {
        self.configuration.as_deref()
    }
    /// The terminal for a program an auto-started session's adapter asks to run, so that the
    /// program can read what the user types.
    pub(in crate::app) fn adapter_terminal(
        &self,
        cwd: &std::path::Path,
        arguments: &[String],
        environment: &HashMap<String, String>,
    ) -> Option<TerminalProfileConfig> {
        if !self.adapter_runs_terminal {
            return None;
        }
        let (program, arguments) = arguments.split_first()?;
        Some(TerminalProfileConfig {
            program: program.into(),
            arguments: arguments.to_vec(),
            cwd: (!cwd.as_os_str().is_empty()).then(|| cwd.into()),
            environment: environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            columns: 80,
            rows: 24,
            float: false,
            close_on_exit: false,
        })
    }
    pub(in crate::app) fn terminal_request(
        &self,
        cwd: &std::path::Path,
        arguments: &[String],
        environment: &HashMap<String, String>,
        policy: &TerminalsConfig,
    ) -> Result<&str, String> {
        let original = self
            .terminal_profile
            .as_ref()
            .ok_or("runInTerminal is not enabled by the original debug configuration")?;
        if !self
            .service
            .context()
            .is_some_and(|c| self.service.current(&c))
            || policy.profiles.get(&original.name) != Some(&original.profile)
        {
            return Err("original debug terminal profile retired".into());
        }
        if cwd != original.cwd
            || arguments.first().map(String::as_str) != original.profile.program.to_str()
            || arguments.get(1..) != Some(original.profile.arguments.as_slice())
            || environment.len() != original.profile.environment.len()
            || original
                .profile
                .environment
                .iter()
                .any(|(k, v)| environment.get(k) != Some(v))
        {
            return Err(
                "runInTerminal request differs from the original configured terminal profile"
                    .into(),
            );
        }
        Ok(&original.name)
    }
    pub(in crate::app) fn reply_failures(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        self.reply_receipts.retain(|(adapter, sequence, receipt)| match receipt.outcome() {
            DapReplyOutcome::Written => false,
            DapReplyOutcome::Failed | DapReplyOutcome::Unknown => { errors.push(format!("DAP {adapter} reverse request {sequence}: {:?}; no peer acknowledgement claimed", receipt.outcome())); false },
            _ => true,
        });
        errors
    }
    pub(in crate::app) fn control(
        &mut self,
        control: DapControl,
    ) -> Result<(), DapError> {
        self.control_request(control).map(|_| ())
    }
    fn control_request(&mut self, control: DapControl) -> Result<Option<u64>, DapError> {
        // Source requests require a private selection and an original activation target.
        if matches!(control, DapControl::Source { .. }) {
            return Err(DapError::Retired);
        }
        let epoch = self.stop_epoch;
        // A reference ALREADY OPEN on this path is shown as a reference and never expanded again.
        // The refusal happens HERE, before anything is sent, so no answer can materialise it
        // recursively. A later page of the node already open is not a re-entry: it continues what
        // is already displayed.
        if let DapControl::Variables {
            reference,
            start,
            count,
        } = &control
        {
            let (reference, start, count) = (*reference, *start, *count);
            let open = self.expansion.contains(&reference);
            let continuation = start > 0 && self.expansion.last() == Some(&reference);
            if !continuation && (open || self.expansion.len() >= MAX_EXPANSION_DEPTH) {
                self.note_refused_expansion(reference);
                return Ok(None);
            }
            let id = self.send_control(control)?;
            if !continuation {
                self.expansion.push(reference);
            }
            self.pending_variable_pages
                .push_back((reference, start, count));
            return Ok(Some(id));
        }
        let evaluation = match &control {
            DapControl::Evaluate {
                expression,
                context,
                ..
            } => Some((context == "watch").then(|| expression.clone())),
            _ => None,
        };
        let stack = matches!(&control, DapControl::StackTrace { .. });
        let scopes = match &control {
            DapControl::Scopes { frame_id } => Some(*frame_id),
            _ => None,
        };
        let id = self.send_control(control)?;
        self.evaluations.extend(evaluation);
        // Recorded only once the request is actually away, so a refused send leaves nothing that a
        // later answer could be matched against.
        self.note_issued(stack, scopes, epoch);
        Ok(Some(id))
    }
    fn send_control(&mut self, control: DapControl) -> Result<u64, DapError> {
        let repl = matches!(&control, DapControl::Evaluate { context, .. } if context != "watch");
        let breakpoint_path = match &control {
            DapControl::SetBreakpoints { path, .. } => Some(path.clone()),
            _ => None,
        };
        let result = self
            .service
            .context()
            .ok_or(DapError::NotRunning)
            .and_then(|context| self.service.control_tracked(&context, control));
        match result {
            Ok(id) => {
                if let Some(path) = breakpoint_path {
                    self.latest_breakpoints.insert(path, id);
                }
                let submission = repl
                    .then(|| self.pending_submissions.back().map(|p| p.0))
                    .flatten();
                self.requests.insert(
                    id,
                    PendingRequest {
                        epoch: self.stop_epoch,
                        submission,
                        frame: self.console_frame,
                        automatic: false,
                    },
                );
                Ok(id)
            }
            Err(error) => {
                if repl {
                    self.pending_submissions.pop_back();
                }
                Err(error)
            }
        }
    }
    pub(in crate::app) fn prepare_answer(&mut self, id: u64, request: &DapControl, failed: bool) {
        let pending = self.requests.remove(&id);
        let epoch = pending
            .as_ref()
            .map_or(self.stop_epoch, |pending| pending.epoch);
        let submission = pending.and_then(|pending| pending.submission);
        match request {
            DapControl::Evaluate {
                expression,
                context,
                ..
            } => {
                let watch = (context == "watch").then(|| expression.clone());
                if let Some(position) = self.evaluations.iter().position(|old| old == &watch) {
                    self.evaluations.remove(position);
                }
                self.evaluations.push_front(watch);
                if let Some(submission) = submission
                    && let Some(position) = self
                        .pending_submissions
                        .iter()
                        .position(|p| p.0 == submission)
                    && let Some(pending) = self.pending_submissions.remove(position)
                    && !failed
                {
                    self.pending_submissions.push_front(pending);
                }
            }
            DapControl::StackTrace { .. } => {
                self.issued_stack.clear();
                if !failed {
                    self.issued_stack.push_back(epoch);
                }
            }
            DapControl::Scopes { frame_id } => {
                self.issued_scopes.clear();
                if !failed {
                    self.issued_scopes.push_back((*frame_id, epoch));
                }
            }
            DapControl::Variables {
                reference,
                start,
                count,
            } => {
                self.pending_variable_pages.clear();
                if !failed {
                    self.pending_variable_pages
                        .push_back((*reference, *start, *count));
                }
            }
            _ => {}
        }
    }
    /// The stop a stopped-data request belongs to, recorded at issue time.
    pub(in crate::app) fn note_issued(
        &mut self,
        stack: bool,
        scopes: Option<i64>,
        epoch: u64,
    ) {
        if stack {
            self.issued_stack.push_back(epoch);
        }
        if let Some(frame) = scopes {
            self.issued_scopes.push_back((frame, epoch));
        }
    }
    /// Whether an answer issued at `epoch` may be presented now: only if it belongs to THIS stop.
    fn is_current_stop_answer(issued: Option<u64>, epoch: u64) -> bool {
        issued.is_none_or(|issued| issued == epoch)
    }
    /// A refused EXPANSION is visible too: the reference stays a reference.
    fn note_refused_expansion(&mut self, reference: i64) {
        self.push_console(crate::debugger::DapConsoleEntry::new(
            crate::debugger::DapConsoleOrigin::Notice,
            "dust",
            format!("reference {reference} is already open on this path; it is not expanded again"),
        ));
    }
    /// A refused answer is visible rather than silent: it is a notice, not a value.
    fn note_refused_answer(&mut self) {
        self.push_console(crate::debugger::DapConsoleEntry::new(
            crate::debugger::DapConsoleOrigin::Notice,
            "dust",
            "a debugger answer from an earlier stop was refused; it is not current",
        ));
    }
    pub(in crate::app) fn try_event(&mut self) -> Option<DapEvent> {
        self.try_event_once()
    }
    fn try_event_once(&mut self) -> Option<DapEvent> {
        self.refresh_breakpoints();
        self.refresh_watches();
        let (context, event) = loop {
            let (context, event) = self.service.try_owned_event()?;
            let answer = match &event {
                DapEvent::Response {
                    request_id,
                    request: Some(request),
                    ..
                }
                | DapEvent::RequestFailed {
                    request_id,
                    request: Some(request),
                    ..
                } => Some((*request_id, request)),
                _ => None,
            };
            if let Some((id, request)) = answer {
                let frame_dependent = matches!(
                    request,
                    DapControl::Scopes { .. } | DapControl::Variables { .. }
                ) || matches!(request, DapControl::Evaluate { context, .. } if context == "watch");
                // The transport owns the initial automatic threads request (ID zero);
                // subsequent follow requests are recorded when we send them.
                let retired_threads = id == 0
                    && matches!(request, DapControl::Threads)
                    && self.follow_request != Some(0);
                if retired_threads
                    || self.requests.get(&id).is_some_and(|pending| {
                        (pending.automatic && self.follow_request != Some(id))
                            || (frame_dependent
                                && (pending.epoch != self.stop_epoch
                                    || pending.frame != self.console_frame))
                    })
                {
                    self.requests.remove(&id);
                    if let DapControl::Evaluate {
                        expression,
                        context,
                        ..
                    } = request
                    {
                        let watch = (context == "watch").then(|| expression.clone());
                        if let Some(position) =
                            self.evaluations.iter().position(|old| old == &watch)
                        {
                            self.evaluations.remove(position);
                        }
                    }
                    continue;
                }
            }
            break (context, event);
        };
        self.answer_request = match &event {
            DapEvent::Response { request_id, .. } => Some(*request_id),
            _ => None,
        };
        match &event {
            DapEvent::Response {
                request_id,
                request: Some(request),
                ..
            } => self.prepare_answer(*request_id, request, false),
            DapEvent::RequestFailed {
                request_id,
                request: Some(request),
                ..
            } => self.prepare_answer(*request_id, request, true),
            _ => {}
        }
        match &event {
            DapEvent::Response {
                request: Some(DapControl::SetBreakpoints { path, breakpoints }),
                request_id,
                response,
                ..
            } if !self.breakpoint_refresh.contains(path)
                && self.latest_breakpoints.get(path).copied().unwrap_or(0) == *request_id =>
            {
                self.apply_breakpoints(path, breakpoints, response);
            }
            DapEvent::RequestFailed {
                request: Some(DapControl::SetBreakpoints { path, breakpoints }),
                request_id,
                ..
            } if !self.breakpoint_refresh.contains(path)
                && self.latest_breakpoints.get(path).copied().unwrap_or(0) == *request_id =>
            {
                self.apply_breakpoints(
                    path,
                    breakpoints,
                    &DapResponse::Breakpoints(vec![(false, None, None); breakpoints.len()]),
                );
            }
            DapEvent::Response {
                response: DapResponse::Disconnected { terminate_debuggee },
                ..
            } => {
                self.end_state = Some(if *terminate_debuggee {
                    crate::debugger::DapSessionEnd::Terminated
                } else {
                    crate::debugger::DapSessionEnd::Detached
                });
                let _ = self.stop();
            }
            DapEvent::Response {
                response:
                    DapResponse::BreakpointChanged {
                        path,
                        requested,
                        verified,
                        line,
                        removed,
                    },
                ..
            } => {
                let key = (path.clone(), *requested);
                if !self.disabled_breakpoints.contains(&key)
                    && self
                        .breakpoints
                        .get(path)
                        .is_some_and(|rows| rows.iter().any(|row| row.line == *requested))
                {
                    if *removed {
                        self.breakpoint_states.remove(&key);
                    } else {
                        self.breakpoint_states.insert(
                            key,
                            if *verified {
                                crate::debugger::DapBreakpointState::Verified {
                                    line: line.unwrap_or(*requested),
                                }
                            } else {
                                crate::debugger::DapBreakpointState::Requested { accepted: false }
                            },
                        );
                    }
                }
            }
            _ => {}
        }
        self.event_source = Some(context);
        // Remember WHICH adapter is talking, so a record created later (by a submission) can still
        // name the actor that produced it.
        if let Some(actor) = match &event {
            DapEvent::Response { adapter, .. }
            | DapEvent::Output { adapter, .. }
            | DapEvent::Stopped { adapter, .. }
            | DapEvent::Running { adapter, .. }
            | DapEvent::RequestFailed { adapter, .. }
            | DapEvent::RunInTerminal { adapter, .. }
            | DapEvent::Failed { adapter, .. } => Some(adapter.clone()),
            _ => None,
        } {
            self.actor = Some(actor);
        }
        if self
            .source
            .as_ref()
            .is_some_and(|s| !self.service.current(&s.context))
        {
            self.source = None;
        }
        match &event {
            DapEvent::RunInTerminal {
                adapter,
                request_seq,
                reply,
                ..
            } => {
                // At most 8 resident actors x 32 original reverse obligations.
                if self.reply_receipts.len() >= 256 {
                    let _ =
                        reply.respond(false, Some("debugger reply receipt admission full".into()));
                    let _ = self.stop();
                    return Some(DapEvent::Failed {
                        adapter: adapter.clone(),
                        message: "debugger reply receipt admission full; write outcome unknown"
                            .into(),
                    });
                }
                self.reply_receipts
                    .push((adapter.clone(), *request_seq, reply.receipt()));
            }
            DapEvent::RequestFailed { command, .. } if command == "source" => {
                self.source = None;
            }
            DapEvent::RequestFailed { request_id, .. }
                if self.follow_request == Some(*request_id) =>
            {
                self.stop_follow = StopFollow::Idle;
                self.follow_request = None;
            }
            DapEvent::Output { adapter, output } => {
                self.record_program_output(adapter, output);
            }
            DapEvent::Stopped {
                reason, thread_id, ..
            } => {
                // A STOP is a new stop: whatever was captured at the previous one is not current.
                self.begin_stop();
                self.stop_reason = Some(reason.clone());
                self.stop_follow = StopFollow::Threads(*thread_id);
                // The transport issues the initial threads request at each stop with ID zero.
                self.follow_request = Some(0);
            }
            DapEvent::Running { .. } => {
                // Execution is running, so NOTHING captured while stopped is current.
                self.begin_running();
            }
            _ => {}
        }
        Some(event)
    }
    /// Record what the user TYPED and what the adapter ANSWERED, with their own origins, so a
    /// result is never presented as program output and the two can be told apart.
    pub(in crate::app) fn record_console_command(&mut self, text: String) {
        self.next_submission = self.next_submission.wrapping_add(1);
        let submission = self.next_submission;
        // Captured NOW, at submission: this is the stop and frame that produced it, even if the
        // answer arrives after the session has moved on to another stop.
        let stop = self.stop_epoch;
        let frame = self.console_frame;
        let actor = self.actor.clone();
        let mut entry = crate::debugger::DapConsoleEntry::new(
            crate::debugger::DapConsoleOrigin::Command,
            "repl",
            text,
        );
        entry.submission = Some(submission);
        self.push_console(entry);
        self.pending_submissions
            .push_back((submission, stop, frame, actor));
    }
    fn record_program_output(&mut self, adapter: &str, output: &str) {
        for chunk in output.split_inclusive('\n') {
            let line = if let Some(index) = self.output_line {
                index
            } else {
                self.push_console(crate::debugger::DapConsoleEntry::new(
                    crate::debugger::DapConsoleOrigin::ProgramOutput,
                    adapter,
                    String::new(),
                ));
                self.console.len() - 1
            };
            let entry = &mut self.console[line];
            entry
                .text
                .push_str(chunk.strip_suffix('\n').unwrap_or(chunk));
            if chunk.ends_with('\n') {
                if entry.text.ends_with('\r') {
                    entry.text.pop();
                }
                self.output_line = None;
            } else {
                self.output_line = Some(line);
            }
        }
    }
    pub(in crate::app) fn record_inspected_value(&mut self, text: String) {
        self.push_console(crate::debugger::DapConsoleEntry::new(
            crate::debugger::DapConsoleOrigin::Evaluation,
            "inspector",
            text,
        ));
    }
    pub(in crate::app) fn record_console_evaluation(&mut self, text: String) {
        // The answer belongs to the OLDEST submission still waiting for one, so a LATE result
        // updates the record that ASKED for it and never the newer draft typed after it.
        let pending = self.pending_submissions.pop_front();
        let answer = text.clone();
        let mut entry = crate::debugger::DapConsoleEntry::new(
            crate::debugger::DapConsoleOrigin::Evaluation,
            "repl",
            text,
        );
        if let Some((submission, stop, frame, actor)) = pending {
            entry.submission = Some(submission);
            entry.stop = Some(stop);
            entry.frame = frame;
            entry.actor = actor;
            // The submission's OWN record is updated in place, which is what makes it its result.
            if let Some(record) = self
                .console
                .iter_mut()
                .rev()
                .find(|record| record.submission == Some(submission))
            {
                record.result = Some(answer);
            }
        }
        self.push_console(entry);
    }
    fn push_console(&mut self, mut entry: crate::debugger::DapConsoleEntry) {
        // Provenance is stamped HERE, once, for every record: what the record itself supplied is
        // kept, and what is ambient is filled in.
        if entry.stop.is_none() {
            entry.stop = Some(self.stop_epoch);
        }
        if entry.frame.is_none() {
            entry.frame = self.console_frame;
        }
        if entry.actor.is_none() {
            entry.actor = self.actor.clone();
        }
        self.console.push_back(entry);
        while self.console.len() > 10_000 {
            self.console.pop_front();
            self.output_line = self.output_line.and_then(|index| index.checked_sub(1));
        }
    }
    pub(in crate::app) fn console(
        &self,
    ) -> impl Iterator<Item = &crate::debugger::DapConsoleEntry> {
        self.console.iter()
    }
    pub(in crate::app) fn watches(&self) -> &[String] {
        &self.watches
    }
    pub(in crate::app) fn add_watch(&mut self, expression: String) {
        if !expression.is_empty()
            && expression.len() <= 64 * 1024
            && !expression.contains('\0')
            && self.watches.len() < MAXIMUM_WATCHES
            && !self.watches.contains(&expression)
        {
            if let Some(changed) = &mut self.changed_watches {
                changed.insert(expression.clone());
            }
            self.watches.push(expression.clone());
            // Paused, the value can be shown now rather than at the next stop.
            if self.console_frame.is_some() {
                self.watch_refresh.push_back(expression);
                self.refresh_watches();
            }
        }
    }
    pub(in crate::app) fn remove_watch(&mut self, expression: &str) {
        if let Some(changed) = &mut self.changed_watches {
            changed.insert(expression.into());
        }
        self.watches.retain(|watch| watch != expression);
        self.watch_refresh.retain(|watch| watch != expression);
        self.watch_values.remove(expression);
    }
    pub(in crate::app) fn breakpoint_at(
        &self,
        path: &std::path::Path,
        line: u64,
    ) -> Option<&DapSourceBreakpoint> {
        self.breakpoints
            .get(path)?
            .iter()
            .find(|row| row.line == line)
    }

    pub(in crate::app) fn breakpoint(
        &mut self,
        row: DapSourceBreakpoint,
        toggle: bool,
    ) -> Result<bool, String> {
        if row.line == 0
            || !row.path.is_absolute()
            || [
                row.condition.as_ref(),
                row.hit_condition.as_ref(),
                row.log_message.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|s| s.len() > 64 * 1024 || s.contains('\0'))
        {
            return Err("invalid debugger breakpoint".into());
        }
        let count = self.breakpoints.values().map(Vec::len).sum::<usize>();
        let existing = self
            .breakpoints
            .get(&row.path)
            .and_then(|rows| rows.iter().position(|old| old.line == row.line));
        if existing.is_none() && count >= MAXIMUM_BREAKPOINTS {
            return Err("debugger breakpoint limit reached".into());
        }
        if let Some(changed) = &mut self.changed_breakpoints {
            changed.insert((row.path.clone(), row.line));
        }
        let rows = self.breakpoints.entry(row.path.clone()).or_default();
        let removed = toggle && existing.is_some();
        // The line that this call REMOVES, so its verdict is dropped rather than left behind.
        let removed_line = existing
            .filter(|_| toggle)
            .and_then(|index| rows.get(index).map(|old| old.line));
        if let Some(index) = existing {
            if toggle {
                rows.remove(index);
            } else {
                rows[index] = row.clone();
            }
        } else {
            rows.push(row.clone());
        }
        let empty = rows.is_empty();
        // Every request is recorded as a request and queued for its answer, so an adapter's
        // reply is matched to what was asked even when the reply arrives later.
        for sent in rows.iter() {
            self.breakpoint_states.insert(
                (row.path.clone(), sent.line),
                if self
                    .disabled_breakpoints
                    .contains(&(row.path.clone(), sent.line))
                {
                    crate::debugger::DapBreakpointState::Disabled
                } else {
                    crate::debugger::DapBreakpointState::Requested { accepted: true }
                },
            );
        }

        // A breakpoint that was REMOVED carries no verdict any more, so none may linger.
        if let Some(removed) = removed_line {
            self.breakpoint_states.remove(&(row.path.clone(), removed));
            self.disabled_breakpoints
                .remove(&(row.path.clone(), removed));
        }
        if empty {
            self.breakpoint_states
                .retain(|(path, _), _| path != &row.path);
            self.disabled_breakpoints
                .retain(|(path, _)| path != &row.path);
        }
        if empty {
            self.breakpoints.remove(&row.path);
        }
        self.resend_breakpoints(&row.path);
        Ok(removed)
    }
    pub(in crate::app) fn apply_breakpoints(
        &mut self,
        path: &std::path::Path,
        rows: &[DapSourceBreakpoint],
        answer: &DapResponse,
    ) {
        let path = path.to_path_buf();
        let states = crate::debugger::breakpoint_states(rows, &[], answer);
        for (row, state) in rows.iter().zip(states) {
            if self
                .disabled_breakpoints
                .contains(&(path.clone(), row.line))
                || !self
                    .breakpoints
                    .get(&path)
                    .is_some_and(|current| current.iter().any(|r| r == row))
            {
                continue;
            }
            self.breakpoint_states
                .insert((path.clone(), row.line), state);
        }
    }
    pub(in crate::app) fn breakpoint_disabled(
        &self,
        path: &std::path::Path,
        line: u64,
    ) -> Option<bool> {
        self.breakpoints
            .get(path)?
            .iter()
            .any(|row| row.line == line)
            .then(|| {
                self.disabled_breakpoints
                    .contains(&(path.to_path_buf(), line))
            })
    }
    /// Disable or enable a breakpoint. Session state, and independent of the adapter.
    pub(in crate::app) fn set_breakpoint_disabled(
        &mut self,
        path: &std::path::Path,
        line: u64,
        disabled: bool,
    ) {
        if self.breakpoint_disabled(path, line).is_none() {
            return;
        }
        let key = (path.to_path_buf(), line);
        if let Some(changed) = &mut self.changed_breakpoints {
            changed.insert(key.clone());
        }
        if disabled {
            self.disabled_breakpoints.insert(key.clone());
            self.breakpoint_states
                .insert(key, crate::debugger::DapBreakpointState::Disabled);
        } else {
            self.disabled_breakpoints.remove(&key);
            // Re-enabled: it is a request again until the adapter answers for it.
            self.breakpoint_states.insert(
                key,
                crate::debugger::DapBreakpointState::Requested { accepted: true },
            );
        }
        self.resend_breakpoints(path);
    }
    fn resend_breakpoints(&mut self, path: &std::path::Path) {
        if self.service.is_running() {
            self.latest_breakpoints.remove(path);
            self.breakpoint_refresh.insert(path.into());
            self.refresh_breakpoints();
        }
    }
    fn refresh_breakpoints(&mut self) {
        for _ in 0..16 {
            let Some(path) = self.breakpoint_refresh.first().cloned() else {
                break;
            };
            let breakpoints = self
                .breakpoints
                .get(&path)
                .into_iter()
                .flatten()
                .filter(|row| {
                    !self
                        .disabled_breakpoints
                        .contains(&(row.path.clone(), row.line))
                })
                .cloned()
                .collect();
            match self.send_control(DapControl::SetBreakpoints {
                path: path.clone(),
                breakpoints,
            }) {
                Ok(_) => {
                    self.breakpoint_refresh.remove(&path);
                }
                Err(DapError::Full) => break,
                Err(error) => {
                    self.breakpoint_refresh.remove(&path);
                    self.push_console(crate::debugger::DapConsoleEntry::new(
                        crate::debugger::DapConsoleOrigin::Notice,
                        "dust",
                        error.to_string(),
                    ));
                }
            }
        }
    }
    /// TERMINATE: ask for the debuggee to end with the session, and record that we asked.
    pub(in crate::app) fn terminate_debuggee(&mut self) -> Result<(), String> {
        self.end_session(DapControl::Disconnect {
            terminate_debuggee: true,
        })
    }
    /// DETACH: leave the session WITHOUT asking the debuggee to stop. The editor is no longer
    /// attached; whether the program keeps running is the adapter's side of the contract, and this
    /// editor does not claim it killed anything it did not ask to kill.
    pub(in crate::app) fn detach_debuggee(&mut self) -> Result<(), String> {
        self.end_session(DapControl::Disconnect {
            terminate_debuggee: false,
        })
    }
    fn end_session(&mut self, control: DapControl) -> Result<(), String> {
        self.service.control(control).map_err(|e| e.to_string())
    }
    /// The threads the adapter last reported.
    pub(in crate::app) fn threads(&self) -> &[String] {
        &self.threads
    }
    /// The stack of the CURRENT stop, or nothing when execution has run on.
    pub(in crate::app) fn stack(&self) -> &[String] {
        &self.stack
    }
    /// What this session is attached to or running, when it started: the target's identity.
    pub(in crate::app) fn target(&self) -> Option<&DapTargetIdentity> {
        self.target.as_ref()
    }
    /// How this session ended, if it has: `Detached` and `Terminated` are different facts.
    pub(in crate::app) fn end_state(&self) -> Option<crate::debugger::DapSessionEnd> {
        self.end_state
    }
    /// What is true about a breakpoint, as a surface must render it.
    pub(in crate::app) fn breakpoint_state(
        &self,
        path: &std::path::Path,
        line: u64,
    ) -> Option<crate::debugger::DapBreakpointState> {
        self.breakpoint_states
            .get(&(path.to_path_buf(), line))
            .copied()
    }
    /// What the adapter is told as soon as it is ready: a path may hold only disabled rows, which
    /// still clears what the adapter might remember for it.
    pub(in crate::app) fn startup_breakpoints(
        &mut self,
    ) -> Vec<(PathBuf, Vec<DapSourceBreakpoint>)> {
        for (path, rows) in &self.breakpoints {
            for row in rows {
                let key = (path.clone(), row.line);
                self.breakpoint_states.insert(
                    key.clone(),
                    if self.disabled_breakpoints.contains(&key) {
                        crate::debugger::DapBreakpointState::Disabled
                    } else {
                        crate::debugger::DapBreakpointState::Requested { accepted: true }
                    },
                );
            }
        }
        self.breakpoints
            .iter()
            .map(|(path, rows)| {
                let enabled = rows
                    .iter()
                    .filter(|row| {
                        !self
                            .disabled_breakpoints
                            .contains(&(path.clone(), row.line))
                    })
                    .cloned()
                    .collect();
                (path.clone(), enabled)
            })
            .collect()
    }
    pub(in crate::app) fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }
    pub(in crate::app) fn execution(&self) -> Option<&(PathBuf, u64)> {
        self.execution.as_ref()
    }
    pub(in crate::app) fn variables(&self) -> &[String] {
        &self.variables
    }
    pub(in crate::app) fn watch_value(&self, expression: &str) -> Option<&str> {
        self.watch_values.get(expression).map(String::as_str)
    }
    /// The frame an evaluation should run in, which is what makes locals visible to it.
    pub(in crate::app) fn stopped_thread(&self) -> Option<i64> {
        self.stopped_thread
    }
    pub(in crate::app) fn current_frame(&self) -> Option<i64> {
        self.console_frame
    }
    /// A watch that cannot be evaluated here is a normal thing to see in the pane, not an error.
    pub(in crate::app) fn watch_request_failed(&mut self, command: &str) -> bool {
        if command != "evaluate" {
            return false;
        }
        let Some(expression) = self.evaluations.pop_front().flatten() else {
            return false;
        };
        if self.watches.contains(&expression) {
            self.watch_values.insert(expression, "not available".into());
        }
        true
    }
    pub(in crate::app) fn persisted_breakpoints(&self) -> Vec<PersistedBreakpoint> {
        self.breakpoints
            .values()
            .flatten()
            .take(MAXIMUM_BREAKPOINTS)
            .map(|r| PersistedBreakpoint {
                path: r.path.clone(),
                line: r.line,
                column: r.column,
                condition: r.condition.clone(),
                hit_condition: r.hit_condition.clone(),
                log_message: r.log_message.clone(),
            })
            .collect()
    }
    pub(in crate::app) fn persisted_watches(&self) -> Vec<PersistedWatch> {
        self.watches
            .iter()
            .map(|expression| PersistedWatch {
                expression: expression.clone(),
            })
            .collect()
    }
    pub(in crate::app) fn begin_restore(&mut self) {
        self.changed_breakpoints = Some(Default::default());
        self.changed_watches = Some(Default::default());
    }
    pub(in crate::app) fn finish_restore(&mut self) {
        self.changed_breakpoints = None;
        self.changed_watches = None;
    }
    pub(in crate::app) fn restore(
        &mut self,
        rows: Vec<PersistedBreakpoint>,
        watches: Vec<PersistedWatch>,
        workspace: &WorkspaceSession,
    ) {
        let old_paths: Vec<_> = self.breakpoints.keys().cloned().collect();
        let changed_breakpoints = self.changed_breakpoints.take().unwrap_or_default();
        let changed_watches = self.changed_watches.take().unwrap_or_default();
        self.breakpoint_states
            .retain(|key, _| changed_breakpoints.contains(key));
        self.disabled_breakpoints
            .retain(|key| changed_breakpoints.contains(key));
        self.latest_breakpoints.clear();
        self.breakpoints.retain(|path, rows| {
            rows.retain(|r| changed_breakpoints.contains(&(path.clone(), r.line)));
            !rows.is_empty()
        });
        for r in rows.into_iter().take(MAXIMUM_BREAKPOINTS) {
            if changed_breakpoints.contains(&(r.path.clone(), r.line))
                || workspace.require_path(&r.path).is_err()
            {
                continue;
            }
            let _ = self.breakpoint(
                DapSourceBreakpoint {
                    path: r.path,
                    line: r.line,
                    column: r.column,
                    condition: r.condition,
                    hit_condition: r.hit_condition,
                    log_message: r.log_message,
                },
                false,
            );
        }
        for path in old_paths {
            if !self.breakpoints.contains_key(&path) {
                self.resend_breakpoints(&path);
            }
        }
        self.watches.retain(|watch| changed_watches.contains(watch));
        self.watch_values
            .retain(|watch, _| changed_watches.contains(watch));
        self.watch_refresh
            .retain(|watch| changed_watches.contains(watch));
        for watch in watches.into_iter().take(MAXIMUM_WATCHES) {
            if !changed_watches.contains(&watch.expression) {
                self.add_watch(watch.expression);
            }
        }
    }
    pub(in crate::app) fn install(&mut self, values: Vec<(String, String, Choice)>) -> Vec<DebugRow> {
        let source = self.event_source.take().or_else(|| self.service.context());
        let Some(source) = source.filter(|s| self.service.current(s)) else {
            self.choices = None;
            return Vec::new();
        };
        let epoch = Arc::new(());
        let mut choices = Vec::new();
        let rows = values
            .into_iter()
            .take(10_000)
            .enumerate()
            .map(|(index, (label, detail, choice))| {
                choices.push(choice);
                DebugRow {
                    label,
                    detail,
                    key: DebugKey {
                        epoch: epoch.clone(),
                        index,
                    },
                }
            })
            .collect();
        self.choices = Some(Choices {
            epoch,
            source,
            values: choices,
        });
        rows
    }
    pub(in crate::app) fn controls(
        &mut self,
        rows: Vec<(String, String, DapControl)>,
    ) -> Vec<DebugRow> {
        self.install(
            rows.into_iter()
                .map(|(a, b, c)| (a, b, Choice::Control(c)))
                .collect(),
        )
    }
    pub(in crate::app) fn response(&mut self, response: DapResponse) -> DebugResponse {
        if !self
            .event_source
            .as_ref()
            .is_some_and(|s| self.service.current(s))
        {
            return DebugResponse::Retired;
        }
        if matches!(response, DapResponse::Evaluate(_))
            && let Some(expression) = self.evaluations.pop_front().flatten()
        {
            if let DapResponse::Evaluate(value) = response
                && self.watches.contains(&expression)
            {
                self.watch_values.insert(expression, value.value);
            }
            return DebugResponse::Absorbed;
        }
        let response = match self.follow_stop(response) {
            Ok(response) => response,
            Err(followed) => return followed,
        };
        let (kind, rows) = match response {
            DapResponse::Disconnected { .. } | DapResponse::BreakpointChanged { .. } => {
                return DebugResponse::Absorbed;
            }
            DapResponse::Threads(rows) => ("threads", {
                // RETAINED: the left inspector shows the process's threads, not only a picker.
                self.threads = rows
                    .iter()
                    .map(|r| format!("{} ({})", r.name, r.id))
                    .collect();
                self.controls(
                    rows.into_iter()
                        .map(|r| {
                            (
                                r.name,
                                r.id.to_string(),
                                DapControl::StackTrace {
                                    thread_id: r.id,
                                    start: 0,
                                    levels: 256,
                                },
                            )
                        })
                        .collect(),
                )
            }),
            DapResponse::Stack(rows) => {
                let issued = self.issued_stack.pop_front();
                if Self::is_current_stop_answer(issued, self.stop_epoch) {
                    // RETAINED only here: an answer belonging to an earlier stop retains nothing.
                    self.stack = rows
                        .iter()
                        .map(|f| format!("{}:{}", f.name, f.line))
                        .collect();
                    ("frames", self.frames(rows))
                } else {
                    self.note_refused_answer();
                    ("frames", Vec::new())
                }
            }
            DapResponse::Scopes(_rows)
                if !Self::is_current_stop_answer(
                    self.issued_scopes.pop_front().map(|(_, epoch)| epoch),
                    self.stop_epoch,
                ) =>
            {
                self.note_refused_answer();
                ("scopes", Vec::new())
            }
            DapResponse::Scopes(rows) => {
                // Scopes are the ROOT of expansion: a fresh list starts a fresh path.
                self.expansion.clear();
                self.pending_variable_pages.clear();
                (
                    "scopes",
                    self.controls(
                        rows.into_iter()
                            .map(|r| {
                                (
                                    r.name,
                                    if r.expensive {
                                        "expensive".into()
                                    } else {
                                        String::new()
                                    },
                                    DapControl::Variables {
                                        reference: r.variables_reference,
                                        start: 0,
                                        count: VARIABLE_PAGE,
                                    },
                                )
                            })
                            .collect(),
                    ),
                )
            }
            DapResponse::Variables(rows) => ("variables", {
                // The transport retires responses from earlier stops.
                let page = self.pending_variable_pages.pop_front();
                let current = rows;
                let open: Vec<i64> = self.expansion.clone();
                let shown = current.len();
                let mut values: Vec<(String, String, Choice)> = current
                    .into_iter()
                    .map(|r| {
                        // A reference already OPEN on this path is shown AS a reference: the
                        // detail says so, and expanding it is refused.
                        let detail = if r.variables_reference != 0
                            && open.contains(&r.variables_reference)
                        {
                            format!(
                                "{} (already open, not expanded)",
                                r.type_name.unwrap_or_default()
                            )
                        } else {
                            r.type_name.unwrap_or_default()
                        };
                        let label = format!("{} = {}", r.name, r.value);
                        let choice = if r.variables_reference == 0 {
                            Choice::Value(label.clone())
                        } else {
                            Choice::Control(DapControl::Variables {
                                reference: r.variables_reference,
                                start: 0,
                                count: VARIABLE_PAGE,
                            })
                        };
                        (label, detail, choice)
                    })
                    .collect();
                // A FULL page means more may follow, so the continuation carries the EXACT
                // count shown and asks for the next page by OFFSET rather than guessing.
                if let Some((reference, start, count)) = page
                    && count > 0
                    && shown as u64 >= count
                {
                    values.push((
                        format!("load more ({shown} shown)"),
                        format!("continue from {}", start + shown as u64),
                        Choice::Control(DapControl::Variables {
                            reference,
                            start: start + shown as u64,
                            count: VARIABLE_PAGE,
                        }),
                    ));
                }
                self.install(values)
            }),
            value => return DebugResponse::Result(value),
        };
        DebugResponse::Picker { kind, rows }
    }
    /// Take the next step of following a stop, or hand back a response that is not part of it.
    fn follow_stop(&mut self, response: DapResponse) -> Result<DapResponse, DebugResponse> {
        if self.follow_request != self.answer_request {
            return Ok(response);
        }
        match (self.stop_follow, response) {
            (StopFollow::Threads(stopped), DapResponse::Threads(rows)) => {
                self.threads = rows
                    .iter()
                    .map(|r| format!("{} ({})", r.name, r.id))
                    .collect();
                self.stop_follow = StopFollow::Idle;
                self.stopped_thread = stopped.or(rows.first().map(|r| r.id));
                if let Some(thread_id) = self.stopped_thread {
                    self.stop_follow = StopFollow::Stack;
                    self.follow_control(DapControl::StackTrace {
                        thread_id,
                        start: 0,
                        levels: 256,
                    });
                }
                Err(DebugResponse::Absorbed)
            }
            (StopFollow::Stack, DapResponse::Stack(rows)) => {
                self.stop_follow = StopFollow::Idle;
                let issued = self.issued_stack.pop_front();
                if !Self::is_current_stop_answer(issued, self.stop_epoch) {
                    self.note_refused_answer();
                    return Err(DebugResponse::Absorbed);
                }
                self.stack = rows
                    .iter()
                    .map(|f| format!("{}:{}", f.name, f.line))
                    .collect();
                let Some(top) = rows.into_iter().next() else {
                    return Err(DebugResponse::Absorbed);
                };
                self.console_frame = Some(top.id);
                self.stop_follow = StopFollow::Scopes;
                self.follow_control(DapControl::Scopes { frame_id: top.id });
                self.watch_refresh = self.watches.iter().cloned().collect();
                self.refresh_watches();
                if top.source_reference.is_some_and(|reference| reference > 0) {
                    return Err(DebugResponse::StoppedSource(top));
                }
                Err(match top.path {
                    Some(path) => {
                        self.execution = Some((path.clone(), top.line));
                        DebugResponse::StoppedAt(DebugLocation {
                            path,
                            line: top.line,
                            column: top.column,
                        })
                    }
                    None => DebugResponse::Absorbed,
                })
            }
            (StopFollow::Scopes, DapResponse::Scopes(rows)) => {
                self.stop_follow = StopFollow::Idle;
                let issued = self.issued_scopes.pop_front().map(|(_, epoch)| epoch);
                if !Self::is_current_stop_answer(issued, self.stop_epoch) {
                    self.note_refused_answer();
                    return Err(DebugResponse::Absorbed);
                }
                self.expansion.clear();
                self.pending_variable_pages.clear();
                // The first scope is the locals, which is what VS Code opens too.
                if let Some(scope) = rows.into_iter().find(|scope| !scope.expensive) {
                    self.stop_follow = StopFollow::Variables;
                    self.follow_control(DapControl::Variables {
                        reference: scope.variables_reference,
                        start: 0,
                        count: VARIABLE_PAGE,
                    });
                }
                Err(DebugResponse::Absorbed)
            }
            (StopFollow::Variables, DapResponse::Variables(rows)) => {
                self.stop_follow = StopFollow::Idle;
                self.expansion.clear();
                self.pending_variable_pages.clear();
                self.variables = rows
                    .into_iter()
                    .filter(|row| !row.value.is_empty())
                    .map(|row| format!("{} = {}", row.name, row.value))
                    .collect();
                Err(DebugResponse::Absorbed)
            }
            (_, response) => Ok(response),
        }
    }
    pub(in crate::app) fn refresh_watches(&mut self) {
        if self
            .event_source
            .as_ref()
            .is_some_and(|source| !self.service.current(source))
        {
            self.watch_refresh.clear();
            return;
        }
        for _ in 0..16 {
            let Some(expression) = self.watch_refresh.front().cloned() else {
                break;
            };
            match self.control(DapControl::Evaluate {
                expression,
                frame_id: self.console_frame,
                context: "watch".into(),
            }) {
                Ok(()) => {
                    self.watch_refresh.pop_front();
                }
                Err(DapError::Full) => break,
                Err(_) => {
                    self.watch_refresh.clear();
                    break;
                }
            }
        }
    }
    pub(in crate::app) fn follow_control(&mut self, control: DapControl) {
        match self.control_request(control) {
            Ok(Some(id)) => {
                self.requests
                    .get_mut(&id)
                    .expect("issued follow request")
                    .automatic = true;
                self.follow_request = Some(id);
            }
            Ok(None) => {
                self.follow_request = None;
                self.stop_follow = StopFollow::Idle;
            }
            Err(error) => {
                self.follow_request = None;
                self.stop_follow = StopFollow::Idle;
                self.push_console(crate::debugger::DapConsoleEntry::new(
                    crate::debugger::DapConsoleOrigin::Notice,
                    "dust",
                    error.to_string(),
                ));
            }
        }
    }
    fn clear_paused_state(&mut self) {
        self.stop_follow = StopFollow::Idle;
        self.follow_request = None;
        self.answer_request = None;
        self.execution = None;
        self.variables.clear();
        self.watch_values.clear();
        // The adapter actor drops what was in flight when a stop ends, so its answers never come.
        self.evaluations.clear();
        self.pending_submissions.clear();
        self.issued_stack.clear();
        self.issued_scopes.clear();
        self.watch_refresh.clear();
        self.requests.clear();
    }
    pub(in crate::app) fn frames(
        &mut self,
        frames: Vec<DapStackFrame>,
    ) -> Vec<DebugRow> {
        self.install(
            frames
                .into_iter()
                .map(|f| {
                    let detail = f
                        .path
                        .as_ref()
                        .map(|p| format!("{}:{}:{}", p.display(), f.line, f.column))
                        .unwrap_or_default();
                    (f.name.clone(), detail, Choice::Frame(f))
                })
                .collect(),
        )
    }
    pub(in crate::app) fn dismiss_choices(&mut self) {
        self.choices = None;
        // Hiding the inspector closes the expansion path with it, so a node can be opened again
        // after the inspector is reopened rather than being refused forever.
        self.expansion.clear();
        self.pending_variable_pages.clear();
    }
    /// Execution ran on: invalidate the stopped data and every value captured at that stop, so a
    /// previous value can never be presented as current.
    pub(in crate::app) fn begin_running(&mut self) {
        self.clear_paused_state();
        self.stop_reason = None;
        self.stop_epoch = self.stop_epoch.wrapping_add(1);
        self.choices = None;
        self.source = None;
        // Nothing captured at the stop that has ended is open any more.
        self.threads.clear();
        self.stack.clear();
        self.expansion.clear();
        self.pending_variable_pages.clear();
        // The bound frame belonged to the stop that has ended.
        self.console_frame = None;
    }
    /// A new stop began: the PREVIOUS stop's data is not current either, so it goes the same way
    /// before anything is captured for this stop.
    pub(in crate::app) fn begin_stop(&mut self) {
        self.clear_paused_state();
        self.stop_epoch = self.stop_epoch.wrapping_add(1);
        self.choices = None;
        self.source = None;
        // A new stop has its own references; the previous stop's path is not this one's.
        self.threads.clear();
        self.stack.clear();
        self.expansion.clear();
        self.pending_variable_pages.clear();
        // A new stop has its own frames; the previous frame is not this stop's frame.
        self.console_frame = None;
    }
    #[cfg(test)]
    #[must_use]
    pub(in crate::app) fn stop_epoch(&self) -> u64 {
        self.stop_epoch
    }
    pub(in crate::app) fn take_selection(
        &mut self,
        key: &DebugKey,
    ) -> Result<DebugSelection, DapError> {
        let choices = self
            .choices
            .as_ref()
            .filter(|c| Arc::ptr_eq(&c.epoch, &key.epoch) && self.service.current(&c.source))
            .ok_or(DapError::Retired)?;
        if key.index >= choices.values.len() {
            return Err(DapError::Retired);
        }
        let mut choices = self.choices.take().expect("checked choices");
        let choice = choices.values.swap_remove(key.index);
        // Selecting a frame BINDS the console to it: a submission then knows which frame produced
        // it, and the console is the one for that frame rather than for no frame at all.
        if let Choice::Frame(frame) = &choice {
            self.console_frame = Some(frame.id);
            self.variables.clear();
            self.watch_values.clear();
            self.watch_refresh = self.watches.iter().cloned().collect();
            self.stop_follow = StopFollow::Idle;
            self.follow_request = None;
        }
        Ok(DebugSelection {
            expansion: self.expansion.clone(),
            source: choices.source,
            choice,
        })
    }
    pub(in crate::app) fn activate(
        &mut self,
        selection: DebugSelection,
        mut target: SourceTarget,
    ) -> Result<Option<DebugLocation>, DapError> {
        if !self.service.current(&selection.source) {
            return Err(DapError::Retired);
        }
        match selection.choice {
            Choice::Value(_) => Ok(None),
            Choice::Control(control) => {
                self.expansion = selection.expansion;
                self.control(control)?;
                Ok(None)
            }
            Choice::Frame(frame) => {
                let location = if let Some(reference) = frame.source_reference.filter(|r| *r > 0) {
                    target.line = frame.line;
                    target.column = frame.column;
                    self.request_source(reference, target).map(|()| None)
                } else {
                    Ok(frame.path.map(|path| DebugLocation {
                        path,
                        line: frame.line,
                        column: frame.column,
                    }))
                };
                self.stop_follow = StopFollow::Scopes;
                self.follow_control(DapControl::Scopes { frame_id: frame.id });
                self.refresh_watches();
                location
            }
        }
    }
    pub(in crate::app) fn request_source(
        &mut self,
        reference: i64,
        target: SourceTarget,
    ) -> Result<(), DapError> {
        if self.source.is_some() {
            return Err(DapError::Full);
        }
        let context = self.service.context().ok_or(DapError::NotRunning)?;
        self.service
            .control_owned(&context, DapControl::Source { reference })?;
        self.source = Some(Source { context, target });
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::app) fn source_pending(&self) -> bool {
        self.source.is_some()
    }
    pub(in crate::app) fn take_source(&mut self) -> Option<SourceTarget> {
        let source = self.source.take()?;
        self.service
            .current(&source.context)
            .then_some(source.target)
    }
    pub(in crate::app) fn register_terminal(
        &mut self,
        id: PtyId,
        reply: DapTerminalReply,
    ) {
        self.terminal_replies.insert(id, reply);
    }
    pub(in crate::app) fn take_terminal(
        &mut self,
        id: &PtyId,
    ) -> Option<DapTerminalReply> {
        self.terminal_replies.remove(id)
    }
    pub(in crate::app) fn terminal_current(&self, id: &PtyId) -> bool {
        self.terminal_replies
            .get(id)
            .is_none_or(DapTerminalReply::current)
    }
    pub(in crate::app) fn reject_terminals(&mut self, reason: &str) {
        // Keep the association until terminal preflight completes: a rejected reply must still
        // fail the debugger ownership check rather than look like an ordinary terminal.
        for reply in self.terminal_replies.values() {
            let _ = reply.respond(false, Some(reason.into()));
        }
    }
}

#[cfg(test)]
pub(super) mod tests;

pub(in crate::app) mod interaction;

pub(in crate::app) mod commands;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod surfaces_tests;
