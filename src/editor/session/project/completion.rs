use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_completion_detail(
        item: &crate::language::lsp::LspCompletionItem,
        server: &str,
    ) -> String {
        let kind = match item.kind {
            Some(1) => "Text",
            Some(2) => "Method",
            Some(3) => "Function",
            Some(4) => "Constructor",
            Some(5) => "Field",
            Some(6) => "Variable",
            Some(7) => "Class",
            Some(8) => "Interface",
            Some(9) => "Module",
            Some(10) => "Property",
            Some(11) => "Unit",
            Some(12) => "Value",
            Some(13) => "Enum",
            Some(14) => "Keyword",
            Some(15) => "Snippet",
            Some(16) => "Color",
            Some(17) => "File",
            Some(18) => "Reference",
            Some(19) => "Folder",
            Some(20) => "Enum member",
            Some(21) => "Constant",
            Some(22) => "Struct",
            Some(23) => "Event",
            Some(24) => "Operator",
            Some(25) => "Type parameter",
            _ => "Item",
        };
        [kind, item.detail.as_str(), server]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    pub(in crate::editor::session) fn refresh_lsp_completion_documentation(&mut self) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        if self
            .lsp_completion_items
            .get(selected)
            .is_some_and(|item| item.documentation.is_some())
        {
            self.open_lsp_completion_documentation();
        } else if self
            .input
            .documentation()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Completion)
        {
            self.close_documentation();
        }
    }

    pub(in crate::editor::session) fn move_lsp_completion(&mut self, direction: i8) {
        {
            let Some(completion) = &mut self.ui.language_completion else {
                return;
            };
            if completion.items.is_empty() {
                return;
            }
            completion.selected = if direction < 0 {
                completion
                    .selected
                    .checked_sub(1)
                    .unwrap_or(completion.items.len() - 1)
            } else {
                (completion.selected + 1) % completion.items.len()
            };
            let visible_rows = usize::from(
                self.configuration
                    .settings()
                    .ui
                    .command_line
                    .completion_max_rows
                    .max(1),
            );
            if completion.selected < completion.scroll {
                completion.scroll = completion.selected;
            } else if completion.selected >= completion.scroll.saturating_add(visible_rows) {
                completion.scroll = completion
                    .selected
                    .saturating_add(1)
                    .saturating_sub(visible_rows);
            }
        }
        self.refresh_lsp_completion_documentation();
    }

    pub(in crate::editor::session) fn accept_lsp_completion(&mut self) {
        let Some(completion) = &self.ui.language_completion else {
            return;
        };
        let Some(item) = self.lsp_completion_items.get(completion.selected).cloned() else {
            return;
        };
        let Some((buffer_id, document)) = self.active_lsp_document() else {
            return;
        };
        if !item.resolved
            && self
                .language
                .capabilities()
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && let Some(resolve_item) = item.resolve_item.clone()
        {
            self.lsp_request_generation = self.lsp_request_generation.saturating_add(1);
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            let Some(server) = self.language.active().cloned() else {
                return;
            };
            if let Err(error) = self.language.request(
                &server,
                LspFeature::CompletionResolve { item: resolve_item },
                document,
                offset,
                self.lsp_request_generation,
            ) {
                self.show_error(error)
            }
            return;
        }
        let encoding = self
            .language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let snippet = item.snippet;
        let session_allowed = item.additional_edits.is_empty();
        let edit = item.text_edit.unwrap_or_else(|| {
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            let buffer = TextBuffer::from_text(&document.text);
            let (line, character) = PositionCodec::new(&buffer)
                .byte_to_lsp(offset, encoding)
                .unwrap_or((0, 0));
            crate::language::lsp::LspTextEdit {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition { line, character },
                    end: crate::language::lsp::LspPosition { line, character },
                },
                new_text: item.insert_text,
            }
        });
        let mut edit = edit;
        let mut tabstops = Vec::new();
        if snippet {
            match crate::language::lsp::parse_snippet(&edit.new_text, 1024 * 1024) {
                Ok(parsed) => {
                    edit.new_text = parsed.text;
                    tabstops = parsed.tabstops;
                }
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        }
        let insertion_start = TextBuffer::from_text(&document.text);
        let insertion_start = PositionCodec::new(&insertion_start)
            .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
            .ok();
        let mut edits = vec![edit];
        edits.extend(item.additional_edits);
        let workspace_edit = LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![crate::language::lsp::LspDocumentEdit {
                uri: document.uri(),
                version: Some(document.version),
                edits,
            }],
        };
        if let Err(error) = self.apply_lsp_workspace_edit(workspace_edit) {
            self.show_error(error);
        } else if session_allowed
            && let Some(start) = insertion_start
            && !tabstops.is_empty()
        {
            let source = self.state.window_version();
            self.input.install_snippet(
                crate::editor::session::input::SnippetDocument {
                    editor: &source,
                    buffer: buffer_id,
                    text: &self.state.buffers[buffer_id].text,
                    view: self.state.active_view,
                },
                start,
                tabstops
                    .into_iter()
                    .map(|stop| (stop.index, stop.start, stop.end)),
            );
            self.select_snippet_current();
        }
        self.lsp_completion_items.clear();
        self.lsp_completion_owner = None;
        self.ui.language_completion = None;
    }

    pub(in crate::editor::session) fn select_snippet_current(&mut self) {
        let source = self.state.window_version();
        let document = self.input.snippet_buffer().and_then(|id| {
            self.state.buffers.get(id).map(|buffer| {
                crate::editor::session::input::SnippetDocument {
                    editor: &source,
                    buffer: id,
                    text: &buffer.text,
                    view: self.state.active_view.filter(|view| {
                        self.state
                            .views
                            .get(*view)
                            .is_some_and(|view| view.buffer == id)
                    }),
                }
            })
        });
        let Some(operations) = self.input.snippet_selection(document) else {
            return;
        };
        let mut transaction =
            EditorTransaction::new(self.transaction_id()).with_undo_policy(UndoPolicy::Ignore);
        transaction.operations.extend(operations);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
    pub(in crate::editor::session) fn navigate_snippet(&mut self, direction: i8) {
        self.input.navigate_snippet(direction);
        self.select_snippet_current();
    }
}
