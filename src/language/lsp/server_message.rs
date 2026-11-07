//! Incoming language-server messages and responses.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Instant;

use serde_json::Value;

use serde_json::json;

use crate::process::write_content_length_message;

use crate::language::lsp::MAX_LOG_LINE_BYTES;

use crate::language::lsp::MAX_LOG_LINES;

use crate::language::lsp::MAX_PENDING_REQUESTS;

use crate::language::lsp::server::LspCapabilities;

use crate::language::lsp::messages::LspDocument;

use crate::language::lsp::messages::LspEvent;

use crate::language::lsp::server::LspServerConfiguration;

use crate::language::lsp::messages::ServerRequestId;

use crate::language::lsp::origins::EditContext;

use crate::language::lsp::origins::EventSender;

use crate::language::lsp::protocol::diagnostics;

use crate::language::lsp::protocol::lsp_progress_event;

use crate::language::lsp::protocol::sanitize_log;

use crate::language::lsp::protocol::send_cancel;

use crate::language::lsp::protocol::send_event;

use crate::language::lsp::protocol::server_status_event;

use crate::language::lsp::protocol::workspace_edit;

use crate::language::lsp::session::PendingRequest;

pub(super) struct ServerMessageContext<'a> {
    pub server: &'a str,
    pub root: &'a std::path::Path,
    pub stdin: &'a mut dyn std::io::Write,
    pub events: &'a EventSender,
    pub logs: &'a mut VecDeque<String>,
    pub base_capabilities: &'a LspCapabilities,
    pub capabilities: &'a mut LspCapabilities,
    pub registrations: &'a mut HashMap<String, (String, Value)>,
    pub configuration: &'a LspServerConfiguration,
    pub sources: &'a HashMap<String, (crate::editor::BufferId, crate::editor::text::TextVersion)>,
    pub diagnostic_lineage: &'a crate::language::lsp::origins::DiagnosticLineage,
    pub documents: &'a HashMap<String, LspDocument>,
    pub apply_requests: &'a mut HashMap<ServerRequestId, std::sync::Arc<()>>,
}

pub(super) fn handle_server_message(context: ServerMessageContext<'_>, message: &Value) {
    let ServerMessageContext {
        server,
        root,
        stdin,
        events,
        logs,
        base_capabilities,
        capabilities,
        registrations,
        configuration,
        sources,
        diagnostic_lineage,
        documents,
        apply_requests,
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
            let reported = message
                .pointer("/params/version")
                .filter(|value| !value.is_null());
            let version = reported.and_then(Value::as_u64);
            let ambiguous = diagnostic_lineage.ambiguous(uri);
            let rejected = ambiguous
                || diagnostic_lineage.tracked(uri)
                    && (!sources.contains_key(uri)
                        || reported.is_some() && version.is_none()
                        || version.is_some_and(|version| {
                            documents
                                .get(uri)
                                .is_none_or(|document| document.version < version)
                        }));
            // Header/source refusal precedes per-diagnostic string/range normalization.
            // Unowned public-service notifications retain their compatibility payload.
            let diagnostics = if rejected {
                Vec::new()
            } else {
                diagnostics(
                    message
                        .pointer("/params/diagnostics")
                        .unwrap_or(&Value::Null),
                )
            };
            let disk = (!rejected && !diagnostic_lineage.tracked(uri))
                .then(|| {
                    let path = crate::language::lsp::paths::path_from_file_uri(uri)?;
                    let path = std::fs::canonicalize(path).ok()?;
                    let root = std::fs::canonicalize(root).ok()?;
                    if !path.starts_with(root) {
                        return None;
                    }
                    let bytes = crate::files::read_file_bounded(&path, 4 * 1024 * 1024).ok()?;
                    Some((
                        uri.to_owned(),
                        crate::editor::text::TextBuffer::from_text(&String::from_utf8(bytes).ok()?),
                    ))
                })
                .flatten();
            let event_source = EventSender {
                context: EditContext {
                    encoding: capabilities.position_encoding,
                    disk,
                    diagnostic_ambiguous: ambiguous,
                    sources: std::sync::Arc::new(
                        sources
                            .get(uri)
                            .filter(|_| !rejected)
                            .map(|source| HashMap::from([(uri.to_owned(), source.clone())]))
                            .unwrap_or_default(),
                    ),
                    ..events.context.clone()
                },
                ..events.clone()
            };
            send_event(
                &event_source,
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
        "experimental/serverStatus" => {
            if let Some(event) = server_status_event(server, message) {
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
                    method: Some(method.into()),
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
                .filter(|items| items.len() <= 256)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            Some((
                                item.get("id")?.as_str()?.to_owned(),
                                (
                                    item.get("method")?.as_str()?.to_owned(),
                                    item.get("registerOptions").cloned().unwrap_or(Value::Null),
                                ),
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
                .filter(|items| items.len() <= 256)
                .map(|items| {
                    items
                        .iter()
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
                    registrations
                        .values()
                        .map(|(method, options)| (method.as_str(), options)),
                );
                for (method, options) in registrations.values() {
                    if method == "textDocument/completion" {
                        capabilities.completion_resolve |=
                            options.get("resolveProvider").and_then(Value::as_bool) == Some(true);
                        if let Some(triggers) =
                            options.get("triggerCharacters").and_then(Value::as_array)
                        {
                            for trigger in triggers.iter().filter_map(Value::as_str) {
                                if !capabilities
                                    .completion_triggers
                                    .iter()
                                    .any(|s| s == trigger)
                                {
                                    capabilities.completion_triggers.push(trigger.into());
                                }
                            }
                        }
                    }
                }
                send_event(
                    events,
                    LspEvent::CapabilitiesChanged {
                        server: server.into(),
                        method: method.into(),
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
            (Some(request_id), Ok(Some(edit))) => {
                if apply_requests.contains_key(&request_id) {
                    return;
                }
                let key = std::sync::Arc::new(());
                let delivered = apply_requests.len() < MAX_PENDING_REQUESTS
                    && events.apply(
                        LspEvent::ApplyEditRequest {
                            server: server.into(),
                            request_id: request_id.clone(),
                            label: message
                                .pointer("/params/label")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            edit,
                        },
                        request_id.clone(),
                        key.clone(),
                        capabilities.position_encoding,
                        sources.clone(),
                    );
                if delivered {
                    apply_requests.insert(request_id, key);
                } else {
                    let _ = write_content_length_message(
                        stdin,
                        &json!({"jsonrpc":"2.0","id":request_id.value(),"result":{"applied":false,"failureReason":"workspace edit request queue is full"}}),
                    );
                }
            }
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
                            .map(|item| {
                                // Empty rather than null: the CSS server reads null as no
                                // settings at all and then validates nothing.
                                match item.get("section").and_then(Value::as_str) {
                                    Some(section) => configuration
                                        .settings
                                        .pointer(&format!("/{}", section.replace('.', "/")))
                                        .cloned()
                                        .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
                                    None => configuration.settings.clone(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            "workspace/workspaceFolders" => crate::language::lsp::protocol::initialize_message(
                root, None,
            )["params"]["workspaceFolders"]
                .clone(),
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
    registrations: impl Iterator<Item = (&'a str, &'a Value)>,
) -> LspCapabilities {
    let mut value = base.clone();
    let mut methods = HashMap::<&str, Vec<Value>>::new();
    for (method, options) in registrations {
        let method = if method == "textDocument/semanticTokens/full" {
            "textDocument/semanticTokens"
        } else {
            method
        };
        methods.entry(method).or_default().push(options.clone());
    }
    for (method, options) in methods {
        if method == "textDocument/didChange" {
            value.sync_registrations = options;
            continue;
        }
        let enabled = match method {
            "textDocument/diagnostic" => &mut value.pull_diagnostics,
            "textDocument/semanticTokens" | "textDocument/semanticTokens/full" => {
                &mut value.semantic_tokens
            }
            "textDocument/inlayHint" => &mut value.inlay_hints,
            "textDocument/documentColor" => &mut value.document_colors,
            "textDocument/codeLens" => &mut value.code_lens,
            "textDocument/documentHighlight" => &mut value.document_highlights,
            "textDocument/foldingRange" => &mut value.folding_ranges,
            "textDocument/selectionRange" => &mut value.selection_ranges,
            "textDocument/completion" => &mut value.completion,
            "textDocument/hover" => &mut value.hover,
            "textDocument/signatureHelp" => &mut value.signature_help,
            "textDocument/definition" => &mut value.definition,
            "textDocument/declaration" => &mut value.declaration,
            "textDocument/typeDefinition" => &mut value.type_definition,
            "textDocument/implementation" => &mut value.implementation,
            "textDocument/references" => &mut value.references,
            "textDocument/documentSymbol" => &mut value.document_symbols,
            "workspace/symbol" => &mut value.workspace_symbols,
            "textDocument/rename" => &mut value.rename,
            "textDocument/formatting" => &mut value.formatting,
            "textDocument/rangeFormatting" => &mut value.range_formatting,
            "textDocument/codeAction" => &mut value.code_action,
            "workspace/executeCommand" => &mut value.execute_command,
            _ => continue,
        };
        // Static providers still apply to every document attached to the server.
        if !*enabled {
            value.document_selectors.insert(method.into(), options);
        }
        *enabled = true;
    }
    value
}

pub(super) fn expire_requests(
    server: &str,
    stdin: &mut (impl std::io::Write + ?Sized),
    pending: &mut HashMap<i64, PendingRequest>,
    events: &EventSender,
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
                    request_id: request.request_id,
                    method: request.feature.method().into(),
                    message: "language-server request timed out".into(),
                },
            );
        }
    }
}
