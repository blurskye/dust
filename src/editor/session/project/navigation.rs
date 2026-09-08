use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn navigate_workspace_search_match(
        &mut self,
        found: WorkspaceSearchMatch,
    ) {
        if self
            .state
            .buffers
            .iter()
            .any(|(_, buffer)| buffer.path.as_ref() == Some(&found.canonical_path))
        {
            if let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            {
                self.navigate_byte_location(&file_uri(&found.canonical_path), range);
            }
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.documents.read_scoped(found.canonical_path, roots) {
            Ok(id) => {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::WorkspaceSearchNavigation { range: found.range },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_byte_location(
        &mut self,
        uri: &str,
        range: ByteRange,
    ) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer, _)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        let transaction = self.transaction_id();
        match self
            .state
            .prepare_document_location(buffer, range.start, transaction)
            .and_then(|draft| self.commit_editor_draft(draft, None, false))
        {
            Ok(()) => self.ensure_cursor_visible(),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_lsp_location(
        &mut self,
        location: &crate::language::lsp::LspLocation,
    ) {
        let Some(path) = path_from_file_uri(&location.uri) else {
            self.show_error(self.message("lsp_invalid_uri", &[("uri", location.uri.clone())]));
            return;
        };
        if let Err(error) = self.workspace.require_path(&path) {
            self.show_error(error);
            return;
        }
        let encoding = self
            .language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            match self.documents.read_scoped(path, roots) {
                Ok(id) => {
                    self.pending_vfs.insert(
                        id,
                        PendingVfs::LspNavigation {
                            range: location.range.clone(),
                            encoding,
                        },
                    );
                    self.notify_message(self.message("opening_file", &[]), false);
                }
                Err(error) => self.show_error(error),
            }
            return;
        };
        let Ok(position) = PositionCodec::new(&buffer.text).lsp_to_byte(
            location.range.start.line,
            location.range.start.character,
            encoding,
        ) else {
            self.show_error(self.message("lsp_invalid_position", &[]));
            return;
        };
        if let Ok(view) = self.state.show_buffer(buffer_id) {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_precondition(Precondition::ViewDisplays {
                    view,
                    buffer: buffer_id,
                })
                .with_operation(EditorOperation::SetCursor { view, position })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            } else {
                self.ensure_cursor_visible();
            }
        }
    }

    pub(in crate::editor::session) fn current_persisted_location(
        &self,
    ) -> Option<PersistedLocation> {
        let view = self.state.views.get(self.state.active_view?)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        Some(PersistedLocation {
            path: buffer.path.clone()?,
            byte: view.cursor.get(),
            anchor: view
                .selection
                .map_or(view.cursor.get(), |selection| selection.anchor.get()),
            scroll_line: view.scroll_line,
            scroll_column: view.scroll_column,
        })
    }

    pub(in crate::editor::session) fn push_change_location(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            if self.change_list.last().is_none_or(|previous| {
                previous.path != location.path || previous.byte != location.byte
            }) {
                self.change_list.push(location);
                let maximum = self.configuration.settings().persistence.changes;
                if self.change_list.len() > maximum {
                    self.change_list.drain(..self.change_list.len() - maximum);
                }
            }
        }
    }

    pub(in crate::editor::session) fn navigate_matcher_source(&mut self, source: MatcherSource) {
        if !source.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| source.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.documents.read_scoped(source.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::MatcherNavigation { source });
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_persisted_location(
        &mut self,
        location: PersistedLocation,
    ) {
        if !location.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| location.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        if let Some(current) = self.current_persisted_location() {
            self.jump_list.push(current);
            let maximum = self.configuration.settings().persistence.jumps;
            if self.jump_list.len() > maximum {
                self.jump_list.drain(..self.jump_list.len() - maximum);
            }
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.documents.read_scoped(location.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::PersistedNavigation { location });
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_diagnostic(
        &mut self,
        direction: i8,
        filter: DiagnosticFilter,
    ) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(path) = self.state.buffers[buffer_id].path.clone() else {
            return;
        };
        let uri = crate::language::lsp::file_uri(&path);
        self.diagnostics.retire_documents(&self.state);
        let Some(key) = self
            .diagnostics
            .adjacent(&uri, cursor, direction, filter.severities())
        else {
            self.show_error(self.message("diagnostic_none", &[]));
            return;
        };
        let Some(target) = self.diagnostics.resolve(&key, &self.state) else {
            self.sync_ui_diagnostics();
            return;
        };
        let position = target.range.start;
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        let result = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())
            .and_then(|draft| self.commit_editor_draft(draft, None, false));
        if let Err(error) = result {
            self.show_error(error);
        } else {
            self.diagnostics
                .expand(key, filter.severities(), &self.state);
            self.sync_ui_diagnostics();
            self.clear_notice();
            self.ensure_cursor_visible();
        }
    }
}
