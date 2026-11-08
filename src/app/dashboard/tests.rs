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
fn the_dashboard_lists_recent_files_here_and_other_recent_projects() {
    use crate::app::workspace::tests::wait_root_operation;
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.process_trust_events();
    app.maybe_prompt_for_unseen_workspace();
    app.close_prompt();
    wait_trust(&mut app);
    let first = app.state.workspace_root.clone();
    let second = tempfile::tempdir().unwrap();
    let second_path = second.path().canonicalize().unwrap();
    app.switch_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert_eq!(app.state.workspace_root, second_path);
    assert!(app.workspace.seen_roots().contains(&first));

    // Recorded oldest first; the newest comes first in each list.
    for path in [
        first.join("old.rs"),
        second_path.join("src/lib.rs"),
        std::path::PathBuf::from("/never-opened-as-a-workspace/x.rs"),
        first.join("newer.rs"),
        second_path.join("main.rs"),
    ] {
        app.persistence.record_recent(path, 256);
    }
    app.sync_dashboard_ui();
    let summary = &app.ui.dashboard_workspace;
    assert_eq!(summary.recent_files, ["main.rs", "src/lib.rs"]);
    assert_eq!(summary.recent_projects, [first]);
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
fn the_dashboard_cursor_runs_items_and_ctrl_h_l_reach_an_agent_beside_it() {
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

    // The cursor stops at the ends and sits on the label of the item Enter runs.
    for key in ["k", "j", "j", "k", "down"] {
        driver.key(key);
    }
    assert_eq!(driver.application_mut().ui.dashboard_selected, 2);
    driver.key("up");
    let frame = driver.frame();
    let (x, y, _) = frame.cursor.expect("dashboard cursor");
    let label = (x..x + 8)
        .filter_map(|column| {
            frame
                .grid
                .get(column, y)
                .map(|cell| cell.symbol.to_string())
        })
        .collect::<String>();
    assert_eq!(label, "New File");
    driver.key("enter");
    assert!(driver.application_mut().state.active_view().is_some());
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
    // The dashboard is centred in the room left of the agent, not under it; the rule under
    // its header spans its whole width.
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
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|cells| cells.concat().contains("Find File"))
    );
    let header = rows
        .iter()
        .position(|cells| cells.concat().contains("⠿ dust"))
        .expect("the dashboard draws its header");
    let rule = &rows[header + 1];
    let start = rule.iter().position(|cell| cell == "─").unwrap();
    let end = rule.iter().rposition(|cell| cell == "─").unwrap();
    assert_eq!(
        rows[header].iter().position(|cell| cell == "⠿"),
        Some(start)
    );
    let middle = (start + end) / 2;
    assert!(
        middle.abs_diff(usize::from(rect.x) / 2) <= 2,
        "dashboard from {start} to {end}, agent at {}",
        rect.x
    );
    driver.key("ctrl-l");
    assert_eq!(
        driver.application_mut().window_session.focused_terminal(),
        Some(agent.0)
    );
}
