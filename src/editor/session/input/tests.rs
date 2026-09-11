use super::InputSession;
use crate::{
    config::load_bundled_config,
    editor::{CommandRegistry, PromptMove, register_app_commands, register_builtin_commands},
    ui::{DocumentationMarkupKind, DocumentationPopupKind, PromptKind},
};
use std::path::Path;

#[test]
fn prompt_edits_preserve_grapheme_boundaries_and_history_policy() {
    let mut input = InputSession::new(100);
    let mut policy = load_bundled_config().config;
    input.open_prompt(PromptKind::Command, "é界".into(), None);
    input.prompt_move(PromptMove::Left);
    input.prompt_backspace();
    input.prompt_insert("ø");
    assert_eq!(input.prompt().unwrap().input, "ø界");
    assert_eq!(input.prompt().unwrap().cursor, 2);
    input.prompt_move(PromptMove::Right);
    assert_eq!(input.prompt().unwrap().cursor, "ø界".len());
    policy.history.command.maximum_entries = 2;
    for entry in ["one", "two", "two", "three"] {
        input.record_prompt_history(PromptKind::Command, entry, &policy);
    }
    input.record_prompt_history(PromptKind::WorkspaceTrust, "secret confirmation", &policy);
    assert_eq!(input.command_history(), ["two", "three"]);
    assert!(input.search_history().is_empty());
    policy.history.command.maximum_entries = 1;
    input.configure_history(&policy.history);
    input.prompt_history(-1, &policy);
    assert_eq!(input.prompt().unwrap().input, "three");
    policy.history.command.enabled = false;
    input.configure_history(&policy.history);
    assert!(input.command_history().is_empty());
}

#[test]
fn confirmation_choice_is_typed_and_does_not_accept_text() {
    let mut input = InputSession::new(10);
    input.open_prompt(PromptKind::ExplorerDelete, String::new(), None);
    assert!(input.configure_confirmation_actions(
        "Delete?".into(),
        "target".into(),
        crate::ui::ConfirmationChoice::No,
        "Yes",
        "No",
        String::new(),
    ));
    input.prompt_insert("yes");
    assert!(input.prompt().unwrap().input.is_empty());
    input.prompt_move(PromptMove::Left);
    assert_eq!(
        input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::Yes
    );
    input.toggle_confirmation();
    assert_eq!(
        input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No
    );
}

#[test]
fn closed_reopened_and_reconfigured_prompt_rejects_the_old_directory_reply() {
    let mut policy = load_bundled_config().config;
    policy.input.completion_select_first = true;
    let mut registry = CommandRegistry::default();
    register_builtin_commands(&mut registry).unwrap();
    register_app_commands(&mut registry).unwrap();
    let mut input = InputSession::new(policy.input.count_limit);
    let root = Path::new("/isolated");
    input.open_prompt(PromptKind::Command, "e foo".into(), None);
    let old = input
        .refresh_completions(
            &policy,
            root,
            std::iter::empty::<&Path>(),
            registry.specs().map(|spec| spec.id.as_str()),
        )
        .unwrap();
    input.close_prompt(&crate::editor::EditorState::new(Default::default()).window_version());
    input.open_prompt(PromptKind::Command, "e foo".into(), None);
    let current = input
        .refresh_completions(
            &policy,
            root,
            std::iter::empty::<&Path>(),
            registry.specs().map(|spec| spec.id.as_str()),
        )
        .unwrap();
    assert!(!input.accept_prompt_paths(&old.key, vec![root.join("foo-old.rs")], root, &policy));
    assert!(input.accept_prompt_paths(
        &current.key,
        vec![root.join("foo-current.rs")],
        root,
        &policy
    ));
    assert_eq!(
        input.prompt().unwrap().completions[0].label,
        "foo-current.rs"
    );
    assert_eq!(input.prompt().unwrap().selected_completion, Some(0));
    input.reconfigure(&policy);
    assert_eq!(input.prompt().unwrap().input, "e foo");
    assert_eq!(input.prompt().unwrap().selected_completion, None);
    assert!(!input.accept_prompt_paths(
        &current.key,
        vec![root.join("foo-late.rs")],
        root,
        &policy
    ));
    input.close_prompt(&crate::editor::EditorState::new(Default::default()).window_version());
    input.open_prompt(PromptKind::WorkspaceTrust, "e foo".into(), None);
    assert!(!input.accept_prompt_paths(
        &current.key,
        vec![root.join("foo-trust.rs")],
        root,
        &policy
    ));
    assert!(input.prompt().unwrap().completions.is_empty());
}

#[test]
fn documentation_is_bounded_at_utf8_boundaries_and_scroll_accepts_explicit_content_rows() {
    let mut input = InputSession::new(100);
    let mut policy = load_bundled_config().config.ui.documentation_popup;
    policy.maximum_bytes = 3;
    policy.maximum_lines = 2;
    input.present_documentation(
        DocumentationPopupKind::Hover,
        "Hover".into(),
        "p".repeat(300),
        None,
        [(DocumentationMarkupKind::PlainText, "éé\r\nline\x1b".into())],
        &policy,
    );
    let popup = input.documentation().unwrap();
    assert_eq!(popup.provider.len(), 256);
    assert_eq!(popup.sections[0].lines, ["é", "l"]);
    input.scroll_documentation(1, 100, 1);
    assert_eq!(input.documentation().unwrap().scroll, 1);
    input.scroll_documentation(-1, 100, 1);
    assert_eq!(input.documentation().unwrap().scroll, 0);
    input.close_documentation();
    assert!(input.documentation().is_none());
}

#[test]
fn prompt_mode_restore_rejects_a_new_editor_with_reused_view_ids() {
    use crate::editor::{EditorMode, EditorState, text::TextBuffer};
    let mut first = EditorState::new(Default::default());
    first.open_text(TextBuffer::from_text("first"), None);
    let mut other = EditorState::new(Default::default());
    other.open_text(TextBuffer::from_text("other"), None);
    assert_eq!(first.active_view, other.active_view);
    let mut input = InputSession::new(100);
    assert_eq!(
        input.open_prompt(
            PromptKind::Command,
            String::new(),
            Some((
                first.window_version(),
                first.active_view.unwrap(),
                EditorMode::Insert
            ))
        ),
        Some(EditorMode::CommandLine)
    );
    assert!(input.close_prompt(&other.window_version()).is_none());
}
