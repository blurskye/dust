use super::InputSession;
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
        self.command_history.clear();
        self.search_history.clear();
        self.history_index = None;
    }
    pub(in crate::editor::session) fn configure_history(
        &mut self,
        policy: &crate::config::HistoryConfig,
    ) {
        for (history, configured) in [
            (&mut self.command_history, &policy.command),
            (&mut self.search_history, &policy.search),
        ] {
            if !configured.enabled {
                history.clear();
            } else {
                let excess = history.len().saturating_sub(configured.maximum_entries);
                history.drain(..excess);
            }
        }
        self.history_index = None;
    }
    pub(in crate::editor::session) fn restore_history(
        &mut self,
        commands: Vec<String>,
        searches: Vec<String>,
        policy: &crate::config::PersistenceConfig,
    ) {
        if policy.persist_command_history {
            self.command_history = commands;
        }
        if policy.persist_search_history {
            self.search_history = searches;
        }
        for history in [&mut self.command_history, &mut self.search_history] {
            let excess = history.len().saturating_sub(policy.maximum_entries);
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
            .take(policy.runtime.completion_limit)
            .map(|path| CompletionItem {
                label: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                detail: detail.clone(),
            })
            .collect();
        prompt.selected_completion =
            (policy.input.completion_select_first && !prompt.completions.is_empty()).then_some(0);
        true
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
        if let Some(prompt) = &mut self.prompt {
            if prompt.confirmation.is_some() {
                return;
            }
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
    }

    pub(in crate::editor::session) fn prompt_move(&mut self, movement: PromptMove) {
        if let Some(prompt) = &mut self.prompt {
            if let Some(confirmation) = &mut prompt.confirmation {
                confirmation.selected = match movement {
                    PromptMove::Left | PromptMove::Home => ConfirmationChoice::Yes,
                    PromptMove::Right | PromptMove::End => ConfirmationChoice::No,
                };
                return;
            }
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

    pub(in crate::editor::session) fn prompt_history(
        &mut self,
        direction: i8,
        policy: &crate::config::Config,
    ) {
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
        ) {
            return;
        }
        let (enabled, history) = if kind == PromptKind::Command {
            (policy.history.command.enabled, &self.command_history)
        } else {
            (policy.history.search.enabled, &self.search_history)
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
        if let Some(prompt) = &mut self.prompt {
            prompt.input = selected;
            prompt.cursor = prompt.input.len();
            prompt.error = None;
            prompt.selected_completion = None;
        }
    }

    pub(in crate::editor::session) fn prompt_complete(
        &mut self,
        direction: i8,
        policy: &crate::config::Config,
    ) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        if prompt.completions.is_empty() {
            return;
        }
        let length = prompt.completions.len();
        let wrap = policy.input.completion_wrap;
        prompt.selected_completion = Some(match (prompt.selected_completion, direction < 0) {
            (None, false) => 0,
            (None, true) => length - 1,
            (Some(current), false) if wrap => (current + 1) % length,
            (Some(current), true) if wrap => current.checked_sub(1).unwrap_or(length - 1),
            (Some(current), false) => (current + 1).min(length - 1),
            (Some(current), true) => current.saturating_sub(1),
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
                .is_some_and(|id| matches!(id.as_str(), "file.open" | "workspace.root.add"))
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
        if let Some((command, fragment)) = input.split_once(char::is_whitespace) {
            if policy.input.command_aliases.get(command).is_some_and(|id| {
                matches!(
                    id.as_str(),
                    "file.open" | "file.save" | "workspace.root.add"
                )
            }) {
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
                    limit: policy.runtime.completion_limit,
                });
            }
        } else {
            let query = input.to_lowercase();
            for (alias, command_id) in &policy.input.command_aliases {
                let detail = policy
                    .command_labels
                    .get(command_id)
                    .expect("resolved configuration validates command labels");
                if alias.starts_with(&query) || detail.to_lowercase().contains(&query) {
                    candidates.insert(alias.clone(), detail.into());
                }
            }
            for id in commands {
                let label = &policy.command_labels[id];
                if id.starts_with(&query) || label.to_lowercase().contains(&query) {
                    candidates.insert(id.into(), label.clone());
                }
            }
        }
        if let Some(prompt) = &mut self.prompt {
            prompt.completions = candidates
                .into_iter()
                .take(policy.runtime.completion_limit)
                .map(|(label, detail)| CompletionItem { label, detail })
                .collect();
            if prompt
                .selected_completion
                .is_some_and(|index| index >= prompt.completions.len())
            {
                prompt.selected_completion = None;
            }
            if prompt.selected_completion.is_none()
                && policy.input.completion_select_first
                && !prompt.completions.is_empty()
            {
                prompt.selected_completion = Some(0);
            }
        }
        request
    }

    pub(in crate::editor::session) fn record_prompt_history(
        &mut self,
        kind: PromptKind,
        input: &str,
        policy: &crate::config::Config,
    ) {
        if input.is_empty() {
            return;
        }
        if matches!(
            kind,
            PromptKind::WorkspaceTrust
                | PromptKind::ExplorerRename
                | PromptKind::ReviewComment
        ) {
            return;
        }
        let configured = if kind == PromptKind::Command {
            &policy.history.command
        } else {
            &policy.history.search
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
}
