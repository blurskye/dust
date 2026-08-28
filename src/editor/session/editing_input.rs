use super::*;

impl InteractiveApplication {
    pub(super) fn process_grammar_input(&mut self, input: crate::editor::input::GrammarInput) {
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        match self.input.feed(mode, input) {
            Ok(InputAction::Invoke(invocation)) => self.invoke_mapped(invocation),
            Ok(InputAction::Pending) => {
                if self.input.effective_mode(mode) == EditorMode::OperatorPending
                    && mode != EditorMode::OperatorPending
                {
                    self.set_mode(EditorMode::OperatorPending);
                }
            }
            Ok(InputAction::Cancelled) => {
                if mode == EditorMode::OperatorPending {
                    self.set_mode(EditorMode::Normal);
                }
            }
            Ok(InputAction::Text(text)) => {
                let context = self.active_input_context();
                if let Some(command_id) = self
                    .configuration
                    .settings
                    .input
                    .text_commands
                    .get(&context)
                    .cloned()
                {
                    self.invoke_text_command(&command_id, text);
                }
            }
            Ok(InputAction::Unmapped) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<CommandInvocation> {
        let view_id = invocation.target.view.or(self.state.active_view)?;
        let view = self.state.views.get(view_id)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        let selection = view.selection?;
        let (shape, width, height) = match selection.kind {
            crate::editor::SelectionKind::Character => {
                let low = selection.anchor.min(selection.head);
                let high = selection.anchor.max(selection.head);
                let end = next_grapheme_boundary(&buffer.text, high);
                let selected = buffer.text.slice(ByteRange::new(low, end).ok()?).ok()?;
                ("character", selected.graphemes(true).count(), 1_usize)
            }
            crate::editor::SelectionKind::Line => {
                let anchor = buffer.text.byte_to_line(selection.anchor).ok()?;
                let head = buffer.text.byte_to_line(selection.head).ok()?;
                ("line", 1, anchor.abs_diff(head).saturating_add(1))
            }
            crate::editor::SelectionKind::Block => {
                let anchor_line = buffer.text.byte_to_line(selection.anchor).ok()?;
                let head_line = buffer.text.byte_to_line(selection.head).ok()?;
                let anchor_start = buffer.text.line_to_byte(anchor_line).ok()?;
                let head_start = buffer.text.line_to_byte(head_line).ok()?;
                let anchor_text = buffer.text.line(anchor_line).ok()?;
                let head_text = buffer.text.line(head_line).ok()?;
                let width_policy = DisplayWidthPolicy {
                    tab_width: self.state.policy.tab_width,
                    ambiguous_wide: self.state.policy.ambiguous_width_wide,
                };
                let anchor_column = display_column_at(
                    anchor_text.strip_suffix('\n').unwrap_or(&anchor_text),
                    selection.anchor.get().saturating_sub(anchor_start.get()),
                    width_policy,
                );
                let head_column = display_column_at(
                    head_text.strip_suffix('\n').unwrap_or(&head_text),
                    selection.head.get().saturating_sub(head_start.get()),
                    width_policy,
                );
                (
                    "block",
                    anchor_column.abs_diff(head_column).saturating_add(1),
                    anchor_line.abs_diff(head_line).saturating_add(1),
                )
            }
        };
        let operator = invocation.arguments.get("operator")?.clone();
        let mut repeat = CommandInvocation::new("edit.repeat_visual", invocation.source).ok()?;
        repeat.arguments.insert("operator".into(), operator);
        repeat
            .arguments
            .insert("shape".into(), CommandValue::String(shape.into()));
        repeat.arguments.insert(
            "width".into(),
            CommandValue::Integer(i64::try_from(width).ok()?),
        );
        repeat.arguments.insert(
            "height".into(),
            CommandValue::Integer(i64::try_from(height).ok()?),
        );
        repeat.register = invocation.register;
        repeat.count = invocation.count;
        Some(repeat)
    }

    pub(super) fn record_successful_invocation(
        &mut self,
        invocation: &CommandInvocation,
        repeat_override: Option<&CommandInvocation>,
        suppress_repeat: bool,
    ) {
        if !invocation.id.as_str().starts_with("macro.")
            && !invocation.id.as_str().starts_with("input.")
        {
            if let Some((_, recorded)) = &mut self.recording_macro {
                let mut invocation = invocation.clone();
                invocation.target = Default::default();
                recorded.push(invocation);
            }
        }
        if invocation.source == InvocationSource::Macro {
            return;
        }
        if suppress_repeat {
            return;
        }
        let mut semantic = repeat_override.unwrap_or(invocation).clone();
        semantic.target = Default::default();
        match invocation.id.as_str() {
            "mode.insert" | "mode.replace" | "edit.substitute" => {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.operator" | "edit.repeat_visual"
                if semantic
                    .arguments
                    .get("operator")
                    .is_some_and(|value| value == &CommandValue::String("change".into())) =>
            {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.insert_text"
            | "edit.backspace"
            | "edit.replace_text"
            | "edit.replace_backspace" => {
                if let Some(pending) = &mut self.pending_change {
                    pending.push(semantic);
                }
            }
            "mode.normal" => {
                if let Some(mut pending) = self.pending_change.take() {
                    pending.push(semantic);
                    self.last_change = pending;
                }
            }
            "edit.delete_char"
            | "edit.delete_backward"
            | "edit.replace"
            | "edit.join_lines"
            | "edit.toggle_case"
            | "edit.put" => self.last_change = vec![semantic],
            "edit.operator" | "edit.repeat_visual" => self.last_change = vec![semantic],
            _ => {}
        }
    }

    pub(super) fn repeat_last_change(&mut self) {
        if self.last_change.is_empty() {
            self.show_error(self.message("no_previous_change", &[]));
            return;
        }
        if self.last_change.len() > self.configuration.settings.runtime.repeat_step_limit {
            self.show_error(
                self.message(
                    "repeat_step_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings
                            .runtime
                            .repeat_step_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        let invocations = self.last_change.clone();
        for mut invocation in invocations {
            invocation.source = InvocationSource::Macro;
            invocation.target = Default::default();
            self.invoke(invocation);
        }
    }

    pub(super) fn replay_macro(&mut self, requested: char) {
        let register = if requested == '@' {
            let Some(register) = self.last_macro else {
                self.show_error(self.message("no_previous_macro", &[]));
                return;
            };
            register
        } else {
            requested
        };
        let Some(invocations) = self.state.registers.get_macro(register).map(<[_]>::to_vec) else {
            self.show_error(self.message("empty_macro", &[("register", register.to_string())]));
            return;
        };
        if self.replay_depth >= self.configuration.settings.runtime.macro_recursion_limit {
            self.show_error(
                self.message(
                    "macro_recursion_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings
                            .runtime
                            .macro_recursion_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        self.last_macro = Some(register);
        self.replay_depth += 1;
        for mut invocation in invocations {
            if self.replay_steps >= self.configuration.settings.runtime.macro_step_limit {
                self.show_error(
                    self.message(
                        "macro_step_limit",
                        &[(
                            "limit",
                            self.configuration
                                .settings
                                .runtime
                                .macro_step_limit
                                .to_string(),
                        )],
                    ),
                );
                break;
            }
            self.replay_steps += 1;
            invocation.source = InvocationSource::Macro;
            invocation.target = Default::default();
            self.invoke(invocation);
        }
        self.replay_depth -= 1;
    }

    pub(super) fn search_word_under_cursor(&mut self, forward: bool) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let source = self.state.buffers[view.buffer].text.to_string();
        let cursor = view.cursor.get();
        let Some(character) = source[cursor..].chars().next() else {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        };
        if !(character.is_alphanumeric() || character == '_') {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        }
        let start = source[..cursor]
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
            .last()
            .map_or(cursor, |(index, _)| index);
        let end = source[cursor..]
            .char_indices()
            .find(|(_, character)| !(character.is_alphanumeric() || *character == '_'))
            .map_or(source.len(), |(relative, _)| cursor + relative);
        self.ui.search_query = Some(source[start..end].to_owned());
        self.last_search_forward = forward;
        self.perform_search(forward);
    }

    pub(super) fn apply_authoritative_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
    ) -> Result<(), String> {
        let mut candidate = self.state.clone();
        candidate
            .apply_transaction(transaction)
            .map_err(|error| error.to_string())?;
        if candidate.window_revision() == self.state.window_revision() {
            self.state = candidate;
            return Ok(());
        }

        let previous_state = std::mem::replace(&mut self.state, candidate);
        let previous_graph = self.windows.clone();
        let previous_editor = self.editor_windows.clone();
        let previous_surfaces = self.surface_windows.clone();
        let previous_ui = self.ui.clone();
        self.reconcile_editor_windows();
        let accepted = self.editor_windows.is_current(
            &self.state,
            &self.configuration.settings.ui.buffer_line.scratch_label,
        ) && self.windows.validate().is_ok()
            && self.windows.revision().is_some()
            && self.compositor_projection().is_some();
        if accepted {
            Ok(())
        } else {
            self.state = previous_state;
            self.windows = previous_graph;
            self.editor_windows = previous_editor;
            self.surface_windows = previous_surfaces;
            self.ui = previous_ui;
            Err("authoritative editor/window transaction was rejected".into())
        }
    }

    pub(super) fn focus_surface(&mut self, direction: FocusDirection) {
        self.reconcile_editor_windows();
        let Some(layout) = self.ui.compositor_layout.as_ref() else {
            return;
        };
        // Dock focus is host-owned and intentionally distinct from the graph's
        // tiled-editor focus. Spatial movement must therefore start from the
        // visibly focused dock, not from the last editor window.
        let current = if self.terminal_focused {
            layout.surfaces.terminal.map(|(_, window)| window)
        } else if self.ui.explorer_focused {
            layout.surfaces.explorer
        } else {
            self.windows.focused()
        };
        let Some(current) = current else {
            return;
        };
        let direction = match direction {
            FocusDirection::Left => SpatialDirection::Left,
            FocusDirection::Down => SpatialDirection::Down,
            FocusDirection::Up => SpatialDirection::Up,
            FocusDirection::Right => SpatialDirection::Right,
        };
        let Some(target) = layout.projection.spatial_neighbor(current, direction) else {
            return;
        };
        if layout.surfaces.explorer == Some(target) {
            self.ui.explorer_focused = true;
            self.terminal_focused = false;
        } else if layout
            .surfaces
            .terminal
            .is_some_and(|(_, window)| window == target)
        {
            self.ui.explorer_focused = false;
            self.terminal_focused = true;
        } else {
            let Some(pane) = self.ui.editor_window_layout.as_ref().and_then(|editor| {
                editor
                    .bindings
                    .iter()
                    .find(|binding| binding.window == target)
                    .map(|binding| binding.pane)
            }) else {
                return;
            };
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_operation(EditorOperation::FocusPaneId { pane })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.apply_authoritative_editor_transaction(&transaction) {
                self.show_error(error);
                return;
            }
            self.ui.explorer_focused = false;
            self.terminal_focused = false;
        }
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.ensure_cursor_visible();
        self.sync_terminal_panel();
    }

    pub(super) fn apply_layout_action(&mut self, action: LayoutAction) {
        let closes_dirty = match action {
            LayoutAction::ClosePane => self
                .state
                .active_buffer_id()
                .and_then(|buffer| self.state.buffers.get(buffer))
                .is_some_and(|buffer| buffer.dirty),
            LayoutAction::CloseTab => self.state.active_tab_views().into_iter().any(|view| {
                self.state
                    .views
                    .get(view)
                    .and_then(|view| self.state.buffers.get(view.buffer))
                    .is_some_and(|buffer| buffer.dirty)
            }),
            _ => false,
        };
        if closes_dirty {
            self.show_error(self.message("dirty_close", &[]));
            return;
        }
        let operation = match action {
            LayoutAction::Split(axis) => EditorOperation::SplitPane {
                axis,
                ratio: self.configuration.settings.ui.panes.default_ratio,
            },
            LayoutAction::ClosePane => EditorOperation::ClosePane,
            LayoutAction::ResizePane(direction) => EditorOperation::ResizePane {
                delta: i16::from(direction)
                    * i16::try_from(self.configuration.settings.ui.panes.resize_step)
                        .unwrap_or(i16::MAX),
            },
            LayoutAction::FocusPane(offset) => EditorOperation::FocusPane {
                offset,
                wrap: self.configuration.settings.ui.panes.focus_wrap,
            },
            LayoutAction::NewTab => EditorOperation::NewTab,
            LayoutAction::CloseTab => EditorOperation::CloseTab,
            LayoutAction::FocusTab(offset) => EditorOperation::FocusTab {
                offset,
                wrap: self.configuration.settings.ui.tab_line.focus_wrap,
            },
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(operation)
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.apply_authoritative_editor_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ui.dashboard = false;
            self.cancel_pending_mapping();
            self.input.cancel_pending();
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn apply_retained_ui_interaction(&mut self, interaction: PluginUiInteraction) {
        match self.plugin_windows.interact_focused(
            &self.windows,
            &self.plugin_contributions,
            interaction,
        ) {
            Ok(action) => {
                self.ui.plugin_surfaces = self.plugin_windows.snapshots();
                if let Some(action) = action {
                    self.invoke_mapped_command(&action);
                }
            }
            Err(error) => self.show_error(error.detail),
        }
    }

    pub(super) fn select_retained_ui_at_mouse(&mut self) {
        let Some((column, row)) = self.mouse_position else {
            return;
        };
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let Some(window) = projection.hit_test(column, row) else {
            return;
        };
        let Some(rect) = projection.window(window).map(|projected| projected.rect) else {
            return;
        };
        if let Err(error) = self
            .plugin_windows
            .focus_node_at_row(window, usize::from(row.saturating_sub(rect.y)))
        {
            self.show_error(error.detail);
            return;
        }
        self.ui.plugin_surfaces = self.plugin_windows.snapshots();
    }

    pub(super) fn activate_private_surface_action(&mut self, at_mouse: bool) {
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let target = if at_mouse {
            self.mouse_position.and_then(|(x, y)| {
                projection.hit_test(x, y).and_then(|window| {
                    projection.window(window).map(|projected| {
                        (
                            window,
                            x.saturating_sub(projected.rect.x),
                            y.saturating_sub(projected.rect.y),
                        )
                    })
                })
            })
        } else {
            self.windows.focused().and_then(|window| {
                self.ui
                    .private_surfaces
                    .iter()
                    .find(|surface| surface.window == window)
                    .and_then(|surface| surface.cursor.map(|(x, y)| (window, x, y)))
            })
        };
        let Some((window, x, y)) = target else { return };
        match self
            .plugin_windows
            .private_action_at(window, x, y, &self.plugin_contributions)
        {
            Ok((plugin, load, event)) => {
                let action = event.action.clone();
                let pending = self
                    .pending_plugin_ui_events
                    .entry((plugin, PluginLoadId(load)))
                    .or_default();
                if pending.len() >= 64 {
                    self.show_error("private surface action queue is full");
                    return;
                }
                pending.push(event);
                if let Some(action) = action {
                    self.invoke_mapped_command(&action);
                }
            }
            Err(error) => self.show_error(error.detail),
        }
    }

    pub(super) fn mouse_pane_geometry(
        &self,
    ) -> Option<(crate::editor::PaneId, u16, u16, u16, u16)> {
        let (global_column, global_row) = self.mouse_position?;
        let workspace = self.workspace_bounds()?;
        if !rect_contains_point(workspace, global_column, global_row) {
            return None;
        }
        let content_width = workspace.width;
        let local_column = global_column.saturating_sub(workspace.x);
        let local_row = global_row.saturating_sub(workspace.y);
        pane_viewports(
            &self.state,
            &self.ui,
            &self.configuration.settings,
            content_width,
            self.workspace_render_height(),
        )
        .into_iter()
        .find(|viewport| {
            local_column >= viewport.x
                && local_column < viewport.x.saturating_add(viewport.width)
                && local_row >= viewport.y
                && local_row < viewport.y.saturating_add(viewport.height)
        })
        .map(|viewport| {
            (
                viewport.pane,
                local_column.saturating_sub(viewport.x),
                local_row.saturating_sub(viewport.y),
                viewport.width,
                viewport.height,
            )
        })
    }

    pub(super) fn focus_mouse_pane(&mut self) {
        self.reconcile_editor_windows();
        let Some((pane, ..)) = self.mouse_pane_geometry() else {
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::FocusPaneId { pane })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.apply_authoritative_editor_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn apply_mouse_cursor(&mut self, extend_selection: bool) {
        self.reconcile_editor_windows();
        let Some((pane_id, column, row, pane_width, pane_height)) = self.mouse_pane_geometry()
        else {
            return;
        };
        let Some(view_id) = self.state.panes.get(pane_id).map(|pane| pane.view) else {
            return;
        };
        let view = &self.state.views[view_id];
        let original_cursor = view.cursor;
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let body_height = pane_height.saturating_sub(u16::from(
            self.configuration.settings.ui.status_line.enabled,
        ));
        if row >= body_height {
            return;
        }
        let gutter = editor_state_gutter_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings.editor,
            pane_width,
        );
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings.editor,
            pane_width,
        );
        if available == 0 {
            return;
        }
        let wrap_enabled = self.configuration.settings.editor.wrap
            && !(self.ui.large_file_buffers.contains(&buffer_id)
                && self.configuration.settings.editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            &self.configuration.settings,
            pane_width,
            wrap_enabled,
        );
        let start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let target_anchor = visual_rows.forward(start_anchor, usize::from(row));
        if visual_rows.screen_row(start_anchor, target_anchor, usize::from(body_height))
            != Some(usize::from(row))
        {
            return;
        }
        let line_index = target_anchor.line;
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let byte = if wrap_enabled {
            visual_rows.byte_at_cell(target_anchor, usize::from(column.saturating_sub(gutter)))
        } else {
            let line = buffer.text.line(line_index).unwrap_or_default();
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let snapped_scroll = snap_display_column(
                content,
                view.scroll_column,
                self.configuration.settings.editor.tab_width,
                self.configuration.settings.editor.ambiguous_width,
            );
            let target_column =
                usize::from(column.saturating_sub(gutter)).saturating_add(snapped_scroll);
            let skipped_bytes = configured_byte_offset_at_display_column(
                content,
                view.scroll_column,
                self.configuration.settings.editor.tab_width,
                self.configuration.settings.editor.ambiguous_width,
            );
            let visible_start = ByteOffset(line_start.get().saturating_add(skipped_bytes));
            let line_end = ByteOffset(line_start.get().saturating_add(content.len()));
            let mut hints = self
                .ui
                .lsp_inlay_hints
                .iter()
                .filter(|hint| {
                    hint.buffer == buffer_id
                        && hint.revision == revision
                        && hint.position >= visible_start
                        && hint.position <= line_end
                })
                .collect::<Vec<_>>();
            hints.sort_unstable_by_key(|hint| hint.position);
            hints.truncate(32);
            let mut hint_cursor = 0usize;
            let mut display_column = 0_usize;
            let mut virtual_cells = 0usize;
            'target: {
                for (relative, grapheme) in content.grapheme_indices(true) {
                    let global = ByteOffset(line_start.get().saturating_add(relative));
                    while let Some(hint) = hints
                        .get(hint_cursor)
                        .filter(|hint| hint.position == global)
                    {
                        let width = configured_text_width(
                            &hint.label,
                            self.configuration.settings.editor.tab_width,
                            self.configuration.settings.editor.ambiguous_width,
                        );
                        let start = display_column.saturating_add(virtual_cells);
                        if start.saturating_add(width) > target_column {
                            break 'target relative;
                        }
                        virtual_cells = virtual_cells.saturating_add(width);
                        hint_cursor = hint_cursor.saturating_add(1);
                    }
                    let width = if grapheme == "\t" {
                        self.configuration.settings.editor.tab_width
                            - (display_column % self.configuration.settings.editor.tab_width)
                    } else {
                        configured_grapheme_width(
                            grapheme,
                            self.configuration.settings.editor.ambiguous_width,
                        )
                    };
                    if display_column
                        .saturating_add(virtual_cells)
                        .saturating_add(width)
                        > target_column
                    {
                        break 'target relative;
                    }
                    display_column = display_column.saturating_add(width);
                }
                content.len()
            }
        };
        let position = ByteOffset(line_start.get().saturating_add(byte));
        let mut transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::FocusPaneId { pane: pane_id })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if extend_selection {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: Some(crate::editor::Selection {
                    anchor: self
                        .mouse_selection_anchor
                        .filter(|(anchor_view, _)| *anchor_view == view_id)
                        .map_or(original_cursor, |(_, anchor)| anchor),
                    head: position,
                    kind: crate::editor::SelectionKind::Character,
                }),
            });
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::VisualChar,
            });
        } else {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: None,
            });
        }
        if let Err(error) = self.apply_authoritative_editor_transaction(&transaction) {
            self.show_error(error);
        } else {
            if !extend_selection {
                self.mouse_selection_anchor = Some((view_id, position));
            }
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn scroll_view(&mut self, kind: &str, count: u64) {
        self.reconcile_editor_windows();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let Some(workspace) = self.workspace_bounds() else {
            return;
        };
        let content_width = workspace.width;
        let Some(viewport) = active_pane_viewport(
            &self.state,
            &self.ui,
            &self.configuration.settings,
            content_width,
            self.workspace_render_height(),
        )
        .filter(|viewport| viewport.view == view_id) else {
            return;
        };
        let body = usize::from(
            viewport
                .height
                .saturating_sub(u16::from(
                    self.configuration.settings.ui.status_line.enabled,
                ))
                .max(1),
        );
        let percent = if kind.starts_with("half_") {
            self.configuration.settings.editor.half_page_percent
        } else {
            self.configuration.settings.editor.full_page_percent
        };
        let amount = body.saturating_mul(usize::from(percent)) / 100;
        let amount = amount
            .max(1)
            .saturating_mul(usize::try_from(count).unwrap_or(usize::MAX));
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings.editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings.editor.wrap
            && !(self.ui.large_file_buffers.contains(&view.buffer)
                && self.configuration.settings.editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            &self.configuration.settings,
            viewport.width,
            wrap_enabled,
        );
        let old_start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let first_anchor = if kind.ends_with("up") {
            visual_rows.backward(old_start_anchor, amount)
        } else {
            visual_rows.forward(old_start_anchor, amount)
        };
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let last_anchor = visual_rows.forward(first_anchor, body.saturating_sub(1));
        let target_anchor = cursor_anchor.clamp(first_anchor, last_anchor);
        let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let old_line_start = buffer
            .text
            .line_to_byte(current_line)
            .unwrap_or(ByteOffset(0));
        let old_prefix = buffer
            .text
            .slice(ByteRange::new(old_line_start, view.cursor).expect("cursor line range"))
            .unwrap_or_default();
        let current_column = configured_text_width(
            &old_prefix,
            self.configuration.settings.editor.tab_width,
            self.configuration.settings.editor.ambiguous_width,
        );
        let target_line = target_anchor.line;
        let target_start = buffer
            .text
            .line_to_byte(target_line)
            .unwrap_or(ByteOffset(0));
        let target_byte = if wrap_enabled {
            let current_cell = visual_rows.cursor_projection(view.cursor).1;
            visual_rows.byte_at_cell(target_anchor, current_cell)
        } else {
            let target_text = buffer.text.line(target_line).unwrap_or_default();
            let target_content = target_text.strip_suffix('\n').unwrap_or(&target_text);
            let mut display_column = 0_usize;
            let mut target_byte = 0_usize;
            for (relative, grapheme) in target_content.grapheme_indices(true) {
                let width = if grapheme == "\t" {
                    self.configuration.settings.editor.tab_width
                        - (display_column % self.configuration.settings.editor.tab_width)
                } else {
                    configured_grapheme_width(
                        grapheme,
                        self.configuration.settings.editor.ambiguous_width,
                    )
                };
                if display_column.saturating_add(width) > current_column {
                    target_byte = relative;
                    break;
                }
                display_column = display_column.saturating_add(width);
                target_byte = relative.saturating_add(grapheme.len());
            }
            target_byte
        };
        let target = ByteOffset(target_start.get().saturating_add(target_byte));
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first_anchor.line,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row: if wrap_enabled {
                    first_anchor.wrap_row
                } else {
                    0
                },
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: target,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}
