//! Input prompts, confirmations and command completion.

use std::collections::BTreeMap;

/// `:` completions, Vim style: one row per command, named by its shortest alias that fits
/// what is typed, so `w`, `q` and `e` come first. Labels match too. Commands without an alias
/// follow under their dotted id; plumbing only keys use appears once the input has a `.`.
pub(in crate::app) fn command_completions<'a>(
    policy: &crate::config::Config,
    input: &str,
    commands: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let query = input.to_lowercase();
    // Rank: typed prefix first, then shorter, then lowercase, then alphabetical.
    let rank = |alias: &str, prefix: bool| {
        (
            !prefix,
            alias.len(),
            alias.chars().any(char::is_uppercase),
            alias.to_owned(),
        )
    };
    let mut best = BTreeMap::<String, (bool, usize, bool, String)>::new();
    for (alias, command_id) in &policy.input.command_aliases {
        let label = policy.command_labels.get(command_id).unwrap_or(command_id);
        let prefix = alias.to_lowercase().starts_with(&query);
        if !prefix && !label.to_lowercase().contains(&query) {
            continue;
        }
        let candidate = rank(alias, prefix);
        best.entry(command_id.clone())
            .and_modify(|current| {
                if candidate < *current {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let hidden = &policy.command_palette.hidden;
    for id in commands {
        let label = policy.command_labels.get(id).map_or(id, String::as_str);
        let plumbing = hidden.iter().any(|prefix| id.starts_with(prefix.as_str()));
        if best.contains_key(id)
            || (plumbing && !query.contains('.'))
            || !(id.contains(&query) || label.to_lowercase().contains(&query))
        {
            continue;
        }
        best.insert(id.to_owned(), rank(id, id.starts_with(&query)));
    }
    let mut rows = best
        .into_iter()
        .map(|(command_id, rank)| {
            let label = policy
                .command_labels
                .get(&command_id)
                .cloned()
                .unwrap_or(command_id);
            (rank, label)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows.into_iter()
        .map(|(rank, label)| (rank.3, label))
        .collect()
}

/// Commands whose argument is a path, which the command line completes from files.
pub(in crate::app) fn takes_path(command: &str) -> bool {
    matches!(command, "file.open" | "file.save" | "workspace.root.add")
}

mod commands;
pub(in crate::app) mod replacement;

use crate::app::App;
use crate::app::commands::PromptMove;
use crate::editor::EditorMode;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::UndoPolicy;
use crate::editor::input::parse_ex;
use crate::editor::text::ByteOffset;
use crate::ui::PromptKind;

impl App {
    pub(in crate::app) fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind != kind)
        {
            self.close_prompt();
        }
        if kind != PromptKind::ExplorerRename {
            self.cancel_explorer_rename_prompt();
        }
        if kind != PromptKind::ExplorerDelete {
            self.cancel_explorer_confirmation();
        }
        if kind != PromptKind::TerminalClose {
            self.terminal_session.cancel_close();
        }
        if kind != PromptKind::DirtyClose {
            self.window_session.cancel_editor_close();
        }
        self.reconcile_editor_windows();
        self.input.hold_focus(self.window_session.capture_focus());
        let active = self.state.active_view().map(|view| {
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
        if kind != PromptKind::Text {
            self.request_input_replacement();
        }
    }
}

impl App {
    pub(in crate::app) fn open_confirmation(
        &mut self,
        kind: PromptKind,
        title: String,
        message: String,
        selected: crate::ui::ConfirmationChoice,
    ) {
        self.open_confirmation_actions(kind, title, message, selected, "Yes", "No");
    }
}

impl App {
    pub(in crate::app) fn open_confirmation_actions(
        &mut self,
        kind: PromptKind,
        title: String,
        message: String,
        selected: crate::ui::ConfirmationChoice,
        positive_label: &str,
        negative_label: &str,
    ) {
        self.open_prompt(kind, String::new());
        self.cancel_input_replacement();
        let binding = |command: &str| {
            self.configuration
                .keymaps()
                .binding_for("confirmation_prompt", command)
        };
        // Arrows and tab also move the selection; the footer names only y/n so it fits.
        let selections = [
            "surface.confirmation.select_yes",
            "surface.confirmation.select_no",
        ]
        .into_iter()
        .filter_map(binding)
        .collect::<Vec<_>>()
        .join("/");
        let mut hints = Vec::new();
        if !selections.is_empty() {
            hints.push(format!("{selections} answer now"));
        }
        if let Some(key) = binding("command_line.submit") {
            hints.push(format!("{key} selected"));
        }
        if let Some(key) = binding("command_line.cancel") {
            hints.push(format!("{key} cancel"));
        }
        // Help is a focusable action of the confirmation itself, not a global key: it is
        // advertised here through the same keymap lookup as every other action.
        if let Some(key) = binding("overlay.help.form") {
            hints.push(format!("{key} help"));
        }
        let hint = hints.join(" · ");
        self.input.configure_confirmation_actions(
            title,
            message,
            selected,
            positive_label,
            negative_label,
            hint,
        );
        self.project_input_prompt();
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn close_prompt(&mut self) {
        self.close_prompt_with_editor_close(true);
    }
}

impl App {
    pub(in crate::app) fn close_prompt_with_editor_close(&mut self, cancel_editor_close: bool) {
        self.cancel_input_replacement();
        self.finish_text_input(None);
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::GitDestructive)
        {
            self.git.cancel_destructive();
        }
        if let Some((view, cursor, line, wrap_row, column)) = self.input.take_search_origin() {
            self.move_search_cursor(view, cursor);
            if let Some(view) = self.state.views.get_mut(view) {
                view.scroll_line = line;
                view.scroll_wrap_row = wrap_row;
                view.scroll_column = column;
            }
        }
        self.input.take_pending_breakpoint();
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::GitDestructive)
        {
            if self.git.has_pending_replacement() {
                self.git.take_pending_replacement();
            }
            self.git.take_pending_destructive();
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.extension
                .answer_confirmation(crate::plugins::PluginConfirmationAnswer::Cancelled);
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::AgentPermission)
        {
            self.cancel_agent_permission();
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::GitStageUnsaved)
        {
            self.input.clear_pending_stage();
        }
        self.cancel_explorer_rename_prompt();
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::TerminalClose)
        {
            self.terminal_session.cancel_close();
        }
        if cancel_editor_close
            && self
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == PromptKind::DirtyClose)
        {
            self.window_session.cancel_editor_close();
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust)
        {
            self.workspace.cancel_confirmation();
        }
        self.cancel_explorer_confirmation();
        let substituting = self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::SubstituteConfirm);
        if let Some((view, mode)) = self.input.close_prompt(&self.state.window_version())
            && self.state.views.contains_key(view)
        {
            self.set_specific_mode(view, mode);
        }
        self.restore_input_focus();
        self.project_input_prompt();
        // Closing the question any other way than answering it stops, keeping what was replaced.
        if substituting {
            self.finish_substitution();
        }
    }
}

impl App {
    pub(in crate::app) fn prompt_insert(&mut self, text: &str) {
        self.input.prompt_insert(text);
        self.refresh_completions();
        self.preview_search();
    }
}

impl App {
    pub(in crate::app) fn prompt_backspace(&mut self) {
        self.prompt_delete(|input| input.prompt_backspace());
    }
}

impl App {
    pub(in crate::app) fn prompt_delete(
        &mut self,
        delete: impl FnOnce(&mut crate::app::input::InputState),
    ) {
        delete(&mut self.input);
        self.refresh_completions();
        self.preview_search();
    }
}

impl App {
    pub(in crate::app) fn prompt_move(&mut self, movement: PromptMove) {
        self.input.prompt_move(movement);
        self.project_input_prompt();
    }
}

impl App {
    pub(in crate::app) fn prompt_history(&mut self, direction: i8) {
        self.input.prompt_history(direction);
        self.refresh_completions();
        self.preview_search();
    }
}

impl App {
    pub(in crate::app) fn prompt_complete(&mut self, direction: i8) {
        self.input.prompt_complete(direction);
        self.project_input_prompt();
    }
}

impl App {
    pub(in crate::app) fn accept_completion(&mut self) -> bool {
        let accepted = self.input.accept_completion(self.configuration.settings());
        if accepted {
            self.refresh_completions();
        }
        accepted
    }
}

impl App {
    pub(in crate::app) fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        self.input.record_prompt_history(kind, input);
    }
}

impl App {
    pub(in crate::app) fn refresh_completions(&mut self) {
        if let Some(request) = self.input.refresh_completions(
            self.configuration.settings(),
            &self.state.workspace_root,
            self.state
                .buffers
                .values()
                .filter_map(|buffer| buffer.path.as_deref()),
            self.commands.specs().map(|spec| spec.id.as_str()),
        ) && !self.input.completion_read_pending()
            && let Ok(id) = self.documents.list_directory(
                request.directory,
                request.key.prefix.clone(),
                request.limit,
            )
        {
            self.input.register_completion_read(id, request.key);
        }
        self.project_input_prompt();
    }
}

impl App {
    pub(in crate::app) fn submit_prompt(&mut self) {
        if self.accept_completion() {
            // A chosen command runs at once, as Enter does in Vim's wildmenu; a chosen path
            // waits, so a directory can be completed further.
            let command = self.input.prompt().is_some_and(|prompt| {
                prompt.kind == PromptKind::Command && !prompt.input.contains(char::is_whitespace)
            });
            if !command {
                return;
            }
        }
        let Some(prompt) = self.input.prompt().cloned() else {
            return;
        };
        if prompt.confirmation.as_ref().is_some_and(|confirmation| {
            prompt.kind == PromptKind::DirtyClose
                || confirmation.selected == crate::ui::ConfirmationChoice::Yes
        }) {
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
            PromptKind::Text
                | PromptKind::WorkspaceTrust
                | PromptKind::GitReset
                | PromptKind::ExplorerDelete
                | PromptKind::ExplorerRename
                | PromptKind::PluginConfirmation
                | PromptKind::TerminalClose
                | PromptKind::DirtyClose
                | PromptKind::BreakpointCondition
                | PromptKind::ReplaceFind
                | PromptKind::ReplaceWith
                | PromptKind::SubstituteConfirm
        ) {
            self.record_prompt_history(prompt.kind, &prompt.input);
        }
        match prompt.kind {
            PromptKind::Text => {
                self.finish_text_input(Some(prompt.input));
                self.close_prompt();
            }
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
            PromptKind::WorkspaceTrust => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                if selected == crate::ui::ConfirmationChoice::Yes {
                    // Confirm before closing: closing cancels a still-pending confirmation.
                    self.confirm_workspace_trust();
                    self.close_prompt();
                } else {
                    self.close_prompt();
                    self.notify_message(self.message("workspace_trust_declined", &[]), false);
                }
            }
            PromptKind::GitReset => {
                self.close_prompt();
                self.confirm_reset_selected_git_hunk(prompt.input);
            }
            PromptKind::GitDestructive => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                let replacement = self.git.take_pending_replacement();
                let action = self.git.take_pending_destructive();
                self.close_prompt();
                // Rebuild the replacement against current repository state before applying it.
                if let Some(replacement) = replacement {
                    self.apply_git_replacement(replacement, selected);
                    return;
                }
                if selected == crate::ui::ConfirmationChoice::Yes {
                    self.confirm_git_destructive(action);
                } else if action.is_some() {
                    self.notify_message("Nothing was removed".into(), false);
                }
            }
            PromptKind::GitStageUnsaved => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                let effect = self.input.take_pending_stage();
                self.close_prompt();
                if selected == crate::ui::ConfirmationChoice::Yes {
                    if let Some((effect, line)) = effect {
                        self.input.set_pending_stage(effect);
                        self.input.set_pending_stage_line(line);
                        self.confirm_stage_unsaved();
                    }
                } else {
                    self.input.clear_pending_stage();
                    self.notify_message("Nothing was staged".into(), false);
                }
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
            PromptKind::TerminalClose => self.submit_terminal_close(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::DirtyClose => self.submit_dirty_close(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::ExplorerRename => self.submit_explorer_rename_prompt(&prompt.input),
            PromptKind::SubstituteConfirm => self.answer_substitution(
                prompt
                    .confirmation
                    .is_some_and(|state| state.selected == crate::ui::ConfirmationChoice::Yes),
                false,
            ),
            PromptKind::ReviewComment => {
                self.close_prompt();
                self.submit_review_comment(prompt.input);
            }
            PromptKind::ReplaceFind => {
                self.close_prompt();
                if !prompt.input.is_empty() {
                    self.replace_query = Some(prompt.input);
                    self.open_prompt(PromptKind::ReplaceWith, String::new());
                }
            }
            PromptKind::ReplaceWith => {
                self.close_prompt();
                if let Some(query) = self.replace_query.take() {
                    self.start_workspace_search(query, Some(prompt.input));
                }
            }
            PromptKind::BreakpointCondition => {
                let row = self.input.take_pending_breakpoint();
                self.close_prompt();
                if let Some(mut row) = row {
                    let condition = prompt.input.trim();
                    row.condition = (!condition.is_empty()).then(|| condition.to_owned());
                    if let Err(error) = self.debugger_session.breakpoint(row, false) {
                        self.show_error(error);
                    }
                }
            }
            PromptKind::AgentPermission => self.answer_agent_permission(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::SearchForward | PromptKind::SearchBackward => {
                if prompt.input.is_empty() {
                    let message = self.message("search_empty", &[]);
                    self.prompt_error(message);
                    return;
                }
                if self.state.active_view().is_none() {
                    let message = self.message("search_no_buffer", &[]);
                    self.prompt_error(message);
                    return;
                }
                let forward = prompt.kind == PromptKind::SearchForward;
                self.ui.search_query = Some(prompt.input);
                self.ui.search_whole_word = false;
                self.input.remember_search(forward);
                self.close_prompt();
                self.perform_search(forward, 1);
            }
        }
    }
}

impl App {
    /// Selects `start..end` in character Visual mode, with the cursor on its last character.
    pub(in crate::app) fn select_text(
        &mut self,
        view_id: crate::editor::ViewId,
        start: usize,
        end: usize,
    ) -> bool {
        let Some(text) = self
            .state
            .views
            .get(view_id)
            .and_then(|view| self.state.buffers.get(view.buffer))
            .map(|buffer| &buffer.text)
        else {
            return false;
        };
        let empty = start == end;
        let head = if empty {
            ByteOffset(start)
        } else {
            crate::editor::text::previous_grapheme_boundary(text, ByteOffset(end))
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetMode {
                view: view_id,
                mode: if empty {
                    EditorMode::Normal
                } else {
                    EditorMode::VisualChar
                },
            })
            .with_operation(EditorOperation::SetSelection {
                view: view_id,
                selection: (!empty).then_some(crate::editor::Selection {
                    anchor: ByteOffset(start),
                    head,
                    kind: crate::editor::SelectionKind::Character,
                }),
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: head,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return false;
        }
        self.ensure_cursor_visible();
        true
    }
}

impl App {
    pub(in crate::app) fn project_input_prompt(&mut self) {
        let prompt = if self.input_replacement_pending() || self.search.help_is_open() {
            None
        } else {
            self.input.prompt().cloned().map(|mut prompt| {
                self.mask_text_input(&mut prompt);
                prompt
            })
        };
        self.ui.set(prompt, |ui| &mut ui.prompt);
        self.preview_substitute();
    }
}

impl App {
    pub(in crate::app) fn prompt_help(&mut self, help: String) {
        self.input.prompt_help(help);
        self.project_input_prompt();
    }
}

impl App {
    pub(in crate::app) fn prompt_error(&mut self, error: String) {
        self.input.prompt_error(error);
        self.project_input_prompt();
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
