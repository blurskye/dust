use crate::app::language::snippets;
mod mouse;

pub(in crate::app) const COMPLETION_LIMIT: usize = 1000;
pub(in crate::app) const HISTORY_LIMIT: usize = 100;

pub(in crate::app) use mouse::MouseDocument;

pub(in crate::app) use mouse::MouseGeometry;
pub(in crate::app) use crate::app::editing::replay::ReplayError;
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
    pub(in crate::app) snippet: Option<snippets::SnippetSession>,
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

    pub(in crate::app) fn new(count_limit: u64) -> Self {
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
    pub(in crate::app) fn mapping_context(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.context.as_str())
    }
    pub(in crate::app) fn finish_mapping(&mut self, context: &str, revision: u64) -> MappingResolution {
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

pub(in crate::app) mod interaction;


use crate::editor::CommandValue;
use crate::editor::InvocationSource;

use crate::app::editing::replay::repeated;
use crate::app::editing::replay::replayed;




/// Semantic recording and replay limits belong to input, not command dispatch.
impl InputSession {
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
                if text_changed && let Some(pending) = &mut self.pending_change {
                    pending.push(semantic);
                    self.pending_change_edited = true;
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

    pub(in crate::app) fn macro_register(
        &self,
        requested: char,
    ) -> Result<char, ReplayError> {
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


impl InputSession {
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




impl InputSession {
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
        self.completion_generation = self.completion_generation.saturating_add(1);
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
    pub(in crate::app) fn restore_history(
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
            let excess = history.len().saturating_sub(crate::app::input::HISTORY_LIMIT);
            history.drain(..excess);
        }
        self.history_index = None;
        self.history_draft = None;
    }
    pub(in crate::app) fn completion_current(&self, key: &PromptCompletionKey) -> bool {
        key.generation == self.completion_generation
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
        if request.generation != self.completion_generation || prompt.input != request.prompt_input
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

    pub(in crate::app) fn accept_completion(
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
            self.completion_generation = self.completion_generation.saturating_add(1);
            let generation = self.completion_generation;
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
                    generation,
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

    pub(in crate::app) fn record_prompt_history(
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
        let excess = history.len().saturating_sub(crate::app::input::HISTORY_LIMIT);
        if excess > 0 {
            history.drain(..excess);
        }
    }
}

use crate::editor::text::ByteRange;
use crate::ui::DocumentationMarkupKind;
use crate::ui::DocumentationPopupKind;
use crate::ui::DocumentationPopupState;
use crate::ui::DocumentationSection;
use crate::app::documentation::DOCUMENTATION_MAXIMUM_SECTIONS;
use crate::app::documentation::DOCUMENTATION_MAXIMUM_LINES;
use crate::app::documentation::DOCUMENTATION_MAXIMUM_BYTES;
use crate::app::documentation::markdown_rows;
use crate::app::documentation::wrap_prose;




impl InputSession {
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

use crate::editor::text::ByteOffset;

use crate::editor::Selection;
use crate::editor::SelectionKind;

use std::sync::Arc;

use crate::app::language::snippets::SnippetSession;



#[derive(Clone, Debug)]
pub(in crate::app) struct PromptCompletionKey {
    generation: u64,
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
