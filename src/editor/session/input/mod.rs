mod documentation;
mod mouse;
mod prompts;
mod replay;
mod snippets;
pub(in crate::editor::session) use mouse::{MouseDocument, MouseGeometry};
pub(in crate::editor::session) use replay::ReplayError;
pub(in crate::editor::session) use snippets::SnippetDocument;
#[cfg(test)]
mod tests;
use super::configuration::{KeymapOperation, ResolvedKeymaps, display_key_sequence};
use crate::{
    editor::{
        CommandInvocation, EditorMode,
        input::{
            GrammarInput, InputAction, InputError, KeyEvent, KeymapSequence, MappingResolution,
            ModalResolver, PendingInputContext,
        },
    },
    ui::{KeyHintUiRow, KeyHintUiState},
};
pub(in crate::editor::session) use prompts::PromptCompletionKey;
use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

struct PendingKeymap {
    context: String,
    prefix: String,
    started_at: Instant,
    deadline: Option<Instant>,
    binding_revision: u64,
}

/// Runtime input state is separate from immutable compiled configuration bindings.
pub(super) struct InputSession {
    snippet: Option<snippets::SnippetSession>,
    mouse: mouse::MouseState,
    last_search_forward: bool,
    modal: ModalResolver,
    sequence: KeymapSequence,
    pending: Option<PendingKeymap>,
    hints: Option<KeyHintUiState>,
    prompt: Option<crate::ui::PromptState>,
    /// The staging effect whose plan was refused for unsaved content. Held here rather
    /// than on the coordinator, whose 26-field ownership census is a frozen boundary,
    /// and because this is a prompt obligation: it is replayed when the answer is Yes.
    pending_stage: Option<super::AppEffect>,
    picker_normal: bool,
    documentation: Option<crate::ui::DocumentationPopupState>,
    command_history: Vec<String>,
    search_history: Vec<String>,
    history_index: Option<usize>,
    previous_mode: Option<(
        crate::editor::EditorWindowVersion,
        crate::editor::ViewId,
        EditorMode,
    )>,
    capture: Option<super::windows::FocusLease>,
    capture_epoch: std::sync::Arc<()>,
    completion_generation: u64,
    last_change: Vec<CommandInvocation>,
    pending_change: Option<Vec<CommandInvocation>>,
    recording_macro: Option<(char, Vec<CommandInvocation>)>,
    last_macro: Option<char>,
    replay_depth: usize,
    replay_steps: usize,
    completion_reads: HashMap<crate::files::RequestId, PromptCompletionKey>,
}

impl InputSession {
    pub(in crate::editor::session) fn register_completion_read(
        &mut self,
        id: crate::files::RequestId,
        key: PromptCompletionKey,
    ) {
        self.completion_reads.insert(id, key);
    }
    pub(in crate::editor::session) fn take_completion_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<PromptCompletionKey> {
        self.completion_reads.remove(&id)
    }
    /// Path completion keeps one directory listing in flight; a slow directory such as
    /// `/tmp` would otherwise fill the file worker queue with one listing per keystroke.
    pub(in crate::editor::session) fn completion_read_pending(&self) -> bool {
        !self.completion_reads.is_empty()
    }
    pub(in crate::editor::session) fn retire_document_request(
        &mut self,
        id: crate::files::RequestId,
    ) {
        self.completion_reads.remove(&id);
    }
    pub(in crate::editor::session) fn begin_picker(&mut self) {
        self.picker_normal = false;
        self.cancel_pending();
        self.cancel_mapping();
        self.capture_changed();
    }
    pub(in crate::editor::session) fn picker_normal(&self) -> bool {
        self.picker_normal
    }
    pub(in crate::editor::session) fn set_picker_normal(&mut self, normal: bool) {
        self.picker_normal = normal;
        self.cancel_mapping();
    }
    pub(in crate::editor::session) fn capture_epoch(&self) -> std::sync::Arc<()> {
        self.capture_epoch.clone()
    }
    pub(in crate::editor::session) fn capture_current(&self, source: &std::sync::Arc<()>) -> bool {
        std::sync::Arc::ptr_eq(source, &self.capture_epoch)
    }
    pub(in crate::editor::session) fn capture_changed(&mut self) {
        self.capture_epoch = std::sync::Arc::new(());
    }

    pub(super) fn new(count_limit: u64) -> Self {
        Self {
            snippet: None,
            mouse: mouse::MouseState::default(),
            last_search_forward: true,
            modal: ModalResolver::with_count_limit(count_limit),
            sequence: KeymapSequence::default(),
            pending: None,
            hints: None,
            prompt: None,
            pending_stage: None,
            picker_normal: false,
            documentation: None,
            command_history: Vec::new(),
            search_history: Vec::new(),
            history_index: None,
            previous_mode: None,
            capture: None,
            capture_epoch: std::sync::Arc::new(()),
            completion_generation: 0,
            last_change: Vec::new(),
            pending_change: None,
            recording_macro: None,
            last_macro: None,
            replay_depth: 0,
            replay_steps: 0,
            completion_reads: HashMap::new(),
        }
    }
    pub(in crate::editor::session) fn hold_focus(
        &mut self,
        capture: Option<super::windows::FocusLease>,
    ) {
        if self.capture.is_none() {
            self.capture = capture;
        }
    }
    pub(in crate::editor::session) fn released_focus(
        &mut self,
    ) -> Option<super::windows::FocusLease> {
        if self.capture_context().is_none() {
            self.capture.take()
        } else {
            None
        }
    }
    pub(in crate::editor::session) fn set_pending_stage(
        &mut self,
        effect: super::AppEffect,
    ) {
        self.pending_stage = Some(effect);
    }
    pub(in crate::editor::session) fn take_pending_stage(
        &mut self,
    ) -> Option<super::AppEffect> {
        self.pending_stage.take()
    }
    pub(in crate::editor::session) fn clear_pending_stage(&mut self) {
        self.pending_stage = None;
    }
    pub(in crate::editor::session) fn capture_context(&self) -> Option<&'static str> {
        if let Some(prompt) = &self.prompt {
            return Some(match prompt.kind {
                crate::ui::PromptKind::Command
                | crate::ui::PromptKind::ExplorerRename
                | crate::ui::PromptKind::ReviewComment => {
                    "command_prompt"
                }
                crate::ui::PromptKind::SearchForward | crate::ui::PromptKind::SearchBackward => {
                    "search_prompt"
                }
                crate::ui::PromptKind::GitReset => "workspace_trust_prompt",
                crate::ui::PromptKind::WorkspaceTrust
                | crate::ui::PromptKind::ExplorerDelete
                | crate::ui::PromptKind::PluginConfirmation
                | crate::ui::PromptKind::TerminalClose
                | crate::ui::PromptKind::DirtyClose
                | crate::ui::PromptKind::GitStageUnsaved => "confirmation_prompt",
                crate::ui::PromptKind::GitDestructive | crate::ui::PromptKind::AgentPermission => {
                    "confirmation_prompt"
                }
            });
        }
        self.documentation
            .as_ref()
            .filter(|popup| popup.kind != crate::ui::DocumentationPopupKind::Completion)
            .map(|_| "documentation")
    }
    pub(super) fn reconfigure(&mut self, policy: &crate::config::Config) {
        self.modal.set_count_limit(policy.input.count_limit);
        self.cancel_mapping();
        self.cancel_prompt_completions();
        self.configure_history(&policy.history);
        self.close_documentation();
        if let Some(prompt) = &mut self.prompt {
            prompt.completions.clear();
            prompt.selected_completion = None;
        }
    }
    pub(super) fn cancel_pending(&mut self) {
        self.modal.cancel_pending();
    }
    pub(super) fn effective_mode(&self, mode: EditorMode) -> EditorMode {
        self.modal.effective_mode(mode)
    }
    pub(super) fn pending_context(&self) -> Option<PendingInputContext> {
        self.modal.pending_context()
    }
    pub(super) fn decorate_invocation(&mut self, invocation: &mut CommandInvocation) {
        self.modal.decorate_invocation(invocation);
    }
    pub(super) fn feed(
        &mut self,
        mode: EditorMode,
        input: GrammarInput,
    ) -> Result<InputAction, InputError> {
        let pending = self.modal.has_pending_grammar();
        let result = self.modal.feed(mode, input);
        if !pending && self.modal.has_pending_grammar() {
            self.capture_changed();
        }
        result
    }

    pub(super) fn resolve(
        &mut self,
        context: &str,
        key: KeyEvent,
        bindings: &ResolvedKeymaps,
        revision: u64,
        now: Instant,
        timeout: Duration,
    ) -> MappingResolution {
        if self.pending.as_ref().is_some_and(|pending| {
            pending.context != context || pending.binding_revision != revision
        }) {
            self.cancel_mapping();
        }
        let was_pending = self.pending.is_some();
        let result = bindings.resolve(&mut self.sequence, context, key);
        match result {
            MappingResolution::Pending | MappingResolution::Ambiguous => {
                let started_at = self
                    .pending
                    .as_ref()
                    .map_or(now, |pending| pending.started_at);
                self.pending = Some(PendingKeymap {
                    context: context.into(),
                    prefix: display_key_sequence(self.sequence.pending_sequence()),
                    started_at,
                    deadline: matches!(result, MappingResolution::Ambiguous)
                        .then_some(now + timeout),
                    binding_revision: revision,
                });
            }
            _ => self.pending = None,
        }
        if !was_pending && self.pending.is_some() {
            self.capture_changed();
        }
        self.hints = None;
        result
    }
    pub(super) fn cancel_mapping(&mut self) {
        self.pending = None;
        self.sequence.cancel();
        self.hints = None;
    }
    pub(super) fn flush_timeout(
        &mut self,
        context: &str,
        revision: u64,
        now: Instant,
    ) -> Option<CommandInvocation> {
        let pending = self.pending.as_ref()?;
        if pending.context != context || pending.binding_revision != revision {
            self.cancel_mapping();
            return None;
        }
        if pending.deadline.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.pending = None;
        self.hints = None;
        match self.sequence.flush_timeout() {
            MappingResolution::Ready(invocation) => Some(invocation),
            _ => None,
        }
    }
    pub(super) fn hide_hints(&mut self) {
        self.hints = None;
    }
    pub(super) fn hints(&self) -> Option<&KeyHintUiState> {
        self.hints.as_ref()
    }
    pub(super) fn update_hints(
        &mut self,
        context: &str,
        version: (&ResolvedKeymaps, u64),
        policy: &crate::config::KeyHintConfig,
        labels: &BTreeMap<String, String>,
        now: Instant,
    ) {
        self.hints = None;
        let Some(pending) = &self.pending else {
            return;
        };
        let (bindings, revision) = version;
        if pending.context != context || pending.binding_revision != revision {
            self.cancel_mapping();
            return;
        }
        if !policy.enabled
            || pending.deadline.is_some_and(|deadline| now >= deadline)
            || now.saturating_duration_since(pending.started_at)
                < Duration::from_millis(policy.delay_ms)
        {
            return;
        }
        let Some(projection) = bindings.hints_for_prefix(context, &pending.prefix, labels) else {
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
            self.hints = Some(KeyHintUiState {
                prefix: projection.prefix,
                rows,
            });
        }
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn mapping_prefix(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.prefix.as_str())
    }
    pub(in crate::editor::session) fn mapping_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::editor::session) fn grammar_pending(&self) -> bool {
        self.modal.has_pending_grammar()
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn mapping_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().and_then(|pending| pending.deadline)
    }
}
