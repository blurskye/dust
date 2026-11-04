//! The MCP server Dust gives each top-level agent session. It has one tool, `agent`, which
//! starts a subagent in a new tab of Dust's agent panel and returns the subagent's final
//! answer, so every agent gets the same subagents whatever its own harness offers. It runs
//! as `dust --agent-tools <tab>`, speaks MCP (JSON-RPC, one message per line) on stdin and
//! stdout, and asks the running Dust over the socket in `DUST_SERVER`. Agents that only read
//! MCP servers from their own config, such as pi, run `dust --agent-tools` with no tab; it
//! then uses `DUST_AGENT_TAB`, which Dust sets for every agent it starts. Outside Dust, or in
//! a subagent, the server offers no tools.

use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use serde_json::{Value, json};

use crate::remote;

/// The configured agent profiles, comma separated, for the tool's description.
pub const PROFILES_VARIABLE: &str = "DUST_AGENT_PROFILES";
/// The tab number of the agent session, for servers started from an agent's own config.
pub const TAB_VARIABLE: &str = "DUST_AGENT_TAB";

const INSTRUCTIONS: &str = "You run inside Dust, a terminal editor. Dust provides subagents \
through the `agent` tool of this server. Whenever a subagent would help (a broad search, \
independent tasks that can run in parallel, a self-contained change), use this tool and \
never a built-in subagent or task tool. Each subagent shows up as its own tab in Dust.";

/// The `mcpServers` entry of `session/new` that runs this server for the tab `tab`.
#[must_use]
pub fn server_entry(dust: &Path, socket: &Path, tab: u64, profiles: &str) -> Value {
    json!({
        "name": "dust",
        "command": dust,
        "args": ["--agent-tools", tab.to_string()],
        "env": [
            {"name": remote::SERVER_VARIABLE, "value": socket},
            {"name": PROFILES_VARIABLE, "value": profiles},
        ],
    })
}

/// Answer MCP requests until stdin closes. Tool calls run on their own threads, so several
/// subagents can work at once. Without Dust's socket and a tab there are no tools.
pub fn serve(target: Option<(PathBuf, u64)>, profiles: &str) -> io::Result<()> {
    let output = Arc::new(Mutex::new(io::stdout()));
    let tools = if target.is_some() {
        vec![tool(profiles)]
    } else {
        Vec::new()
    };
    for line in io::stdin().lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications such as `notifications/initialized` need no answer.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let params = &message["params"];
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "dust", "version": env!("CARGO_PKG_VERSION")},
                "instructions": if target.is_some() { INSTRUCTIONS } else { "" },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools})),
            "tools/call" => {
                let Some((socket, tab)) = target.clone() else {
                    write(
                        &output,
                        &answer(
                            id,
                            Err(json!({"code": -32602, "message": "Dust offers no tools here"})),
                        ),
                    );
                    continue;
                };
                let (params, output) = (params.clone(), Arc::clone(&output));
                thread::spawn(move || write(&output, &answer(id, Ok(call(&socket, tab, &params)))));
                continue;
            }
            method => Err(json!({"code": -32601, "message": format!("{method} is not supported")})),
        };
        write(&output, &answer(id, result));
    }
    Ok(())
}

fn answer(id: Value, result: Result<Value, Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

fn write(output: &Mutex<io::Stdout>, message: &Value) {
    if let Ok(mut output) = output.lock() {
        let _ = writeln!(output, "{message}").and_then(|()| output.flush());
    }
}

fn tool(profiles: &str) -> Value {
    let agents = if profiles.is_empty() {
        String::new()
    } else {
        format!(" One of: {profiles}.")
    };
    json!({
        "name": "agent",
        "description": "Launch a subagent that works on a task on its own and returns its final \
            answer. Use it for searches across many files, for a self-contained change, and to \
            run independent tasks in parallel (call the tool several times in one message). \
            The subagent works in the same folder with the same tools but knows nothing of \
            this conversation, so the prompt must say everything it needs: the goal, the \
            relevant files and facts, and what to report back. The user does not see its \
            answer unless you pass it on. Prefer this tool over any built-in subagent or task \
            tool.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A short title for the task (3-6 words), shown on the subagent's tab."
                },
                "prompt": {
                    "type": "string",
                    "description": "The complete task for the subagent."
                },
                "agent": {
                    "type": "string",
                    "description": format!("Which agent runs it; the same agent as you when left out.{agents}")
                },
                "model": {
                    "type": "string",
                    "description": "The model the subagent uses, by the name or id its agent gives it; the agent's default when left out. If the agent does not offer it, the call fails with the models it does offer."
                }
            },
            "required": ["description", "prompt"]
        }
    })
}

fn call(socket: &Path, tab: u64, params: &Value) -> Value {
    let arguments = &params["arguments"];
    let (text, failed) = if params["name"] != "agent" {
        (format!("Dust has no tool {}", params["name"]), true)
    } else {
        let request = json!({
            "parent": tab,
            "description": arguments["description"],
            "prompt": arguments["prompt"],
            "agent": arguments["agent"],
            "model": arguments["model"],
        });
        match remote::ask_agent(socket, &request) {
            Ok(reply) => match reply["text"].as_str() {
                Some(text) => (text.to_owned(), false),
                None => (
                    reply["error"]
                        .as_str()
                        .unwrap_or("the subagent gave no answer")
                        .to_owned(),
                    true,
                ),
            },
            Err(error) => (format!("could not reach Dust: {error}"), true),
        }
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}
