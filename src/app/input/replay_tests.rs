use crate::app::editing::replay::ReplayError;
use crate::app::input::InputSession;
use crate::editor::CommandInvocation;
use crate::editor::InvocationSource;

fn invocation(id: &str) -> CommandInvocation {
    let mut buffers = slotmap::SlotMap::<crate::editor::BufferId, ()>::with_key();
    let mut invocation = CommandInvocation::new(id, InvocationSource::Test).unwrap();
    invocation.target.buffer = Some(buffers.insert(()));
    invocation
}

#[test]
fn cancelling_pending_grammar_keeps_macro_recording() {
    use crate::editor::EditorMode;
    use crate::editor::input::GrammarInput;
    use crate::editor::input::InputAction;
    let mut input = InputSession::new(100);
    input
        .feed(
            EditorMode::Normal,
            GrammarInput::MacroRecordSelect("a".into()),
        )
        .unwrap();
    input.start_macro_recording('a');
    input.cancel_pending();
    assert!(input.recording_macro.is_some());
    let InputAction::Invoke(stop) = input
        .feed(EditorMode::Normal, GrammarInput::MacroRecordToggle)
        .unwrap()
    else {
        panic!("stop recording")
    };
    assert_eq!(stop.id.as_str(), "macro.record.stop");
}

#[test]
fn macro_toggle_uses_actual_recording_after_pending_grammar_is_cancelled() {
    use crate::editor::EditorMode;
    use crate::editor::input::GrammarInput;
    use crate::editor::input::InputAction;
    use crate::editor::input::PendingInputContext;
    let mut input = InputSession::new(100);
    input.start_macro_recording('a');
    input.record_successful_invocation(&invocation("edit.delete_char"), None, false, true);
    input.cancel_pending();
    let InputAction::Invoke(stop) = input
        .feed(EditorMode::Normal, GrammarInput::MacroRecordToggle)
        .unwrap()
    else {
        panic!("stop recording");
    };
    assert_eq!(stop.id.as_str(), "macro.record.stop");
    let (register, recorded) = input.finish_macro_recording().unwrap();
    assert_eq!(register, 'a');
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        input
            .feed(EditorMode::Normal, GrammarInput::MacroRecordToggle)
            .unwrap(),
        InputAction::Pending
    );
    assert_eq!(
        input.pending_context(),
        Some(PendingInputContext::MacroRecord)
    );
}

#[test]
fn recording_and_repeat_retain_semantics_and_strip_original_targets() {
    let mut input = InputSession::new(100);
    assert_eq!(input.repeat_plan(10, None), Err(ReplayError::NoChange));
    input.start_macro_recording('a');
    for id in ["mode.insert", "edit.insert_text", "mode.normal"] {
        input.record_successful_invocation(&invocation(id), None, false, id.starts_with("edit."));
    }
    input.record_successful_invocation(&invocation("macro.record_stop"), None, false, false);
    let (register, recorded) = input.finish_macro_recording().unwrap();
    assert_eq!(register, 'a');
    assert_eq!(recorded.len(), 3);
    assert!(
        recorded
            .iter()
            .all(|step| step.target == Default::default())
    );
    assert!(input.finish_macro_recording().is_none());
    assert_eq!(input.repeat_plan(2, None), Err(ReplayError::RepeatLimit));
    let plan = input.repeat_plan(3, None).unwrap();
    assert_eq!(plan.len(), 3);
    assert!(
        plan.iter().all(
            |step| step.source == InvocationSource::Repeat && step.target == Default::default()
        )
    );
    for step in &plan {
        input.record_successful_invocation(step, None, false, true);
    }
    input.record_successful_invocation(&invocation("edit.operator"), None, true, true);
    assert_eq!(input.repeat_plan(3, None).unwrap(), plan);
}

#[test]
fn visual_repeat_keeps_surround_delimiters_and_comment_tokens() {
    use crate::editor::CommandRegistry;
    use crate::editor::CommandValue;
    use crate::editor::EditorMode;
    use crate::editor::EditorState;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::register_builtin_commands;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::DisplayWidthPolicy;
    use crate::editor::text::TextBuffer;
    for (operator, name, value, expected) in [
        ("surround_add", "character", ")", "(one)\n(two)"),
        ("comment", "comment", "//", "// one\n// two"),
    ] {
        let mut input = InputSession::new(100);
        let mut state = EditorState::new(Default::default());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text("one\ntwo"), None)
            .unwrap();
        let selection = Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(2),
            kind: SelectionKind::Character,
        };
        state.views[view].mode = EditorMode::VisualChar;
        state.views[view].selection = Some(selection);
        let invocation = CommandInvocation::new("edit.operator", InvocationSource::Test)
            .unwrap()
            .with_argument("operator", CommandValue::String(operator.into()))
            .with_argument("motion", CommandValue::String("visual".into()))
            .with_argument("linewise", CommandValue::Bool(false))
            .with_argument(name, CommandValue::String(value.into()));
        let repeat = input
            .normalized_visual_repeat(
                &invocation,
                &state.buffers[buffer].text,
                selection,
                DisplayWidthPolicy::default(),
            )
            .unwrap();
        assert_eq!(repeat.arguments.get(name), invocation.arguments.get(name));
        let mut registry = CommandRegistry::<()>::default();
        register_builtin_commands(&mut registry).unwrap();
        let outcome = registry.execute(&state, &invocation).unwrap();
        state
            .apply_transaction(outcome.transaction.as_ref().unwrap())
            .unwrap();
        input.record_successful_invocation(&invocation, Some(&repeat), false, true);
        state.views[view].cursor = state.buffers[buffer].text.line_to_byte(1).unwrap();
        for step in input.repeat_plan(10, None).unwrap() {
            let outcome = registry.execute(&state, &step).unwrap();
            state
                .apply_transaction(outcome.transaction.as_ref().unwrap())
                .unwrap();
        }
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
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
#[test]
fn macro_recording_keeps_repeat_commands_without_their_expansion() {
    let mut input = InputSession::new(100);
    input.start_macro_recording('a');
    input.record_successful_invocation(&invocation("edit.repeat_last"), None, false, false);
    let mut expanded = invocation("edit.delete_char");
    expanded.source = InvocationSource::Repeat;
    input.record_successful_invocation(&expanded, None, false, true);
    let (_, recorded) = input.finish_macro_recording().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].id.as_str(), "edit.repeat_last");
}

#[test]
fn insert_delete_joins_the_change_and_yanks_leave_it_alone() {
    let mut input = InputSession::new(100);
    for id in [
        "mode.insert",
        "edit.insert_text",
        "edit.delete_char",
        "mode.normal",
    ] {
        input.record_successful_invocation(&invocation(id), None, false, id.starts_with("edit."));
    }
    let plan = input.repeat_plan(10, None).unwrap();
    assert_eq!(
        plan.iter()
            .map(|invocation| invocation.id.as_str())
            .collect::<Vec<_>>(),
        [
            "mode.insert",
            "edit.insert_text",
            "edit.delete_char",
            "mode.normal"
        ]
    );
    let yank = invocation("edit.operator").with_argument(
        "operator",
        crate::editor::CommandValue::String("yank".into()),
    );
    input.start_macro_recording('a');
    input.record_successful_invocation(&yank, None, false, false);
    assert_eq!(input.repeat_plan(10, None).unwrap(), plan);
    assert_eq!(input.finish_macro_recording().unwrap().1.len(), 1);
}
