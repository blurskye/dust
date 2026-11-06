use crate::app::tests::*;

use crate::app::documents::project_tests::wait_changes_preview;


#[test]
pub(in crate::app) fn the_confirmation_help_action_is_focusable_advertised_and_leaves_the_question_pending() {
    let (_app_dirs, mut app) = app();
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Discard changes?".into(),
        "The buffer has unsaved edits.".into(),
        crate::ui::ConfirmationChoice::No,
    );
    app.project_input_prompt();
    let before = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .cloned()
        .expect("a confirmation is pending");
    assert_eq!(before.selected, crate::ui::ConfirmationChoice::No);

    // The action is advertised on the confirmation itself, from the keymap like every other
    // action, so a user can see it without knowing it exists.
    assert!(
        before.hint.contains("? help"),
        "the confirmation advertises the Help action: {:?}",
        before.hint
    );

    // Choosing Help opens help ABOUT THIS QUESTION, not a generic page.
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.form", InvocationSource::Test)
            .expect("command id"),
    );
    let help = app.search.picker_projection().expect("form help opened");
    let rows = help
        .rows
        .iter()
        .map(|row| (row.detail.clone(), row.label.clone()))
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "asks" && value == "Discard changes?"),
        "the help names the question being asked: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "detail" && value == "The buffer has unsaved edits."),
        "the help carries the confirmation's own message: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "choices" && value == "Yes / No"),
        "the help shows the choices: {rows:?}"
    );
    app.search.close_picker();

    // The question is still pending, with the SAME selection: help is not an answer.
    app.project_input_prompt();
    let after = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .cloned()
        .expect("the confirmation survived help");
    assert_eq!(
        after, before,
        "the confirmation is byte-identical after help: same title, message, choices, hint and selection"
    );
}


/// W-26: the diff pane follows the focused row and shows that path's real diff, and `Tab`
/// switches it between the two comparisons.
#[test]
pub(in crate::app) fn the_changes_diff_pane_follows_focus_and_tab_switches_the_comparison() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    fs::write(app.state.workspace_root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    // Stage one change and then change the file again, so the two comparisons differ.
    fs::write(app.state.workspace_root.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        index.write().unwrap();
    }
    fs::write(app.state.workspace_root.join("f.txt"), "one\nTWO!\nthree\n").unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let frame = |app: &mut App| {
        app.handle_event(crate::ui::UiEvent::Resize(200, 50));
        app.reconcile_editor_windows();
        let frame = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        (0..frame.grid.height())
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<String>>()
            .join("\n")
    };

    wait_changes_preview(&mut app);
    // The pane shows the focused row's UNSTAGED diff: index to disk. These strings are
    // produced only by the pane, so the row's own provenance column cannot satisfy them.
    let unstaged = frame(&mut app);
    assert!(
        unstaged.contains("@@ -1,3 +1,3 @@"),
        "the pane draws the hunk header: {unstaged}"
    );
    assert!(
        unstaged.contains("- TWO"),
        "the pane shows the index line as removed: {unstaged}"
    );
    assert!(
        unstaged.contains("+ TWO!"),
        "the pane shows the worktree line as added: {unstaged}"
    );

    // Tab switches the pane to the staged comparison, with different content.
    let switch = crate::editor::CommandInvocation::new(
        "workspace.git.diff.layer.switch",
        InvocationSource::Test,
    )
    .expect("static command");
    app.invoke(switch);
    wait_changes_preview(&mut app);
    let staged = frame(&mut app);
    assert!(
        staged.contains("HEAD -> index"),
        "the pane names the staged comparison after the switch: {staged}"
    );
    assert!(
        staged.contains("- two") && staged.contains("+ TWO"),
        "the pane shows the staged change: {staged}"
    );
    assert!(
        !staged.contains("TWO!"),
        "the pane no longer shows the worktree-only line: {staged}"
    );
    assert_ne!(unstaged, staged, "the two comparisons render differently");

    // With Tab undone, moving onto the staged row shows what was staged, without a Tab.
    for id in ["workspace.git.diff.layer.switch", "picker.next"] {
        let command = crate::editor::CommandInvocation::new(id, InvocationSource::Test)
            .expect("static command");
        app.invoke(command);
    }
    wait_changes_preview(&mut app);
    let staged_row = frame(&mut app);
    assert!(
        staged_row.contains("HEAD -> index"),
        "a staged row previews its staged change: {staged_row}"
    );
}


/// A long file's only change sits far below the top of a small diff pane: the pane opens on it,
/// and the page keys move through the rest.
#[test]
pub(in crate::app) fn a_small_changes_pane_opens_on_the_change_and_ctrl_d_scrolls_it() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let lines = |changed: &str| {
        (1..=90)
            .map(|n| match n {
                45 => changed.to_owned(),
                60 => format!("later {changed}"),
                n => format!("line {n}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    fs::write(app.state.workspace_root.join("notes.txt"), lines("before")).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("notes.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    fs::write(app.state.workspace_root.join("notes.txt"), lines("after")).unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let frame = |app: &mut App| {
        app.handle_event(crate::ui::UiEvent::Resize(80, 24));
        app.reconcile_editor_windows();
        let frame = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        (0..frame.grid.height())
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<String>>()
            .join("\n")
    };

    wait_changes_preview(&mut app);
    let opened = frame(&mut app);
    assert!(
        opened.contains("index -> disk"),
        "the comparison stays on top: {opened}"
    );
    assert!(
        opened.contains("- before"),
        "the pane opens on the change: {opened}"
    );
    assert!(
        opened.contains("+ after"),
        "the pane opens on the change: {opened}"
    );

    assert_eq!(frame(&mut app), opened, "a redraw keeps the pane's place");

    let mut scrolled = opened.clone();
    for _ in 0..8 {
        app.handle_configured_key("ctrl-d".parse::<KeyEvent>().expect("ctrl-d"));
        scrolled = frame(&mut app);
        if scrolled.contains("+ later after") {
            break;
        }
    }
    assert!(
        scrolled.contains("+ later after"),
        "ctrl-d reaches the next change: {scrolled}"
    );
    assert!(
        scrolled.contains("index -> disk"),
        "the comparison stays on top: {scrolled}"
    );

    for _ in 0..12 {
        app.handle_configured_key("pageup".parse::<KeyEvent>().expect("pageup"));
    }
    let top = frame(&mut app);
    assert!(
        top.contains("unchanged lines"),
        "pageup reaches the top of the diff: {top}"
    );
}


/// W-26: the footer labels `s` and `u` by focus, so the change surface stages the ROW UNDER
/// THE CURSOR rather than whatever buffer happens to be active.
#[test]
pub(in crate::app) fn staging_from_the_change_surface_acts_on_the_focused_row_not_the_active_buffer() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(app.state.workspace_root.join(name), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(app.state.workspace_root.join(name), "two\n").unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    // Open a.txt so the ACTIVE BUFFER is a.txt, while the cursor will be on b.txt.
    app.start_open(app.state.workspace_root.join("a.txt"));
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    // Put the cursor on b.txt by MOVING it, so the picker's own selection changes rather
    // than only the projection.
    let selected_label = |app: &App| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker
            .selected
            .map(|index| picker.rows[index].label.clone())
    };
    for _ in 0..8 {
        if selected_label(&app).as_deref() == Some("b.txt") {
            break;
        }
        app.picker_move(1);
    }
    assert_eq!(
        selected_label(&app).as_deref(),
        Some("b.txt"),
        "the cursor is on the focused row"
    );

    let stage =
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command");
    app.invoke(stage);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The staged diff names b.txt and not a.txt.
    let staged = crate::git::read_hunks(&crate::git::GitHunkPolicy {
        scope_root: app.state.workspace_root.clone(),
        repository_id: app.git.status().expect("status").repository_id,
        status_revision: app.git.status().expect("status").revision,
        root_revision: app.workspace.root_revision(),
        file: std::path::PathBuf::from("b.txt"),
        layer: crate::git::GitDiffLayer::Staged,
        maximum_hunks: 100,
        maximum_lines: 1_000,
        maximum_bytes: 1024 * 1024,
        deadline: Duration::from_secs(5),
    })
    .expect("b.txt hunks");
    assert!(!staged.hunks.is_empty(), "the FOCUSED row b.txt was staged");
    let a_staged = crate::git::read_hunks(&crate::git::GitHunkPolicy {
        scope_root: app.state.workspace_root.clone(),
        repository_id: app.git.status().expect("status").repository_id,
        status_revision: app.git.status().expect("status").revision,
        root_revision: app.workspace.root_revision(),
        file: std::path::PathBuf::from("a.txt"),
        layer: crate::git::GitDiffLayer::Staged,
        maximum_hunks: 100,
        maximum_lines: 1_000,
        maximum_bytes: 1024 * 1024,
        deadline: Duration::from_secs(5),
    })
    .expect("a.txt hunks");
    assert!(
        a_staged.hunks.is_empty(),
        "the ACTIVE BUFFER a.txt was left unstaged"
    );
}


#[test]
pub(in crate::app) fn staging_an_unsaved_focused_row_offers_the_choice_and_writes_nothing() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["unsaved.txt", "other.txt"] {
        fs::write(app.state.workspace_root.join(name), "disk\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    // Make unsaved.txt's buffer dirty, then make OTHER.txt the active buffer, so the row the
    // cursor sits on is not the buffer the editor is showing.
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    app.start_open(app.state.workspace_root.join("other.txt"));
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let selected_label = |app: &App| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker
            .selected
            .map(|index| picker.rows[index].label.clone())
    };
    for _ in 0..8 {
        if selected_label(&app).as_deref() == Some("unsaved.txt") {
            break;
        }
        app.picker_move(1);
    }
    assert_eq!(selected_label(&app).as_deref(), Some("unsaved.txt"));

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        app.process_git_mutation_events();
        if app
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::GitStageUnsaved)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the save-and-stage choice did not appear for the focused row"
        );
        thread::yield_now()
    }
    // The refusal wrote nothing: the index still holds the committed bytes.
    let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
    let index = repo.index().unwrap();
    let entry = index
        .get_path(std::path::Path::new("unsaved.txt"), 0)
        .expect("index entry");
    assert_eq!(
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap(),
        "disk\n",
        "the index was not written by the refused stage"
    );
    assert_eq!(
        fs::read_to_string(app.state.workspace_root.join("unsaved.txt")).unwrap(),
        "disk\n",
        "nothing was saved on the user's behalf either"
    );
}