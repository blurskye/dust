mod mouse;

pub(crate) const COMPLETION_LIMIT: usize = 1000;
pub(crate) const HISTORY_LIMIT: usize = 100;

pub(in crate::app) use mouse::MouseDocument;

pub(in crate::app) use mouse::MouseGeometry;
pub(in crate::app) use replay::ReplayError;
pub(in crate::app) use snippets::SnippetDocument;
#[cfg(test)]
mod tests;
use crate::app::configuration::KeymapOperation;
use crate::app::configuration::ResolvedKeymaps;
use crate::app::configuration::display_key_sequence;
use crate::editor::CommandInvocation;
use crate::editor::EditorMode;
use crate::editor::input::GrammarInput;
use crate::editor::input::InputAction;
use crate::editor::input::InputError;
use crate::editor::input::KeyEvent;
use crate::editor::input::KeymapSequence;
use crate::editor::input::MappingResolution;
use crate::editor::input::ModalResolver;
use crate::editor::input::PendingInputContext;
use crate::ui::KeyHintUiRow;
use crate::ui::KeyHintUiState;
pub(in crate::app) use prompts::PromptCompletionKey;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

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
    whole_word_search: bool,
    /// Cursor and viewport before incremental search moved them.
    search_origin: Option<(
        crate::editor::ViewId,
        crate::editor::text::ByteOffset,
        usize,
        usize,
        usize,
    )>,
    pending_breakpoint: Option<crate::debugger::DapSourceBreakpoint>,
    modal: ModalResolver,
    sequence: KeymapSequence,
    pending: Option<PendingKeymap>,
    hints: Option<KeyHintUiState>,
    prompt: Option<crate::ui::PromptState>,
    /// The staging effect whose plan was refused for unsaved content. Held here rather
    /// than on the coordinator, whose 26-field ownership census is a frozen boundary,
    /// and because this is a prompt obligation: it is replayed when the answer is Yes.
    pending_stage: Option<(crate::app::AppEffect, Option<u32>)>,
    stage_save: Option<crate::files::RequestId>,
    picker_normal: bool,
    documentation: Option<crate::ui::DocumentationPopupState>,
    history_cleared: bool,
    command_history: Vec<String>,
    search_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<(String, usize)>,
    previous_mode: Option<(
        crate::editor::EditorWindowVersion,
        crate::editor::ViewId,
        EditorMode,
    )>,
    capture: Option<crate::app::windows::FocusLease>,
    capture_epoch: std::sync::Arc<()>,
    completion_generation: u64,
    last_change: Vec<CommandInvocation>,
    pending_change: Option<Vec<CommandInvocation>>,
    pending_change_edited: bool,
    recording_macro: Option<(char, Vec<CommandInvocation>)>,
    last_macro: Option<char>,
    replay_depth: usize,
    replay_steps: usize,
    completion_reads: HashMap<crate::files::RequestId, PromptCompletionKey>,
}

impl InputSession {
    pub(in crate::app) fn register_completion_read(
        &mut self,
        id: crate::files::RequestId,
        key: PromptCompletionKey,
    ) {
        self.completion_reads.insert(id, key);
    }
    pub(in crate::app) fn take_completion_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<PromptCompletionKey> {
        self.completion_reads.remove(&id)
    }
    /// Path completion keeps one directory listing in flight; a slow directory such as
    /// `/tmp` would otherwise fill the file worker queue with one listing per keystroke.
    pub(in crate::app) fn completion_read_pending(&self) -> bool {
        !self.completion_reads.is_empty()
    }
    pub(in crate::app) fn retire_document_request(
        &mut self,
        id: crate::files::RequestId,
    ) {
        self.completion_reads.remove(&id);
        if self.stage_save == Some(id) {
            self.clear_pending_stage();
        }
    }
    pub(in crate::app) fn begin_picker(&mut self) {
        self.picker_normal = false;
        self.cancel_pending();
        self.cancel_mapping();
        self.capture_changed();
    }
    pub(in crate::app) fn picker_normal(&self) -> bool {
        self.picker_normal
    }
    pub(in crate::app) fn set_picker_normal(&mut self, normal: bool) {
        self.picker_normal = normal;
        self.cancel_mapping();
    }
    pub(in crate::app) fn capture_epoch(&self) -> std::sync::Arc<()> {
        self.capture_epoch.clone()
    }
    pub(in crate::app) fn capture_current(&self, source: &std::sync::Arc<()>) -> bool {
        std::sync::Arc::ptr_eq(source, &self.capture_epoch)
    }
    pub(in crate::app) fn capture_changed(&mut self) {
        self.capture_epoch = std::sync::Arc::new(());
    }

    pub(super) fn new(count_limit: u64) -> Self {
        Self {
            snippet: None,
            mouse: mouse::MouseState::default(),
            last_search_forward: true,
            whole_word_search: false,
            search_origin: None,
            pending_breakpoint: None,
            modal: ModalResolver::with_count_limit(count_limit),
            sequence: KeymapSequence::default(),
            pending: None,
            hints: None,
            prompt: None,
            pending_stage: None,
            stage_save: None,
            picker_normal: false,
            documentation: None,
            history_cleared: false,
            command_history: Vec::new(),
            search_history: Vec::new(),
            history_index: None,
            history_draft: None,
            previous_mode: None,
            capture: None,
            capture_epoch: std::sync::Arc::new(()),
            completion_generation: 0,
            last_change: Vec::new(),
            pending_change: None,
            pending_change_edited: false,
            recording_macro: None,
            last_macro: None,
            replay_depth: 0,
            replay_steps: 0,
            completion_reads: HashMap::new(),
        }
    }
    pub(in crate::app) fn hold_focus(
        &mut self,
        capture: Option<crate::app::windows::FocusLease>,
    ) {
        if self.capture.is_none() {
            self.capture = capture;
        }
    }
    pub(in crate::app) fn released_focus(
        &mut self,
    ) -> Option<crate::app::windows::FocusLease> {
        if self.capture_context().is_none() {
            self.capture.take()
        } else {
            None
        }
    }
    pub(in crate::app) fn set_pending_stage(&mut self, effect: crate::app::AppEffect) {
        self.stage_save = None;
        self.pending_stage = Some((effect, None));
    }
    pub(in crate::app) fn take_pending_stage(
        &mut self,
    ) -> Option<(crate::app::AppEffect, Option<u32>)> {
        self.pending_stage.take()
    }
    pub(in crate::app) fn set_pending_stage_line(&mut self, line: Option<u32>) {
        if let Some((_, selected)) = &mut self.pending_stage {
            *selected = line;
        }
    }
    pub(in crate::app) fn clear_pending_stage(&mut self) {
        self.pending_stage = None;
        self.stage_save = None;
    }
    pub(in crate::app) fn bind_stage_save(&mut self, request: crate::files::RequestId) {
        if self.pending_stage.is_some() {
            self.stage_save = Some(request);
        }
    }
    pub(in crate::app) fn take_stage_save(
        &mut self,
        request: crate::files::RequestId,
    ) -> Option<(crate::app::AppEffect, Option<u32>)> {
        if self.stage_save == Some(request) {
            self.stage_save = None;
            self.take_pending_stage()
        } else {
            None
        }
    }
    pub(in crate::app) fn capture_context(&self) -> Option<&'static str> {
        if let Some(prompt) = &self.prompt {
            return Some(match prompt.kind {
                crate::ui::PromptKind::Command
                | crate::ui::PromptKind::ExplorerRename
                | crate::ui::PromptKind::ReviewComment
                | crate::ui::PromptKind::BreakpointCondition
                | crate::ui::PromptKind::ReplaceFind
                | crate::ui::PromptKind::ReplaceWith => "command_prompt",
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
                crate::ui::PromptKind::GitDestructive
                | crate::ui::PromptKind::AgentPermission
                | crate::ui::PromptKind::SubstituteConfirm => "confirmation_prompt",
            });
        }
        self.documentation
            .as_ref()
            .filter(|popup| popup.kind != crate::ui::DocumentationPopupKind::Completion)
            .map(|_| "documentation")
    }
    pub(super) fn reconfigure(&mut self) {
        self.cancel_mapping();
        self.cancel_prompt_completions();
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
        if input == GrammarInput::MacroRecordToggle && self.recording_macro.is_some() {
            return Ok(InputAction::Invoke(CommandInvocation::new(
                "macro.record.stop",
                crate::editor::InvocationSource::Keymap,
            )?));
        }
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
                    // While typing, a key that may start a mapping (`j` of `jk`) is typed
                    // once the wait is over, as in Vim; elsewhere a bare prefix waits.
                    deadline: (matches!(result, MappingResolution::Ambiguous)
                        || matches!(context, "editor.insert" | "editor.replace" | "snippet"))
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
    pub(super) fn mapping_context(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.context.as_str())
    }
    pub(super) fn finish_mapping(&mut self, context: &str, revision: u64) -> MappingResolution {
        let Some(pending) = self.pending.take() else {
            return MappingResolution::Missing;
        };
        if pending.context != context || pending.binding_revision != revision {
            self.cancel_mapping();
            return MappingResolution::Missing;
        }
        self.hints = None;
        self.sequence.flush_timeout()
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
    ) -> MappingResolution {
        let Some(pending) = self.pending.as_ref() else {
            return MappingResolution::Missing;
        };
        if pending.context != context || pending.binding_revision != revision {
            self.cancel_mapping();
            return MappingResolution::Missing;
        }
        if pending.deadline.is_none_or(|deadline| now < deadline) {
            return MappingResolution::Missing;
        }
        self.finish_mapping(context, revision)
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
                    label: if group || row.direct {
                        row.label
                    } else {
                        crate::ui::render::terminal::KEY_HINT_FALLBACK_LABEL.into()
                    },
                    icon: if group { row.icon } else { None },
                    group,
                }
            })
            .take(
                usize::from(crate::ui::render::terminal::KEY_HINT_MAX_ROWS)
                    * usize::from(crate::ui::render::terminal::KEY_HINT_MAX_COLUMNS),
            )
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            self.hints = Some(KeyHintUiState {
                prefix: projection.prefix,
                rows,
            });
        }
    }
    #[cfg(test)]
    pub(in crate::app) fn mapping_prefix(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.prefix.as_str())
    }
    pub(in crate::app) fn mapping_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::app) fn grammar_pending(&self) -> bool {
        self.modal.has_pending_grammar()
    }
    #[cfg(test)]
    pub(in crate::app) fn mapping_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().and_then(|pending| pending.deadline)
    }
}
