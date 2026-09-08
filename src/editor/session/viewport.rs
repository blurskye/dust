use super::*;

impl InteractiveApplication {
    pub(super) fn set_mode(&mut self, mode: EditorMode) {
        if let Some(view) = self.state.active_view {
            self.set_specific_mode(view, mode);
        }
    }

    pub(super) fn set_specific_mode(&mut self, view: ViewId, mode: EditorMode) {
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewExists(view))
            .with_operation(EditorOperation::SetMode { view, mode })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    pub(super) fn ensure_cursor_visible(&mut self) {
        self.reconcile_editor_windows();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let Some(workspace) = self.workspace_bounds() else {
            return;
        };
        let content_width = workspace.width;
        let Some(viewport) = active_pane_viewport(
            &self.state,
            &self.ui,
            self.configuration.settings(),
            content_width,
            self.workspace_render_height(),
        ) else {
            return;
        };
        if viewport.view != view_id {
            return;
        }
        let body_height = usize::from(
            viewport
                .height
                .saturating_sub(u16::from(
                    self.configuration.settings().ui.status_line.enabled,
                ))
                .max(1),
        );
        let vertical_margin = usize::from(
            self.configuration
                .settings()
                .editor
                .viewport
                .vertical_margin,
        )
        .min(body_height.saturating_sub(1) / 2);
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&view.buffer)
                && self.configuration.settings().editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap_enabled,
        );
        let start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let first_anchor = match visual_rows.screen_row(start_anchor, cursor_anchor, body_height) {
            Some(row) if row < vertical_margin => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            Some(row) if row >= body_height.saturating_sub(vertical_margin) => visual_rows
                .backward(
                    cursor_anchor,
                    body_height
                        .saturating_sub(vertical_margin)
                        .saturating_sub(1),
                ),
            Some(_) => start_anchor,
            None if cursor_anchor < start_anchor => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            None => visual_rows.backward(
                cursor_anchor,
                body_height
                    .saturating_sub(vertical_margin)
                    .saturating_sub(1),
            ),
        };
        let first = first_anchor.line;
        let first_wrap_row = if wrap_enabled {
            first_anchor.wrap_row
        } else {
            0
        };
        let first_column = if wrap_enabled || available == 0 {
            0
        } else {
            let line_start = buffer.text.line_to_byte(line).unwrap_or(ByteOffset(0));
            let prefix = buffer
                .text
                .slice(ByteRange::new(line_start, view.cursor).expect("cursor line range"))
                .unwrap_or_default();
            let cursor_column = configured_text_width(
                &prefix,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            );
            let margin = usize::from(
                self.configuration
                    .settings()
                    .editor
                    .viewport
                    .horizontal_margin,
            )
            .min(available.saturating_sub(1));
            let target = if cursor_column < view.scroll_column.saturating_add(margin) {
                cursor_column.saturating_sub(margin)
            } else if cursor_column
                >= view
                    .scroll_column
                    .saturating_add(available.saturating_sub(margin))
            {
                cursor_column.saturating_sub(available.saturating_sub(margin).saturating_sub(1))
            } else {
                view.scroll_column
            };
            let line_text = buffer.text.line(line).unwrap_or_default();
            let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);
            let snapped = snap_display_column(
                line_text,
                target,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            );
            if cursor_column.saturating_sub(snapped) < available {
                snapped
            } else {
                snap_display_column_forward(
                    line_text,
                    cursor_column.saturating_add(1).saturating_sub(available),
                    self.configuration.settings().editor.tab_width,
                    self.configuration.settings().editor.ambiguous_width,
                )
            }
        };
        if first == view.scroll_line
            && first_wrap_row == view.scroll_wrap_row
            && first_column == view.scroll_column
        {
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row,
            })
            .with_operation(EditorOperation::SetScrollColumn {
                view: view_id,
                first_column,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    pub(super) fn transaction_id(&mut self) -> TransactionId {
        let id = TransactionId(self.next_transaction);
        self.next_transaction = self.next_transaction.saturating_add(1);
        id
    }

    pub(super) fn message(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut message = self
            .configuration
            .settings()
            .messages
            .get(key)
            .expect("resolved configuration validates required messages")
            .clone();
        for (name, value) in replacements {
            message = message.replace(&format!("{{{name}}}"), value);
        }
        message
    }

    pub(super) fn show_error(&mut self, error: impl std::fmt::Display) {
        self.notify_message(error.to_string(), true);
    }
}
