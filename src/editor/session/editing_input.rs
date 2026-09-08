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
                    .settings()
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
        let view = self
            .state
            .views
            .get(invocation.target.view.or(self.state.active_view)?)?;
        self.input.normalized_visual_repeat(
            invocation,
            &self.state.buffers.get(view.buffer)?.text,
            view.selection?,
            DisplayWidthPolicy {
                tab_width: self.state.policy.tab_width,
                ambiguous_wide: self.state.policy.ambiguous_width_wide,
            },
        )
    }

    pub(super) fn repeat_last_change(&mut self) {
        let limit = self.configuration.settings().runtime.repeat_step_limit;
        match self.input.repeat_plan(limit) {
            Ok(invocations) => {
                for invocation in invocations {
                    self.invoke(invocation);
                }
            }
            Err(super::super::input::ReplayError::NoChange) => {
                self.show_error(self.message("no_previous_change", &[]));
            }
            Err(_) => {
                self.show_error(self.message("repeat_step_limit", &[("limit", limit.to_string())]))
            }
        }
    }

    pub(super) fn replay_macro(&mut self, requested: char) {
        let Ok(register) = self.input.macro_register(requested) else {
            self.show_error(self.message("no_previous_macro", &[]));
            return;
        };
        let Some(invocations) = self.state.registers.get_macro(register).map(<[_]>::to_vec) else {
            self.show_error(self.message("empty_macro", &[("register", register.to_string())]));
            return;
        };
        if self
            .input
            .begin_macro_replay(
                register,
                self.configuration.settings().runtime.macro_recursion_limit,
            )
            .is_err()
        {
            self.show_error(
                self.message(
                    "macro_recursion_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings()
                            .runtime
                            .macro_recursion_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        for invocation in invocations {
            let Ok(invocation) = self.input.macro_step(
                invocation,
                self.configuration.settings().runtime.macro_step_limit,
            ) else {
                self.show_error(
                    self.message(
                        "macro_step_limit",
                        &[(
                            "limit",
                            self.configuration
                                .settings()
                                .runtime
                                .macro_step_limit
                                .to_string(),
                        )],
                    ),
                );
                break;
            };
            self.invoke(invocation);
        }
        self.input.finish_macro_replay();
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
        self.input.remember_search(forward);
        self.perform_search(forward);
    }

    pub(super) fn apply_authoritative_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
    ) -> Result<(), String> {
        self.commit_editor_transaction(transaction, None)
    }
    pub(super) fn apply_window_plan(
        &mut self,
        plan: super::super::windows::WindowPlan,
    ) -> Result<(), String> {
        self.window_session.check_plan(
            &plan,
            &self.state.window_version(),
            &self.plugin_contributions.version(),
        )?;
        if plan.operations().is_empty() {
            self.window_session.accept_native_plan(
                plan,
                &self.state.window_version(),
                &self.plugin_contributions.version(),
            )?;
            if let Some(id) = self
                .window_session
                .focused_terminal()
                .map(PtyId)
                .filter(|id| self.terminal_session.contains(*id))
            {
                self.terminal_session.select(id);
            }
            self.project_window_activation();
            self.reconcile_editor_windows();
            return Ok(());
        }
        let transaction = plan
            .operations()
            .iter()
            .cloned()
            .fold(
                EditorTransaction::new(self.transaction_id()),
                |transaction, operation| transaction.with_operation(operation),
            )
            .with_undo_policy(UndoPolicy::Ignore);
        self.commit_editor_transaction(&transaction, Some(plan))
    }
    fn commit_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
        plan: Option<super::super::windows::WindowPlan>,
    ) -> Result<(), String> {
        if let Some(plan) = &plan {
            self.window_session.check_plan(
                plan,
                &self.state.window_version(),
                &self.plugin_contributions.version(),
            )?;
        }
        self.window_session
            .check_editor_admission(&(&self.state).into())?;
        let candidate = self
            .state
            .prepare_transaction(transaction)
            .map_err(|error| error.to_string())?;
        self.commit_editor_draft(candidate, plan, false)
    }
    pub(super) fn commit_editor_draft(
        &mut self,
        candidate: crate::editor::EditorDraft,
        plan: Option<super::super::windows::WindowPlan>,
        activate_document: bool,
    ) -> Result<(), String> {
        self.window_session
            .check_editor_admission(&(&self.state).into())?;
        let current = self.state.version();
        candidate.check_current(&current)?;
        if !activate_document
            && plan.is_none()
            && candidate.window_version() == self.state.window_version()
        {
            self.state = candidate.accept(&current)?;
            return Ok(());
        }

        let terminals = self.terminal_session.descriptions();
        let mut signature = self.native_surface_signature(self.window_session.editor_ids());
        signature.dashboard_visible =
            self.window_session.dashboard_visible() || candidate.topology().active_tab().is_none();
        let title = self.message("explorer_title", &[]);
        let descriptions = NativeDescriptions {
            signature,
            explorer_title: &title,
            dashboard_title: &self.configuration.dashboard().identity,
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title),
            terminals: &terminals,
        };
        let catalog = self.plugin_contributions.version();
        let transition = if let Some(plan) = plan {
            self.window_session.prepare_editor_plan(
                plan,
                (&current, &catalog),
                candidate,
                &self.configuration.settings().ui.buffer_line.scratch_label,
                descriptions,
            )?
        } else if activate_document {
            self.window_session.prepare_document_transition(
                candidate,
                &current,
                catalog.clone(),
                &self.configuration.settings().ui.buffer_line.scratch_label,
                descriptions,
            )?
        } else {
            self.window_session.prepare_editor_transition(
                candidate,
                &current,
                catalog.clone(),
                &self.configuration.settings().ui.buffer_line.scratch_label,
                Some(descriptions),
            )?
        };
        self.state = self
            .window_session
            .accept_editor_transition(transition, &current, &catalog)?;
        self.reconcile_editor_windows();
        Ok(())
    }

    pub(super) fn focus_surface(&mut self, direction: FocusDirection) {
        self.reconcile_editor_windows();
        let direction = match direction {
            FocusDirection::Left => SpatialDirection::Left,
            FocusDirection::Down => SpatialDirection::Down,
            FocusDirection::Up => SpatialDirection::Up,
            FocusDirection::Right => SpatialDirection::Right,
        };
        let Some(target) = self.window_session.spatial_target(direction) else {
            return;
        };
        let result = self
            .window_session
            .plan_focus(
                target,
                &self.state.window_version(),
                self.plugin_contributions.version(),
            )
            .and_then(|plan| self.apply_window_plan(plan));
        if let Err(error) = result {
            self.show_error(error);
            return;
        }
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.ensure_cursor_visible();
        self.sync_terminal_panel();
    }

    pub(super) fn apply_layout_action(&mut self, action: LayoutAction) {
        self.reconcile_editor_windows();
        let plan = match self.window_session.plan_layout(
            action,
            &(&self.state).into(),
            self.configuration.settings(),
            self.plugin_contributions.version(),
        ) {
            Ok(plan) => plan,
            Err(super::super::windows::LayoutPlanError::Dirty) => {
                self.show_error(self.message("dirty_close", &[]));
                return;
            }
            Err(super::super::windows::LayoutPlanError::Rejected(error)) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_window_plan(plan) {
            self.show_error(error);
        } else {
            self.cancel_pending_mapping();
            self.input.cancel_pending();
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn apply_retained_ui_interaction(&mut self, interaction: PluginUiInteraction) {
        match self.plugin_windows.interact_focused(
            self.window_session.graph(),
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
        let Some((column, row)) = self.input.mouse_position() else {
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
            self.input.mouse_position().and_then(|(x, y)| {
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
            self.window_session.graph().focused().and_then(|window| {
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
                if let Err(error) = self
                    .extension
                    .push_action(plugin, PluginLoadId(load), event)
                {
                    self.show_error(error);
                    return;
                }
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
        let (global_column, global_row) = self.input.mouse_position()?;
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
            self.configuration.settings(),
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
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let body_height = pane_height.saturating_sub(u16::from(
            self.configuration.settings().ui.status_line.enabled,
        ));
        if row >= body_height {
            return;
        }
        let gutter = editor_state_gutter_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        if available == 0 {
            return;
        }
        let wrap_enabled = self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&buffer_id)
                && self.configuration.settings().editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            pane_width,
            wrap_enabled,
        );
        let source = self.state.version();
        let transaction_id = TransactionId(self.next_transaction);
        let plan = self.input.mouse_plan(
            crate::editor::session::input::MouseDocument {
                editor: &source,
                view_id,
                pane_id,
                view,
                text: &buffer.text,
            },
            crate::editor::session::input::MouseGeometry {
                column,
                row,
                body_height,
                gutter,
                wrap: wrap_enabled,
                rows: &visual_rows,
                inlays: &self.ui.lsp_inlay_hints,
            },
            &self.configuration.settings().editor,
            transaction_id,
            extend_selection,
        );
        let Some(plan) = plan else {
            return;
        };
        self.transaction_id();
        let transaction = match self.input.mouse_transaction(&plan, &self.state.version()) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_authoritative_editor_transaction(transaction) {
            self.show_error(error);
        } else {
            let source = self.state.window_version();
            let current = self.state.active_view.and_then(|id| {
                self.state.views.get(id).and_then(|view| {
                    self.state
                        .buffers
                        .get(view.buffer)
                        .map(|buffer| (id, view.buffer, buffer.text.revision()))
                })
            });
            self.input.accept_mouse_plan(plan, &source, current);
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
            self.configuration.settings(),
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
                    self.configuration.settings().ui.status_line.enabled,
                ))
                .max(1),
        );
        let percent = if kind.starts_with("half_") {
            self.configuration.settings().editor.half_page_percent
        } else {
            self.configuration.settings().editor.full_page_percent
        };
        let amount = body.saturating_mul(usize::from(percent)) / 100;
        let amount = amount
            .max(1)
            .saturating_mul(usize::try_from(count).unwrap_or(usize::MAX));
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
            self.configuration.settings().editor.tab_width,
            self.configuration.settings().editor.ambiguous_width,
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
                    self.configuration.settings().editor.tab_width
                        - (display_column % self.configuration.settings().editor.tab_width)
                } else {
                    configured_grapheme_width(
                        grapheme,
                        self.configuration.settings().editor.ambiguous_width,
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
