//! Requests to a running Dust from programs it starts. Dust listens on a Unix socket named
//! in `DUST_SERVER`. `dust --remote FILE` (lazygit's edit key) sends one line per file,
//! `<line>\t<JSON path bytes>` with line 0 when none was given, and exits after the request
//! is accepted. `dust --agent-tools`
//! (the MCP server of an agent session) sends `agent\t<json>` to start a subagent and
//! waits on the same connection for its answer.

use std::fs;

use std::io;

use std::io::BufRead;

use std::io::BufReader;

use std::io::Read;

use std::io::Write;

use std::net::Shutdown;

#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
pub use std::os::unix::net::UnixStream;
// Windows 10 and later have AF_UNIX sockets; this crate gives them std's API.
#[cfg(windows)]
use uds_windows::UnixListener;
#[cfg(windows)]
pub use uds_windows::UnixStream;

use std::path::Path;

use std::path::PathBuf;

use std::sync::Arc;

use std::sync::Mutex;

use std::sync::atomic::AtomicBool;

use std::sync::atomic::Ordering;

use std::sync::mpsc;

use std::thread;

use std::time::Duration;

use std::time::Instant;

#[cfg(unix)]
use nix::poll::PollFd;
#[cfg(unix)]
use nix::poll::PollFlags;
#[cfg(unix)]
use nix::poll::PollTimeout;
#[cfg(unix)]
use nix::poll::poll;
use serde_json::Value;
use serde_json::json;

/// The variable terminals started by Dust find the socket in.
pub const SERVER_VARIABLE: &str = "DUST_SERVER";
const REQUEST_BYTE_LIMIT: u64 = 1024 * 1024;
const AGENT_PREFIX: &str = "agent\t";
const REQUEST_QUEUE_LIMIT: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteOpen {
    pub path: PathBuf,
    /// 1-based.
    pub line: Option<usize>,
}

/// A subagent asked for by the agent session numbered `parent`.
#[derive(Debug)]
pub struct AgentRequest {
    pub parent: u64,
    /// A few words for the subagent's tab.
    pub description: String,
    pub prompt: String,
    /// A profile in `[agents.profiles]`; the parent's own when absent.
    pub agent: Option<String>,
    /// A model the subagent's agent offers; its default when absent.
    pub model: Option<String>,
    pub reply: Reply,
}

/// The connection a subagent's answer goes back on.
#[derive(Debug)]
pub struct Reply {
    stream: UnixStream,
    sender: mpsc::SyncSender<(UnixStream, Result<String, String>)>,
}

impl Reply {
    /// The MCP client cancels a call by closing its connection. The request reader
    /// has already finished, so this is the only reader of this socket now.
    pub fn is_cancelled(&self) -> bool {
        #[cfg(unix)]
        {
            let mut descriptors = [PollFd::new(self.stream.as_fd(), PollFlags::POLLIN)];
            if !matches!(poll(&mut descriptors, PollTimeout::ZERO), Ok(1)) {
                return false;
            }
            matches!((&self.stream).read(&mut [0]), Ok(0))
        }
        #[cfg(windows)]
        {
            // Without poll, a non-blocking read tells a closed peer (end of file) apart.
            if self.stream.set_nonblocking(true).is_err() {
                return false;
            }
            let closed = matches!((&self.stream).read(&mut [0]), Ok(0));
            let _ = self.stream.set_nonblocking(false);
            closed
        }
    }

    /// Written from a worker thread, so a reader that stalls cannot stall Dust.
    pub fn send(self, answer: Result<String, String>) {
        // A full queue closes this connection; it must not create another worker.
        let _ = self.sender.try_send((self.stream, answer));
    }
}

fn send_reply(mut stream: UnixStream, answer: Result<String, String>) -> io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let message = match answer {
        Ok(text) => json!({"text": text}),
        Err(error) => json!({"error": error}),
    };
    stream.write_all(format!("{message}\n").as_bytes())
}

#[derive(Debug)]
pub enum RemoteRequest {
    Open(RemoteOpen),
    Agent(AgentRequest),
}

pub struct RemoteServer {
    path: PathBuf,
    requests: mpsc::Receiver<RemoteRequest>,
    stopping: Arc<AtomicBool>,
    reading: Arc<Mutex<Option<UnixStream>>>,
    writing: Arc<Mutex<Option<UnixStream>>>,
    wake: UnixStream,
    workers: Vec<thread::JoinHandle<()>>,
}

impl RemoteServer {
    /// Listen in `directory`, which should be private to the user.
    pub fn start(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("dust-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (wake, _wake_reader) = UnixStream::pair()?;
        let (sender, requests) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
        let (replies, pending) = mpsc::sync_channel::<(UnixStream, Result<String, String>)>(8);
        let stopping = Arc::new(AtomicBool::new(false));
        let reading = Arc::new(Mutex::new(None));
        let writing = Arc::new(Mutex::new(None));
        let reply_writing = writing.clone();
        let reply_stop = stopping.clone();
        let reply_worker = thread::Builder::new()
            .name("dust-remote-replies".into())
            .spawn(move || {
                while !reply_stop.load(Ordering::Acquire) {
                    match pending.recv_timeout(Duration::from_millis(100)) {
                        Ok((stream, answer)) => {
                            {
                                let mut writing = reply_writing.lock().unwrap();
                                if reply_stop.load(Ordering::Acquire) {
                                    break;
                                }
                                *writing = stream.try_clone().ok();
                            }
                            // Failed delivery closes the socket and lets the next answer proceed.
                            let _ = send_reply(stream, answer);
                            *reply_writing.lock().unwrap() = None;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        let listener_stop = stopping.clone();
        let listener_reading = reading.clone();
        let listener_worker = thread::Builder::new()
            .name("dust-remote".into())
            .spawn(move || {
                loop {
                    #[cfg(unix)]
                    {
                        let mut ready = [
                            PollFd::new(listener.as_fd(), PollFlags::POLLIN),
                            PollFd::new(_wake_reader.as_fd(), PollFlags::POLLIN),
                        ];
                        match poll(&mut ready, PollTimeout::NONE) {
                            Err(nix::errno::Errno::EINTR) => continue,
                            Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                    if listener_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            // Windows has no poll here, so the listener checks for a stop
                            // request between non-blocking accepts.
                            #[cfg(windows)]
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        Err(_) => break,
                    };
                    // macOS and Windows hand accepted sockets the listener's non-blocking mode.
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    {
                        let mut reading = listener_reading.lock().unwrap();
                        if listener_stop.load(Ordering::Acquire) {
                            break;
                        }
                        *reading = stream.try_clone().ok();
                    }
                    let deadline = Instant::now() + REQUEST_TIMEOUT;
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let mut reader = BufReader::new(reader.take(REQUEST_BYTE_LIMIT));
                    let mut line = Vec::new();
                    let mut complete = false;
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        // The timeout goes on the handle that reads: Windows sockets keep it
                        // per duplicated handle.
                        if remaining.is_zero()
                            || reader
                                .get_ref()
                                .get_ref()
                                .set_read_timeout(Some(remaining))
                                .is_err()
                        {
                            break;
                        }
                        // fill_buf performs at most one socket read, so each read
                        // gets the remaining overall deadline, even for partial lines.
                        let Ok(buffer) = reader.fill_buf() else { break };
                        let eof = buffer.is_empty();
                        if eof {
                            complete = line.is_empty();
                            if complete {
                                break;
                            }
                        } else {
                            let end = buffer.iter().position(|byte| *byte == b'\n');
                            let length = end.map_or(buffer.len(), |end| end + 1);
                            line.extend_from_slice(&buffer[..length]);
                            reader.consume(length);
                            if end.is_none() {
                                continue;
                            }
                        }
                        let Ok(text) = std::str::from_utf8(&line) else {
                            break;
                        };
                        let text = text
                            .strip_suffix('\n')
                            .map(|text| text.strip_suffix('\r').unwrap_or(text))
                            .unwrap_or(text);
                        let request = match text.strip_prefix(AGENT_PREFIX) {
                            Some(body) => stream.try_clone().ok().and_then(|reply| {
                                parse_agent(
                                    body,
                                    Reply {
                                        stream: reply,
                                        sender: replies.clone(),
                                    },
                                )
                            }),
                            None => parse(text).map(RemoteRequest::Open),
                        };
                        let asked_for_agent = matches!(request, Some(RemoteRequest::Agent(_)));
                        if let Some(request) = request {
                            match sender.try_send(request) {
                                Ok(()) => {}
                                // Reject this connection rather than block new connections or
                                // retain more requests while the editor catches up.
                                Err(mpsc::TrySendError::Full(_)) => break,
                                Err(mpsc::TrySendError::Disconnected(_)) => return,
                            }
                        }
                        line.clear();
                        // The asker now waits for the answer instead of writing more.
                        if asked_for_agent {
                            break;
                        }
                        if eof {
                            complete = true;
                            break;
                        }
                    }
                    // A successful socket write alone does not mean the bounded
                    // request queue accepted every file in a bulk open.
                    if complete && stream.set_write_timeout(Some(REQUEST_TIMEOUT)).is_ok() {
                        let _ = stream.write_all(b"ok\n");
                    }
                    *listener_reading.lock().unwrap() = None;
                }
            });
        let listener_worker = match listener_worker {
            Ok(worker) => worker,
            Err(error) => {
                stopping.store(true, Ordering::Release);
                let _ = reply_worker.join();
                let _ = fs::remove_file(&path);
                return Err(error);
            }
        };
        Ok(Self {
            path,
            requests,
            stopping,
            reading,
            writing,
            wake,
            workers: vec![listener_worker, reply_worker],
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn try_request(&self) -> Option<RemoteRequest> {
        self.requests.try_recv().ok()
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for active in [&self.reading, &self.writing] {
            if let Some(stream) = active.lock().unwrap().as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        // Wake the listener even if its socket path has already been removed.
        let _ = self.wake.shutdown(Shutdown::Both);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn parse(line: &str) -> Option<RemoteOpen> {
    let (number, path) = line.split_once('\t')?;
    let path = if path.starts_with(['[', '"', '{']) {
        crate::files::path_encoding::decode(path).ok()?
    } else {
        PathBuf::from(path)
    };
    path.is_absolute().then(|| RemoteOpen {
        path,
        line: number.parse().ok().filter(|line| *line > 0),
    })
}

fn parse_agent(body: &str, reply: Reply) -> Option<RemoteRequest> {
    let request: Value = serde_json::from_str(body).ok()?;
    Some(RemoteRequest::Agent(AgentRequest {
        parent: request["parent"].as_u64()?,
        description: request["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        prompt: request["prompt"].as_str()?.to_owned(),
        agent: request["agent"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        model: request["model"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        reply,
    }))
}

/// Ask for a subagent on a connected socket and wait for its answer: `{"text": …}` or
/// `{"error": …}`. Shutting down a clone of the socket cancels the call.
pub fn ask_agent(mut stream: UnixStream, request: &Value) -> io::Result<Value> {
    stream.write_all(format!("{AGENT_PREFIX}{request}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    serde_json::from_str(&answer)
        .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "Dust closed the connection"))
}

/// Ask the Dust at `server` to open `paths`; `line` applies to the first.
pub fn send(server: &Path, paths: &[PathBuf], line: Option<usize>) -> io::Result<()> {
    let mut message = String::new();
    for (index, path) in paths.iter().enumerate() {
        let line = if index == 0 { line.unwrap_or(0) } else { 0 };
        let path = std::path::absolute(path)?;
        let encoded = crate::files::path_encoding::encode(&path).map_err(io::Error::other)?;
        let entry = format!("{line}\t{encoded}\n");
        if message.len().saturating_add(entry.len()) > REQUEST_BYTE_LIMIT as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote request exceeds 1 MiB",
            ));
        }
        message.push_str(&entry);
    }
    let mut stream = UnixStream::connect(server)?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT * 2))?;
    stream.write_all(message.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut answer = [0; 3];
    stream.read_exact(&mut answer).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Dust did not accept all remote files: {error}"),
        )
    })?;
    if &answer != b"ok\n" {
        return Err(io::Error::other("Dust did not accept all remote files"));
    }
    Ok(())
}

/// Write a lazygit config whose edit keys open files in this Dust, and return the
/// `LG_CONFIG_FILE` value that layers it over the user's own lazygit config.
pub fn lazygit_config(directory: &Path, dust: &Path) -> io::Result<String> {
    #[cfg(unix)]
    let program =
        format!("'{}'", dust.display().to_string().replace('\'', "'\\''")).replace('\'', "''");
    #[cfg(windows)]
    let program = format!("\"{}\"", dust.display()).replace('\'', "''");
    let overlay = directory.join(format!("lazygit-{}.yml", std::process::id()));
    fs::write(
        &overlay,
        format!(
            "# Written by Dust: lazygit's edit keys open files in the Dust that started it.\n\
             os:\n  \
             edit: '{program} --remote {{{{filename}}}}'\n  \
             editAtLine: '{program} --remote {{{{filename}}}} --line {{{{line}}}}'\n  \
             editAtLineAndWait: '{program} --remote {{{{filename}}}} --line {{{{line}}}}'\n"
        ),
    )?;
    let user = std::env::var("LG_CONFIG_FILE")
        .ok()
        .filter(|files| !files.is_empty())
        .or_else(|| {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })?;
            let file = config.join("lazygit/config.yml");
            file.is_file().then(|| file.display().to_string())
        });
    Ok(match user {
        Some(files) => format!("{files},{}", overlay.display()),
        None => overlay.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sent_file_arrives_with_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let file = directory.path().join("a.rs");
        send(
            server.path(),
            &[file.clone(), crate::files::absolute_test_path("/b.rs")],
            Some(12),
        )
        .unwrap();
        let mut received = Vec::new();
        for _ in 0..200 {
            received.extend(server.try_request().and_then(|request| match request {
                RemoteRequest::Open(open) => Some(open),
                RemoteRequest::Agent(_) => None,
            }));
            if received.len() == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            received,
            [
                RemoteOpen {
                    path: file,
                    line: Some(12)
                },
                RemoteOpen {
                    path: crate::files::absolute_test_path("/b.rs"),
                    line: None
                },
            ]
        );
        assert!(parse("3\trelative.rs").is_none());
    }

    #[test]
    fn request_queue_is_bounded_without_blocking_the_listener() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let mut client = UnixStream::connect(server.path()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        for _ in 0..=REQUEST_QUEUE_LIMIT {
            writeln!(
                client,
                "0\t{}",
                crate::files::path_encoding::encode(&crate::files::absolute_test_path("/file.rs"))
                    .unwrap()
            )
            .unwrap();
        }
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        let received: Vec<_> = std::iter::from_fn(|| server.try_request()).collect();
        assert_eq!(received.len(), REQUEST_QUEUE_LIMIT);
        send(
            server.path(),
            &[crate::files::absolute_test_path("/next.rs")],
            None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(RemoteRequest::Open(open)) = server.try_request() {
                assert_eq!(open.path, crate::files::absolute_test_path("/next.rs"));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn bulk_opens_report_rejected_requests_instead_of_silent_success() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let paths = vec![crate::files::absolute_test_path("/file.rs"); REQUEST_QUEUE_LIMIT];
        send(server.path(), &paths, None).unwrap();
        assert!(
            send(
                server.path(),
                &[crate::files::absolute_test_path("/rejected.rs")],
                None
            )
            .is_err()
        );
        let received: Vec<_> = std::iter::from_fn(|| server.try_request()).collect();
        assert_eq!(received.len(), REQUEST_QUEUE_LIMIT);
        assert!(
            send(
                server.path(),
                &[crate::files::absolute_test_path("/next.rs")],
                None
            )
            .is_ok()
        );

        let server = RemoteServer::start(&directory.path().join("bulk")).unwrap();
        let paths = vec![crate::files::absolute_test_path("/file.rs"); REQUEST_QUEUE_LIMIT + 1];
        assert!(send(server.path(), &paths, None).is_err());
    }

    // The raw request lines carry Unix path text.
    #[cfg(unix)]
    #[test]
    fn final_unterminated_remote_lines_and_trailing_carriage_returns_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let mut client = UnixStream::connect(server.path()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(b"0\t/first\r\r\n0\t/last\r").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "ok\n");
        for expected in ["/first\r", "/last\r"] {
            let Some(RemoteRequest::Open(open)) = server.try_request() else {
                panic!("missing file")
            };
            assert_eq!(open.path, Path::new(expected));
        }
    }

    #[test]
    fn trickling_clients_cannot_extend_the_request_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let mut client = UnixStream::connect(server.path()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let trickling = thread::spawn(move || {
            while writer.write_all(b"x").is_ok() {
                thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        assert!(started.elapsed() < REQUEST_TIMEOUT + Duration::from_secs(2));
        trickling.join().unwrap();
        assert!(server.try_request().is_none());
    }

    #[test]
    fn dropping_the_server_stops_idle_and_reading_workers() {
        for active in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let server = RemoteServer::start(directory.path()).unwrap();
            let workers = Arc::downgrade(&server.stopping);
            let path = server.path().to_owned();
            let _client = active.then(|| {
                let mut stream = UnixStream::connect(&path).unwrap();
                stream.write_all(b"unfinished").unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while server.reading.lock().unwrap().is_none() {
                    assert!(std::time::Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                stream
            });
            drop(server);
            assert!(
                workers.upgrade().is_none(),
                "both workers released their stop flag"
            );
            assert!(!path.exists());
        }
    }

    #[test]
    fn dropping_the_server_after_its_socket_is_removed_still_stops_workers() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let workers = Arc::downgrade(&server.stopping);
        fs::remove_file(server.path()).unwrap();
        drop(server);
        assert!(workers.upgrade().is_none());
    }

    #[test]
    fn dropping_the_server_interrupts_a_stalled_reply() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let mut client = UnixStream::connect(server.path()).unwrap();
        writeln!(client, "agent\t{}", json!({"parent": 1, "prompt": "task"})).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let request = loop {
            if let Some(RemoteRequest::Agent(request)) = server.try_request() {
                break request;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        request.reply.send(Ok("x".repeat(4 * 1024 * 1024)));
        while server.writing.lock().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let started = std::time::Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn oversized_bulk_requests_are_rejected_before_connecting() {
        let paths = vec![crate::files::absolute_test_path(&format!("/{}", "a".repeat(1024))); 1100];
        let error = send(Path::new("/no-such-dust.sock"), &paths, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    // Exercises POSIX shell quoting through a shell-script executable.
    #[cfg(unix)]
    #[test]
    fn lazygit_commands_quote_shell_paths_separately_from_yaml() {
        let directory = tempfile::tempdir().unwrap();
        let dust = directory
            .path()
            .join("dust '\"$HOME`echo expanded`\\binary");
        fs::write(&dust, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&dust, fs::Permissions::from_mode(0o700)).unwrap();
        lazygit_config(directory.path(), &dust).unwrap();
        let overlay = directory
            .path()
            .join(format!("lazygit-{}.yml", std::process::id()));
        let yaml = fs::read_to_string(overlay).unwrap();
        let command = yaml
            .lines()
            .find_map(|line| line.trim().strip_prefix("edit: '"))
            .unwrap()
            .strip_suffix('\'')
            .unwrap()
            .replace("''", "'")
            .replace("{{filename}}", "sample.rs");
        let output = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, b"--remote\nsample.rs\n");
    }

    #[test]
    fn lazygit_overlays_are_per_process() {
        const CHILD_DIRECTORY: &str = "DUST_TEST_LAZYGIT_DIRECTORY";
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            lazygit_config(Path::new(&directory), Path::new("/second/dust")).unwrap();
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        lazygit_config(directory.path(), Path::new("/first/dust")).unwrap();
        let first = directory
            .path()
            .join(format!("lazygit-{}.yml", std::process::id()));
        let original = fs::read_to_string(&first).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "remote::tests::lazygit_overlays_are_per_process"])
            .env(CHILD_DIRECTORY, directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        let overlays: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(overlays.len(), 2);
        assert_eq!(fs::read_to_string(first).unwrap(), original);
        assert!(overlays.iter().any(|entry| {
            fs::read_to_string(entry.path())
                .unwrap()
                .contains("/second/dust")
        }));
    }

    // Tests Unix path bytes that have no Unicode representation.
    #[cfg(unix)]
    #[test]
    fn remote_paths_preserve_newlines_tabs_and_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let file = directory
            .path()
            .join(std::ffi::OsString::from_vec(b"a\n\tb-\xff.rs".to_vec()));
        send(server.path(), std::slice::from_ref(&file), Some(9)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(RemoteRequest::Open(open)) = server.try_request() {
                assert_eq!(
                    open,
                    RemoteOpen {
                        path: file,
                        line: Some(9)
                    }
                );
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn stalled_reply_delivery_times_out_and_request_queue_is_bounded() {
        let (stream, _stalled) = UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        let result = send_reply(stream, Ok("x".repeat(4 * 1024 * 1024)));
        // Windows buffers the whole reply instead of stalling the writer.
        if cfg!(unix) {
            assert!(matches!(
                result.unwrap_err().kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ));
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        let (sender, _pending) = mpsc::sync_channel(1);
        let (stream, _reader) = UnixStream::pair().unwrap();
        Reply {
            stream,
            sender: sender.clone(),
        }
        .send(Ok("first".into()));
        let (stream, mut reader) = UnixStream::pair().unwrap();
        Reply { stream, sender }.send(Ok("rejected".into()));
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn a_subagent_request_is_answered_on_its_connection() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let path = server.path().to_owned();
        let asking = thread::spawn(move || {
            ask_agent(
                UnixStream::connect(&path).unwrap(),
                &json!({"parent": 3, "description": "look", "prompt": "find it"}),
            )
        });
        let request = loop {
            if let Some(RemoteRequest::Agent(request)) = server.try_request() {
                break request;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            (
                request.parent,
                request.prompt.as_str(),
                request.agent.as_deref()
            ),
            (3, "find it", None)
        );
        request.reply.send(Ok("found".into()));
        assert_eq!(asking.join().unwrap().unwrap(), json!({"text": "found"}));
    }
}
