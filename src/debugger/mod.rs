#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap},
    io::BufReader,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::Ordering,
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use thiserror::Error;

use crate::process::read_content_length_message;

#[derive(Clone, Debug)]
pub enum DapEvent {
    Starting {
        adapter: String,
    },
    Running {
        adapter: String,
    },
    Stopped {
        adapter: String,
        reason: String,
        thread_id: Option<i64>,
    },
    Output {
        adapter: String,
        output: String,
    },
    Terminated {
        adapter: String,
    },
    Response {
        adapter: String,
        stop_generation: u64,
        response: DapResponse,
    },
    RequestFailed {
        adapter: String,
        stop_generation: u64,
        request_seq: i64,
        command: String,
        message: String,
    },
    RunInTerminal {
        adapter: String,
        request_seq: i64,
        reply: DapTerminalReply,
        cwd: PathBuf,
        arguments: Vec<String>,
        environment: HashMap<String, String>,
    },
    Failed {
        adapter: String,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct DapThread {
    pub id: i64,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct DapStackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: u64,
    pub column: u64,
    pub source_reference: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct DapScope {
    pub name: String,
    pub variables_reference: i64,
    pub expensive: bool,
}
#[derive(Clone, Debug)]
pub struct DapVariable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub variables_reference: i64,
}
#[derive(Clone, Debug)]
pub enum DapResponse {
    Threads(Vec<DapThread>),
    Stack(Vec<DapStackFrame>),
    Scopes(Vec<DapScope>),
    Variables(Vec<DapVariable>),
    Evaluate(DapVariable),
    Breakpoints(Vec<(bool, Option<u64>, Option<String>)>),
    Source {
        name: String,
        content: String,
        mime_type: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct DapSourceBreakpoint {
    pub path: PathBuf,
    pub line: u64,
    pub column: Option<u64>,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
}
#[derive(Clone, Debug)]
pub enum DapControl {
    Continue,
    Pause,
    Next,
    StepIn,
    StepOut,
    Threads,
    StackTrace {
        thread_id: i64,
        start: u64,
        levels: u64,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        reference: i64,
        start: u64,
        count: u64,
    },
    Evaluate {
        expression: String,
        frame_id: Option<i64>,
        context: String,
    },
    /// End the session, saying whether the debuggee should be ended with it. This is what makes
    /// DETACH distinct from TERMINATE: the editor stops being attached either way, but only one
    /// of them asks for the debuggee to die.
    Disconnect {
        terminate_debuggee: bool,
    },
    SetBreakpoints {
        path: PathBuf,
        breakpoints: Vec<DapSourceBreakpoint>,
    },
    Source {
        reference: i64,
    },
    ReverseResponse {
        request_seq: i64,
        success: bool,
        message: Option<String>,
        process_id: Option<u32>,
    },
    Stop,
}

mod reverse;
pub(crate) use reverse::DapReplyReceipt;
pub use reverse::{DapReplyOutcome, DapTerminalReply};

mod session;
pub use session::DapService;
pub(crate) use session::{DapContext, ExecutionScope};
use session::{Events, Writer};

pub struct DapLaunch {
    pub adapter: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: HashMap<String, String>,
    pub path_maps: BTreeMap<PathBuf, PathBuf>,
    pub root: PathBuf,
    pub request: String,
    pub request_arguments: Value,
    /// Sent when the adapter says it is ready, before the program may run.
    pub breakpoints: Vec<(PathBuf, Vec<DapSourceBreakpoint>)>,
}

fn set_breakpoints_arguments(
    path: &Path,
    breakpoints: Vec<DapSourceBreakpoint>,
    path_maps: &BTreeMap<PathBuf, PathBuf>,
) -> Value {
    let path = map_path(path, path_maps, false);
    json!({"source":{"path":path},"breakpoints":breakpoints.into_iter().take(10_000).map(|item|json!({"line":item.line,"column":item.column,"condition":item.condition,"hitCondition":item.hit_condition,"logMessage":item.log_message})).collect::<Vec<_>>()})
}

/// The argument that makes the Dust binary start a program in a session of its own.
pub const ADAPTER_SESSION_ARGUMENT: &str = "--adapter-session";
static ADAPTER_SESSION_LAUNCHER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Name the binary that can start adapters in their own session. `main` does this once.
pub fn use_adapter_session_launcher(binary: PathBuf) {
    let _ = ADAPTER_SESSION_LAUNCHER.set(binary);
}

/// Become a session leader and turn into `program`. This has no controlling terminal, so debugpy
/// cannot make its program the foreground group of /dev/tty, which would leave the editor unable
/// to read keys. Only returns if it failed.
#[cfg(unix)]
pub fn exec_in_own_session(program: &std::ffi::OsStr, arguments: &[std::ffi::OsString]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    if let Err(error) = nix::unistd::setsid() {
        return error.into();
    }
    Command::new(program).args(arguments).exec()
}

fn adapter_command(executable: PathBuf, arguments: Vec<String>) -> Command {
    match ADAPTER_SESSION_LAUNCHER.get().filter(|binary| binary.is_file()) {
        Some(binary) => {
            let mut command = Command::new(binary);
            command
                .arg(ADAPTER_SESSION_ARGUMENT)
                .arg(executable)
                .args(arguments);
            command
        }
        None => {
            let mut command = Command::new(executable);
            command.args(arguments);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            command
        }
    }
}

fn run_adapter(
    launch: DapLaunch,
    controls: Receiver<session::Control>,
    events: Events,
    scope: Option<ExecutionScope>,
) {
    let live = events.actor.live.clone();
    struct Live(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let _live = Live(live.clone());
    let DapLaunch {
        adapter,
        executable,
        arguments,
        environment,
        path_maps,
        root,
        request,
        request_arguments,
        breakpoints: mut initial_breakpoints,
    } = launch;
    if let Some(scope) = &scope {
        if let Err(message) = scope.check() {
            events.send(DapEvent::Failed { adapter, message }).ok();
            return;
        }
    }
    let mut command = adapter_command(executable, arguments);
    command
        .env_clear()
        .envs(environment)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if events.actor.cancel.load(Ordering::Acquire) {
        return;
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            events
                .send(DapEvent::Failed {
                    adapter,
                    message: error.to_string(),
                })
                .ok();
            return;
        }
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            terminate_adapter(&mut self.0);
        }
    }
    let mut child = Child(child);
    // Retire reply/source authority before potentially blocking child cleanup,
    // including unwind; rebinding transfers this guard's drop order.
    let _live = _live;
    let child = &mut child.0;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        events
            .send(DapEvent::Failed {
                adapter,
                message: "adapter pipes unavailable".into(),
            })
            .ok();
        return;
    };
    let (messages, receiver) = mpsc::sync_channel(32);
    let reader_events = events.clone();
    let reader = thread::Builder::new()
        .name("dust-dap-reader".into())
        .spawn(move || {
            let _resident = reader_events.clone();
            let mut reader = BufReader::new(stdout);
            loop {
                // Framing already enforces a bounded header and 16MiB message limit.
                let value = match read_content_length_message(&mut reader) {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => break,
                    Err(error) => Err(error.to_string()),
                };
                if !session::deliver(&messages, value, &reader_events.actor.cancel) {
                    break;
                }
            }
        });
    if let Err(error) = reader {
        events
            .send(DapEvent::Failed {
                adapter,
                message: error.to_string(),
            })
            .ok();
        return;
    }
    // A pipe write cannot hold the controller hostage. Both I/O threads retain
    // resident leases, including while an inherited descriptor is stuck.
    let (mut stdin, writer) = match Writer::start(stdin, events.clone()) {
        Ok(writer) => writer,
        Err(error) => {
            events
                .send(DapEvent::Failed {
                    adapter,
                    message: error.to_string(),
                })
                .ok();
            return;
        }
    };
    let mut sequence = 1_i64;
    send_request(
        &mut stdin,
        sequence,
        "initialize",
        json!({
            "clientID":"dust", "clientName":"Dust", "adapterID":adapter,
            "pathFormat":"path", "linesStartAt1":true, "columnsStartAt1":true,
            "supportsVariableType":true, "supportsRunInTerminalRequest":true
        }),
    );
    let initialize_sequence = sequence;
    sequence += 1;
    let mut launched = false;
    let mut launch_accepted = false;
    let mut paused = false;
    let mut supports_configuration_done = false;
    let mut thread_id = None;
    let mut stopping = false;
    let mut stop_generation = 0_u64;
    let mut pending: HashMap<i64, (String, u64)> = HashMap::new();
    let mut reverse = Vec::<reverse::ReverseRequest>::new();
    let mut last_reverse_sequence = 0;
    while !stopping && !events.actor.cancel.load(Ordering::Acquire) {
        if sequence > i64::MAX - 128 {
            break;
        }
        reverse.retain_mut(|request| !request.flush(&mut stdin, &mut sequence));
        for _ in 0..32 {
            let Ok(control) = controls.try_recv() else {
                break;
            };
            if control.generation != events.actor.generation.load(Ordering::Acquire) {
                continue;
            }
            let control = control.value;
            if let DapControl::ReverseResponse { request_seq, .. } = &control {
                if let Some(request) = reverse.iter_mut().find(|r| r.sequence == *request_seq) {
                    request.raw(control);
                }
                continue;
            }
            if pending.len() >= 128 || sequence > i64::MAX - 8 {
                events
                    .send(DapEvent::Failed {
                        adapter: adapter.clone(),
                        message: "DAP request capacity or sequence exhausted".into(),
                    })
                    .ok();
                stopping = true;
                break;
            }
            if matches!(
                control,
                DapControl::Continue | DapControl::Next | DapControl::StepIn | DapControl::StepOut
            ) {
                let Some(next) = stop_generation.checked_add(1) else {
                    stopping = true;
                    break;
                };
                stop_generation = next;
                events.actor.generation.store(next, Ordering::Release);
                pending.retain(|_, (command, _)| is_startup_request(command));
            }
            let (command, arguments) = match control {
                DapControl::Continue => (
                    Some("continue"),
                    thread_id.map_or_else(|| json!({}), |id| json!({"threadId":id})),
                ),
                DapControl::Pause => (
                    Some("pause"),
                    thread_id.map_or_else(|| json!({}), |id| json!({"threadId":id})),
                ),
                DapControl::Next => (
                    Some("next"),
                    thread_id.map_or_else(|| json!({}), |id| json!({"threadId":id})),
                ),
                DapControl::StepIn => (
                    Some("stepIn"),
                    thread_id.map_or_else(|| json!({}), |id| json!({"threadId":id})),
                ),
                DapControl::StepOut => (
                    Some("stepOut"),
                    thread_id.map_or_else(|| json!({}), |id| json!({"threadId":id})),
                ),
                DapControl::Threads => (Some("threads"), json!({})),
                DapControl::StackTrace {
                    thread_id,
                    start,
                    levels,
                } => (
                    Some("stackTrace"),
                    json!({"threadId":thread_id,"startFrame":start,"levels":levels.min(1000)}),
                ),
                DapControl::Scopes { frame_id } => (Some("scopes"), json!({"frameId":frame_id})),
                DapControl::Variables {
                    reference,
                    start,
                    count,
                } => (
                    Some("variables"),
                    json!({"variablesReference":reference,"start":start,"count":count.min(10_000)}),
                ),
                DapControl::Evaluate {
                    expression,
                    frame_id,
                    context,
                } => (
                    Some("evaluate"),
                    json!({"expression":expression.chars().take(64*1024).collect::<String>(),"frameId":frame_id,"context":context}),
                ),
                DapControl::Source { reference } => {
                    (Some("source"), json!({"sourceReference":reference}))
                }
                DapControl::ReverseResponse { .. } => unreachable!("reverse control routed above"),
                DapControl::Disconnect { terminate_debuggee } => {
                    // The editor is leaving the session either way; whether the program dies is
                    // exactly what the argument says, so a detach cannot be labelled a terminate.
                    (Some("disconnect"), disconnect_arguments(terminate_debuggee))
                }
                DapControl::SetBreakpoints { path, breakpoints } => (
                    Some("setBreakpoints"),
                    set_breakpoints_arguments(&path, breakpoints, &path_maps),
                ),
                DapControl::Stop => {
                    stopping = true;
                    (None, json!({}))
                }
            };
            if let Some(command) = command {
                send_request(&mut stdin, sequence, command, arguments);
                pending.insert(sequence, (command.into(), stop_generation));
                sequence += 1;
            }
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(message)) => match message.get("type").and_then(Value::as_str) {
                Some("response")
                    if message.get("request_seq").and_then(Value::as_i64)
                        == Some(initialize_sequence)
                        && message.get("command").and_then(Value::as_str) == Some("initialize")
                        && !launched =>
                {
                    if message.get("success").and_then(Value::as_bool) != Some(true) {
                        events
                            .send(DapEvent::Failed {
                                adapter: adapter.clone(),
                                message: "DAP initialize rejected".into(),
                            })
                            .ok();
                        break;
                    }
                    supports_configuration_done = message
                        .pointer("/body/supportsConfigurationDoneRequest")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if let Some(scope) = &scope {
                        if let Err(message) = scope.check() {
                            events
                                .send(DapEvent::Failed {
                                    adapter: adapter.clone(),
                                    message,
                                })
                                .ok();
                            break;
                        }
                    }
                    send_request(&mut stdin, sequence, &request, request_arguments.clone());
                    pending.insert(sequence, (request.clone(), stop_generation));
                    sequence += 1;
                    launched = true;
                }
                Some("response") => {
                    if let Some(request_seq) = message.get("request_seq").and_then(Value::as_i64) {
                        if pending.get(&request_seq).is_some_and(|(command, _)| {
                            message.get("command").and_then(Value::as_str) == Some(command.as_str())
                        }) {
                            if let Some((command, generation)) = pending.remove(&request_seq) {
                                if generation == stop_generation || is_startup_request(&command) {
                                    if message.get("success").and_then(Value::as_bool) != Some(true)
                                    {
                                        let message = format!(
                                            "DAP {command} request rejected: {}",
                                            bounded(message.get("message"))
                                        );
                                        if is_startup_request(&command) {
                                            events
                                                .send(DapEvent::Failed {
                                                    adapter: adapter.clone(),
                                                    message,
                                                })
                                                .ok();
                                            break;
                                        }
                                        events
                                            .send(DapEvent::RequestFailed {
                                                adapter: adapter.clone(),
                                                stop_generation: generation,
                                                request_seq,
                                                command,
                                                message,
                                            })
                                            .ok();
                                        continue;
                                    }
                                    if is_startup_request(&command) {
                                        launch_accepted = true;
                                        // A launch acknowledgement is not a continued event.
                                        if !paused {
                                            events
                                                .send(DapEvent::Running {
                                                    adapter: adapter.clone(),
                                                })
                                                .ok();
                                        }
                                    } else if matches!(
                                        command.as_str(),
                                        "continue" | "next" | "stepIn" | "stepOut"
                                    ) {
                                        paused = false;
                                        if launch_accepted {
                                            events
                                                .send(DapEvent::Running {
                                                    adapter: adapter.clone(),
                                                })
                                                .ok();
                                        }
                                    }
                                    if let Some(response) = normalize_response(
                                        &command,
                                        message.get("body").unwrap_or(&Value::Null),
                                        &path_maps,
                                    ) {
                                        events
                                            .send(DapEvent::Response {
                                                adapter: adapter.clone(),
                                                stop_generation: generation,
                                                response,
                                            })
                                            .ok();
                                    } else if matches!(
                                        command.as_str(),
                                        "threads"
                                            | "stackTrace"
                                            | "scopes"
                                            | "variables"
                                            | "evaluate"
                                            | "source"
                                            | "setBreakpoints"
                                    ) {
                                        events
                                            .send(DapEvent::RequestFailed {
                                                adapter: adapter.clone(),
                                                stop_generation: generation,
                                                request_seq,
                                                command,
                                                message:
                                                    "DAP response body is missing or malformed"
                                                        .into(),
                                            })
                                            .ok();
                                    }
                                }
                            }
                        }
                    }
                }
                Some("event") => {
                    let event = message
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match event {
                        "initialized" => {
                            for (path, rows) in std::mem::take(&mut initial_breakpoints) {
                                let arguments = set_breakpoints_arguments(&path, rows, &path_maps);
                                send_request(&mut stdin, sequence, "setBreakpoints", arguments);
                                pending.insert(sequence, ("setBreakpoints".into(), stop_generation));
                                sequence += 1;
                            }
                            if supports_configuration_done {
                                send_request(&mut stdin, sequence, "configurationDone", json!({}));
                                sequence += 1;
                            }
                        }
                        "stopped" => {
                            let Some(next) = stop_generation.checked_add(1) else {
                                break;
                            };
                            stop_generation = next;
                            events.actor.generation.store(next, Ordering::Release);
                            paused = true;
                            // Startup belongs to this actor, not to one stopped context.
                            pending.retain(|_, (command, _)| is_startup_request(command));
                            thread_id = message.pointer("/body/threadId").and_then(Value::as_i64);
                            let reason = message
                                .pointer("/body/reason")
                                .and_then(Value::as_str)
                                .unwrap_or("stopped")
                                .to_owned();
                            send_request(&mut stdin, sequence, "threads", json!({}));
                            pending.insert(sequence, ("threads".into(), stop_generation));
                            sequence += 1;
                            events
                                .send(DapEvent::Stopped {
                                    adapter: adapter.clone(),
                                    reason,
                                    thread_id,
                                })
                                .ok();
                        }
                        "continued" => {
                            let Some(next) = stop_generation.checked_add(1) else {
                                break;
                            };
                            stop_generation = next;
                            events.actor.generation.store(next, Ordering::Release);
                            paused = false;
                            pending.retain(|_, (command, _)| is_startup_request(command));
                            if launch_accepted {
                                events
                                    .send(DapEvent::Running {
                                        adapter: adapter.clone(),
                                    })
                                    .ok();
                            }
                        }
                        "output"
                            if message.pointer("/body/category").and_then(Value::as_str)
                                == Some("telemetry") => {}
                        "output" => {
                            let output = message
                                .pointer("/body/output")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            events
                                .send(DapEvent::Output {
                                    adapter: adapter.clone(),
                                    output: sanitize_output(output),
                                })
                                .ok();
                        }
                        "terminated" | "exited" => {
                            events
                                .send(DapEvent::Terminated {
                                    adapter: adapter.clone(),
                                })
                                .ok();
                            stopping = true;
                        }
                        _ => {}
                    }
                }
                Some("request") => {
                    let request_seq = message.get("seq").and_then(Value::as_i64).unwrap_or(0);
                    let command = message
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if request_seq <= last_reverse_sequence || reverse.len() >= 32 {
                        events
                            .send(DapEvent::Failed {
                                adapter: adapter.clone(),
                                message: "DAP reverse request identity reused or admission full"
                                    .into(),
                            })
                            .ok();
                        break;
                    }
                    last_reverse_sequence = request_seq;
                    if command == "runInTerminal" {
                        // Reserve the original obligation BEFORE argv/environment projection.
                        let (request, reply) =
                            reverse::ReverseRequest::reserve(request_seq, live.clone());
                        reverse.push(request);
                        if let Some((cwd, arguments, environment)) =
                            reverse::terminal_arguments(&message)
                        {
                            events
                                .send(DapEvent::RunInTerminal {
                                    adapter: adapter.clone(),
                                    request_seq,
                                    reply,
                                    cwd,
                                    arguments,
                                    environment,
                                })
                                .ok();
                        } else {
                            let _ = reply.respond(
                                false,
                                Some("invalid bounded runInTerminal arguments".into()),
                            );
                        }
                    } else {
                        let response = json!({"seq":sequence,"type":"response","request_seq":request_seq,"success":false,"command":command,"message":"unsupported reverse request"});
                        let _ = stdin.submit(response);
                        sequence += 1;
                    }
                }
                _ => {}
            },
            Ok(Err(message)) => {
                events
                    .send(DapEvent::Failed {
                        adapter: adapter.clone(),
                        message,
                    })
                    .ok();
                stopping = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => stopping = true,
        }
        if child.try_wait().ok().flatten().is_some() {
            stopping = true;
        }
        if writer.is_finished() && !events.actor.cancel.load(Ordering::Acquire) {
            live.store(false, Ordering::Release);
            events
                .send(DapEvent::Failed {
                    adapter: adapter.clone(),
                    message: "DAP writer stopped; pending write outcomes unknown".into(),
                })
                .ok();
            stopping = true;
        }
    }
    // Out-of-band cancellation is not a successful protocol disconnect.
    // Controller owns kill/wait; no foreground thread waits for this cleanup.
    live.store(false, Ordering::Release);
    events.actor.cancel.store(true, Ordering::Release);
    drop(stdin);
    drop(reader);
    drop(writer);
}

/// The arguments a `disconnect` request carries. Extracted so the value that distinguishes a
/// detach from a terminate is directly testable rather than buried in the request builder.
#[must_use]
pub fn disconnect_arguments(terminate_debuggee: bool) -> Value {
    json!({ "terminateDebuggee": terminate_debuggee })
}

/// Where a console entry came from. The distinction is the point: program output and a REPL
/// result are different things, and a surface that cannot tell them apart misleads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DapConsoleOrigin {
    /// The debuggee wrote it.
    ProgramOutput,
    /// The adapter's answer to an evaluation the user asked for.
    Evaluation,
    /// What the user typed into the console.
    Command,
    /// The editor's own report, such as a failed submission.
    Notice,
}

impl DapConsoleOrigin {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ProgramOutput => "program output",
            Self::Evaluation => "evaluation result",
            Self::Command => "console command",
            Self::Notice => "editor notice",
        }
    }
}

/// One console entry, carrying its ORIGIN as well as its source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DapConsoleEntry {
    pub origin: DapConsoleOrigin,
    pub source: String,
    pub text: String,
    /// The adapter that produced this record, when the session knows it.
    pub actor: Option<String>,
    /// The STOP this record belongs to, when it belongs to one.
    pub stop: Option<u64>,
    /// The frame the submission was bound to, when one was selected.
    pub frame: Option<i64>,
    /// The submission this record belongs to. A late answer names the submission it answers.
    pub submission: Option<u64>,
    /// The answer attached to the submission's OWN record, once it arrives.
    pub result: Option<String>,
}
impl DapConsoleEntry {
    /// A record with no provenance yet: the session stamps what it knows at the console choke
    /// point, so provenance is recorded in ONE place rather than re-derived at every site.
    #[must_use]
    pub fn new(
        origin: DapConsoleOrigin,
        source: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            source: source.into(),
            text: text.into(),
            actor: None,
            stop: None,
            frame: None,
            submission: None,
            result: None,
        }
    }
}

/// How a session ended, so a surface can distinguish leaving from killing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DapSessionEnd {
    /// The editor detached: it is no longer attached, and it did NOT ask the debuggee to stop.
    Detached,
    /// The editor terminated: it asked for the debuggee to end with the session.
    Terminated,
}

fn is_startup_request(command: &str) -> bool {
    matches!(command, "launch" | "attach")
}

fn bounded(value: Option<&Value>) -> String {
    sanitize_output(value.and_then(Value::as_str).unwrap_or_default())
}
fn map_path(path: &Path, maps: &BTreeMap<PathBuf, PathBuf>, incoming: bool) -> PathBuf {
    maps.iter()
        .filter_map(|(local, remote)| {
            let (from, to) = if incoming {
                (remote, local)
            } else {
                (local, remote)
            };
            path.strip_prefix(from)
                .ok()
                .map(|suffix| (from.components().count(), to.join(suffix)))
        })
        .max_by_key(|(length, _)| *length)
        .map_or_else(|| path.to_path_buf(), |(_, mapped)| mapped)
}
fn normalize_response(
    command: &str,
    body: &Value,
    path_maps: &BTreeMap<PathBuf, PathBuf>,
) -> Option<DapResponse> {
    match command {
        "threads" => Some(DapResponse::Threads(
            body.get("threads")?
                .as_array()?
                .iter()
                .take(1024)
                .filter_map(|item| {
                    Some(DapThread {
                        id: item.get("id")?.as_i64()?,
                        name: bounded(item.get("name")),
                    })
                })
                .collect(),
        )),
        "stackTrace" => Some(DapResponse::Stack(
            body.get("stackFrames")?
                .as_array()?
                .iter()
                .take(4096)
                .filter_map(|item| {
                    Some(DapStackFrame {
                        id: item.get("id")?.as_i64()?,
                        name: bounded(item.get("name")),
                        path: item
                            .pointer("/source/path")
                            .and_then(Value::as_str)
                            .map(PathBuf::from)
                            .map(|path| map_path(&path, path_maps, true)),
                        line: item.get("line").and_then(Value::as_u64).unwrap_or(0),
                        column: item.get("column").and_then(Value::as_u64).unwrap_or(0),
                        source_reference: item
                            .pointer("/source/sourceReference")
                            .and_then(Value::as_i64)
                            .filter(|value| *value > 0),
                    })
                })
                .collect(),
        )),
        "scopes" => Some(DapResponse::Scopes(
            body.get("scopes")?
                .as_array()?
                .iter()
                .take(1024)
                .filter_map(|item| {
                    Some(DapScope {
                        name: bounded(item.get("name")),
                        variables_reference: item.get("variablesReference")?.as_i64()?,
                        expensive: item
                            .get("expensive")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect(),
        )),
        "variables" => Some(DapResponse::Variables(
            body.get("variables")?
                .as_array()?
                .iter()
                .take(10_000)
                .map(|item| DapVariable {
                    name: bounded(item.get("name")),
                    value: bounded(item.get("value")),
                    type_name: item
                        .get("type")
                        .and_then(Value::as_str)
                        .map(sanitize_output),
                    variables_reference: item
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                })
                .collect(),
        )),
        "evaluate" => Some(DapResponse::Evaluate(DapVariable {
            name: "result".into(),
            value: bounded(body.get("result")),
            type_name: body
                .get("type")
                .and_then(Value::as_str)
                .map(sanitize_output),
            variables_reference: body
                .get("variablesReference")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        })),
        "source" => {
            let content = body.get("content")?.as_str()?;
            if content.len() > 16 * 1024 * 1024 {
                return None;
            }
            Some(DapResponse::Source {
                name: bounded(body.get("name")),
                content: content
                    .chars()
                    .filter(|character| !matches!(*character, '\u{1b}' | '\0'))
                    .collect(),
                mime_type: body
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(sanitize_output),
            })
        }
        "setBreakpoints" => Some(DapResponse::Breakpoints(
            body.get("breakpoints")?
                .as_array()?
                .iter()
                .take(10_000)
                .map(|item| {
                    (
                        item.get("verified")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        item.get("line").and_then(Value::as_u64),
                        item.get("message")
                            .and_then(Value::as_str)
                            .map(sanitize_output),
                    )
                })
                .collect(),
        )),
        _ => None,
    }
}
fn terminate_adapter(child: &mut std::process::Child) {
    // Do not signal a numeric process group after the direct child was reaped.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn send_request(writer: &mut Writer, seq: i64, command: &str, arguments: Value) {
    let _ = writer.submit(json!({
        "seq":seq, "type":"request", "command":command, "arguments":arguments
    }));
}

fn sanitize_output(output: &str) -> String {
    output
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(4096)
        .collect()
}

#[derive(Debug, Error)]
pub enum DapError {
    #[error("tool `{0}` is not a debug adapter")]
    NotAdapter(String),
    #[error("a debug adapter session is already running")]
    AlreadyRunning,
    #[error("no debug adapter session is running")]
    NotRunning,
    #[error("debug adapter executable `{0}` is not installed")]
    ExecutableMissing(String),
    #[error("invalid debug adapter configuration")]
    InvalidConfiguration,
    #[error("debug adapter admission is full")]
    Full,
    #[error("debug adapter source or stopped context retired")]
    Retired,
    #[error("failed to create debug-adapter thread: {0}")]
    Thread(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::sanitize_output;

    #[test]
    fn adapter_output_is_bounded_and_terminal_sanitized() {
        assert_eq!(sanitize_output("ok\u{1b}[31m"), "ok[31m");
        assert_eq!(sanitize_output(&"x".repeat(5000)).len(), 4096);
    }
}

#[cfg(test)]
pub(crate) fn terminal_reply_source_fixture() {
    reverse::fixture();
}

#[cfg(test)]
pub(crate) use session::{fixture as test_session, TestAdapter};
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
pub(crate) use transport_tests::{
    ADAPTER as TEST_ADAPTER, LOGGING_ADAPTER as TEST_LOGGING_ADAPTER, python as test_python,
};

/// What is TRUE about a breakpoint, as opposed to what was asked for.
///
/// The distinction is the whole point of this type: a breakpoint the adapter has not answered
/// for is `Requested` and must never be shown as verified, because presenting a request as a
/// fact is exactly the failure mode an optimistic UI produces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DapBreakpointState {
    /// Sent, and the adapter has not answered yet.
    ///
    /// Carries the adapter's own verdict when it has answered without verifying the breakpoint:
    /// an ANSWERED but unverified request is still not a fact, which is why the two are one state
    /// with the answer recorded rather than two states that a caller could confuse.
    Requested { accepted: bool },
    /// The adapter confirmed the breakpoint at this location.
    Verified { line: u64 },
    /// The user disabled it. Independent of what the adapter thinks.
    Disabled,
}

impl DapBreakpointState {
    /// Whether this breakpoint may be PRESENTED as verified. Only a real acknowledgement may.
    #[must_use]
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Requested { accepted: false } => "requested (adapter did not verify it)",
            Self::Requested { accepted: true } => "requested (awaiting acknowledgement)",
            Self::Verified { .. } => "verified",
            Self::Disabled => "disabled",
        }
    }
}

/// What the adapter answered for a `setBreakpoints` request, paired with what was asked for.
///
/// `requested` is the list the editor sent, in order; `answer` is the adapter's own list. A
/// breakpoint the adapter did not answer for stays `Requested`: an unacknowledged request is
/// never promoted to a fact.
#[must_use]
pub fn breakpoint_states(
    requested: &[DapSourceBreakpoint],
    disabled: &[bool],
    answer: &DapResponse,
) -> Vec<DapBreakpointState> {
    let DapResponse::Breakpoints(rows) = answer else {
        return requested
            .iter()
            .enumerate()
            .map(|(index, _)| {
                if disabled.get(index).copied().unwrap_or(false) {
                    DapBreakpointState::Disabled
                } else {
                    DapBreakpointState::Requested { accepted: false }
                }
            })
            .collect();
    };
    requested
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if disabled.get(index).copied().unwrap_or(false) {
                return DapBreakpointState::Disabled;
            }
            match rows.get(index) {
                Some((true, line, _)) => DapBreakpointState::Verified {
                    // The adapter may move a breakpoint; the VERIFIED line is its line, falling
                    // back to the requested one when it reports none.
                    line: line.unwrap_or(0),
                },
                Some((false, _, _)) => DapBreakpointState::Requested { accepted: false },
                // No answer for this entry: still only a request.
                None => DapBreakpointState::Requested { accepted: true },
            }
        })
        .collect()
}

/// Where execution ACTUALLY stopped. This is a separate fact from any breakpoint's state: a
/// verified breakpoint is not evidence that execution is there now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DapExecutionPoint {
    pub line: u64,
    pub thread_id: Option<i64>,
}

impl DapExecutionPoint {
    /// Whether execution is standing on the given verified line.
    #[must_use]
    pub fn is_at(self, state: DapBreakpointState, line: u64) -> bool {
        matches!(state, DapBreakpointState::Verified { .. }) && self.line == line
    }
}

#[cfg(test)]
mod disconnect_tests {
    use super::disconnect_arguments;

    #[test]
    fn detach_and_terminate_send_different_requests() {
        // The editor-side distinction, asserted on the value that carries it: a detach must NOT
        // ask for the debuggee to stop, and a terminate must.
        assert_eq!(
            disconnect_arguments(false)["terminateDebuggee"],
            serde_json::json!(false),
            "a detach leaves the debuggee alone"
        );
        assert_eq!(
            disconnect_arguments(true)["terminateDebuggee"],
            serde_json::json!(true),
            "a terminate asks for it to end"
        );
        assert_ne!(
            disconnect_arguments(false),
            disconnect_arguments(true),
            "the two are not the same request wearing different names"
        );
    }
}

#[cfg(test)]
mod breakpoint_state_tests {
    use super::{
        DapBreakpointState, DapExecutionPoint, DapResponse, DapSourceBreakpoint, breakpoint_states,
    };
    use std::path::PathBuf;

    fn requested(lines: &[u64]) -> Vec<DapSourceBreakpoint> {
        lines
            .iter()
            .map(|line| DapSourceBreakpoint {
                path: PathBuf::from("/workspace/main.rs"),
                line: *line,
                column: None,
                condition: None,
                hit_condition: None,
                log_message: None,
            })
            .collect()
    }

    #[test]
    fn breakpoint_states_never_present_a_request_as_verified() {
        let asked = requested(&[10, 20, 30]);
        let up_to_date = vec![false, false, false];

        // NOTHING ACKNOWLEDGED: every breakpoint is only a request, and NONE is verified.
        let pending = breakpoint_states(&asked, &up_to_date, &DapResponse::Breakpoints(Vec::new()));
        assert!(
            pending.iter().all(|state| !state.is_verified()),
            "an unanswered request is never verified: {pending:?}"
        );
        assert!(
            pending
                .iter()
                .all(|state| matches!(state, DapBreakpointState::Requested { .. })),
            "and every one is reported as requested: {pending:?}"
        );
        assert_eq!(pending[0].label(), "requested (awaiting acknowledgement)");

        // The adapter ANSWERS but declines to verify one of them: an answered request is still
        // not a fact, so it stays unverified and says so.
        let partial = breakpoint_states(
            &asked,
            &up_to_date,
            &DapResponse::Breakpoints(vec![
                (true, Some(10), None),
                (false, None, Some("no code at line 20".into())),
                (true, Some(31), None),
            ]),
        );
        assert_eq!(partial[0], DapBreakpointState::Verified { line: 10 });
        assert_eq!(partial[1], DapBreakpointState::Requested { accepted: false });
        assert!(
            !partial[1].is_verified(),
            "an adapter that did NOT verify a breakpoint must not be shown as verifying it"
        );
        assert_eq!(
            partial[1].label(),
            "requested (adapter did not verify it)",
            "and the state says which of the two it is"
        );
        assert_eq!(
            partial[2],
            DapBreakpointState::Verified { line: 31 },
            "the VERIFIED line is the adapter's line, which it may have moved"
        );

        // DISABLED is the user's state and survives an adapter that verified everything.
        let with_disabled = breakpoint_states(
            &asked,
            &[false, true, false],
            &DapResponse::Breakpoints(vec![
                (true, Some(10), None),
                (true, Some(20), None),
                (true, Some(30), None),
            ]),
        );
        assert_eq!(with_disabled[1], DapBreakpointState::Disabled);
        assert!(
            !with_disabled[1].is_verified(),
            "a disabled breakpoint is not verified even when the adapter accepted it"
        );

        // EXECUTION is a SEPARATE fact: a verified breakpoint is not evidence that execution is
        // standing there.
        let stopped = DapExecutionPoint {
            line: 10,
            thread_id: Some(1),
        };
        assert!(stopped.is_at(DapBreakpointState::Verified { line: 10 }, 10));
        assert!(
            !stopped.is_at(DapBreakpointState::Requested { accepted: true }, 10),
            "an unacknowledged breakpoint is never the execution point"
        );
        assert!(
            !stopped.is_at(DapBreakpointState::Disabled, 10),
            "and neither is a disabled one"
        );
        assert!(
            !DapExecutionPoint {
                line: 99,
                thread_id: None
            }
            .is_at(DapBreakpointState::Verified { line: 10 }, 10),
            "a verified breakpoint away from the stopped line is not the execution point"
        );

        // A response that is NOT a breakpoint answer leaves everything a request.
        let wrong = breakpoint_states(&asked, &up_to_date, &DapResponse::Threads(Vec::new()));
        assert!(wrong.iter().all(|state| !state.is_verified()));
    }
}
