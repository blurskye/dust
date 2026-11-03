//! Requests to a running Dust from programs it starts. Dust listens on a Unix socket named
//! in `DUST_SERVER`. `dust --remote FILE` (lazygit's edit key) sends one line per file,
//! `<line>\t<absolute path>` with line 0 when none was given, and exits. `dust --agent-tools`
//! (the MCP server of an agent session) sends `agent\t<json>` to start a subagent and
//! waits on the same connection for its answer.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
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
pub struct Reply(UnixStream);

impl Reply {
    /// Written from a worker thread, so a reader that stalls cannot stall Dust.
    pub fn send(self, answer: Result<String, String>) {
        let message = match answer {
            Ok(text) => json!({"text": text}),
            Err(error) => json!({"error": error}),
        };
        let mut stream = self.0;
        thread::spawn(move || {
            let _ = stream.write_all(format!("{message}\n").as_bytes());
        });
    }
}

#[derive(Debug)]
pub enum RemoteRequest {
    Open(RemoteOpen),
    Agent(AgentRequest),
}

pub struct RemoteServer {
    path: PathBuf,
    requests: mpsc::Receiver<RemoteRequest>,
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
        thread::Builder::new()
            .name("dust-remote".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    // A client that never writes must not hold up the next one.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let reader = BufReader::new(reader.take(REQUEST_BYTE_CEILING));
                    for line in reader.lines().map_while(Result::ok) {
                        let request = match line.strip_prefix(AGENT_PREFIX) {
                            Some(body) => stream
                                .try_clone()
                                .ok()
                                .and_then(|reply| parse_agent(body, Reply(reply))),
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
                }
            })?;
        Ok(Self { path, requests })
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
        let _ = fs::remove_file(&self.path);
    }
}

fn parse(line: &str) -> Option<RemoteOpen> {
    let (number, path) = line.split_once('\t')?;
    let path = PathBuf::from(path);
    path.is_absolute().then(|| RemoteOpen {
        path,
        line: number.parse().ok().filter(|line| *line > 0),
    })
}

fn parse_agent(body: &str, reply: Reply) -> Option<RemoteRequest> {
    let request: Value = serde_json::from_str(body).ok()?;
    Some(RemoteRequest::Agent(AgentRequest {
        parent: request["parent"].as_u64()?,
        description: request["description"].as_str().unwrap_or_default().to_owned(),
        prompt: request["prompt"].as_str()?.to_owned(),
        agent: request["agent"].as_str().filter(|name| !name.is_empty()).map(str::to_owned),
        model: request["model"].as_str().filter(|name| !name.is_empty()).map(str::to_owned),
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
    let mut stream = UnixStream::connect(server)?;
    let mut message = String::new();
    for (index, path) in paths.iter().enumerate() {
        let line = if index == 0 { line.unwrap_or(0) } else { 0 };
        message.push_str(&format!("{line}\t{}\n", std::path::absolute(path)?.display()));
    }
    stream.write_all(message.as_bytes())
}

/// Write a lazygit config whose edit keys open files in this Dust, and return the
/// `LG_CONFIG_FILE` value that layers it over the user's own lazygit config.
pub fn lazygit_config(directory: &Path, dust: &Path) -> io::Result<String> {
    let program = format!("\"{}\"", dust.display()).replace('\'', "''");
    let overlay = directory.join("lazygit.yml");
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
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
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
        send(server.path(), &[file.clone(), PathBuf::from("/b.rs")], Some(12)).unwrap();
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
                RemoteOpen { path: file, line: Some(12) },
                RemoteOpen { path: "/b.rs".into(), line: None },
            ]
        );
        assert!(parse("3\trelative.rs").is_none());
    }

    #[test]
    fn a_subagent_request_is_answered_on_its_connection() {
        let directory = tempfile::tempdir().unwrap();
        let server = RemoteServer::start(directory.path()).unwrap();
        let path = server.path().to_owned();
        let asking = thread::spawn(move || {
            ask_agent(&path, &json!({"parent": 3, "description": "look", "prompt": "find it"}))
        });
        let request = loop {
            if let Some(RemoteRequest::Agent(request)) = server.try_request() {
                break request;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((request.parent, request.prompt.as_str(), request.agent.as_deref()), (3, "find it", None));
        request.reply.send(Ok("found".into()));
        assert_eq!(asking.join().unwrap().unwrap(), json!({"text": "found"}));
    }
}
