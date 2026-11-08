//! The MCP server Dust gives each top-level agent session. It has one tool, `agent`, which
//! starts a subagent in a new tab of Dust's agent panel and returns the subagent's final
//! answer, so every agent gets the same subagents whatever its own harness offers. It runs
//! as `dust --agent-tools <tab>`, speaks MCP (JSON-RPC, one message per line) on stdin and
//! stdout, and asks the running Dust over the socket in `DUST_SERVER`. Agents that only read
//! MCP servers from their own config, such as pi, run `dust --agent-tools` with no tab; it
//! then uses `DUST_AGENT_TAB`, which Dust sets for every agent it starts. Outside Dust, or in
//! a subagent, the server offers no tools.

use std::io;

use std::io::BufRead;

use std::io::Read;

use std::io::Write;

use std::net::Shutdown;

use crate::remote::UnixStream;

use std::path::Path;

use std::path::PathBuf;

use std::sync::Arc;

use std::sync::Mutex;

use std::thread;

use serde_json::Value;

use serde_json::json;

use crate::remote;

/// The configured agent profiles, comma separated, for the tool's description.
pub const PROFILES_VARIABLE: &str = "DUST_AGENT_PROFILES";
/// The tab number of the agent session, for servers started from an agent's own config.
pub const TAB_VARIABLE: &str = "DUST_AGENT_TAB";

const MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
const MAX_CONCURRENT_CALLS: usize = 8;

const INSTRUCTIONS: &str = "You run inside Dust, a terminal editor. Dust provides subagents \
through the `agent` tool of this server. Whenever a subagent would help (a broad search, \
independent tasks that can run in parallel, a self-contained change), use this tool and \
never a built-in subagent or task tool. Each subagent shows up as its own tab in Dust.";

/// The `mcpServers` entry of `session/new` that runs this server for the tab `tab`.
pub fn server_entry(dust: &Path, socket: &Path, tab: u64, profiles: &str) -> Result<Value, String> {
    let dust = dust.to_str().ok_or("Dust's MCP server requires a UTF-8 executable path; move Dust to a UTF-8 installation directory")?;
    let socket = socket
        .to_str()
        .ok_or("Dust's MCP server requires a UTF-8 socket path; use a UTF-8 runtime directory")?;
    Ok(json!({
        "name": "dust",
        "command": dust,
        "args": ["--agent-tools", tab.to_string()],
        "env": [
            {"name": remote::SERVER_VARIABLE, "value": socket},
            {"name": PROFILES_VARIABLE, "value": profiles},
        ],
    }))
}

/// Answer MCP requests until stdin closes. Tool calls run on their own threads, so several
/// subagents can work at once. Without Dust's socket and a tab there are no tools.
pub fn serve(target: Option<(PathBuf, u64)>, profiles: &str) -> io::Result<()> {
    serve_io(
        io::stdin().lock(),
        Arc::new(Mutex::new(io::stdout())),
        target,
        profiles,
    )
}

fn serve_io<W: Write + Send + 'static>(
    mut input: impl BufRead,
    output: Arc<Mutex<W>>,
    target: Option<(PathBuf, u64)>,
    profiles: &str,
) -> io::Result<()> {
    let mut calls: Vec<(Value, UnixStream, thread::JoinHandle<()>)> = Vec::new();
    let tools = if target.is_some() {
        vec![tool(profiles)]
    } else {
        Vec::new()
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        if input
            .by_ref()
            .take(MAX_MESSAGE_BYTES + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.len() as u64 > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds 1 MiB",
            ));
        }
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        // Closing this call's connection tells Dust to stop only its subagent.
        if message["method"] == "notifications/cancelled" && message.get("id").is_none() {
            if let Some((_, stream, _)) = calls
                .iter()
                .find(|(id, _, call)| id == &message["params"]["requestId"] && !call.is_finished())
            {
                let _ = stream.shutdown(Shutdown::Both);
            }
            continue;
        }
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
                calls.retain(|(_, _, call)| !call.is_finished());
                if calls.len() == MAX_CONCURRENT_CALLS {
                    Err(
                        json!({"code": -32000, "message": "Dust's subagent capacity is full; retry after a call finishes"}),
                    )
                } else {
                    match UnixStream::connect(&socket)
                        .and_then(|stream| stream.try_clone().map(|cancel| (stream, cancel)))
                    {
                        Ok((stream, cancel)) => {
                            let (params, output) = (params.clone(), Arc::clone(&output));
                            let request_id = id.clone();
                            let worker = thread::spawn(move || {
                                write(&output, &answer(id, Ok(call(stream, tab, &params))));
                            });
                            calls.push((request_id, cancel, worker));
                            continue;
                        }
                        Err(error) => Err(
                            json!({"code": -32603, "message": format!("could not reach Dust: {error}")}),
                        ),
                    }
                }
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

fn write(output: &Mutex<impl Write>, message: &Value) {
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

fn call(stream: UnixStream, tab: u64, params: &Value) -> Value {
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
        match remote::ask_agent(stream, &request) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    use std::time::Duration;
    use std::time::Instant;
    #[cfg(windows)]
    use uds_windows::UnixListener;

    // Byte paths that are not UTF-8 exist only on Unix.
    #[cfg(unix)]
    #[test]
    fn mcp_server_entry_rejects_non_utf8_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let invalid = Path::new(OsStr::from_bytes(b"/invalid-\xff"));
        let valid = Path::new("/dust");
        assert!(
            server_entry(invalid, valid, 1, "")
                .unwrap_err()
                .contains("executable path")
        );
        assert!(
            server_entry(valid, invalid, 1, "")
                .unwrap_err()
                .contains("socket path")
        );
        let entry = server_entry(valid, Path::new("/socket"), 1, "pi").unwrap();
        assert_eq!(entry["command"], "/dust");
        assert_eq!(entry["env"][0]["value"], "/socket");
    }

    #[test]
    fn mcp_input_rejects_oversized_lines_before_reading_the_rest() {
        for terminated in [false, true] {
            let mut bytes = vec![b' '; MAX_MESSAGE_BYTES as usize * 2];
            if terminated {
                bytes.push(b'\n');
            }
            let mut input = Cursor::new(bytes);
            let error =
                serve_io(&mut input, Arc::new(Mutex::new(Vec::new())), None, "").unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(input.position(), MAX_MESSAGE_BYTES + 1);
        }
    }

    #[test]
    fn mcp_input_accepts_the_limit_and_processes_following_messages() {
        let ping = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut bytes = vec![b' '; MAX_MESSAGE_BYTES as usize - ping.len()];
        bytes.extend_from_slice(ping);
        bytes.extend_from_slice(ping);
        let output = Arc::new(Mutex::new(Vec::new()));
        serve_io(bytes.as_slice(), Arc::clone(&output), None, "").unwrap();
        let output = output.lock().unwrap();
        let replies: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2);
        assert!(
            replies
                .iter()
                .all(|reply| reply["id"] == 1 && reply["result"] == json!({}))
        );
    }

    #[test]
    fn mcp_cancellation_closes_only_the_matching_calls_connection() {
        let directory = tempfile::tempdir().unwrap();
        let server = remote::RemoteServer::start(directory.path()).unwrap();
        let target = server.path().to_owned();
        let (mut client, input) = UnixStream::pair().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let serving = thread::spawn(move || {
            serve_io(BufReader::new(input), output, Some((target, 1)), "").unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        for id in [json!(1), json!("1")] {
            writeln!(
                client,
                "{}",
                json!({
                    "id": id, "method": "tools/call", "params": {"name": "agent", "arguments": {
                        "description": "test", "prompt": "wait"
                    }}
                })
            )
            .unwrap();
            loop {
                if let Some(remote::RemoteRequest::Agent(request)) = server.try_request() {
                    requests.push(request);
                    break;
                }
                assert!(Instant::now() < deadline, "call did not reach Dust");
                thread::yield_now();
            }
        }
        writeln!(
            client,
            "{}",
            json!({
                "method": "notifications/cancelled", "params": {"requestId": 1}
            })
        )
        .unwrap();
        while !requests[0].reply.is_cancelled() {
            assert!(Instant::now() < deadline, "cancellation did not reach Dust");
            thread::yield_now();
        }
        assert!(!requests[1].reply.is_cancelled());
        for request in requests {
            request.reply.send(Ok("done".into()));
        }
        drop(client);
        serving.join().unwrap();
    }

    #[test]
    fn mcp_concurrent_calls_are_bounded_without_blocking_ping() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("remote.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut requests = String::new();
        for id in 0..=MAX_CONCURRENT_CALLS {
            requests.push_str(&format!(
                "{}\n",
                json!({
                    "id":id,"method":"tools/call", "params":{"name":"agent", "arguments":{
                        "description":"test", "prompt":"wait for the reply"
                    }}
                })
            ));
        }
        requests.push_str("{\"id\":\"ping\",\"method\":\"ping\"}\n");
        let output = Arc::new(Mutex::new(Vec::new()));
        serve_io(
            requests.as_bytes(),
            Arc::clone(&output),
            Some((socket, 1)),
            "",
        )
        .unwrap();
        {
            let output = output.lock().unwrap();
            let replies: Vec<Value> = output
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect();
            assert_eq!(replies.len(), 2);
            assert_eq!(replies[0]["id"], MAX_CONCURRENT_CALLS);
            assert_eq!(replies[0]["error"]["code"], -32000);
            assert!(
                replies[0]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("capacity")
            );
            assert_eq!(replies[1]["id"], "ping");
            assert_eq!(replies[1]["result"], json!({}));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connections = Vec::new();
        while connections.len() < MAX_CONCURRENT_CALLS {
            match listener.accept() {
                Ok((stream, _)) => connections.push(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "subagents did not connect");
                    thread::yield_now();
                }
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        for mut stream in connections {
            // macOS hands out accepted sockets non-blocking like their listener.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.starts_with("agent\t"));
            writeln!(stream, "{{\"text\":\"done\"}}").unwrap();
        }
        loop {
            let output = output.lock().unwrap();
            if output.iter().filter(|byte| **byte == b'\n').count() == MAX_CONCURRENT_CALLS + 2 {
                let replies: Vec<Value> = output
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(|line| serde_json::from_slice(line).unwrap())
                    .collect();
                for id in 0..MAX_CONCURRENT_CALLS {
                    let reply = replies.iter().find(|reply| reply["id"] == id).unwrap();
                    assert_eq!(reply["result"]["isError"], false);
                    assert_eq!(reply["result"]["content"][0]["text"], "done");
                }
                break;
            }
            drop(output);
            assert!(Instant::now() < deadline, "subagent replies did not arrive");
            thread::yield_now();
        }
    }
}
