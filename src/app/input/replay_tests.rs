//! Replay tests for keyboard, mouse and mapped command input.

use crate::app::editing::replay::ReplayError;
use crate::app::input::InputState;
use crate::editor::CommandInvocation;
use crate::editor::InvocationSource;

fn pending_session() -> crate::app::tests::driver::TestSession {
    use crate::app::commands::AppEffect;
    use crate::app::input::InputCommand;
    use crate::editor::{CommandOutcome, CommandSpec};
    let mut session =
        crate::app::tests::driver::TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    let app = &mut session.application;
    app.handle_configured_key("i".parse().unwrap());
    app.invoke_insert_text("abcdef".into());
    app.invoke(CommandInvocation::new("mode.normal", InvocationSource::Test).unwrap());
    let view = app.state.active_view().unwrap();
    app.state.views[view].cursor = crate::editor::text::ByteOffset(0);
    app.commands
        .register(
            CommandSpec::builtin("test.pending_edit", "Pending edit").unwrap(),
            move |_, _| {
                let mut outcome = CommandOutcome::empty();
                outcome.effects.push(AppEffect::Input(InputCommand::Wait(
                    crate::editor::PendingCommandId::allocate()?,
                )));
                Ok(outcome)
            },
        )
        .unwrap();
    let mut layer = crate::editor::input::Keymap::default();
    layer.insert(
        &[crate::editor::input::KeyEvent::character('x')],
        CommandInvocation::new("test.pending_edit", InvocationSource::Keymap).unwrap(),
    );
    app.process_effect(AppEffect::Input(InputCommand::PushLayer(Box::new(layer))));
    session
}

fn complete_edit(app: &mut crate::app::App) {
    let id = app.input.pending_input.unwrap();
    let delete = CommandInvocation::new("edit.delete_char", InvocationSource::Test).unwrap();
    let outcome = app.commands.execute(&app.state, &delete).unwrap();
    app.apply_authoritative_editor_transaction(outcome.transaction.as_ref().unwrap())
        .unwrap();
    app.process_effect(crate::app::commands::AppEffect::Input(
        crate::app::input::InputCommand::Complete {
            id,
            succeeded: true,
            text_changed: true,
        },
    ));
}

fn keys(app: &mut crate::app::App, keys: &[&str]) {
    for key in keys {
        app.handle_event(crate::ui::UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
}

#[test]
fn pending_command_holds_keys_and_replays_them_in_order_after_completion() {
    let mut session = pending_session();
    let app = &mut session.application;
    let buffer = app.state.active_buffer_id().unwrap();
    keys(app, &["x", "i", "1", "2", "escape"]);
    assert!(app.input.command_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abcdef");
    app.complete_input_command(
        crate::editor::PendingCommandId::allocate().unwrap(),
        true,
        true,
    );
    assert!(app.input.command_pending());
    complete_edit(app);
    assert!(!app.input.command_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "12bcdef");
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn dot_records_the_completed_pending_command_and_waits_again_when_replayed() {
    let mut session = pending_session();
    let app = &mut session.application;
    let before = app.input.repeat_plan(100, None).unwrap();
    keys(app, &["x", ".", "l"]);
    assert_eq!(app.input.repeat_plan(100, None).unwrap(), before);
    complete_edit(app);
    let plan = app.input.repeat_plan(100, None).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].id.as_str(), "test.pending_edit");
    assert!(app.input.command_pending());
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        0
    );
    complete_edit(app);
    assert!(!app.input.command_pending());
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        1
    );
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "cdef"
    );
}

#[test]
fn macro_records_completion_and_pauses_its_remaining_steps_during_replay() {
    let mut session = pending_session();
    let app = &mut session.application;
    keys(app, &["q", "a", "x", "l", "q"]);
    assert!(app.input.recording_macro.as_ref().unwrap().1.is_empty());
    complete_edit(app);
    let recorded = app.state.registers.get_macro('a').unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].id.as_str(), "test.pending_edit");
    assert_eq!(recorded[1].id.as_str(), "view.cursor.move");
    keys(app, &["@", "a"]);
    assert!(app.input.command_pending());
    assert_eq!(app.input.replay_depth(), 1);
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        1
    );
    complete_edit(app);
    assert_eq!(app.input.replay_depth(), 0);
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        2
    );
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "bdef"
    );
}

#[test]
fn failed_pending_command_is_not_recorded_and_releases_later_keys() {
    let mut session = pending_session();
    let app = &mut session.application;
    let before = app.input.repeat_plan(100, None).unwrap();
    keys(app, &["q", "a", "x", "l", "q"]);
    let id = app.input.pending_input.unwrap();
    app.complete_input_command(id, false, false);
    assert!(!app.input.command_pending());
    assert_eq!(app.input.repeat_plan(100, None).unwrap(), before);
    let recorded = app.state.registers.get_macro('a').unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].id.as_str(), "view.cursor.move");
}

fn wrapped_session() -> crate::app::tests::driver::TestSession {
    use crate::editor::{CommandAuthority, CommandWrapperAction};
    let mut session = pending_session();
    let app = &mut session.application;
    let id = crate::editor::CommandId::new("edit.delete_char").unwrap();
    app.commands
        .register_wrapper(
            &id,
            "wrapper".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, result| {
                Ok(match result {
                    None => CommandWrapperAction::Pending(vec![]),
                    Some(result) => CommandWrapperAction::Complete(result),
                })
            },
        )
        .unwrap();
    let mut layer = crate::editor::input::Keymap::default();
    layer.insert(
        &[crate::editor::input::KeyEvent::character('x')],
        CommandInvocation::new("edit.delete_char", InvocationSource::Keymap).unwrap(),
    );
    app.process_effect(crate::app::commands::AppEffect::Input(
        crate::app::input::InputCommand::PushLayer(Box::new(layer)),
    ));
    session
}

fn resume_wrapper(app: &mut crate::app::App) -> bool {
    let pending = app.input.pending_input_command().unwrap();
    let invocation = app.input.pending_invocation(pending).unwrap().invocation;
    app.process_effect_checked(crate::app::commands::AppEffect::CommandResume {
        pending,
        owner: "wrapper".into(),
        action: Box::new(crate::editor::CommandWrapperAction::CallNext(invocation)),
    })
}

#[test]
fn wrapper_pending_holds_keys_and_replays_them_in_order() {
    let mut session = wrapped_session();
    let app = &mut session.application;
    let buffer = app.state.active_buffer_id().unwrap();
    keys(app, &["x", "i", "1", "2", "escape"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abcdef");
    assert!(resume_wrapper(app));
    assert!(!app.input.command_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "12bcdef");
}

#[test]
fn wrapper_completion_records_dot_once_and_dot_waits_before_later_keys() {
    let mut session = wrapped_session();
    let app = &mut session.application;
    let before = app.input.repeat_plan(100, None).unwrap();
    keys(app, &["x", ".", "l"]);
    assert_eq!(app.input.repeat_plan(100, None).unwrap(), before);
    assert!(resume_wrapper(app));
    let plan = app.input.repeat_plan(100, None).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].id.as_str(), "edit.delete_char");
    assert!(app.input.command_pending());
    assert!(resume_wrapper(app));
    assert!(!app.input.command_pending());
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "cdef"
    );
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        1
    );
}

#[test]
fn wrapper_completion_records_one_macro_step_and_replay_waits() {
    let mut session = wrapped_session();
    let app = &mut session.application;
    keys(app, &["q", "a", "x", "l", "q"]);
    assert!(app.input.recording_macro.as_ref().unwrap().1.is_empty());
    assert!(resume_wrapper(app));
    let recorded = app.state.registers.get_macro('a').unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].id.as_str(), "edit.delete_char");
    keys(app, &["@", "a"]);
    assert!(app.input.command_pending());
    assert_eq!(app.input.replay_depth(), 1);
    assert!(resume_wrapper(app));
    assert_eq!(app.input.replay_depth(), 0);
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "bdef"
    );
}

#[test]
fn failed_wrapper_resume_releases_keys_without_recording_the_command() {
    let mut session = wrapped_session();
    let app = &mut session.application;
    let before = app.input.repeat_plan(100, None).unwrap();
    keys(app, &["q", "a", "x", "l", "q"]);
    let pending = app.input.pending_input_command().unwrap();
    assert!(
        !app.process_effect_checked(crate::app::commands::AppEffect::CommandResume {
            pending,
            owner: "wrapper".into(),
            action: Box::new(crate::editor::CommandWrapperAction::CallNext(
                CommandInvocation::new("edit.insert_text", InvocationSource::Test).unwrap()
            )),
        })
    );
    assert!(!app.input.command_pending());
    assert_eq!(app.input.repeat_plan(100, None).unwrap(), before);
    let recorded = app.state.registers.get_macro('a').unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].id.as_str(), "view.cursor.move");
}

#[test]
fn unloaded_wrapper_releases_held_keys() {
    let mut session = wrapped_session();
    let app = &mut session.application;
    keys(app, &["x", "l"]);
    app.commands.unregister_plugin("wrapper");
    app.flush_keymap_timeout();
    assert!(!app.input.command_pending());
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        1
    );
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "abcdef"
    );
}

#[test]
fn incoming_lsp_wait_does_not_replace_or_release_the_keyboard_command() {
    use crate::app::commands::AppEffect;
    use crate::editor::{CommandAuthority, CommandValue, CommandWrapperAction};
    let mut session = wrapped_session();
    let app = &mut session.application;
    let command = crate::app::language::incoming::command_id("window/showMessage");
    app.commands
        .register_wrapper(
            &crate::editor::CommandId::new(&command).unwrap(),
            "filter".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, _| Ok(CommandWrapperAction::Pending(Vec::new())),
        )
        .unwrap();
    keys(app, &["x", "l"]);
    let keyboard = app.input.pending_input_command().unwrap();
    let incoming = CommandInvocation::new(&command, InvocationSource::Startup)
        .unwrap()
        .with_argument("event", CommandValue::Integer(42))
        .with_argument("method", CommandValue::String("window/showMessage".into()))
        .with_argument("server", CommandValue::String("server".into()));
    assert!(app.invoke_checked(incoming));
    assert!(app.incoming_command_pending(42));
    assert_eq!(app.input.pending_input_command(), Some(keyboard));
    let background = *app
        .input
        .pending_commands
        .keys()
        .find(|id| **id != keyboard)
        .unwrap();
    assert!(app.process_effect_checked(AppEffect::CommandResume {
        pending: background,
        owner: "filter".into(),
        action: Box::new(CommandWrapperAction::Complete(
            crate::editor::CommandOutcome::empty()
        )),
    }));
    assert!(!app.incoming_command_pending(42));
    assert_eq!(app.input.pending_input_command(), Some(keyboard));
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "abcdef"
    );
    assert!(resume_wrapper(app));
    assert!(!app.input.command_pending());
    assert!(app.input.pending_commands.is_empty());
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "bcdef"
    );
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()]
            .cursor
            .get(),
        1
    );
}

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
    let mut input = InputState::new(100);
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
    let mut input = InputState::new(100);
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
    let mut input = InputState::new(100);
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
        let mut input = InputState::new(100);
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
    let mut input = InputState::new(100);
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
    let mut input = InputState::new(100);
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
    let mut input = InputState::new(100);
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
