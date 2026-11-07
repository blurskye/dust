//! LSP protocol parsing and serialization.

use std::path::Path;

use serde_json::Value;

use serde_json::json;

use crate::editor::text::ByteOffset;

use crate::editor::text::PositionCodec;

use crate::editor::text::PositionEncoding;

use crate::editor::text::TextBuffer;

use crate::process::write_content_length_message;

use crate::language::lsp::MAX_LOG_LINE_BYTES;

use crate::language::lsp::markdown::sanitize_markdown;

use crate::language::lsp::server::LspCapabilities;

use crate::language::lsp::messages::LspCodeAction;

use crate::language::lsp::messages::LspCodeLens;

use crate::language::lsp::messages::LspColor;

use crate::language::lsp::messages::LspCommand;

use crate::language::lsp::messages::LspCompletionItem;

use crate::language::lsp::messages::LspDiagnostic;

use crate::language::lsp::messages::LspDocument;

use crate::language::lsp::messages::LspDocumentEdit;

use crate::language::lsp::messages::LspEvent;

use crate::language::lsp::messages::LspFeature;

use crate::language::lsp::messages::LspFeatureResult;

use crate::language::lsp::messages::LspHover;

use crate::language::lsp::messages::LspInlayHint;

use crate::language::lsp::messages::LspLocation;

use crate::language::lsp::messages::LspMarkupContent;

use crate::language::lsp::messages::LspMarkupKind;

use crate::language::lsp::messages::LspPosition;

use crate::language::lsp::messages::LspProgressKind;

use crate::language::lsp::messages::LspPullDiagnostics;

use crate::language::lsp::messages::LspRange;

use crate::language::lsp::messages::LspResourceOperation;

use crate::language::lsp::messages::LspSemanticTokenDelta;

use crate::language::lsp::messages::LspSemanticTokenEdit;

use crate::language::lsp::messages::LspSemanticTokens;

use crate::language::lsp::messages::LspSymbol;

use crate::language::lsp::messages::LspTextEdit;

use crate::language::lsp::messages::LspWorkspaceEdit;

use crate::language::lsp::server::SyncKind;

use crate::language::lsp::origins::EditContext;

use crate::language::lsp::origins::EventSender;

use crate::language::lsp::paths::file_uri;

use crate::language::lsp::session::PendingRequest;

/// `tags` is in `resolveSupport` because rust-analyzer only advertises resolve, and with it
/// auto-imports, when the client defers a field it recognises. Dust doesn't read tags.
/// Pull diagnostics are not advertised: Dust only pulls on an explicit command, and servers such
/// as rust-analyzer stop pushing their own diagnostics to a client that says it pulls.
pub(super) fn initialize_message(root: &Path, initialization_options: Option<&Value>) -> Value {
    let mut message = json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{
            "processId":std::process::id(),
            "clientInfo":{"name":"dust","version":env!("CARGO_PKG_VERSION")},
            "rootUri":file_uri(root),
            "workspaceFolders":[{"uri":file_uri(root),"name":root.file_name().and_then(|v|v.to_str()).unwrap_or("workspace")}],
            "capabilities":{
                "general":{"positionEncodings":["utf-8","utf-16","utf-32"]},
                "workspace":{"workspaceFolders":true,"configuration":true,"inlayHint":{"refreshSupport":true},"applyEdit":true,"executeCommand":{"dynamicRegistration":false},"symbol":{"dynamicRegistration":true}},
                "window":{"workDoneProgress":true},
                "experimental":{"serverStatusNotification":true},
                "textDocument":{
                    "synchronization":{"didSave":true,"dynamicRegistration":false},
                    "semanticTokens":{
                        "dynamicRegistration":false,
                        "requests":{"full":{"delta":true},"range":false},
                        "tokenTypes":["namespace","type","class","enum","interface","struct","typeParameter","parameter","variable","property","enumMember","event","function","method","macro","keyword","modifier","comment","string","number","regexp","operator","decorator"],
                        "tokenModifiers":[],
                        "formats":["relative"],
                        "overlappingTokenSupport":false,
                        "multilineTokenSupport":false
                    },
                    "completion":{"dynamicRegistration":true,"completionItem":{"snippetSupport":true,"documentationFormat":["markdown","plaintext"],"resolveSupport":{"properties":["additionalTextEdits","tags"]}}},
                    "hover":{"dynamicRegistration":true,"contentFormat":["markdown","plaintext"]},
                    "signatureHelp":{"dynamicRegistration":true},
                    "definition":{"dynamicRegistration":true,"linkSupport":true},
                    "declaration":{"dynamicRegistration":true,"linkSupport":true},
                    "typeDefinition":{"dynamicRegistration":true,"linkSupport":true},
                    "implementation":{"dynamicRegistration":true,"linkSupport":true},
                    "references":{"dynamicRegistration":true},
                    "documentSymbol":{"dynamicRegistration":true,"hierarchicalDocumentSymbolSupport":true},
                    "publishDiagnostics":{"relatedInformation":true,"versionSupport":true},
                    "rename":{"dynamicRegistration":true,"prepareSupport":false},
                    "formatting":{"dynamicRegistration":true},
                    "rangeFormatting":{"dynamicRegistration":true},
                    "codeAction":{"dynamicRegistration":true,"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","refactor","source"]}}},
                    "inlayHint":{"dynamicRegistration":true},
                    "colorProvider":{"dynamicRegistration":true}
                }
            }
        }
    });
    if let Some(options) = initialization_options {
        message["params"]["initializationOptions"] = options.clone();
    }
    message
}

pub(super) fn initialize_capabilities(result: Option<&Value>) -> Result<LspCapabilities, String> {
    let capabilities = result
        .and_then(|value| value.get("capabilities"))
        .ok_or_else(|| "initialize response has no capabilities".to_owned())?;
    let encoding = capabilities
        .get("positionEncoding")
        .and_then(Value::as_str)
        .and_then(PositionEncoding::parse_lsp)
        .unwrap_or_default();
    let sync = capabilities.get("textDocumentSync");
    let sync_value = sync
        .and_then(Value::as_u64)
        .or_else(|| {
            sync.and_then(|value| value.get("change"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let sync_kind = match sync_value {
        0 => SyncKind::None,
        2 => SyncKind::Incremental,
        _ => SyncKind::Full,
    };
    Ok(LspCapabilities {
        position_encoding: encoding,
        sync_kind,
        completion: capability(capabilities, "completionProvider"),
        completion_resolve: capabilities
            .get("completionProvider")
            .and_then(|value| value.get("resolveProvider"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        completion_triggers: capabilities
            .get("completionProvider")
            .and_then(|value| value.get("triggerCharacters"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
        hover: capability(capabilities, "hoverProvider"),
        signature_help: capability(capabilities, "signatureHelpProvider"),
        definition: capability(capabilities, "definitionProvider"),
        declaration: capability(capabilities, "declarationProvider"),
        type_definition: capability(capabilities, "typeDefinitionProvider"),
        implementation: capability(capabilities, "implementationProvider"),
        references: capability(capabilities, "referencesProvider"),
        document_symbols: capability(capabilities, "documentSymbolProvider"),
        workspace_symbols: capability(capabilities, "workspaceSymbolProvider"),
        rename: capability(capabilities, "renameProvider"),
        formatting: capability(capabilities, "documentFormattingProvider"),
        range_formatting: capability(capabilities, "documentRangeFormattingProvider"),
        code_action: capability(capabilities, "codeActionProvider"),
        execute_command: capability(capabilities, "executeCommandProvider"),
        semantic_tokens: capabilities
            .pointer("/semanticTokensProvider/full")
            .is_some_and(|v| v == &Value::Bool(true) || v.is_object()),
        semantic_tokens_delta: capabilities
            .pointer("/semanticTokensProvider/full/delta")
            .and_then(Value::as_bool)
            == Some(true),
        inlay_hints: capability(capabilities, "inlayHintProvider"),
        pull_diagnostics: capability(capabilities, "diagnosticProvider"),
        code_lens: capability(capabilities, "codeLensProvider"),
        code_lens_resolve: capabilities
            .pointer("/codeLensProvider/resolveProvider")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        document_highlights: capability(capabilities, "documentHighlightProvider"),
        folding_ranges: capability(capabilities, "foldingRangeProvider"),
        selection_ranges: capability(capabilities, "selectionRangeProvider"),
        document_colors: capability(capabilities, "colorProvider"),
        semantic_token_types: capabilities
            .pointer("/semanticTokensProvider/legend/tokenTypes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(256)
            .filter_map(Value::as_str)
            .map(|value| value.chars().take(128).collect())
            .collect(),
        document_selectors: Default::default(),
        sync_registrations: Vec::new(),
    })
}

pub(super) fn capability(capabilities: &Value, key: &str) -> bool {
    match capabilities.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

pub(super) fn send_open(
    stdin: &mut (impl std::io::Write + ?Sized),
    document: &LspDocument,
) -> Result<(), crate::process::ProtocolError> {
    write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":document.uri(),"languageId":document.language_id,"version":document.version,"text":document.text}}
        }),
    )
}

pub(super) fn send_change(
    stdin: &mut (impl std::io::Write + ?Sized),
    previous: Option<&LspDocument>,
    document: &LspDocument,
    capabilities: &LspCapabilities,
) -> Result<(), crate::process::ProtocolError> {
    let sync_kind = capabilities.sync_kind_for_document(document);
    if sync_kind == SyncKind::None {
        return Ok(());
    }
    let content_changes = if sync_kind == SyncKind::Incremental {
        previous
            .and_then(|old| incremental_change(old, document, capabilities.position_encoding))
            .map_or_else(|| json!([{"text":document.text}]), |change| json!([change]))
    } else {
        json!([{"text":document.text}])
    };
    write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didChange",
            "params":{"textDocument":{"uri":document.uri(),"version":document.version},"contentChanges":content_changes}
        }),
    )
}

pub(super) fn incremental_change(
    old: &LspDocument,
    new: &LspDocument,
    encoding: PositionEncoding,
) -> Option<Value> {
    let prefix = old
        .text
        .bytes()
        .zip(new.text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut prefix = prefix;
    while prefix > 0 && (!old.text.is_char_boundary(prefix) || !new.text.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let old_tail = &old.text[prefix..];
    let new_tail = &new.text[prefix..];
    let suffix = old_tail
        .bytes()
        .rev()
        .zip(new_tail.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(old_tail.len())
        .min(new_tail.len());
    let mut old_end = old.text.len().saturating_sub(suffix);
    let mut new_end = new.text.len().saturating_sub(suffix);
    while old_end < old.text.len() && !old.text.is_char_boundary(old_end) {
        old_end += 1;
    }
    while new_end < new.text.len() && !new.text.is_char_boundary(new_end) {
        new_end += 1;
    }
    if old_end < prefix || new_end < prefix {
        return None;
    }
    let old_buffer = TextBuffer::from_text(&old.text);
    let codec = PositionCodec::new(&old_buffer);
    let (start_line, start_character) = codec.byte_to_lsp(ByteOffset(prefix), encoding).ok()?;
    let (end_line, end_character) = codec.byte_to_lsp(ByteOffset(old_end), encoding).ok()?;
    Some(json!({
        "range":{"start":{"line":start_line,"character":start_character},"end":{"line":end_line,"character":end_character}},
        "rangeLength": protocol_units(&old.text[prefix..old_end], encoding),
        "text":new.text[prefix..new_end]
    }))
}

pub(super) fn protocol_units(text: &str, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => text.len(),
        PositionEncoding::Utf16 => text.encode_utf16().count(),
        PositionEncoding::Utf32 => text.chars().count(),
    }
}

pub(super) fn send_save(
    stdin: &mut (impl std::io::Write + ?Sized),
    document: &LspDocument,
) -> Result<(), crate::process::ProtocolError> {
    write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didSave",
            "params":{"textDocument":{"uri":document.uri()},"text":document.text}
        }),
    )
}

pub(super) fn send_close(
    stdin: &mut (impl std::io::Write + ?Sized),
    uri: &str,
) -> Result<(), crate::process::ProtocolError> {
    write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}
        }),
    )
}

pub(super) fn send_cancel(stdin: &mut (impl std::io::Write + ?Sized), id: i64) {
    let _ = write_content_length_message(
        stdin,
        &json!({
            "jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}
        }),
    );
}

pub(super) fn feature_params(
    feature: &LspFeature,
    uri: &str,
    line: usize,
    character: usize,
) -> Value {
    let document = json!({"uri":uri});
    let position = json!({"line":line,"character":character});
    match feature {
        LspFeature::CompletionResolve { item } | LspFeature::CodeLensResolve { item } => {
            item.clone()
        }
        LspFeature::WorkspaceSymbols { query } => json!({"query":query}),
        LspFeature::ExecuteCommand(command) => {
            json!({"command":command.command,"arguments":command.arguments})
        }
        LspFeature::DocumentSymbols => json!({"textDocument":document}),
        LspFeature::Formatting {
            tab_size,
            insert_spaces,
        } => json!({
            "textDocument":document,"options":{"tabSize":tab_size,"insertSpaces":insert_spaces}
        }),
        LspFeature::References => json!({
            "textDocument":document,"position":position,"context":{"includeDeclaration":true}
        }),
        LspFeature::Rename { new_name } => json!({
            "textDocument":document,"position":position,"newName":new_name
        }),
        LspFeature::RangeFormatting {
            range,
            tab_size,
            insert_spaces,
        } => json!({
            "textDocument":document,"range":range_json(range),"options":{"tabSize":tab_size,"insertSpaces":insert_spaces}
        }),
        LspFeature::CodeAction { range, diagnostics } => json!({
            "textDocument":document,"range":range_json(range),
            "context":{"diagnostics":diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>()}
        }),
        LspFeature::PullDiagnostics { previous_result_id } => json!({
            "textDocument":document,"previousResultId":previous_result_id
        }),
        LspFeature::SemanticTokensDelta { previous_result_id } => {
            json!({"textDocument":document,"previousResultId":previous_result_id})
        }
        LspFeature::SemanticTokensFull
        | LspFeature::CodeLens
        | LspFeature::FoldingRanges
        | LspFeature::DocumentColors => {
            json!({"textDocument":document})
        }
        LspFeature::InlayHints { range } => {
            json!({"textDocument":document,"range":range_json(range)})
        }
        LspFeature::SelectionRanges => json!({"textDocument":document,"positions":[position]}),
        _ => json!({"textDocument":document,"position":position}),
    }
}

fn diagnostic_json(diagnostic: &LspDiagnostic) -> Value {
    let mut value = json!({"range":range_json(&diagnostic.range),"message":diagnostic.message});
    if let Some(severity) = diagnostic.severity {
        value["severity"] = json!(severity);
    }
    if let Some(code) = &diagnostic.code {
        value["code"] = code.clone();
    }
    if let Some(source) = &diagnostic.source {
        value["source"] = json!(source);
    }
    if let Some(data) = &diagnostic.data {
        value["data"] = data.clone();
    }
    value
}

pub(super) fn range_json(range: &LspRange) -> Value {
    json!({"start":{"line":range.start.line,"character":range.start.character},
        "end":{"line":range.end.line,"character":range.end.character}})
}

pub(super) fn handle_response(
    server: &str,
    request: PendingRequest,
    message: Value,
    events: &EventSender,
) {
    let events = &EventSender {
        context: EditContext {
            encoding: request.encoding,
            sources: request.sources.clone(),
            ..events.context.clone()
        },
        ..events.clone()
    };
    if let Some(error) = message.get("error") {
        send_event(
            events,
            LspEvent::RequestFailed {
                server: server.into(),
                request_id: request.request_id,
                method: request.feature.method().into(),
                message: error.to_string(),
            },
        );
        return;
    }
    let result = message.get("result").unwrap_or(&Value::Null);
    let normalized = normalize_result(&request.feature, result);
    match normalized {
        Ok(result) => send_event(
            events,
            LspEvent::Result {
                server: server.into(),
                request_id: request.request_id,
                method: request.feature.method().into(),
                uri: request.document.uri(),
                version: request.document.version,
                result,
            },
        ),
        Err(message) => send_event(
            events,
            LspEvent::RequestFailed {
                server: server.into(),
                request_id: request.request_id,
                method: request.feature.method().into(),
                message,
            },
        ),
    }
}

pub(super) fn normalize_result(
    feature: &LspFeature,
    result: &Value,
) -> Result<LspFeatureResult, String> {
    if result.is_null() {
        return Ok(match feature {
            LspFeature::InlayHints { .. } => LspFeatureResult::InlayHints(Vec::new()),
            LspFeature::DocumentColors => LspFeatureResult::Colors(Vec::new()),
            LspFeature::DocumentHighlights
            | LspFeature::FoldingRanges
            | LspFeature::SelectionRanges => LspFeatureResult::Ranges(Vec::new()),
            LspFeature::SemanticTokensFull | LspFeature::SemanticTokensDelta { .. } => {
                LspFeatureResult::SemanticTokens(LspSemanticTokens {
                    result_id: None,
                    data: Vec::new(),
                })
            }
            _ => LspFeatureResult::Empty,
        });
    }
    match feature {
        LspFeature::Hover => Ok(LspFeatureResult::Hover(hover_result(result))),
        LspFeature::Completion => Ok(LspFeatureResult::Completion {
            items: completion_items(result)?,
            incomplete: result
                .get("isIncomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        LspFeature::CompletionResolve { .. } => Ok(LspFeatureResult::CompletionResolved(Box::new(
            completion_item(result, true)?,
        ))),
        LspFeature::SignatureHelp => Ok(LspFeatureResult::Signature(LspMarkupContent {
            kind: LspMarkupKind::PlainText,
            value: signature_text(result),
        })),
        LspFeature::Definition
        | LspFeature::Declaration
        | LspFeature::TypeDefinition
        | LspFeature::Implementation
        | LspFeature::References => Ok(LspFeatureResult::Locations(locations(result)?)),
        LspFeature::DocumentSymbols | LspFeature::WorkspaceSymbols { .. } => {
            Ok(LspFeatureResult::Symbols(symbols(result)?))
        }
        LspFeature::Rename { .. } => Ok(LspFeatureResult::WorkspaceEdit(workspace_edit(result)?)),
        LspFeature::Formatting { .. } | LspFeature::RangeFormatting { .. } => {
            Ok(LspFeatureResult::WorkspaceEdit(LspWorkspaceEdit {
                resources: Vec::new(),
                documents: vec![LspDocumentEdit {
                    uri: String::new(),
                    version: None,
                    edits: text_edits(result)?,
                }],
            }))
        }
        LspFeature::CodeAction { .. } => Ok(LspFeatureResult::CodeActions(code_actions(result)?)),
        LspFeature::PullDiagnostics { .. } => {
            Ok(LspFeatureResult::PullDiagnostics(LspPullDiagnostics {
                result_id: result
                    .get("resultId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                items: diagnostics(result.get("items").unwrap_or(&Value::Null)),
                unchanged: result.get("kind").and_then(Value::as_str) == Some("unchanged"),
            }))
        }
        LspFeature::SemanticTokensFull => {
            let values = result
                .get("data")
                .and_then(Value::as_array)
                .ok_or("semantic token data is not an array")?;
            if values.len() > 500_000 || values.len() % 5 != 0 {
                return Err("semantic token data has invalid tuple count".into());
            }
            let data = values
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or("semantic token value is invalid")
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(LspFeatureResult::SemanticTokens(LspSemanticTokens {
                result_id: result
                    .get("resultId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                data,
            }))
        }
        LspFeature::SemanticTokensDelta { .. } => {
            if result.get("data").is_some() {
                return normalize_result(&LspFeature::SemanticTokensFull, result);
            }
            let edits = result
                .get("edits")
                .and_then(Value::as_array)
                .ok_or("semantic token delta edits are not an array")?;
            if edits.len() > 10_000 {
                return Err("semantic token delta exceeds edit limit".into());
            }
            let mut normalized = Vec::new();
            for edit in edits {
                let start = usize::try_from(
                    edit.get("start")
                        .and_then(Value::as_u64)
                        .ok_or("semantic delta start missing")?,
                )
                .map_err(|_| "semantic delta start invalid")?;
                let delete_count = usize::try_from(
                    edit.get("deleteCount")
                        .and_then(Value::as_u64)
                        .ok_or("semantic delta delete count missing")?,
                )
                .map_err(|_| "semantic delta delete count invalid")?;
                let data = edit
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or("semantic delta value invalid")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if data.len() > 500_000 {
                    return Err("semantic delta insert exceeds limit".into());
                }
                normalized.push(LspSemanticTokenEdit {
                    start,
                    delete_count,
                    data,
                });
            }
            Ok(LspFeatureResult::SemanticTokenDelta(
                LspSemanticTokenDelta {
                    result_id: result
                        .get("resultId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    edits: normalized,
                },
            ))
        }
        LspFeature::InlayHints { .. } => Ok(LspFeatureResult::InlayHints(inlay_hints(result))),
        LspFeature::CodeLens => Ok(LspFeatureResult::CodeLens(code_lenses(result))),
        LspFeature::CodeLensResolve { .. } => {
            let lenses = code_lenses(&json!([result]));
            if lenses.first().is_none_or(|lens| lens.command.is_none()) {
                return Err("resolved code lens has no valid command".into());
            }
            Ok(LspFeatureResult::CodeLens(lenses))
        }
        LspFeature::ExecuteCommand(_) => Ok(LspFeatureResult::Empty),
        LspFeature::DocumentHighlights => Ok(LspFeatureResult::Ranges(result_ranges(result))),
        LspFeature::FoldingRanges => Ok(LspFeatureResult::Ranges(folding_ranges(result))),
        LspFeature::SelectionRanges => Ok(LspFeatureResult::Ranges(selection_ranges(result))),
        LspFeature::DocumentColors => Ok(LspFeatureResult::Colors(document_colors(result))),
    }
}

pub(super) fn result_ranges(value: &Value) -> Vec<LspRange> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| parse_range(item.get("range").unwrap_or(item)))
        .collect()
}
pub(super) fn folding_ranges(value: &Value) -> Vec<LspRange> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            Some(LspRange {
                start: LspPosition {
                    line: usize::try_from(item.get("startLine")?.as_u64()?).ok()?,
                    character: usize::try_from(
                        item.get("startCharacter")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .ok()?,
                },
                end: LspPosition {
                    line: usize::try_from(item.get("endLine")?.as_u64()?).ok()?,
                    character: usize::try_from(
                        item.get("endCharacter")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .ok()?,
                },
            })
        })
        .collect()
}
pub(super) fn selection_ranges(value: &Value) -> Vec<LspRange> {
    let mut out = Vec::new();
    for item in value.as_array().into_iter().flatten().take(10_000) {
        let mut current = Some(item);
        for _ in 0..64 {
            let Some(value) = current else { break };
            if let Some(range) = value.get("range").and_then(parse_range) {
                out.push(range);
                if out.len() >= 10_000 {
                    return out;
                }
            }
            current = value.get("parent");
        }
    }
    out
}
pub(super) fn document_colors(value: &Value) -> Vec<LspColor> {
    // Channels arrive as 0..1 floats; alpha is ignored because the terminal cannot blend.
    let channel = |color: &Value, key: &str| {
        let value = color.get(key)?.as_f64()?;
        value
            .is_finite()
            .then(|| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            let color = item.get("color")?;
            Some(LspColor {
                range: parse_range(item.get("range")?)?,
                rgb: (
                    channel(color, "red")?,
                    channel(color, "green")?,
                    channel(color, "blue")?,
                ),
            })
        })
        .collect()
}
pub(super) fn inlay_hints(value: &Value) -> Vec<LspInlayHint> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            let value = item.get("label")?;
            let label: String = if let Some(text) = value.as_str() {
                text.to_owned()
            } else {
                value
                    .as_array()?
                    .iter()
                    .filter_map(|part| part.get("value").and_then(Value::as_str))
                    .collect()
            };
            let mut label = sanitize_log(&label, 4096);
            if item
                .get("paddingLeft")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !label.starts_with(char::is_whitespace)
            {
                label.insert(0, ' ');
            }
            if item
                .get("paddingRight")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !label.ends_with(char::is_whitespace)
            {
                label.push(' ');
            }
            let position = parse_position(item.get("position")?)?;
            (!label.is_empty()).then_some(LspInlayHint { position, label })
        })
        .collect()
}
fn parse_command(value: &Value) -> Option<LspCommand> {
    Some(LspCommand {
        command: value.get("command")?.as_str()?.to_owned(),
        arguments: value
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

pub(super) fn code_lenses(value: &Value) -> Vec<LspCodeLens> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|item| {
            let command = item.get("command");
            Some(LspCodeLens {
                range: parse_range(item.get("range")?)?,
                title: command
                    .and_then(|value| value.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(4096)
                    .collect(),
                command: command.and_then(parse_command),
                resolve_item: command.is_none_or(Value::is_null).then(|| item.clone()),
            })
        })
        .collect()
}

pub(super) fn completion_items(result: &Value) -> Result<Vec<LspCompletionItem>, String> {
    let items = result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array))
        .ok_or_else(|| "completion response is not an item list".to_owned())?;
    items
        .iter()
        .take(10_000)
        .map(|item| completion_item(item, false))
        .collect()
}
pub(super) fn completion_item(item: &Value, resolved: bool) -> Result<LspCompletionItem, String> {
    let encoded = serde_json::to_vec(item).map_err(|_| "completion item is not serializable")?;
    if encoded.len() > 64 * 1024 {
        return Err("completion item exceeds byte limit".into());
    }
    let label = item
        .get("label")
        .and_then(Value::as_str)
        .ok_or("completion item has no label")?;
    let insert_text = item
        .get("insertText")
        .and_then(Value::as_str)
        .unwrap_or(label)
        .chars()
        .take(1024 * 1024)
        .collect();
    let documentation = item.get("documentation").and_then(|documentation| {
        let value = documentation
            .as_str()
            .or_else(|| documentation.get("value").and_then(Value::as_str))?;
        let kind = if documentation.get("kind").and_then(Value::as_str) == Some("markdown") {
            LspMarkupKind::Markdown
        } else {
            LspMarkupKind::PlainText
        };
        Some(LspMarkupContent {
            kind,
            value: sanitize_markdown(value, 64 * 1024),
        })
    });
    let text_field = |name: &str| {
        item.get(name)
            .and_then(Value::as_str)
            .map(|value| value.chars().take(4096).collect::<String>())
    };
    Ok(LspCompletionItem {
        filter_text: text_field("filterText"),
        sort_text: text_field("sortText"),
        label: label.chars().take(4096).collect(),
        detail: item
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect(),
        insert_text,
        text_edit: item.get("textEdit").and_then(parse_text_edit),
        additional_edits: item
            .get("additionalTextEdits")
            .map(text_edits)
            .transpose()?
            .unwrap_or_default(),
        snippet: item.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
        kind: item
            .get("kind")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
        documentation,
        command: item.get("command").and_then(parse_command),
        resolve_item: (!resolved).then(|| item.clone()),
        resolved,
    })
}

pub(super) fn locations(result: &Value) -> Result<Vec<LspLocation>, String> {
    let values = result
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(result));
    values
        .iter()
        .take(2048)
        .map(|value| {
            let target = value.get("targetUri").map_or_else(
                || value.clone(),
                |uri| json!({
                    "uri":uri,"range":value.get("targetSelectionRange").or_else(||value.get("targetRange"))
                }),
            );
            let uri = target
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("location has no URI")?
                .to_owned();
            let range = parse_range(target.get("range").ok_or("location has no range")?)
                .ok_or("location range is malformed")?;
            Ok(LspLocation { uri, range })
        })
        .collect()
}

pub(super) fn symbols(result: &Value) -> Result<Vec<LspSymbol>, String> {
    let values = result
        .as_array()
        .ok_or_else(|| "symbol response is not a list".to_owned())?;
    let mut found = Vec::new();
    collect_symbols(values, 0, &mut found)?;
    Ok(found)
}

/// Flat symbols carry a `location`. Nested ones hold a range in the requested document, so their
/// URI stays empty for the caller, and their children follow them indented.
fn collect_symbols(
    values: &[Value],
    depth: usize,
    found: &mut Vec<LspSymbol>,
) -> Result<(), String> {
    for value in values {
        if found.len() >= 4096 || depth > 32 {
            break;
        }
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or("symbol has no name")?;
        let location = value
            .get("location")
            .and_then(|location| locations(location).ok()?.into_iter().next())
            .or_else(|| {
                let range = value.get("selectionRange").or_else(|| value.get("range"))?;
                Some(LspLocation {
                    uri: String::new(),
                    range: parse_range(range)?,
                })
            });
        found.push(LspSymbol {
            name: format!("{}{name}", "  ".repeat(depth)),
            kind: value
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|v| u8::try_from(v).ok()),
            extent: value
                .get("range")
                .and_then(parse_range)
                .or_else(|| location.as_ref().map(|location| location.range.clone())),
            location,
        });
        if let Some(children) = value.get("children").and_then(Value::as_array) {
            collect_symbols(children, depth + 1, found)?;
        }
    }
    Ok(())
}

pub(super) fn workspace_edit(value: &Value) -> Result<LspWorkspaceEdit, String> {
    let mut documents = Vec::new();
    let mut resources = Vec::new();
    let count = value
        .get("changes")
        .and_then(Value::as_object)
        .map_or(0, |c| c.len())
        .saturating_add(
            value
                .get("documentChanges")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        );
    if count > 256 {
        return Err("workspace edit exceeds 256 documents or operations".into());
    }
    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes.iter() {
            documents.push(LspDocumentEdit {
                uri: uri.clone(),
                version: None,
                edits: text_edits(edits)?,
            });
        }
    }
    if let Some(changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in changes.iter() {
            let Some(text_document) = change.get("textDocument") else {
                let kind = change
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let options = change.get("options").unwrap_or(&Value::Null);
                let operation = match kind {
                    "create" => LspResourceOperation::Create {
                        uri: change
                            .get("uri")
                            .and_then(Value::as_str)
                            .ok_or("create has no URI")?
                            .into(),
                        overwrite: options
                            .get("overwrite")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        ignore_if_exists: options
                            .get("ignoreIfExists")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                    "rename" => LspResourceOperation::Rename {
                        old_uri: change
                            .get("oldUri")
                            .and_then(Value::as_str)
                            .ok_or("rename has no oldUri")?
                            .into(),
                        new_uri: change
                            .get("newUri")
                            .and_then(Value::as_str)
                            .ok_or("rename has no newUri")?
                            .into(),
                        overwrite: options
                            .get("overwrite")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        ignore_if_exists: options
                            .get("ignoreIfExists")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                    "delete" => LspResourceOperation::Delete {
                        uri: change
                            .get("uri")
                            .and_then(Value::as_str)
                            .ok_or("delete has no URI")?
                            .into(),
                        recursive: options
                            .get("recursive")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        ignore_if_not_exists: options
                            .get("ignoreIfNotExists")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                    _ => return Err("unknown workspace resource operation".into()),
                };
                resources.push(operation);
                continue;
            };
            let uri = text_document
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("document edit has no URI")?;
            documents.push(LspDocumentEdit {
                uri: uri.into(),
                version: match text_document.get("version") {
                    None | Some(Value::Null) => None,
                    Some(version) => Some(
                        version
                            .as_u64()
                            .ok_or("document edit version must be a nonnegative integer or null")?,
                    ),
                },
                edits: text_edits(change.get("edits").unwrap_or(&Value::Null))?,
            });
        }
    }
    Ok(LspWorkspaceEdit {
        documents,
        resources,
    })
}

pub(super) fn code_actions(value: &Value) -> Result<Vec<LspCodeAction>, String> {
    let actions = value
        .as_array()
        .ok_or_else(|| "code-action response is not a list".to_owned())?;
    let mut normalized = Vec::new();
    for action in actions.iter().take(256) {
        let Some(title) = action.get("title").and_then(Value::as_str) else {
            continue;
        };
        normalized.push(LspCodeAction {
            title: title.to_owned(),
            kind: action
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned),
            preferred: action
                .get("isPreferred")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            edit: action.get("edit").map(workspace_edit).transpose()?,
            // A server may answer with a bare command in place of an action.
            command: action.get("command").and_then(|command| {
                parse_command(if command.is_string() { action } else { command })
            }),
        });
    }
    Ok(normalized)
}

pub(super) fn text_edits(value: &Value) -> Result<Vec<LspTextEdit>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| "text edits are not a list".to_owned())?;
    if values.len() > 4096 {
        return Err("document edit exceeds 4096 edits".into());
    }
    values
        .iter()
        .map(|value| parse_text_edit(value).ok_or_else(|| "text edit is malformed".to_owned()))
        .collect()
}

pub(super) fn parse_text_edit(value: &Value) -> Option<LspTextEdit> {
    Some(LspTextEdit {
        range: parse_range(
            value
                .get("range")
                .or_else(|| value.get("insert"))
                .or_else(|| value.get("replace"))?,
        )?,
        new_text: value.get("newText")?.as_str()?.to_owned(),
    })
}

pub(super) fn parse_range(value: &Value) -> Option<LspRange> {
    Some(LspRange {
        start: parse_position(value.get("start")?)?,
        end: parse_position(value.get("end")?)?,
    })
}

pub(super) fn parse_position(value: &Value) -> Option<LspPosition> {
    Some(LspPosition {
        line: usize::try_from(value.get("line")?.as_u64()?).ok()?,
        character: usize::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

pub(super) fn diagnostics(value: &Value) -> Vec<LspDiagnostic> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(|diagnostic| {
            Some(LspDiagnostic {
                range: parse_range(diagnostic.get("range")?)?,
                severity: diagnostic
                    .get("severity")
                    .and_then(Value::as_u64)
                    .and_then(|v| u8::try_from(v).ok()),
                code: diagnostic
                    .get("code")
                    .filter(|v| v.is_string() || v.is_number())
                    .cloned(),
                related: diagnostic
                    .get("relatedInformation")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(256)
                    .filter_map(|related| {
                        let location = related.get("location")?;
                        Some((
                            LspLocation {
                                uri: location.get("uri")?.as_str()?.to_owned(),
                                range: parse_range(location.get("range")?)?,
                            },
                            sanitize_log(related.get("message")?.as_str()?, MAX_LOG_LINE_BYTES),
                        ))
                    })
                    .collect(),
                source: diagnostic
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: sanitize_log(
                    diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    MAX_LOG_LINE_BYTES,
                ),
                data: diagnostic.get("data").cloned(),
            })
        })
        .collect()
}

pub(super) fn signature_text(result: &Value) -> String {
    let signatures = result.get("signatures").and_then(Value::as_array);
    let active = result
        .get("activeSignature")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .unwrap_or(0);
    signatures
        .and_then(|items| items.get(active).or_else(|| items.first()))
        .and_then(|signature| signature.get("label"))
        .and_then(Value::as_str)
        .unwrap_or("no signature information")
        .to_owned()
}

pub(super) fn markup_content(value: &Value) -> Option<LspMarkupContent> {
    let (kind, source) = if let Some(source) = value.as_str() {
        (LspMarkupKind::Markdown, source)
    } else {
        let source = value.get("value").and_then(Value::as_str)?;
        let kind = match value.get("kind").and_then(Value::as_str) {
            Some("plaintext") => LspMarkupKind::PlainText,
            // A language-tagged MarkedString contains literal code, not Markdown.
            None if value.get("language").and_then(Value::as_str).is_some() => {
                LspMarkupKind::PlainText
            }
            _ => LspMarkupKind::Markdown,
        };
        (kind, source)
    };
    Some(LspMarkupContent {
        kind,
        value: sanitize_markdown(source, MAX_LOG_LINE_BYTES),
    })
}

pub(super) fn hover_result(result: &Value) -> LspHover {
    let contents = result.get("contents").unwrap_or(result);
    let mut sections = if let Some(parts) = contents.as_array() {
        parts
            .iter()
            .filter_map(markup_content)
            .take(16)
            .collect::<Vec<_>>()
    } else {
        markup_content(contents).into_iter().collect()
    };
    if sections.is_empty() {
        sections.push(LspMarkupContent {
            kind: LspMarkupKind::PlainText,
            value: "no hover information".into(),
        });
    }
    LspHover {
        sections,
        range: result.get("range").and_then(parse_range),
    }
}

pub(super) fn sanitize_log(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(limit)
        .collect()
}

/// rust-analyzer's `experimental/serverStatus`, which says when the workspace loaded only in
/// part, for example because build scripts or proc macros failed.
pub(super) fn server_status_event(server: &str, message: &Value) -> Option<LspEvent> {
    let healthy = match message.pointer("/params/health").and_then(Value::as_str)? {
        "ok" => true,
        "warning" | "error" => false,
        _ => return None,
    };
    Some(LspEvent::ServerStatus {
        server: server.into(),
        healthy,
        message: sanitize_log(
            message
                .pointer("/params/message")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            MAX_LOG_LINE_BYTES,
        ),
    })
}

pub(super) fn lsp_progress_event(server: &str, message: &Value) -> Option<LspEvent> {
    let token = message.pointer("/params/token")?;
    if let Some(text) = token.as_str() {
        if text.is_empty() || text.chars().count() > 128 || text.chars().any(char::is_control) {
            return None;
        }
    } else if token.as_i64().is_none() && token.as_u64().is_none() {
        return None;
    }
    // JSON quoting keeps string tokens distinct from numeric tokens in progress rows.
    let token = token.to_string();
    let value = message.pointer("/params/value")?;
    let kind = match value.get("kind").and_then(Value::as_str)? {
        "begin" => LspProgressKind::Begin,
        "report" => LspProgressKind::Report,
        "end" => LspProgressKind::End,
        _ => return None,
    };
    let one_line = |field: &str| {
        sanitize_log(
            value.get(field).and_then(Value::as_str).unwrap_or_default(),
            512,
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
    };
    Some(LspEvent::Progress {
        server: server.into(),
        token,
        kind,
        title: one_line("title"),
        message: one_line("message"),
        percentage: value
            .get("percentage")
            .and_then(Value::as_u64)
            .filter(|percentage| *percentage <= 100)
            .map(|percentage| percentage as u8),
    })
}

pub(super) fn send_event(events: &EventSender, event: LspEvent) {
    events.send(event);
}

pub(super) fn fail_child(
    child: &mut std::process::Child,
    events: &EventSender,
    server: &str,
    message: &str,
) {
    send_event(
        events,
        LspEvent::Failed {
            server: server.into(),
            message: message.into(),
        },
    );
    terminate_server(child);
}

pub(super) fn terminate_server(child: &mut std::process::Child) {
    #[cfg(unix)]
    crate::process::terminate_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}
