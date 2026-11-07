//! Real component boundary tests for the ABI translator.

use super::*;
use crate::app::tests::app_with_plugin_queue;
use crate::editor::text::TextBuffer;
use sha2::{Digest, Sha256};

pub(super) fn real_app(owners: &[&str]) -> (Vec<tempfile::TempDir>, App) {
    real_app_with(owners, &[], &[])
}

pub(super) fn real_app_with(
    owners: &[&str],
    added: &[PluginCapability],
    denied: &[PluginCapability],
) -> (Vec<tempfile::TempDir>, App) {
    let (dirs, mut app) = app_with_plugin_queue(16);
    let mut roots = vec![dirs.0, dirs.1];
    app.state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let core = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    for owner in owners {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guest.wasm"), &component).unwrap();
        let mut manifest =
            crate::app::plugins::fixture_manifest(format!("{:x}", Sha256::digest(&component)));
        manifest.id = (*owner).into();
        manifest.contributions = Default::default();
        manifest.capabilities.retain(|cap| {
            !matches!(
                cap,
                PluginCapability::PluginStorage
                    | PluginCapability::PickerProviders
                    | PluginCapability::VirtualDocuments
                    | PluginCapability::PrivateCells
            )
        });
        manifest.capabilities.extend([
            PluginCapability::Commands,
            PluginCapability::Input,
            PluginCapability::Decorations,
            PluginCapability::Completion,
            PluginCapability::Timers,
            PluginCapability::Http,
        ]);
        manifest.capabilities.extend(added.iter().cloned());
        manifest.capabilities.retain(|cap| !denied.contains(cap));
        manifest
            .commands
            .get_mut("drive")
            .unwrap()
            .arguments
            .insert(
                "abi".into(),
                crate::plugins::PluginArgumentDeclaration {
                    kind: crate::plugins::PluginArgumentKind::String,
                    required: false,
                },
            );
        crate::app::tests::write_plugin_manifest(root.path(), &manifest);
        let load = app
            .extension
            .index_from_store(
                (*owner).into(),
                root.path().into(),
                "manifest.toml".into(),
                "guest.wasm".into(),
                app.plugin_limits(),
            )
            .unwrap();
        pump(&mut app, |app| app.extension.is_active_load(owner, load));
        roots.push(root);
    }
    (roots, app)
}

pub(super) fn pump(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.process_plugin_events();
        if done(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "plugin callback timed out: {:?}",
            app.ui.notification
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn action(app: &mut App, owner: &str, action: &str) {
    let invocation =
        CommandInvocation::new(&format!("plugin.{owner}.drive"), InvocationSource::Test)
            .unwrap()
            .with_argument("abi", CommandValue::String(action.into()));
    assert!(app.invoke_checked(invocation));
    pump(app, |app| !app.input.command_pending());
}

#[test]
fn structured_values_round_trip_without_losing_names_or_nested_types() {
    let value = CommandValue::Record(BTreeMap::from([
        ("text".into(), CommandValue::String("λ".into())),
        (
            "items".into(),
            CommandValue::List(vec![
                CommandValue::Integer(i64::MAX),
                CommandValue::Bool(true),
                CommandValue::Record(BTreeMap::new()),
            ]),
        ),
    ]));
    assert_eq!(decode_value(encode_value(&value)).unwrap(), value);
}

#[test]
fn malformed_values_reject_cycles_aliases_and_duplicate_record_names() {
    for nodes in [
        vec![wit::ValueNode::Sequence(vec![0])],
        vec![
            wit::ValueNode::Integer(1),
            wit::ValueNode::Sequence(vec![0, 0]),
        ],
        vec![
            wit::ValueNode::Integer(1),
            wit::ValueNode::Integer(2),
            wit::ValueNode::Object(vec![
                wit::ValueField {
                    name: "x".into(),
                    value: 0,
                },
                wit::ValueField {
                    name: "x".into(),
                    value: 1,
                },
            ]),
        ],
    ] {
        assert!(decode_value(wit::CommandValue { nodes }).is_err());
    }
}

#[test]
fn invocation_keeps_targets_counts_registers_and_named_arguments() {
    let mut invocation = CommandInvocation::new("edit.insert_text", InvocationSource::Keymap)
        .unwrap()
        .with_argument("text", CommandValue::String("x".into()));
    invocation.count = std::num::NonZeroU64::new(7);
    invocation.register = Some('a');
    let decoded = super::invocation(&command_call(&invocation, None)).unwrap();
    assert_eq!(decoded.arguments, invocation.arguments);
    assert_eq!(decoded.count, invocation.count);
    assert_eq!(decoded.register, invocation.register);
}

#[test]
fn real_guest_registers_a_typed_command_and_returns_nested_values() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "register");
    let value = CommandValue::Record(BTreeMap::from([(
        "items".into(),
        CommandValue::List(vec![CommandValue::Integer(42), CommandValue::Bool(true)]),
    )]));
    let invocation = CommandInvocation::new("plugin.demo.echo", InvocationSource::Test)
        .unwrap()
        .with_argument("abi", CommandValue::String("echo".into()))
        .with_argument("value", value.clone());
    app.extension.abi.running = Some(("demo".into(), 99));
    assert!(app.invoke_checked(invocation));
    pump(&mut app, |app| !app.input.command_pending());
    let reply = app.extension.abi.replies["demo"].last().unwrap();
    assert!(reply.error.is_none());
    let result = decode_value(reply.value.clone().unwrap()).unwrap();
    assert_eq!(result.as_record().unwrap()["value"], value);
}

#[test]
fn two_real_guest_wrappers_continue_the_same_builtin_and_hold_later_keys() {
    let (_roots, mut app) = real_app(&["demo", "other"]);
    action(&mut app, "demo", "wrap");
    action(&mut app, "other", "wrap");
    app.set_mode(crate::editor::EditorMode::Insert);
    let buffer = app.state.active_buffer_id().unwrap();
    assert!(
        app.invoke_checked(
            CommandInvocation::new("edit.insert_text", InvocationSource::Test)
                .unwrap()
                .with_argument("text", CommandValue::String("x".into()))
        )
    );
    assert!(app.input.command_pending());
    let pending = app.input.pending_input_command().unwrap();
    app.handle_configured_key("z".parse().unwrap());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    pump(&mut app, |app| {
        !app.input.command_pending() && app.extension.abi.waiting.is_empty()
    });
    assert_eq!(app.state.buffers[buffer].text.to_string(), "xzabc");
    assert!(app.input.pending_invocation(pending).is_none());
    assert!(!app.commands.is_pending(pending));
}

#[test]
fn real_guest_modes_key_capture_layers_and_unload_use_native_input() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "mode");
    assert_eq!(app.editor_input_mode().unwrap().label(), "REVIEW");
    action(&mut app, "demo", "capture");
    assert_eq!(app.editor_input_mode().unwrap().base(), None);
    action(&mut app, "demo", "layer");
    assert_eq!(app.extension.abi.layers.len(), 1);
    let load = app.extension.load_id("demo").unwrap();
    app.extension.disable("demo").unwrap();
    app.remove_plugin_contributions("demo", load);
    assert!(app.extension.abi.layers.is_empty());
    assert_ne!(app.editor_input_mode().unwrap().label(), "CAPTURE");
}

#[test]
fn real_guest_anchors_follow_native_edits_and_undo_and_are_removed_on_unload() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "marks");
    let buffer = app.state.active_buffer_id().unwrap();
    app.set_mode(crate::editor::EditorMode::Insert);
    app.invoke(
        CommandInvocation::new("edit.insert_text", InvocationSource::Test)
            .unwrap()
            .with_argument("text", CommandValue::String("λ".into())),
    );
    let delivery = app.plugin_delivery("demo").unwrap();
    assert_eq!(delivery.marks[0].marks[0].start, 2);
    app.invoke(CommandInvocation::new("history.undo", InvocationSource::Test).unwrap());
    assert_eq!(
        app.plugin_delivery("demo").unwrap().marks[0].marks[0].start,
        0
    );
    let load = app.extension.load_id("demo").unwrap();
    app.extension.disable("demo").unwrap();
    app.remove_plugin_contributions("demo", load);
    assert!(app.extension.abi.marks.is_empty());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
}

#[test]
fn real_guest_subscribes_to_occurrences_and_receives_a_snapshot_after_a_gap() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "events");
    pump(&mut app, |app| {
        app.extension.abi.subscriptions["demo"].1.is_some() && app.extension.abi.waiting.is_empty()
    });
    let first = app.extension.abi.subscriptions["demo"].1.unwrap();
    let buffer = app.state.active_buffer_id().unwrap();
    for _ in 0..300 {
        let revision = app.state.buffers[buffer].text.revision();
        app.state
            .mark_buffer_saved_as(buffer, revision, b"abc".to_vec(), None);
    }
    let batch = events::event_batch(&app.state, Some(first)).unwrap();
    assert!(batch.snapshot.is_some());
    assert!(batch.records.is_empty());
    pump(&mut app, |app| {
        app.extension.abi.subscriptions["demo"].1 == Some(batch.sequence)
            && app.extension.abi.waiting.is_empty()
    });
    assert!(
        events::event_batch(&app.state, Some(batch.sequence))
            .unwrap()
            .records
            .is_empty()
    );
}

#[test]
fn real_guest_publishes_menu_and_inline_completions_through_one_pipeline() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "completion");
    app.set_mode(crate::editor::EditorMode::Insert);
    pump(&mut app, |app| {
        app.language
            .completion_items()
            .iter()
            .any(|item| item.label == "plugin-value")
    });
    assert_eq!(
        app.language.inline_completion(&app.state).as_deref(),
        Some("plugin-value")
    );
    let buffer = app.state.active_buffer_id().unwrap();
    app.accept_lsp_completion();
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "plugin-valueabc"
    );
}

#[test]
fn real_guest_runs_native_commands_and_an_ordinary_error_does_not_unload_it() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "run");
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "runabc"
    );
    action(&mut app, "demo", "error");
    assert!(matches!(
        app.extension.statuses()["demo"],
        crate::plugins::PluginStatus::Active(_)
    ));
    assert!(
        !app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("must-not-publish"))
    );
    action(&mut app, "demo", "mode");
    assert_eq!(app.editor_input_mode().unwrap().label(), "REVIEW");
}

#[test]
fn real_guest_native_edits_record_the_completed_command_for_dot() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "run");
    let repeat = app.input.repeat_plan(100, None).unwrap();
    assert_eq!(repeat.len(), 1);
    assert_eq!(repeat[0].id.as_str(), "plugin.demo.drive");
    app.handle_configured_key(".".parse().unwrap());
    assert!(app.input.command_pending());
    pump(&mut app, |app| !app.input.command_pending());
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "runrunabc"
    );
}

#[test]
fn plugin_command_requests_allow_granted_effects_but_refuse_host_only_effects() {
    let (_roots, mut app) = real_app(&["demo"]);
    let authority = CommandAuthority::Plugin {
        initiator: "demo".into(),
        grants: [CommandAccess::Ui].into(),
    };
    for (name, effect, allowed) in [
        ("test.dismiss", AppEffect::NotificationDismiss, true),
        ("test.quit", AppEffect::Quit { force: true }, false),
    ] {
        let mut spec = crate::editor::CommandSpec::builtin(name, "Effect").unwrap();
        spec.access = [CommandAccess::Ui].into();
        app.commands
            .register(spec, move |_, _| {
                Ok(CommandOutcome {
                    effects: vec![effect.clone()],
                    ..CommandOutcome::empty()
                })
            })
            .unwrap();
        app.show_error("visible notice");
        let call = command_call(
            &CommandInvocation::new(name, InvocationSource::Plugin).unwrap(),
            None,
        );
        app.apply_editor_request("demo", &authority, EditorRequest::Run(42, call))
            .unwrap();
        let reply = app
            .extension
            .abi
            .replies
            .get_mut("demo")
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(reply.error.is_none(), allowed);
        assert_eq!(app.ui.notification.is_none(), allowed);
        assert!(!app.should_quit);
        assert_eq!(app.effect_authority, CommandAuthority::Host);
    }
}

#[test]
fn real_guest_timers_fire_once_and_http_requires_an_approved_host() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "timer");
    pump(&mut app, |app| {
        app.extension.abi.timers.is_empty() && app.extension.abi.waiting.is_empty()
    });
    assert!(app.extension.abi.timers.is_empty());
    action(&mut app, "demo", "http-denied");
    assert!(app.ui.notification.as_deref().is_some_and(
        |notice| notice.contains("trusted workspace") || notice.contains("not granted")
    ));
}

#[test]
fn real_guest_lsp_filters_wait_for_call_next_or_suppress_the_message() {
    for (action_name, expected) in [("filter-pass", true), ("filter-drop", false)] {
        let (_roots, mut app) = real_app(&["demo"]);
        let path = app.state.workspace_root.join("filter.rs");
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("abc"), Some(path))
            .unwrap();
        let endpoint = crate::app::language::tests::fixture_owned_lsp(&mut app, "server", buffer);
        action(&mut app, "demo", action_name);
        app.route_lsp_event(crate::language::lsp::OwnedLspEvent {
            event: crate::language::lsp::LspEvent::Log {
                server: "server".into(),
                method: Some("window/showMessage".into()),
                message: "filtered message".into(),
            },
            context: endpoint.context(),
            reply: None,
        });
        assert!(
            !app.input.command_pending(),
            "server messages must not hold keyboard input"
        );
        let pending = app
            .extension
            .abi
            .waiting
            .values()
            .find_map(|call| call.pending)
            .unwrap();
        assert!(app.input.pending_invocation(pending).is_some());
        assert!(app.incoming_command_pending(1));
        pump(&mut app, |app| {
            app.input.pending_invocation(pending).is_none()
        });
        assert!(!app.incoming_command_pending(1));
        assert_eq!(
            app.language
                .logs()
                .iter()
                .any(|line| line.contains("filtered message")),
            expected
        );
    }
}

#[test]
fn restricted_calls_cannot_wrap_host_only_commands_or_amplify_edit_grants() {
    let (_roots, mut app) = real_app(&["demo"]);
    let authority = CommandAuthority::Plugin {
        initiator: "demo".into(),
        grants: BTreeSet::new(),
    };
    assert!(
        app.install_plugin_wrapper(
            "demo",
            wit::CommandWrapper {
                command: "edit.insert_text".into(),
                handler: "drive".into(),
                order: 0
            },
            authority.clone()
        )
        .is_err()
    );
    let invocation = CommandInvocation::new("edit.insert_text", InvocationSource::Plugin)
        .unwrap()
        .with_argument("text", CommandValue::String("denied".into()));
    assert!(!app.invoke_with_authority(invocation, authority));
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .text
            .to_string(),
        "abc"
    );
    let mut spec = crate::editor::CommandSpec::builtin("editor.protected", "Host only").unwrap();
    spec.context = crate::editor::ContextRequirement::Application;
    app.commands
        .register(
            spec,
            |_, _| -> Result<CommandOutcome<AppEffect>, crate::editor::CommandError> {
                panic!("host-only command must not run")
            },
        )
        .unwrap();
    assert!(
        app.install_plugin_wrapper(
            "demo",
            wit::CommandWrapper {
                command: "editor.protected".into(),
                handler: "drive".into(),
                order: 0
            },
            app.plugin_authority("demo")
        )
        .is_err()
    );
}

#[test]
fn expired_callback_drops_late_edits_without_unloading_the_component() {
    let (_roots, mut app) = real_app(&["demo"]);
    assert!(
        app.invoke_checked(
            CommandInvocation::new("plugin.demo.drive", InvocationSource::Test)
                .unwrap()
                .with_argument("abi", CommandValue::String("marks".into()))
        )
    );
    for waiting in app.extension.abi.waiting.values_mut() {
        waiting.deadline = Instant::now() - Duration::from_secs(1);
    }
    app.poll_plugin_callbacks();
    assert!(!app.input.command_pending());
    assert!(app.extension.abi.marks.is_empty());
    assert!(matches!(
        app.extension.statuses()["demo"],
        crate::plugins::PluginStatus::Active(_)
    ));
    action(&mut app, "demo", "mode");
    assert!(app.extension.abi.marks.is_empty());
    assert_eq!(app.editor_input_mode().unwrap().label(), "REVIEW");
}

#[test]
fn fixture_wit_is_the_production_contract() {
    assert_eq!(
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/wit/dust-plugin.wit")),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/retained-guest/wit/dust-plugin.wit"
        ))
    );
}
