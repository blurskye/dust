//! Tests for typed command calls, wrapper ordering, and access checks.

use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::editor::command::{
    ArgumentSchema, ArgumentType, CommandAccess, CommandAuthority, CommandError, CommandId,
    CommandInvocation, CommandOutcome, CommandRegistry, CommandSpec, CommandValue,
    CommandWrapperAction, InvocationSource, register_builtin_commands,
};
use crate::editor::{EditorState, text::TextBuffer};

fn plugin(grants: &[CommandAccess]) -> CommandAuthority {
    CommandAuthority::Plugin {
        initiator: "example".into(),
        grants: grants.iter().copied().collect(),
    }
}

fn editing() -> (EditorState, CommandRegistry<()>, CommandInvocation) {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(TextBuffer::from_text(""), None).unwrap();
    let mut registry = CommandRegistry::default();
    register_builtin_commands(&mut registry).unwrap();
    let mut invocation = CommandInvocation::new("edit.insert_text", InvocationSource::Test)
        .unwrap()
        .with_argument("text", CommandValue::String("a".into()));
    invocation.target.view = Some(view);
    invocation.target.buffer = Some(buffer);
    (state, registry, invocation)
}

#[test]
fn typed_arguments_and_results_round_trip_with_target_count_and_register() {
    let (state, mut registry, mut invocation) = editing();
    invocation.id = CommandId::new("editor.echo").unwrap();
    invocation.count = NonZeroU64::new(3);
    invocation.register = Some('a');
    let value = CommandValue::Record(
        [
            ("name".into(), CommandValue::String("hello".into())),
            ("enabled".into(), CommandValue::Bool(true)),
            (
                "items".into(),
                CommandValue::List(vec![
                    CommandValue::Integer(42),
                    CommandValue::Number(serde_json::Number::from_f64(1.5).unwrap()),
                    CommandValue::Record(Default::default()),
                ]),
            ),
        ]
        .into(),
    );
    invocation.arguments = [("value".into(), value.clone())].into();
    let expected = invocation.clone();
    let mut spec = CommandSpec::builtin("editor.echo", "Echo").unwrap();
    spec.arguments = ArgumentSchema::default().field("value", ArgumentType::Record, true);
    spec.result = Some(ArgumentType::Record);
    registry
        .register(spec, move |_, invocation| {
            assert_eq!(invocation, &expected);
            Ok(CommandOutcome {
                value: Some(invocation.arguments["value"].clone()),
                ..CommandOutcome::empty()
            })
        })
        .unwrap();
    let result = registry
        .execute(&state, &invocation)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(result, value);
    let items = result.as_record().unwrap()["items"].as_list().unwrap();
    assert_eq!(items[0].as_number().unwrap(), serde_json::Number::from(42));
    assert_eq!(items[1].as_number().unwrap().as_f64(), Some(1.5));
    invocation
        .arguments
        .insert("value".into(), CommandValue::Bool(false));
    assert!(matches!(
        registry.execute(&state, &invocation),
        Err(CommandError::InvalidArguments(_))
    ));
}

#[test]
fn typed_results_reject_wrong_handler_and_wrapper_values() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let invocation = CommandInvocation::new("editor.answer", InvocationSource::Test).unwrap();
    let mut spec = CommandSpec::builtin("editor.answer", "Answer").unwrap();
    spec.result = Some(ArgumentType::Bool);
    spec.access = BTreeSet::new();
    let mut registry = CommandRegistry::<()>::default();
    registry
        .register(spec, |_, _| Ok(CommandOutcome::empty()))
        .unwrap();
    assert!(matches!(
        registry.execute(&state, &invocation),
        Err(CommandError::Failed(_))
    ));
    registry
        .register_wrapper(
            &invocation.id,
            "wrong".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, _| {
                Ok(CommandWrapperAction::Complete(CommandOutcome {
                    value: Some(CommandValue::String("not a bool".into())),
                    ..CommandOutcome::empty()
                }))
            },
        )
        .unwrap();
    assert!(matches!(
        registry.execute(&state, &invocation),
        Err(CommandError::Failed(_))
    ));
}

#[test]
fn two_wrappers_run_around_a_builtin_in_order_and_replace_its_result() {
    let (mut state, mut registry, invocation) = editing();
    let calls = Arc::new(Mutex::new(Vec::new()));
    // Register in reverse order to prove the explicit order, not the insertion order, wins.
    for (name, order, suffix) in [("inner", 20, "c"), ("outer", 10, "b")] {
        let calls = Arc::clone(&calls);
        registry
            .register_wrapper(
                &invocation.id,
                name.into(),
                order,
                CommandAuthority::Host,
                move |_, invocation, _, result| {
                    if let Some(mut result) = result {
                        calls.lock().unwrap().push(format!("{name}:after"));
                        result.value = Some(CommandValue::String(name.into()));
                        Ok(CommandWrapperAction::Complete(result))
                    } else {
                        calls.lock().unwrap().push(format!("{name}:before"));
                        let mut invocation = invocation.clone();
                        let text = format!("{}{suffix}", invocation.arguments["text"].as_str()?);
                        invocation
                            .arguments
                            .insert("text".into(), CommandValue::String(text));
                        Ok(CommandWrapperAction::CallNext(invocation))
                    }
                },
            )
            .unwrap();
    }
    let outcome = registry.execute(&state, &invocation).unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        ["outer:before", "inner:before", "inner:after", "outer:after"]
    );
    assert_eq!(outcome.value, Some(CommandValue::String("outer".into())));
    state
        .apply_transaction(outcome.transaction.as_ref().unwrap())
        .unwrap();
    assert_eq!(
        state.buffers[invocation.target.buffer.unwrap()]
            .text
            .to_string(),
        "abc"
    );
}

#[test]
fn call_next_resumes_only_the_waiting_invocation_and_cannot_be_reused() {
    let (mut state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "delayed".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, result| {
                Ok(match result {
                    None => CommandWrapperAction::Pending(vec![]),
                    Some(mut result) => {
                        result.value = Some(CommandValue::Bool(true));
                        CommandWrapperAction::Complete(result)
                    }
                })
            },
        )
        .unwrap();
    let pending = registry
        .execute(&state, &invocation)
        .unwrap()
        .pending
        .unwrap();
    let other = registry
        .execute(&state, &invocation)
        .unwrap()
        .pending
        .unwrap();
    assert_ne!(pending, other);
    assert!(matches!(
        registry.resume(
            &state,
            pending,
            "intruder",
            CommandWrapperAction::CallNext(invocation.clone())
        ),
        Err(CommandError::WrongWrapper)
    ));
    assert!(registry.is_pending(pending));
    let mut changed = invocation.clone();
    changed
        .arguments
        .insert("text".into(), CommandValue::String("resumed".into()));
    let result = registry
        .resume(
            &state,
            pending,
            "delayed",
            CommandWrapperAction::CallNext(changed),
        )
        .unwrap();
    assert_eq!(result.value, Some(CommandValue::Bool(true)));
    state
        .apply_transaction(result.transaction.as_ref().unwrap())
        .unwrap();
    assert_eq!(
        state.buffers[invocation.target.buffer.unwrap()]
            .text
            .to_string(),
        "resumed"
    );
    assert!(matches!(
        registry.resume(
            &state,
            pending,
            "delayed",
            CommandWrapperAction::CallNext(invocation)
        ),
        Err(CommandError::UnknownPending)
    ));
    assert!(registry.is_pending(other));
    assert!(registry.cancel(other));
    assert!(!registry.cancel(other));
}

#[test]
fn skipping_a_builtin_leaves_text_unchanged() {
    let (state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "skip".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, _| {
                Ok(CommandWrapperAction::Complete(CommandOutcome {
                    value: Some(CommandValue::String("skipped".into())),
                    ..CommandOutcome::empty()
                }))
            },
        )
        .unwrap();
    let outcome = registry.execute(&state, &invocation).unwrap();
    assert!(outcome.transaction.is_none());
    assert_eq!(outcome.value, Some(CommandValue::String("skipped".into())));
    assert_eq!(
        state.buffers[invocation.target.buffer.unwrap()]
            .text
            .to_string(),
        ""
    );
}

#[test]
fn dispatch_refuses_missing_access_before_a_wrapper_can_skip_the_command() {
    let (state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "skip".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, _| panic!("access must be checked first"),
        )
        .unwrap();
    assert!(matches!(
        registry.execute_with_authority(&state, &invocation, plugin(&[CommandAccess::Ui])),
        Err(CommandError::AccessDenied(_))
    ));
    let mut plugin_call = invocation;
    plugin_call.source = InvocationSource::Plugin;
    assert!(matches!(
        registry.execute(&state, &plugin_call),
        Err(CommandError::AccessDenied(_))
    ));
}

#[test]
fn host_only_commands_cannot_be_wrapped_or_called_with_plugin_grants() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let mut registry = CommandRegistry::<()>::default();
    let spec = CommandSpec::builtin("workspace.trust", "Trust").unwrap();
    let invocation = CommandInvocation::new("workspace.trust", InvocationSource::Plugin).unwrap();
    registry
        .register(spec, |_, _| panic!("host-only handler"))
        .unwrap();
    let authority = plugin(&[
        CommandAccess::HostOnly,
        CommandAccess::Edit,
        CommandAccess::Ui,
        CommandAccess::Files,
        CommandAccess::Process,
        CommandAccess::Network,
    ]);
    assert!(matches!(
        registry.execute_with_authority(&state, &invocation, authority.clone()),
        Err(CommandError::AccessDenied(_))
    ));
    assert!(matches!(
        registry.register_wrapper(
            &invocation.id,
            "plugin".into(),
            0,
            authority,
            |_, _, _, _| Ok(CommandWrapperAction::Pending(vec![]))
        ),
        Err(CommandError::AccessDenied(_))
    ));
}

#[test]
fn wrapper_delegation_keeps_the_initiator_and_never_adds_grants() {
    let (state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "example".into(),
            0,
            plugin(&[CommandAccess::Edit, CommandAccess::Process]),
            |_, invocation, _, result| {
                Ok(match result {
                    None => CommandWrapperAction::CallNext(invocation.clone()),
                    Some(result) => CommandWrapperAction::Complete(result),
                })
            },
        )
        .unwrap();
    let authority = CommandAuthority::Plugin {
        initiator: "caller".into(),
        grants: [CommandAccess::Edit].into(),
    };
    let outcome = registry
        .execute_with_authority(&state, &invocation, authority.clone())
        .unwrap();
    assert_eq!(outcome.authority, authority);
}

#[test]
fn unregistering_a_plugin_cancels_its_pending_wrappers_and_restores_the_builtin() {
    let (state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "example".into(),
            0,
            CommandAuthority::Host,
            |_, _, _, _| Ok(CommandWrapperAction::Pending(vec![])),
        )
        .unwrap();
    let pending = registry
        .execute(&state, &invocation)
        .unwrap()
        .pending
        .unwrap();
    registry.unregister_plugin("example");
    assert!(!registry.is_pending(pending));
    let result = registry.execute(&state, &invocation).unwrap();
    assert!(result.pending.is_none());
    assert!(result.transaction.is_some());
}

#[test]
fn call_next_revalidates_changed_arguments_before_running_the_builtin() {
    let (state, mut registry, invocation) = editing();
    registry
        .register_wrapper(
            &invocation.id,
            "bad".into(),
            0,
            CommandAuthority::Host,
            |_, invocation, _, _| {
                let mut invocation = invocation.clone();
                invocation
                    .arguments
                    .insert("text".into(), CommandValue::Bool(true));
                Ok(CommandWrapperAction::CallNext(invocation))
            },
        )
        .unwrap();
    assert!(matches!(
        registry.execute(&state, &invocation),
        Err(CommandError::InvalidArguments(_))
    ));
}
