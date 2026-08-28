#![forbid(unsafe_code)]

use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(pub u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobStream {
    Stdout,
    Stderr,
}
#[derive(Clone, Debug)]
pub struct JobPolicy {
    pub maximum_output_bytes: usize,
    pub maximum_line_bytes: usize,
    pub maximum_stdin_bytes: usize,
    pub timeout: Duration,
}
impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            maximum_output_bytes: 16 * 1024 * 1024,
            maximum_line_bytes: 1024 * 1024,
            maximum_stdin_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(300),
        }
    }
}
#[derive(Clone, Debug)]
pub struct JobRequest {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub root_revision: u64,
    pub stdin: Option<Vec<u8>>,
    pub policy: JobPolicy,
}
#[derive(Clone, Debug)]
pub enum JobEvent {
    Started {
        job: JobId,
        root_revision: u64,
    },
    Output {
        job: JobId,
        stream: JobStream,
        bytes: Vec<u8>,
        truncated: bool,
    },
    Exited {
        job: JobId,
        code: Option<i32>,
        timed_out: bool,
    },
    Failed {
        job: JobId,
        message: String,
    },
}
struct RunningJob {
    child: Child,
    started: Instant,
    timeout: Duration,
}
pub struct JobSupervisor {
    next: u64,
    jobs: HashMap<JobId, RunningJob>,
    sender: SyncSender<JobEvent>,
    receiver: Receiver<JobEvent>,
}
impl JobSupervisor {
    #[must_use]
    pub fn new(event_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(event_capacity.clamp(1, 4096));
        Self {
            next: 1,
            jobs: HashMap::new(),
            sender,
            receiver,
        }
    }
    pub fn start(&mut self, request: JobRequest) -> Result<JobId, JobError> {
        let argument_bytes = request.arguments.iter().map(String::len).sum::<usize>();
        let environment_bytes = request
            .environment
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .sum::<usize>();
        if !request.program.is_absolute()
            || !request.cwd.is_absolute()
            || !request.cwd.is_dir()
            || request.arguments.len() > 4096
            || request.environment.len() > 4096
            || argument_bytes > 1024 * 1024
            || environment_bytes > 1024 * 1024
            || request.policy.maximum_output_bytes == 0
            || request.policy.maximum_output_bytes > 256 * 1024 * 1024
            || request.policy.maximum_line_bytes == 0
            || request.policy.maximum_line_bytes > 16 * 1024 * 1024
            || request.policy.maximum_stdin_bytes > 256 * 1024 * 1024
            || request
                .stdin
                .as_ref()
                .is_some_and(|bytes| bytes.len() > request.policy.maximum_stdin_bytes)
            || request.policy.timeout.is_zero()
            || request.policy.timeout > Duration::from_secs(86_400)
        {
            return Err(JobError::InvalidPolicy);
        }
        let id = JobId(self.next);
        self.next = self.next.saturating_add(1);
        let mut command = Command::new(&request.program);
        command
            .args(&request.arguments)
            .current_dir(&request.cwd)
            .env_clear()
            .envs(&request.environment)
            .stdin(if request.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(JobError::Spawn)?;
        if let Some(bytes) = request.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                thread::spawn(move || {
                    let _ = stdin.write_all(&bytes);
                });
            }
        }
        let total = Arc::new(AtomicUsize::new(0));
        if let Some(stdout) = child.stdout.take() {
            stream_reader(
                id,
                JobStream::Stdout,
                stdout,
                self.sender.clone(),
                total.clone(),
                request.policy.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            stream_reader(
                id,
                JobStream::Stderr,
                stderr,
                self.sender.clone(),
                total,
                request.policy.clone(),
            );
        }
        self.jobs.insert(
            id,
            RunningJob {
                child,
                started: Instant::now(),
                timeout: request.policy.timeout,
            },
        );
        let _ = self.sender.try_send(JobEvent::Started {
            job: id,
            root_revision: request.root_revision,
        });
        Ok(id)
    }
    pub fn poll(&mut self) -> Vec<JobEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            events.push(event)
        }
        let now = Instant::now();
        let done = self
            .jobs
            .iter_mut()
            .filter_map(|(id, running)| {
                let timed_out = now.duration_since(running.started) >= running.timeout;
                if timed_out {
                    terminate_tree(&mut running.child);
                }
                match running.child.try_wait() {
                    Ok(Some(status)) => Some((*id, status.code(), timed_out)),
                    Err(error) => {
                        let _ = self.sender.try_send(JobEvent::Failed {
                            job: *id,
                            message: error.to_string(),
                        });
                        Some((*id, None, timed_out))
                    }
                    Ok(None) => None,
                }
            })
            .collect::<Vec<_>>();
        for (id, code, timed_out) in done {
            self.jobs.remove(&id);
            events.push(JobEvent::Exited {
                job: id,
                code,
                timed_out,
            });
        }
        events
    }
    pub fn cancel(&mut self, id: JobId) -> Result<(), JobError> {
        let running = self.jobs.get_mut(&id).ok_or(JobError::Unknown(id))?;
        terminate_tree(&mut running.child);
        Ok(())
    }
    #[must_use]
    pub fn is_running(&self, id: JobId) -> bool {
        self.jobs.contains_key(&id)
    }
}
fn stream_reader(
    id: JobId,
    stream: JobStream,
    reader: impl Read + Send + 'static,
    sender: SyncSender<JobEvent>,
    total: Arc<AtomicUsize>,
    policy: JobPolicy,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut bytes = Vec::new();
            let Ok(read) = reader
                .by_ref()
                .take(policy.maximum_line_bytes.saturating_add(1) as u64)
                .read_until(b'\n', &mut bytes)
            else {
                break;
            };
            if read == 0 {
                break;
            }
            let line_truncated = bytes.len() > policy.maximum_line_bytes;
            bytes.truncate(policy.maximum_line_bytes);
            let allowed = reserve_output(&total, policy.maximum_output_bytes, bytes.len());
            bytes.truncate(allowed);
            let truncated = line_truncated || bytes.len() < read || allowed < read;
            if !bytes.is_empty() {
                let _ = sender.try_send(JobEvent::Output {
                    job: id,
                    stream,
                    bytes,
                    truncated,
                });
            }
            if truncated && allowed == 0 {
                break;
            }
        }
    });
}
fn reserve_output(total: &AtomicUsize, ceiling: usize, requested: usize) -> usize {
    let mut current = total.load(Ordering::Relaxed);
    loop {
        let allowed = requested.min(ceiling.saturating_sub(current));
        match total.compare_exchange_weak(
            current,
            current.saturating_add(allowed),
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return allowed,
            Err(actual) => current = actual,
        }
    }
}

impl Drop for JobSupervisor {
    fn drop(&mut self) {
        for running in self.jobs.values_mut() {
            terminate_tree(&mut running.child);
            let _ = running.child.wait();
        }
    }
}

fn terminate_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let _ = child.kill();
}
#[derive(Debug, Error)]
pub enum JobError {
    #[error("job policy or request is invalid")]
    InvalidPolicy,
    #[error("cannot spawn job: {0}")]
    Spawn(#[source] io::Error),
    #[error("unknown job {0:?}")]
    Unknown(JobId),
}

pub const PROTOCOL_MESSAGE_BYTE_CEILING: usize = 16 * 1024 * 1024;
const PROTOCOL_HEADER_BYTE_CEILING: usize = 16 * 1024;

pub fn read_content_length_message(
    reader: &mut impl BufRead,
) -> Result<Option<Value>, ProtocolError> {
    let mut content_length = None;
    let mut header_bytes = 0_usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(ProtocolError::Io)?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(ProtocolError::UnexpectedEof)
            };
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > PROTOCOL_HEADER_BYTE_CEILING {
            return Err(ProtocolError::HeadersTooLarge);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| ProtocolError::InvalidContentLength)?,
            );
        }
    }
    let content_length = content_length.ok_or(ProtocolError::MissingContentLength)?;
    if content_length > PROTOCOL_MESSAGE_BYTE_CEILING {
        return Err(ProtocolError::MessageTooLarge(content_length));
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).map_err(ProtocolError::Io)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(ProtocolError::Json)
}

pub fn write_content_length_message(
    writer: &mut impl Write,
    value: &Value,
) -> Result<(), ProtocolError> {
    let body = serde_json::to_vec(value).map_err(ProtocolError::Json)?;
    if body.len() > PROTOCOL_MESSAGE_BYTE_CEILING {
        return Err(ProtocolError::MessageTooLarge(body.len()));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).map_err(ProtocolError::Io)?;
    writer.write_all(&body).map_err(ProtocolError::Io)?;
    writer.flush().map_err(ProtocolError::Io)
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("protocol I/O failed: {0}")]
    Io(#[source] io::Error),
    #[error("protocol stream ended in the middle of headers")]
    UnexpectedEof,
    #[error("protocol headers exceed the safety ceiling")]
    HeadersTooLarge,
    #[error("protocol message is missing Content-Length")]
    MissingContentLength,
    #[error("protocol Content-Length is invalid")]
    InvalidContentLength,
    #[error("protocol message length {0} exceeds the safety ceiling")]
    MessageTooLarge(usize),
    #[error("protocol JSON is invalid: {0}")]
    Json(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::{read_content_length_message, write_content_length_message};
    #[cfg(unix)]
    #[test]
    fn preflight_pins_symlinked_executable_and_cwd_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let executable = root.join("server-real");
        std::fs::write(&executable, b"#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let executable_link = temp.path().join("server");
        let cwd_link = temp.path().join("cwd");
        symlink(&executable, &executable_link).unwrap();
        symlink(&root, &cwd_link).unwrap();
        let result = super::preflight(
            super::PreflightId(1),
            super::ExecutionPreflightRequest {
                executable: executable_link,
                cwd: cwd_link,
                roots: vec![root.clone()],
                root_revision: 7,
            },
        )
        .unwrap();
        assert_eq!(
            result.executable,
            std::fs::canonicalize(executable).unwrap()
        );
        assert_eq!(result.cwd, std::fs::canonicalize(&root).unwrap());
        assert_eq!(result.root_revision, 7);
    }

    #[test]
    fn framing_handles_fragment_equivalent_and_coalesced_messages() {
        let mut bytes = Vec::new();
        write_content_length_message(&mut bytes, &json!({"jsonrpc":"2.0","id":1})).expect("one");
        write_content_length_message(&mut bytes, &json!({"jsonrpc":"2.0","method":"x"}))
            .expect("two");
        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(
            read_content_length_message(&mut reader)
                .expect("one")
                .unwrap()["id"],
            1
        );
        assert_eq!(
            read_content_length_message(&mut reader)
                .expect("two")
                .unwrap()["method"],
            "x"
        );
        assert!(
            read_content_length_message(&mut reader)
                .expect("eof")
                .is_none()
        );
    }

    #[test]
    fn framing_rejects_missing_and_oversized_lengths() {
        let mut missing = BufReader::new(Cursor::new(b"X: y\r\n\r\n"));
        assert!(read_content_length_message(&mut missing).is_err());
        let source = format!(
            "Content-Length: {}\r\n\r\n",
            super::PROTOCOL_MESSAGE_BYTE_CEILING + 1
        );
        let mut oversized = BufReader::new(Cursor::new(source));
        assert!(read_content_length_message(&mut oversized).is_err());
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PtyId(pub u64);
#[derive(Clone, Debug)]
pub struct PtyPolicy {
    pub maximum_output_bytes: usize,
    pub maximum_input_bytes: usize,
    pub event_capacity: usize,
    pub scrollback_bytes: usize,
}
impl Default for PtyPolicy {
    fn default() -> Self {
        Self {
            maximum_output_bytes: 32 * 1024 * 1024,
            maximum_input_bytes: 1024 * 1024,
            event_capacity: 256,
            scrollback_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug)]
pub struct PtyRequest {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub columns: u16,
    pub rows: u16,
    pub root_revision: u64,
    pub policy: PtyPolicy,
}
#[derive(Clone, Debug)]
pub enum PtyEvent {
    Started {
        id: PtyId,
        root_revision: u64,
    },
    Output {
        id: PtyId,
        bytes: Vec<u8>,
        truncated: bool,
    },
    Exited {
        id: PtyId,
        code: Option<u32>,
    },
    Failed {
        id: PtyId,
        message: String,
    },
}
enum PtyRequestMessage {
    Open { id: PtyId, request: PtyRequest },
    Input { id: PtyId, bytes: Vec<u8> },
    Resize { id: PtyId, columns: u16, rows: u16 },
    Close { id: PtyId },
    Shutdown,
}
enum PtyControl {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}
pub struct PtyService {
    next: u64,
    sender: SyncSender<PtyRequestMessage>,
    receiver: Receiver<PtyEvent>,
}
impl PtyService {
    pub fn start(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<PtyRequestMessage>(capacity.clamp(1, 1024));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 4096));
        thread::Builder::new()
            .name("dust-pty-manager".into())
            .spawn(move || {
                let mut sessions: HashMap<PtyId, SyncSender<PtyControl>> = HashMap::new();
                while let Ok(message) = rx.recv() {
                    match message {
                        PtyRequestMessage::Open { id, request } => {
                            match spawn_pty(id, request, etx.clone()) {
                                Ok(control) => {
                                    sessions.insert(id, control);
                                }
                                Err(error) => {
                                    let _ = etx.try_send(PtyEvent::Failed {
                                        id,
                                        message: error.to_string(),
                                    });
                                }
                            }
                        }
                        PtyRequestMessage::Input { id, bytes } => {
                            if let Some(control) = sessions.get(&id) {
                                let _ = control.try_send(PtyControl::Input(bytes));
                            }
                        }
                        PtyRequestMessage::Resize { id, columns, rows } => {
                            if let Some(control) = sessions.get(&id) {
                                let _ = control.try_send(PtyControl::Resize(columns, rows));
                            }
                        }
                        PtyRequestMessage::Close { id } => {
                            if let Some(control) = sessions.remove(&id) {
                                let _ = control.try_send(PtyControl::Close);
                            }
                        }
                        PtyRequestMessage::Shutdown => {
                            for (_, control) in sessions.drain() {
                                let _ = control.try_send(PtyControl::Close);
                            }
                            break;
                        }
                    }
                }
            })
            .expect("pty manager");
        Self {
            next: 1,
            sender: tx,
            receiver: erx,
        }
    }
    pub fn open(&mut self, request: PtyRequest) -> Result<PtyId, PtyError> {
        validate_pty_request(&request)?;
        let id = PtyId(self.next);
        self.next = self.next.saturating_add(1);
        self.sender
            .try_send(PtyRequestMessage::Open { id, request })
            .map_err(pty_queue)?;
        Ok(id)
    }
    pub fn input(&self, id: PtyId, bytes: Vec<u8>, ceiling: usize) -> Result<(), PtyError> {
        if bytes.len() > ceiling {
            return Err(PtyError::InvalidPolicy);
        }
        self.sender
            .try_send(PtyRequestMessage::Input { id, bytes })
            .map_err(pty_queue)
    }
    pub fn resize(&self, id: PtyId, columns: u16, rows: u16) -> Result<(), PtyError> {
        if columns == 0 || rows == 0 {
            return Err(PtyError::InvalidPolicy);
        }
        self.sender
            .try_send(PtyRequestMessage::Resize { id, columns, rows })
            .map_err(pty_queue)
    }
    pub fn close(&self, id: PtyId) -> Result<(), PtyError> {
        self.sender
            .try_send(PtyRequestMessage::Close { id })
            .map_err(pty_queue)
    }
    pub fn poll(&self) -> Result<Vec<PtyEvent>, PtyError> {
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(events),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Err(PtyError::Stopped),
            }
        }
    }
}
impl Drop for PtyService {
    fn drop(&mut self) {
        let _ = self.sender.try_send(PtyRequestMessage::Shutdown);
    }
}
fn validate_pty_request(request: &PtyRequest) -> Result<(), PtyError> {
    let argv = request.arguments.iter().map(String::len).sum::<usize>();
    let env = request
        .environment
        .iter()
        .map(|(key, value)| key.len().saturating_add(value.len()))
        .sum::<usize>();
    if !request.program.is_absolute()
        || !request.cwd.is_absolute()
        || !request.cwd.is_dir()
        || request.columns == 0
        || request.rows == 0
        || request.arguments.len() > 4096
        || argv > 1024 * 1024
        || env > 1024 * 1024
        || request.policy.maximum_output_bytes == 0
        || request.policy.maximum_output_bytes > 256 * 1024 * 1024
        || request.policy.maximum_input_bytes == 0
        || request.policy.event_capacity == 0
    {
        return Err(PtyError::InvalidPolicy);
    }
    Ok(())
}
fn spawn_pty(
    id: PtyId,
    request: PtyRequest,
    events: SyncSender<PtyEvent>,
) -> Result<SyncSender<PtyControl>, PtyError> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: request.rows,
            cols: request.columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    let mut command = CommandBuilder::new(&request.program);
    command.args(&request.arguments);
    command.cwd(&request.cwd);
    for (key, value) in &request.environment {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    let (control_tx, control_rx) =
        mpsc::sync_channel::<PtyControl>(request.policy.event_capacity.clamp(1, 1024));
    let output_events = events.clone();
    let ceiling = request.policy.maximum_output_bytes;
    thread::spawn(move || {
        let mut total = 0usize;
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let allowed = read.min(ceiling.saturating_sub(total));
                    total = total.saturating_add(allowed);
                    if allowed > 0 {
                        let _ = output_events.try_send(PtyEvent::Output {
                            id,
                            bytes: buffer[..allowed].to_vec(),
                            truncated: allowed < read,
                        });
                    }
                    if allowed < read || total >= ceiling {
                        break;
                    }
                }
            }
        }
    });
    thread::Builder::new()
        .name(format!("dust-pty-{}", id.0))
        .spawn(move || {
            let _ = events.try_send(PtyEvent::Started {
                id,
                root_revision: request.root_revision,
            });
            loop {
                while let Ok(control) = control_rx.try_recv() {
                    match control {
                        PtyControl::Input(bytes) => {
                            let _ = writer.write_all(&bytes);
                            let _ = writer.flush();
                        }
                        PtyControl::Resize(columns, rows) => {
                            let _ = pair.master.resize(PtySize {
                                rows,
                                cols: columns,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                        PtyControl::Close => {
                            let _ = child.kill();
                        }
                    }
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let _ = events.try_send(PtyEvent::Exited {
                            id,
                            code: Some(status.exit_code()),
                        });
                        break;
                    }
                    Err(error) => {
                        let _ = events.try_send(PtyEvent::Failed {
                            id,
                            message: error.to_string(),
                        });
                        let _ = child.kill();
                        break;
                    }
                    _ => thread::sleep(Duration::from_millis(10)),
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        })
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    Ok(control_tx)
}
fn pty_queue(error: std::sync::mpsc::TrySendError<PtyRequestMessage>) -> PtyError {
    match error {
        std::sync::mpsc::TrySendError::Full(_) => PtyError::QueueFull,
        std::sync::mpsc::TrySendError::Disconnected(_) => PtyError::Stopped,
    }
}
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("invalid PTY request or policy")]
    InvalidPolicy,
    #[error("PTY spawn failed: {0}")]
    Spawn(String),
    #[error("PTY queue is full")]
    QueueFull,
    #[error("PTY service stopped")]
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreflightId(pub u64);
#[derive(Clone, Debug)]
pub struct ExecutionPreflightRequest {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub roots: Vec<PathBuf>,
    pub root_revision: u64,
}
#[derive(Clone, Debug)]
pub struct ExecutionPreflightResult {
    pub id: PreflightId,
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub root_revision: u64,
}
#[derive(Debug)]
pub enum ExecutionPreflightEvent {
    Ready(ExecutionPreflightResult),
    Failed {
        id: PreflightId,
        root_revision: u64,
        message: String,
    },
}
struct PreflightCommand {
    id: PreflightId,
    request: ExecutionPreflightRequest,
}
pub struct ExecutionPreflightService {
    next: u64,
    sender: SyncSender<PreflightCommand>,
    receiver: Receiver<ExecutionPreflightEvent>,
}
impl ExecutionPreflightService {
    pub fn start(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<PreflightCommand>(capacity.clamp(1, 256));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 256));
        thread::Builder::new()
            .name("dust-execution-preflight".into())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    let revision = command.request.root_revision;
                    let event = preflight(command.id, command.request).map_or_else(
                        |message| ExecutionPreflightEvent::Failed {
                            id: command.id,
                            root_revision: revision,
                            message,
                        },
                        ExecutionPreflightEvent::Ready,
                    );
                    let _ = etx.try_send(event);
                }
            })
            .expect("execution preflight");
        Self {
            next: 1,
            sender: tx,
            receiver: erx,
        }
    }
    pub fn request(
        &mut self,
        request: ExecutionPreflightRequest,
    ) -> Result<PreflightId, PreflightError> {
        if request.roots.is_empty() || request.roots.len() > 64 {
            return Err(PreflightError::Invalid);
        }
        let id = PreflightId(self.next);
        self.next = self.next.saturating_add(1);
        self.sender
            .try_send(PreflightCommand { id, request })
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => PreflightError::QueueFull,
                std::sync::mpsc::TrySendError::Disconnected(_) => PreflightError::Stopped,
            })?;
        Ok(id)
    }
    pub fn poll(&self) -> Result<Vec<ExecutionPreflightEvent>, PreflightError> {
        let mut out = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => out.push(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(out),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(PreflightError::Stopped);
                }
            }
        }
    }
}
fn preflight(
    id: PreflightId,
    request: ExecutionPreflightRequest,
) -> Result<ExecutionPreflightResult, String> {
    let executable =
        std::fs::canonicalize(&request.executable).map_err(|error| error.to_string())?;
    let cwd = std::fs::canonicalize(&request.cwd).map_err(|error| error.to_string())?;
    let executable_metadata =
        std::fs::symlink_metadata(&executable).map_err(|error| error.to_string())?;
    let cwd_metadata = std::fs::symlink_metadata(&cwd).map_err(|error| error.to_string())?;
    if !executable_metadata.file_type().is_file() || !cwd_metadata.file_type().is_dir() {
        return Err("resolved execution target must be a regular executable and resolved cwd must be a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if executable_metadata.permissions().mode() & 0o111 == 0 {
            return Err("execution target is not executable".into());
        }
    }
    let mut roots = request
        .roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .filter(|root| cwd.starts_with(root))
        .collect::<Vec<_>>();
    roots.sort_by_key(|root| std::cmp::Reverse(root.components().count()));
    let Some(root) = roots.into_iter().next() else {
        return Err("execution cwd is outside workspace roots".into());
    };
    Ok(ExecutionPreflightResult {
        id,
        executable,
        cwd,
        root,
        root_revision: request.root_revision,
    })
}
#[derive(Debug, Error)]
pub enum PreflightError {
    #[error("invalid execution preflight request")]
    Invalid,
    #[error("execution preflight queue is full")]
    QueueFull,
    #[error("execution preflight worker stopped")]
    Stopped,
}
