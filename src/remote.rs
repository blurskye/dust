//! Requests to a running Dust from programs it starts. Dust listens on a Unix socket named
//! in `DUST_SERVER`. `dust --remote FILE` (lazygit's edit key) sends one line per file,
//! `<line>\t<JSON path bytes>` with line 0 when none was given, and exits. `dust --agent-tools`
//! (the MCP server of an agent session) sends `agent\t<json>` to start a subagent and
//! waits on the same connection for its answer.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    net::Shutdown,
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

/// The variable terminals started by Dust find the socket in.
pub const SERVER_VARIABLE: &str = "DUST_SERVER";
const REQUEST_BYTE_CEILING: u64 = 1024 * 1024;
const AGENT_PREFIX: &str = "agent\t";

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
    workers: Vec<thread::JoinHandle<()>>,
}

impl RemoteServer {
    /// Listen in `directory`, which should be private to the user.
    pub fn start(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("dust-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let (sender, requests) = mpsc::channel();
        let (replies, pending) = mpsc::sync_channel(8);
        let stopping = Arc::new(AtomicBool::new(false));
        let reading = Arc::new(Mutex::new(None));
        let reply_stop = stopping.clone();
        let reply_worker = thread::Builder::new()
            .name("dust-remote-replies".into())
            .spawn(move || {
                while !reply_stop.load(Ordering::Acquire) {
                    match pending.recv_timeout(Duration::from_millis(100)) {
                        Ok((stream, answer)) => {
                            // Failed delivery closes the socket and lets the next answer proceed.
                            let _ = send_reply(stream, answer);
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
                for stream in listener.incoming().flatten() {
                    {
                        let mut reading = listener_reading.lock().unwrap();
                        if listener_stop.load(Ordering::Acquire) {
                            break;
                        }
                        *reading = stream.try_clone().ok();
                    }
                    // A client that never writes must not hold up the next one.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let reader = BufReader::new(reader.take(REQUEST_BYTE_CEILING));
                    for line in reader.lines().map_while(Result::ok) {
                        let request = match line.strip_prefix(AGENT_PREFIX) {
                            Some(body) => stream.try_clone().ok().and_then(|reply| {
                                parse_agent(
                                    body,
                                    Reply {
                                        stream: reply,
                                        sender: replies.clone(),
                                    },
                                )
                            }),
                            None => parse(&line).map(RemoteRequest::Open),
                        };
                        let asked_for_agent = matches!(request, Some(RemoteRequest::Agent(_)));
                        if let Some(request) = request
                            && sender.send(request).is_err()
                        {
                            return;
                        }
                        // The asker now waits for the answer instead of writing more.
                        if asked_for_agent {
                            break;
                        }
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
        Ok(Self { path, requests, stopping, reading, workers: vec![listener_worker, reply_worker] })
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
        if let Some(stream) = self.reading.lock().unwrap().as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        // Wake an idle accept; an active read is interrupted by shutdown above.
        let _ = UnixStream::connect(&self.path);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn parse(line: &str) -> Option<RemoteOpen> {
    let (number, path) = line.split_once('\t')?;
    let path = if path.starts_with('[') {
        PathBuf::from(std::ffi::OsString::from_vec(
            serde_json::from_str(path).ok()?,
        ))
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

/// Ask the Dust at `server` for a subagent and wait for its answer: `{"text": …}` or
/// `{"error": …}`.
pub fn ask_agent(server: &Path, request: &Value) -> io::Result<Value> {
    let mut stream = UnixStream::connect(server)?;
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
        let encoded =
            serde_json::to_string(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let entry = format!("{line}\t{encoded}\n");
        if message.len().saturating_add(entry.len()) > REQUEST_BYTE_CEILING as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "remote request exceeds 1 MiB"));
        }
        message.push_str(&entry);
    }
    let mut stream = UnixStream::connect(server)?;
    stream.write_all(message.as_bytes())
}

/// Write a lazygit config whose edit keys open files in this Dust, and return the
/// `LG_CONFIG_FILE` value that layers it over the user's own lazygit config.
pub fn lazygit_config(directory: &Path, dust: &Path) -> io::Result<String> {
    let program = format!("'{}'", dust.display().to_string().replace('\'', "'\\''"))
        .replace('\'', "''");
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

    #[test]
    fn a_sent_file_arrives_with_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let file = directory.path().join("a.rs");
        send(
            server.path(),
            &[file.clone(), PathBuf::from("/b.rs")],
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
                    path: "/b.rs".into(),
                    line: None
                },
            ]
        );
        assert!(parse("3\trelative.rs").is_none());
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
            assert!(workers.upgrade().is_none(), "both workers released their stop flag");
            assert!(!path.exists());
        }
    }

    #[test]
    fn oversized_bulk_requests_are_rejected_before_connecting() {
        let paths = vec![PathBuf::from(format!("/{}", "a".repeat(1024))); 400];
        let error = send(Path::new("/no-such-dust.sock"), &paths, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn lazygit_commands_quote_shell_paths_separately_from_yaml() {
        let directory = tempfile::tempdir().unwrap();
        let dust = directory.path().join("dust '\"$HOME`echo expanded`\\binary");
        fs::write(&dust, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&dust, fs::Permissions::from_mode(0o700)).unwrap();
        lazygit_config(directory.path(), &dust).unwrap();
        let overlay = directory.path().join(format!("lazygit-{}.yml", std::process::id()));
        let yaml = fs::read_to_string(overlay).unwrap();
        let command = yaml.lines().find_map(|line| line.trim().strip_prefix("edit: '")).unwrap()
            .strip_suffix('\'').unwrap().replace("''", "'")
            .replace("{{filename}}", "sample.rs");
        let output = std::process::Command::new("sh").args(["-c", &command]).output().unwrap();
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
        let first = directory.path().join(format!("lazygit-{}.yml", std::process::id()));
        let original = fs::read_to_string(&first).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "remote::tests::lazygit_overlays_are_per_process"])
            .env(CHILD_DIRECTORY, directory.path())
            .status().unwrap();
        assert!(status.success());
        let overlays: Vec<_> = fs::read_dir(directory.path()).unwrap().map(Result::unwrap).collect();
        assert_eq!(overlays.len(), 2);
        assert_eq!(fs::read_to_string(first).unwrap(), original);
        assert!(overlays.iter().any(|entry| fs::read_to_string(entry.path()).unwrap().contains("/second/dust")));
    }

    #[test]
    fn remote_paths_preserve_newlines_tabs_and_non_utf8_bytes() {
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
    fn stalled_reply_delivery_times_out_and_admission_is_bounded() {
        let (stream, _stalled) = UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        let error = send_reply(stream, Ok("x".repeat(4 * 1024 * 1024))).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
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
                &path,
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
