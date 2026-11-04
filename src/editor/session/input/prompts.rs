use super::InputSession;
use crate::editor::session::input::COMPLETION_LIMIT;
use crate::{
    editor::{EditorMode, PromptMove, ViewId},
    ui::{CompletionItem, ConfirmationChoice, ConfirmationState, PromptKind, PromptState},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PromptCompletionKey {
    generation: u64,
    prompt_input: String,
    fragment: String,
}

pub(in crate::editor::session) struct PromptCompletionRequest {
    pub(in crate::editor::session) key: PromptCompletionKey,
    pub(in crate::editor::session) directory: PathBuf,
    pub(in crate::editor::session) limit: usize,
}

impl InputSession {
    pub(in crate::editor::session) fn prompt(&self) -> Option<&PromptState> {
        self.prompt.as_ref()
    }
    pub(in crate::editor::session) fn open_prompt(
        &mut self,
        kind: PromptKind,
        initial: String,
        active: Option<(crate::editor::EditorWindowVersion, ViewId, EditorMode)>,
    ) -> Option<EditorMode> {
        self.capture_changed();
        self.cancel_pending();
        self.cancel_mapping();
        self.cancel_prompt_completions();
        let mode = if self.prompt.is_none() {
            self.previous_mode = active;
            self.previous_mode.as_ref().map(|_| {
                if kind == PromptKind::Command {
                    EditorMode::CommandLine
                } else {
                    EditorMode::Prompt
                }
            })
        } else {
            None
        };
        let mut prompt = PromptState::new(kind);
        prompt.cursor = initial.len();
        prompt.input = initial;
        self.prompt = Some(prompt);
        self.history_index = None;
        mode
    }
    pub(in crate::editor::session) fn configure_confirmation_actions(
        &mut self,
        title: String,
        message: String,
        selected: ConfirmationChoice,
        positive_label: &str,
        negative_label: &str,
        hint: String,
    ) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        prompt.confirmation = Some(ConfirmationState {
            title,
            message,
            selected,
            positive_label: positive_label.into(),
            negative_label: negative_label.into(),
            hint,
        });
        true
    }
    pub(in crate::editor::session) fn select_confirmation(&mut self, selected: ConfirmationChoice) {
        if let Some(confirmation) = self
            .prompt
            .as_mut()
            .and_then(|prompt| prompt.confirmation.as_mut())
        {
            confirmation.selected = selected;
        }
    }
    pub(in crate::editor::session) fn toggle_confirmation(&mut self) {
        if let Some(confirmation) = self
            .prompt
            .as_mut()
            .and_then(|prompt| prompt.confirmation.as_mut())
        {
            confirmation.selected = match confirmation.selected {
                ConfirmationChoice::Yes => ConfirmationChoice::No,
                ConfirmationChoice::No => ConfirmationChoice::Yes,
            };
        }
    }
    pub(in crate::editor::session) fn close_prompt(
        &mut self,
        current: &crate::editor::EditorWindowVersion,
    ) -> Option<(ViewId, EditorMode)> {
        if self.prompt.is_some() {
            self.capture_changed();
        }
        self.prompt = None;
        self.history_index = None;
        self.cancel_prompt_completions();
        self.previous_mode
            .take()
            .filter(|(source, _, _)| source.same_editor(current))
            .map(|(_, view, mode)| (view, mode))
    }
    pub(in crate::editor::session) fn cancel_prompt_completions(&mut self) {
        self.completion_generation = self.completion_generation.saturating_add(1);
    }
    pub(in crate::editor::session) fn prompt_help(&mut self, help: String) {
        if let Some(prompt) = &mut self.prompt {
            prompt.help = Some(help);
        }
    }
    pub(in crate::editor::session) fn prompt_error(&mut self, error: String) {
        if let Some(prompt) = &mut self.prompt {
            prompt.error = Some(error);
        }
    }
    pub(in crate::editor::session) fn command_history(&self) -> &[String] {
        &self.command_history
    }
    pub(in crate::editor::session) fn search_history(&self) -> &[String] {
        &self.search_history
    }
    pub(in crate::editor::session) fn clear_history(&mut self) {
        self.history_cleared = true;
        self.command_history.clear();
        self.search_history.clear();
        self.history_index = None;
    }
    pub(in crate::editor::session) fn restore_history(
        &mut self,
        commands: Vec<String>,
        searches: Vec<String>,
    ) {
        if self.history_cleared {
            return;
        }
        let merge = |mut saved: Vec<String>, current: &mut Vec<String>| {
            saved.retain(|entry| !current.contains(entry));
            saved.append(current);
            *current = saved;
        };
        merge(commands, &mut self.command_history);
        merge(searches, &mut self.search_history);
        for history in [&mut self.command_history, &mut self.search_history] {
            let excess = history.len().saturating_sub(super::HISTORY_LIMIT);
            history.drain(..excess);
        }
        self.history_index = None;
    }
    pub(in crate::editor::session) fn completion_current(&self, key: &PromptCompletionKey) -> bool {
        key.generation == self.completion_generation
            && self
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.input == key.prompt_input)
    }
    pub(in crate::editor::session) fn accept_prompt_paths(
        &mut self,
        request: &PromptCompletionKey,
        paths: Vec<PathBuf>,
        root: &Path,
        policy: &crate::config::Config,
    ) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        if request.generation != self.completion_generation || prompt.input != request.prompt_input
        {
            return false;
        }
        let prefix = PathBuf::from(&request.fragment)
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_lowercase());
        let detail = &policy.command_labels["file.open"];
        prompt.completions = paths
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase().starts_with(&prefix))
            })
            .take(COMPLETION_LIMIT)
            .map(|path| CompletionItem {
                icon: None,
                color: None,
                label: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                detail: detail.clone(),
            })
            .collect();
        prompt.selected_completion = None;
        true
    }
    pub(in crate::editor::session) fn search_origin(
        &mut self,
        id: crate::editor::ViewId,
        view: &crate::editor::View,
    ) -> crate::editor::text::ByteOffset {
        self.search_origin.get_or_insert((id, view.cursor, view.scroll_line, view.scroll_wrap_row, view.scroll_column)).1
    }
    pub(in crate::editor::session) fn take_search_origin(
        &mut self,
    ) -> Option<(crate::editor::ViewId, crate::editor::text::ByteOffset, usize, usize, usize)> {
        self.search_origin.take()
    }
    pub(in crate::editor::session) fn set_pending_breakpoint(&mut self, row: crate::debugger::DapSourceBreakpoint) {
        self.pending_breakpoint = Some(row);
    }
    pub(in crate::editor::session) fn take_pending_breakpoint(&mut self) -> Option<crate::debugger::DapSourceBreakpoint> {
        self.pending_breakpoint.take()
    }
    pub(in crate::editor::session) fn prompt_insert(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            if prompt.confirmation.is_some() {
                return;
            }
            if prompt.kind == PromptKind::ExplorerRename
                && (prompt.input.len().saturating_add(text.len()) > 4096
                    || text.chars().any(char::is_control))
            {
                prompt.error =
                    Some("rename path must contain no controls and at most 4096 bytes".into());
                return;
            }
            if prompt.input.is_char_boundary(prompt.cursor) {
                prompt.input.insert_str(prompt.cursor, text);
                prompt.cursor += text.len();
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
    }

    pub(in crate::editor::session) fn prompt_backspace(&mut self) {
        self.prompt_delete_back(|before| {
            before
                .grapheme_indices(true)
                .next_back()
                .map_or(before.len(), |(index, _)| index)
        });
    }

    /// Vim's ctrl-w: the word or run of punctuation before the cursor, and the spaces after it.
    pub(in crate::editor::session) fn prompt_delete_word(&mut self) {
        self.prompt_delete_back(|before| {
            let trimmed = before.trim_end();
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let in_word = trimmed.chars().next_back().is_some_and(word);
            trimmed
                .char_indices()
                .rev()
                .take_while(|(_, c)| !c.is_whitespace() && word(*c) == in_word)
                .last()
                .map_or(trimmed.len(), |(index, _)| index)
        });
    }

    /// Vim's ctrl-u: everything before the cursor.
    pub(in crate::editor::session) fn prompt_delete_to_start(&mut self) {
        self.prompt_delete_back(|_| 0);
    }

    fn prompt_delete_back(&mut self, start: impl FnOnce(&str) -> usize) {
        if let Some(prompt) = &mut self.prompt {
            if prompt.confirmation.is_some() {
                return;
            }
            let start = start(&prompt.input[..prompt.cursor]);
            if start < prompt.cursor {
                prompt.input.replace_range(start..prompt.cursor, "");
                prompt.cursor = start;
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
    }

    pub(in crate::editor::session) fn prompt_move(&mut self, movement: PromptMove) {
        if let Some(prompt) = &mut self.prompt {
            if let Some(confirmation) = &mut prompt.confirmation {
                confirmation.selected = match movement {
                    PromptMove::Left | PromptMove::WordLeft | PromptMove::Home => {
                        ConfirmationChoice::Yes
                    }
                    PromptMove::Right | PromptMove::WordRight | PromptMove::End => {
                        ConfirmationChoice::No
                    }
                };
                return;
            }
            prompt.cursor = movement.apply(&prompt.input, prompt.cursor);
        }
    }

    pub(in crate::editor::session) fn prompt_history(&mut self, direction: i8) {
        let Some(kind) = self.prompt.as_ref().map(|prompt| prompt.kind) else {
            return;
        };
        if matches!(
            kind,
            PromptKind::WorkspaceTrust
                | PromptKind::GitReset
                | PromptKind::ExplorerDelete
                | PromptKind::ExplorerRename
                | PromptKind::PluginConfirmation
                | PromptKind::TerminalClose
                | PromptKind::DirtyClose
                | PromptKind::GitStageUnsaved
                | PromptKind::GitDestructive
                | PromptKind::AgentPermission
                | PromptKind::ReviewComment
                | PromptKind::BreakpointCondition
                | PromptKind::ReplaceFind
                | PromptKind::ReplaceWith
                | PromptKind::SubstituteConfirm
        ) {
            return;
        }
        let history = if kind == PromptKind::Command {
            &self.command_history
        } else {
            &self.search_history
        };
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
        if let Some(prompt) = &mut self.prompt {
            prompt.input = selected;
            prompt.cursor = prompt.input.len();
            prompt.error = None;
            prompt.selected_completion = None;
        }
    }

    pub(in crate::editor::session) fn prompt_complete(&mut self, direction: i8) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        if prompt.completions.is_empty() {
            return;
        }
        let length = prompt.completions.len();
        prompt.selected_completion = Some(match (prompt.selected_completion, direction < 0) {
            (None, false) => 0,
            (None, true) => length - 1,
            (Some(current), false) => (current + 1) % length,
            (Some(current), true) => current.checked_sub(1).unwrap_or(length - 1),
        });
    }

    pub(in crate::editor::session) fn accept_completion(
        &mut self,
        policy: &crate::config::Config,
    ) -> bool {
        let Some(prompt) = &mut self.prompt else {
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
            if policy
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| takes_path(id))
            {
                prompt.input = format!("{command} {label}");
            } else {
                prompt.input = label;
            }
        } else {
            prompt.input = label;
            if policy
                .input
                .command_aliases
                .get(prompt.input.as_str())
                .is_some_and(|id| takes_path(id))
            {
                prompt.input.push(' ');
            } else {
                // A command that needs typed text completes to what the palette opens it with.
                let command = policy.input.command_aliases.get(prompt.input.as_str());
                let command = command.map_or(prompt.input.as_str(), String::as_str);
                if let Some(initial) = policy.command_palette.prompts.get(command) {
                    prompt.input = initial.clone();
                }
            }
        }
        prompt.cursor = prompt.input.len();
        prompt.selected_completion = None;
        prompt.error = None;
        true
    }

    pub(in crate::editor::session) fn refresh_completions<'a>(
        &mut self,
        policy: &crate::config::Config,
        root: &Path,
        buffers: impl IntoIterator<Item = &'a Path>,
        commands: impl IntoIterator<Item = &'a str>,
    ) -> Option<PromptCompletionRequest> {
        let Some(prompt) = &self.prompt else {
            return None;
        };
        if prompt.kind != PromptKind::Command {
            if let Some(prompt) = &mut self.prompt {
                prompt.completions.clear();
                prompt.selected_completion = None;
            }
            return None;
        }
        let input = prompt.input.clone();
        let mut request = None;
        let mut candidates = BTreeMap::<String, String>::new();
        if let Some((command, fragment)) = input.split_once(char::is_whitespace)
            && policy
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| takes_path(id))
        {
            let fragment = fragment.trim();
            let query = fragment.to_lowercase();
            for path in buffers {
                let label = path.display().to_string();
                if label.to_lowercase().contains(&query) {
                    let detail = &policy.command_labels["file.open"];
                    candidates.insert(label, detail.clone());
                }
            }
            self.completion_generation = self.completion_generation.saturating_add(1);
            let generation = self.completion_generation;
            let entered = PathBuf::from(fragment);
            let parent = entered.parent().filter(|path| !path.as_os_str().is_empty());
            let directory = parent.map_or_else(
                || root.to_path_buf(),
                |parent| {
                    if parent.is_absolute() {
                        parent.to_path_buf()
                    } else {
                        root.join(parent)
                    }
                },
            );
            request = Some(PromptCompletionRequest {
                key: PromptCompletionKey {
                    generation,
                    prompt_input: input.clone(),
                    fragment: fragment.into(),
                },
                directory,
                limit: COMPLETION_LIMIT,
            });
        }
        let mut ordered = candidates.into_iter().collect::<Vec<_>>();
        if request.is_none() && !input.contains(char::is_whitespace) {
            ordered = command_completions(policy, &input, commands);
        }
        if let Some(prompt) = &mut self.prompt {
            prompt.completions = ordered
                .into_iter()
                .take(COMPLETION_LIMIT)
                .map(|(label, detail)| CompletionItem {
                    icon: None,
                    label,
                    detail,
                    color: None,
                })
                .collect();
            if prompt
                .selected_completion
                .is_some_and(|index| index >= prompt.completions.len())
            {
                prompt.selected_completion = None;
            }
        }
        request
    }

    pub(in crate::editor::session) fn record_prompt_history(
        &mut self,
        kind: PromptKind,
        input: &str,
    ) {
        if input.is_empty() {
            return;
        }
        if matches!(
            kind,
            PromptKind::WorkspaceTrust
                | PromptKind::ExplorerRename
                | PromptKind::ReviewComment
                | PromptKind::BreakpointCondition
                | PromptKind::ReplaceFind
                | PromptKind::ReplaceWith
                | PromptKind::SubstituteConfirm
        ) {
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
        let excess = history.len().saturating_sub(super::HISTORY_LIMIT);
        if excess > 0 {
            history.drain(..excess);
        }
    }
}

/// `:` completions, Vim style: one row per command, named by its shortest alias that fits
/// what is typed, so `w`, `q` and `e` come first. Labels match too. Commands without an alias
/// follow under their dotted id; plumbing only keys use appears once the input has a `.`.
fn command_completions<'a>(
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
        let label = policy
            .command_labels
            .get(command_id)
            .expect("resolved configuration validates command labels");
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
        let Some(label) = policy.command_labels.get(id) else {
            continue;
        };
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
        .map(|(command_id, rank)| (rank, policy.command_labels[&command_id].clone()))
        .collect::<Vec<_>>();
    rows.sort();
    rows.into_iter()
        .map(|(rank, label)| (rank.3, label))
        .collect()
}

/// Commands whose argument is a path, which the command line completes from files.
fn takes_path(command: &str) -> bool {
    matches!(command, "file.open" | "file.save" | "workspace.root.add")
}
