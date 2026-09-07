use super::*;

impl InteractiveApplication {
    pub(super) fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        self.cancel_pending_mapping();
        if self.ui.prompt.is_none() {
            if let Some(view) = self.state.active_view {
                let mode = self.state.views[view].mode;
                self.previous_mode = Some((view, mode));
                self.set_mode(match kind {
                    PromptKind::Command => EditorMode::CommandLine,
                    PromptKind::SearchForward
                    | PromptKind::SearchBackward
                    | PromptKind::WorkspaceTrust
                    | PromptKind::GitReset
                    | PromptKind::ExplorerDelete => EditorMode::Prompt,
                });
            }
        }
        let mut prompt = PromptState::new(kind);
        prompt.cursor = initial.len();
        prompt.input = initial;
        self.ui.prompt = Some(prompt);
        self.history_index = None;
        self.refresh_completions();
    }

    pub(super) fn close_prompt(&mut self) {
        if self
            .ui
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.kind == PromptKind::ExplorerDelete)
        {
            self.explorer_delete_target = None;
        }
        self.ui.prompt = None;
        self.history_index = None;
        if let Some((view, mode)) = self.previous_mode.take() {
            if self.state.views.contains_key(view) {
                self.set_specific_mode(view, mode);
            }
        }
    }

    pub(super) fn prompt_insert(&mut self, text: &str) {
        if let Some(prompt) = &mut self.ui.prompt {
            if prompt.input.is_char_boundary(prompt.cursor) {
                prompt.input.insert_str(prompt.cursor, text);
                prompt.cursor += text.len();
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
        self.refresh_completions();
    }

    pub(super) fn prompt_backspace(&mut self) {
        if let Some(prompt) = &mut self.ui.prompt {
            let before = &prompt.input[..prompt.cursor];
            let previous = before
                .grapheme_indices(true)
                .next_back()
                .map(|(index, _)| index);
            if let Some(previous) = previous {
                prompt.input.replace_range(previous..prompt.cursor, "");
                prompt.cursor = previous;
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
        self.refresh_completions();
    }

    pub(super) fn prompt_move(&mut self, movement: PromptMove) {
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.cursor = match movement {
                PromptMove::Home => 0,
                PromptMove::End => prompt.input.len(),
                PromptMove::Left => prompt.input[..prompt.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(index, _)| index),
                PromptMove::Right => prompt.input[prompt.cursor..]
                    .grapheme_indices(true)
                    .nth(1)
                    .map_or(prompt.input.len(), |(index, _)| prompt.cursor + index),
            };
        }
    }

    pub(super) fn prompt_history(&mut self, direction: i8) {
        let Some(kind) = self.ui.prompt.as_ref().map(|prompt| prompt.kind) else {
            return;
        };
        if matches!(
            kind,
            PromptKind::WorkspaceTrust | PromptKind::GitReset | PromptKind::ExplorerDelete
        ) {
            return;
        }
        let (enabled, history) = if kind == PromptKind::Command {
            (
                self.configuration.settings.history.command.enabled,
                &self.command_history,
            )
        } else {
            (
                self.configuration.settings.history.search.enabled,
                &self.search_history,
            )
        };
        if !enabled {
            return;
        }
        if history.is_empty() {
            return;
        }
        let current = self.history_index.unwrap_or(history.len());
        let next = if direction < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(history.len())
        };
        self.history_index = (next < history.len()).then_some(next);
        let selected = self
            .history_index
            .and_then(|index| history.get(index).cloned())
            .unwrap_or_default();
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.input = selected;
            prompt.cursor = prompt.input.len();
            prompt.error = None;
            prompt.selected_completion = None;
        }
        self.refresh_completions();
    }

    pub(super) fn prompt_complete(&mut self, direction: i8) {
        let Some(prompt) = &mut self.ui.prompt else {
            return;
        };
        if prompt.completions.is_empty() {
            return;
        }
        let length = prompt.completions.len();
        let wrap = self.configuration.settings.input.completion_wrap;
        prompt.selected_completion = Some(match (prompt.selected_completion, direction < 0) {
            (None, false) => 0,
            (None, true) => length - 1,
            (Some(current), false) if wrap => (current + 1) % length,
            (Some(current), true) if wrap => current.checked_sub(1).unwrap_or(length - 1),
            (Some(current), false) => (current + 1).min(length - 1),
            (Some(current), true) => current.saturating_sub(1),
        });
    }

    pub(super) fn accept_completion(&mut self) -> bool {
        let Some(prompt) = &mut self.ui.prompt else {
            return false;
        };
        let Some(index) = prompt.selected_completion else {
            return false;
        };
        let Some(item) = prompt.completions.get(index) else {
            return false;
        };
        let label = item.label.clone();
        if let Some((command, _)) = prompt.input.split_once(char::is_whitespace) {
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| matches!(id.as_str(), "file.open" | "workspace.root.add"))
            {
                prompt.input = format!("{command} {label}");
            } else {
                prompt.input = label;
            }
        } else {
            prompt.input = label;
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(prompt.input.as_str())
                .is_some_and(|id| {
                    matches!(
                        id.as_str(),
                        "file.open" | "file.save" | "workspace.root.add"
                    )
                })
            {
                prompt.input.push(' ');
            }
        }
        prompt.cursor = prompt.input.len();
        prompt.selected_completion = None;
        prompt.error = None;
        self.refresh_completions();
        true
    }

    pub(super) fn refresh_completions(&mut self) {
        let Some(prompt) = &self.ui.prompt else {
            return;
        };
        if prompt.kind != PromptKind::Command {
            if let Some(prompt) = &mut self.ui.prompt {
                prompt.completions.clear();
                prompt.selected_completion = None;
            }
            return;
        }
        let input = prompt.input.clone();
        let mut candidates = BTreeMap::<String, String>::new();
        if let Some((command, fragment)) = input.split_once(char::is_whitespace) {
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| {
                    matches!(
                        id.as_str(),
                        "file.open" | "file.save" | "workspace.root.add"
                    )
                })
            {
                let fragment = fragment.trim();
                let query = fragment.to_lowercase();
                for (_, buffer) in &self.state.buffers {
                    if let Some(path) = &buffer.path {
                        let label = path.display().to_string();
                        if label.to_lowercase().contains(&query) {
                            let detail = &self.configuration.settings.command_labels["file.open"];
                            candidates.insert(label, detail.clone());
                        }
                    }
                }
                self.completion_generation = self.completion_generation.saturating_add(1);
                let generation = self.completion_generation;
                let entered = PathBuf::from(fragment);
                let parent = entered.parent().filter(|path| !path.as_os_str().is_empty());
                let directory = parent.map_or_else(
                    || self.state.workspace_root.clone(),
                    |parent| {
                        if parent.is_absolute() {
                            parent.to_path_buf()
                        } else {
                            self.state.workspace_root.join(parent)
                        }
                    },
                );
                if let Ok(id) = self.vfs.list_directory(
                    directory,
                    self.configuration.settings.runtime.completion_limit,
                ) {
                    self.pending_vfs.insert(
                        id,
                        PendingVfs::Completion {
                            generation,
                            prompt_input: input.clone(),
                            fragment: fragment.into(),
                        },
                    );
                }
            }
        } else {
            let query = input.to_lowercase();
            for (alias, command_id) in &self.configuration.settings.input.command_aliases {
                let detail = self
                    .configuration
                    .settings
                    .command_labels
                    .get(command_id)
                    .expect("resolved configuration validates command labels");
                if alias.starts_with(&query) || detail.to_lowercase().contains(&query) {
                    candidates.insert(alias.clone(), detail.into());
                }
            }
            for spec in self.commands.specs() {
                let id = spec.id.as_str();
                let label = &self.configuration.settings.command_labels[id];
                if id.starts_with(&query) || label.to_lowercase().contains(&query) {
                    candidates.insert(id.into(), label.clone());
                }
            }
        }
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.completions = candidates
                .into_iter()
                .take(self.configuration.settings.runtime.completion_limit)
                .map(|(label, detail)| CompletionItem { label, detail })
                .collect();
            if prompt
                .selected_completion
                .is_some_and(|index| index >= prompt.completions.len())
            {
                prompt.selected_completion = None;
            }
            if prompt.selected_completion.is_none()
                && self.configuration.settings.input.completion_select_first
                && !prompt.completions.is_empty()
            {
                prompt.selected_completion = Some(0);
            }
        }
    }

    pub(super) fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        if input.is_empty() {
            return;
        }
        if kind == PromptKind::WorkspaceTrust {
            return;
        }
        let configured = if kind == PromptKind::Command {
            &self.configuration.settings.history.command
        } else {
            &self.configuration.settings.history.search
        };
        if !configured.enabled || configured.maximum_entries == 0 {
            return;
        }
        let history = if kind == PromptKind::Command {
            &mut self.command_history
        } else {
            &mut self.search_history
        };
        if history.last().is_some_and(|entry| entry == input) {
            return;
        }
        history.push(input.into());
        let excess = history.len().saturating_sub(configured.maximum_entries);
        if excess > 0 {
            history.drain(..excess);
        }
    }

    pub(super) fn submit_prompt(&mut self) {
        if self
            .configuration
            .settings
            .input
            .completion_accept_on_submit
            && self.accept_completion()
        {
            return;
        }
        let Some(prompt) = self.ui.prompt.clone() else {
            return;
        };
        if !matches!(
            prompt.kind,
            PromptKind::WorkspaceTrust | PromptKind::GitReset | PromptKind::ExplorerDelete
        ) {
            self.record_prompt_history(prompt.kind, &prompt.input);
        }
        match prompt.kind {
            PromptKind::Command => match parse_ex(
                &prompt.input,
                &self.configuration.settings.input.command_aliases,
            ) {
                Ok(invocation) => {
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(error.to_string());
                    }
                }
            },
            PromptKind::WorkspaceTrust => match CommandInvocation::new(
                "workspace.trust.confirm",
                InvocationSource::CommandLine,
            ) {
                Ok(invocation) => {
                    let invocation =
                        invocation.with_argument("phrase", CommandValue::String(prompt.input));
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(error.to_string());
                    }
                }
            },
            PromptKind::GitReset => {
                self.close_prompt();
                self.confirm_reset_selected_git_hunk(prompt.input);
            }
            PromptKind::ExplorerDelete => {
                self.confirm_explorer_delete(prompt.input);
                self.close_prompt();
            }
            PromptKind::SearchForward | PromptKind::SearchBackward => {
                if prompt.input.is_empty() {
                    let message = self.message("search_empty", &[]);
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(message);
                    }
                    return;
                }
                if self.state.active_view.is_none() {
                    let message = self.message("search_no_buffer", &[]);
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(message);
                    }
                    return;
                }
                if self.configuration.settings.search.regex {
                    let source = self
                        .state
                        .active_buffer_id()
                        .and_then(|buffer| self.state.buffers.get(buffer))
                        .map(|buffer| buffer.text.to_string())
                        .unwrap_or_default();
                    if let Err(error) = crate::editor::input::search_matches(
                        &source,
                        &prompt.input,
                        self.configuration.settings.search.smart_case,
                        true,
                    ) {
                        if let Some(active) = &mut self.ui.prompt {
                            active.error = Some(error.to_string());
                        }
                        return;
                    }
                }
                let forward = prompt.kind == PromptKind::SearchForward;
                self.ui.search_query = Some(prompt.input);
                self.last_search_forward = forward;
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
            self.configuration.settings.search.smart_case,
            self.configuration.settings.search.regex,
        ) {
            Ok(matches) => matches,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let wrap = self.configuration.settings.search.wrap;
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
