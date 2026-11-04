use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[cfg(unix)]
use std::{
    process::{Command, Stdio},
    time::Instant,
};

pub(super) const MAXIMUM_TOOL_VERSION_BYTES: u64 = 1024;
pub(super) const MAXIMUM_TOOL_VERSION_CHARS: usize = 160;

#[derive(Clone, Debug)]
pub struct ToolVersionRequest {
    pub id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub current_directory: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct ToolVersionResult {
    pub id: String,
    pub result: Result<String, String>,
}

pub(super) enum ToolVersionCommand {
    Probe(ToolVersionRequest),
    Shutdown,
}

/// Bounded, shell-free version inspection for the tool-manager presentation.
/// It never decides whether a tool is executable; launch still uses canonical
/// preflight immediately before the protocol process starts.
pub struct ToolVersionService {
    sender: SyncSender<ToolVersionCommand>,
    receiver: Receiver<ToolVersionResult>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Default for ToolVersionService {
    fn default() -> Self {
        Self::start()
    }
}

impl ToolVersionService {
    #[must_use]
    pub fn start() -> Self {
        let (sender, requests) = mpsc::sync_channel(32);
        let (results, receiver) = mpsc::sync_channel(32);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("dust-tool-version-probe".into())
            .spawn(move || {
                while !worker_cancel.load(Ordering::Acquire) {
                    let Ok(command) = requests.recv() else {
                        break;
                    };
                    let ToolVersionCommand::Probe(request) = command else {
                        break;
                    };
                    let id = request.id.clone();
                    let result = probe_tool_version(request, &worker_cancel);
                    let mut result = ToolVersionResult { id, result };
                    loop {
                        if worker_cancel.load(Ordering::Acquire) {
                            return;
                        }
                        match results.try_send(result) {
                            Ok(()) => break,
                            Err(TrySendError::Disconnected(_)) => return,
                            Err(TrySendError::Full(returned)) => result = returned,
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            })
            .expect("tool-version worker must start");
        Self {
            sender,
            receiver,
            cancel,
            thread: Some(thread),
        }
    }

    pub fn probe(&self, request: ToolVersionRequest) -> Result<(), String> {
        self.sender
            .try_send(ToolVersionCommand::Probe(request))
            .map_err(|error| match error {
                TrySendError::Full(_) => "tool-version probe queue is full".into(),
                TrySendError::Disconnected(_) => "tool-version probe worker stopped".into(),
            })
    }

    pub fn try_result(&self) -> Option<ToolVersionResult> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for ToolVersionService {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.sender.try_send(ToolVersionCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
pub(super) fn drain_version_pipe<Fd: std::os::fd::AsFd>(
    pipe: &Fd,
    output: &mut Vec<u8>,
    oversized: &mut bool,
) -> Result<bool, String> {
    let mut buffer = [0_u8; 512];
    loop {
        match nix::unistd::read(pipe, &mut buffer) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                let remaining = (MAXIMUM_TOOL_VERSION_BYTES as usize)
                    .saturating_add(1)
                    .saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(remaining)]);
                if read > remaining || output.len() > MAXIMUM_TOOL_VERSION_BYTES as usize {
                    *oversized = true;
                    return Err("version probe output exceeded its limit".into());
                }
            }
            Err(nix::errno::Errno::EAGAIN) => return Ok(false),
            Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(format!("cannot read version probe output: {error}")),
        }
    }
}

#[cfg(unix)]
pub(super) fn probe_tool_version(
    request: ToolVersionRequest,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let mut command = Command::new(&request.executable);
    command
        .args(&request.arguments)
        .envs(&request.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = &request.current_directory {
        command.current_dir(directory);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start version probe: {error}"))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("version probe stdout was unavailable".into());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("version probe stderr was unavailable".into());
    };
    for pipe in [
        &stdout as &dyn std::os::fd::AsFd,
        &stderr as &dyn std::os::fd::AsFd,
    ] {
        let configured = fcntl(pipe, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .map_err(|error| format!("cannot inspect version probe pipe: {error}"))
            .and_then(|flags| {
                fcntl(pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
                    .map(|_| ())
                    .map_err(|error| format!("cannot bound version probe pipe: {error}"))
            });
        if let Err(error) = configured {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut oversized = false;
    let deadline = Instant::now() + request.timeout.min(Duration::from_secs(30));
    let status = loop {
        if !stdout_closed {
            match drain_version_pipe(&stdout, &mut stdout_bytes, &mut oversized) {
                Ok(closed) => stdout_closed = closed,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if !stderr_closed {
            match drain_version_pipe(&stderr, &mut stderr_bytes, &mut oversized) {
                Ok(closed) => stderr_closed = closed,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("version probe cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("version probe timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("version probe failed: {error}"));
            }
        }
    };
    let close_deadline = Instant::now() + Duration::from_millis(125);
    while !(stdout_closed && stderr_closed) && Instant::now() < close_deadline {
        if !stdout_closed {
            stdout_closed = drain_version_pipe(&stdout, &mut stdout_bytes, &mut oversized)?;
        }
        if !stderr_closed {
            stderr_closed = drain_version_pipe(&stderr, &mut stderr_bytes, &mut oversized)?;
        }
        if !(stdout_closed && stderr_closed) {
            thread::sleep(Duration::from_millis(5));
        }
    }
    if !(stdout_closed && stderr_closed) {
        return Err("version probe output remained open after process exit".into());
    }
    let bytes = if stdout_bytes.is_empty() {
        &stderr_bytes
    } else {
        &stdout_bytes
    };
    if !status.success() {
        let diagnostic =
            String::from_utf8_lossy(&bytes[..bytes.len().min(MAXIMUM_TOOL_VERSION_BYTES as usize)])
                .split_whitespace()
                .take(24)
                .collect::<Vec<_>>()
                .join(" ");
        return Err(if diagnostic.is_empty() {
            format!("version probe exited with {status}")
        } else {
            format!(
                "version probe exited with {status}: {diagnostic}{}",
                if oversized { " (output truncated)" } else { "" }
            )
        });
    }
    if oversized {
        return Err("version probe output exceeded its limit".into());
    }
    let version = String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if version.is_empty() {
        return Err("version probe produced no output".into());
    }
    let version = version.strip_prefix("Version: ").unwrap_or(&version);
    Ok(version.chars().take(MAXIMUM_TOOL_VERSION_CHARS).collect())
}

#[cfg(not(unix))]
pub(super) fn probe_tool_version(
    _request: ToolVersionRequest,
    _cancelled: &AtomicBool,
) -> Result<String, String> {
    Err("bounded tool-version probes are not qualified on this platform".into())
}
