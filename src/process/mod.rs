//! Child processes, framing and execution preflight.

#![forbid(unsafe_code)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::io::BufRead;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::thread;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
use thiserror::Error;

mod jobs;
pub use jobs::JobError;
pub use jobs::JobEvent;
pub use jobs::JobId;
pub use jobs::JobPolicy;
pub use jobs::JobRequest;
pub use jobs::JobStream;
pub use jobs::JobSupervisor;

#[cfg(unix)]
pub(crate) fn terminate_process_group(id: u32) {
    use nix::sys::signal::Signal;
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    let group = Pid::from_raw(id as i32);
    if killpg(group, Signal::SIGTERM).is_ok() {
        thread::sleep(std::time::Duration::from_millis(20));
        let _ = killpg(group, Signal::SIGKILL);
    }
}

#[cfg(windows)]
pub(crate) fn terminate_process_group(id: u32) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill.exe")
        .args(["/PID", &id.to_string(), "/T", "/F"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Observe exit without reaping the leader while teardown can still signal its group.
pub(crate) fn child_exit_status(
    child: &mut std::process::Child,
) -> io::Result<Option<Option<i32>>> {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = Pid::from_raw(child.id() as i32)
            .ok_or_else(|| io::Error::other("invalid child process ID"))?;
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map(|status| status.map(|status| status.exit_status()))
        .map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    child
        .try_wait()
        .map(|status| status.map(|status| status.code()))
}

pub const MAX_PROTOCOL_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROTOCOL_HEADER_BYTES: usize = 16 * 1024;

pub fn read_content_length_message(
    reader: &mut impl BufRead,
) -> Result<Option<Value>, ProtocolError> {
    let mut content_length = None;
    let mut header_bytes = 0_usize;
    loop {
        let mut line = String::new();
        // Limit materialization as well as the post-read length check. A peer
        // need not supply a newline before the header limit takes effect.
        let remaining = MAX_PROTOCOL_HEADER_BYTES.saturating_sub(header_bytes) + 1;
        let read = std::io::Read::take(&mut *reader, remaining as u64)
            .read_line(&mut line)
            .map_err(ProtocolError::Io)?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(ProtocolError::UnexpectedEof)
            };
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > MAX_PROTOCOL_HEADER_BYTES {
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
    if content_length > MAX_PROTOCOL_MESSAGE_BYTES {
        return Err(ProtocolError::MessageTooLarge(content_length));
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).map_err(ProtocolError::Io)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(ProtocolError::Json)
}

pub fn write_content_length_message(
    writer: &mut (impl Write + ?Sized),
    value: &Value,
) -> Result<(), ProtocolError> {
    let body = serde_json::to_vec(value).map_err(ProtocolError::Json)?;
    if body.len() > MAX_PROTOCOL_MESSAGE_BYTES {
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
    #[error("protocol headers exceed the safety limit")]
    HeadersTooLarge,
    #[error("protocol message is missing Content-Length")]
    MissingContentLength,
    #[error("protocol Content-Length is invalid")]
    InvalidContentLength,
    #[error("protocol message length {0} exceeds the safety limit")]
    MessageTooLarge(usize),
    #[error("protocol JSON is invalid: {0}")]
    Json(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;
    use std::io::Cursor;

    use serde_json::json;

    use crate::process::read_content_length_message;

    use crate::process::write_content_length_message;
    #[cfg(unix)]
    #[test]
    fn preflight_pins_symlinked_executable_and_cwd_targets() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::fs::symlink;
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
        let result = crate::process::preflight(
            crate::process::PreflightId(1),
            crate::process::ExecutionPreflightRequest {
                executable: executable_link,
                cwd: cwd_link,
                roots: vec![root.clone()],
                root_revision: 7,
            },
        )
        .unwrap();
        assert_eq!(
            result.executable,
            crate::files::canonicalize(executable).unwrap()
        );
        assert_eq!(result.cwd, crate::files::canonicalize(&root).unwrap());
        assert_eq!(result.root_revision, 7);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_checks_effective_execution_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("tool");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        for mode in [0o641, 0o600, 0o700] {
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(mode)).unwrap();
            let can_execute = std::process::Command::new(&executable).status().is_ok();
            let result = crate::process::preflight(
                crate::process::PreflightId(1),
                crate::process::ExecutionPreflightRequest {
                    executable: executable.clone(),
                    cwd: root.path().into(),
                    roots: vec![root.path().into()],
                    root_revision: 1,
                },
            );
            assert_eq!(result.is_ok(), can_execute, "mode {mode:o}: {result:?}");
        }
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
            crate::process::MAX_PROTOCOL_MESSAGE_BYTES + 1
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
    pub argv0: Option<PathBuf>,
    pub arguments: Vec<std::ffi::OsString>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub columns: u16,
    pub rows: u16,
    pub root_revision: u64,
    pub policy: PtyPolicy,
}
#[derive(Clone, Debug)]
pub enum PtyEvent {
    Resized {
        id: PtyId,
        sequence: u64,
        columns: u16,
        rows: u16,
    },
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
    IoComplete {
        id: PtyId,
        result: Result<(), String>,
    },
    Failed {
        id: PtyId,
        message: String,
    },
    Unknown {
        id: PtyId,
        message: String,
    },
}
mod pty;
pub use pty::PtyWorker;
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("PTY identity exhausted")]
    Exhausted,
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
    identities: Vec<crate::files::RootIdentity>,
    id: PreflightId,
    request: ExecutionPreflightRequest,
}
pub struct ExecutionPreflightWorker {
    next: u64,
    sender: SyncSender<PreflightCommand>,
    receiver: Receiver<ExecutionPreflightEvent>,
}
impl ExecutionPreflightWorker {
    pub(crate) fn identity_available(&self) -> bool {
        self.next < u64::MAX
    }
    pub fn start(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<PreflightCommand>(capacity.clamp(1, 256));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 256));
        thread::Builder::new()
            .name("dust-execution-preflight".into())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    let revision = command.request.root_revision;
                    let event = preflight(command.id, command.request)
                        .and_then(|result| {
                            for expected in command.identities {
                                let actual =
                                    crate::files::RootIdentity::capture(&expected.canonical_path)
                                        .map_err(|e| e.to_string())?;
                                if actual != expected {
                                    return Err("execution root filesystem identity changed".into());
                                }
                            }
                            Ok(result)
                        })
                        .map_or_else(
                            |message| ExecutionPreflightEvent::Failed {
                                id: command.id,
                                root_revision: revision,
                                message,
                            },
                            ExecutionPreflightEvent::Ready,
                        );
                    if etx.send(event).is_err() {
                        break;
                    }
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
        self.request_scoped(request, Vec::new())
    }
    pub(crate) fn request_scoped(
        &mut self,
        request: ExecutionPreflightRequest,
        identities: Vec<crate::files::RootIdentity>,
    ) -> Result<PreflightId, PreflightError> {
        if request.roots.is_empty() || request.roots.len() > 64 {
            return Err(PreflightError::Invalid);
        }
        let id = PreflightId(self.next);
        self.next = self.next.checked_add(1).ok_or(PreflightError::Invalid)?;
        self.sender
            .try_send(PreflightCommand {
                id,
                request,
                identities,
            })
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => PreflightError::QueueFull,
                std::sync::mpsc::TrySendError::Disconnected(_) => PreflightError::Stopped,
            })?;
        Ok(id)
    }
    pub fn poll(&self) -> Result<Vec<ExecutionPreflightEvent>, PreflightError> {
        let mut out = Vec::new();
        for _ in 0..256 {
            match self.receiver.try_recv() {
                Ok(event) => out.push(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(out),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return if out.is_empty() {
                        Err(PreflightError::Stopped)
                    } else {
                        Ok(out)
                    };
                }
            }
        }
        Ok(out)
    }
}
fn preflight(
    id: PreflightId,
    request: ExecutionPreflightRequest,
) -> Result<ExecutionPreflightResult, String> {
    let executable =
        crate::files::canonicalize(&request.executable).map_err(|error| error.to_string())?;
    let cwd = crate::files::canonicalize(&request.cwd).map_err(|error| error.to_string())?;
    let executable_metadata =
        std::fs::symlink_metadata(&executable).map_err(|error| error.to_string())?;
    let cwd_metadata = std::fs::symlink_metadata(&cwd).map_err(|error| error.to_string())?;
    if !executable_metadata.file_type().is_file() || !cwd_metadata.file_type().is_dir() {
        return Err("resolved execution target must be a regular executable and resolved cwd must be a real directory".into());
    }
    if !crate::process::executable_file(&executable) {
        return Err("execution target is not executable".into());
    }
    let mut roots = request
        .roots
        .into_iter()
        .filter_map(|root| crate::files::canonicalize(root).ok())
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

#[cfg(test)]
mod preflight_lifecycle_tests {
    use crate::process::*;
    #[test]
    fn full_result_queue_retains_every_terminal_reply_and_exhaustion_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut service = ExecutionPreflightWorker::start(1);
        let request = || ExecutionPreflightRequest {
            executable: std::env::current_exe().unwrap(),
            cwd: root.path().into(),
            roots: vec![root.path().into()],
            root_revision: 7,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut ids = Vec::new();
        for _ in 0..3 {
            loop {
                match service.request(request()) {
                    Ok(id) => {
                        ids.push(id);
                        break;
                    }
                    Err(PreflightError::QueueFull) => {
                        assert!(Instant::now() < deadline);
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        }
        let mut replies = Vec::new();
        while replies.len() < 3 {
            replies.extend(service.poll().unwrap());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let returned = replies
            .into_iter()
            .map(|r| match r {
                ExecutionPreflightEvent::Ready(r) => r.id,
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(returned, ids);
        service.next = u64::MAX;
        assert!(matches!(
            service.request(request()),
            Err(PreflightError::Invalid)
        ));
    }
}

/// Nonblocking child I/O that yields to session cancellation while a pipe is idle.
pub(crate) struct CancellablePipe<T> {
    // Windows moves a written pipe to its writer thread on the first write.
    pipe: Option<T>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    write_timeout: Option<std::time::Duration>,
    #[cfg(windows)]
    writer: Option<PipeWriter>,
}
/// Windows pipes cannot be made nonblocking, so writes run on a thread and the caller
/// waits with its deadline and cancellation. A write that times out leaves the thread
/// blocked until the child is stopped, and later writes fail.
#[cfg(windows)]
struct PipeWriter {
    requests: SyncSender<Vec<u8>>,
    results: Receiver<io::Result<()>>,
}
impl<T> CancellablePipe<T> {
    #[cfg(unix)]
    pub(crate) fn new(
        pipe: T,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Self>
    where
        T: std::os::fd::AsFd,
    {
        use nix::fcntl::FcntlArg;
        use nix::fcntl::OFlag;
        use nix::fcntl::fcntl;
        let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL)?);
        fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self {
            pipe: Some(pipe),
            cancelled,
            write_timeout: None,
        })
    }
    #[cfg(not(unix))]
    pub(crate) fn new(
        pipe: T,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Self> {
        Ok(Self {
            pipe: Some(pipe),
            cancelled,
            write_timeout: None,
            writer: None,
        })
    }
    pub(crate) fn set_write_timeout(&mut self, timeout: std::time::Duration) {
        self.write_timeout = Some(timeout);
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        self.write_timeout
            .map(|timeout| std::time::Instant::now() + timeout)
    }
    fn retry<R>(
        &mut self,
        deadline: Option<std::time::Instant>,
        mut operation: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        loop {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let pipe = self
                .pipe
                .as_mut()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
            match operation(pipe) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
}
impl<T: io::Read> io::Read for CancellablePipe<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.retry(None, |pipe| pipe.read(bytes))
    }
}
#[cfg(unix)]
impl<T: Write> Write for CancellablePipe<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let deadline = self.deadline();
        self.retry(deadline, |pipe| pipe.write(bytes))
    }
    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let deadline = self.deadline();
        while !bytes.is_empty() {
            match self.retry(deadline, |pipe| pipe.write(bytes)) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        let deadline = self.deadline();
        self.retry(deadline, Write::flush)
    }
}
#[cfg(windows)]
impl<T: Write + Send + 'static> Write for CancellablePipe<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes).map(|()| bytes.len())
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let deadline = self.deadline();
        if self.writer.is_none() {
            let mut pipe = self
                .pipe
                .take()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
            let (requests, pending) = mpsc::sync_channel::<Vec<u8>>(0);
            let (finished, results) = mpsc::sync_channel(1);
            let cancelled = self.cancelled.clone();
            thread::Builder::new()
                .name("dust-pipe-writer".into())
                .spawn(move || {
                    for bytes in pending {
                        // A nonblocking socket reports a full send buffer as WouldBlock.
                        let mut rest = &bytes[..];
                        let result = loop {
                            if rest.is_empty() {
                                break pipe.flush();
                            }
                            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                break Err(io::ErrorKind::BrokenPipe.into());
                            }
                            match pipe.write(rest) {
                                Ok(0) => break Err(io::ErrorKind::WriteZero.into()),
                                Ok(count) => rest = &rest[count..],
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                                    ) =>
                                {
                                    thread::sleep(std::time::Duration::from_millis(1));
                                }
                                Err(error) => break Err(error),
                            }
                        };
                        if finished.send(result).is_err() {
                            break;
                        }
                    }
                })?;
            self.writer = Some(PipeWriter { requests, results });
        }
        let writer = self.writer.as_ref().expect("pipe writer started");
        writer
            .requests
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        loop {
            match writer
                .results
                .recv_timeout(std::time::Duration::from_millis(5))
            {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let failure =
                        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                            io::ErrorKind::TimedOut
                        } else if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            io::ErrorKind::BrokenPipe
                        } else {
                            continue;
                        };
                    // The blocked write still owns the pipe; later writes fail at once.
                    self.writer = None;
                    return Err(failure.into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        // Each write flushes on the writer thread.
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod cancellable_pipe_tests {
    use crate::process::*;

    #[test]
    fn partial_writes_share_one_deadline() {
        struct SlowWriter;
        impl Write for SlowWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                thread::sleep(std::time::Duration::from_millis(2));
                Ok(bytes.len().min(1))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut input = CancellablePipe {
            pipe: Some(SlowWriter),
            cancelled: Default::default(),
            write_timeout: Some(std::time::Duration::from_millis(20)),
        };
        let error = input.write_all(&[0; 100]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn blocked_child_writes_time_out_without_cancellation() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input =
            CancellablePipe::new(child.stdin.take().unwrap(), Default::default()).unwrap();
        input.set_write_timeout(std::time::Duration::from_millis(30));
        let result = input.write_all(&vec![0; 1024 * 1024]);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn cancellation_releases_blocked_child_reads_and_writes() {
        use std::io::Read;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut input =
            CancellablePipe::new(child.stdin.take().unwrap(), cancelled.clone()).unwrap();
        let mut output =
            CancellablePipe::new(child.stdout.take().unwrap(), cancelled.clone()).unwrap();
        let writer = thread::spawn(move || input.write_all(&vec![0; 1024 * 1024]));
        let reader = thread::spawn(move || output.read(&mut [0]));
        thread::sleep(std::time::Duration::from_millis(20));
        let started = std::time::Instant::now();
        cancelled.store(true, Ordering::Release);
        assert!(writer.join().unwrap().is_err());
        assert!(reader.join().unwrap().is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

mod executable;
pub use executable::discover_executable;
pub(crate) use executable::executable_file;
#[cfg(windows)]
pub(crate) use executable::path_extensions;
pub(crate) use executable::terminal_executable;
