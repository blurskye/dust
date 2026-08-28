use super::*;

impl InteractiveApplication {
    pub(super) fn update_notification_timer(&mut self) {
        if self.ui.notification != self.observed_notification {
            self.observed_notification = self.ui.notification.clone();
            self.notification_deadline = self.ui.notification.as_ref().and_then(|_| {
                let timeout = self.configuration.settings.ui.notifications.timeout_ms;
                (timeout > 0).then(|| Instant::now() + Duration::from_millis(timeout))
            });
        }
        if self
            .notification_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.ui.notification = None;
            self.ui.notification_error = false;
            self.observed_notification = None;
            self.notification_deadline = None;
        }
    }

    pub(super) fn toggle_buffer_pin(&mut self, buffer: crate::editor::BufferId) {
        if !self.state.buffers.contains_key(buffer) {
            return;
        }
        if !self.content_line_pins.remove(&buffer) {
            self.content_line_pins.insert(buffer);
        }
        self.content_line_order_revision = self.content_line_order_revision.saturating_add(1);
        self.content_line_projection_signature = None;
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
        let pinned = self.content_line_pins.contains(&buffer);
        let Some(index) = self
            .content_line_order
            .iter()
            .position(|candidate| *candidate == buffer)
        else {
            self.show_error("active buffer has no content-line entry");
            return;
        };
        let target = if direction < 0 {
            self.content_line_order[..index]
                .iter()
                .rposition(|candidate| self.content_line_pins.contains(candidate) == pinned)
        } else {
            self.content_line_order[index.saturating_add(1)..]
                .iter()
                .position(|candidate| self.content_line_pins.contains(candidate) == pinned)
                .map(|offset| index + 1 + offset)
        };
        let Some(target) = target else {
            self.show_error("buffer is already at the content-line boundary");
            return;
        };
        self.content_line_order.swap(index, target);
        self.content_line_order_revision = self.content_line_order_revision.saturating_add(1);
        self.content_line_projection_signature = None;
        self.reconcile_editor_windows();
    }

    pub(super) fn handle_content_tab_mouse(
        &mut self,
        key: &KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        if self.ui.prompt.is_some() || self.ui.picker.is_some() || self.ui.tool_menu.is_some() {
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
        let tab_line = self.configuration.settings.ui.tab_line.enabled
            && (!self.configuration.settings.ui.tab_line.hide_when_single
                || self.state.tabs.len() > 1);
        let buffer_line = self.configuration.settings.ui.buffer_line.enabled
            && (!self.configuration.settings.ui.buffer_line.hide_when_single
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
            &self.configuration.settings,
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
        let Some(buffer) = self.editor_windows.buffer_for_content(identity.0) else {
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
                self.terminal_width = width;
                self.terminal_height = height;
                self.surface_windows.invalidate();
                self.resize_active_terminal_to_panel();
                self.sync_terminal_panel();
                self.sync_explorer_ui();
                if self.configuration.settings.ui.keep_cursor_visible_on_resize {
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
                if self.ui.prompt.as_ref().is_some_and(|prompt| {
                    matches!(
                        prompt.kind,
                        PromptKind::WorkspaceTrust | PromptKind::GitReset
                    )
                }) {
                    self.close_prompt();
                }
                self.handle_key_in_context("focus", KeyEvent::focus_lost());
            }
            UiEvent::Mouse { key, column, row } => {
                self.mouse_position = Some((column, row));
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
                            &self.configuration.settings,
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
                    .and_then(|window| {
                        self.plugin_windows
                            .input_context(window, &self.plugin_contributions)
                    });
                if self.ui.picker.is_none()
                    && self.ui.prompt.is_none()
                    && self.ui.tool_menu.is_none()
                    && let Some(context) = plugin_context
                {
                    if let Some(window) = self
                        .compositor_projection()
                        .and_then(|projection| projection.hit_test(column, row))
                    {
                        let mut graph = self.windows.clone();
                        if graph.focus(window).and_then(|_| graph.validate()).is_ok() {
                            self.windows = graph;
                            self.surface_windows.invalidate();
                            self.reconcile_editor_windows();
                        }
                    }
                    self.handle_key_in_context(&context, key);
                    return;
                }
                let context = if self.ui.picker.is_some() {
                    "picker_mouse"
                } else if self.ui.prompt.is_some() {
                    "prompt_mouse"
                } else if self.ui.tool_menu.is_some() {
                    "tool_manager_mouse"
                } else if over_terminal {
                    "terminal_mouse"
                } else if over_explorer {
                    self.ui.explorer_focused = true;
                    "explorer_mouse"
                } else if self.ui.dashboard || self.state.active_view.is_none() {
                    "dashboard_mouse"
                } else {
                    self.ui.explorer_focused = false;
                    "editor_mouse"
                };
                if context == "terminal_mouse"
                    && self.terminal_focused
                    && let Some(id) = self.active_terminal
                    && let Some(terminal) = self.terminals.get(&id)
                    && terminal.mode == TerminalPanelMode::Input
                    && let Some(rect) = self.terminal_window_rect()
                    && let Some(event) = key.terminal_mouse()
                {
                    if let Some(bytes) =
                        crate::ui::encode_terminal_mouse_event(crate::ui::TerminalMouseEncoding {
                            event,
                            column,
                            row,
                            left: rect.x,
                            top: rect.y.saturating_add(1),
                            width: rect.width,
                            height: rect.height.saturating_sub(2),
                            mode: terminal.screen.mouse_mode(),
                            sgr: terminal.screen.sgr_mouse(),
                            ceiling: self.configuration.settings.terminals.maximum_input_bytes,
                        })
                    {
                        let _ = self.ptys.input(
                            id,
                            bytes,
                            self.configuration.settings.terminals.maximum_input_bytes,
                        );
                        return;
                    }
                }
                self.handle_key_in_context(context, key);
            }
        }
    }

    pub(super) fn active_input_context(&self) -> String {
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
        if self.terminal_focused
            && self.active_terminal.is_some()
            && self.terminal_window_rect().is_some()
        {
            return self
                .active_terminal
                .and_then(|id| self.terminals.get(&id))
                .map_or("terminal", |terminal| match terminal.mode {
                    TerminalPanelMode::Input => "terminal",
                    TerminalPanelMode::Normal => "terminal_normal",
                    TerminalPanelMode::Visual => "terminal_visual",
                })
                .into();
        }
        if self.workspace_proposal.is_some() {
            return "workspace_edit".into();
        }
        if self.ui.explorer.is_some() && self.ui.explorer_focused {
            return "explorer".into();
        }
        if self.ui.picker.is_some() {
            return "picker".into();
        }
        if self.ui.language_completion.is_some() {
            return "completion".into();
        }
        if let Some(prompt) = &self.ui.prompt {
            return match prompt.kind {
                PromptKind::Command => "command_prompt",
                PromptKind::SearchForward | PromptKind::SearchBackward => "search_prompt",
                PromptKind::WorkspaceTrust | PromptKind::GitReset => "workspace_trust_prompt",
            }
            .into();
        }
        if self.ui.tool_menu.is_some() {
            return "tool_manager".into();
        }
        if let Some(context) = self.windows.focused().and_then(|window| {
            self.plugin_windows
                .input_context(window, &self.plugin_contributions)
        }) {
            return context;
        }
        if self.ui.dashboard || self.state.active_view.is_none() {
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
        if self.terminal_focused
            && let Some(id) = self.active_terminal
            && let Some(terminal) = self.terminals.get(&id)
            && let Some(bytes) = crate::ui::encode_focus(focused, terminal.screen.focus_reporting())
        {
            let _ = self.ptys.input(
                id,
                bytes.to_vec(),
                self.configuration.settings.terminals.maximum_input_bytes,
            );
        }
    }

    pub(super) fn handle_configured_key(&mut self, key: KeyEvent) {
        if self.terminal_focused
            && self
                .active_terminal
                .and_then(|id| self.terminals.get(&id))
                .is_some_and(|terminal| terminal.mode == TerminalPanelMode::Input)
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
                && let Some(id) = self.active_terminal
                && let Some(terminal) = self.terminals.get(&id)
            {
                let bytes =
                    crate::ui::encode_key(encoded, terminal.screen.application_cursor()).to_vec();
                if bytes.len() <= self.configuration.settings.terminals.maximum_input_bytes {
                    let _ = self.ptys.input(
                        id,
                        bytes,
                        self.configuration.settings.terminals.maximum_input_bytes,
                    );
                }
                return;
            }
        }
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }

    pub(super) fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
        self.ui.documentation_popup = None;
        let context = context.to_owned();
        if self
            .pending_keymap
            .as_ref()
            .is_some_and(|pending| pending.context != context)
        {
            self.cancel_pending_mapping();
        }
        let resolution = self
            .configuration
            .keymaps
            .resolvers
            .get_mut(&context)
            .map_or(MappingResolution::Missing, |resolver| {
                resolver.feed(key.clone())
            });
        match resolution {
            MappingResolution::Ready(invocation) => {
                self.pending_keymap = None;
                self.invoke_mapped(invocation);
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.pending_keymap = None;
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Replay(replay) => {
                self.pending_keymap = None;
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Disabled => self.pending_keymap = None,
            MappingResolution::Pending | MappingResolution::Ambiguous => {
                let now = Instant::now();
                let prefix = self
                    .configuration
                    .keymaps
                    .pending_prefix(&context)
                    .expect("pending resolution owns a non-empty resolver sequence");
                let started_at = self.pending_keymap.as_ref().map_or(now, |pending| {
                    if pending.context == context
                        && pending.configuration_generation == self.configuration_generation
                    {
                        pending.started_at
                    } else {
                        now
                    }
                });
                self.pending_keymap = Some(PendingKeymap {
                    context,
                    prefix,
                    started_at,
                    deadline: now
                        + Duration::from_millis(
                            self.configuration.settings.input.mapping_timeout_ms,
                        ),
                    configuration_generation: self.configuration_generation,
                });
                self.ui.key_hints = None;
                return;
            }
            MappingResolution::Missing => {
                self.pending_keymap = None;
                let mut handled = false;
                if let Some(character) = key.text_character()
                    && let Some(command_id) = self
                        .configuration
                        .settings
                        .input
                        .character_fallback_commands
                        .get(&context)
                        .cloned()
                        .or_else(|| {
                            self.configuration
                                .settings
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
                        .settings
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
        self.ui.documentation_popup = None;
        if self.terminal_focused
            && let Some(id) = self.active_terminal
            && let Some(terminal) = self.terminals.get(&id)
            && terminal.mode == TerminalPanelMode::Input
        {
            if let Some(bytes) = crate::ui::encode_paste(
                &text,
                terminal.screen.bracketed_paste(),
                self.configuration.settings.terminals.maximum_input_bytes,
            ) {
                let _ = self.ptys.input(
                    id,
                    bytes,
                    self.configuration.settings.terminals.maximum_input_bytes,
                );
            }
            return;
        }
        self.cancel_pending_mapping();
        let context = self.active_input_context();
        if let Some(command_id) = self
            .configuration
            .settings
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
        self.ui.key_hints = None;
        if let Some(pending) = self.pending_keymap.take()
            && let Some(resolver) = self
                .configuration
                .keymaps
                .resolvers
                .get_mut(&pending.context)
        {
            resolver.cancel();
        }
    }

    pub(super) fn flush_keymap_timeout(&mut self) {
        let Some(pending) = self.pending_keymap.clone() else {
            self.ui.key_hints = None;
            return;
        };
        if Instant::now() < pending.deadline {
            return;
        }
        self.pending_keymap = None;
        self.ui.key_hints = None;
        if pending.context != self.active_input_context()
            || pending.configuration_generation != self.configuration_generation
        {
            if let Some(resolver) = self
                .configuration
                .keymaps
                .resolvers
                .get_mut(&pending.context)
            {
                resolver.cancel();
            }
            return;
        }
        if let Some(resolver) = self
            .configuration
            .keymaps
            .resolvers
            .get_mut(&pending.context)
        {
            if let MappingResolution::Ready(invocation) = resolver.flush_timeout() {
                self.invoke_mapped(invocation);
                self.ensure_cursor_visible();
            }
        }
    }

    pub(super) fn update_key_hint_ui(&mut self) {
        self.ui.key_hints = None;
        let Some(pending) = self.pending_keymap.clone() else {
            return;
        };
        if pending.configuration_generation != self.configuration_generation
            || pending.context != self.active_input_context()
        {
            self.cancel_pending_mapping();
            return;
        }
        let policy = &self.configuration.settings.ui.key_hints;
        let now = Instant::now();
        if !policy.enabled
            || self.ui.prompt.is_some()
            || self.workspace_proposal.is_some()
            || now >= pending.deadline
            || now.duration_since(pending.started_at) < Duration::from_millis(policy.delay_ms)
        {
            return;
        }
        let Some(projection) = self.configuration.keymaps.hints_for_prefix(
            &pending.context,
            &pending.prefix,
            &self.configuration.settings.command_labels,
        ) else {
            return;
        };
        let rows = projection
            .rows
            .into_iter()
            .filter(|row| {
                row.mappings
                    .iter()
                    .any(|mapping| mapping.operation == KeymapOperation::Command)
            })
            .map(|row| {
                let group = row.group_id.is_some();
                KeyHintUiRow {
                    next_key: row.next_key,
                    label: if group || (row.direct && policy.show_command_titles) {
                        row.label
                    } else {
                        policy.fallback_label.clone()
                    },
                    icon: (group && policy.show_group_icons)
                        .then_some(row.icon)
                        .flatten(),
                    group,
                }
            })
            .take(usize::from(policy.max_rows) * usize::from(policy.max_columns))
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            self.ui.key_hints = Some(KeyHintUiState {
                prefix: projection.prefix,
                rows,
            });
        }
    }

    pub(super) fn invoke(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("lsp.documentation.") {
            self.ui.documentation_popup = None;
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
                    let snippet_groups = self.prepare_snippet_mirror_transaction(&mut transaction);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                        return;
                    }
                    if let Some(groups) = snippet_groups
                        && let Some(session) = self.snippet_session.as_mut()
                    {
                        session.groups = groups;
                        session.revision = self.state.buffers[session.buffer].text.revision();
                    }
                }

                if changed {
                    self.push_change_location();
                }
                self.record_successful_invocation(
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
