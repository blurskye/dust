#![forbid(unsafe_code)]

//! Agent Client Protocol (ACP) client. An agent such as Claude Code (`claude-agent-acp`),
//! Codex (`codex-acp`), pi (`pi-acp`), OpenCode (`opencode acp`) or Gemini CLI
//! (`gemini --experimental-acp`) runs as a child process and speaks JSON-RPC 2.0 as one
//! JSON object per line on stdin/stdout.

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

pub mod history;
pub mod mcp;

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

/// Parameters of `session/new`: `tools` are the MCP servers the session gets and `meta` the
/// profile's adapter settings, sent as `_meta`.
#[must_use]
pub fn new_session_params(cwd: &Path, tools: &Value, meta: Option<&Value>) -> Value {
    let mut params = json!({"cwd": cwd, "mcpServers": tools});
    if let Some(meta) = meta {
        params["_meta"] = meta.clone();
    }
    params
}

/// How an agent can pick up one of its earlier sessions, from its `initialize` reply.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rejoin {
    /// `session/resume`: reconnect without replaying the conversation.
    pub resume: bool,
    /// `session/load`: reconnect and replay the conversation as updates.
    pub load: bool,
}

#[must_use]
pub fn rejoin_support(initialize: &Value) -> Rejoin {
    let capabilities = &initialize["agentCapabilities"];
    Rejoin {
        resume: capabilities["sessionCapabilities"]["resume"].is_object(),
        load: capabilities["loadSession"].as_bool() == Some(true),
    }
}

/// Parameters of `session/resume` and `session/load`.
#[must_use]
pub fn rejoin_params(session: &str, cwd: &Path, tools: &Value, meta: Option<&Value>) -> Value {
    let mut params = new_session_params(cwd, tools, meta);
    params["sessionId"] = json!(session);
    params
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
    /// The full list of session options, after the agent changed one.
    Options(Vec<SessionOption>),
    /// The agent switched its mode (the older mode API).
    Mode(String),
    /// The slash commands the agent offers, by name and description.
    Commands(Vec<(String, String)>),
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
        "config_option_update" => SessionUpdate::Options(config_options(update)),
        "current_mode_update" => SessionUpdate::Mode(string("currentModeId").unwrap_or_default()),
        "available_commands_update" => SessionUpdate::Commands(
            update["availableCommands"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|command| {
                    Some((
                        command["name"].as_str()?.trim_start_matches('/').to_owned(),
                        command["description"].as_str().unwrap_or_default().to_owned(),
                    ))
                })
                .collect(),
        ),
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

/// A setting of the session the user can change, such as its model, mode or thinking level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOption {
    pub id: String,
    pub name: String,
    /// `model`, `mode`, `thought_level` or an agent's own category.
    pub category: String,
    pub current: String,
    pub values: Vec<OptionValue>,
    pub source: OptionSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionValue {
    pub value: String,
    pub name: String,
    pub description: String,
}

/// Which request changes the option: agents that predate session config options offer
/// the mode and model through their own methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionSource {
    Config,
    Mode,
    Model,
}

/// The options in a `session/new` (or `session/set_config_option`) result. When the agent
/// sends `configOptions`, the older `modes` and `models` fields are ignored, as the
/// protocol asks.
#[must_use]
pub fn session_options(result: &Value) -> Vec<SessionOption> {
    if result["configOptions"].is_array() {
        return config_options(result);
    }
    let mut options = Vec::new();
    let legacy = [
        ("modes", "mode", "Mode", "currentModeId", "availableModes", "id", OptionSource::Mode),
        ("models", "model", "Model", "currentModelId", "availableModels", "modelId", OptionSource::Model),
    ];
    for (field, id, name, current, available, key, source) in legacy {
        let Some(values) = result[field][available].as_array() else {
            continue;
        };
        options.push(SessionOption {
            id: id.into(),
            name: name.into(),
            category: id.into(),
            current: result[field][current].as_str().unwrap_or_default().to_owned(),
            values: values
                .iter()
                .map(|value| OptionValue {
                    value: value[key].as_str().unwrap_or_default().to_owned(),
                    name: value["name"].as_str().unwrap_or_default().to_owned(),
                    description: value["description"].as_str().unwrap_or_default().to_owned(),
                })
                .collect(),
            source,
        });
    }
    options
}

fn config_options(value: &Value) -> Vec<SessionOption> {
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    value["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|option| option["type"].as_str().is_none_or(|kind| kind == "select"))
        .map(|option| {
            let mut values = Vec::new();
            for entry in option["options"].as_array().into_iter().flatten() {
                // Values may come in named groups; the group goes into the description.
                match entry["options"].as_array() {
                    Some(group) => values.extend(group.iter().map(|value| OptionValue {
                        value: text(value, "value"),
                        name: text(value, "name"),
                        description: text(entry, "name"),
                    })),
                    None => values.push(OptionValue {
                        value: text(entry, "value"),
                        name: text(entry, "name"),
                        description: text(entry, "description"),
                    }),
                }
            }
            SessionOption {
                id: text(option, "id"),
                name: text(option, "name"),
                category: option["category"].as_str().unwrap_or("other").to_owned(),
                current: option["currentValue"]
                    .as_str()
                    .map_or_else(|| option["currentValue"].to_string(), str::to_owned),
                values,
                source: OptionSource::Config,
            }
        })
        .collect()
}

/// The request that sets `option` to `value` in `session`.
#[must_use]
pub fn set_option_request(session: &str, option: &SessionOption, value: &str) -> (&'static str, Value) {
    match option.source {
        OptionSource::Config => (
            "session/set_config_option",
            json!({"sessionId": session, "configId": option.id, "value": value}),
        ),
        OptionSource::Mode => ("session/set_mode", json!({"sessionId": session, "modeId": value})),
        OptionSource::Model => ("session/set_model", json!({"sessionId": session, "modelId": value})),
    }
}

/// A way to sign in that the agent offers in its `initialize` result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[must_use]
pub fn auth_methods(initialize: &Value) -> Vec<AuthMethod> {
    initialize["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|method| AuthMethod {
            id: method["id"].as_str().unwrap_or_default().to_owned(),
            name: method["name"].as_str().unwrap_or_default().to_owned(),
            description: method["description"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// A permission option the agent offers: `allow_once`, `allow_always`, `reject_once` or
/// `reject_always`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// What a permission request is for, in words, from the tool call's kind.
#[must_use]
pub fn permission_action(params: &Value) -> &'static str {
    match params["toolCall"]["kind"].as_str() {
        Some("read") => "read a file",
        Some("edit") => "change a file",
        Some("delete") => "delete a file",
        Some("move") => "move a file",
        Some("search") => "search",
        Some("execute") => "run a command",
        Some("fetch") => "fetch from the web",
        _ => "run a tool",
    }
}

/// The files a permission request would change, as the tool call names them.
#[must_use]
pub fn permission_changes(params: &Value) -> Vec<PathBuf> {
    let call = &params["toolCall"];
    if !matches!(call["kind"].as_str(), Some("edit" | "delete" | "move")) {
        return Vec::new();
    }
    let locations = call["locations"].as_array().into_iter().flatten();
    let diffs = call["content"].as_array().into_iter().flatten().filter(|item| item["type"] == "diff");
    let mut paths: Vec<PathBuf> = locations
        .chain(diffs)
        .filter_map(|item| item["path"].as_str())
        .chain(call["rawInput"]["file_path"].as_str())
        .map(PathBuf::from)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The tool call's own title, then the command it would run and the size of an edit, which a title
/// such as a bare path leaves out.
#[must_use]
pub fn permission_detail(params: &Value) -> String {
    let call = &params["toolCall"];
    let mut detail = call["title"].as_str().unwrap_or("a tool").to_owned();
    if let Some(command) = call["rawInput"]["command"].as_str()
        && !detail.contains(command)
    {
        detail.push_str(&format!("\n$ {command}"));
    }
    let mut shows_change = call["rawInput"]["command"].is_string();
    for change in call["content"].as_array().into_iter().flatten() {
        if change["type"] == "diff" {
            shows_change = true;
            detail.push_str(&changed_lines(
                change["oldText"].as_str().unwrap_or_default(),
                change["newText"].as_str().unwrap_or_default(),
            ));
        }
    }
    if !shows_change {
        detail.push_str(&tool_arguments(&call["rawInput"]));
    }
    detail
}

/// What any other tool was asked to do, one short line per argument.
fn tool_arguments(input: &Value) -> String {
    const SHOWN: usize = 6;
    const LINE: usize = 200;
    let Some(arguments) = input.as_object() else {
        return String::new();
    };
    let mut text = String::new();
    for (name, value) in arguments.iter().take(SHOWN) {
        let value = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let clipped: String = value.chars().take(LINE).collect();
        let more = if clipped.len() < value.len() { "…" } else { "" };
        text.push_str(&format!("\n{name}: {clipped}{more}"));
    }
    if arguments.len() > SHOWN {
        text.push_str(&format!("\n… {} more", arguments.len() - SHOWN));
    }
    text
}

/// The lines an edit changes, between what it leaves alone at either end.
fn changed_lines(old: &str, new: &str) -> String {
    const SHOWN: usize = 6;
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let front = old.iter().zip(&new).take_while(|(old, new)| old == new).count();
    let back = old[front..]
        .iter()
        .rev()
        .zip(new[front..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let mut text = String::new();
    let mut side = |sign: char, lines: &[&str]| {
        for line in lines.iter().take(SHOWN) {
            text.push_str(&format!("\n{sign} {line}"));
        }
        if lines.len() > SHOWN {
            text.push_str(&format!("\n{sign} … {} more lines", lines.len() - SHOWN));
        }
    };
    side('-', &old[front..old.len() - back]);
    side('+', &new[front..new.len() - back]);
    text
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
    fn permission_requests_name_the_action_and_what_it_touches() {
        let edit = json!({"toolCall": {
            "kind": "edit",
            "title": "src/main.rs",
            "content": [{"type": "diff", "path": "src/main.rs", "oldText": "a\nb\n", "newText": "c\n"}],
        }});
        assert_eq!(permission_action(&edit), "change a file");
        assert_eq!(permission_detail(&edit), "src/main.rs\n- a\n- b\n+ c");
        let run = json!({"toolCall": {"kind": "execute", "title": "Run", "rawInput": {"command": "cargo test"}}});
        assert_eq!(permission_action(&run), "run a command");
        assert_eq!(permission_detail(&run), "Run\n$ cargo test");
        assert_eq!(permission_action(&json!({"toolCall": {}})), "run a tool");
        assert_eq!(permission_changes(&edit), [PathBuf::from("src/main.rs")]);
        assert_eq!(permission_changes(&run), [] as [PathBuf; 0]);
        let located = json!({"toolCall": {
            "kind": "edit",
            "locations": [{"path": "/w/a.rs"}],
            "rawInput": {"file_path": "/w/a.rs"},
        }});
        assert_eq!(permission_changes(&located), [PathBuf::from("/w/a.rs")]);
        let agent = json!({"toolCall": {
            "title": "mcp__dust__agent",
            "rawInput": {"agent": "reviewer", "count": 2, "task": "reply\n  ok"},
        }});
        assert_eq!(
            permission_detail(&agent),
            "mcp__dust__agent\nagent: reviewer\ncount: 2\ntask: reply ok"
        );
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
    fn options_come_from_config_options_or_the_older_mode_and_model_fields() {
        let config = json!({"configOptions": [
            {"id": "model", "name": "Model", "category": "model", "type": "select",
             "currentValue": "a", "options": [{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]},
            {"id": "effort", "name": "Thinking", "category": "thought_level", "type": "select",
             "currentValue": "high", "options": [{"group": "g", "name": "Levels",
                 "options": [{"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]}]}
        ], "modes": {"currentModeId": "x", "availableModes": [{"id": "x", "name": "X"}]}});
        let options = session_options(&config);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].values.len(), 2);
        assert_eq!(options[1].category, "thought_level");
        assert_eq!(options[1].values[1].value, "high");
        assert_eq!(set_option_request("s", &options[1], "low").0, "session/set_config_option");

        let legacy = json!({
            "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "Default"}, {"id": "plan", "name": "Plan"}]},
            "models": {"currentModelId": "m1", "availableModels": [{"modelId": "m1", "name": "One"}]}
        });
        let options = session_options(&legacy);
        assert_eq!(options[0].current, "default");
        assert_eq!(options[1].values[0].value, "m1");
        let (method, params) = set_option_request("s", &options[0], "plan");
        assert_eq!((method, params["modeId"].as_str()), ("session/set_mode", Some("plan")));
    }

    #[test]
    fn read_requests_take_a_line_window() {
        assert_eq!(slice_lines("a\nb\nc\n", Some(2), Some(1)), "b\n");
        assert_eq!(slice_lines("a\nb\n", None, None), "a\nb\n");
    }
}
