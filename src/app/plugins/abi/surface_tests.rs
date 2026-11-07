//! Surface capabilities through the retained Wasm guest and native owners.

use super::*;
use crate::app::plugins::abi::tests::{action, pump, real_app, real_app_with};
use crate::plugins::PluginCapability;

fn settle(app: &mut App) {
    pump(app, |app| app.extension.abi.waiting.is_empty());
}

#[test]
fn guest_input_select_and_notifications_use_native_ui_and_return_replies() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-secret");
    assert_eq!(app.input.prompt().unwrap().input, "initial");
    assert_eq!(app.ui.prompt.as_ref().unwrap().input, "*******");
    app.submit_prompt();
    settle(&mut app);
    assert!(
        app.ui.notification.as_deref().unwrap().contains("initial"),
        "{:?}",
        app.ui.notification
    );
    action(&mut app, "demo", "surface-input");
    app.close_prompt();
    settle(&mut app);
    assert!(
        app.ui
            .notification
            .as_deref()
            .unwrap()
            .contains("value: None")
    );
    action(&mut app, "demo", "surface-select");
    assert_eq!(app.ui.picker.as_ref().unwrap().title, "Fixture select");
    app.accept_picker();
    settle(&mut app);
    assert!(app.ui.notification.as_deref().unwrap().contains("one"));
    action(&mut app, "demo", "surface-notify");
    assert_eq!(app.ui.notification.as_deref(), Some("surface warning"));
    assert!(app.ui.notification_warning);
}

#[test]
fn guest_presenters_receive_native_prompts_but_never_confirmations() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-presenters");
    let (reply, result) = sync_channel(1);
    app.request_text_input("Native input".into(), String::new(), false, reply)
        .unwrap();
    assert!(app.ui.prompt.is_none());
    pump(&mut app, |app| {
        app.extension.abi.surfaces.callbacks.is_empty() && app.extension.abi.waiting.is_empty()
    });
    app.process_input_replacement();
    assert_eq!(
        result.try_recv().ok(),
        Some(Some("presented".into())),
        "notices: {:?}",
        app.notices.history().collect::<Vec<_>>()
    );
    let (reply, result) = sync_channel(1);
    app.request_item_select(
        "Native select".into(),
        vec![("one".into(), "First".into(), "Detail".into())],
        reply,
    )
    .unwrap();
    settle(&mut app);
    app.process_select_replacement();
    assert_eq!(result.try_recv().unwrap(), Some("one".into()));
    app.open_confirmation(
        crate::ui::PromptKind::WorkspaceTrust,
        "Trust?".into(),
        "Host permission".into(),
        crate::ui::ConfirmationChoice::No,
    );
    assert!(app.ui.prompt.as_ref().unwrap().confirmation.is_some());
    assert!(!app.input_replacement_pending());
    app.notify_warning("Native notice".into());
    assert!(app.ui.notification.is_none());
    assert!(
        app.notices
            .history()
            .any(|(_, message)| message == "Native notice")
    );
    settle(&mut app);
    app.remove_plugin_abi("demo");
    app.notify_message("Host restored".into(), false);
    assert_eq!(app.ui.notification.as_deref(), Some("Host restored"));
}

#[test]
fn guest_editable_document_save_accepts_rejects_and_reuses_the_buffer() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-document");
    settle(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    app.set_mode(crate::editor::EditorMode::Insert);
    app.invoke(
        crate::editor::CommandInvocation::new(
            "edit.insert_text",
            crate::editor::InvocationSource::Test,
        )
        .unwrap()
        .with_argument("text", text("new")),
    );
    assert!(app.state.buffers[buffer].dirty);
    action(&mut app, "demo", "surface-document");
    settle(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(buffer));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "newold");
    app.start_save(false, None, false);
    settle(&mut app);
    app.process_save_work();
    assert!(
        !app.state.buffers[buffer].dirty,
        "{:?}",
        app.ui.notification
    );
    action(&mut app, "demo", "surface-reject");
    app.invoke(
        crate::editor::CommandInvocation::new(
            "edit.insert_text",
            crate::editor::InvocationSource::Test,
        )
        .unwrap()
        .with_argument("text", text("x")),
    );
    app.start_save(false, None, false);
    settle(&mut app);
    app.process_save_work();
    assert!(app.state.buffers[buffer].dirty);
    assert!(app.ui.notification.as_deref().unwrap().contains("refused"));
    assert!(app.state.buffers[buffer].path.is_none());
}

#[test]
fn guest_formatters_and_before_save_apply_edits_and_skip_errors() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-document");
    settle(&mut app);
    action(&mut app, "demo", "surface-formatter");
    action(&mut app, "demo", "surface-before");
    let buffer = app.state.active_buffer_id().unwrap();
    assert!(app.begin_format_on_save(false));
    pump(&mut app, |app| app.extension.abi.waiting.is_empty());
    app.process_save_work();
    settle(&mut app);
    app.process_save_work();
    settle(&mut app);
    app.process_save_work();
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "old!!",
        "{:?}",
        app.ui.notification
    );
    assert!(!app.state.buffers[buffer].dirty);
    action(&mut app, "demo", "surface-format-error");
    assert!(app.begin_format_on_save(false));
    settle(&mut app);
    app.process_save_work();
    settle(&mut app);
    app.process_save_work();
    settle(&mut app);
    app.process_save_work();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "old!!");
    assert!(!app.state.buffers[buffer].dirty);
}

#[test]
fn guest_chrome_segments_update_remove_and_unload_in_native_owner() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-segments");
    assert_eq!(
        app.ui.chrome.status_text["plugin.demo.status.fixture/0"].0,
        "Hello"
    );
    assert_eq!(
        app.ui.chrome.buffer_text["plugin.demo.buffer.fixture/1"].1,
        crate::ui::Style::Accent
    );
    assert!(
        app.window_session
            .segment_command("plugin.demo.status.fixture/0")
            .is_some()
    );
    app.window_session.observe_size((120, 40));
    app.reconcile_editor_windows();
    let view = app.state.active_view().unwrap();
    let column = (0..120)
        .find(|column| {
            crate::ui::render::chrome::status_segment_at_column(
                &app.state,
                view,
                &app.ui,
                app.configuration.settings(),
                120,
                40,
                *column,
            )
            .as_deref()
                == Some("plugin.demo.status.fixture/0")
        })
        .unwrap();
    app.show_error("click dismisses this");
    let click = crate::editor::input::KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseLeftDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_chrome_mouse(&click, column, 39));
    assert!(app.ui.notification.is_none());
    action(&mut app, "demo", "surface-segments-update");
    assert_eq!(
        app.ui.chrome.status_text["plugin.demo.status.fixture/0"].0,
        "Updated"
    );
    action(&mut app, "demo", "surface-segments-remove");
    assert!(app.ui.chrome.status_text.is_empty());
    action(&mut app, "demo", "surface-segments");
    app.remove_plugin_abi("demo");
    assert!(app.ui.chrome.status_text.is_empty());
    assert!(app.ui.chrome.buffer_text.is_empty());
}

#[test]
fn each_surface_import_denies_a_guest_without_the_required_grant() {
    let (_roots, mut app) = real_app_with(
        &["demo"],
        &[],
        &[
            PluginCapability::Input,
            PluginCapability::ShowMessage,
            PluginCapability::ProposeEdits,
        ],
    );
    for operation in [
        "input",
        "select",
        "notify",
        "presenters",
        "answer-input",
        "answer-select",
        "document",
        "formatter",
        "before",
        "segments",
        "segments-remove",
    ] {
        action(&mut app, "demo", &format!("surface-{operation}"));
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("denied")),
            "{operation}: {:?}",
            app.ui.notification
        );
    }
    assert!(app.extension.abi.surfaces.saves.is_empty());
    assert!(app.extension.abi.surfaces.segments.is_empty());
    assert!(app.input.prompt().is_none());
    assert!(app.ui.picker.is_none());
}

#[test]
fn guest_presenters_can_answer_later_from_panel_commands() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-presenters-deferred");
    let (reply, input) = sync_channel(1);
    app.request_text_input("Input".into(), String::new(), false, reply)
        .unwrap();
    settle(&mut app);
    assert_eq!(app.extension.abi.surfaces.deferred.len(), 1);
    assert!(input.try_recv().is_err());
    action(&mut app, "demo", "surface-answer-input");
    app.process_input_replacement();
    assert_eq!(input.try_recv().unwrap(), Some("later".into()));
    let (reply, select) = sync_channel(1);
    app.request_item_select(
        "Select".into(),
        vec![("one".into(), "First".into(), "Detail".into())],
        reply,
    )
    .unwrap();
    settle(&mut app);
    assert_eq!(app.extension.abi.surfaces.deferred.len(), 1);
    action(&mut app, "demo", "surface-answer-select");
    app.process_select_replacement();
    assert_eq!(select.try_recv().unwrap(), Some("one".into()));
    assert!(app.extension.abi.surfaces.deferred.is_empty());
}

#[test]
fn guest_presenter_callbacks_do_not_widen_the_requesters_grants() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-presenters");
    let authority = CommandAuthority::Plugin {
        initiator: "caller".into(),
        grants: [CommandAccess::Ui].into(),
    };
    let (reply, input) = sync_channel(1);
    app.effect_authority = authority.clone();
    app.request_text_input("Input".into(), String::new(), false, reply)
        .unwrap();
    app.effect_authority = CommandAuthority::Host;
    app.poll_plugin_callbacks();
    assert!(!app.extension.abi.waiting.is_empty());
    assert!(
        app.extension
            .abi
            .waiting
            .values()
            .all(|waiting| waiting.authority == authority)
    );
    settle(&mut app);
    app.process_input_replacement();
    assert_eq!(input.try_recv().unwrap(), Some("presented".into()));
    let (reply, select) = sync_channel(1);
    app.effect_authority = authority.clone();
    app.request_item_select(
        "Select".into(),
        vec![("one".into(), "First".into(), String::new())],
        reply,
    )
    .unwrap();
    app.effect_authority = CommandAuthority::Host;
    app.poll_plugin_callbacks();
    assert!(
        app.extension
            .abi
            .waiting
            .values()
            .all(|waiting| waiting.authority == authority)
    );
    settle(&mut app);
    app.process_select_replacement();
    assert_eq!(select.try_recv().unwrap(), Some("one".into()));
    app.effect_authority = authority.clone();
    app.notify_warning("From caller".into());
    app.effect_authority = CommandAuthority::Host;
    app.poll_plugin_callbacks();
    assert!(
        app.extension
            .abi
            .waiting
            .values()
            .all(|waiting| waiting.authority == authority)
    );
    settle(&mut app);
}

#[test]
fn expired_guest_formatter_is_skipped_and_late_edits_cannot_change_the_buffer() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "surface-document");
    settle(&mut app);
    action(&mut app, "demo", "surface-formatter");
    let buffer = app.state.active_buffer_id().unwrap();
    assert!(app.begin_format_on_save(false));
    app.poll_plugin_callbacks();
    assert!(!app.extension.abi.surfaces.callbacks.is_empty());
    for waiting in app.extension.abi.waiting.values_mut() {
        waiting.deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
    }
    app.poll_plugin_callbacks();
    app.process_save_work();
    settle(&mut app);
    app.process_save_work();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "old");
    assert!(!app.state.buffers[buffer].dirty);
}

#[test]
fn surface_final_effect_checks_restricted_authority_and_restores_host_context() {
    let (_roots, mut app) = real_app(&["demo"]);
    let authority = CommandAuthority::Plugin {
        initiator: "demo".into(),
        grants: BTreeSet::new(),
    };
    assert!(
        app.apply_surface_request(
            "demo",
            &authority,
            SurfaceRequest::Notify(wit::NotifyRequest {
                level: wit::NoticeLevel::Info,
                message: "denied".into()
            })
        )
        .is_err()
    );
    assert_eq!(app.effect_authority, CommandAuthority::Host);
}
