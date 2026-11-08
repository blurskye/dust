//! Editing commands, transactions and replay.

pub(in crate::app) mod clipboard;
pub(in crate::app) mod flash;
pub(in crate::app) mod replay;
pub(in crate::app) mod substitute;

pub(in crate::app) mod commands;

#[cfg(test)]
mod clipboard_tests;

use crate::app::App;
use crate::app::FULL_PAGE_PERCENT;
use crate::app::HALF_PAGE_PERCENT;
use crate::app::MACRO_RECURSION_LIMIT;
use crate::app::MACRO_STEP_LIMIT;
use crate::app::REPEAT_STEP_LIMIT;
use crate::app::configuration::configured_grapheme_width;
use crate::app::configuration::configured_text_width;
use crate::app::windows::NativeDescriptions;
use crate::editor::CommandInvocation;
use crate::editor::EditorMode;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::TransactionId;
use crate::editor::UndoPolicy;
use crate::editor::ViewId;
use crate::editor::input::InputAction;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::DisplayWidthPolicy;
use crate::editor::text::TextBuffer;
use crate::ui::VisualRowAnchor;
use crate::ui::VisualRowModel;
use crate::ui::active_editor_pane_viewport;
use crate::ui::editor_state_content_width;
use crate::ui::render::SCRATCH_LABEL;
use crate::ui::snap_display_column;
use crate::ui::snap_display_column_forward;

use crate::app::syntax::nth_non_blank;

impl App {
    pub(in crate::app) fn process_grammar_input(
        &mut self,
        input: crate::editor::input::GrammarInput,
    ) {
        let mode = self
            .state
            .active_view()
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        match self.input.feed(mode, input) {
            Ok(InputAction::Invoke(invocation)) => self.invoke_mapped(invocation),
            Ok(InputAction::Pending) => {
                if self.input.effective_mode(mode) == EditorMode::OperatorPending
                    && mode != EditorMode::OperatorPending
                {
                    self.set_mode(EditorMode::OperatorPending);
                }
            }
            Ok(InputAction::Cancelled) => {
                if mode == EditorMode::OperatorPending {
                    self.set_mode(EditorMode::Normal);
                }
            }
            Ok(InputAction::Text(text)) => {
                let context = self.active_input_context();
                if let Some(command_id) = self
                    .configuration
                    .settings()
                    .input
                    .text_commands
                    .get(&context)
                    .cloned()
                {
                    self.invoke_text_command(&command_id, text);
                }
            }
            Ok(InputAction::Unmapped) => {}
            Err(error) => {
                if mode == EditorMode::OperatorPending {
                    self.set_mode(EditorMode::Normal);
                }
                self.show_error(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<CommandInvocation> {
        let view = self
            .state
            .views
            .get(invocation.target.view.or(self.state.active_view())?)?;
        self.input.normalized_visual_repeat(
            invocation,
            &self.state.buffers.get(view.buffer)?.text,
            view.selection?,
            DisplayWidthPolicy {
                tab_width: self.state.policy.tab_width,
                ambiguous_wide: self.state.policy.ambiguous_width_wide,
            },
        )
    }
}

impl App {
    pub(in crate::app) fn repeat_last_change(&mut self, count: Option<std::num::NonZeroU64>) {
        let limit = REPEAT_STEP_LIMIT;
        match self.input.repeat_plan(limit, count) {
            Ok(invocations) => {
                for invocation in invocations {
                    if !self.invoke_checked(invocation) {
                        break;
                    }
                }
            }
            Err(crate::app::editing::replay::ReplayError::NoChange) => {
                self.show_error(self.message("no_previous_change", &[]));
            }
            Err(_) => {
                self.show_error(self.message("repeat_step_limit", &[("limit", limit.to_string())]))
            }
        }
    }
}

impl App {
    pub(in crate::app) fn replay_macro(&mut self, requested: char, count: u64) -> bool {
        if self.clipboard.waiting() {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::MacroReplay(
                    requested, count,
                ));
            return true;
        }
        let Ok(register) = self.input.macro_register(requested) else {
            self.show_error(self.message("no_previous_macro", &[]));
            return false;
        };
        let Some(invocations) = self.state.registers.get_macro(register).map(<[_]>::to_vec) else {
            self.show_error(self.message("empty_macro", &[("register", register.to_string())]));
            return false;
        };
        if self
            .input
            .begin_macro_replay(register, MACRO_RECURSION_LIMIT)
            .is_err()
        {
            self.show_error(self.message(
                "macro_recursion_limit",
                &[("limit", MACRO_RECURSION_LIMIT.to_string())],
            ));
            return false;
        }
        self.macro_replays.push((invocations, 0, count));
        self.continue_macro_replay()
    }
}

impl App {
    pub(in crate::app) fn cancel_macro_replay(&mut self) {
        for _ in self.macro_replays.drain(..) {
            self.input.finish_macro_replay();
        }
    }
}

impl App {
    pub(in crate::app) fn continue_macro_replay(&mut self) -> bool {
        if self.macro_running || self.clipboard.waiting() || self.input.command_pending() {
            return true;
        }
        self.macro_running = true;
        let mut succeeded = true;
        while let Some((invocations, next, remaining)) = self.macro_replays.last_mut() {
            if *next == invocations.len() {
                if *remaining > 1 && !invocations.is_empty() {
                    *remaining -= 1;
                    *next = 0;
                } else {
                    self.macro_replays.pop();
                    self.input.finish_macro_replay();
                    continue;
                }
            }
            let invocation = invocations[*next].clone();
            *next += 1;
            let Ok(invocation) = self.input.macro_step(invocation, MACRO_STEP_LIMIT) else {
                self.show_error(self.message(
                    "macro_step_limit",
                    &[("limit", MACRO_STEP_LIMIT.to_string())],
                ));
                succeeded = false;
                self.cancel_macro_replay();
                break;
            };
            if !self.invoke_checked(invocation) {
                succeeded = false;
                self.cancel_macro_replay();
                break;
            }
            if self.clipboard.waiting() || self.input.command_pending() {
                break;
            }
        }
        self.macro_running = false;
        succeeded
    }
}

impl App {
    pub(in crate::app) fn apply_authoritative_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
    ) -> Result<(), String> {
        self.commit_editor_transaction(transaction, None)
    }
}

impl App {
    pub(in crate::app) fn commit_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
        plan: Option<crate::app::windows::WindowPlan>,
    ) -> Result<(), String> {
        if let Some(plan) = &plan {
            self.window_session.check_plan(
                plan,
                &self.state.window_version(),
                &self.extension.contributions().version(),
            )?;
        }
        self.window_session
            .check_editor_source(&(&*self.state).into())?;
        let candidate = self
            .state
            .prepare_transaction(transaction)
            .map_err(|error| error.to_string())?;
        self.commit_editor_draft(candidate, plan, false)
    }
}

impl App {
    pub(in crate::app) fn commit_editor_draft(
        &mut self,
        candidate: crate::editor::EditorDraft,
        plan: Option<crate::app::windows::WindowPlan>,
        activate_document: bool,
    ) -> Result<(), String> {
        self.window_session
            .check_editor_source(&(&*self.state).into())?;
        let current = self.state.version();
        candidate.check_current(&current)?;
        if !activate_document
            && plan.is_none()
            && candidate.window_version() == self.state.window_version()
        {
            *self.state = candidate.accept(&current)?;
            return Ok(());
        }

        let terminals = self.terminal_session.descriptions();
        let mut signature = self.native_surface_signature();
        signature.dashboard_visible =
            self.window_session.dashboard_visible() || candidate.topology().active_tab().is_none();
        let title = self.message("explorer_title", &[]);
        let descriptions = NativeDescriptions {
            signature,
            explorer_title: &title,
            dashboard_title: &self.configuration.dashboard().identity,
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title),
            terminals: &terminals,
        };
        let catalog = self.extension.contributions().version();
        let transition = if let Some(plan) = plan {
            self.window_session.prepare_editor_plan(
                plan,
                (&current, &catalog),
                candidate,
                SCRATCH_LABEL,
                descriptions,
            )?
        } else if activate_document {
            self.window_session.prepare_document_transition(
                candidate,
                &current,
                catalog.clone(),
                SCRATCH_LABEL,
                descriptions,
            )?
        } else {
            self.window_session.prepare_editor_transition(
                candidate,
                &current,
                catalog.clone(),
                SCRATCH_LABEL,
                Some(descriptions),
            )?
        };
        self.persistence.reconcile_marks(&self.state);
        *self.state = self
            .window_session
            .accept_editor_transition(transition, &current, &catalog)?;
        self.persistence.reconcile_marks(&self.state);
        self.reconcile_editor_windows();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn scroll_view(&mut self, kind: &str, count: u64) {
        self.reconcile_editor_windows();
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let Some(viewport) = active_editor_pane_viewport(&self.state, &self.ui)
            .filter(|viewport| viewport.view == view_id)
        else {
            return;
        };
        let body = usize::from(viewport.height.max(1));
        let percent = if kind.starts_with("half_") {
            HALF_PAGE_PERCENT
        } else {
            FULL_PAGE_PERCENT
        };
        let amount = body.saturating_mul(usize::from(percent)) / 100;
        let amount = amount
            .max(1)
            .saturating_mul(usize::try_from(count).unwrap_or(usize::MAX));
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&view.buffer));
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap_enabled,
        );
        let old_start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        // Scroll past an expanded diagnostic block even when it is taller than a page.
        // Viewport fitting still uses forward/backward's strict screen-row budget.
        let advance = |anchor| {
            let target = if kind.ends_with("up") {
                visual_rows.backward(anchor, amount)
            } else {
                visual_rows.forward(anchor, amount)
            };
            if target != anchor {
                target
            } else if kind.ends_with("up") {
                visual_rows.previous(anchor)
            } else {
                visual_rows.next(anchor)
            }
        };
        let first_anchor = advance(old_start_anchor);
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let last_anchor = visual_rows.forward(first_anchor, body.saturating_sub(1));
        let cursor_anchor = if kind.starts_with("half_") {
            advance(cursor_anchor)
        } else {
            cursor_anchor
        };
        let target_anchor = cursor_anchor.clamp(first_anchor, last_anchor);
        let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let old_line_start = buffer
            .text
            .line_to_byte(current_line)
            .unwrap_or(ByteOffset(0));
        let old_prefix = buffer
            .text
            .slice(ByteRange::new(old_line_start, view.cursor).expect("cursor line range"))
            .unwrap_or_default();
        let current_column = configured_text_width(
            &old_prefix,
            self.configuration.settings().editor.tab_width,
            self.configuration.settings().editor.ambiguous_width,
        );
        let target_line = target_anchor.line;
        let target_start = buffer
            .text
            .line_to_byte(target_line)
            .unwrap_or(ByteOffset(0));
        let target_byte = if wrap_enabled {
            let current_cell = visual_rows.cursor_projection(view.cursor).1;
            visual_rows.byte_at_cell(target_anchor, current_cell)
        } else {
            let target_text = buffer.text.line(target_line).unwrap_or_default();
            let target_content = target_text.strip_suffix('\n').unwrap_or(&target_text);
            let mut display_column = 0_usize;
            let mut target_byte = 0_usize;
            for (relative, grapheme) in target_content.grapheme_indices(true) {
                let width = if grapheme == "\t" {
                    self.configuration.settings().editor.tab_width
                        - (display_column % self.configuration.settings().editor.tab_width)
                } else {
                    configured_grapheme_width(
                        grapheme,
                        self.configuration.settings().editor.ambiguous_width,
                    )
                };
                if display_column.saturating_add(width) > current_column {
                    target_byte = relative;
                    break;
                }
                display_column = display_column.saturating_add(width);
                target_byte = relative.saturating_add(grapheme.len());
            }
            target_byte
        };
        let mut target = ByteOffset(target_start.get().saturating_add(target_byte));
        if !matches!(view.mode, EditorMode::Insert | EditorMode::Replace) {
            let line = buffer.text.line(target_line).unwrap_or_default();
            let content = line.strip_suffix('\n').unwrap_or(&line);
            if !content.is_empty() && target_byte >= content.len() {
                target = ByteOffset(
                    target_start.get()
                        + content
                            .grapheme_indices(true)
                            .next_back()
                            .expect("nonempty line")
                            .0,
                );
            }
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first_anchor.line,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row: if wrap_enabled {
                    first_anchor.wrap_row
                } else {
                    0
                },
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: target,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    /// Vim never leaves the Normal-mode cursor on the empty line after a final newline; `dd`
    /// on the last line lands on the line above instead.
    /// Where views stand on lines a formatter is replacing. The edit alone would send each to
    /// the end of what it replaced, so `restore_view_positions` puts them back.
    /// Each cursor as the count of non-blank characters before it, for views whose cursor line
    /// `replaced` says a reformat rewrites. Formatting mostly moves whitespace, so the cursor
    /// then stays on the same character, where a line and column would land on other code.
    pub(in crate::app) fn view_positions(
        &self,
        replaced: impl Fn(usize) -> bool,
    ) -> Vec<(
        crate::editor::ViewId,
        crate::editor::text::BufferRevision,
        usize,
        TextBuffer,
    )> {
        self.state
            .views
            .iter()
            .filter_map(|(id, view)| {
                let text = &self.state.buffers.get(view.buffer)?.text;
                if !replaced(text.byte_to_line(view.cursor).ok()?) {
                    return None;
                }
                let before = text
                    .slice(ByteRange::new(ByteOffset(0), view.cursor).ok()?)
                    .ok()?;
                let non_blank = before.chars().filter(|c| !c.is_whitespace()).count();
                Some((id, text.revision(), non_blank, text.clone()))
            })
            .collect()
    }
}

impl App {
    pub(in crate::app) fn restore_view_positions(
        &mut self,
        positions: Vec<(
            crate::editor::ViewId,
            crate::editor::text::BufferRevision,
            usize,
            TextBuffer,
        )>,
    ) {
        for (id, revision, non_blank, content) in positions {
            let Some(view) = self.state.views.get(id) else {
                continue;
            };
            let text = &self.state.buffers[view.buffer].text;
            if text.revision() == revision {
                continue;
            }
            let text = text.to_string();
            // Punctuation and other code changes invalidate the ordinal. In that case keep
            // the cursor already mapped through the edit by the editor transaction.
            if !text
                .chars()
                .filter(|c| !c.is_whitespace())
                .eq(content.to_string().chars().filter(|c| !c.is_whitespace()))
            {
                continue;
            }
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_operation(EditorOperation::SetCursor {
                    view: id,
                    position: ByteOffset(nth_non_blank(&text, non_blank)),
                })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn keep_cursor_off_final_newline(&mut self) {
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        self.keep_view_cursor_off_final_newline(view_id);
    }
}

impl App {
    pub(in crate::app) fn keep_view_cursor_off_final_newline(
        &mut self,
        view_id: crate::editor::ViewId,
    ) {
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
}

impl App {
    /// Whether yanks and pastes go through the system clipboard (`clipboard = "unnamedplus"`).
    pub(in crate::app) fn clipboard_shared(&self) -> bool {
        self.configuration.settings().editor.registers.clipboard
            == crate::config::ClipboardSync::Unnamedplus
    }
}

impl App {
    pub(in crate::app) fn process_clipboard_paste(&mut self) {
        let source = self.state.window_version();
        let enabled = self.state.policy.registers_enabled;
        let was_waiting = self.clipboard.waiting();
        let revision = self.state.registers.revision();
        let paste = self
            .clipboard
            .finish_pull(&mut self.state.value.registers, enabled, &source);
        if self.state.registers.revision() != revision {
            self.state.revision = self.state.revision.wrapping_add(1);
        }
        let cancelled = match paste {
            Ok(Some(invocation)) => {
                let succeeded = self.invoke_checked(invocation);
                if succeeded {
                    self.continue_macro_replay();
                }
                !succeeded
            }
            Ok(None) => was_waiting && !self.clipboard.waiting(),
            Err(error) => {
                self.show_error(error);
                true
            }
        };
        if cancelled {
            self.cancel_macro_replay();
        }
        while let Some(input) = self.clipboard.next_input() {
            // Commands expanded by this input must finish before later queued input.
            let later = self.clipboard.take_deferred();
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
            self.clipboard.append_deferred(later);
        }
    }
}

impl App {
    /// Light up what a transaction yanked for a moment, as LazyVim does.
    pub(in crate::app) fn highlight_yank(
        &mut self,
        transaction: &crate::editor::EditorTransaction,
    ) {
        const SHOWN: std::time::Duration = std::time::Duration::from_millis(150);
        let Some((view, ranges)) =
            transaction
                .operations
                .iter()
                .find_map(|operation| match operation {
                    crate::editor::EditorOperation::Yanked { view, ranges } => {
                        Some((*view, ranges.clone()))
                    }
                    _ => None,
                })
        else {
            return;
        };
        let Some(buffer) = self
            .state
            .views
            .get(view)
            .and_then(|view| self.state.buffers.get(view.buffer))
        else {
            return;
        };
        self.ui.yanked = Some(crate::ui::YankHighlight {
            view,
            revision: buffer.text.revision().0,
            ranges,
            until: self.clock.monotonic() + SHOWN,
        });
    }
}

impl App {
    pub(in crate::app) fn update_yank_highlight(&mut self) {
        if self
            .ui
            .yanked
            .as_ref()
            .is_some_and(|yanked| yanked.until <= self.clock.monotonic())
        {
            self.ui.yanked = None;
        }
    }
}

impl App {
    pub(in crate::app) fn set_mode(&mut self, mode: EditorMode) {
        if let Some(view) = self.state.active_view() {
            self.set_specific_mode(view, mode);
        }
    }
}

impl App {
    pub(in crate::app) fn set_specific_mode(&mut self, view: ViewId, mode: EditorMode) {
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewExists(view))
            .with_operation(EditorOperation::SetMode { view, mode })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn ensure_cursor_visible(&mut self) {
        self.reconcile_editor_windows();
        self.sync_flash_ui();
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let Some(viewport) = active_editor_pane_viewport(&self.state, &self.ui) else {
            return;
        };
        if viewport.view != view_id {
            return;
        }
        let body_height = usize::from(viewport.height.max(1));
        let vertical_margin = usize::from(
            self.configuration
                .settings()
                .editor
                .viewport
                .vertical_margin,
        )
        .min(body_height.saturating_sub(1) / 2);
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings().editor.wrap
            && !self.ui.large_file_buffers.contains(&view.buffer);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap_enabled,
        );
        let start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let first_anchor = match visual_rows.screen_row(start_anchor, cursor_anchor, body_height) {
            Some(row) if row < vertical_margin => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            Some(row) if row >= body_height.saturating_sub(vertical_margin) => visual_rows
                .backward(
                    cursor_anchor,
                    body_height
                        .saturating_sub(vertical_margin)
                        .saturating_sub(1),
                ),
            Some(_) => start_anchor,
            None if cursor_anchor < start_anchor => {
                visual_rows.backward(cursor_anchor, vertical_margin)
            }
            None => visual_rows.backward(
                cursor_anchor,
                body_height
                    .saturating_sub(vertical_margin)
                    .saturating_sub(1),
            ),
        };
        let first = first_anchor.line;
        // Without wrap only a markdown table's row takes more than one row.
        let first_wrap_row = first_anchor.wrap_row;
        let first_column = if wrap_enabled || available == 0 {
            0
        } else {
            let source_column = visual_rows.cursor_projection(view.cursor).1;
            let line_start = buffer
                .text
                .line_to_byte(line)
                .unwrap_or(crate::editor::text::ByteOffset(0));
            let mut hints = self
                .ui
                .lsp_inlay_hints
                .iter()
                .chain(&self.ui.substitute_preview)
                .filter(|hint| {
                    hint.buffer == view.buffer
                        && hint.revision == buffer.text.revision()
                        && hint.position >= line_start
                        && hint.position <= view.cursor
                })
                .take(10_000)
                .collect::<Vec<_>>();
            hints.sort_unstable_by_key(|hint| hint.position);
            let line_text = buffer.text.line(line).unwrap_or_default();
            let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);
            // Scrolling hides hints before the visible source range; they must not
            // keep pushing the source scroll position farther to the right.
            let virtual_width = |scroll| {
                let skipped = crate::ui::render::editor::byte_offset_at_display_column(
                    line_text,
                    scroll,
                    self.configuration.settings().editor.tab_width,
                    self.configuration.settings().editor.ambiguous_width,
                );
                let visible_start = line_start.get().saturating_add(skipped);
                hints
                    .iter()
                    .filter(|hint| hint.position.get() >= visible_start)
                    .take(32)
                    .map(|hint| {
                        crate::ui::geometry::cell_text_width(
                            &hint.label,
                            self.configuration.settings().editor.ambiguous_width,
                        )
                    })
                    .fold(0usize, usize::saturating_add)
            };
            let cursor_column = source_column.saturating_add(virtual_width(view.scroll_column));
            let margin = usize::from(
                self.configuration
                    .settings()
                    .editor
                    .viewport
                    .horizontal_margin,
            )
            .min(available.saturating_sub(1));
            let target = if cursor_column < view.scroll_column.saturating_add(margin) {
                cursor_column.saturating_sub(margin)
            } else if cursor_column
                >= view
                    .scroll_column
                    .saturating_add(available.saturating_sub(margin))
            {
                cursor_column.saturating_sub(available.saturating_sub(margin).saturating_sub(1))
            } else {
                view.scroll_column
            }
            .min(source_column);
            let snapped = snap_display_column(
                line_text,
                target,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            );
            let cursor_column = source_column.saturating_add(virtual_width(snapped));
            if cursor_column.saturating_sub(snapped) < available {
                snapped
            } else {
                snap_display_column_forward(
                    line_text,
                    cursor_column
                        .saturating_add(1)
                        .saturating_sub(available)
                        .min(source_column),
                    self.configuration.settings().editor.tab_width,
                    self.configuration.settings().editor.ambiguous_width,
                )
                .min(source_column)
            }
        };
        if first == view.scroll_line
            && first_wrap_row == view.scroll_wrap_row
            && first_column == view.scroll_column
        {
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row,
            })
            .with_operation(EditorOperation::SetScrollColumn {
                view: view_id,
                first_column,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn transaction_id(&mut self) -> TransactionId {
        let id = TransactionId(self.next_transaction);
        self.next_transaction = self.next_transaction.saturating_add(1);
        id
    }
}

use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
pub(in crate::app) mod tests;
