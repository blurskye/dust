//! Dashboard selection and window rules after configuration changes.

use crate::app::tests::app;

#[test]
pub(in crate::app) fn dashboard_selection_tracks_the_actions_after_configuration_refresh() {
    let (_app_dirs, mut app) = app();
    app.project_configuration();
    app.ui.dashboard_selected = app.ui.dashboard_actions.len() - 1;
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.dashboard.action.truncate(2);
    let expected = fixture.dashboard.action[1].command.clone();
    app.configuration.install_fixture(fixture);
    app.project_configuration();
    assert_eq!(app.ui.dashboard_selected, 1);
    assert_eq!(
        app.configuration
            .dashboard_invocation(app.ui.dashboard_selected)
            .unwrap()
            .id
            .as_str(),
        expected
    );
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.dashboard.action.clear();
    app.configuration.install_fixture(fixture);
    app.project_configuration();
    assert_eq!(app.ui.dashboard_selected, 0);
    assert!(
        app.configuration
            .dashboard_invocation(app.ui.dashboard_selected)
            .is_none()
    );
}

#[test]
pub(in crate::app) fn persisted_window_rules_validate_the_final_fullscreen_configuration() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    app.terminal_session.fixture(
        PtyId(7),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(7));
    app.sync_terminal_panel();
    let terminal = app.window_session.native_ids().terminal(7).unwrap();
    let candidate = app
        .prepare_configured_window_rules(&crate::config::WindowsConfig {
            rules: vec![crate::config::WindowRuleConfig {
                content_type: "builtin.terminal".into(),
                placement: crate::config::WindowPlacementConfig::Fullscreen,
            }],
        })
        .unwrap();
    app.window_session
        .accept_current(candidate, &app.state.window_version(), &mut app.state)
        .unwrap();
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Fullscreen
    );
    app.apply_persisted_window_rules(&[
        PersistedWindowRule {
            content_type: "builtin.explorer".into(),
            placement: PersistedWindowPlacement::Fullscreen,
        },
        PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: PersistedWindowPlacement::Hidden,
        },
    ])
    .unwrap();
    let explorer = app.window_session.native_ids().explorer().unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    assert_eq!(
        app.window_session.native_ids().semantic_defaults(None),
        vec![
            ("builtin.explorer", WindowPlacement::Fullscreen),
            ("builtin.terminal", WindowPlacement::Hidden),
        ]
    );
}
use crate::editor::text::TextBuffer;
use crate::persistence::PersistedWindowPlacement;
use crate::persistence::PersistedWindowRule;
use crate::process::PtyId;
use crate::ui::PtyScreen;
use crate::windows::WindowPlacement;
