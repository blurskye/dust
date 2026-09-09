use super::*;

impl InteractiveApplication {
    pub(super) fn sync_status_summary_ui(&mut self) {
        self.ui.git_changed_files = self
            .git
            .status()
            .map_or(0, |snapshot| snapshot.entries.len());
        self.ui.running_tasks = self.tasks.running_count();
    }

    pub(super) fn toggle_buffer_pin(&mut self, buffer: crate::editor::BufferId) {
        if !self.state.buffers.contains_key(buffer) {
            return;
        }
        self.window_session.toggle_pin(buffer);
        self.reconcile_editor_windows();
    }

    pub(super) fn toggle_active_buffer_pin(&mut self) {
        if let Some(buffer) = self.state.active_buffer_id() {
            self.toggle_buffer_pin(buffer)
        }
    }

    pub(super) fn move_active_buffer_line(&mut self, direction: isize) {
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        if let Err(error) = self.window_session.move_content(buffer, direction) {
            self.show_error(error);
            return;
        }
        self.reconcile_editor_windows();
    }

    pub(super) fn handle_content_tab_mouse(
        &mut self,
        key: &KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        if self.input.prompt().is_some() || self.ui.picker.is_some() || self.ui.tool_menu.is_some()
        {
            return false;
        }
        let Some(kind) = key.terminal_mouse() else {
            return false;
        };
        if !matches!(
            kind,
            crate::editor::input::TerminalMouseInput::LeftDown
                | crate::editor::input::TerminalMouseInput::RightDown
                | crate::editor::input::TerminalMouseInput::MiddleDown
        ) {
            return false;
        }
        let Some(bounds) = self.workspace_bounds() else {
            return false;
        };
        let tab_line = self.configuration.settings().ui.tab_line.enabled
            && (!self.configuration.settings().ui.tab_line.hide_when_single
                || self.state.tabs.len() > 1);
        let buffer_line = self.configuration.settings().ui.buffer_line.enabled
            && (!self
                .configuration
                .settings()
                .ui
                .buffer_line
                .hide_when_single
                || self.ui.content_tabs.len() > 1);
        if !buffer_line
            || row != bounds.y.saturating_add(u16::from(tab_line))
            || column < bounds.x
            || column >= bounds.x.saturating_add(bounds.width)
        {
            return false;
        }
        let Some(identity) = crate::ui::content_tab_at_column(
            &self.ui.content_tabs,
            self.configuration.settings(),
            bounds.width,
            column - bounds.x,
        ) else {
            return false;
        };
        let entry = self
            .ui
            .content_tabs
            .iter()
            .find(|entry| entry.identity == identity)
            .cloned();
        let Some(buffer) = self
            .window_session
            .editor_ids()
            .buffer_for_content(identity.0)
        else {
            if kind == crate::editor::input::TerminalMouseInput::RightDown
                && entry.is_some_and(|entry| !entry.closeable)
            {
                self.show_error("content-line entry is not closeable");
                return true;
            }
            return false;
        };
        if kind == crate::editor::input::TerminalMouseInput::MiddleDown {
            self.toggle_buffer_pin(buffer);
            return true;
        }
        if self.state.show_buffer(buffer).is_err() {
            return false;
        }
        self.reconcile_editor_windows();
        if kind == crate::editor::input::TerminalMouseInput::RightDown {
            if entry.is_some_and(|entry| entry.closeable) {
                self.close_active_buffer(false)
            } else {
                self.show_error("content-line entry is not closeable")
            }
        }
        true
    }

    pub(super) fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Resize(width, height) => {
                self.window_session.observe_size((width, height));
                self.window_session.invalidate();
                self.project_window_geometry();
                self.window_session.invalidate_native();
                self.resize_active_terminal_to_panel();
                self.sync_terminal_panel();
                self.sync_explorer_ui();
                if self
                    .configuration
                    .settings()
                    .ui
                    .keep_cursor_visible_on_resize
                {
                    self.ensure_cursor_visible();
                }
            }
            UiEvent::Key { key, kind } => match kind {
                KeyInputKind::Press | KeyInputKind::Repeat => self.handle_configured_key(key),
                KeyInputKind::Release => self.handle_key_in_context("key_release", key),
            },
            UiEvent::Paste(text) => self.handle_configured_paste(text),
            UiEvent::FocusGained => {
                self.send_terminal_focus_report(true);
                self.handle_key_in_context("focus", KeyEvent::focus_gained())
            }
            UiEvent::FocusLost => {
                self.send_terminal_focus_report(false);
                if self.input.prompt().is_some_and(|prompt| {
                    matches!(
                        prompt.kind,
                        PromptKind::WorkspaceTrust
                            | PromptKind::GitReset
                            | PromptKind::ExplorerDelete
                            | PromptKind::ExplorerRename
                    )
                }) {
                    self.close_prompt();
                }
                self.handle_key_in_context("focus", KeyEvent::focus_lost());
            }
            UiEvent::Mouse { key, column, row } => {
                self.input.observe_mouse(column, row);
                if self.input.documentation().is_some() {
                    match key.terminal_mouse() {
                        Some(crate::editor::input::TerminalMouseInput::ScrollUp) => {
                            self.process_effect(AppEffect::LspDocumentationScroll(-1));
                            return;
                        }
                        Some(crate::editor::input::TerminalMouseInput::ScrollDown) => {
                            self.process_effect(AppEffect::LspDocumentationScroll(1));
                            return;
                        }
                        _ => {}
                    }
                }
                if self.handle_content_tab_mouse(&key, column, row) {
                    return;
                }
                let over_terminal = self
                    .terminal_window_rect()
                    .is_some_and(|rect| rect_contains_point(rect, column, row));
                let over_explorer = !over_terminal
                    && self
                        .explorer_window_rect()
                        .is_some_and(|rect| rect_contains_point(rect, column, row));
                if over_terminal
                    && key.terminal_mouse()
                        == Some(crate::editor::input::TerminalMouseInput::LeftDown)
                    && let Some(rect) = self.terminal_window_rect()
                    && row == rect.y
                    && let Some(id) = self.ui.terminal_panel.as_ref().and_then(|terminal| {
                        terminal_tab_at_column(
                            terminal,
                            self.configuration.settings(),
                            rect.width,
                            column.saturating_sub(rect.x),
                        )
                    })
                {
                    self.process_effect(AppEffect::TerminalFocus(id));
                    return;
                }
                if over_explorer && let Some(mouse) = key.terminal_mouse() {
                    match mouse {
                        crate::editor::input::TerminalMouseInput::ScrollUp => {
                            self.move_explorer_selection(-3);
                            return;
                        }
                        crate::editor::input::TerminalMouseInput::ScrollDown => {
                            self.move_explorer_selection(3);
                            return;
                        }
                        _ => {}
                    }
                }
                let plugin_context = self
                    .compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row))
                    .and_then(|window| self.extension.window_context(window));
                if self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                    && self.ui.tool_menu.is_none()
                    && let Some(context) = plugin_context
                {
                    if let Some(window) = self
                        .compositor_projection()
                        .and_then(|projection| projection.hit_test(column, row))
                    {
                        if self
                            .window_session
                            .plan_focus(
                                window,
                                &self.state.window_version(),
                                self.extension.contributions().version(),
                            )
                            .and_then(|plan| self.apply_window_plan(plan))
                            .is_ok()
                        {
                            self.reconcile_editor_windows();
                        }
                    }
                    self.handle_key_in_context(&context, key);
                    return;
                }
                let context = if self.ui.picker.is_some() {
                    "picker_mouse"
                } else if self.input.prompt().is_some() {
                    "prompt_mouse"
                } else if self.ui.tool_menu.is_some() {
                    "tool_manager_mouse"
                } else if over_terminal {
                    "terminal_mouse"
                } else if over_explorer {
                    self.set_explorer_focus(true);
                    "explorer_mouse"
                } else if self.window_session.dashboard_visible()
                    || self.state.active_view.is_none()
                {
                    "dashboard_mouse"
                } else {
                    self.set_explorer_focus(false);
                    "editor_mouse"
                };
                if context == "terminal_mouse"
                    && self.window_session.terminal_focused()
                    && let Some(id) = self.keyboard_terminal()
                    && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
                    && let Some(rect) =
                        self.window_session
                            .native_ids()
                            .terminal(id.0)
                            .and_then(|ids| {
                                self.compositor_projection()?
                                    .window(ids.window)
                                    .map(|w| w.rect)
                            })
                    && let Some(geometry) = crate::ui::terminal_geometry_from_rect(rect)
                    && let Some(event) = key.terminal_mouse()
                {
                    let rect = WindowRect {
                        x: geometry.x,
                        y: geometry.body_top,
                        width: geometry.width,
                        height: geometry.body_height,
                    };
                    match self.terminal_session.send(
                        id,
                        TerminalInput::Mouse(event, column, row, rect),
                        &self.workspace,
                    ) {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(error) => {
                            self.show_error(error);
                            return;
                        }
                    }
                }
                self.handle_key_in_context(context, key);
            }
        }
    }

    pub(super) fn keyboard_terminal(&self) -> Option<PtyId> {
        if self.input.capture_context().is_some()
            || self.workspace_edits.proposal().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
        {
            return None;
        }
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
    }
    pub(super) fn active_input_context(&self) -> String {
        if let Some(context) = self.input.capture_context() {
            return context.into();
        }
        if let Some(pending) = self.input.pending_context() {
            return match pending {
                crate::editor::input::PendingInputContext::Register => "register",
                crate::editor::input::PendingInputContext::Prefix => "prefix",
                crate::editor::input::PendingInputContext::Replace => "replace",
                crate::editor::input::PendingInputContext::MacroRecord => "macro_record",
                crate::editor::input::PendingInputContext::MacroReplay => "macro_replay",
            }
            .into();
        }
        if self.workspace_edits.proposal().is_some() {
            return "workspace_edit".into();
        }
        if self.ui.picker.is_some() {
            if self.search.theme_picker_key().is_some() {
                return "theme_picker".into();
            }
            return if self.input.picker_normal() {
                "picker_normal"
            } else {
                "picker"
            }
            .into();
        }
        if self.ui.tool_menu.is_some() {
            return "tool_manager".into();
        }
        if self.keyboard_terminal().is_some() && self.terminal_window_rect().is_some() {
            return self
                .keyboard_terminal()
                .and_then(|id| self.terminal_session.mode(id))
                .map_or("terminal", |mode| match mode {
                    TerminalPanelMode::Input => "terminal",
                    TerminalPanelMode::Normal => "terminal_normal",
                    TerminalPanelMode::Visual => "terminal_visual",
                })
                .into();
        }
        if self.ui.language_completion.is_some() {
            return "completion".into();
        }
        if self.ui.explorer.is_some() && self.window_session.explorer_focused() {
            return "explorer".into();
        }
        if let Some(context) = self
            .window_session
            .graph()
            .focused()
            .and_then(|window| self.extension.window_context(window))
        {
            return context;
        }
        if self.window_session.dashboard_visible() || self.state.active_view.is_none() {
            return "dashboard".into();
        }
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        format!("editor.{}", mode_name(mode))
    }

    pub(super) fn send_terminal_focus_report(&mut self, focused: bool) {
        if let Some(id) = self.keyboard_terminal() {
            if let Err(error) =
                self.terminal_session
                    .send(id, TerminalInput::Focus(focused), &self.workspace)
            {
                self.show_error(error);
            }
        }
    }

    pub(super) fn handle_configured_key(&mut self, key: KeyEvent) {
        if self.window_session.terminal_focused()
            && self
                .keyboard_terminal()
                .and_then(|id| self.terminal_session.mode(id))
                == Some(TerminalPanelMode::Input)
        {
            let encoded = key.terminal_navigation().map(|key| match key {
                crate::editor::input::TerminalNavigationKey::Up => crate::ui::PtyInputKey::Up,
                crate::editor::input::TerminalNavigationKey::Down => crate::ui::PtyInputKey::Down,
                crate::editor::input::TerminalNavigationKey::Left => crate::ui::PtyInputKey::Left,
                crate::editor::input::TerminalNavigationKey::Right => crate::ui::PtyInputKey::Right,
                crate::editor::input::TerminalNavigationKey::Home => crate::ui::PtyInputKey::Home,
                crate::editor::input::TerminalNavigationKey::End => crate::ui::PtyInputKey::End,
            });
            if let Some(encoded) = encoded
                && let Some(id) = self.keyboard_terminal()
            {
                if let Err(error) =
                    self.terminal_session
                        .send(id, TerminalInput::Key(encoded), &self.workspace)
                {
                    self.show_error(error);
                }
                return;
            }
        }
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }

    pub(super) fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
        let scrolls_completion_documentation = context == "completion"
            && key.modifiers.control
            && matches!(key.code, crate::editor::input::KeyCode::Char('d' | 'u'));
        if context != "documentation" && !scrolls_completion_documentation {
            self.close_documentation();
        }
        self.diagnostics.dismiss_expansion();
        self.ui.expanded_diagnostic = None;
        let context = context.to_owned();
        let resolution = self.input.resolve(
            &context,
            key.clone(),
            self.configuration.keymaps(),
            self.configuration.binding_revision(),
            self.clock.monotonic(),
            Duration::from_millis(self.configuration.settings().input.mapping_timeout_ms),
        );
        self.project_input_hints();
        match resolution {
            MappingResolution::Ready(invocation) => {
                self.invoke_mapped(invocation);
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Replay(replay) => {
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Disabled => {}
            MappingResolution::Pending | MappingResolution::Ambiguous => return,
            MappingResolution::Missing => {
                let mut handled = false;
                if let Some(character) = key.text_character()
                    && let Some(command_id) = self
                        .configuration
                        .settings()
                        .input
                        .character_fallback_commands
                        .get(&context)
                        .cloned()
                        .or_else(|| {
                            self.configuration
                                .settings()
                                .input
                                .text_commands
                                .get(&context)
                                .cloned()
                        })
                {
                    self.invoke_text_command(&command_id, character.to_string());
                    handled = true;
                }
                if !handled
                    && let Some(command_id) = self
                        .configuration
                        .settings()
                        .input
                        .unmatched_commands
                        .get(&context)
                        .cloned()
                {
                    self.invoke_mapped_command(&command_id);
                    if self.active_input_context() != context {
                        self.handle_configured_key(key);
                    }
                }
            }
        }
        self.ensure_cursor_visible();
    }

    pub(super) fn handle_configured_paste(&mut self, text: String) {
        self.close_documentation();
        if self.window_session.terminal_focused()
            && let Some(id) = self.keyboard_terminal()
            && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
        {
            if let Err(error) =
                self.terminal_session
                    .send(id, TerminalInput::Paste(&text), &self.workspace)
            {
                self.show_error(error);
            }
            return;
        }
        self.cancel_pending_mapping();
        let context = self.active_input_context();
        if let Some(command_id) = self
            .configuration
            .settings()
            .input
            .paste_commands
            .get(&context)
            .cloned()
        {
            self.invoke_text_command(&command_id, text);
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn invoke_mapped_command(&mut self, command_id: &str) {
        match command(command_id) {
            Ok(invocation) => self.invoke_mapped(invocation),
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn invoke_text_command(&mut self, command_id: &str, text: String) {
        match command(command_id) {
            Ok(invocation) => {
                self.invoke(invocation.with_argument("text", CommandValue::String(text)))
            }
            Err(error) => self.show_error(error),
        }
    }

    #[cfg(test)]
    pub(super) fn invoke_insert_text(&mut self, text: String) {
        self.invoke_text_command("edit.insert_text", text);
    }

    pub(super) fn invoke_mapped(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        self.invoke(invocation);
    }

    pub(super) fn cancel_pending_mapping(&mut self) {
        self.input.cancel_mapping();
        self.project_input_hints();
    }
    pub(super) fn flush_keymap_timeout(&mut self) {
        let context = self.active_input_context();
        let invocation = self.input.flush_timeout(
            &context,
            self.configuration.binding_revision(),
            self.clock.monotonic(),
        );
        self.project_input_hints();
        if let Some(invocation) = invocation {
            self.invoke_mapped(invocation);
            self.ensure_cursor_visible();
        }
    }
    pub(super) fn update_key_hint_ui(&mut self) {
        let context = self.active_input_context();
        if self.input.prompt().is_some() || self.workspace_edits.proposal().is_some() {
            self.input.hide_hints();
        } else {
            self.input.update_hints(
                &context,
                (
                    self.configuration.keymaps(),
                    self.configuration.binding_revision(),
                ),
                &self.configuration.settings().ui.key_hints,
                &self.configuration.settings().command_labels,
                self.clock.monotonic(),
            );
        }
        self.project_input_hints();
    }

    pub(super) fn invoke(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("lsp.documentation.") {
            self.close_documentation();
        }
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view;
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
        }
        let visual_operator = invocation.id.as_str() == "edit.operator"
            && invocation
                .arguments
                .get("motion")
                .is_some_and(|motion| motion == &CommandValue::String("visual".into()));
        let suppress_repeat = visual_operator
            && invocation
                .arguments
                .get("operator")
                .is_some_and(|operator| operator == &CommandValue::String("yank".into()));
        let repeat_override = visual_operator
            .then(|| self.normalized_visual_repeat(&invocation))
            .flatten();
        match self.commands.execute(&self.state, &invocation) {
            Ok(outcome) => {
                let changed = outcome.transaction.is_some();
                if let Some(mut transaction) = outcome.transaction {
                    let source = self.state.window_version();
                    let snippet_document = self.input.snippet_buffer().and_then(|id| {
                        self.state.buffers.get(id).map(|buffer| {
                            crate::editor::session::input::SnippetDocument {
                                editor: &source,
                                buffer: id,
                                text: &buffer.text,
                                view: None,
                            }
                        })
                    });
                    let update = self
                        .input
                        .prepare_snippet_mirror(snippet_document, &mut transaction);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                        return;
                    }
                    if let Some(update) = update {
                        let source = self.state.window_version();
                        let document = self.input.snippet_buffer().and_then(|id| {
                            self.state.buffers.get(id).map(|buffer| {
                                crate::editor::session::input::SnippetDocument {
                                    editor: &source,
                                    buffer: id,
                                    text: &buffer.text,
                                    view: None,
                                }
                            })
                        });
                        self.input.accept_snippet_edit(update, document);
                    }
                }

                if changed {
                    self.push_change_location();
                }
                self.input.record_successful_invocation(
                    &invocation,
                    repeat_override.as_ref(),
                    suppress_repeat,
                );
                for effect in outcome.effects {
                    self.process_effect(effect);
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}
