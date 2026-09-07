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
            .ui
            .documentation_popup
            .as_ref()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Completion)
        {
            self.ui.documentation_popup = None;
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
                    .settings
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
                .lsp_capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && let Some(resolve_item) = item.resolve_item.clone()
        {
            self.lsp_request_generation = self.lsp_request_generation.saturating_add(1);
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            let Some(server) = self.active_lsp_server.clone() else {
                return;
            };
            if let Err(error) = self.lsp.request(
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
            .lsp_capabilities
            .as_ref()
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
            let mut grouped = BTreeMap::<u32, Vec<ByteRange>>::new();
            for stop in tabstops {
                let start = ByteOffset(start.get().saturating_add(stop.start));
                let end = ByteOffset(insertion_start.unwrap().get().saturating_add(stop.end));
                if let Ok(range) = ByteRange::new(start, end) {
                    grouped.entry(stop.index).or_default().push(range);
                }
            }
            let mut groups = grouped.into_iter().collect::<Vec<_>>();
            groups.sort_by_key(|(index, _)| (*index == 0, *index));
            let groups = groups
                .into_iter()
                .map(|(_, ranges)| ranges)
                .collect::<Vec<_>>();
            if !groups.is_empty() {
                self.snippet_session = Some(SnippetSession {
                    buffer: buffer_id,
                    revision: self.state.buffers[buffer_id].text.revision(),
                    groups,
                    current: 0,
                });
                self.select_snippet_current();
            }
        }
        self.lsp_completion_items.clear();
        self.lsp_completion_owner = None;
        self.ui.language_completion = None;
    }

    pub(in crate::editor::session) fn prepare_snippet_mirror_transaction(
        &mut self,
        transaction: &mut EditorTransaction,
    ) -> Option<Vec<Vec<ByteRange>>> {
        let session = self.snippet_session.clone()?;
        let Some(buffer) = self.state.buffers.get(session.buffer) else {
            self.snippet_session = None;
            return None;
        };
        if buffer.text.revision() != session.revision {
            self.snippet_session = None;
            return None;
        }
        let Some(primary) = session
            .groups
            .get(session.current)
            .and_then(|group| group.first())
            .copied()
        else {
            self.snippet_session = None;
            return None;
        };
        let mut target = None;
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation {
                if *buffer != session.buffer || target.is_some() {
                    self.snippet_session = None;
                    return None;
                }
                target = Some(edits.clone());
            }
        }
        let edits = target?;
        if edits.base_revision() != session.revision
            || edits.edits().is_empty()
            || edits
                .edits()
                .iter()
                .any(|edit| edit.range().start < primary.start || edit.range().end > primary.end)
        {
            self.snippet_session = None;
            return None;
        }
        let Ok(mut replacement) = buffer.text.slice(primary) else {
            self.snippet_session = None;
            return None;
        };
        let mut local = edits
            .edits()
            .iter()
            .map(|edit| {
                (
                    edit.range().start.get() - primary.start.get(),
                    edit.range().end.get() - primary.start.get(),
                    edit.replacement().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        for (start, end, text) in local {
            if !replacement.is_char_boundary(start) || !replacement.is_char_boundary(end) {
                self.snippet_session = None;
                return None;
            }
            replacement.replace_range(start..end, &text);
        }
        let mut mirrors = session.groups[session.current].clone();
        mirrors.sort_by_key(|range| range.start);
        let replacement_edits = mirrors
            .iter()
            .map(|range| TextEdit::new(*range, replacement.clone()))
            .collect::<Vec<_>>();
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation
                && *buffer == session.buffer
            {
                *edits = TextEditSet::new(session.revision, replacement_edits.clone());
            }
        }
        let shift = |position: ByteOffset| {
            let mut value = position.get() as i128;
            for range in &mirrors {
                if range.end <= position {
                    value += replacement.len() as i128 - range.len() as i128;
                }
            }
            ByteOffset(value.max(0) as usize)
        };
        let mut groups = Vec::with_capacity(session.groups.len());
        for group in &session.groups {
            let mut updated = Vec::with_capacity(group.len());
            for range in group {
                if mirrors.contains(range) {
                    let start = shift(range.start);
                    updated.push(
                        ByteRange::new(start, ByteOffset(start.get() + replacement.len())).ok()?,
                    );
                } else {
                    updated.push(ByteRange::new(shift(range.start), shift(range.end)).ok()?);
                }
            }
            groups.push(updated);
        }
        Some(groups)
    }

    pub(in crate::editor::session) fn select_snippet_current(&mut self) {
        let Some(session) = self.snippet_session.as_ref() else {
            return;
        };
        if self
            .state
            .buffers
            .get(session.buffer)
            .is_none_or(|buffer| buffer.text.revision() != session.revision)
        {
            self.snippet_session = None;
            return;
        }
        let Some(range) = session
            .groups
            .get(session.current)
            .and_then(|group| group.first())
            .copied()
        else {
            return;
        };
        let Some(view) = self
            .state
            .active_view
            .filter(|view| self.state.views[*view].buffer == session.buffer)
        else {
            return;
        };
        self.state.views[view].cursor = range.end;
        self.state.views[view].selection = Some(crate::editor::Selection {
            anchor: range.start,
            head: range.end,
            kind: crate::editor::SelectionKind::Character,
        });
        self.ensure_cursor_visible();
    }

    pub(in crate::editor::session) fn navigate_snippet(&mut self, direction: i8) {
        let Some(session) = self.snippet_session.as_mut() else {
            return;
        };
        if direction > 0 {
            if session.current + 1 >= session.groups.len() {
                self.snippet_session = None;
                return;
            }
            session.current += 1
        } else if direction < 0 {
            session.current = session.current.saturating_sub(1)
        }
        self.select_snippet_current();
    }
}
