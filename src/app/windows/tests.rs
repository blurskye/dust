//! Windows behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::debugger::tests::logging_adapter_fixture;
use crate::app::documents::tests::wait_changes_preview;
use crate::app::tests::*;
use crate::app::windows::ProjectionRequest;
use crate::app::windows::Windows;
use crate::config::load_bundled_config;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::process::PtyId;
use crate::ui::NativeSurfaceWindowBinding;
use crate::ui::PtyScreen;
use crate::ui::UiEvent;
use crate::ui::active_pane_viewport;
use crate::ui::render;
use crate::windows::ContentAdapterKind;
use crate::windows::ContentContract;
use crate::windows::DockEdge;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn the_confirmation_help_action_is_focusable_advertised_and_leaves_the_question_pending() {
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
fn the_changes_diff_pane_follows_focus_and_tab_switches_the_comparison() {
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
fn a_small_changes_pane_opens_on_the_change_and_ctrl_d_scrolls_it() {
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
fn staging_from_the_change_surface_acts_on_the_focused_row_not_the_active_buffer() {
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
fn staging_an_unsaved_focused_row_offers_the_choice_and_writes_nothing() {
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

#[test]
fn placement_swap_moves_editing_focus_to_the_visible_editor() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("oldest"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let (_, first_view) = app
        .state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    let first_pane = app
        .state
        .tab_focused_window(app.state.active_tab().unwrap())
        .unwrap();
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let second_view = app.state.active_view().unwrap();
    let second_pane = app
        .state
        .tab_focused_window(app.state.active_tab().unwrap())
        .unwrap();
    let first = app
        .state
        .graph
        .window(first_pane)
        .map(|window| window.id)
        .unwrap();
    let second = app
        .state
        .graph
        .window(second_pane)
        .map(|window| window.id)
        .unwrap();
    app.place_graph_window(
        &format!("window:{}", first.0),
        WindowPlacement::Floating(WindowRect {
            x: 1,
            y: 1,
            width: 30,
            height: 8,
        }),
    )
    .unwrap();
    app.place_graph_window(&format!("window:{}", second.0), WindowPlacement::Hidden)
        .unwrap();
    assert_eq!(app.state.active_view(), Some(first_view));
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", first.0),
        second: format!("window:{}", second.0),
    });
    assert_eq!(app.state.active_view(), Some(second_view));
    assert_eq!(
        app.state
            .tab_focused_window(app.state.active_tab().unwrap())
            .unwrap(),
        second_pane
    );
    assert_eq!(app.window_session.graph().focused(), Some(second));
}

#[test]
fn fullscreen_editor_swap_moves_editing_focus_to_the_fullscreen_recipient() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let first_pane = app
        .state
        .tab_focused_window(app.state.active_tab().unwrap())
        .unwrap();
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let second_view = app.state.active_view().unwrap();
    let second_pane = app
        .state
        .tab_focused_window(app.state.active_tab().unwrap())
        .unwrap();
    let first = app
        .state
        .graph
        .window(first_pane)
        .map(|window| window.id)
        .unwrap();
    let second = app
        .state
        .graph
        .window(second_pane)
        .map(|window| window.id)
        .unwrap();
    let first_target = format!("window:{}", first.0);
    let second_target = format!("window:{}", second.0);
    app.place_graph_window(
        &second_target,
        WindowPlacement::Floating(WindowRect {
            x: 1,
            y: 1,
            width: 30,
            height: 8,
        }),
    )
    .unwrap();
    app.place_graph_window(&first_target, WindowPlacement::Fullscreen)
        .unwrap();
    app.swap_graph_windows(&first_target, &second_target)
        .unwrap();
    assert_eq!(app.window_session.graph().focused(), Some(second));
    assert_eq!(app.state.active_view(), Some(second_view));
    assert_eq!(
        app.state
            .tab_focused_window(app.state.active_tab().unwrap())
            .unwrap(),
        second_pane
    );
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().focused(), Some(second));
}

#[test]
fn fullscreen_placement_swaps_keep_graph_and_editor_focus_together() {
    for reverse in [false, true] {
        let (_app_dirs, mut app) = app();
        let (_, original_view) = app
            .state
            .open_text(TextBuffer::from_text("first"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.reconcile_editor_windows();
        let original = app.window_session.graph().focused().unwrap();
        app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
            crate::windows::TileAxis::Horizontal,
        )));
        let clone_view = app.state.active_view().unwrap();
        let clone = app.window_session.graph().focused().unwrap();
        let (fullscreen, floating, floating_view) = if reverse {
            (clone, original, original_view)
        } else {
            (original, clone, clone_view)
        };
        let rect = WindowPlacement::Floating(WindowRect {
            x: 2,
            y: 2,
            width: 30,
            height: 8,
        });
        app.place_graph_window(&format!("window:{}", floating.0), rect)
            .unwrap();
        app.place_graph_window(
            &format!("window:{}", fullscreen.0),
            WindowPlacement::Fullscreen,
        )
        .unwrap();
        app.swap_graph_windows(
            &format!("window:{}", original.0),
            &format!("window:{}", clone.0),
        )
        .unwrap();
        assert_eq!(
            app.window_session
                .graph()
                .window(floating)
                .unwrap()
                .placement,
            WindowPlacement::Fullscreen
        );
        assert_eq!(
            app.window_session
                .graph()
                .window(fullscreen)
                .unwrap()
                .placement,
            rect
        );
        assert_eq!(app.window_session.graph().focused(), Some(floating));
        assert_eq!(app.state.active_view(), Some(floating_view));
        app.window_session.graph().validate().unwrap();
    }
}

#[test]
fn tiled_window_resize_accepts_the_minimum_delta() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    app.resize_graph_window("focused", i16::MIN).unwrap();
    assert!(matches!(
        app.state
            .tab_layout(app.state.active_tab().unwrap())
            .unwrap(),
        crate::windows::TiledLayoutNode::Split { ratio: 900, .. }
    ));
}

#[test]
fn nonwrapped_mouse_uses_the_same_snapped_tab_origin_as_rendering() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((6, 5));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.ui.status_line.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    let (_, view) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("a\tb"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].scroll_column = 2;
    app.reconcile_editor_windows();
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    for cell in 0..3 {
        app.state.views[view].scroll_column = 2;
        app.input.observe_mouse(viewport.x + cell, viewport.y);
        app.apply_mouse_cursor(false);
        assert_eq!(
            app.state.views[view].cursor,
            ByteOffset(1),
            "visible tab cell {cell}"
        );
    }
}

#[test]
fn mouse_click_focuses_the_real_target_pane_before_placing_cursor() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abcdef abcdef abcdef"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let first = app.state.active_view().expect("first view");
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    assert_ne!(app.state.active_view(), Some(first));
    let flash_view = app.state.active_view().unwrap();
    app.reconcile_editor_windows();
    app.begin_flash();
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('a'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert!(app.flash.is_some());
    let viewport = crate::ui::pane_viewports(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.window_session.width(),
        app.workspace_render_height(),
    )
    .into_iter()
    .find(|viewport| viewport.view == first)
    .expect("generic first-window viewport");
    let workspace = app.workspace_bounds().expect("workspace");
    let gutter = crate::ui::editor_state_gutter_width(
        &app.state,
        &app.state.buffers[app.state.views[first].buffer].text,
        &app.configuration.settings().editor,
        viewport.width,
    );
    app.input.observe_mouse(
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 1),
        workspace.y.saturating_add(viewport.y),
    );
    let geometry = app.mouse_pane_geometry().expect("mouse geometry");
    assert_eq!(geometry.0, viewport.pane);
    assert_eq!((geometry.3, geometry.4), (viewport.width, viewport.height));
    let (column, row) = app.input.mouse_position().unwrap();
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            Default::default(),
        ),
        column,
        row,
    });
    assert_eq!(app.state.active_view(), Some(first));
    assert_eq!(
        app.state.views[first].cursor,
        crate::editor::text::ByteOffset(1)
    );
    assert!(app.flash.is_none());
    assert!(app.ui.flash.is_none());
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('l'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert_eq!(app.state.views[first].cursor, ByteOffset(2));
    assert_eq!(app.state.views[flash_view].cursor, ByteOffset(0));
}

#[test]
fn universal_window_clone_refuses_native_noncloneable_content_without_mutation() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);

    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "clone-refusal".into(),
            screen: crate::ui::PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(crate::process::PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    let terminal = app
        .window_session
        .native_ids()
        .terminal(1)
        .expect("terminal window")
        .window;
    let graph_before_terminal = app.window_session.graph().clone();
    let source_before_terminal = app.state.window_version();
    let terminal_count = app.terminal_session.len();
    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", terminal.0),
        axis: crate::windows::TileAxis::Vertical,
    });
    assert_eq!(app.window_session.graph(), &graph_before_terminal);
    assert_eq!(app.state.window_version(), source_before_terminal);
    assert_eq!(app.terminal_session.len(), terminal_count);
    assert!(app.terminal_session.contains(crate::process::PtyId(1)));
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("does not support clone-view"))
    );

    let (_closed_app_dirs, mut closed_app) = crate::app::tests::app();
    closed_app
        .state
        .open_text(TextBuffer::from_text("close then clone"), None)
        .unwrap();
    let first_tab = closed_app.state.active_tab().expect("first tab");
    let first_pane = closed_app.state.tab_focused_window(first_tab).unwrap();
    closed_app.state.new_tab().expect("remaining tab");
    closed_app.set_dashboard_visibility(false);
    closed_app.reconcile_editor_windows();
    let closed = closed_app
        .state
        .graph
        .window(first_pane)
        .map(|window| window.id)
        .expect("window to close");
    closed_app
        .state
        .activate_tab(first_tab)
        .expect("activate window tab");
    closed_app
        .state
        .focus_pane_id(first_pane)
        .expect("focus window to close");
    closed_app
        .state
        .close_active_pane()
        .expect("close source pane");
    closed_app.reconcile_editor_windows();
    assert!(closed_app.window_session.graph().window(closed).is_none());
    let graph_after_close = closed_app.window_session.graph().clone();
    let source_after_close = closed_app.state.window_version();
    closed_app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", closed.0),
        axis: crate::windows::TileAxis::Horizontal,
    });
    assert_eq!(closed_app.window_session.graph(), &graph_after_close);
    assert_eq!(closed_app.state.window_version(), source_after_close);
    assert!(
        closed_app
            .ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("unknown window identity"))
    );
}

#[test]
fn window_clone_rejects_an_exhausted_editor_source_before_graph_mutation() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("seed"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let window = app.window_session.graph().focused().expect("source window");
    app.state.exhaust_window_source_for_test();
    let graph_before = app.window_session.graph().clone();
    let panes_before = app.state.editor_windows().count();
    let active_before = app.state.active_view();

    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", window.0),
        axis: crate::windows::TileAxis::Horizontal,
    });

    assert_eq!(app.window_session.graph(), &graph_before);
    assert_eq!(app.state.window_version().revision(), None);
    assert_eq!(app.state.editor_windows().count(), panes_before);
    assert_eq!(app.state.active_view(), active_before);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("source revision is exhausted"))
    );
}

#[test]
fn ordinary_read_only_files_can_reload_without_becoming_writable() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("external.txt");
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    app.state.buffers[buffer].read_only = true;
    fs::write(&path, "new").unwrap();
    app.start_reload(true);
    assert!(app.documents.file_work_pending());
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new");
    assert!(app.state.buffers[buffer].read_only);
}

#[test]
fn shared_dirty_buffers_do_not_block_view_only_tab_close_or_forced_buffer_close() {
    let (_app_dirs, mut app) = app();
    let (dirty, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.buffers[dirty].dirty = true;
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("shared split");
    app.state.new_tab().expect("shared tab");
    let tab_count = app.state.editor_tabs().count();
    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
    assert_eq!(
        app.state.editor_tabs().count(),
        tab_count - 1,
        "a dirty buffer with views outside the tab is not a last reference"
    );
    assert!(app.input.prompt().is_none());
    app.state
        .validate_layout()
        .expect("closed layout remains valid");

    app.close_active_buffer(true);
    assert!(app.state.buffers.contains_key(dirty));
    assert!(app.state.buffers[dirty].dirty);
    app.state
        .cycle_buffer(&[], 1, true)
        .expect("reopen forced-close buffer");
    assert_eq!(app.state.active_buffer_id(), Some(dirty));
    app.state.validate_layout().expect("reopened layout valid");
}

#[test]
fn exact_dirty_window_close_rejects_stale_focus_then_discards_only_its_target() {
    let (_app_dirs, mut app) = app();
    let (keeper, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("keeper"), None)
        .unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let (dirty, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None)
        .unwrap();
    app.state.set_buffer_dirty(dirty, true);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let pane = app
        .state
        .tab_focused_window(app.state.active_tab().unwrap())
        .unwrap();
    let window = app
        .state
        .graph
        .window(pane)
        .map(|window| window.id)
        .unwrap();
    let target = format!("window:{}", window.0);

    app.close_graph_window(&target).unwrap();
    app.apply_layout_action(crate::app::windows::LayoutAction::FocusPane(-1));
    let graph = app.window_session.graph().clone();
    app.submit_prompt();
    assert_eq!(app.window_session.graph(), &graph);
    assert!(app.state.buffers.contains_key(dirty));
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("source or focused target changed"))
    );

    app.close_graph_window(&target).unwrap();
    app.submit_prompt();
    assert!(app.state.buffers.contains_key(keeper));
    assert!(!app.state.buffers.contains_key(dirty));
    assert!(app.window_session.graph().window(window).is_none());
}

#[test]
fn tab_close_reviews_every_last_dirty_reference_before_one_publication() {
    let (_app_dirs, mut app) = app();
    let keeper = app.state.workspace_root.join("keeper.txt");
    std::fs::write(&keeper, "keeper\n").unwrap();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("keeper\n"),
            Some(keeper),
        )
        .unwrap();
    app.state.new_tab().unwrap();
    let first_path = app.state.workspace_root.join("first.txt");
    let second_path = app.state.workspace_root.join("second.rs");
    std::fs::write(&first_path, "first\n").unwrap();
    std::fs::write(&second_path, "second\n").unwrap();
    let (first, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("first\n"),
            Some(first_path.clone()),
        )
        .unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("second\n"),
            Some(second_path),
        )
        .unwrap();
    let (_, document) = app.active_lsp_document().unwrap();
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .unwrap()
        .definition
        .clone();
    app.language.fixture_session(
        "discarded".into(),
        crate::app::language::LspRuntimeState {
            root: app.workspace.active_root().unwrap(),
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::app::language::LspLaunch::Direct {
                executable: crate::tools::discover_executable("false").unwrap(),
                managed: false,
            },
            capabilities: None,
            documents: HashMap::from([(second, document)]),
            restart_count: 0,
            restart_due: None,
            circuit_open: false,
            desired_running: true,
            activate_on_restart: true,
        },
    );
    app.state.buffers[first]
        .text
        .restore_text("first saved\n")
        .unwrap();
    app.state.set_buffer_dirty(first, true);
    app.state.buffers[second]
        .text
        .restore_text("second discarded\n")
        .unwrap();
    app.state.set_buffer_dirty(second, true);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let tabs = app.state.editor_tabs().count();

    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
    assert_eq!(app.state.editor_tabs().count(), tabs);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert_eq!(
        app.state.editor_tabs().count(),
        tabs,
        "queueing a save is not close ACK"
    );
    crate::app::tests::wait_vfs(&mut app);
    assert_eq!(
        app.state.editor_tabs().count(),
        tabs,
        "second dirty target needs review"
    );
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::DirtyClose
    );
    app.submit_prompt();
    assert_eq!(app.state.editor_tabs().count(), tabs - 1);
    assert_eq!(
        std::fs::read_to_string(first_path).unwrap(),
        "first saved\n"
    );
    assert!(!app.state.buffers.contains_key(second));
    assert!(
        !app.language.sessions()["discarded"]
            .documents
            .contains_key(&second)
    );

    app.state.new_tab().unwrap();
    let (a, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("a"), None)
        .unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    let (b, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("b"), None)
        .unwrap();
    app.state.set_buffer_dirty(a, true);
    app.state.set_buffer_dirty(b, true);
    app.reconcile_editor_windows();
    let source = (
        app.state.active_tab(),
        app.state.active_view(),
        app.state.editor_tabs().count(),
        app.state.editor_windows().count(),
        app.state.views.len(),
    );
    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_some(),
        "second dirty target is reviewed"
    );
    app.close_prompt();
    assert_eq!(
        (
            app.state.active_tab(),
            app.state.active_view(),
            app.state.editor_tabs().count(),
            app.state.editor_windows().count(),
            app.state.views.len(),
        ),
        source
    );
    assert!(app.state.buffers.contains_key(a));
    assert!(app.state.buffers.contains_key(b));
}

#[test]
fn editor_lifecycle_reconciles_into_window_graph_and_drives_content_tabs() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/src/main.rs")),
        )
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/tests/main.rs")),
        )
        .unwrap();
    app.state.set_buffer_dirty(first, true);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 2, 2, 1));
    assert_eq!(app.ui.content_tabs.len(), 2);
    assert_ne!(app.ui.content_tabs[0].title, app.ui.content_tabs[1].title);
    assert!(app.ui.content_tabs[0].dirty);
    assert!(app.ui.content_tabs[1].active);
    let before = app.window_session.test_publications();
    app.state
        .set_workspace_root(PathBuf::from("/workspace/src"));
    app.reconcile_editor_windows();
    assert!(app.window_session.test_publications() > before);
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.title == "main.rs ·1")
    );
    assert_eq!(
        app.ui.content_tabs[1].identity,
        crate::ui::ContentTabIdentity(
            app.state
                .content_for_buffer(second)
                .expect("second content")
        )
    );

    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 3, 3, 1));
    let focused = app.window_session.graph().focused().expect("focused split");

    let second_workspace_tab = app.state.new_tab().expect("new workspace tab");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 4, 4, 2));
    assert_eq!(
        app.window_session.graph().active_tab(),
        Some(second_workspace_tab)
    );
    assert_ne!(app.window_session.graph().focused(), Some(focused));

    // Splits and workspace tabs share content entries rather than duplicating them.
    assert_eq!(app.ui.content_tabs.len(), 2);
    let identities = app
        .ui
        .content_tabs
        .iter()
        .map(|entry| entry.identity.0)
        .collect::<BTreeSet<_>>();
    assert_eq!(identities.len(), 2);
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    let row = u16::from(app.state.editor_tabs().count() > 1);
    let line = frame.grid.row_text(row);
    for entry in &app.ui.content_tabs {
        assert_eq!(line.matches(&entry.title).count(), 1, "{line:?}");
        let columns = (0..frame.grid.width())
            .filter(|column| {
                crate::ui::content_tab_at_column(
                    &app.ui.content_tabs,
                    app.configuration.settings(),
                    frame.grid.width(),
                    *column,
                ) == Some(entry.identity)
            })
            .collect::<Vec<_>>();
        assert!(!columns.is_empty(), "each tab is visible");
        let dirty_cells = columns
            .iter()
            .filter(|column| {
                frame.grid.get(**column, row).unwrap().style == crate::ui::Style::BufferLineDirty
            })
            .count();
        assert_eq!(
            dirty_cells > 0,
            entry.dirty,
            "{} dirty indicator",
            entry.title
        );
    }

    app.state.close_active_tab().expect("close workspace tab");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 3, 3, 1));
    assert_eq!(app.ui.content_tabs.len(), 2);
    app.window_session
        .graph()
        .validate()
        .expect("valid reconciled graph");
}

#[test]
fn editor_layout_commands_publish_document_views_and_graph_together() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let initial_revision = app
        .window_session
        .graph()
        .revision()
        .expect("graph revision");
    let initial_window = app
        .window_session
        .graph()
        .focused()
        .expect("initial window");

    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::Split(crate::windows::TileAxis::Horizontal),
    ));
    assert_eq!(app.state.editor_windows().count(), 2);
    assert_eq!(
        app.window_session
            .graph()
            .tiled_layout_snapshot(app.window_session.graph().active_tab().expect("tab"))
            .expect("layout")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 20,
                },
                1,
                1,
            )
            .windows
            .len(),
        2
    );
    assert!(
        app.window_session
            .graph()
            .revision()
            .expect("split revision")
            > initial_revision
    );
    assert!(app.window_session.graph().window(initial_window).is_some());

    let focused = app.window_session.graph().focused().expect("focused split");
    let before_swap = app
        .compositor_projection()
        .expect("swap projection")
        .clone();
    let initial_rect = before_swap
        .window(initial_window)
        .expect("initial rect")
        .rect;
    let focused_rect = before_swap.window(focused).expect("focused rect").rect;
    app.process_effect(crate::app::commands::AppEffect::WindowSwap {
        first: format!("window:{}", initial_window.0),
        second: format!("window:{}", focused.0),
    });
    let after_swap = app.compositor_projection().expect("swapped projection");
    assert_eq!(
        after_swap
            .window(initial_window)
            .expect("initial swapped")
            .rect,
        focused_rect
    );
    assert_eq!(
        after_swap.window(focused).expect("focused swapped").rect,
        initial_rect
    );
    assert_eq!(app.window_session.graph().focused(), Some(focused));

    let width_before_directional = app
        .compositor_projection()
        .and_then(|projection| projection.window(focused))
        .expect("focused before directional resize")
        .rect
        .width;
    app.handle_configured_key("ctrl-right".parse::<KeyEvent>().expect("Ctrl-Right"));
    let width_after_directional = app
        .compositor_projection()
        .and_then(|projection| projection.window(focused))
        .expect("focused after directional resize")
        .rect
        .width;
    assert!(width_after_directional > width_before_directional);

    let before_resize = app.window_session.graph().clone();
    app.process_effect(crate::app::commands::AppEffect::WindowResize {
        target: format!("window:{}", focused.0),
        amount: 50,
    });
    assert_ne!(app.window_session.graph(), &before_resize);
    assert_eq!(app.state.editor_windows().count(), 2);

    app.process_effect(crate::app::commands::AppEffect::WindowClose(format!(
        "window:{}",
        focused.0
    )));
    assert_eq!(app.state.editor_windows().count(), 1);
    assert!(app.window_session.graph().window(initial_window).is_some());
    let state_revision_before_rejected = app.state.window_revision();
    let active_view_before_rejected = app.state.active_view();
    let pane_count_before_rejected = app.state.editor_windows().count();
    let graph_before_rejected = app.window_session.graph().clone();
    app.process_effect(crate::app::commands::AppEffect::WindowClose(
        "focused".into(),
    ));
    assert_eq!(app.state.window_revision(), state_revision_before_rejected);
    assert_eq!(app.state.active_view(), active_view_before_rejected);
    assert_eq!(
        app.state.editor_windows().count(),
        pane_count_before_rejected
    );
    assert_eq!(app.window_session.graph(), &graph_before_rejected);
    assert!(app.ui.notification_error);
}

#[test]
fn overlapping_mixed_floats_paint_and_focus_in_compositor_z_order() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(TextBuffer::from_text("TOP_EDITOR"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.open_explorer();
    app.process_effect(AppEffect::WindowFloat {
        target: "explorer".into(),
        x: 4,
        y: 3,
        width: 35,
        height: 10,
    });
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let pane = app
        .state
        .tab_focused_window(app.state.active_tab().expect("tab"))
        .unwrap();
    let editor = app
        .state
        .graph
        .window(pane)
        .map(|window| window.id)
        .expect("later editor window");
    app.process_effect(AppEffect::WindowFloat {
        target: format!("window:{}", editor.0),
        x: 4,
        y: 3,
        width: 35,
        height: 10,
    });
    let projection = app.compositor_projection().expect("mixed projection");
    let explorer = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer window")
        .window;
    assert!(
        projection
            .window(editor)
            .expect("editor projection")
            .z_index
            > projection
                .window(explorer)
                .expect("explorer projection")
                .z_index
    );
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    let painted = (3..13)
        .map(|row| {
            (4..39)
                .filter_map(|column| frame.grid.get(column, row))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(painted.contains("TOP_EDITOR"), "{painted}");
    app.input.observe_mouse(10, 4);
    app.focus_mouse_pane();
    assert_eq!(
        app.state
            .tab_focused_window(app.state.active_tab().expect("tab"))
            .unwrap(),
        pane
    );
    assert_eq!(app.window_session.graph().focused(), Some(editor));
    assert_eq!(app.active_input_context(), "editor.normal");
    app.handle_configured_key("i".parse::<KeyEvent>().expect("insert mode"));
    app.handle_configured_key("x".parse::<KeyEvent>().expect("insert text"));
    let view = app.state.window_view(pane).unwrap();
    let buffer = app.state.views[view].buffer;
    assert!(app.state.buffers[buffer].text.to_string().starts_with('x'));
}

#[test]
fn editor_window_swap_rejects_mixed_and_cross_tab_targets_atomically() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    app.open_explorer();
    let tab = app.window_session.graph().active_tab().expect("active tab");
    let editor_windows = app
        .window_session
        .graph()
        .tiled_layout_snapshot(tab)
        .expect("editor layout")
        .project(
            WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 20,
            },
            1,
            1,
        )
        .windows
        .into_iter()
        .map(|window| window.window)
        .collect::<Vec<_>>();
    let explorer = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer")
        .window;
    let graph_before_mixed = app.window_session.graph().clone();
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", editor_windows[0].0),
        second: format!("window:{}", explorer.0),
    });
    assert_eq!(app.window_session.graph(), &graph_before_mixed);
    assert!(app.ui.notification_error);

    let old_editor = editor_windows[0];
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::NewTab));
    let current_editor = app
        .window_session
        .graph()
        .focused()
        .expect("new tab editor");
    let graph_before_cross_tab = app.window_session.graph().clone();
    let revision_before_cross_tab = app.state.window_revision();
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", old_editor.0),
        second: format!("window:{}", current_editor.0),
    });
    assert_eq!(app.window_session.graph(), &graph_before_cross_tab);
    assert_eq!(app.state.window_revision(), revision_before_cross_tab);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("tab")),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn editor_window_revision_skips_idle_rebuilds_and_tracks_projection_changes() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/first.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let initial_count = app.window_session.test_publications();
    let initial_graph = app.window_session.graph().clone();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.test_publications(), initial_count);
    assert_eq!(app.window_session.graph(), &initial_graph);

    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs[0].dirty);
    let after_dirty = app.window_session.test_publications();

    let revision = app.state.buffers[buffer].text.revision();
    let bytes = app.state.buffers[buffer].text.to_bytes();
    app.state.mark_buffer_saved_as(
        buffer,
        revision,
        bytes,
        Some(PathBuf::from("/workspace/saved.rs")),
    );
    app.reconcile_editor_windows();
    assert!(!app.ui.content_tabs[0].dirty);
    assert_eq!(app.ui.content_tabs[0].title, "saved.rs");
    assert_eq!(app.window_session.test_publications(), after_dirty + 1);

    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    let split_count = app.window_session.test_publications();
    let first_pane = match &app
        .state
        .tab_layout(app.state.active_tab().expect("tab"))
        .unwrap()
    {
        crate::windows::TiledLayoutNode::Split { first, .. } => match first.as_ref() {
            crate::windows::TiledLayoutNode::Window(pane) => *pane,
            crate::windows::TiledLayoutNode::Split { .. } => panic!("unexpected nested split"),
        },
        crate::windows::TiledLayoutNode::Window(_) => panic!("expected split"),
    };
    app.state.focus_pane_id(first_pane).expect("focus");
    app.reconcile_editor_windows();
    app.state.resize_active_pane(50).expect("resize");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.test_publications(), split_count + 2);

    let before_reload = app.window_session.test_publications();
    {
        let fixture = app.configuration.fixture_snapshot();
        app.configuration.install_fixture(fixture);
    }
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.test_publications(), before_reload);
    app.window_session
        .graph()
        .validate()
        .expect("revision-driven graph is valid");
}

#[test]
fn generic_native_window_operations_are_revision_checked_atomic_and_identity_preserving() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((100, 30));
    app.open_explorer();
    app.terminal_session.fixture(
        PtyId(7),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(100, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(7));
    app.sync_terminal_panel();
    assert_eq!(
        app.window_session
            .graph()
            .content_snapshots()
            .into_iter()
            .find(|content| content.content_type.as_str() == "builtin.terminal")
            .expect("terminal content")
            .lifecycle,
        crate::windows::ContentLifecycle::Running
    );
    let explorer = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer");
    let terminal = app
        .window_session
        .native_ids()
        .terminal(7)
        .expect("terminal");
    let original_explorer = app
        .window_session
        .graph()
        .window(explorer.window)
        .expect("explorer")
        .placement;
    let original_terminal = app
        .window_session
        .graph()
        .window(terminal.window)
        .expect("terminal")
        .placement;

    app.process_effect(crate::app::commands::AppEffect::WindowSwap {
        first: "explorer".into(),
        second: "terminal".into(),
    });
    assert_eq!(app.window_session.native_ids().explorer(), Some(explorer));
    assert_eq!(app.window_session.native_ids().terminal(7), Some(terminal));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("swapped")
            .placement,
        original_terminal
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .expect("swapped")
            .placement,
        original_explorer
    );
    app.process_effect(crate::app::commands::AppEffect::WindowSwap {
        first: "explorer".into(),
        second: "terminal".into(),
    });

    app.process_effect(crate::app::commands::AppEffect::WindowFullscreen(
        "explorer".into(),
    ));
    let terminal_before = app
        .window_session
        .graph()
        .window(terminal.window)
        .expect("terminal")
        .placement;
    app.process_effect(crate::app::commands::AppEffect::WindowFullscreen(
        "terminal".into(),
    ));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("fullscreen")
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .expect("atomic rejection")
            .placement,
        terminal_before
    );
    assert!(app.ui.notification_error);

    let floating = WindowPlacement::Floating(WindowRect {
        x: 5,
        y: 4,
        width: 30,
        height: 8,
    });
    app.place_graph_window("terminal", floating).unwrap();
    assert_eq!(app.window_session.graph().focused(), Some(explorer.window));
    app.swap_graph_windows("explorer", "terminal").unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        floating
    );
    assert_eq!(app.window_session.graph().focused(), Some(terminal.window));
    app.swap_graph_windows("terminal", "explorer").unwrap();
    assert_eq!(app.window_session.graph().focused(), Some(explorer.window));
    app.place_graph_window("terminal", original_terminal)
        .unwrap();

    let bottom_height = app.window_session.terminal_height();
    app.place_graph_window(
        "terminal",
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 62,
        },
    )
    .unwrap();
    assert_eq!(app.window_session.terminal_height(), bottom_height);
    app.resize_graph_window("terminal", 2).unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 64
        }
    );
    assert_eq!(app.window_session.terminal_height(), bottom_height);
    app.place_graph_window("terminal", original_terminal)
        .unwrap();

    app.process_effect(crate::app::commands::AppEffect::WindowDock {
        target: "explorer".into(),
        edge: "left".into(),
        size: 31,
    });
    app.process_effect(crate::app::commands::AppEffect::WindowHide(
        "explorer".into(),
    ));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("hidden")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::app::commands::AppEffect::WindowShow(
        "explorer".into(),
    ));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("restored")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 31,
        }
    );

    let focus_target = if app.window_session.graph().focused() == Some(explorer.window) {
        terminal.window
    } else {
        explorer.window
    };
    let plan = app
        .window_session
        .plan_focus(
            focus_target,
            &app.state.window_version(),
            app.extension.contributions().version(),
        )
        .expect("native focus plan");
    app.window_session
        .accept_native_plan(
            plan,
            &app.state.window_version(),
            &app.extension.contributions().version(),
            &mut app.state,
        )
        .expect("graph-only focus change");
    assert!(app.compositor_projection().is_none());
    let graph_stale = app.window_session.graph().clone();
    assert!(matches!(
        terminal_before,
        WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            ..
        }
    ));
    let height_before = app.window_session.terminal_height();
    assert!(
        app.resize_native_window_direction(
            crate::app::windows::NativeSurfaceTarget::Terminal(7),
            crate::windows::SpatialDirection::Up
        )
        .is_err()
    );
    assert_eq!(app.window_session.terminal_height(), height_before);
    app.process_effect(crate::app::commands::AppEffect::WindowHide(
        "terminal".into(),
    ));
    assert_eq!(app.window_session.graph(), &graph_stale);
    app.reconcile_editor_windows();
    assert!(app.compositor_projection().is_some());

    let before_stale = app.window_session.graph().clone();
    app.ui
        .compositor_layout
        .as_mut()
        .expect("compositor")
        .source_revision = app
        .state
        .window_revision()
        .expect("source revision")
        .saturating_add(1);
    // The rendering cache is no longer authority. Invalidating the real owner
    // still refuses the operation and preserves graph atomicity.
    assert!(app.compositor_projection().is_some());
    app.window_session.invalidate();
    app.process_effect(crate::app::commands::AppEffect::WindowHide(
        "terminal".into(),
    ));
    assert_eq!(app.window_session.graph(), &before_stale);
    assert!(app.ui.notification_error);
    app.reconcile_editor_windows();
    assert!(app.compositor_projection().is_some());
    let idle_graph = app.window_session.graph().clone();
    let idle_reconciliations = app.window_session.test_publications();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.graph(), &idle_graph);
    assert_eq!(app.window_session.test_publications(), idle_reconciliations);

    app.process_effect(crate::app::commands::AppEffect::WindowClose(
        "explorer".into(),
    ));
    assert!(
        app.window_session
            .graph()
            .content(explorer.content)
            .is_none()
    );
    assert!(app.window_session.native_ids().explorer().is_none());
    assert!(app.ui.explorer.is_none());
    assert!(app.terminal_session.contains(PtyId(7)));
}

#[test]
fn configured_window_rule_candidate_is_cloned_validated_and_published_atomically() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let explorer = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer");
    let original = app.window_session.graph().clone();
    let rules = crate::config::WindowsConfig {
        rules: vec![crate::config::WindowRuleConfig {
            content_type: "builtin.explorer".into(),
            placement: crate::config::WindowPlacementConfig::Docked {
                edge: "right".into(),
                size: 31,
            },
        }],
    };
    let candidate = app
        .prepare_configured_window_rules(&rules)
        .expect("configured candidate");
    assert_eq!(app.window_session.graph(), &original);
    assert_eq!(
        candidate
            .graph()
            .window(explorer.window)
            .expect("candidate explorer")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 31,
        }
    );
    app.window_session
        .accept_current(candidate, &app.state.window_version(), &mut app.state)
        .expect("checked window candidate");
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("published")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 31,
        }
    );

    let visible = app.window_session.graph().clone();
    app.show_graph_window("explorer")
        .expect("show already-visible explorer");
    assert_eq!(app.window_session.graph(), &visible);

    let hostile = crate::config::WindowsConfig {
        rules: vec![
            crate::config::WindowRuleConfig {
                content_type: "builtin.dashboard".into(),
                placement: crate::config::WindowPlacementConfig::Fullscreen,
            },
            crate::config::WindowRuleConfig {
                content_type: "builtin.debug".into(),
                placement: crate::config::WindowPlacementConfig::Fullscreen,
            },
        ],
    };
    let graph_before = app.window_session.graph().clone();
    let surfaces_before = app.window_session.native_ids().clone();
    assert!(app.prepare_configured_window_rules(&hostile).is_err());
    assert_eq!(app.window_session.graph(), &graph_before);
    assert_eq!(app.window_session.native_ids(), &surfaces_before);
}

#[test]
fn a_stop_fills_the_panes_and_makes_room_for_them_without_opening_anything() {
    use crate::app::tests::driver::TestSession;
    let (_files, _log_path, mut driver) =
        logging_adapter_fixture(crate::config::DebugRequestKind::Launch);
    driver.resize(120, 40);
    let tiled = |driver: &TestSession| {
        driver
            .application()
            .compositor_projection()
            .expect("a current layout")
            .tiled_bounds
    };
    let before = tiled(&driver);

    driver.debug_start("logging");
    let deadline = Instant::now() + Duration::from_secs(6);
    while driver.application().debugger_session.variables().is_empty() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "the stop never reached its variables: {:?}",
            driver.projection().notification
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    assert!(
        driver.projection().picker.is_none(),
        "a stop shows itself in the panes; it does not open a picker"
    );
    let panes = driver
        .application()
        .ui
        .debug_panes
        .clone()
        .expect("the panes are open during the session");
    assert_eq!(panes.status, "paused: fixture");
    assert_eq!(panes.stack, ["logging-frame:1"]);
    assert_eq!(panes.variables, ["child = 1"]);
    let during = tiled(&driver);
    assert!(
        during.height < before.height || during.width < before.width,
        "the editor windows make room for the panes: {before:?} then {during:?}"
    );

    driver.debug_stop();
    driver.pump_debug();
    assert!(
        driver.application().ui.debug_panes.is_none(),
        "the panes close with the session"
    );
    assert_eq!(tiled(&driver), before, "the editor gets its room back");
}

#[test]
fn zoom_and_zen_show_the_pane_alone_and_put_everything_back() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(
            TextBuffer::from_text("one\ntwo\n"),
            Some(PathBuf::from("/workspace/zoom.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let origin_tab = app.state.active_tab().expect("editor tab");
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let origin_view = app.state.active_view().expect("split view");
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
    assert!(app.state.is_zoomed());
    assert_ne!(app.state.active_tab(), Some(origin_tab));
    let zoomed = app.state.active_view().unwrap();
    app.state.views[zoomed].cursor = crate::editor::text::ByteOffset(4);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
    assert!(!app.state.is_zoomed());
    assert_eq!(app.state.active_tab(), Some(origin_tab));
    assert_eq!(app.state.active_view(), Some(origin_view));
    assert_eq!(app.state.editor_tabs().count(), 1);
    assert_eq!(
        app.state.views[origin_view].cursor,
        crate::editor::text::ByteOffset(4)
    );

    let numbers = app.configuration.settings().editor.line_numbers;
    app.process_effect(AppEffect::Zen);
    assert!(app.state.is_zoomed());
    assert_eq!(
        app.configuration.settings().editor.line_numbers,
        crate::config::LineNumbers::Off
    );
    assert!(!app.configuration.settings().editor.git_signs.enabled);
    app.process_effect(AppEffect::Zen);
    assert!(!app.state.is_zoomed());
    assert_eq!(app.configuration.settings().editor.line_numbers, numbers);
    assert!(app.configuration.settings().editor.git_signs.enabled);

    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
    let zoomed_tab = app.state.active_tab();
    app.process_effect(AppEffect::Zen);
    app.process_effect(AppEffect::Zen);
    assert!(
        app.state.is_zoomed(),
        "zen must preserve a pre-existing zoom"
    );
    assert_eq!(app.state.active_tab(), zoomed_tab);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));

    let wrap = app.configuration.settings().editor.wrap;
    app.process_effect(AppEffect::ViewToggle(
        crate::app::configuration::ViewToggle::Wrap,
    ));
    assert_eq!(app.configuration.settings().editor.wrap, !wrap);
    app.process_effect(AppEffect::ViewToggle(
        crate::app::configuration::ViewToggle::LineNumbers,
    ));
    app.process_effect(AppEffect::ViewToggle(
        crate::app::configuration::ViewToggle::LineNumbers,
    ));
    assert_eq!(app.configuration.settings().editor.line_numbers, numbers);
}

fn editor_fixture() -> (crate::editor::EditorState, Windows) {
    let mut state = crate::editor::EditorState::new("/workspace".into());
    state
        .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
        .unwrap();
    let mut windows = Windows::default();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    windows.commit(candidate, &mut state).unwrap();
    (state, windows)
}

#[test]
fn session_restore_removes_plugin_windows_before_replacing_their_tabs() {
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedPane;
    use crate::persistence::PersistedSession;
    use crate::persistence::PersistedTab;
    use crate::windows::ContentTypeId;
    let (mut state, mut windows) = editor_fixture();
    let tab = windows.graph().active_tab().unwrap();
    let graph = &mut state.graph;
    let content = graph
        .create_content(
            ContentTypeId::new("plugin.demo.panel").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "plugin.demo", 1).unwrap();
    let panel = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Fullscreen,
            true,
            "plugin.demo",
            1,
        )
        .unwrap();
    let candidate = windows.prepare_editor(&(&state).into(), "scratch").unwrap();
    windows.commit(candidate, &mut state).unwrap();
    windows
        .core
        .native
        .set_configured_placements(std::collections::BTreeMap::from([(
            "builtin.terminal".into(),
            WindowPlacement::Fullscreen,
        )]))
        .unwrap();
    let rules = [
        crate::persistence::PersistedWindowRule {
            content_type: "builtin.explorer".into(),
            placement: crate::persistence::PersistedWindowPlacement::Fullscreen,
        },
        crate::persistence::PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: crate::persistence::PersistedWindowPlacement::Hidden,
        },
    ];
    let path = std::path::PathBuf::from("/workspace/restored.rs");
    let session = PersistedSession {
        roots: vec!["/workspace".into()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 0,
                    anchor: 0,
                    scroll_line: 0,
                    scroll_column: 0,
                },
            },
            focused_path: Some(path.clone()),
            focused_pane: None,
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
        pinned: Vec::new(),
    };
    let mut draft = state.draft();
    crate::app::persistence::restore::restore_layout(
        &mut draft,
        &session,
        &std::collections::HashMap::from([(
            path.clone(),
            (
                path.clone(),
                crate::editor::text::TextBuffer::from_text("restored"),
            ),
        )]),
    )
    .unwrap();
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let transition = windows
        .prepare_restore_transition(
            draft,
            (&state.version(), catalog.clone()),
            "scratch",
            crate::app::windows::NativeDescriptions {
                signature: crate::app::windows::NativeSurfaceSignature {
                    editor_tab: Some(tab),
                    editor_window: None,
                    focused_plugin: None,
                    explorer_visible: true,
                    explorer_views: std::sync::Arc::from([1]),
                    focused_explorer: None,
                    explorer_size: 24,
                    explorer_edge: crate::windows::DockEdge::Left,
                    active_terminal: None,
                    shown_terminals: std::sync::Arc::from([]),
                    terminal_focused: false,
                    terminal_size: 8,
                    terminal_shrunk: false,
                    terminal_set_revision: Some(0),
                    terminal_width: 80,
                    terminal_height: 24,
                    dashboard_visible: false,
                    picker_rect: None,
                    picker_surface: None,
                    picker_title: "Picker".into(),
                    protected_prompt_height: 0,
                    configuration_reload_id: 0,
                    screen_bottom: 0,
                },
                explorer_title: "Explorer",
                dashboard_title: "Dust",
                picker_title: "Picker",
                terminals: &[],
            },
            &[("demo".into(), 1)],
            crate::app::windows::RestoreWindowPolicy {
                rules: &rules,
                descriptors: &[],
            },
        )
        .unwrap();
    assert!(
        windows.graph().window(panel).is_some(),
        "preparation is detached"
    );
    let restored = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(
        restored.buffers[restored.active_buffer_id().unwrap()]
            .path
            .as_ref(),
        Some(&path)
    );
    assert!(windows.graph().window(panel).is_none());
    assert!(windows.graph().workspace_tab(tab).is_none());
    let explorer = windows.native_ids().explorer().unwrap();
    assert_eq!(
        windows.graph().window(explorer.window).unwrap().placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(
        windows.native_ids().semantic_defaults(None),
        vec![
            ("builtin.explorer", WindowPlacement::Fullscreen),
            ("builtin.terminal", WindowPlacement::Hidden),
        ]
    );
    windows.graph().validate().unwrap();
}

#[test]
fn candidates_are_bound_to_the_session_and_owner_epoch_not_only_graph_revision() {
    let (mut state, mut windows) = editor_fixture();
    let (_, mut other) = editor_fixture();
    assert_eq!(windows.graph(), other.graph());
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let before = other.graph().clone();
    assert!(other.commit(candidate, &mut state).is_err());
    assert_eq!(other.graph(), &before);
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let before = windows.graph().clone();
    windows.invalidate_native();
    assert_eq!(windows.graph(), &before);
    assert!(windows.commit(candidate, &mut state).is_err());
    assert_eq!(windows.graph(), &before);
}

#[test]
fn failed_explorer_reconstruction_does_not_publish_a_partial_graph_or_resources() {
    let (state, windows) = editor_fixture();
    let graph = windows.graph().clone();
    let native = windows.core.native.clone();
    let epoch = windows.epoch;
    let signature = crate::app::windows::native::NativeSurfaceSignature {
        editor_tab: None,
        editor_window: None,
        focused_plugin: None,
        explorer_visible: true,
        explorer_views: (1..=crate::windows::MAX_WINDOWS as u64)
            .collect::<Vec<_>>()
            .into(),
        focused_explorer: Some(1),
        explorer_size: 24,
        explorer_edge: crate::windows::DockEdge::Left,
        active_terminal: None,
        shown_terminals: Vec::new().into(),
        terminal_focused: false,
        terminal_size: 8,
        terminal_shrunk: false,
        terminal_set_revision: Some(0),
        terminal_width: 80,
        terminal_height: 24,
        dashboard_visible: false,
        picker_rect: None,
        picker_surface: None,
        picker_title: "Picker".into(),
        protected_prompt_height: 0,
        configuration_reload_id: 0,
        screen_bottom: 0,
    };
    let error = match windows.prepare_reconcile(
        &(&state).into(),
        "[scratch]",
        crate::app::windows::NativeDescriptions {
            signature,
            explorer_title: "Explorer",
            dashboard_title: "Dust",
            picker_title: "Picker",
            terminals: &[],
        },
    ) {
        Ok(_) => panic!("graph limit must reject reconstruction"),
        Err(error) => error,
    };
    assert_eq!(error, "workspace tab exceeds its non-tiled window limit");
    assert_eq!(windows.graph(), &graph);
    assert_eq!(windows.core.native, native);
    assert_eq!(windows.epoch, epoch);
}

#[test]
fn editor_transition_rejects_changed_and_foreign_sources_without_window_publication() {
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let (mut state, mut windows) = editor_fixture();
    let base = state.clone();
    let before = state.window_version();
    let proposed = state
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let graph = windows.graph().clone();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("intervening"),
            None,
        )
        .unwrap();
    assert!(
        windows
            .accept_editor_transition(transition, &state.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
    let proposed = base
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let (foreign, _) = editor_fixture();
    assert_eq!(foreign.window_revision(), before.revision());
    assert!(
        windows
            .accept_editor_transition(transition, &foreign.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
    let proposed = base
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    windows.invalidate_native();
    assert!(
        windows
            .accept_editor_transition(transition, &base.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn exhausted_owner_rejects_transition_before_either_live_state_is_replaced() {
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let (state, mut windows) = editor_fixture();
    windows.epoch = u64::MAX - 1;
    let proposed = state
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let expected = transition.proposal.window_version();
    let accepted = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(accepted.window_version(), expected);
    assert_eq!(windows.epoch, u64::MAX);
    let graph = windows.graph().clone();
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor(&(&accepted).into(), "[scratch]")
            .is_err()
    );
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn plugin_drafts_cannot_write_editor_resources_foreign_loads_tabs_or_protected_layers() {
    let catalog = crate::plugins::PluginCatalogVersion::default();
    use crate::windows::ContentTypeId;
    use crate::windows::WindowError;
    let (mut state, mut windows) = editor_fixture();
    let editor_window = windows.graph().focused().unwrap();
    let active = windows.graph().active_tab().unwrap();
    let other_tab = windows
        .core
        .graph
        .create_workspace_tab("fixture", 1)
        .unwrap();
    let before = windows.graph().clone();
    let mut draft = windows
        .begin_plugin("demo", 1, (&state.window_version(), &catalog))
        .unwrap();
    assert_eq!(
        draft.place(editor_window, WindowPlacement::Hidden),
        Err(WindowError::InvalidOwner)
    );
    let kind = ContentTypeId::new("plugin.demo.panel").unwrap();
    assert_eq!(
        draft.create_content_with_contract(
            kind.clone(),
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "foreign".into(),
            false,
            "plugin.other",
            1
        ),
        Err(WindowError::InvalidOwner)
    );
    assert_eq!(
        draft.create_content_with_contract(
            kind.clone(),
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "foreign".into(),
            false,
            "plugin.demo",
            2
        ),
        Err(WindowError::InvalidOwner)
    );
    assert!(
        draft
            .create_content_with_contract(
                ContentTypeId::new("builtin.text-buffer").unwrap(),
                ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
                "reserved".into(),
                false,
                "plugin.demo",
                1
            )
            .is_err()
    );
    assert!(
        draft
            .create_content_with_contract(
                kind.clone(),
                ContentContract::adapter_defaults(ContentAdapterKind::TextBuffer),
                "editor".into(),
                false,
                "plugin.demo",
                1
            )
            .is_err()
    );
    let content = draft
        .create_content_with_contract(
            kind,
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = draft.create_view(content, "plugin.demo", 1).unwrap();
    let placement = WindowPlacement::Floating(WindowRect {
        x: 1,
        y: 1,
        width: 20,
        height: 8,
    });
    assert_eq!(
        draft.open_window(other_tab, view, placement, true, "plugin.demo", 1),
        Err(WindowError::WindowOutsideActiveTab)
    );
    assert_eq!(
        draft.open_window(active, view, WindowPlacement::Tiled, true, "plugin.demo", 1),
        Err(WindowError::InvalidPlacement)
    );
    let window = draft
        .open_window(active, view, placement, true, "plugin.demo", 1)
        .unwrap();
    assert_eq!(
        draft.place(
            window,
            WindowPlacement::Overlay(WindowRect {
                x: 0,
                y: 0,
                width: 20,
                height: 8
            })
        ),
        Err(WindowError::InvalidPlacement)
    );
    assert_eq!(windows.graph(), &before);
    let duplicate = draft.clone();
    windows
        .commit_plugin(draft, &state.window_version(), &catalog, &mut state)
        .unwrap();
    assert!(windows.graph().window(window).is_some());
    assert!(
        windows
            .commit_plugin(duplicate, &state.window_version(), &catalog, &mut state)
            .is_err()
    );
    let pending = windows
        .begin_plugin("demo", 1, (&state.window_version(), &catalog))
        .unwrap();
    windows.remove_plugin_load("demo", 1, &mut state).unwrap();
    assert!(windows.graph().window(window).is_none());
    assert!(
        windows
            .commit_plugin(pending, &state.window_version(), &catalog, &mut state)
            .is_err()
    );
    assert!(windows.graph().window(editor_window).is_some());
}

#[test]
fn body_and_notice_bounds_fail_closed_at_tiny_sizes() {
    let config = load_bundled_config().config;
    for width in 0..=10 {
        for height in 0..=10 {
            let rect = WindowRect {
                x: 5,
                y: 7,
                width,
                height,
            };
            let body = crate::ui::render::explorer::explorer_body_geometry(rect);
            assert!(body.y >= rect.y && body.y + body.height <= rect.y + height);
            assert!(body.width <= width);
            if let Some(notice) =
                crate::ui::render::overlays::error_notification_geometry(width, height)
            {
                assert!(notice.x + notice.width <= width);
                assert!(notice.y + notice.height <= height);
            }
            let rows =
                crate::ui::render::documentation::documentation_page_rows(&config, width, height);
            assert!(rows <= usize::from(height.saturating_sub(2)));
        }
    }
}

#[test]
fn accepted_projection_rejects_old_source_graph_size_and_invalidation() {
    let config = load_bundled_config().config;
    let bounds = WindowRect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    };
    let mut graph = crate::windows::WindowGraph::default();
    let tab = graph.create_workspace_tab("fixture", 1).unwrap();
    let content = graph
        .create_content(
            crate::windows::ContentTypeId::new("fixture.explorer").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Explorer".into(),
            false,
            "fixture",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "fixture", 1).unwrap();
    graph
        .open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Left,
                size: 25,
            },
            true,
            "fixture",
            1,
        )
        .unwrap();
    let revision = graph.revision().unwrap();
    let mut windows = Windows::from_test_graph(graph);
    assert!(windows.current(Some(1), (100, 30)).is_none());
    windows.project(ProjectionRequest {
        tab,
        source_revision: 1,
        graph_revision: revision,
        bounds,
        minimum_size: (4, 2),
        insets: crate::windows::TiledInsets {
            top: 1,
            bottom: 1,
            ..Default::default()
        },
        surfaces: NativeSurfaceWindowBinding {
            explorer: Some(WindowId(1)),
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: Some(WindowRect {
            x: 20,
            y: 1,
            width: 60,
            height: 4,
        }),
    });
    windows.project_content(None, &config);
    assert!(windows.current(Some(1), (100, 30)).is_some());
    assert!(windows.current(Some(2), (100, 30)).is_none());
    assert!(windows.current(Some(1), (80, 24)).is_none());
    assert_eq!(
        windows.layout().unwrap().projection.protected_regions.len(),
        1
    );
    let body = windows.content().explorer.unwrap();
    assert_eq!((body.y, body.height), (3, 27));
    assert_eq!(
        windows.content().documentation_rows,
        crate::ui::render::documentation::documentation_page_rows(&config, 75, 30)
    );
    // Exercise the revision predicate itself, rather than merely clearing the cache.
    windows
        .core
        .graph
        .place(WindowId(1), WindowPlacement::Hidden)
        .unwrap();
    assert!(windows.current(Some(1), (100, 30)).is_none());
    windows.invalidate();
    assert!(windows.layout().is_none());
    assert!(windows.content().explorer.is_none());
}

#[test]
fn staged_window_work_rejects_editor_and_catalog_changes_without_a_window_epoch_change() {
    use crate::plugins::PluginContributionCatalog;
    use crate::plugins::PluginContributionIndex;
    let (mut state, mut windows) = editor_fixture();
    let mut catalog = PluginContributionCatalog::default();
    let before = state.window_version();
    let draft = windows
        .begin_plugin("demo", 7, (&before, &catalog.version()))
        .unwrap();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(state.draft(), candidate, catalog.version())
        .unwrap();
    let epoch = windows.epoch;
    let graph = windows.graph().clone();
    catalog
        .install(PluginContributionIndex {
            capabilities: Default::default(),
            owner: "demo".into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
            plugin_load: 7,
            source: "source fixture".into(),
            commands: Default::default(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        })
        .unwrap();
    assert_eq!(windows.epoch, epoch);
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .commit_plugin(draft, &before, &catalog.version(), &mut state)
            .is_err()
    );
    assert!(
        windows
            .accept_editor_transition(transition, &state.version(), &catalog.version())
            .is_err()
    );
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
    let draft = windows
        .begin_plugin("demo", 7, (&before, &catalog.version()))
        .unwrap();
    let window = windows.graph().focused().unwrap();
    let plan = windows
        .plan_focus(window, &before, catalog.version())
        .unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("intervening source"),
            None,
        )
        .unwrap();
    assert_eq!(windows.epoch, epoch);
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .check_plan(&plan, &state.window_version(), &catalog.version())
            .is_err()
    );
    assert!(
        windows
            .commit_plugin(
                draft,
                &state.window_version(),
                &catalog.version(),
                &mut state
            )
            .is_err()
    );
    assert!(
        windows
            .begin_plugin("demo", 7, (&state.window_version(), &catalog.version()))
            .is_err()
    );
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn cross_tab_focus_plan_targets_the_exact_editor_tab_without_eager_mutation() {
    use crate::editor::EditorOperation;
    use crate::editor::EditorTransaction;
    use crate::editor::TransactionId;
    let (mut state, mut windows) = editor_fixture();
    let catalog = crate::plugins::PluginContributionCatalog::default();
    let original_window = windows.graph().focused().expect("first tab window");
    state
        .apply_transaction(
            &EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    windows.commit(candidate, &mut state).unwrap();
    let active_before = state.active_tab();
    let graph_before = windows.graph().clone();
    let plan = windows
        .plan_focus(original_window, &state.window_version(), catalog.version())
        .unwrap();
    assert!(matches!(
        plan.operations().first(),
        Some(EditorOperation::FocusTabId { tab }) if Some(*tab) != active_before
    ));
    assert!(matches!(
        plan.operations().get(1),
        Some(EditorOperation::FocusPaneId { .. })
    ));
    assert_eq!(state.active_tab(), active_before);
    assert_eq!(windows.graph(), &graph_before);
}

#[test]
fn focus_capture_survives_geometry_invalidation_but_not_reset_or_another_editor() {
    let (mut state, mut windows) = editor_fixture();
    let catalog = crate::plugins::PluginContributionCatalog::default();
    let capture = windows.capture_focus().unwrap();
    windows.invalidate_native();
    assert!(
        windows
            .restore_focus_plan(capture, &state.window_version(), catalog.version())
            .is_some()
    );
    let capture = windows.capture_focus().unwrap();
    let (foreign, _) = editor_fixture();
    assert_eq!(state.active_view(), foreign.active_view());
    assert!(
        windows
            .restore_focus_plan(capture, &foreign.window_version(), catalog.version())
            .is_none()
    );
    let capture = windows.capture_focus().unwrap();
    windows.reset_runtime(&[], &mut state).unwrap();
    assert!(
        windows
            .restore_focus_plan(capture, &state.window_version(), catalog.version())
            .is_none()
    );
}

#[test]
fn editor_proposals_cannot_swap_equal_revision_branches_or_rebase_stale_work() {
    use crate::editor::EditorOperation;
    use crate::editor::EditorTransaction;
    use crate::editor::TransactionId;
    let (mut state, mut windows) = editor_fixture();
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let transaction =
        EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab);
    let left = state.prepare_transaction(&transaction).unwrap();
    let right = state.prepare_transaction(&transaction).unwrap();
    assert_eq!(
        left.window_version().revision(),
        right.window_version().revision()
    );
    let candidate = windows.prepare_editor(&left.topology(), "scratch").unwrap();
    let graph = windows.graph().clone();
    assert!(
        windows
            .stage_editor_transition(right, candidate, catalog.clone())
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);

    // A text/cursor-only live mutation does not change window geometry, but must
    // revoke publication of a full detached editor, not silently lose that change.
    let window_source = state.window_version();
    state
        .apply_transaction(&EditorTransaction::new(TransactionId(2)).with_operation(
            EditorOperation::SetCursor {
                view: state.active_view().unwrap(),
                position: crate::editor::text::ByteOffset(1),
            },
        ))
        .unwrap();
    assert_eq!(state.window_version(), window_source);
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor_transition(left, &state.version(), catalog.clone(), "scratch", None)
            .is_err()
    );
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);

    let proposal = state.prepare_transaction(&transaction).unwrap();
    let expected = proposal.window_version();
    let transition = windows
        .prepare_editor_transition(proposal, &state.version(), catalog.clone(), "scratch", None)
        .unwrap();
    let published = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(published.window_version(), expected);
    assert_eq!(windows.core.source.as_ref(), Some(&expected));
}

#[test]
fn invalid_editor_sources_reject_before_graph_copy() {
    let (state, windows) = editor_fixture();
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    let (foreign, _) = editor_fixture();
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor(&(&exhausted).into(), "scratch")
            .is_err()
    );
    assert!(
        windows
            .prepare_editor(&(&foreign).into(), "scratch")
            .is_err()
    );
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
}

/// Run directly with --ignored --nocapture --test-threads=1. Optional Linux
/// allocation observer: tests/ownership_alloc.c; timings run WITHOUT preloading.
#[test]
#[ignore = "explicit window creation cost probe; prints bounded fixture counters/timing"]
fn window_creation_cost_probe() {
    use crate::editor::EditorOperation;
    use crate::editor::EditorState;
    use crate::editor::EditorTransaction;
    use crate::editor::TransactionId;
    use crate::editor::text::TextBuffer;
    use crate::windows::WindowGraph;
    let case = std::env::var("DUST_COST_CASE").unwrap_or_else(|_| "idle".into());
    let iterations: usize = std::env::var("DUST_COST_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    assert!((1..=10000).contains(&iterations));
    let mut state = EditorState::new("/workspace".into());
    let contents = "0123456789abcdef\n".repeat(3856); // 65,552 bytes, 3,857 lines per buffer.
    for n in 0..32 {
        state
            .open_text(
                TextBuffer::from_text(&contents),
                Some(format!("/workspace/{n}.txt").into()),
            )
            .unwrap();
    }
    let mut windows = Windows::default();
    let candidate = windows.prepare_editor(&(&state).into(), "scratch").unwrap();
    windows.commit(candidate, &mut state).unwrap();
    let foreign = EditorState::new("/foreign".into());
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    let mut changed = state.clone();
    changed
        .open_text(TextBuffer::from_text("changed"), None)
        .unwrap();
    let current = state.version();
    let source = state.window_version();
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let transaction =
        EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab);
    let baseline = (
        WindowGraph::test_clone_count(),
        EditorState::test_clone_count(),
        TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        match case.as_str() {
            "idle" | "unchanged" => {
                std::hint::black_box(windows.require_editor_source(&source)).unwrap();
            }
            "invalid" => {
                assert!(
                    windows
                        .prepare_editor(&(&foreign).into(), "scratch")
                        .is_err()
                );
            }
            "exhausted" => {
                assert!(
                    windows
                        .prepare_editor(&(&exhausted).into(), "scratch")
                        .is_err()
                );
            }
            "stale" => {
                assert!(
                    windows
                        .require_editor_source(&changed.window_version())
                        .is_err()
                );
            }
            "valid" => {
                let draft = state.prepare_transaction(&transaction).unwrap();
                std::hint::black_box(windows.prepare_editor_transition(
                    draft,
                    &current,
                    catalog.clone(),
                    "scratch",
                    None,
                ))
                .unwrap();
            }
            _ => panic!("unknown cost case"),
        }
    }
    let elapsed = start.elapsed().as_nanos();
    eprintln!("DUST_ALLOC_END");
    let text = TextBuffer::test_materializations();
    println!(
        "case={case} iterations={iterations} buffers=32 bytes_per_buffer={} elapsed_ns={elapsed} graph_copies={} editor_copies={} text_decodes={} text_displays={}",
        contents.len(),
        WindowGraph::test_clone_count() - baseline.0,
        EditorState::test_clone_count() - baseline.1,
        text.0 - baseline.2.0,
        text.1 - baseline.2.1
    );
    if case != "valid" {
        assert_eq!(WindowGraph::test_clone_count(), baseline.0);
        assert_eq!(EditorState::test_clone_count(), baseline.1);
    }
    assert_eq!(text, baseline.2);
}

#[test]
fn startup_document_and_dashboard_publish_together_only_for_the_accepted_proposal() {
    let (state, mut windows) = editor_fixture();
    windows.dashboard = true;
    let catalog = crate::plugins::PluginCatalogVersion::default();
    let current = state.version();
    let descriptions = || crate::app::windows::NativeDescriptions {
        signature: crate::app::windows::native::NativeSurfaceSignature {
            editor_tab: None,
            editor_window: None,
            focused_plugin: None,
            explorer_visible: false,
            explorer_views: vec![1].into(),
            focused_explorer: None,
            explorer_size: 24,
            explorer_edge: crate::windows::DockEdge::Left,
            active_terminal: None,
            shown_terminals: Vec::new().into(),
            terminal_focused: false,
            terminal_size: 8,
            terminal_shrunk: false,
            terminal_set_revision: Some(0),
            terminal_width: 80,
            terminal_height: 24,
            dashboard_visible: true,
            picker_rect: None,
            picker_surface: None,
            picker_title: "Picker".into(),
            protected_prompt_height: 0,
            configuration_reload_id: 0,
            screen_bottom: 0,
        },
        explorer_title: "Explorer",
        dashboard_title: "Dust",
        picker_title: "Picker",
        terminals: &[],
    };
    let mut draft = state.draft();
    draft
        .open_document(
            crate::editor::text::TextBuffer::from_text("startup"),
            "/workspace/new.txt".into(),
        )
        .unwrap();
    let transition = windows
        .prepare_document_transition(draft, &current, catalog.clone(), "scratch", descriptions())
        .unwrap();
    assert!(windows.dashboard_visible());
    let graph = windows.graph().clone();
    windows.epoch += 1;
    assert!(
        windows
            .accept_editor_transition(transition, &current, &catalog)
            .is_err()
    );
    assert!(windows.dashboard_visible());
    assert_eq!(windows.graph(), &graph);
    let mut draft = state.draft();
    let (buffer, _) = draft
        .open_document(
            crate::editor::text::TextBuffer::from_text("startup"),
            "/workspace/new.txt".into(),
        )
        .unwrap();
    let transition = windows
        .prepare_document_transition(draft, &current, catalog.clone(), "scratch", descriptions())
        .unwrap();
    let accepted = windows
        .accept_editor_transition(transition, &current, &catalog)
        .unwrap();
    assert_eq!(accepted.buffers[buffer].text.to_string(), "startup");
    assert!(!windows.dashboard_visible());
    windows.epoch = u64::MAX;
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(windows.check_editor_source(&(&accepted).into()).is_err());
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
}

#[test]
fn native_command_plans_are_source_bound_before_any_candidate_copy() {
    use crate::app::windows::CommandPolicy;
    use crate::app::windows::WindowCommand;
    let (mut state, mut windows) = editor_fixture();
    let source = state.window_version();
    let catalog = crate::plugins::PluginCatalogVersion::default();
    windows.project(ProjectionRequest {
        tab: windows.graph().active_tab().unwrap(),
        source_revision: state.window_revision().unwrap(),
        graph_revision: windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let policy = || CommandPolicy {
        selected_terminal: None,
        pane_step: 2,
        terminal_step: 2,
    };
    let copies = crate::windows::WindowGraph::test_clone_count();
    let clone_plan = windows
        .plan_window_command(
            WindowCommand::Clone("focused", crate::windows::TileAxis::Horizontal, 500),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("clone plan"));
    assert!(matches!(
        clone_plan.operations(),
        [crate::editor::EditorOperation::ClonePane { .. }]
    ));
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
    let mut changed = state.clone();
    changed
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .expect("change clone source");
    let before_clone_rejection = windows.graph().clone();
    assert!(
        windows
            .check_plan(&clone_plan, &changed.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &before_clone_rejection);
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    assert!(
        windows
            .check_plan(&clone_plan, &exhausted.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &before_clone_rejection);

    let (foreign_state, mut foreign_windows) = editor_fixture();
    foreign_windows.project(ProjectionRequest {
        tab: foreign_windows.graph().active_tab().unwrap(),
        source_revision: foreign_state.window_revision().unwrap(),
        graph_revision: foreign_windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let foreign_before = foreign_windows.graph().clone();
    assert!(
        foreign_windows
            .check_plan(&clone_plan, &foreign_state.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(foreign_windows.graph(), &foreign_before);

    let placement_plan = windows
        .plan_window_command(
            WindowCommand::Place(
                "focused",
                crate::windows::WindowPlacement::Floating(WindowRect {
                    x: 3,
                    y: 2,
                    width: 30,
                    height: 8,
                }),
            ),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("source-bound editor placement plan"));
    let placement_before = windows.graph().clone();
    windows.invalidate_native();
    assert!(
        windows
            .accept_window_command(placement_plan, &source, &catalog, &mut state)
            .is_err()
    );
    assert_eq!(windows.graph(), &placement_before);

    windows.project(ProjectionRequest {
        tab: windows.graph().active_tab().unwrap(),
        source_revision: state.window_revision().unwrap(),
        graph_revision: windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let plan = windows
        .plan_window_command(
            WindowCommand::Swap("focused", "focused"),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("plan"));
    windows
        .accept_window_command(plan, &source, &catalog, &mut state)
        .unwrap();
    let plan = windows
        .plan_window_command(
            WindowCommand::Clone("focused", crate::windows::TileAxis::Vertical, 500),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("clone plan"));
    windows.invalidate_native();
    let before = windows.graph().clone();
    let copies = crate::windows::WindowGraph::test_clone_count();
    assert!(
        windows
            .accept_window_command(plan, &source, &catalog, &mut state)
            .is_err()
    );
    assert_eq!(windows.graph(), &before);
    assert_eq!(crate::windows::WindowGraph::test_clone_count(), copies);
}

#[test]
fn a_debug_session_shrinks_the_terminal_only_on_a_short_screen_and_gives_it_back() {
    let mut session = Windows::new(false, 12);
    session.observe_size((80, 24));
    session.set_debugging(true);
    assert_eq!(session.terminal_height(), 5);
    session.observe_size((120, 40));
    assert!(!session.terminal_shrunk());
    assert_eq!(session.terminal_height(), 12);
    session.observe_size((80, 24));
    assert!(session.terminal_shrunk());
    assert_eq!(session.terminal_height(), 5);
    session.set_debugging(false);
    assert_eq!(session.terminal_height(), 12);
    session.observe_size((120, 40));
    session.observe_size((80, 24));
    assert_eq!(session.terminal_height(), 12);
    session.set_debugging(false);
    session.observe_size((80, 24));
    assert_eq!(session.terminal_height(), 12);
}
