//! Keyboard, mouse and mapped command input.

use crate::app::language::snippets;
mod mouse;

pub(in crate::app) const COMPLETION_LIMIT: usize = 1000;
pub(in crate::app) const HISTORY_LIMIT: usize = 100;

pub(in crate::app) use mouse::MouseDocument;

use crate::app::editing::replay::ReplayError;
pub(in crate::app) use mouse::MouseGeometry;
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
use crate::editor::input::{Keymap, ModeCursor, ModeKeys, NamedMode};
use crate::ui::KeyHintUiRow;
use crate::ui::KeyHintUiState;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
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
pub(super) struct InputState {
    pub(in crate::app) snippet: Option<snippets::SnippetExpansion>,
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
    named_mode: Option<(
        crate::editor::EditorWindowVersion,
        crate::editor::ViewId,
        NamedMode,
    )>,
    key_layers: Vec<(Option<(String, String)>, Keymap)>,
    pending_commands: BTreeMap<crate::editor::PendingCommandId, PendingCommand>,
    pending_input: Option<crate::editor::PendingCommandId>,
    held_input: VecDeque<crate::app::editing::clipboard::DeferredInput>,
    sequence: KeymapSequence,
    pending: Option<PendingKeymap>,
    hints: Option<KeyHintUiState>,
    prompt: Option<crate::ui::PromptState>,
    /// The staging effect refused for unsaved content, replayed when the answer is Yes.
    pending_stage: Option<(crate::app::commands::AppEffect, Option<u32>)>,
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
    completion_request_id: u64,
    last_change: Vec<CommandInvocation>,
    pending_change: Option<Vec<CommandInvocation>>,
    pending_change_edited: bool,
    recording_macro: Option<(char, Vec<CommandInvocation>)>,
    last_macro: Option<char>,
    replay_depth: usize,
    replay_steps: usize,
    completion_reads: HashMap<crate::files::RequestId, PromptCompletionKey>,
}

impl InputState {
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
    pub(in crate::app) fn forget_document_request(&mut self, id: crate::files::RequestId) {
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

    pub(in crate::app) fn new(count_limit: u64) -> Self {
        Self {
            snippet: None,
            mouse: mouse::MouseState::default(),
            last_search_forward: true,
            whole_word_search: false,
            search_origin: None,
            pending_breakpoint: None,
            modal: ModalResolver::with_count_limit(count_limit),
            named_mode: None,
            key_layers: Vec::new(),
            pending_commands: BTreeMap::new(),
            pending_input: None,
            held_input: VecDeque::new(),
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
            completion_request_id: 0,
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
    pub(in crate::app) fn hold_focus(&mut self, capture: Option<crate::app::windows::FocusLease>) {
        if self.capture.is_none() {
            self.capture = capture;
        }
    }
    pub(in crate::app) fn released_focus(&mut self) -> Option<crate::app::windows::FocusLease> {
        if self.capture_context().is_none() {
            self.capture.take()
        } else {
            None
        }
    }
    pub(in crate::app) fn set_pending_stage(&mut self, effect: crate::app::commands::AppEffect) {
        self.stage_save = None;
        self.pending_stage = Some((effect, None));
    }
    pub(in crate::app) fn take_pending_stage(
        &mut self,
    ) -> Option<(crate::app::commands::AppEffect, Option<u32>)> {
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
    ) -> Option<(crate::app::commands::AppEffect, Option<u32>)> {
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
                crate::ui::PromptKind::Text => "command_prompt",
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
    pub(in crate::app) fn reconfigure(&mut self) {
        self.cancel_mapping();
        self.cancel_prompt_completions();
        self.close_documentation();
        if let Some(prompt) = &mut self.prompt {
            prompt.completions.clear();
            prompt.selected_completion = None;
        }
    }
    pub(in crate::app) fn cancel_pending(&mut self) {
        self.modal.cancel_pending();
    }
    pub(in crate::app) fn effective_mode(&self, mode: EditorMode) -> EditorMode {
        self.modal.effective_mode(mode)
    }
    pub(in crate::app) fn pending_context(&self) -> Option<PendingInputContext> {
        self.modal.pending_context()
    }
    pub(in crate::app) fn decorate_invocation(&mut self, invocation: &mut CommandInvocation) {
        self.modal.decorate_invocation(invocation);
    }
    pub(in crate::app) fn feed(
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

    pub(in crate::app) fn resolve(
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
        let result = if context.starts_with("editor.") {
            let mode = self.named_mode.as_ref().map(|(_, _, mode)| mode);
            let inherit = mode.is_none_or(|mode| mode.base().is_some());
            self.sequence.feed_layers(
                self.key_layers
                    .iter()
                    .map(|(_, layer)| layer)
                    .rev()
                    .chain(mode.map(NamedMode::layer))
                    .chain(bindings.layers(context).filter(|_| inherit)),
                key,
            )
        } else {
            bindings.resolve(&mut self.sequence, context, key)
        };
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
    pub(in crate::app) fn mapping_context(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.context.as_str())
    }
    pub(in crate::app) fn finish_mapping(
        &mut self,
        context: &str,
        revision: u64,
    ) -> MappingResolution {
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
    pub(in crate::app) fn cancel_mapping(&mut self) {
        self.pending = None;
        self.sequence.cancel();
        self.hints = None;
    }
    pub(in crate::app) fn flush_timeout(
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
    pub(in crate::app) fn hide_hints(&mut self) {
        self.hints = None;
    }
    pub(in crate::app) fn hints(&self) -> Option<&KeyHintUiState> {
        self.hints.as_ref()
    }
    pub(in crate::app) fn update_hints(
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

use crate::editor::CommandValue;
use crate::editor::InvocationSource;

use crate::app::editing::replay::repeated;
use crate::app::editing::replay::replayed;

/// Semantic recording and replay limits belong to input, not command dispatch.
impl InputState {
    pub(in crate::app) fn record_successful_invocation(
        &mut self,
        invocation: &CommandInvocation,
        repeat_override: Option<&CommandInvocation>,
        suppress_repeat: bool,
        text_changed: bool,
    ) {
        if !matches!(
            invocation.source,
            InvocationSource::Macro | InvocationSource::Repeat
        ) && !(invocation.id.as_str().starts_with("command_line.")
            && (self
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::Command)
                || invocation.arguments.get("kind")
                    == Some(&CommandValue::String("command".into()))))
            && (!invocation.id.as_str().starts_with("macro.")
                || invocation.id.as_str() == "macro.replay")
            && !invocation.id.as_str().starts_with("input.")
            && let Some((_, recorded)) = &mut self.recording_macro
        {
            let mut invocation = invocation.clone();
            invocation.target = Default::default();
            recorded.push(invocation);
        }
        if invocation.source == InvocationSource::Repeat
            || suppress_repeat
            || (matches!(
                invocation.id.as_str(),
                "edit.operator" | "edit.repeat_visual"
            ) && invocation.arguments.get("operator")
                == Some(&CommandValue::String("yank".into())))
        {
            return;
        }
        let mut semantic = repeat_override.unwrap_or(invocation).clone();
        semantic.target = Default::default();
        match invocation.id.as_str() {
            "mode.insert" | "mode.replace" | "edit.substitute" => {
                self.pending_change = Some(vec![semantic]);
                self.pending_change_edited = text_changed;
            }
            "edit.operator" | "edit.repeat_visual"
                if semantic.arguments.get("operator")
                    == Some(&CommandValue::String("change".into())) =>
            {
                self.pending_change = Some(vec![semantic]);
                self.pending_change_edited = text_changed;
            }
            "edit.insert_text"
            | "edit.backspace"
            | "edit.replace_text"
            | "edit.replace_backspace" => {
                // Typing a closer may move over an existing pair without changing text.
                // Keep that keystroke in an edited insert so dot restores the cursor too.
                if (text_changed
                    || (invocation.id.as_str() == "edit.insert_text" && self.pending_change_edited))
                    && let Some(pending) = &mut self.pending_change
                {
                    pending.push(semantic);
                    self.pending_change_edited |= text_changed;
                }
            }
            "view.cursor.move" if self.pending_change.is_some() => {
                let pending = self.pending_change.take().expect("pending insert");
                let mode = if pending[0].id.as_str() == "mode.replace" {
                    "mode.replace"
                } else {
                    "mode.insert"
                };
                if self.pending_change_edited {
                    self.last_change = pending;
                    self.last_change.push(
                        CommandInvocation::new("mode.normal", invocation.source).expect("builtin"),
                    );
                }
                let mut transition =
                    CommandInvocation::new(mode, invocation.source).expect("builtin");
                if mode == "mode.insert" {
                    transition = transition
                        .with_argument("placement", CommandValue::String("before".into()));
                }
                self.pending_change = Some(vec![transition]);
                self.pending_change_edited = false;
            }
            "mode.normal" => {
                if let Some(mut pending) = self.pending_change.take()
                    && self.pending_change_edited
                {
                    pending.push(semantic);
                    self.last_change = pending;
                }
                self.pending_change_edited = false;
            }
            "edit.delete_char" if self.pending_change.is_some() => {
                if text_changed {
                    self.pending_change
                        .as_mut()
                        .expect("pending insert")
                        .push(semantic);
                    self.pending_change_edited = true;
                }
            }
            "edit.delete_char"
            | "edit.delete_backward"
            | "edit.replace"
            | "edit.join_lines"
            | "edit.toggle_case"
            | "edit.put"
            | "edit.surround"
            | "edit.operator"
            | "edit.repeat_visual"
                if text_changed =>
            {
                self.last_change = vec![semantic];
            }
            _ if text_changed && repeat_override.is_some() => {
                if let Some(pending) = &mut self.pending_change {
                    pending.push(semantic);
                    self.pending_change_edited = true;
                } else {
                    self.last_change = vec![semantic];
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn repeat_plan(
        &mut self,
        limit: usize,
        count: Option<std::num::NonZeroU64>,
    ) -> Result<Vec<CommandInvocation>, ReplayError> {
        if self.last_change.is_empty() {
            return Err(ReplayError::NoChange);
        }
        if self.last_change.len() > limit {
            return Err(ReplayError::RepeatLimit);
        }
        let mut change = self.last_change.clone();
        if let Some(count) = count {
            change[0].count = Some(count);
        }
        let plan = if matches!(change[0].id.as_str(), "mode.insert" | "mode.replace") {
            let repeats = usize::try_from(change[0].count.map_or(1, |count| count.get()))
                .map_err(|_| ReplayError::RepeatLimit)?;
            let opens_line = matches!(change[0].arguments.get("placement"),
                Some(CommandValue::String(placement)) if matches!(placement.as_str(), "open_above" | "open_below"));
            let body = &change[usize::from(!opens_line)..change.len() - 1];
            let steps = body
                .len()
                .checked_mul(repeats)
                .and_then(|steps| steps.checked_add(if opens_line { 1 } else { 2 }))
                .filter(|steps| *steps <= limit)
                .ok_or(ReplayError::RepeatLimit)?;
            let mut plan = Vec::with_capacity(steps);
            if !opens_line {
                let mut start = repeated(change[0].clone());
                start.count = None;
                plan.push(start);
            }
            for _ in 0..repeats {
                plan.extend(body.iter().cloned().map(|invocation| {
                    let mut invocation = repeated(invocation);
                    if invocation.id.as_str() == "mode.insert" {
                        invocation.count = None;
                    }
                    invocation
                }));
            }
            plan.push(repeated(change.last().expect("recorded change").clone()));
            plan
        } else {
            change.iter().cloned().map(repeated).collect()
        };
        self.last_change = change;
        Ok(plan)
    }

    pub(in crate::app) fn start_macro_recording(&mut self, register: char) {
        self.recording_macro = Some((register, Vec::new()));
    }

    pub(in crate::app) fn finish_macro_recording(
        &mut self,
    ) -> Option<(char, Vec<CommandInvocation>)> {
        self.recording_macro.take()
    }

    pub(in crate::app) fn macro_register(&self, requested: char) -> Result<char, ReplayError> {
        if requested == '@' {
            self.last_macro.ok_or(ReplayError::NoMacro)
        } else {
            Ok(requested)
        }
    }

    pub(in crate::app) fn begin_macro_batch(&mut self) {
        if self.replay_depth == 0 {
            self.replay_steps = 0;
        }
    }

    pub(in crate::app) fn begin_macro_replay(
        &mut self,
        register: char,
        limit: usize,
    ) -> Result<(), ReplayError> {
        if self.replay_depth >= limit {
            return Err(ReplayError::RecursionLimit);
        }
        self.last_macro = Some(register);
        self.replay_depth += 1;
        Ok(())
    }

    pub(in crate::app) fn macro_step(
        &mut self,
        invocation: CommandInvocation,
        limit: usize,
    ) -> Result<CommandInvocation, ReplayError> {
        if self.replay_steps >= limit {
            return Err(ReplayError::StepLimit);
        }
        self.replay_steps += 1;
        Ok(replayed(invocation))
    }

    pub(in crate::app) fn finish_macro_replay(&mut self) {
        self.replay_depth = self.replay_depth.saturating_sub(1);
    }

    #[cfg(test)]
    pub(in crate::app) fn last_macro(&self) -> Option<char> {
        self.last_macro
    }

    #[cfg(test)]
    pub(in crate::app) fn replay_depth(&self) -> usize {
        self.replay_depth
    }
}

impl InputState {
    pub(in crate::app) fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
        text: &crate::editor::text::TextBuffer,
        selection: crate::editor::Selection,
        width_policy: crate::editor::text::DisplayWidthPolicy,
    ) -> Option<CommandInvocation> {
        use crate::editor::text::ByteRange;
        use crate::editor::text::display_column_at;
        use crate::editor::text::next_grapheme_boundary;
        use unicode_segmentation::UnicodeSegmentation;
        let (shape, width, height) = match selection.kind {
            crate::editor::SelectionKind::Character => {
                let low = selection.anchor.min(selection.head);
                let high = selection.anchor.max(selection.head);
                let end = next_grapheme_boundary(text, high);
                let first_line = text.byte_to_line(low).ok()?;
                let last_line = text.byte_to_line(high).ok()?;
                let start = if first_line == last_line {
                    low
                } else {
                    text.line_to_byte(last_line).ok()?
                };
                let selected = text.slice(ByteRange::new(start, end).ok()?).ok()?;
                (
                    "character",
                    selected.graphemes(true).count(),
                    last_line - first_line + 1,
                )
            }
            crate::editor::SelectionKind::Line => {
                let anchor = text.byte_to_line(selection.anchor).ok()?;
                let head = text.byte_to_line(selection.head).ok()?;
                ("line", 1, anchor.abs_diff(head).saturating_add(1))
            }
            crate::editor::SelectionKind::Block => {
                let anchor_line = text.byte_to_line(selection.anchor).ok()?;
                let head_line = text.byte_to_line(selection.head).ok()?;
                let anchor_start = text.line_to_byte(anchor_line).ok()?;
                let head_start = text.line_to_byte(head_line).ok()?;
                let anchor_text = text.line(anchor_line).ok()?;
                let head_text = text.line(head_line).ok()?;
                let anchor_column = display_column_at(
                    anchor_text.strip_suffix('\n').unwrap_or(&anchor_text),
                    selection.anchor.get().saturating_sub(anchor_start.get()),
                    width_policy,
                );
                let head_column = display_column_at(
                    head_text.strip_suffix('\n').unwrap_or(&head_text),
                    selection.head.get().saturating_sub(head_start.get()),
                    width_policy,
                );
                (
                    "block",
                    anchor_column.abs_diff(head_column).saturating_add(1),
                    anchor_line.abs_diff(head_line).saturating_add(1),
                )
            }
        };
        let operator = invocation.arguments.get("operator")?.clone();
        let mut repeat = CommandInvocation::new("edit.repeat_visual", invocation.source).ok()?;
        repeat.arguments.insert("operator".into(), operator);
        for name in ["character", "comment"] {
            if let Some(value) = invocation.arguments.get(name) {
                repeat.arguments.insert(name.into(), value.clone());
            }
        }
        repeat
            .arguments
            .insert("shape".into(), CommandValue::String(shape.into()));
        repeat.arguments.insert(
            "width".into(),
            CommandValue::Integer(i64::try_from(width).ok()?),
        );
        repeat.arguments.insert(
            "height".into(),
            CommandValue::Integer(i64::try_from(height).ok()?),
        );
        repeat.register = invocation.register;
        repeat.count = invocation.count;
        Some(repeat)
    }
}

use crate::app::commands::PromptMove;
use crate::editor::ViewId;
use crate::ui::CompletionItem;
use crate::ui::ConfirmationChoice;
use crate::ui::ConfirmationState;
use crate::ui::PromptKind;
use crate::ui::PromptState;

use std::path::Path;
use std::path::PathBuf;

use crate::app::prompt::command_completions;
use crate::app::prompt::takes_path;

impl InputState {
    pub(in crate::app) fn prompt(&self) -> Option<&PromptState> {
        self.prompt.as_ref()
    }
    pub(in crate::app) fn open_prompt(
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
        self.history_draft = None;
        mode
    }
    pub(in crate::app) fn configure_confirmation_actions(
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
    pub(in crate::app) fn select_confirmation(&mut self, selected: ConfirmationChoice) {
        if let Some(confirmation) = self
            .prompt
            .as_mut()
            .and_then(|prompt| prompt.confirmation.as_mut())
        {
            confirmation.selected = selected;
        }
    }
    pub(in crate::app) fn toggle_confirmation(&mut self) {
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
    pub(in crate::app) fn close_prompt(
        &mut self,
        current: &crate::editor::EditorWindowVersion,
    ) -> Option<(ViewId, EditorMode)> {
        if self.prompt.is_some() {
            self.capture_changed();
        }
        self.prompt = None;
        self.history_index = None;
        self.history_draft = None;
        self.cancel_prompt_completions();
        self.previous_mode
            .take()
            .filter(|(source, _, _)| source.same_editor(current))
            .map(|(_, view, mode)| (view, mode))
    }
    pub(in crate::app) fn cancel_prompt_completions(&mut self) {
        self.completion_request_id = self.completion_request_id.saturating_add(1);
    }
    pub(in crate::app) fn prompt_help(&mut self, help: String) {
        if let Some(prompt) = &mut self.prompt {
            prompt.help = Some(help);
        }
    }
    pub(in crate::app) fn prompt_error(&mut self, error: String) {
        if let Some(prompt) = &mut self.prompt {
            prompt.error = Some(error);
        }
    }
    pub(in crate::app) fn command_history(&self) -> &[String] {
        &self.command_history
    }
    pub(in crate::app) fn search_history(&self) -> &[String] {
        &self.search_history
    }
    pub(in crate::app) fn clear_history(&mut self) {
        self.history_cleared = true;
        self.command_history.clear();
        self.search_history.clear();
        self.history_index = None;
        self.history_draft = None;
    }
    pub(in crate::app) fn restore_history(&mut self, commands: Vec<String>, searches: Vec<String>) {
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
            let excess = history
                .len()
                .saturating_sub(crate::app::input::HISTORY_LIMIT);
            history.drain(..excess);
        }
        self.history_index = None;
        self.history_draft = None;
    }
    pub(in crate::app) fn completion_current(&self, key: &PromptCompletionKey) -> bool {
        key.request_id == self.completion_request_id
            && self
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.input == key.prompt_input)
    }
    pub(in crate::app) fn accept_prompt_paths(
        &mut self,
        request: &PromptCompletionKey,
        paths: Vec<PathBuf>,
        root: &Path,
        policy: &crate::config::Config,
    ) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        if request.request_id != self.completion_request_id || prompt.input != request.prompt_input
        {
            return false;
        }
        let prefix = &request.prefix;
        let detail = &policy.command_labels["file.open"];
        let mut candidates = prompt
            .completions
            .iter()
            .map(|item| (item.label.clone(), item.detail.clone()))
            .collect::<BTreeMap<_, _>>();
        for path in paths.into_iter().filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().to_lowercase().starts_with(prefix))
        }) {
            let label = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            candidates.insert(label, detail.clone());
        }
        prompt.completions = candidates
            .into_iter()
            .take(COMPLETION_LIMIT)
            .map(|(label, detail)| CompletionItem {
                icon: None,
                color: None,
                label,
                detail,
            })
            .collect();
        prompt.selected_completion = None;
        true
    }
    pub(in crate::app) fn search_origin(
        &mut self,
        id: crate::editor::ViewId,
        view: &crate::editor::View,
    ) -> crate::editor::text::ByteOffset {
        self.search_origin
            .get_or_insert((
                id,
                view.cursor,
                view.scroll_line,
                view.scroll_wrap_row,
                view.scroll_column,
            ))
            .1
    }
    pub(in crate::app) fn take_search_origin(
        &mut self,
    ) -> Option<(
        crate::editor::ViewId,
        crate::editor::text::ByteOffset,
        usize,
        usize,
        usize,
    )> {
        self.search_origin.take()
    }
    pub(in crate::app) fn set_pending_breakpoint(
        &mut self,
        row: crate::debugger::DapSourceBreakpoint,
    ) {
        self.pending_breakpoint = Some(row);
    }
    pub(in crate::app) fn take_pending_breakpoint(
        &mut self,
    ) -> Option<crate::debugger::DapSourceBreakpoint> {
        self.pending_breakpoint.take()
    }
    pub(in crate::app) fn prompt_insert(&mut self, text: &str) {
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

    pub(in crate::app) fn prompt_backspace(&mut self) {
        self.prompt_delete_back(|before| {
            before
                .grapheme_indices(true)
                .next_back()
                .map_or(before.len(), |(index, _)| index)
        });
    }

    /// Vim's ctrl-w: the word or run of punctuation before the cursor, and the spaces after it.
    pub(in crate::app) fn prompt_delete_word(&mut self) {
        self.prompt_delete_back(|before| {
            let mut graphemes = before.grapheme_indices(true).rev().peekable();
            let whitespace = |text: &str| text.chars().all(char::is_whitespace);
            let word = |text: &str| text.chars().any(|c| c.is_alphanumeric() || c == '_');
            let mut start = before.len();
            while let Some(&(index, text)) = graphemes.peek() {
                if !whitespace(text) {
                    break;
                }
                start = index;
                graphemes.next();
            }
            if let Some(&(_, text)) = graphemes.peek() {
                let in_word = word(text);
                for (index, text) in graphemes {
                    if whitespace(text) || word(text) != in_word {
                        break;
                    }
                    start = index;
                }
            }
            start
        });
    }

    /// Vim's ctrl-u: everything before the cursor.
    pub(in crate::app) fn prompt_delete_to_start(&mut self) {
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

    pub(in crate::app) fn prompt_move(&mut self, movement: PromptMove) {
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

    pub(in crate::app) fn prompt_history(&mut self, direction: i8) {
        let Some(kind) = self.prompt.as_ref().map(|prompt| prompt.kind) else {
            return;
        };
        if matches!(
            kind,
            PromptKind::Text
                | PromptKind::WorkspaceTrust
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
        if current == next {
            return;
        }
        if self.history_index.is_none() {
            self.history_draft = self
                .prompt
                .as_ref()
                .map(|prompt| (prompt.input.clone(), prompt.cursor));
        }
        self.history_index = (next < history.len()).then_some(next);
        let (selected, cursor) = if let Some(index) = self.history_index {
            let selected = history[index].clone();
            let cursor = selected.len();
            (selected, cursor)
        } else {
            self.history_draft.take().unwrap_or_default()
        };
        if let Some(prompt) = &mut self.prompt {
            prompt.input = selected;
            prompt.cursor = cursor;
            prompt.error = None;
            prompt.selected_completion = None;
        }
    }

    pub(in crate::app) fn prompt_complete(&mut self, direction: i8) {
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

    pub(in crate::app) fn accept_completion(&mut self, policy: &crate::config::Config) -> bool {
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

    pub(in crate::app) fn refresh_completions<'a>(
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
                let label = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                if path.to_string_lossy().to_lowercase().contains(&query) {
                    let detail = &policy.command_labels["file.open"];
                    candidates.insert(label, detail.clone());
                }
            }
            self.completion_request_id = self.completion_request_id.saturating_add(1);
            let request_id = self.completion_request_id;
            let entered = PathBuf::from(fragment);
            let trailing_separator = fragment.ends_with(std::path::is_separator);
            let parent = if trailing_separator {
                Some(entered.as_path())
            } else {
                entered.parent().filter(|path| !path.as_os_str().is_empty())
            };
            let prefix = if trailing_separator {
                String::new()
            } else {
                entered
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().to_lowercase())
            };
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
                    request_id,
                    prompt_input: input.clone(),
                    prefix,
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

    pub(in crate::app) fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        if input.is_empty() {
            return;
        }
        if matches!(
            kind,
            PromptKind::Text
                | PromptKind::WorkspaceTrust
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
        let excess = history
            .len()
            .saturating_sub(crate::app::input::HISTORY_LIMIT);
        if excess > 0 {
            history.drain(..excess);
        }
    }
}

use crate::app::documentation::DOCUMENTATION_MAXIMUM_BYTES;
use crate::app::documentation::DOCUMENTATION_MAXIMUM_LINES;
use crate::app::documentation::DOCUMENTATION_MAXIMUM_SECTIONS;
use crate::app::documentation::markdown_rows;
use crate::app::documentation::wrap_prose;
use crate::editor::text::ByteRange;
use crate::ui::DocumentationMarkupKind;
use crate::ui::DocumentationPopupKind;
use crate::ui::DocumentationPopupState;
use crate::ui::DocumentationSection;

impl InputState {
    pub(in crate::app) fn documentation(&self) -> Option<&DocumentationPopupState> {
        self.documentation.as_ref()
    }
    pub(in crate::app) fn close_documentation(&mut self) {
        if self.documentation.is_some() {
            self.capture_changed();
        }
        self.documentation = None;
    }
    pub(in crate::app) fn scroll_documentation(
        &mut self,
        direction: isize,
        step: usize,
        visible_rows: usize,
        total_rows: usize,
    ) {
        if let Some(popup) = &mut self.documentation {
            let maximum = total_rows.saturating_sub(visible_rows.max(1));
            let scroll = popup.scroll.min(maximum);
            popup.scroll = if direction < 0 {
                scroll.saturating_sub(step)
            } else {
                scroll.saturating_add(step).min(maximum)
            };
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn present_documentation(
        &mut self,
        kind: DocumentationPopupKind,
        title: String,
        provider: String,
        source_range: Option<ByteRange>,
        source_sections: impl IntoIterator<Item = (DocumentationMarkupKind, String)>,
        config: &crate::config::Config,
        text_width: usize,
    ) {
        let policy = &config.ui.documentation_popup;
        self.capture_changed();
        if !policy.enabled {
            self.documentation = None;
            return;
        }
        let mut remaining_lines = DOCUMENTATION_MAXIMUM_LINES;
        let mut remaining_bytes = DOCUMENTATION_MAXIMUM_BYTES;
        let mut sections = Vec::new();
        for (markup, value) in source_sections
            .into_iter()
            .take(DOCUMENTATION_MAXIMUM_SECTIONS)
        {
            if remaining_lines == 0 || remaining_bytes == 0 {
                break;
            }
            let mut end = value.len().min(remaining_bytes);
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            let normalized = value[..end]
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .take(remaining_lines)
                .collect::<Vec<_>>()
                .join("\n");
            let mut lines = Vec::new();
            let mut spans = Vec::new();
            let markdown = markup == DocumentationMarkupKind::Markdown;
            let source_rows = if markdown {
                markdown_rows(&normalized, text_width, config.editor.ambiguous_width)
            } else {
                normalized
                    .split('\n')
                    .map(|line| (line.to_owned(), Vec::new()))
                    .collect()
            };
            for (line, line_spans) in source_rows {
                if remaining_lines == 0 || remaining_bytes == 0 {
                    break;
                }
                let sanitized = line
                    .chars()
                    .filter(|character| !character.is_control() || *character == '\t')
                    .take(8192)
                    .collect::<String>();
                let take = sanitized.len().min(remaining_bytes);
                let mut end = take;
                while !sanitized.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                remaining_bytes = remaining_bytes.saturating_sub(end);
                // Markdown is laid out already and diffs keep their lines; plain prose fits the
                // popup.
                let rows = if kind == DocumentationPopupKind::Git {
                    let mut rows = Vec::new();
                    let mut row = String::new();
                    let mut used = 0;
                    for grapheme in sanitized[..end].graphemes(true) {
                        let cells = crate::ui::geometry::text_width(
                            grapheme,
                            config.editor.ambiguous_width,
                        );
                        if !row.is_empty() && used + cells > text_width.max(1) {
                            rows.push(std::mem::take(&mut row));
                            used = 0;
                        }
                        row.push_str(grapheme);
                        used += cells;
                    }
                    rows.push(row);
                    rows
                } else if !markdown {
                    wrap_prose(&sanitized[..end], text_width)
                } else {
                    vec![sanitized[..end].to_owned()]
                };
                let line_spans = line_spans
                    .into_iter()
                    .filter(|(range, _)| range.end <= end)
                    .collect::<Vec<_>>();
                for row in rows.into_iter().take(remaining_lines) {
                    lines.push(row);
                    spans.push(line_spans.clone());
                    remaining_lines = remaining_lines.saturating_sub(1);
                }
            }
            if !lines.iter().all(|line| line.trim().is_empty()) {
                sections.push(DocumentationSection {
                    heading: None,
                    markup,
                    lines,
                    spans,
                });
            }
        }
        if sections.is_empty() {
            self.documentation = None;
            return;
        }
        self.documentation = Some(DocumentationPopupState {
            kind,
            title,
            provider: provider.chars().take(256).collect(),
            source_range,
            sections,
            scroll: 0,
        });
    }
}

#[derive(Clone, Debug)]
pub(in crate::app) struct PromptCompletionKey {
    request_id: u64,
    prompt_input: String,
    pub(in crate::app) prefix: String,
}

pub(in crate::app) struct PromptCompletionRequest {
    pub(in crate::app) key: PromptCompletionKey,
    pub(in crate::app) directory: PathBuf,
    pub(in crate::app) limit: usize,
}

use unicode_segmentation::UnicodeSegmentation;

mod commands;

#[cfg(test)]
mod replay_tests;

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::terminal::TerminalInput;
use crate::app::windows::NativeSurfaceTarget;
use crate::editor::TransactionId;
use crate::ui::VisualRowModel;
use crate::ui::editor_pane_viewport_at;
use crate::ui::editor_state_content_width;
use crate::ui::editor_state_gutter_width;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::process::PtyId;

use crate::ui::KeyInputKind;

use crate::app::command;
use crate::app::mode_name;
use crate::app::windows::rect_contains_point;
use crate::ui::TerminalPanelMode;
use crate::ui::UiEvent;
use crate::ui::terminal_tab_at_column;
use crate::windows::PANE_MINIMUM_HEIGHT;
use crate::windows::PANE_MINIMUM_WIDTH;

impl App {
    pub(in crate::app) fn mouse_pane_geometry(
        &self,
    ) -> Option<(crate::windows::WindowId, u16, u16, u16, u16)> {
        let (global_column, global_row) = self.input.mouse_position()?;
        editor_pane_viewport_at(&self.state, &self.ui, global_column, global_row).map(|viewport| {
            (
                viewport.pane,
                global_column.saturating_sub(viewport.x),
                global_row.saturating_sub(viewport.y),
                viewport.width,
                viewport.height,
            )
        })
    }
}

impl App {
    pub(in crate::app) fn focus_mouse_pane(&mut self) {
        self.reconcile_editor_windows();
        let Some((pane, ..)) = self.mouse_pane_geometry() else {
            return;
        };
        let Some(window) = self.state.graph.window(pane).map(|window| window.id) else {
            return;
        };
        let result = self
            .window_session
            .plan_focus(
                window,
                &self.state.window_version(),
                self.extension.contributions().version(),
            )
            .and_then(|plan| self.apply_window_plan(plan));
        if let Err(error) = result {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    pub(in crate::app) fn apply_mouse_cursor(&mut self, extend_selection: bool) {
        self.reconcile_editor_windows();
        let Some((pane_id, column, row, pane_width, pane_height)) = self.mouse_pane_geometry()
        else {
            return;
        };
        let Some(view_id) = self.state.window_view(pane_id) else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let body_height = pane_height;
        if row >= body_height {
            return;
        }
        let gutter = editor_state_gutter_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        if available == 0 {
            return;
        }
        let wrap_enabled = self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&buffer_id));
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            pane_width,
            wrap_enabled,
        );
        let source = self.state.version();
        let transaction_id = TransactionId(self.next_transaction);
        let plan = self.input.mouse_plan(
            crate::app::input::MouseDocument {
                editor: &source,
                view_id,
                pane_id,
                view,
                text: &buffer.text,
            },
            crate::app::input::MouseGeometry {
                column,
                row,
                body_height,
                gutter,
                rows: &visual_rows,
            },
            transaction_id,
            extend_selection,
        );
        let Some(plan) = plan else {
            return;
        };
        self.diagnostics.dismiss_expansion();
        self.ui.expanded_diagnostic = None;
        self.transaction_id();
        let transaction = match self.input.mouse_transaction(&plan, &self.state.version()) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_authoritative_editor_transaction(transaction) {
            self.show_error(error);
        } else {
            let source = self.state.window_version();
            let current = self.state.active_view().and_then(|id| {
                self.state.views.get(id).and_then(|view| {
                    self.state
                        .buffers
                        .get(view.buffer)
                        .map(|buffer| (id, view.buffer, buffer.text.revision()))
                })
            });
            self.input.accept_mouse_plan(plan, &source, current);
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    /// Dragging a docked terminal's inner edge resizes it: the border left of a right-hand
    /// terminal, or the top row of the bottom panel.
    fn handle_dock_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        let docked = |this: &Self, id: PtyId| {
            let rect = this.terminal_rect(id)?;
            match this
                .current_native_placement(NativeSurfaceTarget::Terminal(id.0))
                .ok()?
            {
                WindowPlacement::Docked { edge, .. } => Some((edge, rect)),
                _ => None,
            }
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::LeftDown) => {
                let edge_at = self
                    .shown_terminal_windows()
                    .into_iter()
                    .find_map(|(id, _)| {
                        let (edge, rect) = docked(self, id)?;
                        let on_edge = match edge {
                            DockEdge::Right => column == rect.x,
                            DockEdge::Left => column + 1 == rect.x.saturating_add(rect.width),
                            DockEdge::Bottom => row == rect.y,
                            DockEdge::Top => row + 1 == rect.y.saturating_add(rect.height),
                        } && rect_contains_point(rect, column, row);
                        on_edge.then_some((id, edge))
                    });
                self.dock_drag = edge_at.map(|(id, _)| id);
                // The bottom panel's top row holds its tabs, so a click there still picks one.
                edge_at.is_some_and(|(_, edge)| matches!(edge, DockEdge::Left | DockEdge::Right))
            }
            Some(TerminalMouseInput::LeftDrag) => {
                let Some(id) = self.dock_drag else {
                    return false;
                };
                let Some((edge, rect)) = docked(self, id) else {
                    self.dock_drag = None;
                    return false;
                };
                let (width, height) = (self.window_session.width(), self.window_session.height());
                let size = match edge {
                    DockEdge::Right => rect.x.saturating_add(rect.width).saturating_sub(column),
                    DockEdge::Left => column.saturating_add(1).saturating_sub(rect.x),
                    DockEdge::Bottom => rect.y.saturating_add(rect.height).saturating_sub(row),
                    DockEdge::Top => row.saturating_add(1).saturating_sub(rect.y),
                };
                let size = match edge {
                    DockEdge::Left | DockEdge::Right => {
                        size.clamp(20, width.saturating_sub(PANE_MINIMUM_WIDTH).max(20))
                    }
                    DockEdge::Top | DockEdge::Bottom => size.clamp(
                        crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT,
                        height.saturating_sub(PANE_MINIMUM_HEIGHT).max(4),
                    ),
                };
                if let Err(error) = self.apply_native_window_placement(
                    NativeSurfaceTarget::Terminal(id.0),
                    WindowPlacement::Docked { edge, size },
                ) {
                    self.show_error(error);
                }
                true
            }
            Some(TerminalMouseInput::LeftUp) => self.dock_drag.take().is_some(),
            _ => false,
        }
    }
}

impl App {
    /// A click on a terminal shown beside the active one, such as a docked agent, gives it the
    /// keys; other mouse input over it goes nowhere.
    fn handle_other_terminal_mouse(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        if self.ui.picker.is_some() || self.input.prompt().is_some() {
            return false;
        }
        let active = self.displayed_terminal();
        let Some(id) = self
            .compositor_projection()
            .and_then(|projection| projection.hit_test(column, row))
            .and_then(|window| self.window_session.native_ids().target_for_window(window))
            .and_then(|target| match target {
                NativeSurfaceTarget::Terminal(id) => Some(PtyId(id)),
                _ => None,
            })
            .filter(|id| Some(*id) != active)
        else {
            return false;
        };
        if key.terminal_mouse() == Some(crate::editor::input::TerminalMouseInput::LeftDown) {
            self.process_effect(AppEffect::TerminalFocus(id.0));
        }
        true
    }
}

impl App {
    /// Dragging the border between two editor windows resizes them, as in Neovim.
    fn handle_border_drag(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        use crate::editor::input::TerminalMouseInput;
        use crate::windows::TileAxis;
        let Some(projection) = self.compositor_projection() else {
            return false;
        };
        let pane_at = |x: u16, y: u16| {
            projection
                .windows
                .iter()
                .filter(|window| rect_contains_point(window.rect, x, y))
                .find_map(|window| self.state.window_view(window.window).map(|_| window.window))
        };
        match key.terminal_mouse() {
            Some(TerminalMouseInput::LeftDown) => {
                let sides = projection
                    .borders
                    .iter()
                    .find_map(|border| match border.axis {
                        TileAxis::Horizontal
                            if column == border.x
                                && (border.y..border.y.saturating_add(border.length))
                                    .contains(&row) =>
                        {
                            Some((
                                pane_at(column.wrapping_sub(1), row)?,
                                pane_at(column + 1, row)?,
                                true,
                            ))
                        }
                        TileAxis::Vertical
                            if row == border.y
                                && (border.x..border.x.saturating_add(border.length))
                                    .contains(&column) =>
                        {
                            Some((
                                pane_at(column, row.wrapping_sub(1))?,
                                pane_at(column, row + 1)?,
                                false,
                            ))
                        }
                        _ => None,
                    });
                self.border_drag = sides;
                sides.is_some()
            }
            Some(TerminalMouseInput::LeftDrag) => {
                let Some((first, second, side_by_side)) = self.border_drag else {
                    return false;
                };
                let Some(panes) = self.state.panes_of_split_between(first, second) else {
                    self.border_drag = None;
                    return false;
                };
                // Measure the split from the windows under it, then put the border at the mouse.
                let rects: Vec<WindowRect> = projection
                    .windows
                    .iter()
                    .filter(|window| {
                        self.state
                            .window_view(window.window)
                            .map(|_| window.window)
                            .is_some_and(|pane| panes.contains(&pane))
                    })
                    .map(|window| window.rect)
                    .collect();
                let (start, end, at) = if side_by_side {
                    (
                        rects.iter().map(|rect| rect.x).min().unwrap_or(0),
                        rects
                            .iter()
                            .map(|rect| rect.x + rect.width)
                            .max()
                            .unwrap_or(0),
                        column,
                    )
                } else {
                    (
                        rects.iter().map(|rect| rect.y).min().unwrap_or(0),
                        rects
                            .iter()
                            .map(|rect| rect.y + rect.height)
                            .max()
                            .unwrap_or(0),
                        row,
                    )
                };
                let available = u32::from(end.saturating_sub(start).saturating_sub(1)).max(1);
                let ratio = u32::from(at.saturating_sub(start)) * 1_000 / available;
                if self
                    .state
                    .set_split_ratio_between(first, second, u16::try_from(ratio).unwrap_or(900))
                    .is_ok()
                {
                    self.reconcile_editor_windows();
                }
                true
            }
            Some(TerminalMouseInput::LeftUp) => self.border_drag.take().is_some(),
            _ => false,
        }
    }
}

impl App {
    fn handle_debug_tab_mouse(&mut self, key: &KeyEvent, column: u16, row: u16) -> bool {
        if key.terminal_mouse() != Some(crate::editor::input::TerminalMouseInput::LeftDown)
            || self.input.prompt().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
            || self.ui.dashboard
            || self.state.active_view().is_none()
        {
            return false;
        }
        let (Some(bounds), Some(selected)) = (
            self.workspace_bounds(),
            self.ui.debug_panes.as_ref().map(|panes| panes.selected),
        ) else {
            return false;
        };
        let top =
            crate::ui::workspace_top_rows(&self.state, &self.ui, self.configuration.settings());
        let region = crate::windows::WindowRect {
            y: bounds.y.saturating_add(top),
            height: bounds.height.saturating_sub(top),
            ..bounds
        };
        let Some(pane) = crate::ui::debug_pane_tab_at(region, selected, column, row) else {
            return false;
        };
        if let Some(panes) = self.ui.debug_panes.as_mut() {
            panes.selected = pane;
        }
        true
    }
}

impl App {
    pub(in crate::app) fn handle_content_tab_mouse(
        &mut self,
        key: &KeyEvent,
        column: u16,
        row: u16,
    ) -> bool {
        if self.input.prompt().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
        {
            return false;
        }
        let Some(kind) = key.terminal_mouse() else {
            return false;
        };
        if kind == crate::editor::input::TerminalMouseInput::LeftDrag
            && let Some(last) = self.tab_drag
        {
            self.drag_content_tab(last, column);
            return true;
        }
        self.tab_drag = None;
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
        let tab_line = self.state.editor_tabs().count() > 1;
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
        let Some(buffer) = self.state.buffer_for_content(identity.0) else {
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
        if kind == crate::editor::input::TerminalMouseInput::LeftDown {
            self.tab_drag = Some(column);
        }
        if kind == crate::editor::input::TerminalMouseInput::RightDown {
            if entry.is_some_and(|entry| entry.closeable) {
                self.close_active_buffer(false)
            } else {
                self.show_error("content-line entry is not closeable")
            }
        }
        true
    }
}

impl App {
    /// A dragged tab moves past the tab under the mouse. Only when the mouse is moving that
    /// way, because swapping tabs of different widths can leave it over the same tab.
    fn drag_content_tab(&mut self, last: u16, column: u16) {
        self.tab_drag = Some(column);
        let Some(bounds) = self.workspace_bounds() else {
            return;
        };
        let Some(over) = crate::ui::content_tab_at_column(
            &self.ui.content_tabs,
            self.configuration.settings(),
            bounds.width,
            column.saturating_sub(bounds.x),
        ) else {
            return;
        };
        let active = self.state.active_buffer_id();
        let documents = &self.state;
        let tabs = &self.ui.content_tabs;
        let active = tabs
            .iter()
            .position(|entry| documents.buffer_for_content(entry.identity.0) == active);
        let over = tabs.iter().position(|entry| entry.identity == over);
        let (Some(active), Some(over)) = (active, over) else {
            return;
        };
        if over > active && column > last {
            self.move_active_buffer_line(1);
        } else if over < active && column < last {
            self.move_active_buffer_line(-1);
        }
    }
}

impl App {
    pub(in crate::app) fn handle_event(&mut self, event: UiEvent) {
        if self.input.command_pending() && !matches!(event, UiEvent::Resize(_, _)) {
            self.input
                .held_input
                .push_back(crate::app::editing::clipboard::DeferredInput::Event(event));
            return;
        }
        if self.clipboard.waiting() && !matches!(event, UiEvent::Resize(_, _)) {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Event(event));
            return;
        }
        if matches!(event, UiEvent::Key { .. }) {
            self.last_key_at = Some(Instant::now());
        }
        // Information has been read once the user moves on, and must not cover what comes next.
        // Errors and warnings keep their time, and everything stays in `:messages`.
        if matches!(
            event,
            UiEvent::Key {
                kind: KeyInputKind::Press | KeyInputKind::Repeat,
                ..
            }
        ) && self.ui.notification.is_some()
            && !self.ui.notification_error
            && !self.ui.notification_warning
        {
            self.clear_notice();
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
                self.ensure_cursor_visible();
            }
            UiEvent::Key { key, kind } => {
                self.sync_flash_ui();
                match kind {
                    KeyInputKind::Press | KeyInputKind::Repeat
                        if self.record_shortcut_key(&key) => {}
                    KeyInputKind::Press | KeyInputKind::Repeat if self.flash.is_some() => {
                        self.handle_flash_key(&key);
                    }
                    KeyInputKind::Press | KeyInputKind::Repeat => self.handle_configured_key(key),
                    KeyInputKind::Release => self.handle_key_in_context("key_release", key),
                }
            }
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
                if self.ui.settings.is_some()
                    && self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                {
                    self.settings_mouse(&key, column, row);
                    return;
                }
                let overlay = if self.ui.picker.is_some() {
                    Some("picker_mouse")
                } else if self.input.prompt().is_some() {
                    Some("prompt_mouse")
                } else if self.ui.tool_menu.is_some() {
                    Some("tool_manager_mouse")
                } else {
                    None
                };
                if let Some(context) = overlay {
                    self.handle_key_in_context(context, key);
                    return;
                }
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
                let over_floating = self.compositor_projection().is_some_and(|projection| {
                    projection
                        .hit_test(column, row)
                        .and_then(|window| projection.window(window))
                        .is_some_and(|window| {
                            matches!(
                                window.layer,
                                crate::windows::CompositorLayer::Floating
                                    | crate::windows::CompositorLayer::Fullscreen
                            )
                        })
                });
                if (!over_floating
                    && (self.handle_chrome_mouse(&key, column, row)
                        || self.handle_debug_tab_mouse(&key, column, row)
                        || self.handle_content_tab_mouse(&key, column, row)
                        || self.handle_dock_drag(&key, column, row)
                        || self.handle_border_drag(&key, column, row)))
                    || self.handle_other_terminal_mouse(&key, column, row)
                {
                    return;
                }
                let hit = self
                    .compositor_projection()
                    .and_then(|projection| projection.hit_test(column, row));
                let over_terminal = hit.is_some_and(|window| {
                    matches!(
                        self.window_session.native_ids().target_for_window(window),
                        Some(NativeSurfaceTarget::Terminal(_))
                    )
                });
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
                        (!over_floating
                            && !over_terminal
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
                // Only a press moves focus. Terminals also report releases and plain pointer
                // motion, and those must not take the keyboard away from the user.
                let pressed = matches!(
                    key.code,
                    crate::editor::input::KeyCode::MouseLeftDown
                        | crate::editor::input::KeyCode::MouseRightDown
                        | crate::editor::input::KeyCode::MouseMiddleDown
                );
                if let Some(window) = explorer_window.filter(|_| pressed) {
                    // The click picks this row once focus has moved.
                    self.explorer
                        .set_clicked(self.explorer_row_at(window, column, row));
                    if let Ok(plan) = self.window_session.plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    ) && self.apply_window_plan(plan).is_ok()
                    {
                        self.reconcile_editor_windows();
                    }
                }
                if over_terminal
                    && key.terminal_mouse()
                        == Some(crate::editor::input::TerminalMouseInput::LeftDown)
                    && let Some(layout) = self.window_session.layout()
                    && let Some((_, window)) = layout.surfaces.terminal
                    && let Some(window) = self
                        .compositor_projection()
                        .and_then(|projection| projection.window(window))
                    && let Some(geometry) = crate::ui::terminal_window_geometry(window)
                    && row == geometry.top
                    && let Some(id) = self.ui.terminal_panel.as_ref().and_then(|terminal| {
                        terminal_tab_at_column(
                            terminal,
                            self.configuration.settings(),
                            geometry,
                            column,
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
                if !over_terminal
                    && !over_explorer
                    && self.ui.picker.is_none()
                    && self.input.prompt().is_none()
                    && self.agent_panel_mouse(&key, column, row)
                {
                    return;
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
                        .filter(|_| pressed)
                        && self
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
                    "explorer_mouse"
                } else if self.window_session.dashboard_visible()
                    || self.state.active_view().is_none()
                {
                    "dashboard_mouse"
                } else {
                    if pressed {
                        self.set_explorer_focus(false);
                    }
                    "editor_mouse"
                };
                if context == "terminal_mouse"
                    && self.window_session.terminal_focused()
                    && let Some(id) = self.keyboard_terminal()
                    && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
                    && let Some(window) = self
                        .window_session
                        .native_ids()
                        .terminal(id.0)
                        .and_then(|ids| self.compositor_projection()?.window(ids.window))
                    && let Some(geometry) = crate::ui::terminal_window_geometry(window)
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
                        TerminalInput::Mouse(event, key.modifiers, column, row, rect),
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
}

impl App {
    pub(in crate::app) fn keyboard_terminal(&self) -> Option<PtyId> {
        if self.input.capture_context().is_some()
            || self.workspace_edits.proposal().is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
            || self.ui.settings.is_some()
        {
            return None;
        }
        self.window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
    }
}

impl App {
    pub(in crate::app) fn active_input_context(&self) -> String {
        if self.search.help_is_open() {
            return if self.input.picker_normal() {
                "picker_normal"
            } else {
                "picker"
            }
            .into();
        }
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
        if let Some(menu) = &self.ui.tool_menu {
            return if menu.searching {
                "tool_manager_search"
            } else {
                "tool_manager"
            }
            .into();
        }
        if let Some(page) = &self.ui.settings {
            if page.setup.is_some() {
                return "setup".into();
            }
            return match &page.edit {
                Some(crate::ui::SettingsEdit::Text { .. }) => "settings_edit",
                Some(crate::ui::SettingsEdit::Choice { .. }) => "settings_choice",
                Some(crate::ui::SettingsEdit::Keys(_)) => "settings",
                None if page.searching => "settings_search",
                None => "settings",
            }
            .into();
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
            return if self.explorer.searching() {
                "explorer_search"
            } else {
                "explorer"
            }
            .into();
        }
        if let Some(context) = self
            .window_session
            .graph()
            .focused()
            .and_then(|window| self.extension.window_context(window))
        {
            return context;
        }
        if !self.window_session.dashboard_visible() && self.agent_input_focused() {
            return if self.agent_normal_mode() {
                "agent_normal"
            } else {
                "agent"
            }
            .into();
        }
        if self.window_session.dashboard_visible() || self.state.active_view().is_none() {
            return "dashboard".into();
        }
        let mode = self
            .state
            .active_view()
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        if mode == EditorMode::Insert
            && self.input.snippet_buffer() == self.state.active_buffer_id()
            && self.input.snippet_buffer().is_some()
            && self.input.pending_context().is_none()
            && self
                .input
                .mapping_context()
                .is_none_or(|context| context == "snippet")
        {
            return "snippet".into();
        }
        format!("editor.{}", mode_name(mode))
    }
}

impl App {
    fn send_terminal_focus_report(&mut self, focused: bool) {
        if let Some(id) = self.keyboard_terminal()
            && let Err(error) =
                self.terminal_session
                    .send(id, TerminalInput::Focus(focused), &self.workspace)
        {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn handle_configured_key(&mut self, key: KeyEvent) {
        if self.input_replacement_pending() || self.select_replacement_pending() {
            if key.code == crate::editor::input::KeyCode::Escape {
                if self.input_replacement_pending() {
                    self.close_prompt();
                }
                if self.select_replacement_pending() {
                    self.close_picker();
                }
            }
            return;
        }
        if self.input.command_pending() {
            self.input
                .held_input
                .push_back(crate::app::editing::clipboard::DeferredInput::Event(
                    UiEvent::Key {
                        key,
                        kind: KeyInputKind::Press,
                    },
                ));
            return;
        }
        self.reconcile_named_mode();
        if self.clipboard.waiting() {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Event(
                    UiEvent::Key {
                        key,
                        kind: KeyInputKind::Press,
                    },
                ));
            return;
        }
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }
}

impl App {
    pub(in crate::app) fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
        let scrolls_completion_documentation = context == "completion"
            && key.modifiers.control
            && matches!(key.code, crate::editor::input::KeyCode::Char('d' | 'u'));
        if context != "documentation" && !scrolls_completion_documentation {
            self.close_documentation();
        }
        self.ui.lsp_loading = None;
        self.lsp_asked = None;
        self.lsp_asked_request_id = None;
        self.lsp_waiting = None;
        self.lsp_waiting_server = None;
        self.lsp_retry_at = None;
        self.lsp_retries = 0;
        if key.terminal_mouse().is_none() {
            self.diagnostics.dismiss_expansion();
            self.ui.expanded_diagnostic = None;
        }
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
            // A key that finishes no sequence ends it, as in Vim: `]q` must not go on to run `q`,
            // and `<leader>gz` must not start an operator. While typing, all the keys are text,
            // so `ja` with `jk` mapped types `ja`.
            MappingResolution::Replay(prefix, replay) => {
                if context.starts_with("editor.")
                    && !self.typing()
                    && !self
                        .input
                        .named_mode
                        .as_ref()
                        .is_some_and(|(_, _, mode)| mode.base().is_none())
                {
                    self.input.cancel_pending();
                } else {
                    self.type_unmapped(&context, &prefix);
                    for key in replay {
                        self.handle_configured_key(key);
                    }
                }
            }
            MappingResolution::DisabledAndReplay(replay) => {
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Disabled => {}
            MappingResolution::Pending | MappingResolution::Ambiguous => return,
            MappingResolution::Missing if context == "snippet" => {
                self.handle_key_in_context("editor.insert", key);
                return;
            }
            MappingResolution::Missing => {
                if !self.type_unmapped(&context, std::slice::from_ref(&key))
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
}

impl App {
    fn typing(&self) -> bool {
        self.state.active_view().is_some_and(|view| {
            matches!(
                self.state.views[view].mode,
                EditorMode::Insert | EditorMode::Replace
            )
        })
    }
}

impl App {
    /// Types `keys` as text where `context` takes text, without looking them up again.
    /// Returns false when the context takes no text or a key has none.
    fn type_unmapped(&mut self, context: &str, keys: &[KeyEvent]) -> bool {
        if context.starts_with("editor.")
            && self
                .input
                .named_mode
                .as_ref()
                .is_some_and(|(_, _, mode)| mode.base().is_none())
        {
            for key in keys {
                if let Some(invocation) = self
                    .input
                    .named_mode
                    .as_ref()
                    .and_then(|(_, _, mode)| mode.capture(key))
                {
                    self.invoke_mapped(invocation);
                }
            }
            return true;
        }
        let input = &self.configuration.settings().input;
        let context = if context == "snippet" {
            "editor.insert"
        } else {
            context
        };
        let Some(command_id) = input
            .character_fallback_commands
            .get(context)
            .or_else(|| input.text_commands.get(context))
            .cloned()
        else {
            return false;
        };
        let mut typed = true;
        for key in keys {
            match key.text_character() {
                Some(character) => self.invoke_text_command(&command_id, character.to_string()),
                None => typed = false,
            }
        }
        typed
    }
}

impl App {
    pub(in crate::app) fn handle_configured_paste(&mut self, text: String) {
        if self.input_replacement_pending() || self.select_replacement_pending() {
            return;
        }
        if self.input.command_pending() {
            self.input
                .held_input
                .push_back(crate::app::editing::clipboard::DeferredInput::Event(
                    UiEvent::Paste(text),
                ));
            return;
        }
        self.close_documentation();
        if self.window_session.terminal_focused()
            && let Some(id) = self.keyboard_terminal()
            && self.terminal_session.mode(id) == Some(TerminalPanelMode::Input)
        {
            match self
                .terminal_session
                .send(id, TerminalInput::Paste(&text), &self.workspace)
            {
                Ok(true) => {}
                Ok(false) => self.show_error("terminal paste exceeds the input limit"),
                Err(error) => self.show_error(error),
            }
            return;
        }
        let context = self.active_input_context();
        if self.typing()
            && matches!(
                context.as_str(),
                "editor.insert" | "editor.replace" | "snippet"
            )
        {
            match self
                .input
                .finish_mapping(&context, self.configuration.binding_revision())
            {
                MappingResolution::Ready(invocation) => self.invoke_mapped(invocation),
                MappingResolution::ReadyAndReplay(invocation, replay) => {
                    self.invoke_mapped(invocation);
                    for key in replay {
                        self.handle_configured_key(key);
                    }
                }
                MappingResolution::Replay(prefix, replay) => {
                    self.type_unmapped(&context, &prefix);
                    self.type_unmapped(&context, &replay);
                }
                _ => {}
            }
        }
        self.cancel_pending_mapping();
        let context = self.active_input_context();
        let context = if context == "snippet" {
            "editor.insert"
        } else {
            &context
        };
        if let Some(command_id) = self
            .configuration
            .settings()
            .input
            .paste_commands
            .get(context)
            .cloned()
        {
            self.invoke_text_command(&command_id, text);
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    pub(in crate::app) fn invoke_mapped_command(&mut self, command_id: &str) {
        match command(command_id) {
            Ok(invocation) => self.invoke_mapped(invocation),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn invoke_text_command(&mut self, command_id: &str, text: String) {
        match command(command_id) {
            Ok(invocation) => {
                self.invoke(invocation.with_argument("text", CommandValue::String(text)))
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn invoke_insert_text(&mut self, text: String) {
        self.invoke_text_command("edit.insert_text", text);
    }
}

impl App {
    pub(in crate::app) fn invoke_mapped(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        self.invoke(invocation);
    }
}

impl App {
    pub(in crate::app) fn cancel_pending_mapping(&mut self) {
        self.input.cancel_mapping();
        self.project_input_hints();
    }
}

impl App {
    pub(in crate::app) fn flush_keymap_timeout(&mut self) {
        let cancelled: Vec<_> = self
            .input
            .pending_commands
            .iter()
            .filter(|(id, pending)| pending.wrapper && !self.commands.is_pending(**id))
            .map(|(id, _)| *id)
            .collect();
        for id in cancelled {
            self.cancel_plugin_pending_command(id);
        }
        if self.input.command_pending() || self.clipboard.waiting() {
            return;
        }
        let context = self.active_input_context();
        let resolution = self.input.flush_timeout(
            &context,
            self.configuration.binding_revision(),
            self.clock.monotonic(),
        );
        self.project_input_hints();
        match resolution {
            MappingResolution::Ready(invocation) => self.invoke_mapped(invocation),
            MappingResolution::DisabledAndReplay(replay) => {
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            // A lone `j` with `jk` mapped is typed once the wait is over, as in Vim.
            MappingResolution::Replay(prefix, _)
                if !context.starts_with("editor.") || self.typing() =>
            {
                self.type_unmapped(&context, &prefix);
            }
            _ => return,
        }
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn update_key_hint_ui(&mut self) {
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
}

impl App {
    pub(in crate::app) fn restore_input_focus(&mut self) {
        let Some(capture) = self.input.released_focus() else {
            return;
        };
        self.reconcile_editor_windows();
        if let Some(plan) = self.window_session.restore_focus_plan(
            capture,
            &self.state.window_version(),
            self.extension.contributions().version(),
        ) {
            let _ = self.apply_window_plan(plan);
        }
        self.project_window_activation();
    }
}

impl App {
    pub(in crate::app) fn project_input_hints(&mut self) {
        let hints = self.input.hints().cloned();
        self.ui.set(hints, |ui| &mut ui.key_hints);
    }
}

use crate::windows::DockEdge;

/// Native input operations; ABI adapters use these same effects.
#[derive(Clone, Debug)]
pub enum InputCommand {
    EnterMode(Box<NamedMode>),
    LeaveMode,
    PushLayer(Box<Keymap>),
    PopLayer,
    PushNamedLayer {
        owner: String,
        name: String,
        layer: Box<Keymap>,
    },
    RemoveNamedLayer {
        owner: String,
        name: String,
    },
    /// The command's eventual result must complete this id, including on failure or timeout.
    Wait(crate::editor::PendingCommandId),
    Complete {
        id: crate::editor::PendingCommandId,
        succeeded: bool,
        text_changed: bool,
    },
}

struct PendingCommand {
    wrapper: bool,
    invocation: CommandInvocation,
    repeat: Option<CommandInvocation>,
    suppress_repeat: bool,
    text_changed: bool,
}

impl InputState {
    pub(in crate::app) fn command_pending(&self) -> bool {
        self.pending_input.is_some()
    }

    pub(in crate::app) fn hold_invocation(&mut self, invocation: &CommandInvocation) -> bool {
        if !self.command_pending() {
            return false;
        }
        self.held_input
            .push_back(crate::app::editing::clipboard::DeferredInput::Invocation(
                invocation.clone(),
            ));
        true
    }

    pub(in crate::app) fn pending_input_command(&self) -> Option<crate::editor::PendingCommandId> {
        self.pending_input
    }

    pub(in crate::app) fn pending_invocation(
        &self,
        id: crate::editor::PendingCommandId,
    ) -> Option<crate::app::commands::PendingInvocation> {
        let pending = self
            .pending_commands
            .get(&id)
            .filter(|pending| pending.wrapper)?;
        Some(crate::app::commands::PendingInvocation {
            invocation: pending.invocation.clone(),
            repeat_override: pending.repeat.clone(),
            suppress_repeat: pending.suppress_repeat,
        })
    }

    pub(in crate::app) fn suspend_command(
        &mut self,
        id: crate::editor::PendingCommandId,
        invocation: crate::app::commands::PendingInvocation,
    ) {
        if self.pending_commands.contains_key(&id) {
            return;
        }
        // Incoming server handlers can wait without stopping typing. Nested plugin calls
        // share the outer command's input hold, but retain their own continuation.
        if !invocation
            .invocation
            .id
            .as_str()
            .starts_with("lsp.incoming.")
        {
            self.pending_input.get_or_insert(id);
        }
        self.pending_commands.insert(
            id,
            PendingCommand {
                wrapper: true,
                repeat: Some(
                    invocation
                        .repeat_override
                        .unwrap_or_else(|| invocation.invocation.clone()),
                ),
                invocation: invocation.invocation,
                suppress_repeat: invocation.suppress_repeat,
                text_changed: false,
            },
        );
    }

    pub(in crate::app) fn incoming_command_pending(
        &self,
        event: i64,
        commands: &crate::editor::CommandRegistry<AppEffect>,
    ) -> bool {
        self.pending_commands.iter().any(|(id, pending)| {
            pending.wrapper
                && commands.is_pending(*id)
                && pending.invocation.id.as_str().starts_with("lsp.incoming.")
                && pending.invocation.arguments.get("event")
                    == Some(&crate::editor::CommandValue::Integer(event))
        })
    }

    pub(in crate::app) fn wait_for_effects(
        &mut self,
        invocation: &CommandInvocation,
        repeat: Option<&CommandInvocation>,
        suppress_repeat: bool,
        text_changed: bool,
        effects: &[AppEffect],
    ) -> bool {
        if let Some(id) = effects.iter().find_map(|effect| match effect {
            AppEffect::Input(InputCommand::Wait(id)) => Some(*id),
            _ => None,
        }) {
            self.pending_input.get_or_insert(id);
            self.pending_commands.insert(
                id,
                PendingCommand {
                    wrapper: false,
                    invocation: invocation.clone(),
                    // A completed asynchronous edit repeats the semantic command, not its effects.
                    repeat: Some(repeat.unwrap_or(invocation).clone()),
                    suppress_repeat,
                    text_changed,
                },
            );
            true
        } else {
            false
        }
    }

    pub(in crate::app) fn record_command_outcome(
        &mut self,
        invocation: &CommandInvocation,
        repeat: Option<&CommandInvocation>,
        suppress_repeat: bool,
        text_changed: bool,
    ) {
        if let Some(pending) = self
            .pending_input
            .and_then(|id| self.pending_commands.get_mut(&id))
        {
            pending.text_changed |= text_changed;
        } else {
            self.record_successful_invocation(invocation, repeat, suppress_repeat, text_changed);
        }
    }

    fn finish_command(
        &mut self,
        id: crate::editor::PendingCommandId,
        succeeded: bool,
        text_changed: bool,
    ) -> bool {
        let Some(pending) = self.pending_commands.remove(&id) else {
            return false;
        };
        if self.pending_input != Some(id) {
            return false;
        }
        self.pending_input = None;
        if succeeded {
            self.record_successful_invocation(
                &pending.invocation,
                pending.repeat.as_ref(),
                pending.suppress_repeat,
                pending.text_changed || text_changed,
            );
        }
        true
    }
}

impl App {
    pub(in crate::app) fn complete_input_command(
        &mut self,
        id: crate::editor::PendingCommandId,
        succeeded: bool,
        text_changed: bool,
    ) {
        if !self.input.finish_command(id, succeeded, text_changed) {
            return;
        }
        if succeeded {
            self.continue_macro_replay();
        } else {
            self.cancel_macro_replay();
            self.input.held_input.retain(|input| !matches!(input,
                crate::app::editing::clipboard::DeferredInput::Invocation(invocation)
                    if matches!(invocation.source, InvocationSource::Repeat | InvocationSource::Macro)));
        }
        while !self.input.command_pending() {
            let Some(input) = self.input.held_input.pop_front() else {
                break;
            };
            // Expansions of this key (dot, mappings) precede the keys typed after it.
            let later = std::mem::take(&mut self.input.held_input);
            match input {
                crate::app::editing::clipboard::DeferredInput::Event(event) => {
                    self.handle_event(event)
                }
                crate::app::editing::clipboard::DeferredInput::Invocation(invocation) => {
                    self.invoke(invocation)
                }
                crate::app::editing::clipboard::DeferredInput::MacroReplay(register, count) => {
                    self.replay_macro(register, count);
                }
            }
            self.input.held_input.extend(later);
        }
    }

    pub(in crate::app) fn fail_pending_input_command(&mut self) {
        if let Some(id) = self.input.pending_input_command() {
            self.complete_input_command(id, false, false);
        }
    }

    fn reconcile_named_mode(&mut self) {
        let valid =
            self.input
                .named_mode
                .as_ref()
                .is_none_or(|(editor, view, mode)| {
                    editor.same_editor(&self.state.window_version())
                        && self.state.active_view() == Some(*view)
                        && self.state.views.get(*view).is_some_and(|view| {
                            view.mode == mode.base().unwrap_or(EditorMode::Normal)
                        })
                });
        if !valid {
            self.input.named_mode = None;
            self.input.cancel_mapping();
        }
    }

    /// Built-in and named modes expose the same metadata; configuration remains the default owner.
    pub(in crate::app) fn editor_input_mode(&self) -> Option<NamedMode> {
        let view_id = self.state.active_view()?;
        let view = self.state.views.get(view_id)?;
        if let Some((editor, id, mode)) = &self.input.named_mode
            && *id == view_id
            && editor.same_editor(&self.state.window_version())
            && view.mode == mode.base().unwrap_or(EditorMode::Normal)
        {
            return Some(mode.clone());
        }
        let config = self.configuration.settings();
        let name = mode_name(view.mode);
        let shape = match view.mode {
            EditorMode::Insert => config.ui.cursor.insert,
            EditorMode::Replace => crate::ui::render::editor::REPLACE_CURSOR,
            EditorMode::CommandLine => crate::ui::render::editor::COMMAND_CURSOR,
            EditorMode::Prompt => crate::ui::render::editor::PROMPT_CURSOR,
            EditorMode::Normal => config.ui.cursor.normal,
            EditorMode::VisualChar
            | EditorMode::VisualLine
            | EditorMode::VisualBlock
            | EditorMode::Multi => crate::ui::render::editor::VISUAL_CURSOR,
            EditorMode::OperatorPending => crate::ui::render::editor::OPERATOR_PENDING_CURSOR,
        };
        let cursor = match shape {
            crate::config::CursorShape::Default => ModeCursor::Default,
            crate::config::CursorShape::Block => ModeCursor::Block,
            crate::config::CursorShape::Bar => ModeCursor::Bar,
            crate::config::CursorShape::Underline => ModeCursor::Underline,
            crate::config::CursorShape::BlinkingBlock => ModeCursor::BlinkingBlock,
            crate::config::CursorShape::BlinkingBar => ModeCursor::BlinkingBar,
            crate::config::CursorShape::BlinkingUnderline => ModeCursor::BlinkingUnderline,
        };
        NamedMode::new(
            name.into(),
            config
                .ui
                .status_line
                .mode_labels
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.into()),
            cursor,
            Keymap::default(),
            ModeKeys::Base(view.mode),
        )
        .ok()
    }

    pub(in crate::app) fn input_render_config(
        &self,
    ) -> std::borrow::Cow<'_, crate::config::Config> {
        let config = self.configuration.settings();
        if self.input.named_mode.is_some()
            && self.active_input_context().starts_with("editor.")
            && let Some(mode) = self.editor_input_mode()
        {
            let mut config = config.clone();
            let base = mode.base().unwrap_or(EditorMode::Normal);
            config
                .ui
                .status_line
                .mode_labels
                .insert(mode_name(base).into(), mode.label().into());
            std::borrow::Cow::Owned(config)
        } else {
            std::borrow::Cow::Borrowed(config)
        }
    }

    pub(in crate::app) fn project_mode_cursor(&self, frame: &mut crate::ui::Frame) {
        if !self.active_input_context().starts_with("editor.") {
            return;
        }
        if let Some(mode) = self.editor_input_mode()
            && let Some((_, _, cursor)) = &mut frame.cursor
        {
            use crossterm::cursor::SetCursorStyle;
            *cursor = match mode.cursor() {
                ModeCursor::Default => SetCursorStyle::DefaultUserShape,
                ModeCursor::Block => SetCursorStyle::SteadyBlock,
                ModeCursor::Bar => SetCursorStyle::SteadyBar,
                ModeCursor::Underline => SetCursorStyle::SteadyUnderScore,
                ModeCursor::BlinkingBlock => SetCursorStyle::BlinkingBlock,
                ModeCursor::BlinkingBar => SetCursorStyle::BlinkingBar,
                ModeCursor::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
            };
        }
    }
}
