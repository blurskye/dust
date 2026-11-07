//! Dashboard behavior and application integration tests.

use crate::app::AppEffect;
use crate::app::agent::commands::AgentCommand;
use crate::app::configuration::validate_configured_commands;
use crate::app::tests::*;
use crate::editor::text::TextBuffer;
use crate::windows::WindowPlacement;
use std::thread;
use std::time::Duration;
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
    let deadline = Instant::now() + Duration::from_secs(4);
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
    let deadline = Instant::now() + Duration::from_secs(6);
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
    // The menu is centred in the room left of the agent, not under it.
    let frame = driver.frame();
    let menu = (0..rect.y + rect.height)
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
        .find(|cells| cells.concat().contains("Find File"))
        .expect("the dashboard draws");
    let start = menu.iter().position(|cell| cell.trim() != "").unwrap();
    let end = menu.iter().rposition(|cell| cell.trim() != "").unwrap();
    let middle = (start + end) / 2;
    assert!(
        middle.abs_diff(usize::from(rect.x) / 2) <= 2,
        "menu from {start} to {end}, agent at {}",
        rect.x
    );
    driver.key("ctrl-l");
    assert_eq!(
        driver.application_mut().window_session.focused_terminal(),
        Some(agent.0)
    );
}
