use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn request_lsp_inlay_hints(&mut self) {
        self.refresh_lsp_projection_owner();
        self.lsp_inlay_refresh_due = None;
        let Some(owner) = self.lsp_projection_owner.clone() else {
            return;
        };
        if self.ui.large_file_buffers.contains(&owner.buffer) {
            self.ui.lsp_inlay_hints.clear();
            return;
        }
        let Some(capabilities) = self
            .lsp_sessions
            .get(&owner.server)
            .and_then(|session| session.capabilities.as_ref())
            .filter(|capabilities| capabilities.inlay_hints)
            .cloned()
        else {
            self.ui.lsp_inlay_hints.clear();
            return;
        };
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if buffer != owner.buffer || document.uri() != owner.uri {
            return;
        }
        let text = TextBuffer::from_text(&document.text);
        let end = PositionCodec::new(&text)
            .byte_to_lsp(ByteOffset(text.len_bytes()), capabilities.position_encoding)
            .ok()
            .map(|(line, character)| crate::language::lsp::LspPosition { line, character });
        let Some(end) = end else { return };
        let feature = LspFeature::InlayHints {
            range: crate::language::lsp::LspRange {
                start: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 0,
                },
                end,
            },
        };
        self.lsp_inlay_request_generation = self.lsp_inlay_request_generation.saturating_add(1);
        let generation = u64::MAX
            .saturating_sub(self.lsp_inlay_request_generation)
            .max(u64::MAX / 2);
        if self
            .lsp
            .request(&owner.server, feature, document, ByteOffset(0), generation)
            .is_ok()
        {
            self.lsp_inlay_request_owner = Some((generation, owner));
        }
    }

    pub(in crate::editor::session) fn request_lsp(&mut self, mut feature: LspFeature) {
        if matches!(feature, LspFeature::InlayHints { .. }) {
            self.schedule_lsp_inlay_refresh(true);
            self.process_lsp_inlay_refresh();
            return;
        }
        self.ui.documentation_popup = None;
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        let supports = |state: &LspRuntimeState| {
            state.definition.supports_language(&document.language_id)
                && state.documents.contains_key(&buffer)
                && document.path.starts_with(&state.root_path)
        };
        let server = self
            .active_lsp_server
            .as_ref()
            .filter(|key| self.lsp_sessions.get(*key).is_some_and(&supports))
            .cloned()
            .or_else(|| {
                let mut matches = self
                    .lsp_sessions
                    .iter()
                    .filter_map(|(key, state)| supports(state).then_some(key.clone()))
                    .collect::<Vec<_>>();
                matches.sort();
                matches.into_iter().next()
            });
        let Some(server) = server else {
            self.start_lsp(None);
            return;
        };
        if self.active_lsp_server.as_ref() != Some(&server) {
            self.select_lsp_session(&server);
        }
        // The application owns a session record while its process is still
        // initializing. Do not expose that record as a runnable server or
        // send a request that can race the worker's startup/disconnect path.
        // `Running` publishes capabilities before this becomes requestable.
        if self
            .lsp_sessions
            .get(&server)
            .is_none_or(|session| session.capabilities.is_none())
        {
            return;
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            let encoding = self
                .lsp_capabilities
                .as_ref()
                .map_or(PositionEncoding::Utf16, |capabilities| {
                    capabilities.position_encoding
                });
            let buffer = TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&buffer);
            let (start, end) = selection.map_or((offset, offset), |selection| {
                if selection.anchor <= selection.head {
                    (selection.anchor, selection.head)
                } else {
                    (selection.head, selection.anchor)
                }
            });
            let Ok((start_line, start_character)) = codec.byte_to_lsp(start, encoding) else {
                return;
            };
            let Ok((end_line, end_character)) = codec.byte_to_lsp(end, encoding) else {
                return;
            };
            feature = LspFeature::CodeAction {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: start_line,
                        character: start_character,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: end_line,
                        character: end_character,
                    },
                },
            };
        }
        if matches!(feature, LspFeature::PullDiagnostics { .. })
            && let Some(previous) = self.lsp_pull_ids.get(&(server.clone(), document.uri()))
        {
            feature = LspFeature::PullDiagnostics {
                previous_result_id: Some(previous.clone()),
            };
        }
        if matches!(feature, LspFeature::SemanticTokensFull)
            && let Some((Some(previous), _)) =
                self.lsp_semantic_raw.get(&(server.clone(), document.uri()))
        {
            feature = LspFeature::SemanticTokensDelta {
                previous_result_id: previous.clone(),
            };
        }
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.lsp_projection_owner.clone() else {
            return;
        };
        self.lsp_request_generation = self.lsp_request_generation.saturating_add(1);
        let generation = self.lsp_request_generation;
        match self
            .lsp
            .request(&server, feature, document, offset, generation)
        {
            Ok(()) => self.lsp_request_owner = Some((generation, owner)),
            // The worker has already reported (or will imminently report) its
            // real startup/exit failure. Avoid replacing that diagnostic with
            // a misleading raw transport-key "not running" error.
            Err(crate::language::lsp::LspError::NotRunning(_)) => {
                if let Some(session) = self.lsp_sessions.get_mut(&server) {
                    session.capabilities = None;
                }
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn present_documentation_popup(
        &mut self,
        kind: DocumentationPopupKind,
        title: String,
        provider: String,
        source_range: Option<ByteRange>,
        source_sections: Vec<LspMarkupContent>,
    ) {
        let policy = &self.configuration.settings.ui.documentation_popup;
        if !policy.enabled {
            self.ui.documentation_popup = None;
            return;
        }
        let mut remaining_lines = policy.maximum_lines;
        let mut remaining_bytes = policy.maximum_bytes;
        let mut sections = Vec::new();
        for source in source_sections.into_iter().take(policy.maximum_sections) {
            if remaining_lines == 0 || remaining_bytes == 0 {
                break;
            }
            let normalized = source.value.replace("\r\n", "\n").replace('\r', "\n");
            let mut lines = Vec::new();
            for line in normalized.split('\n') {
                if remaining_lines == 0 || remaining_bytes == 0 {
                    break;
                }
                let sanitized = line
                    .chars()
                    .filter(|character| !character.is_control() || *character == '\t')
                    .take(8192)
                    .collect::<String>();
                let take = sanitized.len().min(remaining_bytes);
                let mut end = take;
                while !sanitized.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                lines.push(sanitized[..end].to_owned());
                remaining_bytes = remaining_bytes.saturating_sub(end);
                remaining_lines = remaining_lines.saturating_sub(1);
            }
            if !lines.iter().all(|line| line.trim().is_empty()) {
                sections.push(DocumentationSection {
                    heading: None,
                    markup: match source.kind {
                        LspMarkupKind::PlainText => DocumentationMarkupKind::PlainText,
                        LspMarkupKind::Markdown => DocumentationMarkupKind::Markdown,
                    },
                    lines,
                });
            }
        }
        if sections.is_empty() {
            self.ui.documentation_popup = None;
            return;
        }
        self.ui.documentation_popup = Some(DocumentationPopupState {
            kind,
            title,
            provider: provider.chars().take(256).collect(),
            source_range,
            sections,
            scroll: 0,
        });
    }

    pub(in crate::editor::session) fn open_lsp_completion_documentation(&mut self) {
        let Some((generation, owner)) = self.lsp_completion_owner.clone() else {
            self.ui.documentation_popup = None;
            self.show_error("completion documentation is stale");
            return;
        };
        let live_owner = self.state.active_view == Some(owner.view)
            && self
                .state
                .views
                .get(owner.view)
                .is_some_and(|view| view.buffer == owner.buffer)
            && self
                .state
                .buffers
                .get(owner.buffer)
                .is_some_and(|buffer| buffer.text.revision() == owner.revision);
        if generation != self.lsp_request_generation
            || self.lsp_projection_owner.as_ref() != Some(&owner)
            || !live_owner
        {
            self.ui.documentation_popup = None;
            self.show_error("completion documentation is stale");
            return;
        }
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        let Some((label, documentation)) =
            self.lsp_completion_items.get(selected).and_then(|item| {
                item.documentation
                    .clone()
                    .map(|documentation| (item.label.clone(), documentation))
            })
        else {
            self.show_error("selected completion has no documentation");
            return;
        };
        let server_label = self.lsp_server_label(&owner.server);
        self.present_documentation_popup(
            DocumentationPopupKind::Completion,
            self.message(
                "lsp_completion_documentation_title",
                &[("server", server_label), ("label", label)],
            ),
            owner.server,
            None,
            vec![documentation],
        );
    }

    pub(in crate::editor::session) fn present_lsp_result(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
    ) {
        let server_label = self.lsp_server_label(server);
        match result {
            LspFeatureResult::Hover(hover) => {
                let source_range = hover.range.and_then(|range| {
                    let buffer_id = self.state.active_buffer_id()?;
                    let buffer = self.state.buffers.get(buffer_id)?;
                    let codec = PositionCodec::new(&buffer.text);
                    let encoding = self.lsp_position_encoding();
                    let start = codec
                        .lsp_to_byte(range.start.line, range.start.character, encoding)
                        .ok()?;
                    let end = codec
                        .lsp_to_byte(range.end.line, range.end.character, encoding)
                        .ok()?;
                    ByteRange::new(start, end).ok()
                });
                self.present_documentation_popup(
                    DocumentationPopupKind::Hover,
                    self.message("lsp_hover_title", &[("server", server_label.clone())]),
                    server.into(),
                    source_range,
                    hover.sections,
                );
            }
            LspFeatureResult::Signature(content) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Signature,
                    self.message("lsp_signature_title", &[("server", server_label.clone())]),
                    server.into(),
                    None,
                    vec![content],
                );
            }
            LspFeatureResult::Completion(items) => {
                self.lsp_completion_items = items;
                self.lsp_completion_owner = self
                    .lsp_projection_owner
                    .clone()
                    .map(|owner| (generation, owner));
                self.ui.language_completion =
                    (!self.lsp_completion_items.is_empty()).then(|| LanguageCompletionState {
                        items: self
                            .lsp_completion_items
                            .iter()
                            .map(|item| CompletionItem {
                                label: item.label.clone(),
                                detail: Self::lsp_completion_detail(item, &server_label),
                            })
                            .collect(),
                        selected: 0,
                        scroll: 0,
                    });
                if self.ui.language_completion.is_none() {
                    self.show_error(self.message("lsp_no_result", &[]));
                } else {
                    self.refresh_lsp_completion_documentation();
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                if let Some(selected) = self
                    .ui
                    .language_completion
                    .as_ref()
                    .map(|completion| completion.selected)
                    && selected < self.lsp_completion_items.len()
                {
                    self.lsp_completion_items[selected] = item;
                    self.accept_lsp_completion();
                }
            }
            LspFeatureResult::Locations(locations) => {
                if locations.is_empty() {
                    self.show_error(self.message("lsp_no_result", &[]));
                } else if method == "textDocument/references" || locations.len() > 1 {
                    self.open_lsp_locations_picker(server, locations);
                } else if let Some(location) = locations.first() {
                    self.navigate_lsp_location(location);
                }
            }
            LspFeatureResult::Symbols(symbols) => {
                self.open_lsp_symbols_picker(server, symbols);
            }
            LspFeatureResult::WorkspaceEdit(mut edit) => {
                for document in &mut edit.documents {
                    if document.uri.is_empty() {
                        document.uri = uri.into();
                    }
                }
                self.begin_workspace_proposal(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.open_lsp_code_actions_picker(server, actions);
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.lsp_pull_ids.insert((server.into(), uri.into()), id);
                }
                if result.unchanged {
                    return;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((_, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                    && let Ok(owner) = DiagnosticOwner::new(format!("lsp-pull:{server}"))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let diagnostics = result
                        .items
                        .into_iter()
                        .filter_map(|item| {
                            let start = codec
                                .lsp_to_byte(
                                    item.range.start.line,
                                    item.range.start.character,
                                    encoding,
                                )
                                .ok()?;
                            let end = codec
                                .lsp_to_byte(
                                    item.range.end.line,
                                    item.range.end.character,
                                    encoding,
                                )
                                .ok()?;
                            let range = ByteRange::new(start, end).ok()?;
                            Some(Diagnostic {
                                uri: uri.into(),
                                version: Some(buffer.text.revision()),
                                range,
                                severity: match item.severity.unwrap_or(1) {
                                    1 => DiagnosticSeverity::Error,
                                    2 => DiagnosticSeverity::Warning,
                                    3 => DiagnosticSeverity::Information,
                                    _ => DiagnosticSeverity::Hint,
                                },
                                source: item.source.unwrap_or_else(|| server_label.clone()),
                                code: item.code,
                                message: item.message,
                                related: Vec::new(),
                                tags: Vec::new(),
                                fixes: Vec::new(),
                                timestamp: crate::clock::system_time(),
                            })
                        })
                        .collect();
                    let revision = Some(buffer.text.revision());
                    let _ = self.diagnostics.replace(
                        owner,
                        uri.into(),
                        revision,
                        revision,
                        diagnostics,
                    );
                    self.sync_ui_diagnostics();
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                self.lsp_semantic_raw.insert(
                    (server.into(), uri.into()),
                    (result.result_id.clone(), result.data.clone()),
                );
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let legend = self
                        .lsp_capabilities
                        .as_ref()
                        .map(|value| value.semantic_token_types.as_slice())
                        .unwrap_or(&[]);
                    let mut line = 0usize;
                    let mut character = 0usize;
                    let mut spans = Vec::new();
                    for token in result.data.chunks_exact(5).take(100_000) {
                        line = line.saturating_add(token[0] as usize);
                        character = if token[0] == 0 {
                            character.saturating_add(token[1] as usize)
                        } else {
                            token[1] as usize
                        };
                        let Some(name) = legend.get(token[3] as usize) else {
                            continue;
                        };
                        let Ok(start) = codec.lsp_to_byte(line, character, encoding) else {
                            continue;
                        };
                        let Ok(end) = codec.lsp_to_byte(
                            line,
                            character.saturating_add(token[2] as usize),
                            encoding,
                        ) else {
                            continue;
                        };
                        let Ok(range) = ByteRange::new(start, end) else {
                            continue;
                        };
                        spans.push(UiSyntaxSpan {
                            buffer: buffer_id,
                            revision: buffer.text.revision(),
                            range,
                            role: semantic_role(name),
                        });
                    }
                    self.ui.lsp_semantic_spans = spans;
                }
            }
            LspFeatureResult::SemanticTokenDelta(delta) => {
                let key = (server.to_owned(), uri.to_owned());
                let mut valid = false;
                if let Some((result_id, data)) = self.lsp_semantic_raw.get_mut(&key) {
                    valid = true;
                    for edit in delta.edits {
                        if edit.start.saturating_add(edit.delete_count) > data.len()
                            || data
                                .len()
                                .saturating_sub(edit.delete_count)
                                .saturating_add(edit.data.len())
                                > 500_000
                        {
                            valid = false;
                            break;
                        }
                        data.splice(edit.start..edit.start + edit.delete_count, edit.data);
                    }
                    if data.len() % 5 != 0 {
                        valid = false
                    }
                    if valid {
                        *result_id = delta.result_id.clone();
                        let full = crate::language::lsp::LspSemanticTokens {
                            result_id: result_id.clone(),
                            data: data.clone(),
                        };
                        self.present_lsp_result(
                            server,
                            generation,
                            method,
                            uri,
                            LspFeatureResult::SemanticTokens(full),
                        );
                    }
                }
                if !valid {
                    self.lsp_semantic_raw.remove(&key);
                    self.request_lsp(LspFeature::SemanticTokensFull);
                }
            }
            LspFeatureResult::InlayHints(items) => {
                let provisional = items
                    .iter()
                    .any(|item| item.label.to_ascii_lowercase().contains("{unknown}"));
                if items.is_empty() || provisional {
                    self.schedule_lsp_inlay_warmup_retry();
                } else {
                    self.lsp_inlay_empty_retries = 0;
                    self.lsp_inlay_refresh_due = None;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    self.ui.lsp_inlay_hints = items
                        .into_iter()
                        .filter_map(|item| {
                            Some(UiInlayHint {
                                buffer: buffer_id,
                                revision: buffer.text.revision(),
                                position: codec
                                    .lsp_to_byte(
                                        item.position.line,
                                        item.position.character,
                                        encoding,
                                    )
                                    .ok()?,
                                label: item.label,
                            })
                        })
                        .collect();
                }
            }
            LspFeatureResult::CodeLens(items) => {
                let rows = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("code-lens:{index}")),
                                label: item.title,
                                detail: item.command.unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_code_lens_title", &[]), rows);
            }
            LspFeatureResult::Ranges(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let ranges = items
                        .into_iter()
                        .filter_map(|item| {
                            ByteRange::new(
                                codec
                                    .lsp_to_byte(item.start.line, item.start.character, encoding)
                                    .ok()?,
                                codec
                                    .lsp_to_byte(item.end.line, item.end.character, encoding)
                                    .ok()?,
                            )
                            .ok()
                        })
                        .collect::<Vec<_>>();
                    match method {
                        "textDocument/documentHighlight" => {
                            self.ui.lsp_highlight_spans = ranges
                                .into_iter()
                                .map(|range| UiSyntaxSpan {
                                    buffer: buffer_id,
                                    revision: buffer.text.revision(),
                                    range,
                                    role: crate::language::syntax::SyntaxRole::Variable,
                                })
                                .collect()
                        }
                        "textDocument/foldingRange" => {
                            if let Some(view) = self.state.active_view {
                                self.ui.lsp_folds = ranges
                                    .into_iter()
                                    .filter_map(|range| {
                                        Some(UiFold {
                                            view,
                                            start_line: buffer
                                                .text
                                                .byte_to_line(range.start)
                                                .ok()?,
                                            end_line: buffer.text.byte_to_line(range.end).ok()?,
                                            collapsed: false,
                                        })
                                    })
                                    .collect()
                            }
                        }
                        "textDocument/selectionRange" => self.lsp_selection_ranges = ranges,
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Empty => self.show_error(self.message("lsp_no_result", &[])),
        }
    }
}
