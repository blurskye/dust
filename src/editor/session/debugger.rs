//! Private debugger runtime, controls, breakpoint/watch policy and sourced selections.
mod launch;
use super::{
    windows::NavigationStamp,
    workspace::{WorkspaceSession, WorkspaceVersion},
};
use crate::{
    config::{DebugConfig, TerminalProfileConfig, TerminalsConfig},
    debugger::{
        DapContext, DapControl, DapError, DapEvent, DapReplyOutcome, DapReplyReceipt, DapResponse,
        DapService, DapSourceBreakpoint, DapStackFrame, DapTerminalReply,
    },
    persistence::{PersistedBreakpoint, PersistedWatch},
    process::PtyId,
};
pub(in crate::editor::session) use launch::DebugIntent;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct DebugKey {
    epoch: Arc<()>,
    index: usize,
}
pub(in crate::editor::session) enum DebugResponse {
    Picker {
        kind: &'static str,
        rows: Vec<DebugRow>,
    },
    Result(DapResponse),
    Retired,
}
pub(in crate::editor::session) struct DebugRow {
    pub label: String,
    pub detail: String,
    pub key: DebugKey,
}
enum Choice {
    Control(DapControl),
    Frame(DapStackFrame),
}
pub(in crate::editor::session) struct DebugSelection {
    source: DapContext,
    choice: Choice,
}
struct Choices {
    epoch: Arc<()>,
    source: DapContext,
    values: Vec<Choice>,
}
pub(in crate::editor::session) struct SourceTarget {
    pub navigation: NavigationStamp,
    pub capture: Arc<()>,
}
struct Source {
    context: DapContext,
    target: SourceTarget,
}
pub(in crate::editor::session) struct DebugLocation {
    pub path: PathBuf,
    pub line: u64,
    pub column: u64,
}
struct TerminalPolicy {
    name: String,
    profile: TerminalProfileConfig,
    cwd: PathBuf,
}
pub(super) struct DebugSession {
    service: DapService,
    launch: Option<launch::Launch>,
    console: VecDeque<(String, String)>,
    breakpoints: BTreeMap<PathBuf, Vec<DapSourceBreakpoint>>,
    watches: Vec<String>,
    configuration: Option<String>,
    terminal_profile: Option<TerminalPolicy>,
    terminal_replies: HashMap<PtyId, DapTerminalReply>,
    reply_receipts: Vec<(String, i64, DapReplyReceipt)>,
    policy: DebugConfig,
    scope: WorkspaceVersion,
    choices: Option<Choices>,
    event_source: Option<DapContext>,
    source: Option<Source>,
    validation: Option<(
        super::workspace::ValidationId,
        crate::editor::session::debugger::DebugIntent,
    )>,
}
impl DebugSession {
    pub(super) fn defer_validation(
        &mut self,
        id: super::workspace::ValidationId,
        intent: DebugIntent,
    ) {
        debug_assert!(self.validation.is_none());
        self.validation = Some((id, intent));
    }
    pub(super) fn take_validation(
        &mut self,
        id: super::workspace::ValidationId,
    ) -> Option<crate::editor::session::DeferredExecution> {
        let matches = self
            .validation
            .as_ref()
            .is_some_and(|(expected, _)| *expected == id);
        matches.then(|| {
            let (_, intent) = self.validation.take().expect("matched debugger validation");
            crate::editor::session::DeferredExecution::StartDebug(intent)
        })
    }
    pub(in crate::editor::session) fn new(policy: &DebugConfig, scope: WorkspaceVersion) -> Self {
        Self {
            service: DapService::new(),
            launch: None,
            console: VecDeque::new(),
            breakpoints: BTreeMap::new(),
            watches: Vec::new(),
            configuration: None,
            terminal_profile: None,
            terminal_replies: HashMap::new(),
            reply_receipts: Vec::new(),
            policy: policy.clone(),
            scope,
            choices: None,
            event_source: None,
            source: None,
            validation: None,
        }
    }
    pub(in crate::editor::session) fn refresh(
        &mut self,
        policy: &DebugConfig,
        scope: WorkspaceVersion,
    ) {
        if self.policy != *policy || self.scope != scope {
            let _ = self.stop();
            self.policy = policy.clone();
            self.scope = scope;
            self.watches.truncate(policy.maximum_watches);
        }
    }
    pub(in crate::editor::session) fn stop(&mut self) -> Result<(), DapError> {
        let pending_launch = self.launch.take().is_some();
        self.choices = None;
        self.event_source = None;
        self.source = None;
        self.reject_terminals("requesting debugger retired");
        self.terminal_profile = None;
        match self.service.stop() {
            Err(DapError::NotRunning) if pending_launch => Ok(()),
            result => result,
        }
    }
    pub(in crate::editor::session) fn reset(&mut self) {
        let _ = self.stop();
        self.event_source = None;
        self.console.clear();
        self.configuration = None;
        self.breakpoints.clear();
        self.watches.clear();
    }
    pub(in crate::editor::session) fn begin_shutdown(&mut self) {
        let _ = self.stop();
        self.service.begin_shutdown();
    }
    pub(in crate::editor::session) fn pending(&self) -> usize {
        self.service.pending() + self.reply_receipts.len()
    }
    pub(in crate::editor::session) fn is_running(&self) -> bool {
        self.service.is_running()
    }
    pub(in crate::editor::session) fn active_adapter(&self) -> Option<&str> {
        self.service.active_adapter()
    }
    pub(in crate::editor::session) fn configuration(&self) -> Option<&str> {
        self.configuration.as_deref()
    }
    pub(in crate::editor::session) fn terminal_request(
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
    pub(in crate::editor::session) fn reply_failures(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        self.reply_receipts.retain(|(adapter, sequence, receipt)| match receipt.outcome() {
            DapReplyOutcome::Written => false,
            DapReplyOutcome::Failed | DapReplyOutcome::Unknown => { errors.push(format!("DAP {adapter} reverse request {sequence}: {:?}; no peer acknowledgement claimed", receipt.outcome())); false },
            _ => true,
        });
        errors
    }
    pub(in crate::editor::session) fn control(&self, control: DapControl) -> Result<(), DapError> {
        // Source requests require a private selection and an original activation target.
        if matches!(control, DapControl::Source { .. }) {
            return Err(DapError::Retired);
        }
        self.service.control(control)
    }
    pub(in crate::editor::session) fn try_event(&mut self) -> Option<DapEvent> {
        let (context, event) = self.service.try_owned_event()?;
        self.event_source = Some(context);
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
            DapEvent::Output { adapter, output } => {
                self.console.push_back((adapter.clone(), output.clone()));
                while self.console.len() > 10_000 {
                    self.console.pop_front();
                }
            }
            DapEvent::Stopped { .. } => {
                for expression in self.watches.iter().take(100) {
                    // Full is visible as a failed watch submission, not successful evaluation.
                    if let Err(error) = self.service.control(DapControl::Evaluate {
                        expression: expression.clone(),
                        frame_id: None,
                        context: "watch".into(),
                    }) {
                        self.console.push_back(("dust".into(), error.to_string()));
                        break;
                    }
                }
            }
            DapEvent::Running { .. } => {
                self.choices = None;
                self.source = None;
            }
            _ => {}
        }
        Some(event)
    }
    pub(in crate::editor::session) fn console(&self) -> impl Iterator<Item = &(String, String)> {
        self.console.iter()
    }
    pub(in crate::editor::session) fn watches(&self) -> &[String] {
        &self.watches
    }
    pub(in crate::editor::session) fn add_watch(&mut self, expression: String) {
        if !expression.is_empty()
            && expression.len() <= 64 * 1024
            && !expression.contains('\0')
            && self.watches.len() < self.policy.maximum_watches
            && !self.watches.contains(&expression)
        {
            self.watches.push(expression);
        }
    }
    pub(in crate::editor::session) fn remove_watch(&mut self, expression: &str) {
        self.watches.retain(|watch| watch != expression);
    }
    pub(in crate::editor::session) fn breakpoint(
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
        if existing.is_none() && count >= self.policy.maximum_breakpoints {
            return Err("debugger breakpoint limit reached".into());
        }
        let rows = self.breakpoints.entry(row.path.clone()).or_default();
        let removed = toggle && existing.is_some();
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
        let result = if self.service.is_running() {
            self.service
                .control(DapControl::SetBreakpoints {
                    path: row.path.clone(),
                    breakpoints: rows.clone(),
                })
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        if empty {
            self.breakpoints.remove(&row.path);
        }
        result?;
        Ok(removed)
    }
    pub(in crate::editor::session) fn persisted_breakpoints(&self) -> Vec<PersistedBreakpoint> {
        self.breakpoints
            .values()
            .flatten()
            .take(self.policy.maximum_breakpoints)
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
    pub(in crate::editor::session) fn persisted_watches(&self) -> Vec<PersistedWatch> {
        self.watches
            .iter()
            .map(|expression| PersistedWatch {
                expression: expression.clone(),
            })
            .collect()
    }
    pub(in crate::editor::session) fn restore(
        &mut self,
        rows: Vec<PersistedBreakpoint>,
        watches: Vec<PersistedWatch>,
        workspace: &WorkspaceSession,
    ) {
        self.breakpoints.clear();
        for r in rows.into_iter().take(self.policy.maximum_breakpoints) {
            if workspace.require_path(&r.path).is_err() {
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
        self.watches.clear();
        for watch in watches.into_iter().take(self.policy.maximum_watches) {
            self.add_watch(watch.expression);
        }
    }
    fn install(&mut self, values: Vec<(String, String, Choice)>) -> Vec<DebugRow> {
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
    pub(in crate::editor::session) fn controls(
        &mut self,
        rows: Vec<(String, String, DapControl)>,
    ) -> Vec<DebugRow> {
        self.install(
            rows.into_iter()
                .map(|(a, b, c)| (a, b, Choice::Control(c)))
                .collect(),
        )
    }
    pub(in crate::editor::session) fn response(&mut self, response: DapResponse) -> DebugResponse {
        if !self
            .event_source
            .as_ref()
            .is_some_and(|s| self.service.current(s))
        {
            return DebugResponse::Retired;
        }
        let (kind, rows) = match response {
            DapResponse::Threads(rows) => (
                "threads",
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
                ),
            ),
            DapResponse::Stack(rows) => ("frames", self.frames(rows)),
            DapResponse::Scopes(rows) => (
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
                                    count: 1000,
                                },
                            )
                        })
                        .collect(),
                ),
            ),
            DapResponse::Variables(rows) => (
                "variables",
                self.controls(
                    rows.into_iter()
                        .map(|r| {
                            (
                                format!("{} = {}", r.name, r.value),
                                r.type_name.unwrap_or_default(),
                                DapControl::Variables {
                                    reference: r.variables_reference,
                                    start: 0,
                                    count: 1000,
                                },
                            )
                        })
                        .collect(),
                ),
            ),
            value => return DebugResponse::Result(value),
        };
        DebugResponse::Picker { kind, rows }
    }
    pub(in crate::editor::session) fn frames(
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
    pub(in crate::editor::session) fn dismiss_choices(&mut self) {
        self.choices = None;
    }
    pub(in crate::editor::session) fn take_selection(
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
        Ok(DebugSelection {
            source: choices.source,
            choice: choices.values.swap_remove(key.index),
        })
    }
    pub(in crate::editor::session) fn activate(
        &mut self,
        selection: DebugSelection,
        target: SourceTarget,
    ) -> Result<Option<DebugLocation>, DapError> {
        if !self.service.current(&selection.source) {
            return Err(DapError::Retired);
        }
        match selection.choice {
            Choice::Control(control) => {
                self.service.control_owned(&selection.source, control)?;
                Ok(None)
            }
            Choice::Frame(frame) => {
                if let Some(reference) = frame.source_reference {
                    if self.source.is_some() {
                        return Err(DapError::Full);
                    }
                    self.service
                        .control_owned(&selection.source, DapControl::Source { reference })?;
                    self.source = Some(Source {
                        context: selection.source,
                        target,
                    });
                    Ok(None)
                } else {
                    self.service.control_owned(
                        &selection.source,
                        DapControl::Scopes { frame_id: frame.id },
                    )?;
                    Ok(frame.path.map(|path| DebugLocation {
                        path,
                        line: frame.line,
                        column: frame.column,
                    }))
                }
            }
        }
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn source_pending(&self) -> bool {
        self.source.is_some()
    }
    pub(in crate::editor::session) fn take_source(&mut self) -> Option<SourceTarget> {
        let source = self.source.take()?;
        self.service
            .current(&source.context)
            .then_some(source.target)
    }
    pub(in crate::editor::session) fn register_terminal(
        &mut self,
        id: PtyId,
        reply: DapTerminalReply,
    ) {
        self.terminal_replies.insert(id, reply);
    }
    pub(in crate::editor::session) fn take_terminal(
        &mut self,
        id: &PtyId,
    ) -> Option<DapTerminalReply> {
        self.terminal_replies.remove(id)
    }
    pub(in crate::editor::session) fn terminal_current(&self, id: &PtyId) -> bool {
        self.terminal_replies
            .get(id)
            .is_none_or(DapTerminalReply::current)
    }
    pub(in crate::editor::session) fn reject_terminals(&mut self, reason: &str) {
        for (_, reply) in self.terminal_replies.drain() {
            let _ = reply.respond(false, Some(reason.into()));
        }
    }
}

#[cfg(test)]
mod tests;
