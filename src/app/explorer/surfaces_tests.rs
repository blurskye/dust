use crate::app::tests::*;




#[test]
fn directional_resize_uses_the_focused_explorer_clone() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let primary = app.window_session.native_ids().explorer().unwrap().window;
    app.clone_graph_window("explorer", crate::editor::Axis::Horizontal)
        .unwrap();
    let clone = app.window_session.graph().focused().unwrap();
    assert_ne!(primary, clone);
    app.place_graph_window(
        &format!("window:{}", clone.0),
        WindowPlacement::Floating(WindowRect {
            x: 3,
            y: 3,
            width: 20,
            height: 8,
        }),
    )
    .unwrap();
    let primary_placement = app
        .window_session
        .graph()
        .window(primary)
        .unwrap()
        .placement;
    app.process_effect(AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Right,
        1,
    ));
    assert_eq!(
        app.window_session
            .graph()
            .window(primary)
            .unwrap()
            .placement,
        primary_placement
    );
    assert!(
        matches!(app.window_session.graph().window(clone).unwrap().placement, WindowPlacement::Floating(rect) if rect.width > 20)
    );
    app.process_effect(AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Left,
        1_000_000,
    ));
    assert!(
        matches!(app.window_session.graph().window(clone).unwrap().placement, WindowPlacement::Floating(rect) if rect.width == 1)
    );
}


#[test]
fn dashboard_exposes_and_dispatches_the_file_explorer_without_picker_fallback() {
    let (_app_dirs, mut app) = app();
    app.project_configuration();
    assert!(app.ui.dashboard);
    assert!(
        app.ui
            .dashboard_actions
            .iter()
            .any(|action| { action.label == "Explorer" && action.key.as_deref() == Some("e") }),
        "resolved dashboard actions: {:?}",
        app.ui.dashboard_actions
    );
    for key in ["space", "f", "e"] {
        app.handle_configured_key(key.parse::<KeyEvent>().expect("dashboard explorer key"));
    }
    assert!(app.ui.explorer.is_some());
    assert!(app.ui.explorer_focused);
    assert!(app.ui.picker.is_none());
}


#[test]
fn clicking_explorer_cancels_flash_without_swallowing_the_next_key() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc abc abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    app.set_explorer_focus(false);
    app.reconcile_editor_windows();
    app.begin_flash();
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('a'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert!(app.flash.is_some());
    let rect = app.explorer_window_rect().unwrap();
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            Default::default(),
        ),
        column: rect.x + 1,
        row: rect.y + 1,
    });
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('j'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert!(app.flash.is_none());
    assert!(app.ui.flash.is_none());
    assert!(app.window_session.explorer_focused());
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
}


#[test]
fn explorer_is_a_graph_dock_and_bundled_disable_is_transactional() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let ids = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer resources");
    assert!(matches!(
        app.window_session
            .graph()
            .window(ids.window)
            .expect("explorer window")
            .placement,
        WindowPlacement::Docked {
            edge: crate::windows::DockEdge::Left,
            ..
        }
    ));
    let rect = app.explorer_window_rect().expect("projected explorer");
    // The status line keeps the last row of the screen.
    assert!(rect.width > 1 && rect.height == app.window_session.height() - 1);
}


#[test]
fn the_explorer_moves_to_the_side_its_setting_names_while_open() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let left = app.explorer_window_rect().expect("projected explorer");
    assert_eq!(left.x, 0);
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.ui.explorer.side = crate::config::PanelSide::Right;
    app.configuration.install_fixture(fixture);
    app.reconcile_editor_windows();
    let right = app.explorer_window_rect().expect("projected explorer");
    assert_eq!(
        right.x.saturating_add(right.width),
        app.window_session.width()
    );
    assert_eq!(right.width, left.width);
}


#[test]
fn swapping_a_configured_explorer_into_hidden_retains_its_actual_placement() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let placement = WindowPlacement::Docked {
        edge: DockEdge::Right,
        size: 31,
    };
    let candidate = app
        .prepare_configured_window_rules(&crate::config::WindowsConfig {
            rules: vec![crate::config::WindowRuleConfig {
                content_type: "builtin.explorer".into(),
                placement: crate::config::WindowPlacementConfig::Docked {
                    edge: "right".into(),
                    size: 31,
                },
            }],
        })
        .unwrap();
    app.window_session
        .accept_current(candidate, &app.state.window_version())
        .unwrap();
    app.open_explorer();
    let explorer = app.window_session.native_ids().explorer().unwrap();
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
    app.place_graph_window("terminal", WindowPlacement::Hidden)
        .unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        placement
    );
    app.swap_graph_windows("explorer", "terminal").unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    app.show_graph_window("explorer").unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        placement
    );
}


#[test]
fn hiding_the_terminal_goes_back_to_the_file_not_the_explorer() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_event(crate::ui::UiEvent::Resize(100, 30));
    app.process_effect(AppEffect::ExplorerOpen);
    app.set_explorer_focus(false);
    app.reconcile_editor_windows();
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
    app.process_effect(AppEffect::TerminalToggle);
    app.reconcile_editor_windows();
    for _ in 0..2 {
        assert_eq!(app.active_input_context(), "editor.normal");
        app.process_effect(AppEffect::TerminalToggle);
        assert!(
            app.window_session.terminal_focused(),
            "{:?}",
            app.ui.notification
        );
        app.process_effect(AppEffect::TerminalToggle);
        app.reconcile_editor_windows();
        assert!(!app.window_session.terminal_focused());
        assert!(
            !app.window_session.explorer_focused(),
            "the file had the keyboard before"
        );
    }
    // From the explorer, it goes back to the explorer.
    app.set_explorer_focus(true);
    app.process_effect(AppEffect::TerminalToggle);
    app.process_effect(AppEffect::TerminalToggle);
    app.reconcile_editor_windows();
    assert!(app.window_session.explorer_focused());
}


#[test]
fn picker_mouse_input_does_not_scroll_the_background_explorer() {
    let (_app_dirs, mut app) = app();
    for name in ["a", "b", "c", "d", "e"] {
        fs::write(app.state.workspace_root.join(name), name).unwrap();
    }
    app.process_effect(AppEffect::ExplorerOpen);
    wait_vfs(&mut app);
    app.reconcile_editor_windows();
    let explorer = app.explorer_window_rect().unwrap();
    let before = app.ui.explorer.clone();
    app.process_effect(AppEffect::OpenPicker(crate::app::PickerKind::Buffers));
    app.handle_event(UiEvent::Mouse {
        key: "mouse-scroll-down".parse().unwrap(),
        column: explorer.x,
        row: explorer.y + 1,
    });
    assert_eq!(app.ui.explorer, before);
}