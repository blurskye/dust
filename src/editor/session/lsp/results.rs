use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn request_lsp_inlay_hints(&mut self) {
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.language.projection() else {
            return;
        };
        if self.ui.large_file_buffers.contains(&owner.buffer)
            || !self
                .language
                .sessions()
                .get(&owner.server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.inlay_hints)
        {
            self.ui.lsp_inlay_hints.clear();
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if buffer == owner.buffer {
            self.language.request_inlays(document);
        }
    }

    pub(in crate::editor::session) fn request_lsp(&mut self, mut feature: LspFeature) {
        if matches!(feature, LspFeature::InlayHints { .. }) {
            self.schedule_lsp_inlay_refresh(true);
            self.process_lsp_inlay_refresh();
            return;
        }
        self.close_documentation();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        let server = self.language.supporting_server(buffer, &document);
        let Some(server) = server else {
            self.start_lsp(None);
            return;
        };
        if self.language.active() != Some(&server) {
            self.select_lsp_session(&server);
        }
        // The application owns a session record while its process is still
        // initializing. Do not expose that record as a runnable server or
        // send a request that can race the worker's startup/disconnect path.
        // `Running` publishes capabilities before this becomes requestable.
        if self
            .language
            .sessions()
            .get(&server)
            .is_none_or(|session| session.capabilities.is_none())
        {
            return;
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            let encoding = self
                .language
                .capabilities()
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
        self.refresh_lsp_projection_owner();
        match self
            .language
            .request_projected(&server, feature, document, offset)
        {
            Ok(()) => (),
            // The worker has already reported (or will imminently report) its
            // real startup/exit failure. Avoid replacing that diagnostic with
            // a misleading raw transport-key "not running" error.
            Err(crate::language::lsp::LspError::NotRunning(_)) => {
                self.language.stopped(&server);
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
        if kind != DocumentationPopupKind::Completion {
            self.reconcile_editor_windows();
            self.input.hold_focus(self.window_session.capture_focus());
        }
        self.input.present_documentation(
            kind,
            title,
            provider,
            source_range,
            source_sections.into_iter().map(|source| {
                (
                    match source.kind {
                        LspMarkupKind::PlainText => DocumentationMarkupKind::PlainText,
                        LspMarkupKind::Markdown => DocumentationMarkupKind::Markdown,
                    },
                    source.value,
                )
            }),
            &self.configuration.settings().ui.documentation_popup,
        );
        self.restore_input_focus();
        self.project_input_documentation();
    }

    pub(in crate::editor::session) fn open_lsp_completion_documentation(&mut self) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        let (server, label, documentation) = match self
            .language
            .completion_documentation(&self.state, selected)
        {
            Ok(value) => value,
            Err(error) => {
                if error == "completion documentation is stale" {
                    self.close_documentation();
                }
                self.show_error(error);
                return;
            }
        };
        let server_label = self.lsp_server_label(&server);
        self.present_documentation_popup(
            DocumentationPopupKind::Completion,
            self.message(
                "lsp_completion_documentation_title",
                &[("server", server_label), ("label", label)],
            ),
            server,
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
        self.present_lsp_result_owned(server, generation, method, uri, result, None);
    }
    pub(in crate::editor::session) fn present_lsp_result_owned(
        &mut self,
        server: &str,
        generation: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
        context: Option<crate::language::lsp::EditContext>,
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
                self.language.install_completions(items, generation);
                self.ui.language_completion =
                    (!self.language.completion_items().is_empty()).then(|| {
                        LanguageCompletionState {
                            items: self
                                .language
                                .completion_items()
                                .iter()
                                .map(|item| CompletionItem {
                                    label: item.label.clone(),
                                    detail: Self::lsp_completion_detail(item, &server_label),
                                })
                                .collect(),
                            selected: 0,
                            scroll: 0,
                        }
                    });
                if self.ui.language_completion.is_none() {
                    self.show_error(self.message("lsp_no_result", &[]));
                } else {
                    self.refresh_lsp_completion_documentation();
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                if self.language.accept_resolved(generation, item, &self.state) {
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
                self.begin_workspace_proposal_owned(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                    context,
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.open_lsp_code_actions_picker(server, actions, context);
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.language.record_pull(server, uri, id);
                }
                if result.unchanged {
                    return;
                }
                let normalized = context
                    .as_ref()
                    .ok_or_else(|| "pull diagnostics lack a requesting context".to_string())
                    .and_then(|context| {
                        self.language.diagnostic_batch(
                            context,
                            &self.state,
                            (uri, None, result.items),
                            (
                                true,
                                self.configuration
                                    .settings()
                                    .workspace
                                    .maximum_diagnostics_per_owner_uri,
                                &server_label,
                            ),
                        )
                    });
                match normalized.and_then(|batch| self.diagnostics.publish(batch, &self.state)) {
                    Ok(()) => self.sync_ui_diagnostics(),
                    Err(error) => self.append_lsp_log(error),
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                if !self.language.record_semantic(server, uri, &result) {
                    return;
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
                    let legend = self
                        .language
                        .capabilities()
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
                if let Some(full) = self.language.semantic_delta(server, uri, delta) {
                    self.present_lsp_result(
                        server,
                        generation,
                        method,
                        uri,
                        LspFeatureResult::SemanticTokens(full),
                    );
                } else {
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
                    self.language.inlays_received();
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
                        "textDocument/selectionRange" => {
                            self.language.install_selection_ranges(ranges)
                        }
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Empty => self.show_error(self.message("lsp_no_result", &[])),
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn begin_workspace_proposal(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
    ) {
        self.begin_workspace_proposal_owned(label, edit, response, encoding, None);
    }
    pub(in crate::editor::session) fn begin_workspace_proposal_owned(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        if let Err(error) = self.workspace_edits.check_begin().and_then(|()| {
            self.state
                .window_version()
                .revision()
                .map(|_| ())
                .ok_or_else(|| "editor source revision is exhausted".into())
        }) {
            self.language.reject_reply(response, error.clone());
            self.show_error(error);
            return;
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        let plan = match self
            .language
            .start_edit(edit, encoding, context, response, &documents)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let ticket = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(ticket) => ticket,
            Err(error) => {
                let _ = self
                    .language
                    .finish_edit(&plan.key, false, Some(error.clone()));
                self.show_error(error);
                return;
            }
        };
        let maximum = usize::try_from(
            self.configuration
                .settings()
                .workspace
                .search
                .maximum_file_bytes,
        )
        .unwrap_or(usize::MAX);
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        if let Err(error) = self.workspace_edits.start_files(
            ticket,
            label,
            Some(plan.key),
            plan.all_files,
            plan.files,
            roots,
            maximum,
        ) {
            self.abort_workspace_edit(error);
            return;
        }
        loop {
            let Some((index, file)) = self.workspace_edits.next_file() else {
                break;
            };
            let snapshot =
                if let crate::editor::session::workspace_edits::InputFile::Read(path) = file {
                    self.state.workspace_snapshot(
                        self.workspace_edits
                            .preparing_sources()
                            .expect("admitted source"),
                        path,
                        maximum,
                    )
                } else {
                    Ok(None)
                };
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.abort_workspace_edit(error);
                    return;
                }
            };
            if let Some(snapshot) = snapshot {
                if let Err(error) = self.workspace_edits.open_snapshot(index, snapshot) {
                    self.abort_workspace_edit(error);
                    return;
                }
            } else {
                let admitted = self
                    .workspace_edits
                    .prepare_io(
                        index,
                        &self.workspace,
                        self.configuration.binding_revision_ref(),
                        &self.state,
                    )
                    .and_then(|permit| self.documents.submit_workspace_io(permit));
                if let Err(error) = admitted {
                    self.abort_workspace_edit(error);
                    return;
                }
            }
        }
        if self.workspace_edits.files_ready() {
            self.finish_workspace_proposal();
        } else {
            self.notify_message(self.message("workspace_edit_loading", &[]), false);
        }
    }
    fn check_workspace_edit_origin(&self) -> Result<(), String> {
        let Some(key) = self.workspace_edits.origin() else {
            return Ok(());
        };
        if self.extension.owns_edit(key) {
            return self.extension.check_edit(
                key,
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        self.language.check_edit(key, &documents)
    }
    pub(in crate::editor::session) fn finish_workspace_proposal(&mut self) {
        let result = (|| {
            self.workspace_edits.current_preparation(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            self.check_workspace_edit_origin()?;
            let source = self
                .workspace_edits
                .preparing_sources()
                .ok_or("workspace preparation source missing")?;
            let files = self.workspace_edits.prepared_files()?;
            self.state
                .validate_workspace_sources(source, files.paths())?;
            let key = self
                .workspace_edits
                .preparing_origin()
                .ok_or("workspace source origin missing")?;
            let drafts = self.language.decode_edit(key, files)?;
            self.workspace_edits.finish_preparation(
                drafts,
                self.workspace.edit_scope(),
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        match result {
            Ok(()) => self.show_workspace_proposal(),
            Err(error) => self.abort_workspace_edit(error),
        }
    }
    pub(in crate::editor::session) fn retire_workspace_picker(
        &mut self,
        id: crate::workspace::ProposalId,
    ) {
        if self.workspace_edits.proposal().is_none_or(|p| p.id != id) {
            return;
        }
        let retired = self.workspace_edits.reject();
        for request in retired.requests {
            self.documents.cancel_request(request);
        }
        if let Some(key) = retired.origin {
            let _ = self.finish_workspace_edit_origin(
                &key,
                false,
                Some("workspace preview dismissed".into()),
            );
        }
    }
    fn finish_workspace_edit_origin(
        &mut self,
        key: &crate::editor::session::workspace_edits::OriginKey,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        if self.extension.owns_edit(key) {
            self.extension.finish_edit(key)
        } else {
            self.language.finish_edit(key, applied, reason)
        }
    }
    pub(in crate::editor::session) fn abort_workspace_edit(&mut self, reason: String) {
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
            let _ = self.finish_workspace_edit_origin(&origin, false, Some(reason.clone()));
        }
        self.close_picker();
        self.show_error(reason);
    }
    pub(in crate::editor::session) fn reject_workspace_proposal(&mut self) {
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
            let _ = self.finish_workspace_edit_origin(
                &origin,
                false,
                Some("workspace edit rejected".into()),
            );
        }
        self.close_picker();
        self.notify_message(self.message("workspace_edit_rejected", &[]), false);
    }
    pub(in crate::editor::session) fn accept_workspace_proposal(&mut self) {
        if let Err(error) = self.check_workspace_edit_origin() {
            self.abort_workspace_edit(error);
            return;
        }
        let transaction = self.transaction_id();
        let prepared = (|| {
            let (source, proposal) = self.workspace_edits.approval(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            let changes = self
                .workspace_edits
                .resources()
                .iter()
                .map(|intent| {
                    let target = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                        ResourceIntentKind::Delete => None,
                    };
                    self.state.check_resource_destination(target)?;
                    self.state.workspace_resource_change(source, &intent.source)
                })
                .collect::<Result<Vec<_>, String>>()?;
            let draft = self
                .state
                .prepare_workspace_edit(source, proposal, transaction)?;
            Ok::<_, String>((draft, changes))
        })();
        let (draft, changes) = match prepared {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.workspace_edits.stage_approval(&draft, changes) {
            self.show_error(error);
            return;
        }
        if let Err(error) = self.commit_editor_draft(draft, None, false) {
            self.workspace_edits.abandon_approval();
            self.show_error(error);
            return;
        }
        match self.workspace_edits.accepted(self.state.publication()) {
            Ok(Some(origin)) => {
                if let Err(error) = self.finish_workspace_edit_origin(&origin, true, None) {
                    self.show_error(error);
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        self.close_picker();
        self.notify_message(self.message("workspace_edit_applied", &[]), false);
    }
    pub(in crate::editor::session) fn apply_lsp_workspace_edit(
        &mut self,
        edit: LspWorkspaceEdit,
    ) -> Result<(), String> {
        let id = self.transaction_id();
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::editor::session::lsp_edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                })
            })
            .collect::<Vec<_>>();
        let transaction =
            crate::editor::session::lsp_edits::LspEditSources::completion_transaction(
                edit,
                self.lsp_position_encoding(),
                &documents,
                id,
            )?;
        let draft = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())?;
        self.commit_editor_draft(draft, None, false)?;
        self.notify_message(self.message("lsp_edit_applied", &[]), false);
        Ok(())
    }
}
