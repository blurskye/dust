#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap},
    io::BufReader,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::{Value, json};
use thiserror::Error;

use crate::process::{read_content_length_message, write_content_length_message};

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
    RunInTerminal {
        adapter: String,
        request_seq: i64,
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

struct DapSession {
    sender: Sender<DapControl>,
    thread: Option<JoinHandle<()>>,
}

pub struct DapLaunch {
    pub adapter: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: HashMap<String, String>,
    pub path_maps: BTreeMap<PathBuf, PathBuf>,
    pub root: PathBuf,
    pub request: String,
    pub request_arguments: Value,
}

pub struct DapService {
    events: Sender<DapEvent>,
    receiver: Receiver<DapEvent>,
    session: Option<(String, DapSession)>,
}

impl DapService {
    #[must_use]
    pub fn new() -> Self {
        let (events, receiver) = mpsc::channel();
        Self {
            events,
            receiver,
            session: None,
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.session.is_some()
    }

    pub fn start_absolute(&mut self, launch: DapLaunch) -> Result<(), DapError> {
        if self.session.is_some() {
            return Err(DapError::AlreadyRunning);
        }
        if !launch.executable.is_absolute()
            || !launch.root.is_absolute()
            || !matches!(launch.request.as_str(), "launch" | "attach")
        {
            return Err(DapError::InvalidConfiguration);
        }
        let (sender, commands) = mpsc::channel();
        let events = self.events.clone();
        events
            .send(DapEvent::Starting {
                adapter: launch.adapter.clone(),
            })
            .ok();
        let session_id = launch.adapter.clone();
        let thread = thread::Builder::new()
            .name(format!("dust-dap-{}", launch.adapter))
            .spawn(move || run_adapter(launch, commands, events))
            .map_err(DapError::Thread)?;
        self.session = Some((
            session_id,
            DapSession {
                sender,
                thread: Some(thread),
            },
        ));
        Ok(())
    }

    pub fn control(&self, control: DapControl) -> Result<(), DapError> {
        self.session
            .as_ref()
            .ok_or(DapError::NotRunning)?
            .1
            .sender
            .send(control)
            .map_err(|_| DapError::NotRunning)
    }

    pub fn stop(&mut self) -> Result<(), DapError> {
        let Some((_, mut session)) = self.session.take() else {
            return Err(DapError::NotRunning);
        };
        session.sender.send(DapControl::Stop).ok();
        if let Some(thread) = session.thread.take() {
            let _ = thread.join();
        }
        Ok(())
    }

    #[must_use]
    pub fn active_adapter(&self) -> Option<&str> {
        self.session.as_ref().map(|(adapter, _)| adapter.as_str())
    }

    pub fn try_event(&self) -> Option<DapEvent> {
        self.receiver.try_recv().ok()
    }
}

impl Default for DapService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DapService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_adapter(launch: DapLaunch, controls: Receiver<DapControl>, events: Sender<DapEvent>) {
    let DapLaunch {
        adapter,
        executable,
        arguments,
        environment,
        path_maps,
        root,
        request,
        request_arguments,
    } = launch;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env_clear()
        .envs(environment)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
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
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        events
            .send(DapEvent::Failed {
                adapter,
                message: "adapter pipes unavailable".into(),
            })
            .ok();
        terminate_adapter(&mut child);
        return;
    };
    let (messages, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_content_length_message(&mut reader) {
                Ok(Some(value)) => {
                    if messages.send(Ok(value)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    messages.send(Err(error.to_string())).ok();
                    break;
                }
            }
        }
    });
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
    let mut supports_configuration_done = false;
    let mut thread_id = None;
    let mut stopping = false;
    let mut stop_generation = 0_u64;
    let mut pending: HashMap<i64, (String, u64)> = HashMap::new();
    while !stopping {
        while let Ok(control) = controls.try_recv() {
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
                DapControl::ReverseResponse {
                    request_seq,
                    success,
                    message,
                    process_id,
                } => {
                    let response = json!({"seq":sequence,"type":"response","request_seq":request_seq,"success":success,"command":"runInTerminal","message":message,"body":{"processId":process_id}});
                    let _ = write_content_length_message(&mut stdin, &response);
                    sequence += 1;
                    (None, json!({}))
                }
                DapControl::SetBreakpoints { path, breakpoints } => {
                    let path = map_path(&path, &path_maps, false);
                    (
                        Some("setBreakpoints"),
                        json!({"source":{"path":path},"breakpoints":breakpoints.into_iter().take(10_000).map(|item|json!({"line":item.line,"column":item.column,"condition":item.condition,"hitCondition":item.hit_condition,"logMessage":item.log_message})).collect::<Vec<_>>()}),
                    )
                }
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
                        && !launched =>
                {
                    supports_configuration_done = message
                        .pointer("/body/supportsConfigurationDoneRequest")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    send_request(&mut stdin, sequence, &request, request_arguments.clone());
                    sequence += 1;
                    launched = true;
                    events
                        .send(DapEvent::Running {
                            adapter: adapter.clone(),
                        })
                        .ok();
                }
                Some("response") => {
                    if let Some(request_seq) = message.get("request_seq").and_then(Value::as_i64)
                        && let Some((command, generation)) = pending.remove(&request_seq)
                        && generation == stop_generation
                    {
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
                        }
                    }
                }
                Some("event") => {
                    let event = message
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match event {
                        "initialized" if supports_configuration_done => {
                            send_request(&mut stdin, sequence, "configurationDone", json!({}));
                            sequence += 1;
                        }
                        "initialized" => {}
                        "stopped" => {
                            stop_generation = stop_generation.saturating_add(1);
                            pending.retain(|_, (_, generation)| *generation == stop_generation);
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
                            events
                                .send(DapEvent::Running {
                                    adapter: adapter.clone(),
                                })
                                .ok();
                        }
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
                    if command == "runInTerminal" {
                        let cwd = message
                            .pointer("/arguments/cwd")
                            .and_then(Value::as_str)
                            .map(PathBuf::from);
                        let arguments = message
                            .pointer("/arguments/args")
                            .and_then(Value::as_array)
                            .map(|values| {
                                values
                                    .iter()
                                    .take(4096)
                                    .filter_map(Value::as_str)
                                    .map(|value| value.chars().take(64 * 1024).collect())
                                    .collect::<Vec<_>>()
                            });
                        let environment = message
                            .pointer("/arguments/env")
                            .and_then(Value::as_object)
                            .map(|values| {
                                values
                                    .iter()
                                    .take(4096)
                                    .filter_map(|(key, value)| {
                                        value.as_str().map(|value| {
                                            (
                                                key.chars().take(1024).collect(),
                                                value.chars().take(64 * 1024).collect(),
                                            )
                                        })
                                    })
                                    .collect::<HashMap<_, _>>()
                            });
                        if let (Some(cwd), Some(arguments), Some(environment)) =
                            (cwd, arguments, environment)
                        {
                            events
                                .send(DapEvent::RunInTerminal {
                                    adapter: adapter.clone(),
                                    request_seq,
                                    cwd,
                                    arguments,
                                    environment,
                                })
                                .ok();
                        } else {
                            let response = json!({"seq":sequence,"type":"response","request_seq":request_seq,"success":false,"command":command,"message":"invalid bounded runInTerminal arguments"});
                            let _ = write_content_length_message(&mut stdin, &response);
                            sequence += 1;
                        }
                    } else {
                        let response = json!({"seq":sequence,"type":"response","request_seq":request_seq,"success":false,"command":command,"message":"unsupported reverse request"});
                        let _ = write_content_length_message(&mut stdin, &response);
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
    }
    send_request(
        &mut stdin,
        sequence,
        "disconnect",
        json!({"terminateDebuggee":true}),
    );
    terminate_adapter(&mut child);
    let _ = reader.join();
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

fn send_request(writer: &mut impl std::io::Write, seq: i64, command: &str, arguments: Value) {
    let _ = write_content_length_message(
        writer,
        &json!({
            "seq":seq, "type":"request", "command":command, "arguments":arguments
        }),
    );
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
