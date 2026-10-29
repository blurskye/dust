#![forbid(unsafe_code)]

//! Agent Client Protocol (ACP) client. An agent such as Claude Code (`claude-code-acp`),
//! Gemini CLI (`gemini --experimental-acp`) or Codex (`codex-acp`) runs as a child process
//! and speaks JSON-RPC 2.0 as one JSON object per line on stdin/stdout.

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

pub const PROTOCOL_VERSION: u64 = 1;
const LINE_BYTE_CEILING: usize = 16 * 1024 * 1024;

/// What arrives from the agent, in order.
#[derive(Debug)]
pub enum AgentMessage {
    /// The reply to one of our requests.
    Response { id: u64, result: Result<Value, String> },
    /// A request the agent makes of the editor: permission and file access.
    Request { id: Value, method: String, params: Value },
    /// `session/update` and other notifications.
    Notification { method: String, params: Value },
    /// A line the agent wrote to stderr.
    Log(String),
    /// The process ended or its output could not be read.
    Exited(String),
}

/// A running agent process.
pub struct AgentProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<AgentMessage>,
    next_id: u64,
}

impl AgentProcess {
    pub fn spawn(
        program: &Path,
        arguments: &[String],
        environment: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<Self, String> {
        let mut child = Command::new(program)
            .args(arguments)
            .envs(environment)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{}: {error}", program.display()))?;
        let stdin = child.stdin.take().ok_or("agent stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("agent stdout is unavailable")?;
        let stderr = child.stderr.take();
        let (sender, messages) = mpsc::channel();
        let log = sender.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let reason = loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break "the agent exited".to_owned(),
                    Ok(_) if line.len() > LINE_BYTE_CEILING => {
                        break "the agent sent an oversized message".to_owned();
                    }
                    Ok(_) => {
                        let Some(message) = parse_line(line.trim()) else {
                            continue;
                        };
                        if sender.send(message).is_err() {
                            return;
                        }
                    }
                    Err(error) => break format!("reading the agent failed: {error}"),
                }
            };
            let _ = sender.send(AgentMessage::Exited(reason));
        });
        if let Some(stderr) = stderr {
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if log.send(AgentMessage::Log(line)).is_err() {
                        return;
                    }
                }
            });
        }
        Ok(Self {
            child,
            stdin,
            messages,
            next_id: 1,
        })
    }

    /// Send a request and return its id; the reply arrives as `AgentMessage::Response`.
    pub fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Answer a request the agent made.
    pub fn respond(&mut self, id: Value, result: Result<Value, String>) -> Result<(), String> {
        let message = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(message) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": message}})
            }
        };
        self.write(&message)
    }

    pub fn try_message(&self) -> Option<AgentMessage> {
        self.messages.try_recv().ok()
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("writing to the agent failed: {error}"))
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_line(line: &str) -> Option<AgentMessage> {
    if line.is_empty() {
        return None;
    }
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return Some(AgentMessage::Log(line.to_owned()));
    };
    let method = value.get("method").and_then(Value::as_str).map(str::to_owned);
    let params = value.get_mut("params").map(Value::take).unwrap_or(Value::Null);
    match (method, value.get("id").cloned()) {
        (Some(method), Some(id)) => Some(AgentMessage::Request { id, method, params }),
        (Some(method), None) => Some(AgentMessage::Notification { method, params }),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let result = match value.get_mut("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the agent reported an error")
                    .to_owned()),
                None => Ok(value.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
            };
            Some(AgentMessage::Response { id, result })
        }
        (None, None) => None,
    }
}

/// `initialize`: the editor reads and writes files for the agent (so it sees unsaved
/// buffers); terminals stay with the agent.
#[must_use]
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": {"readTextFile": true, "writeTextFile": true},
            "terminal": false
        },
        "clientInfo": {"name": "dust", "version": env!("CARGO_PKG_VERSION")}
    })
}

#[must_use]
pub fn new_session_params(cwd: &Path) -> Value {
    json!({"cwd": cwd, "mcpServers": []})
}

/// One piece of a prompt: text, or a file the agent should look at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptPart {
    Text(String),
    /// A file and, optionally, the exact text of a selection in it.
    File { path: PathBuf, text: Option<String> },
}

#[must_use]
pub fn prompt_params(session: &str, parts: &[PromptPart]) -> Value {
    let blocks = parts
        .iter()
        .map(|part| match part {
            PromptPart::Text(text) => json!({"type": "text", "text": text}),
            PromptPart::File { path, text: None } => json!({
                "type": "resource_link",
                "uri": file_uri(path),
                "name": path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default(),
            }),
            PromptPart::File {
                path,
                text: Some(text),
            } => json!({
                "type": "resource",
                "resource": {"uri": file_uri(path), "text": text, "mimeType": "text/plain"}
            }),
        })
        .collect::<Vec<_>>();
    json!({"sessionId": session, "prompt": blocks})
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// A `session/update` notification, reduced to what the panel shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionUpdate {
    AgentText(String),
    Thought(String),
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        id: String,
        title: Option<String>,
        status: Option<String>,
    },
    Plan(Vec<(String, String)>),
    Other,
}

#[must_use]
pub fn session_update(params: &Value) -> SessionUpdate {
    let update = &params["update"];
    let text = |value: &Value| value["content"]["text"].as_str().unwrap_or_default().to_owned();
    let string = |key: &str| update[key].as_str().map(str::to_owned);
    match update["sessionUpdate"].as_str().unwrap_or_default() {
        "agent_message_chunk" => SessionUpdate::AgentText(text(update)),
        "agent_thought_chunk" => SessionUpdate::Thought(text(update)),
        "tool_call" => SessionUpdate::ToolCall {
            id: string("toolCallId").unwrap_or_default(),
            title: string("title").unwrap_or_default(),
            status: string("status").unwrap_or_else(|| "pending".into()),
        },
        "tool_call_update" => SessionUpdate::ToolCallUpdate {
            id: string("toolCallId").unwrap_or_default(),
            title: string("title"),
            status: string("status"),
        },
        "plan" => SessionUpdate::Plan(
            update["entries"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            (
                                entry["status"].as_str().unwrap_or("pending").to_owned(),
                                entry["content"].as_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => SessionUpdate::Other,
    }
}

/// A permission option the agent offers: `allow_once`, `allow_always`, `reject_once` or
/// `reject_always`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[must_use]
pub fn permission_options(params: &Value) -> Vec<PermissionOption> {
    params["options"]
        .as_array()
        .map(|options| {
            options
                .iter()
                .map(|option| PermissionOption {
                    id: option["optionId"].as_str().unwrap_or_default().to_owned(),
                    name: option["name"].as_str().unwrap_or_default().to_owned(),
                    kind: option["kind"].as_str().unwrap_or_default().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn permission_selected(option: &str) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": option}})
}

#[must_use]
pub fn permission_cancelled() -> Value {
    json!({"outcome": {"outcome": "cancelled"}})
}

/// The lines `line` (1-based) .. `line + limit` of `text`, as `fs/read_text_file` asks.
#[must_use]
pub fn slice_lines(text: &str, line: Option<u64>, limit: Option<u64>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let skip = usize::try_from(line.unwrap_or(1).saturating_sub(1)).unwrap_or(usize::MAX);
    let take = limit.map_or(usize::MAX, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_parsed_into_responses_requests_and_notifications() {
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s"}}"#),
            Some(AgentMessage::Response { id: 3, result: Ok(_) })
        ));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","id":4,"error":{"code":1,"message":"no"}}"#),
            Some(AgentMessage::Response { id: 4, result: Err(message) }) if message == "no"
        ));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{}}"#),
            Some(AgentMessage::Request { method, .. }) if method == "session/request_permission"
        ));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#),
            Some(AgentMessage::Notification { method, .. }) if method == "session/update"
        ));
    }

    #[test]
    fn updates_reduce_to_panel_entries() {
        let chunk = json!({"sessionId": "s", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}});
        assert_eq!(session_update(&chunk), SessionUpdate::AgentText("hi".into()));
        let tool = json!({"update": {"sessionUpdate": "tool_call", "toolCallId": "t", "title": "Read main.rs", "status": "in_progress"}});
        assert_eq!(
            session_update(&tool),
            SessionUpdate::ToolCall {
                id: "t".into(),
                title: "Read main.rs".into(),
                status: "in_progress".into()
            }
        );
    }

    #[test]
    fn read_requests_take_a_line_window() {
        assert_eq!(slice_lines("a\nb\nc\n", Some(2), Some(1)), "b\n");
        assert_eq!(slice_lines("a\nb\n", None, None), "a\nb\n");
    }
}
