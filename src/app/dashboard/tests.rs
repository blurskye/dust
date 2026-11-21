//! Dashboard behavior and application integration tests.

#[cfg(unix)]
use crate::app::AppEffect;
#[cfg(unix)]
use crate::app::agent::commands::AgentCommand;
use crate::app::configuration::validate_configured_commands;
use crate::app::tests::*;
use crate::editor::text::TextBuffer;
use crate::windows::WindowPlacement;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[test]
fn showing_the_last_editor_dismisses_the_dashboard() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let editor = app.window_session.graph().focused().unwrap();
    app.place_graph_window(&format!("window:{}", editor.0), WindowPlacement::Hidden)
        .unwrap();
    assert!(app.window_session.dashboard_visible());
    app.show_graph_window(&format!("window:{}", editor.0))
        .unwrap();
    assert!(!app.window_session.dashboard_visible());
    assert_eq!(app.active_input_context(), "editor.normal");
    assert_eq!(app.window_session.graph().focused(), Some(editor));
    let dashboard = app.window_session.native_ids().dashboard().unwrap().window;
    assert_eq!(
        app.window_session
            .graph()
            .window(dashboard)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
}

#[test]
fn the_dashboard_lists_recent_files_with_their_projects_and_this_workspace_first() {
    use crate::app::workspace::tests::wait_root_operation;
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.process_trust_events();
    app.maybe_prompt_for_unseen_workspace();
    app.close_prompt();
    wait_trust(&mut app);
    let first = app.state.workspace_root.clone();
    let second = tempfile::tempdir().unwrap();
    let second_path = crate::files::canonicalize(second.path()).unwrap();
    app.switch_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert_eq!(app.state.workspace_root, second_path);
    assert!(app.workspace.seen_roots().contains(&first));

    // Recorded oldest first; the newest comes first.
    let now = app
        .clock
        .system_time()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere = crate::files::canonicalize(elsewhere.path())
        .unwrap()
        .join("x.rs");
    std::fs::create_dir_all(second_path.join("src")).unwrap();
    for (path, minutes_ago) in [
        (first.join("old.rs"), 3 * 60),
        (second_path.join("src/lib.rs"), 14),
        (elsewhere.clone(), 5),
        (second_path.join("main.rs"), 2),
    ] {
        std::fs::write(&path, "").unwrap();
        app.persistence
            .record_recent(path, now - minutes_ago * 60, 256);
    }
    app.sync_dashboard_ui();
    let first_name = first.file_name().unwrap().to_string_lossy().into_owned();
    let summary = &app.ui.dashboard_workspace;
    let files = summary
        .recent_files
        .iter()
        .map(|file| {
            (
                file.label.as_str(),
                file.project.as_deref(),
                file.age.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        files,
        [
            ("main.rs", None, "2m"),
            (elsewhere.to_str().unwrap(), None, "5m"),
            ("src/lib.rs", None, "14m"),
            ("old.rs", Some(first_name.as_str()), "3h"),
        ]
    );
    assert_eq!(summary.projects[0].path, second_path);
    // Other projects appear once the background read confirms they still exist.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.ui.dashboard_workspace.projects.len() < 2 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.sync_dashboard_ui();
    }
    assert_eq!(app.ui.dashboard_workspace.projects[1].path, first);
}

#[cfg(unix)]
#[test]
fn projects_leave_out_tutor_folders_missing_folders_and_symlinked_duplicates() {
    use crate::app::workspace::tests::wait_root_operation;
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.process_trust_events();
    app.maybe_prompt_for_unseen_workspace();
    app.close_prompt();
    wait_trust(&mut app);
    let home = app.state.workspace_root.clone();
    let folders = tempfile::tempdir().unwrap();
    let folders = folders.path().canonicalize().unwrap();
    let tally = folders.join("tally");
    let gone = folders.join("gone");
    let proj = folders.join("proj");
    std::fs::create_dir(&tally).unwrap();
    std::fs::create_dir(&gone).unwrap();
    std::os::unix::fs::symlink(&tally, &proj).unwrap();
    let practice = tempfile::Builder::new()
        .prefix("dusty-tutor-")
        .tempdir()
        .unwrap();
    // Open each as the workspace so Dusty remembers it, ending in the symlink to tally.
    for root in [
        tally.clone(),
        practice.path().to_path_buf(),
        gone.clone(),
        home.clone(),
        proj.clone(),
    ] {
        app.switch_workspace_root(root);
        wait_root_operation(&mut app);
        wait_trust(&mut app);
        app.close_prompt();
    }
    std::fs::remove_dir(&gone).unwrap();
    let root = app.state.workspace_root.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    app.sync_dashboard_ui();
    while app.dashboard.reading.is_some() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.sync_dashboard_ui();
    }
    let projects = app
        .ui
        .dashboard_workspace
        .projects
        .iter()
        .map(|project| project.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(projects, [root, home]);
}

#[test]
fn tutor_files_are_not_recent_and_deleted_files_leave_the_dashboard() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let practice = tempfile::Builder::new()
        .prefix("dusty-tutor-")
        .tempdir()
        .unwrap();
    let lesson = practice.path().join("practice.rs");
    let kept = root.join("kept.rs");
    let deleted = root.join("deleted.rs");
    for file in [&lesson, &kept, &deleted] {
        std::fs::write(file, "fn main() {}\n").unwrap();
    }
    app.record_recent_file(lesson.clone());
    assert!(app.persistence.recent_files().is_empty());

    // A session saved by an older version may still list a tutor file.
    app.persistence.record_recent(lesson, 0, 256);
    app.record_recent_file(deleted.clone());
    app.record_recent_file(kept);
    std::fs::remove_file(&deleted).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    app.sync_dashboard_ui();
    while app.dashboard.reading.is_some() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.sync_dashboard_ui();
    }
    let labels = app
        .ui
        .dashboard_workspace
        .recent_files
        .iter()
        .map(|file| file.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["kept.rs"]);
}

#[test]
fn recent_files_saved_without_an_open_time_show_no_age() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    std::fs::write(root.join("notes.md"), "").unwrap();
    let mut saved = crate::persistence::PersistedState {
        recent_files: vec![crate::persistence::RecentFile {
            path: root.join("notes.md"),
            opened_at: None,
        }],
        ..crate::persistence::PersistedState::default()
    };
    app.persistence.restore_history(&mut saved);
    app.sync_dashboard_ui();
    let file = &app.ui.dashboard_workspace.recent_files[0];
    assert_eq!((file.label.as_str(), file.age.as_str()), ("notes.md", ""));
}

#[test]
fn ctrl_hjkl_move_between_boxes_and_enter_and_numbers_open_the_right_file() {
    use crate::app::tests::driver::TestSession;
    use crate::ui::DashboardPanel;
    let mut driver = TestSession::new(crate::config::load_bundled_config());
    driver.resize(160, 40);
    let root = driver.application_mut().state.workspace_root.clone();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(root.join(name), format!("{name}\n")).unwrap();
    }
    let show_dashboard = |driver: &mut TestSession| {
        let app = driver.application_mut();
        app.set_dashboard_visibility(true);
        for name in ["c.txt", "b.txt", "a.txt"] {
            app.persistence.record_recent(root.join(name), 0, 256);
        }
        app.sync_dashboard_ui();
        assert_eq!(app.active_input_context(), "dashboard");
    };
    let focus = |driver: &mut TestSession| {
        let app = driver.application_mut();
        crate::ui::render::dashboard_focus(&app.ui, app.dashboard_panels())
    };
    show_dashboard(&mut driver);
    assert_eq!(focus(&mut driver), DashboardPanel::RecentFiles);
    // Ctrl-h/j/k/l move through the 2x2 grid. Outside a Git repository the changes box has
    // nothing to select, so a move toward it goes past it, here off the edge, and stays put.
    for (key, expected) in [
        ("ctrl-l", DashboardPanel::RecentFiles),
        ("ctrl-j", DashboardPanel::Projects),
        ("ctrl-l", DashboardPanel::Start),
        ("ctrl-k", DashboardPanel::Start),
        ("ctrl-h", DashboardPanel::Projects),
        ("ctrl-k", DashboardPanel::RecentFiles),
        ("tab", DashboardPanel::RecentFiles),
    ] {
        driver.key(key);
        assert_eq!(focus(&mut driver), expected, "after {key}");
    }

    // We entered Recent Files from below, so start at its last row and move up one.
    // Enter opens the selected file.
    driver.key("k");
    assert_eq!(driver.application_mut().ui.dashboard_row, 1);
    driver.key("enter");
    assert_eq!(driver.wait_for_opened_file().as_deref(), Some("b.txt"));

    // A number opens that recent file whichever box is focused.
    show_dashboard(&mut driver);
    driver.key("ctrl-j");
    driver.key("3");
    assert_eq!(driver.wait_for_opened_file().as_deref(), Some("c.txt"));
}

#[test]
fn dashboard_motion_continues_at_box_edges() {
    use crate::app::tests::driver::TestSession;
    use crate::app::windows::NativeSurfaceTarget;
    use crate::ui::render::{dashboard_focus, dashboard_selected_row};
    use crate::ui::{DashboardChange, DashboardChanges, DashboardPanel, DashboardProject};
    use crate::windows::WindowRect;

    let mut driver = TestSession::new(crate::config::load_bundled_config());
    driver.resize(160, 40);
    let app = driver.application_mut();
    assert_eq!(app.active_input_context(), "dashboard");
    let root = app.state.workspace_root.clone();
    app.ui.dashboard_workspace.changes = Some(DashboardChanges {
        files: ["a.txt", "b.txt"]
            .map(|name| DashboardChange {
                path: root.join(name),
                label: name.into(),
                code: 'M',
                lines: None,
            })
            .into(),
        changed: 2,
        untracked: 0,
    });
    app.ui.dashboard_workspace.projects = vec![DashboardProject {
        path: root,
        name: "project".into(),
        branch: None,
    }];
    let focus = |driver: &TestSession| {
        let app = driver.application();
        let panel = dashboard_focus(&app.ui, app.dashboard_panels());
        (panel, dashboard_selected_row(&app.ui, panel))
    };

    assert_eq!(focus(&driver), (DashboardPanel::Start, 0));
    let window = driver.focused_window();
    let notices = driver.application().notices.history().count();
    for (key, panel, row) in [
        ("k", DashboardPanel::Changes, 1),
        ("up", DashboardPanel::Changes, 0),
        ("k", DashboardPanel::Changes, 0),
        ("j", DashboardPanel::Changes, 1),
        ("j", DashboardPanel::Start, 0),
        ("h", DashboardPanel::Projects, 0),
        ("l", DashboardPanel::Start, 0),
        ("left", DashboardPanel::Projects, 0),
        ("ctrl-l", DashboardPanel::Start, 0),
        ("right", DashboardPanel::Start, 0),
        ("ctrl-l", DashboardPanel::Start, 0),
        ("ctrl-j", DashboardPanel::Start, 0),
    ] {
        driver.key(key);
        assert_eq!(focus(&driver), (panel, row), "after {key}");
        assert_eq!(driver.focused_window(), window, "after {key}");
    }
    let last = driver.application().ui.dashboard_actions.len() - 1;
    for _ in 0..=last {
        driver.key("down");
    }
    assert_eq!(focus(&driver), (DashboardPanel::Start, last));
    assert_eq!(driver.focused_window(), window);
    assert_eq!(driver.application().notices.history().count(), notices);

    // A window at the bottom right returns to Start, not the previously focused Projects box.
    driver.key("h");
    driver.key("e");
    driver.reconcile_windows();
    driver
        .application_mut()
        .apply_native_window_placement(
            NativeSurfaceTarget::Explorer,
            WindowPlacement::Floating(WindowRect {
                x: 115,
                y: 30,
                width: 30,
                height: 8,
            }),
        )
        .unwrap();
    assert_eq!(driver.application().active_input_context(), "explorer");
    driver.key("ctrl-k");
    assert_eq!(driver.focused_window(), window);
    assert_eq!(focus(&driver), (DashboardPanel::Start, last));
}

#[test]
fn configured_text_and_dashboard_commands_are_schema_validated() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config();
    assert!(validate_configured_commands(&app.commands, &loaded).is_ok());
    loaded
        .config
        .input
        .text_commands
        .insert("editor.insert".into(), "unknown.command".into());
    assert!(
        validate_configured_commands(&app.commands, &loaded)
            .expect_err("invalid command")
            .contains("editor.insert")
    );
}

#[cfg(unix)]
#[test]
fn the_dashboard_selection_runs_actions_and_ctrl_h_l_reach_an_agent_beside_it() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let agents = &mut loaded.config.agents;
    agents.connection = crate::config::AgentConnection::Terminal;
    let mut profile = agents.profiles[&agents.default].clone();
    profile.cli = vec!["sh".into(), "-c".into(), "echo READY; cat".into()];
    agents.profiles.insert("echoer".into(), profile);
    agents.default = "echoer".into();
    let mut driver = TestSession::new(loaded);
    driver.trust_owned_terminal_fixture();
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(driver.application_mut().window_session.terminal_focused());
    driver.key("ctrl-h");
    assert!(!driver.application_mut().window_session.terminal_focused());
    assert_eq!(driver.application_mut().active_input_context(), "dashboard");
    driver.key("ctrl-l");
    assert!(driver.application_mut().window_session.terminal_focused());
    driver.key("ctrl-h");

    // With no recent files the actions are selected; the selection stops at the ends, and
    // Enter runs the highlighted action.
    for key in ["k", "j", "j", "k", "down", "down", "down"] {
        driver.key(key);
    }
    assert_eq!(driver.application_mut().ui.dashboard_selected, 4);
    let frame = driver.frame();
    let selected = (0..frame.grid.height())
        .flat_map(|y| (0..frame.grid.width()).map(move |x| (x, y)))
        .filter_map(|(x, y)| frame.grid.get(x, y))
        .filter(|cell| {
            matches!(
                cell.style,
                crate::ui::Style::Selection | crate::ui::Style::SelectionText(_)
            )
        })
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(selected.contains("tutor"), "{selected:?}");
    // The tutor action offers its languages.
    driver.key("enter");
    assert!(driver.application_mut().ui.picker.is_some());
}

#[cfg(unix)]
#[test]
fn ctrl_h_from_an_agent_beside_the_dashboard_returns_to_it_and_it_stays_beside_the_agent() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let agents = &mut loaded.config.agents;
    agents.connection = crate::config::AgentConnection::Terminal;
    let mut profile = agents.profiles[&agents.default].clone();
    profile.cli = vec!["sh".into(), "-c".into(), "echo READY; cat".into()];
    agents.profiles.insert("echoer".into(), profile);
    agents.default = "echoer".into();
    let mut driver = TestSession::new(loaded);
    driver.trust_owned_terminal_fixture();
    let deadline = Instant::now() + Duration::from_secs(20);
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let agent = driver
        .application_mut()
        .displayed_terminal()
        .expect("agent");
    driver.key("ctrl-h");
    let app = driver.application_mut();
    assert_eq!(app.window_session.focused_terminal(), None);
    assert!(app.window_session.dashboard_visible());
    let rect = app.terminal_rect(agent).expect("the agent stays");
    // The dashboard is drawn in the room left of the agent, not under it: its boxes close
    // before the agent's first column.
    let frame = driver.frame();
    let rows = (0..rect.y + rect.height)
        .map(|y| {
            (0..rect.x)
                .map(|x| {
                    frame
                        .grid
                        .get(x, y)
                        .map_or(" ".into(), |cell| cell.symbol.to_string())
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|row| row.contains("find file")));
    let start = rows
        .iter()
        .find(|row| row.contains("Start"))
        .expect("the dashboard draws its actions box");
    assert!(start.trim_end().ends_with('╮'), "{start:?}");
    driver.key("ctrl-l");
    assert_eq!(
        driver.application_mut().window_session.focused_terminal(),
        Some(agent.0)
    );
}
