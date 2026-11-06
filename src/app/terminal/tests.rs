//! Terminal behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::agent::commands::AgentCommand;
use crate::app::configuration::compile_keymaps;
use crate::app::startup::StartupTarget;
use crate::app::tests::*;
use crate::app::workspace::tests::wait_root_operation;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;
use crate::editor::text::TextBuffer;
use crate::persistence::PersistedWindowPlacement;
use crate::persistence::PersistedWindowRule;
use crate::process::PtyId;
use crate::ui::ContentTabKind;
use crate::ui::PtyScreen;
use crate::ui::UiEvent;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn a_pending_root_switch_rechecks_new_edits_and_terminals() {
    for terminal in [None, Some(false), Some(true)] {
        let (_app_dirs, mut app) = app();
        let old = app.state.workspace_root.clone();
        let new = old.join(".worktree/topic");
        fs::create_dir_all(&new).unwrap();
        prepare_trust_identity(&mut app);
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("clean"), Some(old.join("file.txt")))
            .unwrap();
        app.switch_workspace_root(new);
        assert!(app.workspace.root_operation_pending());
        if let Some(running) = terminal {
            // A newly opened terminal must also block before its process reports Running.
            app.terminal_session.fixture(
                PtyId(1),
                crate::app::terminal::TerminalFixture {
                    profile: "shell".into(),
                    screen: crate::ui::PtyScreen::new(80, 8, 100),
                    running,
                    exit_code: None,
                    truncated: false,
                },
                app.workspace.version(),
            );
            app.terminal_session.fixture_cwd(PtyId(1), old.clone());
        } else {
            app.replace_with_recovered(buffer, "dirty".into()).unwrap();
        }
        app.process_trust_events();
        assert!(!app.workspace.root_operation_pending());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            app.process_trust_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.state.workspace_root, old);
        assert!(app.state.buffers.contains_key(buffer));
        if let Some(running) = terminal {
            assert!(app.terminal_session.contains(PtyId(1)));
            assert_eq!(app.terminal_session.running_count(), usize::from(running));
        }
    }
}

#[test]
fn saving_an_existing_empty_file_detects_external_deletion() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("existing-empty.txt");
    fs::write(&path, b"").unwrap();
    app.edit_path(path.clone());
    crate::app::tests::wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert!(!app.state.buffers[buffer].new_file);
    app.handle_configured_key(KeyEvent::character('i'));
    app.handle_configured_key(KeyEvent::character('x'));
    app.handle_configured_key("escape".parse::<KeyEvent>().unwrap());
    fs::remove_file(&path).unwrap();
    // Reopening the absent path must not turn this existing buffer into a new file.
    app.edit_path(path.clone());
    assert!(!app.state.buffers[buffer].new_file);
    app.invoke(crate::editor::CommandInvocation::new("file.save", InvocationSource::Test).unwrap());
    crate::app::tests::wait_vfs(&mut app);
    assert!(!path.exists());
    assert!(app.state.buffers[buffer].dirty);
}

/// The page keys have names a keymap can use, and a focused terminal receives them.
#[test]
fn page_keys_parse_and_count_as_terminal_navigation() {
    assert_eq!(
        "pagedown".parse::<KeyEvent>().unwrap().code,
        crate::editor::input::KeyCode::PageDown
    );
    assert_eq!(
        "pageup".parse::<KeyEvent>().unwrap().code,
        crate::editor::input::KeyCode::PageUp
    );
    assert_eq!(
        "pagedown"
            .parse::<KeyEvent>()
            .unwrap()
            .terminal_navigation(),
        Some(crate::editor::input::TerminalNavigationKey::PageDown)
    );
}

#[test]
fn an_empty_plugin_list_says_how_to_get_one() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::PluginList);
    assert_eq!(
        app.ui.picker.as_ref().expect("picker").footer.as_deref(),
        Some("No plugins yet · :pkgsearch <name> finds one · Esc Esc Close")
    );
}

#[test]
fn switching_folders_with_open_terminals_warns_before_ending_them() {
    let main = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let main = main.path().canonicalize().unwrap();
    let nested = main.join(".worktree/feature");
    fs::create_dir_all(&nested).unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: nested.clone(),
        },
        crate::config::load_bundled_config(),
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: crate::ui::PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.fixture_cwd(PtyId(1), nested);
    app.switch_workspace_root(main.clone());
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|text| text.contains("switch again")),
        "{:?}",
        app.ui.notification
    );
    assert_eq!(app.terminal_session.running_count(), 1, "nothing ended yet");
    app.switch_workspace_root(main.clone());
    assert_eq!(app.ui.notification.as_deref(), Some("switching…"));
    wait_root_operation(&mut app);
    assert_eq!(app.state.workspace_root, main);
    assert_eq!(app.workspace.roots().len(), 1);
    assert_eq!(app.workspace.roots()[0].identity.canonical_path, main);
    assert!(!app.terminal_session.contains(PtyId(1)));
    assert_eq!(app.terminal_session.running_count(), 0);
}

#[test]
fn reloading_terminal_settings_keeps_running_terminals() {
    let (_app_dirs, mut app) = app();
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: crate::ui::PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    let mut candidate = crate::config::load_bundled_config();
    let mut profile = candidate.config.terminals.profiles["shell"].clone();
    profile.rows = 40;
    candidate
        .config
        .terminals
        .profiles
        .insert("another".into(), profile);
    app.apply_configuration_candidate(candidate, false)
        .expect("accepted configuration");
    assert_eq!(app.terminal_session.running_count(), 1);
    assert!(
        app.configuration
            .settings()
            .terminals
            .profiles
            .contains_key("another")
    );
}

#[test]
fn removing_a_root_preserves_terminals_and_updates_relative_paths() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let first = app.workspace.active_root().unwrap();
    let path = app.state.workspace_root.clone();
    let extra = tempfile::tempdir().unwrap();
    let second = app
        .workspace
        .fixture_add_root(
            "second".into(),
            crate::files::RootIdentity::capture(extra.path()).unwrap(),
        )
        .unwrap();
    app.activate_workspace_root(second);
    app.terminal_session.fixture(
        PtyId(101),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 24, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.fixture_cwd(PtyId(101), path.clone());
    let first_file = path.join("first.txt");
    let second_file = extra.path().canonicalize().unwrap().join("second.txt");
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(app.workspace_relative_label(&first_file), "first.txt");
    assert_eq!(app.workspace_relative_label(&second_file), "second.txt");
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 1);
    assert!(app.workspace.roots().iter().all(|root| root.id != second));
    assert_eq!(app.workspace_relative_label(&first_file), "first.txt");
    assert_eq!(
        app.workspace_relative_label(&second_file),
        second_file.display().to_string()
    );
    assert_eq!(app.workspace.active_root(), Some(first));
    assert_eq!(app.state.workspace_root, path);
    assert!(app.terminal_session.contains(PtyId(101)));
    app.process_pty_events();
    assert!(app.terminal_session.contains(PtyId(101)));
}

#[test]
fn floating_terminal_receives_clicks_above_underlying_buffer_tabs() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    let (second, _) = app
        .state
        .open_text(TextBuffer::from_text("second"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(40, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    app.process_effect(AppEffect::WindowFloat {
        target: "terminal".into(),
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    });
    let target = app.state.content_for_buffer(first).unwrap();
    let bounds = app.workspace_bounds().unwrap();
    let column = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap()
        + bounds.x;
    let row = bounds.y + u16::from(app.state.editor_tabs().count() > 1);
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseRightDown,
            Default::default(),
        ),
        column,
        row,
    });
    assert!(app.state.buffers.contains_key(first));
    assert!(app.state.buffers.contains_key(second));
    assert!(app.window_session.terminal_focused());
}

#[test]
fn closing_an_explicit_terminal_window_keeps_the_focused_terminal() {
    for running in [false, true] {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("source"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        for id in [PtyId(1), PtyId(2)] {
            app.terminal_session.fixture(
                id,
                crate::app::terminal::TerminalFixture {
                    profile: "shell".into(),
                    screen: PtyScreen::new(40, 8, 100),
                    running,
                    exit_code: (!running).then_some(0),
                    truncated: false,
                },
                app.workspace.version(),
            );
        }
        app.terminal_session.select(PtyId(1));
        app.set_terminal_focus(true);
        app.sync_terminal_panel();
        app.reconcile_editor_windows();
        let target = app.window_session.native_ids().terminal(2).unwrap().window;
        assert_eq!(app.displayed_terminal(), Some(PtyId(1)));
        app.close_graph_window(&format!("window:{}", target.0))
            .unwrap();
        if running {
            assert!(
                app.input
                    .prompt()
                    .unwrap()
                    .confirmation
                    .as_ref()
                    .unwrap()
                    .title
                    .contains("#2")
            );
            app.submit_terminal_close(crate::ui::ConfirmationChoice::Yes);
        }
        assert!(app.terminal_session.contains(PtyId(1)));
        assert!(!app.terminal_session.contains(PtyId(2)));
        assert_eq!(app.displayed_terminal(), Some(PtyId(1)));
    }
}

#[test]
fn debug_tabs_remain_clickable_beside_a_right_docked_terminal() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("source"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(20, 24, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.sync_terminal_panel();
    app.process_effect(AppEffect::WindowDock {
        target: "terminal".into(),
        edge: "right".into(),
        size: 20,
    });
    app.ui.debug_panes = Some(crate::ui::DebugPanesUiState {
        selected: crate::ui::DebugPane::Sessions,
        target: String::new(),
        status: String::new(),
        threads: Vec::new(),
        stack: Vec::new(),
        breakpoints: Vec::new(),
        variables: Vec::new(),
        watches: Vec::new(),
        console: Vec::new(),
    });
    app.window_session.set_debugging(true);
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let top = crate::ui::workspace_top_rows(&app.state, &app.ui, app.configuration.settings());
    let region = WindowRect {
        y: bounds.y + top,
        height: bounds.height - top,
        ..bounds
    };
    let (column, row) = (region.y..region.y + region.height)
        .find_map(|row| {
            (region.x..region.x + region.width)
                .find(|column| {
                    crate::ui::debug_pane_tab_at(
                        region,
                        crate::ui::DebugPane::Sessions,
                        *column,
                        row,
                    ) == Some(crate::ui::DebugPane::Threads)
                })
                .map(|column| (column, row))
        })
        .expect("visible Threads tab");
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            Default::default(),
        ),
        column,
        row,
    });
    assert_eq!(
        app.ui.debug_panes.as_ref().unwrap().selected,
        crate::ui::DebugPane::Threads
    );
}

#[test]
fn the_terminal_opens_where_its_placement_setting_says() {
    use crate::app::tests::driver::TestSession;
    for (placement, expected) in [
        (crate::config::TerminalPlacement::Float, "float"),
        (crate::config::TerminalPlacement::Fullscreen, "fullscreen"),
    ] {
        let mut loaded = crate::config::load_bundled_config();
        loaded.config.ui.terminal_panel.placement = placement;
        let mut driver = TestSession::new(loaded);
        driver.new_buffer();
        driver.trust_owned_terminal_fixture();
        driver
            .application_mut()
            .process_effect(crate::app::commands::AppEffect::TerminalToggle);
        let deadline = Instant::now() + Duration::from_secs(6);
        let id = loop {
            if let Some(id) = driver.application_mut().displayed_terminal() {
                break id;
            }
            driver.pump_terminal();
            assert!(Instant::now() < deadline, "{expected} terminal opened");
            thread::sleep(Duration::from_millis(2));
        };
        let shown = driver
            .application_mut()
            .window_session
            .native_ids()
            .terminal_placement(id.0);
        assert!(
            match placement {
                crate::config::TerminalPlacement::Float =>
                    matches!(shown, Some(WindowPlacement::Floating(_))),
                _ => shown == Some(WindowPlacement::Fullscreen),
            },
            "{expected}: {shown:?}"
        );
    }
}

#[test]
fn showing_a_terminal_with_a_hidden_semantic_default_uses_a_visible_fallback() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.apply_persisted_window_rules(&[PersistedWindowRule {
        content_type: "builtin.terminal".into(),
        placement: PersistedWindowPlacement::Hidden,
    }])
    .unwrap();
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
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    app.show_graph_window("terminal").unwrap();
    assert!(matches!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            ..
        }
    ));
    app.reconcile_editor_windows();
    assert_ne!(
        app.window_session
            .graph()
            .window(terminal.window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
}

#[test]
fn default_terminal_float_handles_dimensions_larger_than_u16_multiplication() {
    let (_app_dirs, mut app) = app();
    // Even the largest dimensions must not overflow before the missing-terminal check.
    app.window_session.observe_size((u16::MAX, u16::MAX));
    app.process_effect(AppEffect::TerminalFloat(None));
    assert_eq!(
        app.ui.notification.as_deref(),
        Some(app.message("terminal_missing", &[]).as_str())
    );

    app.window_session.observe_size((80, 24));
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    app.handle_event(UiEvent::Resize(20_000, 24));
    app.process_effect(AppEffect::TerminalFloat(None));
    let ids = app.window_session.native_ids().terminal(1).unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(ids.window)
            .unwrap()
            .placement,
        WindowPlacement::Floating(WindowRect {
            x: 2_000,
            y: 2,
            width: 16_000,
            height: 19
        })
    );
}

#[test]
fn terminal_panel_focus_cycle_resize_and_close_preserve_independent_sessions() {
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.ui.terminal_panel.height = 10;
    let workspace = tempdir().unwrap();
    let state = tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: workspace.path().to_owned(),
        },
        loaded,
        false,
        state.path().to_owned(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((80, 24));
    let mut first_screen = PtyScreen::new(80, 8, 100);
    first_screen.feed_bytes(b"first terminal");
    let mut second_screen = PtyScreen::new(80, 8, 100);
    second_screen.feed_bytes(b"second terminal");
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "first".into(),
            screen: first_screen,
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.fixture(
        PtyId(2),
        crate::app::terminal::TerminalFixture {
            profile: "second".into(),
            screen: second_screen,
            running: false,
            exit_code: Some(7),
            truncated: true,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    let original_key = app.terminal_session.key(PtyId(1)).unwrap();
    let source = app.window_session.terminal_geometry_source(1).unwrap();
    assert!(
        app.terminal_session
            .resize(PtyId(1), u16::MAX, u16::MAX, source, &app.workspace)
            .is_err()
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 8));
    let mut foreign = crate::app::terminal::Terminals::new(&app.configuration.settings().terminals);
    foreign.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "foreign".into(),
            screen: PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    assert!(
        foreign.resolve(&original_key).is_none(),
        "equal numeric ID does not identify another owner"
    );
    app.terminal_session.select(PtyId(2));
    assert_eq!(app.keyboard_terminal(), Some(PtyId(1)));
    app.enter_terminal_normal_mode();
    assert_eq!(
        app.terminal_session.mode(PtyId(1)),
        Some(crate::ui::TerminalPanelMode::Normal)
    );
    assert_eq!(
        app.terminal_session.mode(PtyId(2)),
        Some(crate::ui::TerminalPanelMode::Input)
    );
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    app.enter_terminal_input_mode();
    app.terminal_session.select(PtyId(1));
    app.sync_terminal_panel();
    assert_eq!(app.workspace_render_height(), 13);
    assert_eq!(app.active_input_context(), "terminal");
    assert!(
        app.ui
            .content_tabs
            .iter()
            .all(|entry| entry.kind != ContentTabKind::Terminal),
        "a docked terminal is a separate window, not an editor content tab"
    );

    app.handle_configured_key("ctrl-k".parse::<KeyEvent>().expect("Ctrl-k"));
    assert!(
        !app.window_session.terminal_focused(),
        "Ctrl-k returns to the editor above"
    );
    app.handle_configured_key("ctrl-j".parse::<KeyEvent>().expect("Ctrl-j"));
    assert!(
        app.window_session.terminal_focused(),
        "Ctrl-j returns to the terminal below"
    );

    app.process_effect(crate::app::commands::AppEffect::TerminalUnfocus);
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(!app.window_session.terminal_focused());
    assert!(app.ui.terminal_panel.is_some());
    assert_eq!(app.active_input_context(), "editor.normal");
    app.process_effect(crate::app::commands::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    app.process_effect(crate::app::commands::AppEffect::TerminalToggle);
    assert!(!app.window_session.terminal_focused());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::app::commands::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    assert!(app.terminal_window_rect().is_some());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    app.process_effect(crate::app::commands::AppEffect::TerminalUnfocus);

    app.process_effect(crate::app::commands::AppEffect::TerminalFocus(2));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.window_session.terminal_focused());
    app.process_effect(crate::app::commands::AppEffect::TerminalResize(5));
    assert_eq!(app.window_session.terminal_height(), 20);
    assert_eq!(
        app.ui
            .terminal_panel
            .as_ref()
            .expect("second panel")
            .profile,
        "second"
    );
    assert_eq!(
        app.ui
            .terminal_panel
            .as_ref()
            .expect("terminal tabs")
            .tabs
            .len(),
        2
    );
    app.handle_configured_key("escape".parse::<KeyEvent>().expect("first Escape"));
    assert_eq!(app.active_input_context(), "terminal");
    app.handle_configured_key("escape".parse::<KeyEvent>().expect("second Escape"));
    assert_eq!(app.active_input_context(), "terminal_normal");
    app.handle_configured_key("v".parse::<KeyEvent>().expect("visual"));
    assert_eq!(app.active_input_context(), "terminal_visual");
    app.handle_configured_key("h".parse::<KeyEvent>().expect("move selection"));
    app.handle_configured_key("y".parse::<KeyEvent>().expect("yank selection"));
    assert_eq!(app.active_input_context(), "terminal_normal");
    assert!(
        app.state
            .registers
            .get('"')
            .is_some_and(|value| !value.text.is_empty())
    );
    app.handle_configured_key("i".parse::<KeyEvent>().expect("terminal input"));
    assert_eq!(app.active_input_context(), "terminal");
    app.process_effect(crate::app::commands::AppEffect::TerminalCycle(-1));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_session.fixture_lines(PtyId(2))[0].contains("second"));

    app.process_effect(crate::app::commands::AppEffect::TerminalResize(1));
    assert_eq!(app.window_session.terminal_height(), 12);
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 11));
    let mut resize =
        crate::editor::CommandInvocation::new("window.resize.directional", InvocationSource::Test)
            .unwrap()
            .with_argument("direction", CommandValue::String("up".into()));
    resize.count = std::num::NonZeroU64::new(3);
    app.invoke(resize.clone());
    assert_eq!(app.window_session.terminal_height(), 18);
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 17));
    resize
        .arguments
        .insert("direction".into(), CommandValue::String("down".into()));
    app.invoke(resize);
    assert_eq!(app.window_session.terminal_height(), 12);
    assert_eq!(app.workspace_render_height(), 11);
    app.handle_event(crate::ui::UiEvent::Resize(60, 20));
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 11));
    assert_eq!(app.workspace_render_height(), 7);

    let terminal_ids = app
        .window_session
        .native_ids()
        .terminal(1)
        .expect("terminal resources");
    app.process_effect(crate::app::commands::AppEffect::TerminalFloat(Some(
        crate::windows::WindowRect {
            x: 5,
            y: 3,
            width: 40,
            height: 10,
        },
    )));
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("floating")
            .placement,
        WindowPlacement::Floating(WindowRect {
            x: 5,
            y: 3,
            width: 40,
            height: 10,
        })
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (40, 9));
    app.process_effect(crate::app::commands::AppEffect::WindowMove {
        target: "terminal".into(),
        x: 7,
        y: 4,
    });
    app.process_effect(crate::app::commands::AppEffect::WindowResize {
        target: "terminal".into(),
        amount: 2,
    });
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("moved")
            .placement,
        WindowPlacement::Floating(WindowRect {
            x: 7,
            y: 4,
            width: 42,
            height: 12,
        })
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (42, 11));
    app.process_effect(crate::app::commands::AppEffect::TerminalFullscreen);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("fullscreen")
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 19));
    app.process_effect(crate::app::commands::AppEffect::TerminalHide);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert!(app.terminal_session.contains(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::app::commands::AppEffect::WindowShow(
        "terminal".into(),
    ));
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("shown")
            .placement,
        WindowPlacement::Fullscreen
    );
    app.process_effect(crate::app::commands::AppEffect::TerminalDock {
        edge: "top".into(),
        size: 5,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 4));
    app.process_effect(crate::app::commands::AppEffect::TerminalDock {
        edge: "right".into(),
        size: 10,
    });
    // The column left of a right dock is its border.
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (9, 18));
    app.process_effect(crate::app::commands::AppEffect::TerminalDock {
        edge: "left".into(),
        size: 8,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (8, 18));
    app.process_effect(crate::app::commands::AppEffect::TerminalDock {
        edge: "bottom".into(),
        size: 12,
    });
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 11));
    app.place_graph_window("terminal", WindowPlacement::Tiled)
        .expect("tile live terminal view");
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("tiled terminal")
            .placement,
        WindowPlacement::Tiled
    );
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("tiled terminal survives editor reconciliation")
            .placement,
        WindowPlacement::Tiled
    );
    app.place_graph_window(
        "terminal",
        WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: 12,
        },
    )
    .expect("redock same terminal view");
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    let first_editor_tab = app.state.active_tab().expect("first editor tab");
    let terminal_two_ids = app
        .window_session
        .native_ids()
        .terminal(2)
        .expect("second terminal resources");
    let terminal_state_before_rehost = (
        app.terminal_session.fixture_dimensions(PtyId(1)),
        app.terminal_session.fixture_lines(PtyId(1)),
    );
    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::NewTab,
    ));
    let rehosted_tab = app
        .window_session
        .graph()
        .active_tab()
        .expect("new graph tab");
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session.native_ids().terminal(2),
        Some(terminal_two_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("rehosted terminal window")
            .tab,
        rehosted_tab
    );
    assert_eq!(
        (
            app.terminal_session.fixture_dimensions(PtyId(1)),
            app.terminal_session.fixture_lines(PtyId(1)),
        ),
        terminal_state_before_rehost,
        "cross-tab rehosting preserves the terminal owner state"
    );
    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::FocusTab(-1),
    ));
    assert_eq!(app.state.active_tab(), Some(first_editor_tab));
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_ne!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("terminal rehosted back")
            .tab,
        rehosted_tab
    );
    app.process_effect(crate::app::commands::AppEffect::TerminalFocus(2));
    app.process_effect(crate::app::commands::AppEffect::TerminalFloat(Some(
        crate::windows::WindowRect {
            x: 9,
            y: 4,
            width: 33,
            height: 9,
        },
    )));
    let terminal_rule = app
        .capture_window_rules()
        .into_iter()
        .find(|rule| rule.content_type == "builtin.terminal")
        .expect("selected terminal placement rule");
    assert_eq!(
        terminal_rule.placement,
        PersistedWindowPlacement::Floating {
            x: 9,
            y: 4,
            width: 33,
            height: 9,
        }
    );
    app.process_effect(crate::app::commands::AppEffect::TerminalFocus(1));

    let visible_placement = app
        .window_session
        .graph()
        .window(terminal_ids.window)
        .expect("terminal window")
        .placement;
    app.process_effect(crate::app::commands::AppEffect::TerminalClose);
    let close = app.input.prompt().expect("running terminal close prompt");
    assert_eq!(close.kind, crate::ui::PromptKind::TerminalClose);
    let confirmation = close.confirmation.as_ref().expect("close actions");
    assert_eq!(confirmation.positive_label, "Stop");
    assert_eq!(confirmation.negative_label, "Hide");
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::No);
    app.close_prompt();
    assert!(app.terminal_session.contains(PtyId(1)));
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("cancelled close keeps window")
            .placement,
        visible_placement
    );

    app.process_effect(crate::app::commands::AppEffect::TerminalClose);
    app.submit_prompt();
    assert!(app.terminal_session.contains(PtyId(1)));
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal_ids.window)
            .expect("hidden terminal keeps identity")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::app::commands::AppEffect::TerminalToggle);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );

    app.process_effect(crate::app::commands::AppEffect::TerminalClose);
    assert!(app.terminal_session.select(PtyId(2)));
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(!app.terminal_session.contains(PtyId(1)));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.ui.terminal_panel.is_some());
    app.process_effect(crate::app::commands::AppEffect::TerminalClose);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.terminal_session.len() == 0);
    assert!(
        app.terminal_session.resolve(&original_key).is_none(),
        "closed picker key must not revive its source"
    );
    assert!(app.window_session.native_ids().terminal(1).is_none());
    assert!(app.window_session.native_ids().terminal(2).is_none());
    assert!(
        app.window_session
            .graph()
            .content(terminal_ids.content)
            .is_none()
    );
    assert!(app.terminal_session.selected().is_none());
    assert!(!app.window_session.terminal_focused());
    assert!(app.ui.terminal_panel.is_none());
}

#[cfg(unix)]
#[test]
fn live_pty_clone_refusal_preserves_process_screen_geometry_focus_and_sources() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "clone-refusal".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf x >> starts; printf 'READY\\n'; read value; printf 'ACK:%s\\n' \"$value\"; read end".into(),
            ],
            cwd: None,
            environment: Default::default(),
            columns: 47,
            rows: 9,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("clone-refusal");
    let deadline = Instant::now() + Duration::from_secs(4);
    while !driver
        .projection()
        .terminal_panel
        .as_ref()
        .is_some_and(|panel| {
            panel.cells.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("READY")
            })
        })
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }

    let before = driver.terminal_clone_state();
    driver.clone_terminal();
    assert_eq!(driver.terminal_clone_state(), before);
    assert_eq!(driver.terminal_started_markers(), "x");

    driver.terminal_send("still-original\n");
    while !driver
        .projection()
        .terminal_panel
        .as_ref()
        .is_some_and(|panel| {
            panel.cells.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("ACK:still-original")
            })
        })
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.terminal_started_markers(), "x");
    driver.terminal_close();
    while driver.terminal_pending() != 0 {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(unix)]
#[test]
fn ordinary_terminal_profile_names_do_not_make_agent_terminals() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.agents.connection = crate::config::AgentConnection::Terminal;
    let name = loaded.config.agents.default.clone();
    loaded.config.agents.profiles.get_mut(&name).unwrap().cli = vec!["cat".into()];
    let mut shell = loaded.config.terminals.profiles["shell"].clone();
    shell.program = "/bin/cat".into();
    shell.arguments.clear();
    shell.float = false;
    loaded.config.terminals.profiles.insert(name.clone(), shell);
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open(&name);
    let app = driver.application_mut();
    let shell = app.terminal_session.selected().unwrap();
    assert!(!app.agent_terminal(shell));
    assert!(!app.terminal_session.side(shell));
    app.process_effect(AppEffect::TerminalUnfocus);
    app.process_effect(AppEffect::Agent(
        crate::app::agent::commands::AgentCommand::Ask,
    ));
    let agent = app.terminal_session.selected().unwrap();
    assert_ne!(shell, agent, "an ordinary shell is not reused as an agent");
    assert!(app.agent_terminal(agent));
    assert!(app.terminal_session.side(agent));
    app.process_effect(AppEffect::TerminalUnfocus);
    app.process_effect(AppEffect::Agent(
        crate::app::agent::commands::AgentCommand::Ask,
    ));
    assert_eq!(app.terminal_session.selected(), Some(agent));
    assert_eq!(app.terminal_session.descriptions().len(), 2);
}

#[cfg(unix)]
#[test]
fn a_terminal_profile_named_like_an_agent_is_not_reused_as_that_agent() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.agents.connection = crate::config::AgentConnection::Terminal;
    let name = loaded.config.agents.default.clone();
    loaded.config.agents.profiles.get_mut(&name).unwrap().cli =
        vec!["sh".into(), "-c".into(), "echo AGENT_READY; cat".into()];
    loaded.config.terminals.profiles.insert(
        name.clone(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "echo SHELL_READY; cat".into()],
            cwd: None,
            environment: Default::default(),
            columns: 80,
            rows: 24,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open(&name);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("SHELL_READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let shell = driver.application_mut().displayed_terminal().unwrap();
    assert!(!driver.application_mut().agent_terminal(shell));
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("AGENT_READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let app = driver.application_mut();
    let agent = app.keyboard_terminal().unwrap();
    assert_ne!(shell, agent);
    assert!(app.agent_terminal(agent));
    assert_eq!(app.terminal_session.descriptions().len(), 2);
}

#[cfg(unix)]
#[test]
fn a_terminal_agent_accepts_selections_from_normal_mode_and_preserves_rejected_ones() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.agents.connection = crate::config::AgentConnection::Terminal;
    let name = loaded.config.agents.default.clone();
    loaded.config.agents.profiles.get_mut(&name).unwrap().cli =
        vec!["sh".into(), "-c".into(), "echo AGENT_READY; cat".into()];
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("AGENT_READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let app = driver.application_mut();
    let agent = app.keyboard_terminal().unwrap();
    app.terminal_session.enter_terminal_normal_mode(agent);
    app.process_effect(crate::app::commands::AppEffect::TerminalUnfocus);
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("SELECTION_SENT"), None)
        .unwrap();
    app.reconcile_editor_windows();
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(0),
        head: crate::editor::text::ByteOffset(13),
        kind: crate::editor::SelectionKind::Character,
    });
    app.process_effect(AppEffect::Agent(AgentCommand::Ask));
    assert!(app.state.views[view].selection.is_none());
    assert_eq!(
        app.terminal_session.mode(agent),
        Some(crate::ui::TerminalPanelMode::Input)
    );
    assert_eq!(app.terminal_session.descriptions().len(), 1);
    while !driver
        .application_mut()
        .terminal_session
        .fixture_lines(agent)
        .iter()
        .any(|line| line.contains("SELECTION_SENT"))
    {
        driver.pump_terminal();
        assert!(
            Instant::now() < deadline,
            "{:?}",
            driver
                .application_mut()
                .terminal_session
                .fixture_lines(agent)
        );
        thread::sleep(Duration::from_millis(2));
    }
    let app = driver.application_mut();
    app.process_effect(crate::app::commands::AppEffect::TerminalUnfocus);
    let text = "x".repeat(crate::app::terminal::TERMINAL_MAXIMUM_INPUT_BYTES + 1);
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(&text), None)
        .unwrap();
    app.reconcile_editor_windows();
    let selection = crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(0),
        head: crate::editor::text::ByteOffset(text.len() - 1),
        kind: crate::editor::SelectionKind::Character,
    };
    app.state.views[view].selection = Some(selection);
    app.process_effect(AppEffect::Agent(AgentCommand::Ask));
    assert!(app.state.views[view].selection.is_some());
    assert_eq!(app.terminal_session.descriptions().len(), 1);
}

#[cfg(unix)]
#[test]
fn a_terminal_agent_opens_on_the_right_where_the_agent_panel_goes() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let agents = &mut loaded.config.agents;
    agents.connection = crate::config::AgentConnection::Terminal;
    let mut profile = agents.profiles[&agents.default].clone();
    // The profile's environment reaches the program, as PI_HARDWARE_CURSOR reaches pi.
    profile.cli = vec!["sh".into(), "-c".into(), "echo READY$SHOWN; cat".into()];
    profile.environment.insert("SHOWN".into(), "-shown".into());
    agents.profiles.insert("echoer".into(), profile);
    agents.default = "echoer".into();
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    let app = driver.application_mut();
    app.process_effect(AppEffect::Agent(AgentCommand::Ask));
    let deadline = Instant::now() + Duration::from_secs(4);
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("READY-shown"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let app = driver.application_mut();
    let width = app.window_session.width();
    let rect = app.terminal_window_rect().expect("agent terminal shown");
    assert_eq!(rect.x + rect.width, width, "docked on the right: {rect:?}");
    assert!(rect.x > 0 && rect.width >= 40.min(width), "{rect:?}");
    assert!(
        rect.height > app.window_session.height() / 2,
        "full height: {rect:?}"
    );
    // A border separates it from the code; the program gets the columns right of it.
    assert_eq!(
        driver.terminal_dimensions().map(|(columns, _)| columns),
        Some(rect.width - 1)
    );
    let frame = driver.frame();
    for y in rect.y + 1..rect.y + rect.height {
        assert_eq!(
            frame.grid.get(rect.x, y).map(|cell| cell.symbol.as_str()),
            Some("│")
        );
    }
    let app = driver.application_mut();
    app.process_effect(AppEffect::Agent(AgentCommand::Toggle));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(AppEffect::Agent(AgentCommand::Toggle));
    assert_eq!(app.terminal_window_rect(), Some(rect));
    // From the code beside it, the toggle hides it too.
    app.process_effect(crate::app::commands::AppEffect::TerminalUnfocus);
    app.process_effect(AppEffect::Agent(AgentCommand::Toggle));
    assert!(app.terminal_window_rect().is_none());
    assert_eq!(app.terminal_session.descriptions().len(), 1);
}

#[cfg(unix)]
#[test]
fn ctrl_slash_beside_a_terminal_agent_opens_a_shell_and_both_keep_their_windows_and_sizes() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "shell".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "echo SHELL; cat".into()],
            cwd: None,
            environment: Default::default(),
            columns: 80,
            rows: 10,
            float: false,
            close_on_exit: false,
        },
    );
    let agents = &mut loaded.config.agents;
    agents.connection = crate::config::AgentConnection::Terminal;
    let mut profile = agents.profiles[&agents.default].clone();
    profile.cli = vec!["sh".into(), "-c".into(), "echo READY; cat".into()];
    agents.profiles.insert("echoer".into(), profile);
    agents.default = "echoer".into();
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    let deadline = Instant::now() + Duration::from_secs(6);
    let wait = |driver: &mut TestSession, done: &dyn Fn(&mut TestSession) -> bool| {
        while !done(driver) {
            driver.pump_terminal();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    };
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    wait(&mut driver, &|driver| {
        driver
            .terminal_lines()
            .iter()
            .any(|line| line.contains("READY"))
    });
    let agent = driver
        .application_mut()
        .displayed_terminal()
        .expect("agent");

    // ctrl-/ from the agent opens a shell instead of hiding the agent.
    driver.key("ctrl-/");
    wait(&mut driver, &|driver| {
        driver
            .application_mut()
            .terminal_session
            .descriptions()
            .len()
            == 2
            && driver
                .terminal_lines()
                .iter()
                .any(|line| line.contains("SHELL"))
    });
    let app = driver.application_mut();
    let shell = app.displayed_terminal().expect("shell");
    assert_ne!(shell, agent);
    assert_eq!(app.window_session.focused_terminal(), Some(shell.0));
    let width = app.window_session.width();
    let agent_rect = app
        .terminal_rect(agent)
        .expect("the agent keeps its window");
    let shell_rect = app.terminal_rect(shell).expect("the shell has a window");
    assert_eq!(agent_rect.x + agent_rect.width, width, "{agent_rect:?}");
    assert!(
        shell_rect.x + shell_rect.width <= agent_rect.x
            || shell_rect.y >= agent_rect.y + agent_rect.height,
        "{shell_rect:?} overlaps {agent_rect:?}"
    );
    // Each program has its own window's size, the agent's less its border.
    wait(&mut driver, &|driver| {
        let app = driver.application_mut();
        app.terminal_session.fixture_dimensions(agent).0 == usize::from(agent_rect.width - 1)
            && app.terminal_session.fixture_dimensions(shell).0 == usize::from(shell_rect.width)
    });
    // Both draw, and the agent's tab row names only itself.
    let frame = driver.frame();
    let row = |rect: WindowRect, y: u16| {
        (rect.x..rect.x + rect.width)
            .filter_map(|x| frame.grid.get(x, y).map(|cell| cell.symbol.to_string()))
            .collect::<String>()
    };
    let agent_text = (agent_rect.y..agent_rect.y + agent_rect.height)
        .map(|y| row(agent_rect, y))
        .collect::<Vec<_>>();
    let shell_text = (shell_rect.y..shell_rect.y + shell_rect.height)
        .map(|y| row(shell_rect, y))
        .collect::<Vec<_>>();
    assert!(
        agent_text.iter().any(|line| line.contains("READY")),
        "{agent_text:#?}"
    );
    assert!(
        shell_text.iter().any(|line| line.contains("SHELL")),
        "{shell_text:#?}"
    );
    assert!(!agent_text[0].contains("shell"), "{agent_text:#?}");
    assert!(!shell_text[0].contains("echoer"), "{shell_text:#?}");

    // ctrl-/ in the shell hides it and leaves the agent; again brings it back.
    driver.key("ctrl-/");
    let app = driver.application_mut();
    assert!(app.terminal_rect(shell).is_none());
    assert!(app.terminal_rect(agent).is_some());
    driver.key("ctrl-/");
    let app = driver.application_mut();
    assert_eq!(app.terminal_rect(shell), Some(shell_rect));
    assert_eq!(app.window_session.focused_terminal(), Some(shell.0));

    // The agent widens and narrows in columns, and its program follows.
    app.process_effect(crate::app::commands::AppEffect::TerminalFocus(agent.0));
    assert_eq!(app.window_session.focused_terminal(), Some(agent.0));
    assert!(app.terminal_rect(shell).is_some(), "the shell stays below");
    app.resize_focused_window(crate::windows::SpatialDirection::Left, 1)
        .expect("resize");
    let wider = app.terminal_rect(agent).expect("agent");
    assert_eq!(
        wider.width,
        agent_rect.width + crate::ui::render::TERMINAL_PANEL_RESIZE_STEP
    );
    wait(&mut driver, &|driver| {
        driver
            .application_mut()
            .terminal_session
            .fixture_dimensions(agent)
            .0
            == usize::from(wider.width - 1)
    });
    // terminal.resize, given rows, moves a side terminal in columns too.
    let app = driver.application_mut();
    app.process_effect(crate::app::commands::AppEffect::TerminalResize(-1));
    assert_eq!(
        app.terminal_rect(agent).map(|rect| rect.width),
        Some(agent_rect.width)
    );
}

#[cfg(unix)]
#[test]
fn resizing_a_terminal_that_is_stopping_is_not_an_error() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "stopping".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "echo READY; sleep 30".into()],
            cwd: None,
            environment: Default::default(),
            columns: 47,
            rows: 9,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("stopping");
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
    assert_eq!(driver.stop_terminal_then_resize(), Ok(()));
}

#[cfg(unix)]
#[test]
fn real_pty_geometry_ack_focus_placement_and_cross_tab_keep_one_process() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "geometry".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf x >> starts; printf '\\033[?1049h\\033[?1h\\033[?2004h\\033[?1002h\\033[?1006hREADY\\n'; while read value; do printf 'ACK:%s\\n' \"$value\"; done".into(),
            ],
            cwd: None,
            environment: Default::default(),
            columns: 47,
            rows: 9,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("geometry");
    let deadline = Instant::now() + Duration::from_secs(4);
    let contains = |driver: &TestSession, needle: &str| {
        driver
            .terminal_lines()
            .iter()
            .any(|line| line.contains(needle))
    };
    while !contains(&driver, "READY") {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let initial_identity = driver.terminal_window_identity().unwrap();
    assert_eq!(driver.terminal_started_markers(), "x");
    assert_eq!(
        (
            driver.selected_terminal(),
            driver.displayed_terminal(),
            driver.keyboard_terminal(),
        ),
        (Some(1), Some(1), Some(1))
    );
    driver.key("escape");
    driver.key("escape");
    assert_eq!(
        driver.terminal_mode(),
        Some(crate::ui::TerminalPanelMode::Normal)
    );
    let screen_state = driver.terminal_screen_state().unwrap();
    assert!(screen_state.alternate);
    assert!(screen_state.cursor_visible);
    assert!(screen_state.application_cursor);
    assert!(screen_state.bracketed_paste);
    assert_eq!(screen_state.mouse_mode, crate::ui::PtyMouseMode::Drag);
    assert!(screen_state.sgr_mouse);
    let cursor = driver.terminal_cursor();

    let wait_for_ack = |driver: &mut TestSession| loop {
        driver.pump_terminal();
        let requested = driver
            .terminal_dimensions()
            .map(|(columns, rows)| (usize::from(columns), usize::from(rows)));
        if driver.terminal_screen_dimensions() == requested
            && driver.terminal_pending_resizes() == Some(0)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    };

    let before_rehost = driver.terminal_screen_dimensions().unwrap();
    driver
        .place_terminal(WindowPlacement::Floating(WindowRect {
            x: 5,
            y: 3,
            width: 40,
            height: 10,
        }))
        .unwrap();
    let previous_source = driver.terminal_geometry_source().unwrap();
    assert_eq!(driver.terminal_pending_resizes(), Some(1));
    assert_eq!(driver.terminal_screen_dimensions(), Some(before_rehost));
    driver.reset_terminal_window_runtime().unwrap();
    let identity = driver.terminal_window_identity().unwrap();
    let original_tab = driver.terminal_window_tab().unwrap();
    assert_ne!(identity, initial_identity);
    let current_source = driver.terminal_geometry_source().unwrap();
    assert_ne!(current_source, previous_source);
    assert_eq!(driver.terminal_pending_resizes(), Some(2));
    assert!(
        driver.terminal_pending_resize_matches(&current_source),
        "same-size rehost keeps the earlier physical resize ahead of the current one"
    );
    assert_eq!(driver.terminal_screen_dimensions(), Some(before_rehost));
    assert_eq!(driver.terminal_started_markers(), "x");
    wait_for_ack(&mut driver);
    assert_eq!(driver.terminal_screen_state(), Some(screen_state));
    assert_eq!(driver.terminal_cursor(), cursor);

    for placement in [
        WindowPlacement::Tiled,
        WindowPlacement::Floating(WindowRect {
            x: 3,
            y: 2,
            width: 50,
            height: 10,
        }),
        WindowPlacement::Fullscreen,
        WindowPlacement::Docked {
            edge: DockEdge::Top,
            size: 5,
        },
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 10,
        },
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 8,
        },
        WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: 12,
        },
    ] {
        let before = driver.terminal_screen_dimensions().unwrap();
        driver.place_terminal(placement).unwrap();
        assert_eq!(driver.terminal_window_identity(), Some(identity));
        assert_eq!(driver.terminal_started_markers(), "x");
        if driver
            .terminal_dimensions()
            .is_some_and(|size| size != (before.0 as u16, before.1 as u16))
        {
            assert_eq!(
                driver.terminal_screen_dimensions(),
                Some(before),
                "screen dimensions changed before the exact PTY ACK"
            );
        }
        wait_for_ack(&mut driver);
        assert_eq!(driver.terminal_screen_state(), Some(screen_state));
        assert_eq!(
            driver.terminal_mode(),
            Some(crate::ui::TerminalPanelMode::Normal)
        );
        assert_eq!(driver.terminal_cursor(), cursor);
        assert!(contains(&driver, "READY"));
    }

    let frame_cursor = driver.frame().cursor;
    assert!(frame_cursor.is_some());
    driver.resize(0, 0);
    let zero = driver.frame();
    assert_eq!((zero.grid.width(), zero.grid.height()), (0, 0));
    assert_eq!(zero.cursor, None);
    assert_eq!(driver.terminal_window_identity(), Some(identity));
    assert_eq!(driver.terminal_screen_state(), Some(screen_state));
    assert_eq!(driver.terminal_started_markers(), "x");
    driver.resize(80, 24);
    assert_eq!(driver.frame().cursor, frame_cursor);
    assert_eq!(driver.terminal_screen_state(), Some(screen_state));

    let acknowledged = driver.terminal_screen_dimensions();
    driver.place_terminal(WindowPlacement::Hidden).unwrap();
    assert_eq!(
        (
            driver.selected_terminal(),
            driver.displayed_terminal(),
            driver.keyboard_terminal(),
        ),
        (Some(1), Some(1), None)
    );
    assert_eq!(driver.context(), "editor.normal");
    assert_ne!(driver.focused_window(), Some(identity.2));
    assert_eq!(
        driver.terminal_mode(),
        Some(crate::ui::TerminalPanelMode::Normal)
    );
    assert_eq!(driver.terminal_cursor(), cursor);
    assert_eq!(driver.terminal_screen_dimensions(), acknowledged);
    assert_eq!(driver.terminal_started_markers(), "x");
    driver.key("i");
    driver.paste("hidden-must-not-route");
    driver.key("enter");
    driver.key("escape");
    driver.show_terminal().unwrap();
    assert_eq!(
        (
            driver.selected_terminal(),
            driver.displayed_terminal(),
            driver.keyboard_terminal(),
        ),
        (Some(1), Some(1), None)
    );
    assert_eq!(driver.terminal_screen_state(), Some(screen_state));
    driver.focus_only_terminal();
    assert_eq!(driver.focused_window(), Some(identity.2));
    driver.key("i");
    driver.paste("visible-route");
    driver.key("enter");
    while !contains(&driver, "ACK:visible-route") {
        driver.pump_terminal();
        assert!(
            Instant::now() < deadline,
            "terminal output: {:?}",
            driver.terminal_lines()
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!contains(&driver, "ACK:hidden-must-not-route"));
    driver.key("escape");
    driver.key("escape");
    assert_eq!(
        driver.terminal_mode(),
        Some(crate::ui::TerminalPanelMode::Normal)
    );

    driver.new_tab();
    assert_eq!(driver.terminal_window_identity(), Some(identity));
    assert_ne!(driver.terminal_window_tab(), Some(original_tab));
    assert_eq!(driver.terminal_started_markers(), "x");
    assert_eq!(
        driver.terminal_mode(),
        Some(crate::ui::TerminalPanelMode::Normal)
    );
    assert_eq!(
        (
            driver.selected_terminal(),
            driver.displayed_terminal(),
            driver.keyboard_terminal(),
        ),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(driver.context(), "terminal_normal");
    assert_eq!(driver.focused_window(), Some(identity.2));
    driver.key("i");
    driver.paste("cross-tab-route");
    driver.key("enter");
    while !contains(&driver, "ACK:cross-tab-route") {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.terminal_started_markers(), "x");
    driver.terminal_close();
    while driver.terminal_pending() != 0 {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(unix)]
#[test]
fn terminal_preflight_close_and_capture_keep_original_pty_and_geometry() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert("owned-terminal".into(),crate::config::TerminalProfileConfig {program:"/bin/sh".into(),arguments:vec!["-c".into(),"printf x >> starts; printf 'READY\n'; read value; printf 'ACK:%s\n' \"$value\"; read end".into()],cwd:None,environment:Default::default(),columns:80,rows:8,float:false,close_on_exit:false});
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_request("owned-terminal");
    driver.terminal_close();
    assert!(
        driver.projection().terminal_panel.is_none(),
        "close before the terminal starts cancels the intent"
    );
    assert_eq!(driver.terminal_started_markers(), "");
    driver.terminal_open("owned-terminal");
    let first = driver
        .projection()
        .terminal_panel
        .as_ref()
        .expect("preflight tab")
        .id;
    assert_eq!(
        driver.projection().terminal_panel.as_ref().unwrap().tabs[0].phase,
        crate::ui::TerminalPhase::Preflight
    );
    driver.terminal_close();
    driver.terminal_open("owned-terminal");
    let second = driver
        .projection()
        .terminal_panel
        .as_ref()
        .expect("replacement preflight")
        .id;
    assert!(second > first);
    driver.terminal_unfocus();
    driver.open_command_prompt();
    let deadline = Instant::now() + Duration::from_secs(4);
    let contains = |driver: &TestSession, needle: &str| {
        driver
            .projection()
            .terminal_panel
            .as_ref()
            .is_some_and(|panel| {
                panel.cells.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains(needle)
                })
            })
    };
    while !contains(&driver, "READY") {
        driver.pump_terminal();
        assert!(
            Instant::now() < deadline,
            "{:?}",
            driver.projection().notification
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.terminal_started_markers(),
        "x",
        "closed preflight must not spawn its child"
    );
    assert_eq!(
        driver.projection().terminal_panel.as_ref().unwrap().id,
        second
    );
    assert_eq!(driver.context(), "command_prompt");
    assert_eq!(driver.keyboard_terminal(), None);
    driver.close_prompt();
    assert_eq!(
        driver.keyboard_terminal(),
        None,
        "background readiness cannot seize focus"
    );
    let acknowledged = driver.terminal_screen_dimensions().unwrap();
    driver.resize(63, 27);
    assert_ne!(
        Some((acknowledged.0 as u16, acknowledged.1 as u16)),
        driver.terminal_dimensions(),
        "fixture must request a changed PTY size"
    );
    assert_eq!(
        driver.terminal_screen_dimensions(),
        Some(acknowledged),
        "the virtual screen must letterbox until the PTY resize ACK is drained"
    );
    loop {
        driver.pump_terminal();
        let panel = driver.projection().terminal_panel.as_ref().unwrap();
        if Some((panel.columns, panel.rows)) == driver.terminal_dimensions() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    driver.terminal_send("hello\n");
    while !contains(&driver, "ACK:hello") {
        driver.pump_terminal();
        assert!(
            Instant::now() < deadline,
            "{:?}",
            driver.projection().notification
        );
        thread::sleep(Duration::from_millis(2));
    }
    driver.terminal_close();
    assert_eq!(
        driver
            .projection()
            .terminal_panel
            .as_ref()
            .expect("stopping terminal remains projected")
            .tabs[0]
            .phase,
        crate::ui::TerminalPhase::Stopping
    );
    while driver.terminal_pending() != 0 {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(driver.projection().terminal_panel.is_none());
    assert_eq!(driver.keyboard_terminal(), None);
    // A second open starts a fresh terminal.
    driver.terminal_request("owned-terminal");
    assert!(driver.projection().terminal_panel.is_some());
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.terminal_started_markers() != "xx" {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    driver.focus_only_terminal();
    driver.terminal_close();
    while driver.terminal_pending() != 0 {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(unix)]
#[test]
fn debugger_terminal_displays_without_focus_and_its_exit_keeps_another_terminal_focused() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let mut program = loaded.config.terminals.profiles["shell"].clone();
    program.program = "/bin/sh".into();
    program.arguments = vec![
        "-c".into(),
        "printf ready > debug-ready; while [ ! -f debug-exit ]; do sleep .01; done".into(),
    ];
    program.float = false;
    loaded
        .config
        .terminals
        .profiles
        .insert("debug-output".into(), program);
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("shell");
    let app = driver.application_mut();
    let shell = app.terminal_session.selected().unwrap();
    app.process_effect(AppEffect::TerminalUnfocus);
    let (_service, adapter) = crate::debugger::test_session();
    let (_request, reply) = adapter.terminal_reply();
    app.request_terminal_open("debug-output", Some(reply));
    let debug = app.terminal_session.selected().unwrap();
    assert_ne!(debug, shell);
    assert_eq!(app.terminal_session.displayed(), Some(debug));
    assert!(!app.window_session.terminal_focused());
    let root = app.state.workspace_root.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("debug-ready").exists() {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    driver.application_mut().reveal_terminal(shell);
    std::fs::write(root.join("debug-exit"), "exit").unwrap();
    loop {
        driver.pump_terminal();
        let app = driver.application_mut();
        if app
            .terminal_session
            .descriptions()
            .iter()
            .any(|(id, _, state)| {
                *id == debug.0 && *state == crate::windows::ContentLifecycle::Exited
            })
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.application_mut().window_session.focused_terminal(),
        Some(shell.0)
    );
}

#[cfg(unix)]
#[test]
fn cancelled_debugger_terminal_preflight_never_launches_the_program() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let mut program = loaded.config.terminals.profiles["shell"].clone();
    program.program = "/bin/sh".into();
    program.arguments = vec!["-c".into(), "printf started > closed-debug-started".into()];
    loaded
        .config
        .terminals
        .profiles
        .insert("closed-debug".into(), program);
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    let (_service, adapter) = crate::debugger::test_session();
    let (_request, reply) = adapter.terminal_reply();
    let app = driver.application_mut();
    app.request_terminal_open("closed-debug", Some(reply));
    let debug = app.terminal_session.selected().unwrap();
    assert_eq!(app.terminal_session.preflight_count(), 1);
    let root = app.state.workspace_root.clone();
    let _ = app.debugger_session.stop();
    assert!(!app.debugger_session.terminal_current(&debug));
    let deadline = Instant::now() + Duration::from_secs(5);
    while driver.application_mut().terminal_session.preflight_count() != 0 {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!driver.application_mut().terminal_session.contains(debug));
    assert!(!root.join("closed-debug-started").exists());
}

#[cfg(unix)]
#[test]
fn real_pty_reports_exit_io_failure_and_shared_deadline_unknown_distinctly() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "nonzero".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "printf 'FAILED\\n'; exit 7".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded.clone());
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("nonzero");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        driver.pump_terminal();
        if driver.terminal_pending() == 0
            && driver.terminal_phase() == Some(crate::ui::TerminalPhase::Failed)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.terminal_phase(),
        Some(crate::ui::TerminalPhase::Failed)
    );
    assert_eq!(driver.terminal_exit_code(), Some(7));
    assert!(
        !driver
            .projection()
            .terminal_panel
            .as_ref()
            .unwrap()
            .truncated
    );
    assert!(
        driver
            .notification()
            .is_some_and(|message| message.contains("status Some(7)"))
    );

    loaded.config.terminals.profiles.insert(
        "stale-close".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "read -r release; exit 7".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut stale = TestSession::new(loaded.clone());
    stale.new_buffer();
    stale.trust_owned_terminal_fixture();
    stale.terminal_open("stale-close");
    let deadline = Instant::now() + Duration::from_secs(4);
    while stale.terminal_phase() != Some(crate::ui::TerminalPhase::Running) {
        stale.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    stale.terminal_begin_close();
    stale.terminal_send("release\n");
    while stale.terminal_phase() != Some(crate::ui::TerminalPhase::Failed) {
        stale.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    stale.terminal_submit_close();
    assert_eq!(
        stale.terminal_phase(),
        Some(crate::ui::TerminalPhase::Failed)
    );
    assert!(
        stale
            .notification()
            .is_some_and(|message| message.contains("close source lifecycle changed"))
    );

    loaded.config.terminals.profiles.insert(
        "unknown".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "while :; do :; done".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut unknown = TestSession::new(loaded);
    unknown.new_buffer();
    unknown.trust_owned_terminal_fixture();
    unknown.terminal_open("unknown");
    let deadline = Instant::now() + Duration::from_secs(4);
    while unknown.terminal_phase() != Some(crate::ui::TerminalPhase::Running) {
        unknown.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    unknown.terminal_hide_and_show();
    assert_eq!(
        unknown.terminal_phase(),
        Some(crate::ui::TerminalPhase::Running),
        "hiding and showing a live terminal must not stop or restart its PTY"
    );
    let notices = unknown.expire_shutdown_with_terminal();
    assert!(notices.iter().any(|message| {
        message.contains("outcome is Unknown")
            && message.contains("shared shutdown deadline expired")
    }));
}

#[cfg(unix)]
#[test]
fn successful_terminal_exit_keeps_output_unless_profile_closes_on_exit() {
    use crate::app::tests::driver::TestSession;
    for (profile, close_on_exit) in [
        ("completed", false),
        ("completed", true),
        ("debug", false),
        ("debug", true),
    ] {
        let mut loaded = crate::config::load_bundled_config();
        loaded.config.terminals.profiles.insert(
            profile.into(),
            crate::config::TerminalProfileConfig {
                program: "/bin/sh".into(),
                arguments: vec!["-c".into(), "printf 'finished-output'".into()],
                cwd: None,
                environment: Default::default(),
                columns: 40,
                rows: 8,
                float: false,
                close_on_exit,
            },
        );
        let mut driver = TestSession::new(loaded);
        driver.new_buffer();
        driver.trust_owned_terminal_fixture();
        driver.terminal_open(profile);
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            driver.pump_terminal();
            if driver.terminal_pending() == 0
                && if close_on_exit {
                    driver.projection().terminal_panel.is_none()
                } else {
                    driver.terminal_exit_code() == Some(0)
                }
            {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        if close_on_exit {
            assert!(driver.projection().terminal_panel.is_none());
        } else {
            assert_eq!(
                driver.terminal_phase(),
                Some(crate::ui::TerminalPhase::Exited)
            );
            assert!(
                driver
                    .terminal_lines()
                    .iter()
                    .any(|line| line.contains("finished-output"))
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn oversized_terminal_paste_reports_its_rejection() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "paste".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "printf ready; read line".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("paste");
    let deadline = Instant::now() + Duration::from_secs(4);
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("ready"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline, "{:?}", driver.notification());
        thread::sleep(Duration::from_millis(2));
    }
    driver.paste(&"x".repeat(2 * 1024 * 1024));
    assert_eq!(
        driver.notification(),
        Some("terminal paste exceeds the input limit")
    );
    driver.terminal_close();
}

#[test]
fn configured_terminal_navigation_bindings_run_before_pty_encoding() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
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
    let mut loaded = crate::config::load_bundled_config().keymaps;
    let entry = loaded
        .entries
        .iter_mut()
        .find(|entry| entry.id == "builtin.terminal.surface.terminal-send.up")
        .unwrap();
    entry.command = Some("terminal.mode.normal".into());
    entry.arguments.clear();
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.handle_configured_key("up".parse().unwrap());
    assert_eq!(
        app.terminal_session.mode(crate::process::PtyId(1)),
        Some(crate::ui::TerminalPanelMode::Normal)
    );
}

#[test]
fn hiding_another_terminal_preserves_the_focused_terminal() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((100, 30));
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for id in [PtyId(1), PtyId(2)] {
        app.terminal_session.fixture(
            id,
            crate::app::terminal::TerminalFixture {
                profile: "shell".into(),
                screen: PtyScreen::new(40, 8, 100),
                running: true,
                exit_code: None,
                truncated: false,
            },
            app.workspace.version(),
        );
        app.terminal_session.set_side(id);
    }
    app.terminal_session.select(PtyId(1));
    app.reconcile_editor_windows();
    for id in [1, 2] {
        app.apply_native_window_placement(
            crate::app::windows::NativeSurfaceTarget::Terminal(id),
            WindowPlacement::Docked {
                edge: DockEdge::Bottom,
                size: 8,
            },
        )
        .unwrap();
    }
    app.set_terminal_focus(true);
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.focused_terminal(), Some(1));
    app.apply_native_window_placement(
        crate::app::windows::NativeSurfaceTarget::Terminal(2),
        WindowPlacement::Hidden,
    )
    .unwrap();
    assert_eq!(app.window_session.focused_terminal(), Some(1));
    assert_eq!(app.keyboard_terminal(), Some(PtyId(1)));
}

#[cfg(unix)]
#[test]
fn real_pty_reports_nonzero_exit_and_shared_deadline_unknown_distinctly() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert(
        "nonzero".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "printf 'FAILED\\n'; exit 7".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut driver = TestSession::new(loaded.clone());
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_open("nonzero");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        driver.pump_terminal();
        if driver.terminal_pending() == 0
            && driver.terminal_phase() == Some(crate::ui::TerminalPhase::Failed)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.terminal_phase(),
        Some(crate::ui::TerminalPhase::Failed)
    );
    assert_eq!(driver.terminal_exit_code(), Some(7));
    assert!(
        !driver
            .projection()
            .terminal_panel
            .as_ref()
            .unwrap()
            .truncated
    );
    assert!(
        driver
            .notification()
            .is_some_and(|message| message.contains("status Some(7)"))
    );

    loaded.config.terminals.profiles.insert(
        "stale-close".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "read -r release; exit 7".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut stale = TestSession::new(loaded.clone());
    stale.new_buffer();
    stale.trust_owned_terminal_fixture();
    stale.terminal_open("stale-close");
    let deadline = Instant::now() + Duration::from_secs(4);
    while stale.terminal_phase() != Some(crate::ui::TerminalPhase::Running) {
        stale.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    stale.terminal_begin_close();
    stale.terminal_send("release\n");
    while stale.terminal_phase() != Some(crate::ui::TerminalPhase::Failed) {
        stale.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    stale.terminal_submit_close();
    assert_eq!(
        stale.terminal_phase(),
        Some(crate::ui::TerminalPhase::Failed)
    );
    assert!(
        stale
            .notification()
            .is_some_and(|message| message.contains("close source lifecycle changed"))
    );

    loaded.config.terminals.profiles.insert(
        "unknown".into(),
        crate::config::TerminalProfileConfig {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "while :; do :; done".into()],
            cwd: None,
            environment: Default::default(),
            columns: 40,
            rows: 8,
            float: false,
            close_on_exit: false,
        },
    );
    let mut unknown = TestSession::new(loaded);
    unknown.new_buffer();
    unknown.trust_owned_terminal_fixture();
    unknown.terminal_open("unknown");
    let deadline = Instant::now() + Duration::from_secs(4);
    while unknown.terminal_phase() != Some(crate::ui::TerminalPhase::Running) {
        unknown.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    unknown.terminal_hide_and_show();
    assert_eq!(
        unknown.terminal_phase(),
        Some(crate::ui::TerminalPhase::Running),
        "hiding and showing a live terminal must not stop or restart its PTY"
    );
    let notices = unknown.expire_shutdown_with_terminal();
    assert!(notices.iter().any(|message| {
        message.contains("outcome is Unknown")
            && message.contains("shared shutdown deadline expired")
    }));
}

use crate::app::terminal::*;

pub(in crate::app) fn fixture() -> (tempfile::TempDir, Workspace, Terminals) {
    let root = tempfile::tempdir().unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut session = Terminals::new(&TerminalsConfig::default());
    session.fixture(
        PtyId(1),
        TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(10, 4, 100),
            running: false,
            exit_code: None,
            truncated: false,
        },
        workspace.version(),
    );
    session.fixture_cwd(PtyId(1), root.path().to_path_buf());
    (root, workspace, session)
}

#[cfg(unix)]
fn launch_shell(root: &std::path::Path, workspace: &Workspace, session: &mut Terminals) {
    let mut config = TerminalsConfig::default().profiles["shell"].clone();
    config.arguments = vec!["-c".into(), "read value; exit 0".into()];
    session.tabs.get_mut(&PtyId(1)).unwrap().pending = Some(Open {
        request: ExecutionPreflightRequest {
            executable: "/bin/sh".into(),
            cwd: root.into(),
            roots: vec![root.into()],
            root_revision: workspace.root_revision(),
        },
        arguments: config.arguments.iter().map(Into::into).collect(),
        config,
    });
    let ready = ExecutionReady::fixture(
        workspace,
        crate::process::ExecutionPreflightResult {
            id: crate::process::PreflightId(1),
            executable: "/bin/sh".into(),
            cwd: root.into(),
            root: root.into(),
            root_revision: workspace.root_revision(),
        },
    );
    session
        .launch(session.key(PtyId(1)).unwrap(), ready, workspace)
        .unwrap();
}

#[test]
fn relative_terminal_programs_use_the_requested_working_directory() {
    let (root, workspace, mut session) = fixture();
    let editor = crate::editor::EditorState::new(root.path().into());
    let windows = crate::app::windows::Windows::new(false, 8);
    let mut config = TerminalsConfig::default().profiles["shell"].clone();
    config.program = "./app".into();
    config.cwd = Some("build".into());
    session
        .open_program(
            "debug",
            config,
            &workspace,
            windows.navigation_stamp(editor.window_version(), None),
            Arc::new(()),
            false,
        )
        .unwrap();
    let request = &session.intent.as_ref().unwrap().open.request;
    assert_eq!(request.cwd, root.path().join("build"));
    assert_eq!(request.executable, root.path().join("build/./app"));
}

#[cfg(unix)]
#[test]
fn terminal_lifecycle_revisions_include_launch_and_exit_and_starting_counts_as_running() {
    let (root, workspace, mut session) = fixture();
    let before = session.revision();
    launch_shell(root.path(), &workspace, &mut session);
    assert!(session.revision() > before);
    assert_eq!(session.descriptions()[0].2, ContentLifecycle::Starting);
    assert_eq!(session.running_in_root(root.path()), 1);
    assert!(!session.tabs[&PtyId(1)].running);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.tabs[&PtyId(1)].running {
        session.drain(&workspace, |_| None);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let running_revision = session.revision();
    let pty = session.tabs[&PtyId(1)].pty.unwrap();
    session
        .service
        .input(pty, b"done\n".to_vec(), TERMINAL_MAXIMUM_INPUT_BYTES)
        .unwrap();
    while !session.tabs[&PtyId(1)].io_complete {
        session.drain(&workspace, |_| None);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(session.revision() > running_revision);
    assert_eq!(session.descriptions()[0].2, ContentLifecycle::Exited);
    assert_eq!(session.running_in_root(root.path()), 0);
}

#[cfg(unix)]
#[test]
fn real_pty_stream_failure_reports_incomplete_output_instead_of_exit_status_or_unknown() {
    let (root, workspace, mut session) = fixture();
    let pty = session
        .service
        .open(PtyRequest {
            program: "/bin/sh".into(),
            argv0: None,
            arguments: vec!["-c".into(), "printf 123456789".into()],
            cwd: root.path().into(),
            environment: BTreeMap::new(),
            columns: 10,
            rows: 4,
            root_revision: workspace.root_revision(),
            policy: PtyPolicy {
                maximum_output_bytes: 4,
                ..PtyPolicy::default()
            },
        })
        .unwrap();
    session.tabs.get_mut(&PtyId(1)).unwrap().pty = Some(pty);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut updates = Vec::new();
    while !session.tabs[&PtyId(1)].io_complete {
        updates.extend(session.drain(&workspace, |_| None));
        assert!(std::time::Instant::now() < deadline, "{updates:?}");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let message = "PTY output limit exceeded; stream is incomplete";
    assert!(updates.contains(&TerminalUpdate::IoComplete(PtyId(1))));
    assert!(updates.contains(&TerminalUpdate::Failed(PtyId(1), message.into())));
    let tab = &session.tabs[&PtyId(1)];
    assert_eq!(tab.failure.as_deref(), Some(message));
    assert!(tab.exited);
    assert!(tab.unknown.is_none());
    let panel = session.projection(PtyId(1), true, 4).unwrap();
    assert!(panel.truncated);
    assert_eq!(panel.tabs[0].phase, crate::ui::TerminalPhase::Failed);
    assert_eq!(session.descriptions()[0].2, ContentLifecycle::Failed);
}

#[cfg(unix)]
#[test]
fn terminal_replies_wait_for_input_capacity_and_retry_failed_submissions() {
    let (root, workspace, mut session) = fixture();
    launch_shell(root.path(), &workspace, &mut session);
    let pty = session.tabs[&PtyId(1)].pty.unwrap();
    let reservations = (0..TERMINAL_QUEUE_CAPACITY)
        .map(|_| session.service.reserve_input(pty).unwrap())
        .collect::<Vec<_>>();
    let tab = session.tabs.get_mut(&PtyId(1)).unwrap();
    tab.feed_bytes(b"\x1b[6n");
    let replies = tab.pending_replies.clone();
    assert_eq!(replies, b"\x1b[1;1R");
    tab.flush_replies(&session.service);
    assert_eq!(tab.pending_replies, replies);
    drop(reservations);
    tab.flush_replies(&session.service);
    assert!(tab.pending_replies.is_empty());
    tab.feed_bytes(b"\x1b[6n");
    session.service.close(pty).unwrap();
    tab.flush_replies(&session.service);
    assert_eq!(tab.pending_replies, replies);
}

#[test]
fn one_column_terminal_geometry_settles_at_the_screen_minimum() {
    let (_root, workspace, mut session) = fixture();
    let source = TerminalGeometrySource::fixture(1);
    session
        .resize(PtyId(1), 1, 4, source.clone(), &workspace)
        .unwrap();
    assert_eq!(session.tabs[&PtyId(1)].requested_size, (2, 4));
    assert_eq!(session.tabs[&PtyId(1)].screen.dimensions(), (2, 4));
    session.resize(PtyId(1), 1, 4, source, &workspace).unwrap();
    assert!(session.tabs[&PtyId(1)].pending_resizes.is_empty());
}

#[test]
fn terminal_resize_keeps_visual_text_through_wrapping() {
    let (_root, workspace, mut session) = fixture();
    let tab = session.tabs.get_mut(&PtyId(1)).unwrap();
    tab.screen.feed_bytes(b"0123456789abc\r\n$ ");
    tab.mode = TerminalPanelMode::Visual;
    tab.normal_cursor = Some((0, 8));
    tab.visual_anchor = Some((0, 7));
    let source = TerminalGeometrySource::fixture(1);
    session
        .resize(PtyId(1), 5, 6, source.clone(), &workspace)
        .unwrap();
    assert_eq!(session.tabs[&PtyId(1)].normal_cursor, Some((1, 3)));
    assert_eq!(session.tabs[&PtyId(1)].visual_anchor, Some((1, 2)));
    assert_eq!(
        session.yank_terminal_text(PtyId(1), false),
        Some(("78".into(), false))
    );
    session.start_terminal_visual(PtyId(1), false);
    session
        .move_terminal_normal_cursor(PtyId(1), "left")
        .unwrap();
    session.resize(PtyId(1), 20, 4, source, &workspace).unwrap();
    assert_eq!(session.tabs[&PtyId(1)].normal_cursor, Some((0, 7)));
    assert_eq!(session.tabs[&PtyId(1)].visual_anchor, Some((0, 8)));
    assert_eq!(
        session.yank_terminal_text(PtyId(1), false),
        Some(("78".into(), false))
    );
}

#[test]
fn terminal_placeholders_expand_only_original_tokens() {
    let mut config = TerminalsConfig::default().profiles["shell"].clone();
    config.arguments = vec!["--file={file}:{line}".into(), "{root}/{dir}".into()];
    config.cwd = Some("{dir}".into());
    let (expanded, arguments) = expand_placeholders(
        &config,
        std::path::Path::new("/root/{line}"),
        Some((std::path::Path::new("/root/{line}/a{line}.rs"), 7)),
    )
    .unwrap();
    assert_eq!(arguments[0], "--file=/root/{line}/a{line}.rs:7");
    assert_eq!(arguments[1], "/root/{line}//root/{line}");
    assert_eq!(expanded.cwd.unwrap(), std::path::Path::new("/root/{line}"));
    config.arguments = vec!["{root}".into()];
    config.cwd = None;
    assert!(expand_placeholders(&config, std::path::Path::new("/root/{file}"), None).is_ok());
}

#[cfg(unix)]
#[test]
fn terminal_placeholders_preserve_native_path_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let root = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/root/\xff".to_vec()));
    let file = root.join(std::ffi::OsString::from_vec(b"a\xfe{line}.rs".to_vec()));
    let mut config = TerminalsConfig::default().profiles["shell"].clone();
    config.arguments = vec!["{root}".into(), "prefix:{file}".into(), "{dir}".into()];
    config.cwd = Some(root.join("{line}"));
    let (expanded, arguments) = expand_placeholders(&config, &root, Some((&file, 9))).unwrap();
    assert_eq!(arguments[0], root.as_os_str());
    let mut expected = std::ffi::OsString::from("prefix:");
    expected.push(&file);
    assert_eq!(arguments[1], expected);
    assert_eq!(arguments[2], root.as_os_str());
    assert_eq!(expanded.cwd.unwrap(), root.join("9"));
}

#[test]
fn terminal_normal_motions_and_yanks_skip_wide_continuations() {
    let root = tempfile::tempdir().unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "test".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("state"), 1, false, false, 1);
    let mut session = Terminals::new(&TerminalsConfig::default());
    let mut screen = PtyScreen::new(10, 2, 4);
    screen.feed_bytes("中X".as_bytes());
    let id = PtyId(1);
    session.fixture(
        id,
        TerminalFixture {
            profile: "shell".into(),
            screen,
            running: false,
            exit_code: Some(0),
            truncated: false,
        },
        workspace.version(),
    );
    session.enter_terminal_normal_mode(id);
    session
        .move_terminal_normal_cursor(id, "line_start")
        .unwrap();
    session.move_terminal_normal_cursor(id, "right").unwrap();
    assert_eq!(session.tabs[&id].normal_cursor, Some((0, 2)));
    session.start_terminal_visual(id, false);
    assert_eq!(
        session.yank_terminal_text(id, false),
        Some(("X".into(), false))
    );
    session.move_terminal_normal_cursor(id, "left").unwrap();
    assert_eq!(session.tabs[&id].normal_cursor, Some((0, 0)));
    session.start_terminal_visual(id, false);
    assert_eq!(
        session.yank_terminal_text(id, false),
        Some(("中".into(), false))
    );
}

#[test]
fn disabled_focus_reports_do_not_require_a_live_terminal() {
    let root = tempfile::tempdir().unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "test".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("state"), 1, false, false, 1);
    let mut session = Terminals::new(&TerminalsConfig::default());
    let id = PtyId(1);
    session.fixture(
        id,
        TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(10, 2, 4),
            running: false,
            exit_code: Some(0),
            truncated: false,
        },
        workspace.version(),
    );
    for focused in [true, false] {
        assert!(
            !session
                .send(id, TerminalInput::Focus(focused), &workspace)
                .unwrap()
        );
    }
    session
        .tabs
        .get_mut(&id)
        .unwrap()
        .screen
        .feed_bytes(b"\x1b[?1004h");
    assert_eq!(
        session.send(id, TerminalInput::Focus(true), &workspace),
        Err("terminal has not started".into())
    );
    assert!(
        session
            .send(id, TerminalInput::Text("x"), &workspace)
            .is_err()
    );
}

#[test]
fn workspace_changes_keep_running_terminals_and_cancel_unlaunched_tabs() {
    let root = tempfile::tempdir().unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = crate::config::load_bundled_config().config.terminals;
    let mut terminals = Terminals::new(&policy);
    for (id, running) in [(PtyId(1), true), (PtyId(2), false)] {
        terminals.fixture(
            id,
            TerminalFixture {
                profile: "shell".into(),
                screen: PtyScreen::new(80, 24, 100),
                running,
                exit_code: None,
                truncated: false,
            },
            workspace.version(),
        );
    }
    terminals.tabs.get_mut(&PtyId(2)).unwrap().pending = Some(Open {
        config: policy.profiles["shell"].clone(),
        arguments: Vec::new(),
        request: ExecutionPreflightRequest {
            executable: "sh".into(),
            cwd: root.path().into(),
            roots: vec![root.path().into()],
            root_revision: workspace.root_revision(),
        },
    });
    terminals.retain_workspace(&workspace);
    assert!(terminals.contains(PtyId(1)));
    assert!(!terminals.contains(PtyId(2)));
}

#[test]
fn resize_acknowledgements_are_ordered_exact_and_one_shot() {
    let terminal = PtyId(2);
    let pty = PtyId(12);
    let source = TerminalGeometrySource::fixture(42);
    let replacement = TerminalGeometrySource::fixture(43);
    let pending_resize = |source: &TerminalGeometrySource, sequence, columns, rows| PendingResize {
        terminal,
        pty,
        source: source.clone(),
        sequence,
        columns,
        rows,
    };
    let reply = |sequence, columns, rows| ResizeReply {
        terminal,
        pty,
        sequence,
        columns,
        rows,
    };
    let mut pending = VecDeque::from([
        pending_resize(&source, 7, 40, 10),
        pending_resize(&source, 8, 60, 20),
    ]);
    let mut screen = PtyScreen::new(20, 5, 20);
    screen.feed_bytes(b"0123456789abcdefghijAB");
    let mut cursor = Some((1, 1));
    let mut anchor = Some((0, 18));
    let mut offset = 0;
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(8, 60, 20),
        Some(&source),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        ResizeReply {
            terminal: PtyId(3),
            ..reply(7, 40, 10)
        },
        Some(&source),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert_eq!(pending.len(), 2);
    assert!(acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(7, 40, 10),
        Some(&source),
        &mut cursor,
        &mut anchor,
        &mut offset,
    ));
    assert_eq!(screen.dimensions(), (40, 10));
    assert_eq!(cursor, Some((0, 21)));
    assert_eq!(anchor, Some((0, 18)));
    assert_eq!(pending.len(), 1, "intermediate ACK was applied");
    screen.feed_bytes(b"\x1b[?1049h\x1b[1;999HX");
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(7, 40, 10),
        Some(&source),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert!(acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(8, 60, 20),
        Some(&source),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert_eq!(screen.dimensions(), (60, 20));
    assert_eq!(screen.history_row(0).unwrap()[39].text, "X");
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(8, 60, 20),
        Some(&source),
        &mut None,
        &mut None,
        &mut 0,
    ));

    pending.push_back(pending_resize(&source, 9, 40, 10));
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(9, 40, 10),
        Some(&replacement),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert!(pending.is_empty(), "stale window reply was closed");
    assert_eq!(screen.dimensions(), (40, 10));
    pending.push_back(pending_resize(&replacement, 10, 40, 10));
    assert!(!acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(9, 40, 10),
        Some(&replacement),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert!(acknowledge_resize(
        &mut pending,
        &mut screen,
        reply(10, 40, 10),
        Some(&replacement),
        &mut None,
        &mut None,
        &mut 0,
    ));
    assert!(pending.is_empty());
    assert_eq!(screen.dimensions(), (40, 10));
}
