use super::InputSession;
use crate::editor::{CommandInvocation, CommandValue, InvocationSource};

#[derive(Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum ReplayError {
    NoChange,
    RepeatLimit,
    NoMacro,
    RecursionLimit,
    StepLimit,
}

/// Semantic recording and replay limits belong to input, not command dispatch.
impl InputSession {
    pub(in crate::editor::session) fn record_successful_invocation(
        &mut self,
        invocation: &CommandInvocation,
        repeat_override: Option<&CommandInvocation>,
        suppress_repeat: bool,
    ) {
        if !invocation.id.as_str().starts_with("macro.")
            && !invocation.id.as_str().starts_with("input.")
            && let Some((_, recorded)) = &mut self.recording_macro
        {
            let mut invocation = invocation.clone();
            invocation.target = Default::default();
            recorded.push(invocation);
        }
        if invocation.source == InvocationSource::Macro || suppress_repeat {
            return;
        }
        let mut semantic = repeat_override.unwrap_or(invocation).clone();
        semantic.target = Default::default();
        match invocation.id.as_str() {
            "mode.insert" | "mode.replace" | "edit.substitute" => {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.operator" | "edit.repeat_visual"
                if semantic.arguments.get("operator")
                    == Some(&CommandValue::String("change".into())) =>
            {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.insert_text"
            | "edit.backspace"
            | "edit.replace_text"
            | "edit.replace_backspace" => {
                if let Some(pending) = &mut self.pending_change {
                    pending.push(semantic);
                }
            }
            "mode.normal" => {
                if let Some(mut pending) = self.pending_change.take() {
                    pending.push(semantic);
                    self.last_change = pending;
                }
            }
            "edit.delete_char"
            | "edit.delete_backward"
            | "edit.replace"
            | "edit.join_lines"
            | "edit.toggle_case"
            | "edit.put"
            | "edit.operator"
            | "edit.repeat_visual" => {
                self.last_change = vec![semantic];
            }
            _ => {}
        }
    }

    pub(in crate::editor::session) fn repeat_plan(
        &self,
        limit: usize,
    ) -> Result<Vec<CommandInvocation>, ReplayError> {
        if self.last_change.is_empty() {
            return Err(ReplayError::NoChange);
        }
        if self.last_change.len() > limit {
            return Err(ReplayError::RepeatLimit);
        }
        Ok(self.last_change.iter().cloned().map(replayed).collect())
    }

    pub(in crate::editor::session) fn start_macro_recording(&mut self, register: char) {
        self.recording_macro = Some((register, Vec::new()));
    }

    pub(in crate::editor::session) fn finish_macro_recording(
        &mut self,
    ) -> Option<(char, Vec<CommandInvocation>)> {
        self.recording_macro.take()
    }

    pub(in crate::editor::session) fn macro_register(
        &self,
        requested: char,
    ) -> Result<char, ReplayError> {
        if requested == '@' {
            self.last_macro.ok_or(ReplayError::NoMacro)
        } else {
            Ok(requested)
        }
    }

    pub(in crate::editor::session) fn begin_macro_batch(&mut self) {
        if self.replay_depth == 0 {
            self.replay_steps = 0;
        }
    }

    pub(in crate::editor::session) fn begin_macro_replay(
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

    pub(in crate::editor::session) fn macro_step(
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

    pub(in crate::editor::session) fn finish_macro_replay(&mut self) {
        self.replay_depth = self.replay_depth.saturating_sub(1);
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn last_macro(&self) -> Option<char> {
        self.last_macro
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn replay_depth(&self) -> usize {
        self.replay_depth
    }
}

fn replayed(mut invocation: CommandInvocation) -> CommandInvocation {
    invocation.source = InvocationSource::Macro;
    invocation.target = Default::default();
    invocation
}

impl InputSession {
    pub(in crate::editor::session) fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
        text: &crate::editor::text::TextBuffer,
        selection: crate::editor::Selection,
        width_policy: crate::editor::text::DisplayWidthPolicy,
    ) -> Option<CommandInvocation> {
        use crate::editor::text::{ByteRange, display_column_at, next_grapheme_boundary};
        use unicode_segmentation::UnicodeSegmentation;
        let (shape, width, height) = match selection.kind {
            crate::editor::SelectionKind::Character => {
                let low = selection.anchor.min(selection.head);
                let high = selection.anchor.max(selection.head);
                let end = next_grapheme_boundary(text, high);
                let selected = text.slice(ByteRange::new(low, end).ok()?).ok()?;
                ("character", selected.graphemes(true).count(), 1_usize)
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

#[cfg(test)]
mod tests {
    use super::{InputSession, ReplayError};
    use crate::editor::{CommandInvocation, InvocationSource};

    fn invocation(id: &str) -> CommandInvocation {
        let mut buffers = slotmap::SlotMap::<crate::editor::BufferId, ()>::with_key();
        let mut invocation = CommandInvocation::new(id, InvocationSource::Test).unwrap();
        invocation.target.buffer = Some(buffers.insert(()));
        invocation
    }

    #[test]
    fn recording_and_repeat_retain_semantics_and_strip_original_targets() {
        let mut input = InputSession::new(100);
        assert_eq!(input.repeat_plan(10), Err(ReplayError::NoChange));
        input.start_macro_recording('a');
        for id in ["mode.insert", "edit.insert_text", "mode.normal"] {
            input.record_successful_invocation(&invocation(id), None, false);
        }
        input.record_successful_invocation(&invocation("macro.record_stop"), None, false);
        let (register, recorded) = input.finish_macro_recording().unwrap();
        assert_eq!(register, 'a');
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded
                .iter()
                .all(|step| step.target == Default::default())
        );
        assert!(input.finish_macro_recording().is_none());
        assert_eq!(input.repeat_plan(2), Err(ReplayError::RepeatLimit));
        let plan = input.repeat_plan(3).unwrap();
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(
            |step| step.source == InvocationSource::Macro && step.target == Default::default()
        ));
        for step in &plan {
            input.record_successful_invocation(step, None, false);
        }
        input.record_successful_invocation(&invocation("edit.operator"), None, true);
        assert_eq!(input.repeat_plan(3).unwrap(), plan);
    }

    #[test]
    fn nested_macro_batches_share_step_budget_and_unwind_without_losing_last_register() {
        let mut input = InputSession::new(100);
        assert_eq!(input.macro_register('@'), Err(ReplayError::NoMacro));
        input.begin_macro_batch();
        input.begin_macro_replay('a', 2).unwrap();
        assert!(input.macro_step(invocation("edit.put"), 2).is_ok());
        input.begin_macro_batch(); // Nested invocation must not reset the budget.
        input.begin_macro_replay('b', 2).unwrap();
        assert_eq!(
            input.begin_macro_replay('c', 2),
            Err(ReplayError::RecursionLimit)
        );
        assert!(input.macro_step(invocation("edit.put"), 2).is_ok());
        assert_eq!(
            input.macro_step(invocation("edit.put"), 2),
            Err(ReplayError::StepLimit)
        );
        input.finish_macro_replay();
        input.finish_macro_replay();
        assert_eq!(input.replay_depth(), 0);
        assert_eq!(input.macro_register('@'), Ok('b'));
        input.begin_macro_batch();
        input.begin_macro_replay('a', 2).unwrap();
        assert!(input.macro_step(invocation("edit.put"), 2).is_ok());
        input.finish_macro_replay();
    }
}
