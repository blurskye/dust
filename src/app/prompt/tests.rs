//! Prompt behavior and application integration tests.

use crate::app::configuration::compile_keymaps;
use crate::app::tests::*;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;

#[test]
fn help_preserves_a_draft_and_a_pending_confirmation() {
    let (_app_dirs, mut app) = app();
    app.open_prompt(crate::ui::PromptKind::Command, "exploreradd ".into());
    app.prompt_insert("draft-name.txt");
    app.project_input_prompt();
    let typed = app
        .ui
        .prompt
        .as_ref()
        .expect("a prompt is open")
        .input
        .clone();
    assert!(typed.contains("draft-name.txt"));

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    let help = app
        .search
        .picker_projection()
        .expect("contextual help opened");
    assert!(
        help.rows.iter().all(|row| !row.label.is_empty()),
        "the help page lists commands rather than blank rows"
    );
    assert!(
        help.rows
            .iter()
            .any(|row| row.label == "overlay.help.contextual"),
        "help lists itself, so the list comes from the registry: {:?}",
        help.rows.iter().map(|row| &row.label).collect::<Vec<_>>()
    );
    app.search.close_picker();

    app.project_input_prompt();
    let after = app.ui.prompt.as_ref().expect("the prompt survived help");
    assert_eq!(
        after.input, typed,
        "help preserves the draft exactly, character for character"
    );
    // Pending consent survives help too. A confirmation is its own prompt, so it is opened
    // separately rather than assumed to sit alongside the draft above.
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Confirm".into(),
        "Discard the draft?".into(),
        crate::ui::ConfirmationChoice::Yes,
    );
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    app.search.close_picker();
    app.project_input_prompt();
    let pending = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("the pending confirmation survived help");
    assert_eq!(
        pending.selected,
        crate::ui::ConfirmationChoice::Yes,
        "help does not answer the pending question for the user"
    );
    assert!(
        app.ui.prompt.as_ref().expect("prompt").input.is_empty(),
        "the confirmation prompt is a new prompt, so help must not have leaked the old draft into it"
    );
}

#[test]
fn form_help_captures_keys_without_changing_the_prompt_draft() {
    let (_app_dirs, mut app) = app();
    app.open_prompt(crate::ui::PromptKind::Command, "write draft".into());
    app.handle_configured_key("ctrl-g".parse().unwrap());
    assert!(app.search.help_is_open());
    app.handle_configured_key(KeyEvent::character('j'));
    assert_eq!(app.input.prompt().unwrap().input, "write draft");
    app.handle_configured_key("escape".parse().unwrap());
    app.handle_configured_key("escape".parse().unwrap());
    assert!(!app.search.help_is_open());
    assert_eq!(app.input.prompt().unwrap().input, "write draft");
    app.handle_configured_key(KeyEvent::character('x'));
    assert_eq!(app.input.prompt().unwrap().input, "write draftx");
}

#[test]
fn prompt_mouse_context_is_independently_configurable() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.prompt-mouse-context-is-independently-configurable".into(),
        context: Some("prompt_mouse".into()),
        mode: String::new(),
        keys: Some("mouse-right-down".into()),
        command: Some("command_line.cancel".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.handle_event(crate::ui::UiEvent::Mouse {
        key: crate::editor::input::KeyEvent::plain(crate::editor::input::KeyCode::MouseRightDown),
        column: 1,
        row: 1,
    });
    assert!(app.ui.prompt.is_none());
}

#[test]
fn protected_prompt_region_blocks_ordinary_compositor_hit_testing() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    app.reconcile_editor_windows();
    let geometry = crate::ui::prompt_geometry(
        app.window_session.width(),
        app.workspace_render_height(),
        false,
    );
    let point = (
        geometry.left.saturating_add(geometry.width / 2),
        geometry.top.saturating_add(geometry.height / 2),
    );
    let projection = app.compositor_projection().expect("protected projection");
    assert_eq!(projection.protected_regions.len(), 1);
    assert_eq!(projection.hit_test(point.0, point.1), None);

    app.close_prompt();
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection()
            .expect("ordinary projection")
            .hit_test(point.0, point.1)
            .is_some()
    );
}
