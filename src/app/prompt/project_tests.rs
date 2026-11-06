use crate::app::tests::*;




#[test]
pub(in crate::app) fn help_preserves_a_draft_and_a_pending_confirmation() {
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
pub(in crate::app) fn form_help_captures_keys_without_changing_the_prompt_draft() {
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