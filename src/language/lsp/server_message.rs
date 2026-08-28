use super::protocol::{send_cancel, send_event};
use super::*;

pub(super) struct ServerMessageContext<'a> {
    pub server: &'a str,
    pub stdin: &'a mut ChildStdin,
    pub events: &'a SyncSender<LspEvent>,
    pub logs: &'a mut VecDeque<String>,
    pub base_capabilities: &'a LspCapabilities,
    pub capabilities: &'a mut LspCapabilities,
    pub registrations: &'a mut HashMap<String, String>,
    pub configuration: &'a LspServerConfiguration,
}

pub(super) fn handle_server_message(context: ServerMessageContext<'_>, message: &Value) {
    let ServerMessageContext {
        server,
        stdin,
        events,
        logs,
        base_capabilities,
        capabilities,
        registrations,
        configuration,
    } = context;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "textDocument/publishDiagnostics" => {
            let uri = message
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = message.pointer("/params/version").and_then(Value::as_u64);
            let diagnostics = diagnostics(
                message
                    .pointer("/params/diagnostics")
                    .unwrap_or(&Value::Null),
            );
            send_event(
                events,
                LspEvent::Diagnostics {
                    server: server.into(),
                    uri: uri.into(),
                    version,
                    diagnostics,
                },
            );
        }
        "$/progress" => {
            if let Some(event) = lsp_progress_event(server, message) {
                send_event(events, event);
            }
        }
        "window/logMessage" | "window/showMessage" => {
            let text = sanitize_log(
                message
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                MAX_LOG_LINE_BYTES,
            );
            if logs.len() == MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(text.clone());
            send_event(
                events,
                LspEvent::Log {
                    server: server.into(),
                    message: text,
                },
            );
        }
        _ => {}
    }
    if method == "client/registerCapability" || method == "client/unregisterCapability" {
        let parsed = if method == "client/registerCapability" {
            message
                .pointer("/params/registrations")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .take(256)
                        .filter_map(|item| {
                            Some((
                                item.get("id")?.as_str()?.to_owned(),
                                item.get("method")?.as_str()?.to_owned(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|items| !items.is_empty())
                .map(|items| {
                    for (id, method) in items {
                        registrations.insert(id, method);
                    }
                })
        } else {
            message
                .pointer("/params/unregisterations")
                .or_else(|| message.pointer("/params/unregistrations"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .take(256)
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .for_each(|id| {
                            registrations.remove(id);
                        })
                })
        };
        if let Some(id) = message.get("id") {
            let response = if parsed.is_some() {
                *capabilities = effective_capabilities(
                    base_capabilities,
                    registrations.values().map(String::as_str),
                );
                send_event(
                    events,
                    LspEvent::CapabilitiesChanged {
                        server: server.into(),
                        capabilities: capabilities.clone(),
                    },
                );
                json!({"jsonrpc":"2.0","id":id,"result":null})
            } else {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"malformed dynamic registration"}})
            };
            let _ = write_content_length_message(stdin, &response);
        }
        return;
    }
    if method == "workspace/inlayHint/refresh" {
        send_event(
            events,
            LspEvent::InlayHintsRefresh {
                server: server.into(),
            },
        );
        if let Some(id) = message.get("id") {
            let _ = write_content_length_message(
                stdin,
                &json!({"jsonrpc":"2.0","id":id,"result":null}),
            );
        }
        return;
    }
    if method == "workspace/applyEdit" {
        let request_id = message.get("id").and_then(ServerRequestId::from_value);
        let edit = message
            .pointer("/params/edit")
            .map(workspace_edit)
            .transpose();
        match (request_id, edit) {
            (Some(request_id), Ok(Some(edit))) => send_event(
                events,
                LspEvent::ApplyEditRequest {
                    server: server.into(),
                    request_id,
                    label: message
                        .pointer("/params/label")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    edit,
                },
            ),
            _ => {
                if let Some(id) = message.get("id") {
                    let _ = write_content_length_message(
                        stdin,
                        &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"malformed workspace edit"}}),
                    );
                }
            }
        }
        return;
    }
    if let Some(id) = message.get("id") {
        let response = match method {
            "workspace/configuration" => Value::Array(
                message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .take(256)
                            .map(|item| {
                                let section = item.get("section").and_then(Value::as_str);
                                section
                                    .and_then(|section| configuration.settings.get(section))
                                    .cloned()
                                    .unwrap_or(Value::Null)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            "workspace/workspaceFolders" => Value::Null,
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability" => Value::Null,
            _ => {
                let _ = write_content_length_message(
                    stdin,
                    &json!({
                        "jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not implemented by Dust"}
                    }),
                );
                return;
            }
        };
        let _ = write_content_length_message(
            stdin,
            &json!({"jsonrpc":"2.0","id":id,"result":response}),
        );
    }
}

pub(super) fn effective_capabilities<'a>(
    base: &LspCapabilities,
    methods: impl Iterator<Item = &'a str>,
) -> LspCapabilities {
    let mut value = base.clone();
    for method in methods {
        match method {
            "textDocument/diagnostic" => value.pull_diagnostics = true,
            "textDocument/semanticTokens" | "textDocument/semanticTokens/full" => {
                value.semantic_tokens = true
            }
            "textDocument/inlayHint" => value.inlay_hints = true,
            "textDocument/codeLens" => value.code_lens = true,
            "textDocument/documentHighlight" => value.document_highlights = true,
            "textDocument/foldingRange" => value.folding_ranges = true,
            "textDocument/selectionRange" => value.selection_ranges = true,
            "textDocument/completion" => value.completion = true,
            "textDocument/hover" => value.hover = true,
            _ => {}
        }
    }
    value
}

pub(super) fn expire_requests(
    server: &str,
    stdin: &mut ChildStdin,
    pending: &mut HashMap<i64, PendingRequest>,
    events: &SyncSender<LspEvent>,
) {
    let now = Instant::now();
    let expired = pending
        .iter()
        .filter_map(|(id, request)| (request.deadline <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(request) = pending.remove(&id) {
            send_cancel(stdin, id);
            send_event(
                events,
                LspEvent::RequestFailed {
                    server: server.into(),
                    generation: request.generation,
                    method: request.feature.method().into(),
                    message: "language-server request timed out".into(),
                },
            );
        }
    }
}
