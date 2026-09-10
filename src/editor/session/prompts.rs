use super::*;

impl InteractiveApplication {
    pub(super) fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        if kind != PromptKind::ExplorerRename {
            self.cancel_explorer_rename_prompt();
        }
        if kind != PromptKind::ExplorerDelete {
            self.cancel_explorer_confirmation();
        }
        self.reconcile_editor_windows();
        self.input.hold_focus(self.window_session.capture_focus());
        let active = self.state.active_view.map(|view| {
            (
                self.state.window_version(),
                view,
                self.state.views[view].mode,
            )
        });
        if let Some(mode) = self.input.open_prompt(kind, initial, active) {
            self.set_mode(mode);
        }
        self.project_input_hints();
        self.refresh_completions();
    }
    pub(super) fn open_confirmation(
        &mut self,
        kind: PromptKind,
        title: String,
        message: String,
        selected: crate::ui::ConfirmationChoice,
    ) {
        self.open_prompt(kind, String::new());
        let binding = |command: &str| {
            self.configuration
                .keymaps()
                .binding_for("confirmation_prompt", command)
        };
        let selections = [
            "command_line.move_left",
            "command_line.move_right",
            "surface.confirmation.toggle",
            "surface.confirmation.select_yes",
            "surface.confirmation.select_no",
        ]
        .into_iter()
        .filter_map(binding)
        .collect::<Vec<_>>()
        .join("/");
        let mut hints = Vec::new();
        if !selections.is_empty() {
            hints.push(format!("{selections} select"));
        }
        if let Some(key) = binding("command_line.submit") {
            hints.push(format!("{key} confirm"));
        }
        if let Some(key) = binding("command_line.cancel") {
            hints.push(format!("{key} cancel"));
        }
        let hint = hints.join(" · ");
        self.input
            .configure_confirmation(title, message, selected, hint);
        self.project_input_prompt();
        self.reconcile_editor_windows();
    }
    pub(super) fn close_prompt(&mut self) {
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.extension
                .answer_confirmation(crate::extensions::PluginConfirmationAnswer::Cancelled);
        }
        self.cancel_explorer_rename_prompt();
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust)
        {
            self.workspace.cancel_confirmation();
        }
        self.cancel_explorer_confirmation();
        if let Some((view, mode)) = self.input.close_prompt(&self.state.window_version()) {
            if self.state.views.contains_key(view) {
                self.set_specific_mode(view, mode);
            }
        }
        self.restore_input_focus();
        self.project_input_prompt();
    }
    pub(super) fn present_next_plugin_confirmation(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        let Some(request) = self.extension.next_confirmation(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) else {
            let _ = self.extension.dispatch_confirmation_results(
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
            return;
        };
        let selected = match request.default_answer {
            crate::extensions::PluginConfirmationAnswer::Yes => crate::ui::ConfirmationChoice::Yes,
            crate::extensions::PluginConfirmationAnswer::No
            | crate::extensions::PluginConfirmationAnswer::Cancelled => {
                crate::ui::ConfirmationChoice::No
            }
        };
        self.open_confirmation(
            PromptKind::PluginConfirmation,
            format!("Plugin {} · {}", request.plugin, request.title),
            request.message,
            selected,
        );
    }
    pub(super) fn submit_plugin_confirmation(&mut self, selected: crate::ui::ConfirmationChoice) {
        let answer = match selected {
            crate::ui::ConfirmationChoice::Yes => crate::extensions::PluginConfirmationAnswer::Yes,
            crate::ui::ConfirmationChoice::No => crate::extensions::PluginConfirmationAnswer::No,
        };
        self.extension.answer_confirmation(answer);
        self.close_prompt();
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
    }
    pub(super) fn prompt_insert(&mut self, text: &str) {
        self.input.prompt_insert(text);
        self.refresh_completions();
    }
    pub(super) fn prompt_backspace(&mut self) {
        self.input.prompt_backspace();
        self.refresh_completions();
    }
    pub(super) fn prompt_move(&mut self, movement: PromptMove) {
        self.input.prompt_move(movement);
        self.project_input_prompt();
    }
    pub(super) fn prompt_history(&mut self, direction: i8) {
        self.input
            .prompt_history(direction, self.configuration.settings());
        self.refresh_completions();
    }
    pub(super) fn prompt_complete(&mut self, direction: i8) {
        self.input
            .prompt_complete(direction, self.configuration.settings());
        self.project_input_prompt();
    }
    pub(super) fn accept_completion(&mut self) -> bool {
        let accepted = self.input.accept_completion(self.configuration.settings());
        if accepted {
            self.refresh_completions();
        }
        accepted
    }
    pub(super) fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        self.input
            .record_prompt_history(kind, input, self.configuration.settings());
    }
    pub(super) fn refresh_completions(&mut self) {
        if let Some(request) = self.input.refresh_completions(
            self.configuration.settings(),
            &self.state.workspace_root,
            self.state
                .buffers
                .values()
                .filter_map(|buffer| buffer.path.as_deref()),
            self.commands.specs().map(|spec| spec.id.as_str()),
        ) {
            if let Ok(id) = self
                .documents
                .list_directory(request.directory, request.limit)
            {
                self.input.register_completion_read(id, request.key);
            }
        }
        self.project_input_prompt();
    }

    pub(super) fn submit_prompt(&mut self) {
        if self
            .configuration
            .settings()
            .input
            .completion_accept_on_submit
            && self.accept_completion()
        {
            return;
        }
        let Some(prompt) = self.input.prompt().cloned() else {
            return;
        };
        if prompt
            .confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.selected == crate::ui::ConfirmationChoice::Yes)
        {
            let (width, height) = crate::ui::prompt_surface_dimensions(
                &self.state,
                &self.ui,
                self.window_session.width(),
                self.window_session.height(),
            );
            if !crate::ui::confirmation_fully_visible(
                width,
                height,
                self.configuration.settings(),
                &prompt,
            ) {
                return;
            }
        }
        if !matches!(
            prompt.kind,
            PromptKind::WorkspaceTrust
                | PromptKind::GitReset
                | PromptKind::ExplorerDelete
                | PromptKind::ExplorerRename
                | PromptKind::PluginConfirmation
        ) {
            self.record_prompt_history(prompt.kind, &prompt.input);
        }
        match prompt.kind {
            PromptKind::Command => match parse_ex(
                &prompt.input,
                &self.configuration.settings().input.command_aliases,
            ) {
                Ok(invocation) => {
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    self.prompt_error(error.to_string());
                }
            },
            PromptKind::WorkspaceTrust => match CommandInvocation::new(
                "workspace.trust.confirm",
                InvocationSource::CommandLine,
            ) {
                Ok(invocation) => {
                    let invocation =
                        invocation.with_argument("phrase", CommandValue::String(prompt.input));
                    self.invoke(invocation);
                    self.close_prompt();
                }
                Err(error) => {
                    self.prompt_error(error.to_string());
                }
            },
            PromptKind::GitReset => {
                self.close_prompt();
                self.confirm_reset_selected_git_hunk(prompt.input);
            }
            PromptKind::ExplorerDelete => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                if selected == crate::ui::ConfirmationChoice::Yes {
                    self.confirm_explorer_delete();
                }
                self.close_prompt();
            }
            PromptKind::PluginConfirmation => {
                // The extension owner asynchronously consumes this one-shot result.
                self.submit_plugin_confirmation(
                    prompt
                        .confirmation
                        .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
                );
            }
            PromptKind::ExplorerRename => self.submit_explorer_rename_prompt(&prompt.input),
            PromptKind::SearchForward | PromptKind::SearchBackward => {
                if prompt.input.is_empty() {
                    let message = self.message("search_empty", &[]);
                    self.prompt_error(message);
                    return;
                }
                if self.state.active_view.is_none() {
                    let message = self.message("search_no_buffer", &[]);
                    self.prompt_error(message);
                    return;
                }
                if self.configuration.settings().search.regex {
                    let source = self
                        .state
                        .active_buffer_id()
                        .and_then(|buffer| self.state.buffers.get(buffer))
                        .map(|buffer| buffer.text.to_string())
                        .unwrap_or_default();
                    if let Err(error) = crate::editor::input::search_matches(
                        &source,
                        &prompt.input,
                        self.configuration.settings().search.smart_case,
                        true,
                    ) {
                        self.prompt_error(error.to_string());
                        return;
                    }
                }
                let forward = prompt.kind == PromptKind::SearchForward;
                self.ui.search_query = Some(prompt.input);
                self.input.remember_search(forward);
                self.close_prompt();
                self.perform_search(forward);
            }
        }
    }

    pub(super) fn perform_search(&mut self, forward: bool) {
        let Some(query) = self.ui.search_query.clone() else {
            self.show_error(self.message("no_previous_search", &[]));
            return;
        };
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor_offset = view.cursor;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let source = buffer.text.to_string();
        let matches = match crate::editor::input::search_matches(
            &source,
            &query,
            self.configuration.settings().search.smart_case,
            self.configuration.settings().search.regex,
        ) {
            Ok(matches) => matches,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let wrap = self.configuration.settings().search.wrap;
        let cursor = cursor_offset.get().min(source.len());
        let target = if forward {
            let start = if cursor < source.len() {
                crate::editor::text::next_grapheme_boundary(&buffer.text, cursor_offset).get()
            } else {
                cursor
            };
            matches
                .iter()
                .find(|matched| matched.start >= start)
                .or_else(|| wrap.then(|| matches.first()).flatten())
                .map(|matched| matched.start)
        } else {
            matches
                .iter()
                .rev()
                .find(|matched| matched.start < cursor)
                .or_else(|| wrap.then(|| matches.last()).flatten())
                .map(|matched| matched.start)
        };
        let Some(target) = target else {
            self.show_error(self.message("pattern_not_found", &[("query", query)]));
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(target),
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}
