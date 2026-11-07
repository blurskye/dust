//! DAP connections and protocol messages.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use std::collections::HashMap;

use std::io::BufReader;

use std::io::Read;

use std::path::Path;

use std::path::PathBuf;

use std::process::Command;

use std::process::Stdio;

use std::sync::atomic::Ordering;

use std::sync::mpsc;

use std::sync::mpsc::Receiver;

use std::thread;

use std::time::Duration;

use std::time::Instant;

use serde_json::Value;

use serde_json::json;
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
        stop_id: u64,
        request_id: u64,
        request: Option<DapControl>,
        response: DapResponse,
    },
    RequestFailed {
        adapter: String,
        stop_id: u64,
        request_seq: i64,
        request_id: u64,
        request: Option<DapControl>,
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
    Disconnected {
        terminate_debuggee: bool,
    },
    BreakpointChanged {
        path: PathBuf,
        requested: u64,
        verified: bool,
        line: Option<u64>,
        removed: bool,
    },
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
pub(crate) use reverse::DapReplyDelivery;
pub use reverse::DapReplyOutcome;
pub use reverse::DapTerminalReply;

mod session;
pub use session::DapClient;
pub(crate) use session::DapContext;
use session::Events;
pub(crate) use session::ExecutionScope;
use session::Writer;

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
pub const ADAPTER_BRIDGE_ARGUMENT: &str = "--adapter-bridge";
/// In a catalog entry's `launch_args`, where the socket address of a bridged adapter goes.
const ADAPTER_ADDRESS: &str = "{address}";
/// The same for an adapter that takes only the port of a local socket.
const ADAPTER_PORT: &str = "{port}";
/// In a bridged adapter's environment, asks for the adapter to run in a terminal, so the program
/// it launches reads and writes that terminal. VS Code runs Delve the same way.
pub(crate) const ADAPTER_IN_TERMINAL: &str = "DUST_ADAPTER_IN_TERMINAL";
/// How long an adapter gets to answer `initialize` before Dust says it is not working.
const ADAPTER_ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a stopping adapter gets to end its program and clean up before it is killed.
const ADAPTER_EXIT_GRACE: Duration = Duration::from_secs(2);
static ADAPTER_SESSION_LAUNCHER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Name the binary that can start adapters in their own session. `main` does this once.
pub fn use_adapter_session_launcher(binary: PathBuf) {
    let _ = ADAPTER_SESSION_LAUNCHER.set(binary);
}

/// Become a session leader and turn into `program`. This has no controlling terminal, so debugpy
/// cannot make its program the foreground group of /dev/tty, which would leave the editor unable
/// to read keys. Only returns if it failed.
/// Delve and CodeLLDB speak DAP only over a socket. Give them one and carry it over this
/// process's stdin and stdout, so the rest of Dust has one way to talk to an adapter.
#[cfg(unix)]
pub fn bridge_adapter_socket(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
) -> std::io::Result<()> {
    use std::os::fd::AsFd;
    nix::unistd::setsid()?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| {
            argument
                .to_string_lossy()
                .replace(ADAPTER_ADDRESS, &address.to_string())
                .replace(ADAPTER_PORT, &address.port().to_string())
        })
        .collect();
    let mut early = Vec::new();
    let in_terminal = std::env::var_os(ADAPTER_IN_TERMINAL).is_some()
        && adapter_opened_in_terminal(program, &arguments, &mut early)?;
    let mut adapter = if in_terminal {
        None
    } else {
        Some(
            Command::new(program)
                .args(&arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()?,
        )
    };
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + ADAPTER_ANSWER_TIMEOUT;
    let socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let exited = adapter
                    .as_mut()
                    .is_some_and(|adapter| !matches!(adapter.try_wait(), Ok(None)));
                if exited || Instant::now() >= deadline {
                    if let Some(adapter) = adapter.as_mut() {
                        let _ = adapter.kill();
                        let _ = adapter.wait();
                    }
                    return Err(std::io::Error::other("the adapter did not connect"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;
    let to_adapter = socket.try_clone()?;
    std::thread::spawn(move || {
        if std::io::Write::write_all(&mut &to_adapter, &early).is_err() {
            return;
        }
        relay_adapter_messages(std::io::stdin().lock(), &to_adapter);
        let _ = to_adapter.shutdown(std::net::Shutdown::Write);
    });
    // Not `io::stdout()`: its line buffering would hold back a DAP body, which ends with no newline.
    let stdout = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    relay_adapter_messages(&socket, stdout);
    // The connection closes before Delve has deleted its debug binary.
    if let Some(mut adapter) = adapter {
        if !adapter_exited_within(&mut adapter, ADAPTER_EXIT_GRACE / 2) {
            let _ = adapter.kill();
        }
        let _ = adapter.wait();
    }
    Ok(())
}
/// Ask Dust to run the adapter in a terminal, holding on to what Dust sends meanwhile, which is
/// for the adapter. False when Dust refused, and the adapter then runs without one.
#[cfg(unix)]
fn adapter_opened_in_terminal(
    program: &std::ffi::OsStr,
    arguments: &[String],
    early: &mut Vec<u8>,
) -> std::io::Result<bool> {
    use std::io::Write;
    use std::os::fd::AsFd;
    let args: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
        .chain(arguments.iter().cloned())
        .collect();
    let request = json!({
        "seq": 1,
        "type": "request",
        "command": "runInTerminal",
        "arguments": {"kind": "integrated", "cwd": std::env::current_dir()?, "args": args},
    })
    .to_string();
    let mut stdout = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    write!(stdout, "Content-Length: {}\r\n\r\n{request}", request.len())?;
    let mut stdin = std::io::stdin().lock();
    loop {
        let frame = read_adapter_frame(&mut stdin)?;
        let body = frame.split_at(frame.len() - frame_body_length(&frame)).1;
        let message: Value = serde_json::from_slice(body)?;
        if message.get("type").and_then(Value::as_str) == Some("response") {
            return Ok(message.get("success").and_then(Value::as_bool) == Some(true));
        }
        early.extend_from_slice(&frame);
    }
}
/// One whole DAP message as it arrived, headers included.
#[cfg(unix)]
fn read_adapter_frame(from: &mut impl std::io::BufRead) -> std::io::Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let start = frame.len();
        if from.read_until(b'\n', &mut frame)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if frame[start..].iter().all(u8::is_ascii_whitespace) {
            break;
        }
    }
    let start = frame.len();
    frame.resize(start + frame_body_length(&frame), 0);
    from.read_exact(&mut frame[start..])?;
    Ok(frame)
}
#[cfg(unix)]
fn frame_body_length(frame: &[u8]) -> usize {
    String::from_utf8_lossy(frame)
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix("Content-Length:")?.trim().parse().ok())
        .unwrap_or(0)
}
fn adapter_has_exited(adapter: &mut std::process::Child) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::wait::Id;
        use nix::sys::wait::WaitPidFlag;
        use nix::sys::wait::WaitStatus;
        use nix::sys::wait::waitid;
        matches!(waitid(Id::Pid(nix::unistd::Pid::from_raw(adapter.id() as i32)), WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT), Ok(status) if status != WaitStatus::StillAlive)
    }
    #[cfg(not(unix))]
    {
        !matches!(adapter.try_wait(), Ok(None))
    }
}
fn adapter_exited_within(adapter: &mut std::process::Child, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if adapter_has_exited(adapter) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
#[cfg(unix)]
fn relay_adapter_messages(mut from: impl std::io::Read, mut to: impl std::io::Write) {
    let mut chunk = [0; 64 * 1024];
    while let Ok(read) = from.read(&mut chunk) {
        if read == 0 || to.write_all(&chunk[..read]).is_err() {
            break;
        }
    }
}
#[cfg(unix)]
pub fn exec_in_own_session(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    if let Err(error) = nix::unistd::setsid() {
        return error.into();
    }
    Command::new(program).args(arguments).exec()
}

fn adapter_command(executable: PathBuf, arguments: Vec<String>) -> Command {
    match ADAPTER_SESSION_LAUNCHER
        .get()
        .filter(|binary| binary.is_file())
    {
        Some(binary) => {
            let mut command = Command::new(binary);
            let speaks_over_a_socket = arguments
                .iter()
                .any(|a| a.contains(ADAPTER_ADDRESS) || a.contains(ADAPTER_PORT));
            command
                .arg(if speaks_over_a_socket {
                    ADAPTER_BRIDGE_ARGUMENT
                } else {
                    ADAPTER_SESSION_ARGUMENT
                })
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
        breakpoints: initial_breakpoints,
    } = launch;
    if let Some(scope) = &scope
        && let Err(message) = scope.check()
    {
        events.fail(message);
        return;
    }
    let mut command = adapter_command(executable, arguments);
    command
        .env_clear()
        .envs(environment)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if events.actor.cancel.load(Ordering::Acquire) {
        return;
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            events.fail(error.to_string());
            return;
        }
    };
    struct Child(
        std::process::Child,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    );
    impl Drop for Child {
        fn drop(&mut self) {
            terminate_adapter(&mut self.0, self.1.load(Ordering::Acquire));
        }
    }
    let clean_disconnect = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut child = Child(child, clean_disconnect.clone());
    // Invalidate replies and contexts before potentially blocking child cleanup,
    // including unwind; rebinding transfers this guard's drop order.
    let _live = _live;
    let child = &mut child.0;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        events.fail("adapter pipes unavailable".into());
        return;
    };
    let (messages, receiver) = mpsc::sync_channel(32);
    let reader_events = events.clone();
    let reader = thread::Builder::new()
        .name("dust-dap-reader".into())
        .spawn(move || {
            let _resident = reader_events.clone();
            let Ok(stdout) =
                crate::process::CancellablePipe::new(stdout, reader_events.actor.cancel.clone())
            else {
                return;
            };
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
        events.fail(error.to_string());
        return;
    }
    // A pipe write cannot hold the controller hostage. Both I/O threads retain
    // resident leases, including while an inherited descriptor is stuck.
    let (mut stdin, writer) = match Writer::start(stdin, events.clone()) {
        Ok(writer) => writer,
        Err(error) => {
            events.fail(error.to_string());
            return;
        }
    };
    let stderr_events = events.clone();
    let stderr = child.stderr.take().and_then(|mut stderr| {
        thread::Builder::new()
            .name("dust-dap-stderr".into())
            .spawn(move || {
                #[cfg(unix)]
                {
                    use nix::fcntl::FcntlArg;
                    use nix::fcntl::OFlag;
                    use nix::fcntl::fcntl;
                    let Ok(flags) = fcntl(&stderr, FcntlArg::F_GETFL) else {
                        return;
                    };
                    if fcntl(
                        &stderr,
                        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                let mut bytes = [0; 4096];
                let mut remaining_after_cancel = 64_usize * 1024;
                loop {
                    match stderr.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) => {
                            if let Ok(mut captured) = stderr_events.actor.stderr.lock() {
                                captured.extend_from_slice(&bytes[..count]);
                                let discard = captured.len().saturating_sub(4096);
                                captured.drain(..discard);
                            }
                            if stderr_events.actor.cancel.load(Ordering::Acquire) {
                                remaining_after_cancel =
                                    remaining_after_cancel.saturating_sub(count);
                                if remaining_after_cancel == 0 {
                                    break;
                                }
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stderr_events.actor.cancel.load(Ordering::Acquire) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
            .ok()
    });
    let mut sequence = 1_i64;
    send_request(
        &mut stdin,
        sequence,
        "initialize",
        json!({
            "clientID":"dust", "clientName":"Dust", "adapterID":adapter,
            "pathFormat":"path", "linesStartAt1":true, "columnsStartAt1":true,
            "supportsVariablePaging":true, "supportsVariableType":true, "supportsRunInTerminalRequest":true
        }),
    );
    let initialize_sequence = sequence;
    sequence += 1;
    let mut launched = false;
    let started = Instant::now();
    let mut launch_accepted = false;
    let mut initialized = false;
    let mut configuration_sent = false;
    let mut initial_breakpoints = initial_breakpoints
        .into_iter()
        .map(|(path, rows)| (path, (0, rows)))
        .collect::<BTreeMap<_, _>>();
    let mut paused = false;
    let mut supports_configuration_done = false;
    let mut thread_id = None;
    let mut stopping = false;
    let mut debuggee_exited = false;
    let mut stop_id = 0_u64;
    let mut pending: HashMap<i64, (String, u64, u64, Option<DapControl>)> = HashMap::new();
    let mut breakpoint_ids = HashMap::<i64, (PathBuf, u64)>::new();
    let mut applied_breakpoints = HashMap::<PathBuf, i64>::new();
    let mut reverse = Vec::<reverse::ReverseRequest>::new();
    let mut last_reverse_sequence = 0;
    while !stopping && !events.actor.cancel.load(Ordering::Acquire) {
        if stdin.flush().is_err() {
            break;
        }
        if sequence > i64::MAX - 128 {
            break;
        }
        reverse.retain_mut(|request| !request.flush(&mut stdin, &mut sequence));
        for _ in 0..32 {
            if pending.len() >= 128 {
                break;
            }
            let Ok(control) = controls.try_recv() else {
                break;
            };
            if control.stop_id != events.actor.stop_id.load(Ordering::Acquire)
                && !matches!(
                    control.value,
                    DapControl::SetBreakpoints { .. }
                        | DapControl::Disconnect { .. }
                        | DapControl::ReverseResponse { .. }
                        | DapControl::Stop
                )
            {
                continue;
            }
            let request_id = control.request_id;
            let original = control.value.clone();
            let control = control.value;
            if let DapControl::ReverseResponse { request_seq, .. } = &control {
                if let Some(request) = reverse.iter_mut().find(|r| r.sequence == *request_seq) {
                    request.raw(control);
                }
                continue;
            }
            if let DapControl::SetBreakpoints { path, breakpoints } = &control
                && !configuration_sent
            {
                initial_breakpoints.insert(path.clone(), (request_id, breakpoints.clone()));
                continue;
            }
            if matches!(control, DapControl::Pause) && thread_id.is_none() {
                send_request(&mut stdin, sequence, "threads", json!({}));
                pending.insert(
                    sequence,
                    ("threads".into(), stop_id, request_id, Some(original)),
                );
                sequence += 1;
                continue;
            }
            if sequence > i64::MAX - 8 {
                events.fail("DAP request capacity or sequence exhausted".into());
                stopping = true;
                break;
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
                pending.insert(
                    sequence,
                    (command.into(), stop_id, request_id, Some(original)),
                );
                sequence += 1;
            }
        }
        // Keep startup writes incremental and wait for breakpoint acknowledgements before
        // configurationDone; adapters may process requests asynchronously.
        if initialized
            && !configuration_sent
            && !pending
                .values()
                .any(|(command, _, _, _)| command == "setBreakpoints")
            && pending.len() < 128
        {
            if let Some((path, (request_id, rows))) = initial_breakpoints.pop_first() {
                let original = DapControl::SetBreakpoints {
                    path: path.clone(),
                    breakpoints: rows.clone(),
                };
                send_request(
                    &mut stdin,
                    sequence,
                    "setBreakpoints",
                    set_breakpoints_arguments(&path, rows, &path_maps),
                );
                pending.insert(
                    sequence,
                    ("setBreakpoints".into(), stop_id, request_id, Some(original)),
                );
                sequence += 1;
            } else {
                if supports_configuration_done {
                    send_request(&mut stdin, sequence, "configurationDone", json!({}));
                    pending.insert(sequence, ("configurationDone".into(), stop_id, 0, None));
                    sequence += 1;
                }
                configuration_sent = true;
            }
        }
        if !launched && started.elapsed() >= ADAPTER_ANSWER_TIMEOUT {
            events.fail("it did not answer; check that it works from a terminal".into());
            break;
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
                        events.fail("DAP initialize rejected".into());
                        break;
                    }
                    events.actor.supports_terminate.store(
                        message
                            .pointer("/body/supportTerminateDebuggee")
                            .and_then(Value::as_bool)
                            == Some(true),
                        Ordering::Release,
                    );
                    supports_configuration_done = message
                        .pointer("/body/supportsConfigurationDoneRequest")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if let Some(scope) = &scope
                        && let Err(message) = scope.check()
                    {
                        events.fail(message);
                        break;
                    }
                    send_request(&mut stdin, sequence, &request, request_arguments.clone());
                    pending.insert(sequence, (request.clone(), stop_id, 0, None));
                    sequence += 1;
                    launched = true;
                }
                Some("response") => {
                    if let Some(request_seq) = message.get("request_seq").and_then(Value::as_i64)
                        && pending.get(&request_seq).is_some_and(|(command, _, _, _)| {
                            message.get("command").and_then(Value::as_str) == Some(command.as_str())
                        })
                        && let Some((command, response_stop_id, request_id, original)) =
                            pending.remove(&request_seq)
                        && (response_stop_id == stop_id || is_session_request(&command))
                    {
                        if message.get("success").and_then(Value::as_bool) != Some(true) {
                            let detail = rejection_detail(&message);
                            if is_startup_request(&command) {
                                events.fail(detail);
                                break;
                            }
                            events
                                .send(DapEvent::RequestFailed {
                                    adapter: adapter.clone(),
                                    stop_id: response_stop_id,
                                    request_seq,
                                    request_id,
                                    request: original.clone(),
                                    message: format!("DAP {command} request rejected: {detail}"),
                                    command: if matches!(original, Some(DapControl::Pause)) {
                                        "pause".into()
                                    } else {
                                        command
                                    },
                                })
                                .ok();
                            continue;
                        }
                        if matches!(command.as_str(), "launch" | "attach") {
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
                            // Do not invalidate stopped data until the adapter accepts the resume.
                            // A preceding continued event has already invalidated it.
                            if paused {
                                let Some(next) = stop_id.checked_add(1) else {
                                    break;
                                };
                                stop_id = next;
                                events.actor.stop_id.store(next, Ordering::Release);
                                pending.retain(|_, (command, _, _, _)| is_session_request(command));
                            }
                            paused = false;
                            if launch_accepted {
                                events
                                    .send(DapEvent::Running {
                                        adapter: adapter.clone(),
                                    })
                                    .ok();
                            }
                        }
                        if let Some(DapControl::SetBreakpoints { path, breakpoints }) =
                            original.as_ref()
                            && applied_breakpoints
                                .get(path)
                                .is_none_or(|applied| request_seq > *applied)
                        {
                            applied_breakpoints.insert(path.clone(), request_seq);
                            breakpoint_ids.retain(|_, (old, _)| old != path);
                            if let Some(rows) = message
                                .pointer("/body/breakpoints")
                                .and_then(Value::as_array)
                            {
                                for (asked, row) in breakpoints.iter().zip(rows) {
                                    if let Some(id) = row.get("id").and_then(Value::as_i64) {
                                        breakpoint_ids.insert(id, (path.clone(), asked.line));
                                    }
                                }
                            }
                        }
                        if command == "threads" {
                            let threads =
                                message.pointer("/body/threads").and_then(Value::as_array);
                            thread_id = thread_id
                                .filter(|id| {
                                    threads.is_some_and(|rows| {
                                        rows.iter().any(|row| {
                                            row.get("id").and_then(Value::as_i64) == Some(*id)
                                        })
                                    })
                                })
                                .or_else(|| threads?.first()?.get("id")?.as_i64());
                            if matches!(original, Some(DapControl::Pause)) {
                                if let Some(thread_id) = thread_id {
                                    send_request(
                                        &mut stdin,
                                        sequence,
                                        "pause",
                                        json!({"threadId":thread_id}),
                                    );
                                    pending.insert(
                                        sequence,
                                        ("pause".into(), stop_id, request_id, original.clone()),
                                    );
                                    sequence += 1;
                                } else {
                                    events
                                        .send(DapEvent::RequestFailed {
                                            adapter: adapter.clone(),
                                            stop_id,
                                            request_seq,
                                            request_id,
                                            request: original.clone(),
                                            command: "pause".into(),
                                            message: "adapter reported no threads to pause".into(),
                                        })
                                        .ok();
                                }
                            }
                        }
                        if command == "disconnect"
                            && let Some(DapControl::Disconnect { terminate_debuggee }) =
                                original.as_ref()
                        {
                            events
                                .send(DapEvent::Response {
                                    adapter: adapter.clone(),
                                    stop_id: response_stop_id,
                                    request_id,
                                    request: original.clone(),
                                    response: DapResponse::Disconnected {
                                        terminate_debuggee: *terminate_debuggee,
                                    },
                                })
                                .ok();
                            clean_disconnect.store(true, Ordering::Release);
                            stopping = true;
                        }
                        if let Some(response) = normalize_response(
                            &command,
                            message.get("body").unwrap_or(&Value::Null),
                            &path_maps,
                        ) {
                            events
                                .send(DapEvent::Response {
                                    adapter: adapter.clone(),
                                    stop_id: response_stop_id,
                                    request_id,
                                    request: original.clone(),
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
                                    stop_id: response_stop_id,
                                    request_seq,
                                    request_id,
                                    request: original.clone(),
                                    command,
                                    message: "DAP response body is missing or malformed".into(),
                                })
                                .ok();
                        }
                    }
                }
                Some("event") => {
                    let event = message
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match event {
                        "breakpoint" => {
                            let row = &message["body"]["breakpoint"];
                            if let Some((path, requested)) = row
                                .get("id")
                                .and_then(Value::as_i64)
                                .and_then(|id| breakpoint_ids.get(&id))
                                .cloned()
                            {
                                events
                                    .send(DapEvent::Response {
                                        adapter: adapter.clone(),
                                        stop_id,
                                        request_id: 0,
                                        request: None,
                                        response: DapResponse::BreakpointChanged {
                                            path,
                                            requested,
                                            verified: row.get("verified").and_then(Value::as_bool)
                                                == Some(true),
                                            line: row.get("line").and_then(Value::as_u64),
                                            removed: message
                                                .pointer("/body/reason")
                                                .and_then(Value::as_str)
                                                == Some("removed"),
                                        },
                                    })
                                    .ok();
                            }
                        }
                        "initialized" => initialized = true,
                        "stopped" => {
                            let Some(next) = stop_id.checked_add(1) else {
                                break;
                            };
                            stop_id = next;
                            events.actor.stop_id.store(next, Ordering::Release);
                            paused = true;
                            // Startup belongs to this actor, not to one stopped context.
                            pending.retain(|_, (command, _, _, _)| is_session_request(command));
                            thread_id = message.pointer("/body/threadId").and_then(Value::as_i64);
                            let reason = message
                                .pointer("/body/reason")
                                .and_then(Value::as_str)
                                .unwrap_or("stopped")
                                .to_owned();
                            send_request(&mut stdin, sequence, "threads", json!({}));
                            pending.insert(
                                sequence,
                                ("threads".into(), stop_id, 0, Some(DapControl::Threads)),
                            );
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
                            if message
                                .pointer("/body/allThreadsContinued")
                                .and_then(Value::as_bool)
                                == Some(false)
                                && let (Some(stopped), Some(resumed)) = (
                                    thread_id,
                                    message.pointer("/body/threadId").and_then(Value::as_i64),
                                )
                                && stopped != resumed
                            {
                                continue;
                            }
                            let Some(next) = stop_id.checked_add(1) else {
                                break;
                            };
                            stop_id = next;
                            events.actor.stop_id.store(next, Ordering::Release);
                            paused = false;
                            pending.retain(|_, (command, _, _, _)| is_session_request(command));
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
                        "exited" => debuggee_exited = true,
                        "terminated" => {
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
                    reverse.retain_mut(|request| !request.flush(&mut stdin, &mut sequence));
                    if request_seq <= last_reverse_sequence || reverse.len() >= 32 {
                        events.fail("DAP reverse request identity reused or queue full".into());
                        break;
                    }
                    last_reverse_sequence = request_seq;
                    if command == "runInTerminal" {
                        // Reserve the reply mailbox before copying arguments and environment.
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
                events.fail(message);
                stopping = true;
            }
            Err(error) => {
                // Drain protocol messages before reacting to child exit. A descendant may
                // retain stdout, so an exited adapter with an idle reader must also finish.
                if error == mpsc::RecvTimeoutError::Disconnected || adapter_has_exited(child) {
                    if debuggee_exited {
                        events
                            .send(DapEvent::Terminated {
                                adapter: adapter.clone(),
                            })
                            .ok();
                    }
                    stopping = true;
                }
            }
        }
        if writer.is_finished() && !events.actor.cancel.load(Ordering::Acquire) {
            live.store(false, Ordering::Release);
            events.fail("DAP writer stopped; pending write outcomes unknown".into());
            stopping = true;
        }
    }
    // Out-of-band cancellation is not a successful protocol disconnect.
    // Controller owns kill/wait; no foreground thread waits for this cleanup.
    live.store(false, Ordering::Release);
    events.actor.cancel.store(true, Ordering::Release);
    // Closing its input ends the session; the adapter gets a moment to clean up (see `terminate_adapter`).
    drop(stdin);
    drop(reader);
    drop(writer);
    if let Some(stderr) = stderr {
        let _ = stderr.join();
    }
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

fn is_session_request(command: &str) -> bool {
    is_startup_request(command) || matches!(command, "setBreakpoints" | "disconnect")
}

fn is_startup_request(command: &str) -> bool {
    matches!(command, "launch" | "attach" | "configurationDone")
}

/// Why a request was refused. Delve's `message` is only "Failed to launch"; the reason is in
/// `body.error.format`.
fn rejection_detail(response: &Value) -> String {
    let format = response
        .pointer("/body/error/format")
        .and_then(Value::as_str);
    match format.filter(|format| !format.is_empty()) {
        Some(format) => {
            let variables = response
                .pointer("/body/error/variables")
                .and_then(Value::as_object);
            // Interpret only the original template, never placeholders in variable values.
            let filled: String = format
                .split_inclusive('}')
                .flat_map(|part| {
                    let replacement = part
                        .strip_suffix('}')
                        .and_then(|part| part.rsplit_once('{'))
                        .and_then(|(prefix, name)| {
                            variables?
                                .get(name)
                                .map(|value| (prefix, value.as_str().unwrap_or_default()))
                        });
                    let (prefix, value) = replacement.unwrap_or(("", part));
                    prefix.chars().chain(value.chars())
                })
                .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
                .take(4097)
                .collect();
            sanitize_output(&filled)
        }
        None => bounded(response.get("message")),
    }
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
        "variables" if body.get("variables")?.as_array()?.len() > 10_000 => None,
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
            value: sanitize_output(body.get("result")?.as_str()?),
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
                content: content.to_owned(),
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
fn terminate_adapter(child: &mut std::process::Child, clean: bool) {
    // Share the shutdown grace between closing stdin and escalating group signals.
    let grace = if clean {
        ADAPTER_EXIT_GRACE
    } else {
        ADAPTER_EXIT_GRACE / 2
    };
    if adapter_exited_within(child, grace) && clean {
        let _ = child.wait();
        return;
    }
    #[cfg(unix)]
    if !clean {
        let group = nix::unistd::Pid::from_raw(child.id() as i32);
        if nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGTERM).is_ok() {
            // The adapter may already be gone while a debuggee still owns the group.
            thread::sleep(grace);
            let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
        }
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
    let mut characters = output
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'));
    let mut result: String = characters.by_ref().take(4096).collect();
    if characters.next().is_some() {
        result.push_str("\n[output truncated]\n");
    }
    result
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
    #[error("debug adapter queue is full")]
    Full,
    #[error("debug adapter source or stopped context is stale")]
    StaleContext,
    #[error("failed to create debug-adapter thread: {0}")]
    Thread(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use crate::debugger::DapResponse;
    use crate::debugger::normalize_response;
    use crate::debugger::sanitize_output;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn adapter_output_is_bounded_and_terminal_sanitized() {
        assert_eq!(sanitize_output("ok\u{1b}[31m"), "ok[31m");
        assert_eq!(sanitize_output(&"x".repeat(4096)), "x".repeat(4096));
        assert_eq!(
            sanitize_output(&"x".repeat(8000)),
            format!("{}\n[output truncated]\n", "x".repeat(4096))
        );
        assert_eq!(
            sanitize_output(&"é".repeat(4097)).chars().count(),
            4096 + "\n[output truncated]\n".len()
        );
    }

    #[test]
    fn evaluation_requires_a_string_result() {
        for body in [json!({}), json!({"result":null}), json!({"result":42})] {
            assert!(normalize_response("evaluate", &body, &BTreeMap::new()).is_none());
        }
        for result in ["", "answer"] {
            let Some(DapResponse::Evaluate(value)) =
                normalize_response("evaluate", &json!({"result":result}), &BTreeMap::new())
            else {
                panic!("valid evaluation rejected");
            };
            assert_eq!(value.value, result);
        }
    }

    #[test]
    fn source_content_preserves_control_characters_and_locations() {
        let content = "const text = 'a\u{1b}\0b';\n";
        let Some(DapResponse::Source {
            content: actual, ..
        }) = normalize_response("source", &json!({"content":content}), &BTreeMap::new())
        else {
            panic!("valid source rejected");
        };
        assert_eq!(actual, content);
    }
}

#[cfg(test)]
pub(crate) fn terminal_reply_source_fixture() {
    reverse::fixture();
}

#[cfg(test)]
pub(crate) use session::TestAdapter;
#[cfg(test)]
pub(crate) use session::fixture as test_session;
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
#[cfg(test)]
pub(crate) use transport_tests::ADAPTER as TEST_ADAPTER;
#[cfg(test)]
pub(crate) use transport_tests::LOGGING_ADAPTER as TEST_LOGGING_ADAPTER;
#[cfg(test)]
pub(crate) use transport_tests::python as test_python;

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
    let mut verdict = 0;
    requested
        .iter()
        .enumerate()
        .map(|(index, requested)| {
            if disabled.get(index).copied().unwrap_or(false) {
                return DapBreakpointState::Disabled;
            }
            let row = rows.get(verdict);
            verdict += 1;
            match row {
                Some((true, line, _)) => DapBreakpointState::Verified {
                    // The adapter may move a breakpoint; the VERIFIED line is its line, falling
                    // back to the requested one when it reports none.
                    line: line.unwrap_or(requested.line),
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
    use crate::debugger::disconnect_arguments;

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
    use crate::debugger::DapBreakpointState;
    use crate::debugger::DapExecutionPoint;
    use crate::debugger::DapResponse;
    use crate::debugger::DapSourceBreakpoint;
    use crate::debugger::breakpoint_states;
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
    fn verified_breakpoints_without_lines_use_the_requested_enabled_line() {
        let states = breakpoint_states(
            &requested(&[10, 20, 30]),
            &[true, false, false],
            &DapResponse::Breakpoints(vec![(true, None, None), (true, Some(31), None)]),
        );
        assert_eq!(
            states,
            vec![
                DapBreakpointState::Disabled,
                DapBreakpointState::Verified { line: 20 },
                DapBreakpointState::Verified { line: 31 },
            ]
        );
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
        assert_eq!(
            partial[1],
            DapBreakpointState::Requested { accepted: false }
        );
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
    #[test]
    fn verified_breakpoints_without_lines_keep_the_requested_locations() {
        let asked = requested(&[10, 20, 30]);
        let states = breakpoint_states(
            &asked,
            &[true, false, false],
            &DapResponse::Breakpoints(vec![(true, None, None), (true, Some(32), None)]),
        );
        assert_eq!(states[0], DapBreakpointState::Disabled);
        assert_eq!(states[1], DapBreakpointState::Verified { line: 20 });
        assert_eq!(states[2], DapBreakpointState::Verified { line: 32 });
    }

    #[test]
    fn disabled_rows_do_not_consume_enabled_breakpoint_verdicts() {
        let rows = [10, 20, 30].map(|line| DapSourceBreakpoint {
            path: "/source".into(),
            line,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        });
        let states = breakpoint_states(
            &rows,
            &[true, false, false],
            &DapResponse::Breakpoints(vec![(false, Some(20), None), (true, Some(30), None)]),
        );
        assert_eq!(states[0], DapBreakpointState::Disabled);
        assert!(!states[1].is_verified());
        assert_eq!(states[2], DapBreakpointState::Verified { line: 30 });
    }
}
