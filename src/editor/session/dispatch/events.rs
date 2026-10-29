use super::{
    AppEffect, BTreeMap, BufferRevision, ByteOffset, ByteRange, CommandInvocation, CommandValue,
    CompletionItem, DiagnosticOwner, DocumentEvent, DocumentationMarkupKind,
    DocumentationPopupKind, Duration, EditorMode, EditorOperation, EditorTransaction,
    ExplorerMutation, ExplorerWatchService, HashSet, IndentError, Instant, InteractiveApplication,
    InteractiveError, KeyEvent, KeyInputKind, LanguageCompletionState, LoadedConfig, LogLevel,
    LspEvent, LspFeature, LspFeatureResult, LspLaunch, LspMarkupContent, LspMarkupKind,
    LspProgressKind, LspRuntimeSource, LspWorkspaceEdit, MappingResolution, MatcherSource,
    NativeSurfaceTarget, PersistedLocation, PickerAction, PickerItem, PickerItemId, PickerPayload,
    PickerSurfaceKind, PositionCodec, PositionEncoding, Precondition, PromptKind, PtyId,
    ResourceIntentKind, RestoreContext, RestoreUpdate, SyntaxDocument, SyntaxLanguage,
    SyntaxSupport, TerminalInput, TerminalPanelMode, TextBuffer, TextEdit, TextEditSet, UiEvent,
    UiFold, UiInlayHint, UiSyntaxSpan, UndoPolicy, VfsResult, WindowRect, command,
    discover_executable, explorer_watch_policy, language_for_path, mode_name, path_from_file_uri,
    rect_contains_point, semantic_role, terminal_tab_at_column,
};

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
        if matches!(event, UiEvent::Key { .. }) {
            self.last_key_at = Some(Instant::now());
        }
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
                            | PromptKind::PluginConfirmation
                            | PromptKind::TerminalClose
                            | PromptKind::DirtyClose
                            | PromptKind::GitStageUnsaved
                            | PromptKind::GitDestructive
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
                let explorer_window = (!over_terminal)
                    .then(|| {
                        self.compositor_projection()
                            .and_then(|projection| projection.hit_test(column, row))
                    })
                    .flatten()
                    .filter(|window| {
                        matches!(
                            self.window_session.native_ids().target_for_window(*window),
                            Some(
                                NativeSurfaceTarget::Explorer
                                    | NativeSurfaceTarget::ExplorerView(_)
                            )
                        )
                    })
                    .or_else(|| {
                        (!over_terminal
                            && self
                                .explorer_window_rect()
                                .is_some_and(|rect| rect_contains_point(rect, column, row)))
                        .then(|| {
                            self.window_session
                                .native_ids()
                                .explorer()
                                .map(|ids| ids.window)
                        })
                        .flatten()
                    });
                let over_explorer = explorer_window.is_some();
                if let Some(window) = explorer_window {
                    if let Ok(plan) = self.window_session.plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    ) {
                        if self.apply_window_plan(plan).is_ok() {
                            self.reconcile_editor_windows();
                        }
                    }
                }
                if over_terminal
                    && key.terminal_mouse()
                        == Some(crate::editor::input::TerminalMouseInput::LeftDown)
                {
                    if let Some(rect) = self.terminal_window_rect() {
                        if row == rect.y {
                            if let Some(id) = self.ui.terminal_panel.as_ref().and_then(|terminal| {
                                terminal_tab_at_column(
                                    terminal,
                                    self.configuration.settings(),
                                    rect.width,
                                    column.saturating_sub(rect.x),
                                )
                            }) {
                                self.process_effect(AppEffect::TerminalFocus(id));
                                return;
                            }
                        }
                    }
                }
                if over_explorer {
                    if let Some(mouse) = key.terminal_mouse() {
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
                }
                let plugin_context = self
                    .compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row))
                    .and_then(|window| self.extension.window_context(window));
                if self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                    && self.ui.tool_menu.is_none()
                {
                    if let Some(context) = plugin_context {
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
                    "explorer_mouse"
                } else if self.window_session.dashboard_visible()
                    || self.state.active_view.is_none()
                {
                    "dashboard_mouse"
                } else {
                    self.set_explorer_focus(false);
                    "editor_mouse"
                };
                if context == "terminal_mouse" && self.window_session.terminal_focused() {
                    if let Some(id) = self.keyboard_terminal() {
                        if self.terminal_session.mode(id) == Some(TerminalPanelMode::Input) {
                            if let Some(rect) = self
                                .window_session
                                .native_ids()
                                .terminal(id.0)
                                .and_then(|ids| {
                                    self.compositor_projection()?
                                        .window(ids.window)
                                        .map(|w| w.rect)
                                })
                            {
                                if let Some(geometry) = crate::ui::terminal_geometry_from_rect(rect)
                                {
                                    if let Some(event) = key.terminal_mouse() {
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
                                }
                            }
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
                crate::editor::input::PendingInputContext::Character => "character",
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
            if let Some(encoded) = encoded {
                if let Some(id) = self.keyboard_terminal() {
                    if let Err(error) =
                        self.terminal_session
                            .send(id, TerminalInput::Key(encoded), &self.workspace)
                    {
                        self.show_error(error);
                    }
                    return;
                }
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
                if let Some(character) = key.text_character() {
                    if let Some(command_id) = self
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
                }
                if !handled {
                    if let Some(command_id) = self
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
        }
        self.ensure_cursor_visible();
    }

    pub(super) fn handle_configured_paste(&mut self, text: String) {
        self.close_documentation();
        if self.window_session.terminal_focused() {
            if let Some(id) = self.keyboard_terminal() {
                if self.terminal_session.mode(id) == Some(TerminalPanelMode::Input) {
                    if let Err(error) =
                        self.terminal_session
                            .send(id, TerminalInput::Paste(&text), &self.workspace)
                    {
                        self.show_error(error);
                    }
                    return;
                }
            }
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

    /// Vim never leaves the Normal-mode cursor on the empty line after a final newline; `dd`
    /// on the last line lands on the line above instead.
    fn keep_cursor_off_final_newline(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let Some(view) = self.state.views.get(view_id) else {
            return;
        };
        if view.mode != EditorMode::Normal {
            return;
        }
        let text = &self.state.buffers[view.buffer].text;
        let last = text.visible_lines().saturating_sub(1);
        if text.byte_to_line(view.cursor).unwrap_or(0) <= last {
            return;
        }
        let Ok(start) = text.line_to_byte(last) else {
            return;
        };
        let indent = text.line(last).map_or(0, |line| {
            line.find(|character: char| !character.is_whitespace())
                .unwrap_or(0)
        });
        self.state.views[view_id].cursor = ByteOffset(start.get() + indent);
    }

    /// Whether yanks and pastes go through the system clipboard (`clipboard = "unnamedplus"`).
    pub(super) fn clipboard_shared(&self) -> bool {
        self.configuration.settings().editor.registers.clipboard
            == crate::config::ClipboardSync::Unnamedplus
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
        if invocation.id.as_str() == "edit.put"
            && matches!(invocation.register, None | Some('"' | '+' | '*'))
            && self.clipboard_shared()
        {
            let enabled = self.state.policy.registers_enabled;
            self.clipboard.pull(&mut self.state.registers, enabled);
        }
        if invocation.id.as_str() == "edit.operator"
            && invocation.arguments.get("operator") == Some(&CommandValue::String("comment".into()))
            && !invocation.arguments.contains_key("comment")
        {
            let token = invocation
                .target
                .buffer
                .and_then(|id| self.state.buffers.get(id))
                .and_then(|buffer| buffer.path.as_deref())
                .and_then(|path| path.extension())
                .and_then(|extension| {
                    self.configuration
                        .settings()
                        .editor
                        .comments
                        .get(extension.to_string_lossy().as_ref())
                        .cloned()
                });
            if let Some(token) = token {
                invocation = invocation.with_argument("comment", CommandValue::String(token));
            }
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
                    self.keep_cursor_off_final_newline();
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
            Err(error) => {
                // A failed `dfz` or `di(` must not leave the view stuck in operator-pending.
                if self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view))
                    .is_some_and(|view| view.mode == EditorMode::OperatorPending)
                {
                    self.set_mode(EditorMode::Normal);
                }
                self.show_error(error);
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_configuration_result(&mut self) {
        let result = match self.configuration.next_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let automatic = result.automatic;
        let loaded = match result.candidate {
            Ok(loaded) => loaded,
            Err(diagnostic) => {
                self.retain_configuration_watch();
                let message = if automatic {
                    "auto_reload_rejected"
                } else {
                    "reload_rejected"
                };
                self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
                return;
            }
        };
        if let Err(diagnostic) = self.apply_configuration_candidate(loaded, automatic) {
            self.retain_configuration_watch();
            let message = if automatic {
                "auto_reload_rejected"
            } else {
                "reload_rejected"
            };
            self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
        }
    }

    pub(in crate::editor::session) fn retain_configuration_watch(&self) {
        self.configuration.retain_watch();
    }

    pub(in crate::editor::session) fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let candidate =
            self.configuration
                .prepare(loaded, &self.commands, self.extension.contributions())?;
        let replacement_explorer_watch = if candidate.view().settings.ui.explorer.watch
            != self.configuration.settings().ui.explorer.watch
        {
            Some(
                ExplorerWatchService::start(explorer_watch_policy(
                    &candidate.view().settings.ui.explorer.watch,
                ))
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let window_candidate = (candidate.view().settings.windows
            != self.configuration.settings().windows)
            .then(|| self.prepare_configured_window_rules(&candidate.view().settings.windows))
            .transpose()?;
        self.configuration
            .stage_watch(&candidate, self.extension.contributions())?;
        if let Some(window_candidate) = window_candidate {
            self.window_session
                .accept_current(window_candidate, &self.state.window_version())?;
        }
        let restart_required = candidate.view().settings.runtime.vfs_queue_capacity
            != self.configuration.settings().runtime.vfs_queue_capacity
            || candidate.view().settings.ui.alternate_screen
                != self.configuration.settings().ui.alternate_screen
            || candidate.view().settings.ui.bracketed_paste
                != self.configuration.settings().ui.bracketed_paste
            || candidate.view().settings.ui.focus_events
                != self.configuration.settings().ui.focus_events;
        self.state.policy = crate::editor::EditingPolicy {
            tab_width: candidate.view().settings.editor.tab_width,
            indent_width: candidate.view().settings.editor.indent_width,
            expand_tabs: candidate.view().settings.editor.expand_tabs,
            undo_break_on_text_commit: candidate.view().settings.editor.undo_break_on_text_commit,
            registers_enabled: candidate.view().settings.editor.registers.storage
                == crate::config::RegisterStorage::Internal,
            mirror_named_to_unnamed: candidate
                .view()
                .settings
                .editor
                .registers
                .mirror_named_to_unnamed,
            ambiguous_width_wide: candidate.view().settings.editor.ambiguous_width
                == crate::config::AmbiguousWidth::Wide,
            autopairs: candidate.view().settings.editor.autopairs,
        };
        self.state.mark_editor_changed();
        if candidate.view().settings.observability != self.configuration.settings().observability {
            self.notices
                .configure_logs(&candidate.view().settings, true);
            self.notices.record(
                LogLevel::Info,
                "configuration",
                "logging-reconfigured",
                BTreeMap::new(),
            );
        }
        self.recovery.configure(
            &self.configuration.settings().recovery,
            &candidate.view().settings.recovery,
        );
        if self.window_session.dashboard_visible()
            && !candidate.view().settings.startup.show_dashboard
        {
            self.state.open_text(TextBuffer::from_text(""), None);
            self.set_dashboard_visibility(false);
        }
        self.configuration
            .commit(candidate, self.extension.contributions())?;
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.diagnostics.set_limits(
            self.configuration.settings().workspace.maximum_diagnostics,
            self.configuration
                .settings()
                .workspace
                .maximum_diagnostics_per_owner_uri,
        );
        self.sync_ui_diagnostics();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("configuration changed");
        self.retire_plugin_edit_sources();
        self.input.reconfigure(self.configuration.settings());
        if self
            .terminal_session
            .configure(&self.configuration.settings().terminals)
        {
            self.reject_terminal_responses("terminal configuration changed");
        }
        self.restore_input_focus();
        self.project_configuration();
        self.project_input_hints();
        self.project_input_documentation();
        self.refresh_completions();
        let maximum_inlays = self.configuration.settings().lsp.inlay_hint_level
            == crate::config::InlayHintLevel::Max;
        let sessions = self
            .language
            .sessions()
            .iter()
            .map(|(key, state)| (key.clone(), state.server_id.clone()))
            .collect::<Vec<_>>();
        for (key, server_id) in sessions {
            let configuration = if maximum_inlays {
                crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&server_id)
            } else {
                crate::language::lsp::LspServerConfiguration::standard()
            };
            let _ = self.language.configure(&key, configuration);
        }
        self.schedule_lsp_inlay_refresh(true);
        self.explorer.reconfigure_watch(replacement_explorer_watch);
        if !self.configuration.settings().ui.explorer.bundled_enabled {
            self.ui.explorer = None;
            self.set_explorer_focus(false);
        }
        self.reconcile_explorer_watch();
        self.window_session.constrain_terminal_height(
            self.configuration
                .settings()
                .ui
                .terminal_panel
                .minimum_height,
            self.configuration
                .settings()
                .ui
                .terminal_panel
                .maximum_height,
        );
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.rebuild_matcher_cache();
        if let Some(menu) = &self.ui.tool_menu {
            let category = menu.category.clone();
            self.rebuild_tool_menu(&category, None);
        }
        self.ui.config_diagnostic = None;
        self.notify_message(
            if restart_required {
                self.message("reloaded_restart", &[])
            } else if automatic {
                self.message("auto_reloaded", &[])
            } else {
                self.message("reloaded", &[])
            },
            false,
        );
        self.ensure_cursor_visible();
        Ok(())
    }
}

use std::ops::ControlFlow::{Break, Continue};

enum NavigationRead {
    Matcher(MatcherSource),
    Persisted(PersistedLocation),
    Lsp(crate::language::lsp::LspRange, PositionEncoding),
    WorkspaceSearch(std::ops::Range<usize>),
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn schedule_recovery_work(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            false,
            self.documents.recovery_io(),
        );
    }
    pub(in crate::editor::session) fn queue_dirty_recovery_before_exit(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            true,
            self.documents.recovery_io(),
        );
    }

    pub(in crate::editor::session) fn process_vfs_results(&mut self) {
        loop {
            let result = match self
                .documents
                .poll(&self.state.window_version(), self.workspace.root_revision())
            {
                Ok(Some(DocumentEvent::Legacy(result))) => result,
                Ok(Some(DocumentEvent::TaskRead(reply))) => {
                    if let Err(error) = self.tasks.accept_linter_read(
                        reply,
                        &self.state,
                        &self.workspace,
                        self.configuration.binding_revision(),
                        &self.diagnostics,
                    ) {
                        self.show_error(error);
                    }
                    self.drain_linter_sources();
                    continue;
                }
                Ok(Some(DocumentEvent::WorkspaceIo(reply))) => {
                    self.process_workspace_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Explorer(reply))) => {
                    self.process_explorer_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::File(reply))) => {
                    self.process_file_reply(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Stopped {
                    requests,
                    error,
                    task_reads,
                })) => {
                    for id in requests {
                        self.input.retire_document_request(id);
                        self.search.retire_document_request(id);
                        self.language.retire_document_request(id);
                        self.persistence.retire_document_request(id);
                    }
                    self.recovery.stopped();
                    self.search.clear_preview();
                    self.explorer.clear_preview();
                    self.explorer.retire_intents();
                    self.persistence.restore_transport_stopped();
                    if self.workspace_edits.proposal().is_some() || self.workspace_edits.preparing()
                    {
                        self.reject_workspace_proposal();
                    }
                    self.finish_workspace_resource_obligations("document worker stopped");
                    self.tasks.linter_worker_stopped(task_reads);
                    self.drain_linter_sources();
                    self.show_error(error);
                    break;
                }
                Ok(Some(DocumentEvent::Discarded)) => continue,
                Ok(Some(DocumentEvent::Startup {
                    receipt,
                    path,
                    text,
                })) => {
                    let buffer = match text
                        .and_then(|text| self.open_startup_document(text, path.clone()))
                    {
                        Ok(buffer) => {
                            self.notify_message(
                                self.message("opened", &[("path", path.display().to_string())]),
                                false,
                            );
                            Some(buffer)
                        }
                        Err(error) => {
                            self.show_error(error);
                            None
                        }
                    };
                    self.documents.finish_startup(
                        receipt,
                        buffer,
                        &self.state.window_version(),
                        self.workspace.root_revision(),
                    );
                    self.finish_startup_reads_if_complete();
                    continue;
                }
                Ok(None) => break,
                Err(error) => {
                    self.show_error(error);
                    break;
                }
            };
            let result = match self.persistence.accept_restore(
                result,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) {
                Break(RestoreUpdate::Waiting) => continue,
                Break(RestoreUpdate::Ready(ready)) => {
                    self.finish_session_restore(*ready);
                    continue;
                }
                Break(RestoreUpdate::Failed(error)) => {
                    self.cancel_session_restore();
                    self.show_error(error);
                    continue;
                }
                Continue(result) => result,
            };
            let result = match self.recovery.accept(result) {
                Break(update) => {
                    use crate::editor::session::recovery::RecoveryUpdate;
                    match update {
                        RecoveryUpdate::Quiet | RecoveryUpdate::Pruned(0) => {}
                        RecoveryUpdate::Error(error) => self.show_error(error),
                        RecoveryUpdate::Pruned(count) => self.notify_message(
                            self.message("recovery_pruned", &[("count", count.to_string())]),
                            false,
                        ),
                        RecoveryUpdate::Discarded => {
                            self.notify_message(self.message("recovery_discarded", &[]), false)
                        }
                        RecoveryUpdate::CrashCleared => {
                            self.notify_message(self.message("crash_cleared", &[]), false)
                        }
                    }
                    continue;
                }
                Continue(result) => result,
            };
            match result {
                VfsResult::RecoveryDiscovered { .. }
                | VfsResult::RecoveryCleaned { .. }
                | VfsResult::RecoveryWritten { .. } => {
                    self.show_error(self.message("unexpected_completion", &[]))
                }
                VfsResult::Deleted { result, .. } => match result {
                    Err(error) => self.show_error(error),
                    Ok(()) => self.show_error(self.message("unexpected_completion", &[])),
                },
                VfsResult::ExplorerMutated { .. } => {
                    self.show_error("unowned explorer mutation result")
                }
                VfsResult::ResourceApplied { .. }
                | VfsResult::ResourceResolved { .. }
                | VfsResult::CreateTarget { .. } => self.show_error("unowned workspace I/O result"),
                VfsResult::Read { id, path, result } => {
                    if self.search.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_picker_ui();
                        continue;
                    }
                    if self.explorer.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_explorer_ui();
                        continue;
                    }
                    let pending = self
                        .search
                        .take_matcher_navigation(id)
                        .map(NavigationRead::Matcher)
                        .or_else(|| {
                            self.persistence
                                .take_navigation_read(id)
                                .map(NavigationRead::Persisted)
                        })
                        .or_else(|| {
                            self.language
                                .take_navigation_read(id)
                                .map(|(range, encoding)| NavigationRead::Lsp(range, encoding))
                        })
                        .or_else(|| {
                            self.search
                                .take_workspace_navigation(id)
                                .map(NavigationRead::WorkspaceSearch)
                        });
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(NavigationRead::Matcher(source)) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let start = self.state.buffers[buffer]
                                    .text
                                    .line_to_byte(source.line)
                                    .ok()
                                    .map(|offset| {
                                        ByteOffset(
                                            (offset.get() + source.column)
                                                .min(self.state.buffers[buffer].text.len_bytes()),
                                        )
                                    })
                                    .filter(|offset| {
                                        self.state.buffers[buffer]
                                            .text
                                            .validate_offset(*offset)
                                            .is_ok()
                                    })
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Persisted(location)) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let cursor = ByteOffset(location.byte.min(length));
                                let anchor = ByteOffset(location.anchor.min(length));
                                self.state.views[view].cursor = cursor;
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].scroll_wrap_row = 0;
                                self.state.views[view].scroll_column = location.scroll_column;
                                self.state.views[view].selection =
                                    (anchor != cursor).then_some(crate::editor::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editor::SelectionKind::Character,
                                    });
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Lsp(range, encoding)) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let position = self.state.buffers.get(buffer).and_then(|buffer| {
                                    PositionCodec::new(&buffer.text)
                                        .lsp_to_byte(
                                            range.start.line,
                                            range.start.character,
                                            encoding,
                                        )
                                        .ok()
                                });
                                if let Some(position) = position {
                                    let transaction = EditorTransaction::new(self.transaction_id())
                                        .with_precondition(Precondition::ViewDisplays {
                                            view,
                                            buffer,
                                        })
                                        .with_operation(EditorOperation::SetCursor {
                                            view,
                                            position,
                                        })
                                        .with_undo_policy(UndoPolicy::Ignore);
                                    if let Err(error) = self.state.apply_transaction(&transaction) {
                                        self.show_error(error);
                                    }
                                } else {
                                    self.show_error(self.message("lsp_invalid_position", &[]));
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::WorkspaceSearch(range)) => {
                                let start = ByteOffset(range.start);
                                let end = ByteOffset(range.end);
                                if text.validate_offset(start).is_err()
                                    || text.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = self.state.open_text(text, Some(path));
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: start,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.input.take_completion_read(id);
                    match (pending, result) {
                        (Some(key), _) if !self.input.completion_current(&key) => {
                            self.refresh_completions();
                        }
                        (Some(key), Ok(paths)) => {
                            self.input.accept_prompt_paths(
                                &key,
                                paths,
                                &self.state.workspace_root,
                                self.configuration.settings(),
                            );
                            self.project_input_prompt();
                        }
                        (Some(_), Err(_)) => {}
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { result, .. } => {
                    if let Err(error) = result {
                        self.show_error(error);
                    } else {
                        self.show_error(self.message("unexpected_completion", &[]));
                    }
                }
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_explorer_io(
        &mut self,
        reply: crate::files::ExplorerIoReply,
    ) {
        use crate::editor::session::explorer::MutationUpdate;
        if let crate::files::ExplorerIoReply::RenameInspected { id, result } = reply {
            match self.explorer.accept_rename_inspection(
                id,
                result,
                self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            ) {
                Some(Ok(root)) => self.prompt_help(format!(
                    "Relative to `{}`. Enter renames; Esc cancels.",
                    root.display()
                )),
                Some(Err(error)) => self.prompt_error(error),
                None => {}
            }
            return;
        }
        if let crate::files::ExplorerIoReply::Opened { id, path, result } = reply {
            let Some(reply) = self.explorer.accept_open(
                id,
                &path,
                (
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                ),
                &self.state.window_version(),
            ) else {
                return;
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            if let Err(error) = self
                .window_session
                .check_editor_admission(&(&self.state).into())
            {
                self.show_error(error);
                return;
            }
            let text = match result
                .map_err(|e| e.to_string())
                .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string()))
            {
                Ok(text) => text,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            let activate = self.window_session.navigation_current(
                &reply.navigation,
                &self.state.window_version(),
                self.state.active_view,
            );
            let mut draft = self.state.draft();
            if activate {
                if reply.split && self.state.active_view.is_some() {
                    if let Err(error) =
                        draft.split_document(self.configuration.settings().ui.panes.default_ratio)
                    {
                        self.show_error(error);
                        return;
                    }
                }
                draft.open_document(text, path);
            } else {
                draft.load_document(text, path);
            }
            if let Err(error) = self.commit_editor_draft(draft, None, activate) {
                self.show_error(error);
                return;
            }
            if activate {
                self.set_dashboard_visibility(false);
                self.ensure_cursor_visible();
            }
            return;
        }
        let Some(update) = self.explorer.accept_mutation(
            reply,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) else {
            return;
        };
        match update {
            MutationUpdate::Confirmation(path) => {
                self.open_confirmation(
                    PromptKind::ExplorerDelete,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .explorer_delete_title
                        .clone(),
                    explorer_delete_message(&path),
                    crate::ui::ConfirmationChoice::No,
                );
            }
            MutationUpdate::Apply(prepared) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.apply_prepared_explorer(prepared);
            }
            MutationUpdate::Failed(error) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.prompt_error(error);
                } else {
                    self.show_error(error);
                }
            }
            MutationUpdate::Complete { operation, change } => {
                let reconciliation = (|| -> Result<(), String> {
                    self.window_session
                        .check_editor_admission(&(&self.state).into())?;
                    if let Some(change) = change {
                        let target = match &operation {
                            ExplorerMutation::Rename { target, .. } => Some(target.as_path()),
                            _ => None,
                        };
                        let (draft, recent) = self.state.reconcile_paths(change, target)?;
                        if let Some(draft) = draft {
                            self.commit_editor_draft(draft, None, false)?;
                        }
                        for path in recent {
                            self.record_recent_file(path);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = reconciliation {
                    self.show_error(error);
                } else {
                    self.notify_message(
                        format!(
                            "explorer operation complete: {}",
                            operation
                                .target()
                                .or_else(|| operation.source())
                                .map_or_else(|| "path".into(), |path| path.display().to_string())
                        ),
                        false,
                    );
                }
                self.refresh_explorer();
                self.start_git_status_refresh_silent();
            }
        }
    }
}

impl InteractiveApplication {
    fn process_workspace_io(&mut self, reply: crate::editor::session::documents::WorkspaceIoReply) {
        if self.workspace_edits.owns_preparation_reply(&reply) {
            if let VfsResult::ResourceResolved {
                result: Ok(intent), ..
            } = reply.result()
            {
                let target = match &intent.kind {
                    ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                    ResourceIntentKind::Delete => None,
                };
                if let Err(error) = self.state.check_resource_destination(target) {
                    self.abort_workspace_edit(error);
                    return;
                }
            }
            match self.workspace_edits.accept_preparation_reply(
                reply,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            ) {
                Ok(()) => {
                    if self.workspace_edits.files_ready() {
                        self.finish_workspace_proposal();
                    }
                }
                Err(error) => self.abort_workspace_edit(error),
            }
            return;
        }

        let Some(result) = self.workspace_edits.resource_finished(
            reply,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let (change, target) = request.into_change();
        match self.state.reconcile_paths(change, target.as_deref()) {
            Ok((draft, recent)) => {
                if let Some(draft) = draft {
                    if let Err(error) = self.commit_editor_draft(draft, None, false) {
                        self.show_error(error);
                        return;
                    }
                }
                for path in recent {
                    self.record_recent_file(path);
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn retire_workspace_resources(&mut self, reason: &str) {
        let (notices, requests) = self.workspace_edits.retire_resources(reason);
        for notice in notices {
            self.show_error(notice);
        }
        for id in requests {
            if let Some(cancelled) = self.documents.cancel_workspace_queued(id) {
                if let Some(notice) = self.workspace_edits.resource_cancelled(cancelled) {
                    self.show_error(notice);
                }
            } else {
                self.show_error(
                    "resource publication retired; its filesystem outcome is still pending",
                );
            }
        }
    }
    pub(in crate::editor::session) fn finish_workspace_resource_obligations(
        &mut self,
        reason: &str,
    ) {
        for notice in self.workspace_edits.finish_resources(reason) {
            self.show_error(notice);
        }
    }
}

fn explorer_delete_message(path: &std::path::Path) -> String {
    format!("Permanently delete {path:?}?")
}

#[cfg(all(test, unix))]
#[test]
fn explorer_delete_message_uses_truthful_path_debug_escaping() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    assert_eq!(
        explorer_delete_message(std::path::Path::new("a  界\n\\b")),
        "Permanently delete \"a  界\\n\\\\b\"?"
    );
    let raw = PathBuf::from(OsString::from_vec(b"raw-\xff".to_vec()));
    assert_eq!(
        explorer_delete_message(&raw),
        "Permanently delete \"raw-\\xFF\"?"
    );
}

impl InteractiveApplication {
    pub(super) fn syntax_language_label(&self, language: SyntaxLanguage) -> String {
        self.message(
            match language {
                SyntaxLanguage::Rust => "syntax_language_rust",
                SyntaxLanguage::TypeScript => "syntax_language_typescript",
                SyntaxLanguage::Tsx => "syntax_language_tsx",
                SyntaxLanguage::JavaScript => "syntax_language_javascript",
                SyntaxLanguage::Go => "syntax_language_go",
                SyntaxLanguage::Json => "syntax_language_json",
                SyntaxLanguage::Toml => "syntax_language_toml",
                SyntaxLanguage::Markdown => "syntax_language_markdown",
            },
            &[],
        )
    }

    pub(super) fn syntax_support_label(&self, support: SyntaxSupport) -> String {
        self.message(
            match support {
                SyntaxSupport::Full => "syntax_support_full",
                SyntaxSupport::Partial => "syntax_support_partial",
                SyntaxSupport::Unsupported => "syntax_support_unsupported",
            },
            &[],
        )
    }

    pub(super) fn apply_syntax_indent_line(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let Some(language) = buffer.path.as_deref().and_then(language_for_path) else {
            self.show_error(self.message("syntax_indent_unavailable", &[]));
            return;
        };
        let revision = buffer.text.revision();
        let line = match buffer.text.byte_to_line(cursor) {
            Ok(line) => line,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let levels = match self.syntax.indentation(buffer_id, revision, language, line) {
            Ok(levels) => levels,
            Err(error) => {
                let message = match error {
                    IndentError::Pending => self.message("syntax_indent_pending", &[]),
                    IndentError::Unsupported => self.message(
                        "syntax_indent_unsupported",
                        &[("language", self.syntax_language_label(language))],
                    ),
                    IndentError::Truncated => self.message("syntax_indent_truncated", &[]),
                };
                self.show_error(message);
                return;
            }
        };
        let desired = if self.state.policy.expand_tabs {
            " ".repeat(levels.saturating_mul(self.state.policy.indent_width))
        } else {
            "\t".repeat(levels)
        };
        let text = match buffer.text.line(line) {
            Ok(text) => text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let whitespace = text
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        if text.get(..whitespace) == Some(desired.as_str()) {
            self.notify_message(self.message("syntax_indent_unchanged", &[]), false);
            return;
        }
        let start = match buffer.text.line_to_byte(line) {
            Ok(start) => start,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let end = ByteOffset(start.get().saturating_add(whitespace));
        let edits = TextEditSet::new(
            revision,
            vec![TextEdit::new(
                ByteRange::new(start, end).expect("ordered indentation"),
                desired,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.notify_message(self.message("syntax_indent_applied", &[]), false);
        }
    }

    pub(super) fn process_syntax(&mut self) {
        let documents = self
            .state
            .buffers
            .iter()
            .map(|(buffer, state)| SyntaxDocument {
                buffer,
                text: &state.text,
                language: state.path.as_deref().and_then(language_for_path),
                enabled: !(self.effective_large_file(buffer)
                    && self
                        .configuration
                        .settings()
                        .editor
                        .large_file
                        .disable_syntax),
            })
            .collect::<Vec<_>>();
        let too_large = self.message("syntax_source_too_large", &[]);
        self.syntax.tick(
            &documents,
            &self.configuration.settings().syntax,
            self.clock.monotonic(),
            &too_large,
        );
        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax.result(view.buffer) {
                for fold in &result.folds {
                    syntax_folds.push(UiFold {
                        view: view_id,
                        start_line: fold.start_row,
                        end_line: fold.end_row,
                        collapsed: collapsed.contains(&(view_id, fold.start_row, fold.end_row)),
                    });
                }
            }
        }
        self.ui.syntax_folds = syntax_folds;
        self.ui.syntax_spans = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.spans.iter().filter_map(|span| {
                    Some(UiSyntaxSpan {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        role: span.role,
                    })
                })
            })
            .collect();
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_ui_diagnostics(&mut self) {
        if let Some(candidate) = self.diagnostics.projection(&self.state) {
            self.ui.diagnostics = candidate;
            self.window_session.diagnostics_changed();
        }
        self.ui.expanded_diagnostic = self.diagnostics.expanded(&self.state);
    }
    fn clear_lsp_ui_rows(&mut self) {
        self.ui.language_completion = None;
        self.close_documentation();
        self.ui.lsp_semantic_spans.clear();
        self.ui.lsp_highlight_spans.clear();
        self.ui.lsp_inlay_hints.clear();
        self.ui.lsp_folds.clear();
    }
    pub(in crate::editor::session) fn clear_lsp_ui_projection(&mut self) {
        self.language.clear_projection();
        self.clear_lsp_ui_rows();
    }
    pub(in crate::editor::session) fn refresh_lsp_projection_owner(&mut self) {
        let document = self.active_lsp_document();
        if self.language.refresh_projection(
            &self.state,
            document
                .as_ref()
                .map(|(buffer, document)| (*buffer, document)),
        ) {
            self.clear_lsp_ui_rows();
            if self.language.projection().is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }
    pub(in crate::editor::session) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.language.active().cloned() {
            self.clear_lsp_server_ownership(&server);
        }
        self.language.clear_active();
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.language
            .startup_inlays(&self.configuration.settings().lsp, Instant::now());
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        self.language
            .retry_inlays(&self.configuration.settings().lsp, Instant::now())
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        self.language.schedule_inlays(
            &self.configuration.settings().lsp,
            immediate,
            Instant::now(),
        );
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
    pub(in crate::editor::session) fn process_lsp_inlay_refresh(&mut self) {
        if self
            .language
            .inlay_ready(&self.configuration.settings().lsp, Instant::now())
        {
            self.request_lsp_inlay_hints();
        }
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}

impl InteractiveApplication {
    fn reprobe_failed_path_runtime(&mut self, server: &str) {
        let Some(definition) = self.language.sessions().get(server).and_then(|state| {
            matches!(&state.launch, LspLaunch::Direct { managed: false, .. })
                .then(|| state.definition.clone())
        }) else {
            return;
        };
        let Some(executable) = discover_executable(&definition.executable) else {
            return;
        };
        let key = Self::tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
        self.tooling.forget_probe(&key);
        self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, executable);
    }

    pub(in crate::editor::session) fn append_lsp_log(&mut self, line: String) {
        self.language
            .append_log(line, self.configuration.settings().lsp.maximum_log_lines);
        if self.search.picker_surface() == PickerSurfaceKind::Logs && self.ui.picker.is_some() {
            self.show_lsp_logs();
        }
    }

    pub(in crate::editor::session) fn lsp_server_label(&self, server_key: &str) -> String {
        self.language.server_label(server_key)
    }

    pub(in crate::editor::session) fn update_lsp_progress(
        &mut self,
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        self.language.update_progress(
            (server, token, kind, title, message, percentage),
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        self.project_lsp_progress();
    }
    pub(in crate::editor::session) fn clear_lsp_progress(&mut self, server: &str) {
        self.language.clear_progress(server);
        self.project_lsp_progress();
    }
    fn project_lsp_progress(&mut self) {
        let (rows, frame) = self.language.progress();
        if self.ui.lsp_progress != rows {
            self.ui.lsp_progress = rows.to_vec();
        }
        self.ui.lsp_progress_frame = frame;
    }
    pub(in crate::editor::session) fn update_lsp_progress_animation(&mut self) {
        self.language.animate_progress(
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        self.project_lsp_progress();
    }
    pub(in crate::editor::session) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        generation: u64,
        method: String,
        message: String,
    ) {
        if method == "textDocument/inlayHint" {
            if self.language.fail_inlay(&server, generation) {
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: inlay hint refresh failed: {message}"));
            }
            // Inlay hints are background, revision-owned projections. A failure
            // for any superseded generation is stale by definition, while a
            // current failure is retried above. Neither is user-actionable.
            return;
        }
        let server = self.lsp_server_label(&server);
        self.show_error(self.message(
            "lsp_request_failed",
            &[
                ("server", server),
                ("method", method),
                ("diagnostic", message),
            ],
        ));
    }

    pub(in crate::editor::session) fn process_lsp_events(&mut self) {
        for error in self.language.flush_edit_replies() {
            self.show_error(error);
        }
        while let Some(owned) = self.language.poll_event() {
            let crate::language::lsp::OwnedLspEvent {
                event,
                context,
                reply,
            } = owned;
            if !self.language.owns_event_context(&context) {
                self.language
                    .reject_reply(reply, "requesting language-server session retired".into());
                continue;
            }
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_startup_refresh();
                    }
                    // Successful lifecycle transitions are reflected by the
                    // status line and bounded progress surface. They must not
                    // leave persistent bottom-left notifications.
                }
                LspEvent::CapabilitiesChanged {
                    server,
                    capabilities,
                } => {
                    if self.language.running(&server, capabilities) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }

                LspEvent::InlayHintsRefresh { server } => {
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.schedule_lsp_inlay_refresh(true);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let result = self
                        .language
                        .diagnostic_batch(
                            &context,
                            &self.state,
                            (&uri, version, diagnostics),
                            (
                                false,
                                self.configuration
                                    .settings()
                                    .workspace
                                    .maximum_diagnostics_per_owner_uri,
                                &self.lsp_server_label(&server),
                            ),
                        )
                        .and_then(|batch| self.diagnostics.publish(batch, &self.state));
                    match result {
                        Ok(()) => self.sync_ui_diagnostics(),
                        Err(error) => self.append_lsp_log(error),
                    }
                }

                LspEvent::Result {
                    server,
                    generation,
                    method,
                    uri,
                    version,
                    result,
                } => {
                    let current = self.active_lsp_document();
                    if !current.as_ref().is_some_and(|(_, document)| {
                        document.uri() == uri && document.version == version
                    }) {
                        continue;
                    }
                    self.refresh_lsp_projection_owner();
                    if !self
                        .language
                        .accept_result(&server, generation, &method, &uri, version)
                    {
                        continue;
                    }
                    self.present_lsp_result_owned(
                        &server,
                        generation,
                        &method,
                        &uri,
                        result,
                        Some(context),
                    );
                }
                LspEvent::ApplyEditRequest {
                    server: _,
                    request_id: _,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        reply,
                        context.encoding(),
                    );
                }
                LspEvent::Progress {
                    server,
                    token,
                    kind,
                    title,
                    message,
                    percentage,
                } => self.update_lsp_progress(server, token, kind, title, message, percentage),
                LspEvent::Log { server, message } => {
                    let server = self.lsp_server_label(&server);
                    self.append_lsp_log(format!("{server}: {message}"));
                }
                LspEvent::Health { server, health } => {
                    let server = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message(
                            "lsp_health",
                            &[
                                ("server", server),
                                ("documents", health.attached_documents.to_string()),
                                ("pending", health.pending_requests.to_string()),
                                (
                                    "encoding",
                                    health.capabilities.position_encoding.lsp_name().into(),
                                ),
                            ],
                        ),
                        false,
                    );
                }
                LspEvent::Unsupported { server, method } => {
                    let server = self.lsp_server_label(&server);
                    self.show_error(
                        self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                    );
                }
                LspEvent::RequestFailed {
                    server,
                    generation,
                    method,
                    message,
                } => self.handle_lsp_request_failure(server, generation, method, message),
                LspEvent::Stopped { server, success } => {
                    if !success {
                        self.reprobe_failed_path_runtime(&server);
                    }
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.notify_message(
                        self.message("lsp_stopped", &[("server", server_label)]),
                        false,
                    );
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    self.reprobe_failed_path_runtime(&server);
                    let desired = self.language.stopped(&server);
                    for prefix in ["lsp.", "lsp-pull:"] {
                        if let Ok(producer) = context.diagnostic_producer(prefix) {
                            self.diagnostics.clear_source(&producer);
                        }
                    }
                    self.sync_ui_diagnostics();
                    let _ = self.language.stop(&server);
                    self.language.forget_synced(&server);
                    self.clear_lsp_progress(&server);
                    let server_label = self.lsp_server_label(&server);
                    self.language.failed(server_label.clone(), message.clone());
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server_label), ("diagnostic", message)],
                    ));
                    if self.language.active().map(String::as_str) == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
            }
        }
    }

    pub(in crate::editor::session) fn schedule_lsp_restart(&mut self, key: String) {
        if let Some(server) =
            self.language
                .schedule_restart(&key, &self.configuration.settings().lsp, Instant::now())
        {
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
        }
    }

    pub(in crate::editor::session) fn restart_lsp_if_due(&mut self) {
        if self.workspace.validation_pending() {
            return;
        }
        if let Some(key) = self.language.restart_due(Instant::now()) {
            self.restart_lsp_session(key);
        }
    }
}

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
                if let Some(path) = path_from_file_uri(uri) {
                    if let Some((buffer_id, buffer)) = self
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
                if let Some(path) = path_from_file_uri(uri) {
                    if let Some((buffer_id, buffer)) = self
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
            }
            LspFeatureResult::CodeLens(items) => {
                let rows = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
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
                if let Some(path) = path_from_file_uri(uri) {
                    if let Some((buffer_id, buffer)) = self
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
                                        .lsp_to_byte(
                                            item.start.line,
                                            item.start.character,
                                            encoding,
                                        )
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
                                                end_line: buffer
                                                    .text
                                                    .byte_to_line(range.end)
                                                    .ok()?,
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
            (roots, maximum),
        ) {
            self.abort_workspace_edit(error);
            return;
        }
        while let Some((index, file)) = self.workspace_edits.next_file() {
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
