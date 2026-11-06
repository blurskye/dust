use crate::app::tests::*;
use crate::app::AgentCommand;
use crate::editor::EditorOperation;
use crate::editor::EditorState;
use crate::editor::EditorTransaction;
use crate::editor::TransactionId;
use crate::ui::render::overlays::CUSTOM_ICON;
use crate::ui::render::overlays::SCRATCH_ICON;
use crate::ui::render::overlays::VIRTUAL_ICON;

#[test]
fn placement_swap_moves_editing_focus_to_the_visible_editor() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("oldest"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let (_, first_view) = app
        .state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    let first_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let second_view = app.state.active_view.unwrap();
    let second_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    let first = app
        .window_session
        .editor_ids()
        .window_for(first_pane)
        .unwrap();
    let second = app
        .window_session
        .editor_ids()
        .window_for(second_pane)
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
    assert_eq!(app.state.active_view, Some(first_view));
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", first.0),
        second: format!("window:{}", second.0),
    });
    assert_eq!(app.state.active_view, Some(second_view));
    assert_eq!(
        app.state.tabs[app.state.active_tab.unwrap()].focused,
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
    let first_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let second_view = app.state.active_view.unwrap();
    let second_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    let first = app
        .window_session
        .editor_ids()
        .window_for(first_pane)
        .unwrap();
    let second = app
        .window_session
        .editor_ids()
        .window_for(second_pane)
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
    assert_eq!(app.state.active_view, Some(second_view));
    assert_eq!(
        app.state.tabs[app.state.active_tab.unwrap()].focused,
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
        app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
            crate::editor::Axis::Horizontal,
        )));
        let clone_view = app.state.active_view.unwrap();
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
        assert_eq!(app.state.active_view, Some(floating_view));
        app.window_session.graph().validate().unwrap();
    }
}

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
fn perpendicular_docked_editor_resizes_do_nothing() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let editor = app.window_session.graph().focused().unwrap();
    app.place_graph_window(
        &format!("window:{}", editor.0),
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20,
        },
    )
    .unwrap();
    let graph = app.window_session.graph().clone();
    for key in ["ctrl-up", "ctrl-down"] {
        app.handle_configured_key(key.parse().unwrap());
        assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
        assert_eq!(app.window_session.graph(), &graph);
    }
}

#[test]
fn tiled_window_resize_accepts_the_minimum_delta() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.resize_graph_window("focused", i16::MIN).unwrap();
    assert!(matches!(
        app.state.tabs[app.state.active_tab.unwrap()].root,
        crate::editor::LayoutNode::Split { ratio: 900, .. }
    ));
}

#[test]
fn directional_editor_resize_skips_hidden_splits_and_reaches_the_visible_dock() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let first = app.window_session.graph().focused().unwrap();
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.place_graph_window("focused", WindowPlacement::Hidden)
        .unwrap();
    let tab = app.state.active_tab.unwrap();
    app.handle_configured_key("ctrl-right".parse().unwrap());
    let crate::editor::LayoutNode::Split { ratio, second, .. } = &app.state.tabs[tab].root else {
        panic!("outer split");
    };
    assert!(*ratio < 500);
    assert!(matches!(
        **second,
        crate::editor::LayoutNode::Split { ratio: 500, .. }
    ));
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);

    app.place_graph_window(&format!("window:{}", first.0), WindowPlacement::Hidden)
        .unwrap();
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(20, 20, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    app.place_graph_window(
        "terminal",
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20,
        },
    )
    .unwrap();
    app.process_effect(AppEffect::TerminalUnfocus);
    let root = app.state.tabs[tab].root.clone();
    app.handle_configured_key("ctrl-right".parse().unwrap());
    assert_eq!(app.state.tabs[tab].root, root);
    let terminal = app.window_session.native_ids().terminal(1).unwrap().window;
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal)
            .unwrap()
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20 - crate::ui::render::TERMINAL_PANEL_RESIZE_STEP
        }
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
}

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
fn prompt_mouse_context_is_independently_configurable() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.prompt-mouse-context-is-independently-configurable".into(),
        context: Some("prompt_mouse".into()),
        mode: String::new(),
        keys: Some("mouse-right-down".into()),
        command: Some("command_line.cancel".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.handle_event(crate::ui::UiEvent::Mouse {
        key: crate::editor::input::KeyEvent::plain(crate::editor::input::KeyCode::MouseRightDown),
        column: 1,
        row: 1,
    });
    assert!(app.ui.prompt.is_none());
}

#[test]
fn dashboard_and_picker_are_stable_graph_overlays_with_projected_geometry() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((100, 30));
    app.reconcile_editor_windows();
    let dashboard = app
        .window_session
        .native_ids()
        .dashboard()
        .expect("dashboard resources");
    let dashboard_rect = app
        .compositor_projection()
        .and_then(|projection| projection.window(dashboard.window))
        .expect("dashboard projection")
        .rect;
    assert_eq!(
        dashboard_rect,
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        }
    );

    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.ui.picker = Some(crate::ui::PickerUiState {
        preview_side_scroll: (0, 0),
        footer: None,
        input_hint: String::new(),
        surface: crate::ui::PickerSurfaceKind::Generic,
        title: "Search".into(),
        query: "needle".into(),
        rows: Vec::new(),
        selected: None,
        scroll: 0,
        total: 0,
        loading: false,
        preview: None,
    });
    app.reconcile_editor_windows();
    let picker = app
        .window_session
        .native_ids()
        .picker()
        .expect("picker resources");
    let expected = crate::ui::picker_geometry(WindowRect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    })
    .expect("picker geometry");
    let projection = app.compositor_projection().expect("picker projection");
    assert_eq!(
        projection
            .window(picker.window)
            .expect("picker window")
            .rect,
        expected
    );
    assert_eq!(
        projection.hit_test(expected.x.saturating_add(1), expected.y.saturating_add(1)),
        Some(picker.window)
    );

    app.ui.picker = None;
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.native_ids().dashboard(), Some(dashboard));
    assert_eq!(
        app.window_session
            .native_ids()
            .picker_for(PickerSurfaceKind::Generic),
        Some(picker)
    );
    assert!(
        app.compositor_projection()
            .expect("hidden picker projection")
            .window(picker.window)
            .is_none()
    );

    app.close_active_buffer(false);
    app.reconcile_editor_windows();
    assert!(app.ui.dashboard);
    assert_eq!(app.window_session.native_ids().dashboard(), Some(dashboard));
    assert!(
        app.compositor_projection()
            .expect("dashboard after last buffer")
            .window(dashboard.window)
            .is_some()
    );
}

#[test]
fn notifications_expire_but_remain_available_in_bounded_history() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    app.notify_message("first".into(), false);
    app.update_notification_timer();
    app.notify_message("second".into(), true);
    app.update_notification_timer();
    assert_eq!(app.ui.notification.as_deref(), Some("second"));
    clock.advance(Duration::from_secs(5));
    app.update_notification_timer();
    assert!(app.ui.notification.is_none());
    app.invoke_mapped_command("application.notifications.open");
    let picker = app.ui.picker.as_ref().expect("notification history");
    assert_eq!(picker.title, "Notifications");
    assert_eq!(picker.rows.len(), 2);
    assert_eq!(picker.rows[0].label, "second");
    assert_eq!(picker.rows[0].detail, "error");
    assert_eq!(picker.rows[1].label, "first");

    app.close_picker();
    for index in 0..205 {
        app.notify_message(format!("notice {index}"), false);
    }
    app.invoke_mapped_command("application.notifications.open");
    let picker = app
        .ui
        .picker
        .as_ref()
        .expect("bounded notification history");
    assert_eq!(picker.rows.len(), 200);
    assert_eq!(picker.rows.first().unwrap().label, "notice 204");
    assert_eq!(picker.rows.last().unwrap().label, "notice 5");
}

#[test]
fn log_selection_survives_eviction_of_an_earlier_entry() {
    let (_app_dirs, mut app) = app();
    for line in ["first", "selected", "last"] {
        app.language.append_log(line.into(), 3);
    }
    app.show_lsp_logs();
    app.picker_move(-1);
    let selected = app.ui.picker.as_ref().unwrap().selected.unwrap();
    assert_eq!(
        app.ui.picker.as_ref().unwrap().rows[selected].label,
        "selected"
    );
    app.language.append_log("new".into(), 3);
    app.show_lsp_logs();
    let picker = app.ui.picker.as_ref().unwrap();
    assert_eq!(picker.rows[picker.selected.unwrap()].label, "selected");
}

#[test]
fn breakpoint_condition_prompt_preserves_its_original_target_and_other_settings() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("original.rs");
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("first\nsecond\n"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    let row = crate::debugger::DapSourceBreakpoint {
        path,
        line: 1,
        column: Some(2),
        condition: Some("old".into()),
        hit_condition: Some("3".into()),
        log_message: Some("value={value}".into()),
    };
    app.debugger_session.breakpoint(row.clone(), false).unwrap();
    app.process_effect(AppEffect::DapBreakpointConfigure {
        condition: None,
        hit_condition: None,
        log_message: None,
    });
    assert_eq!(app.input.prompt().unwrap().input, "old");
    // An asynchronous stop can navigate while the condition question is open.
    app.state.views[view].cursor = ByteOffset(6);
    let another = app.state.workspace_root.join("another.rs");
    app.state
        .open_text(TextBuffer::from_text("another"), Some(another))
        .unwrap();
    app.input.prompt_insert(" && ready");
    app.submit_prompt();
    let mut expected = row;
    expected.condition = Some("old && ready".into());
    assert_eq!(app.debugger_session.persisted_breakpoints().len(), 1);
    assert_eq!(
        app.debugger_session
            .breakpoint_at(&expected.path, expected.line),
        Some(&expected)
    );
}

#[test]
fn saved_watches_are_listed_without_a_debugger() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::DapWatchAdd("counter".into()));
    app.process_effect(AppEffect::DapWatchList);
    let picker = app.ui.picker.as_ref().unwrap();
    assert_eq!(picker.rows.len(), 1);
    assert_eq!(picker.rows[0].label, "counter");
    app.accept_picker();
    assert_eq!(app.debugger_session.watches(), ["counter"]);
}

#[test]
fn lsp_log_selection_keeps_the_same_message_when_old_entries_are_evicted() {
    let (_app_dirs, mut app) = app();
    for index in 0..200 {
        app.append_lsp_log(format!("message {index}"));
    }
    app.show_lsp_logs();
    app.search.picker_move(-1, false);
    app.sync_picker_ui();
    let before = app
        .search
        .picker()
        .unwrap()
        .selected_item()
        .unwrap()
        .clone();
    app.append_lsp_log("new message".into());
    let after = app.search.picker().unwrap().selected_item().unwrap();
    assert_eq!(before.id, after.id);
    assert_eq!(before.label, after.label);
}

#[test]
fn live_logs_and_output_refresh_preserve_picker_normal_without_leaking_it_to_new_sources() {
    let (_app_dirs, mut app) = app();
    for logs in [false, true] {
        app.close_picker();
        if logs {
            app.show_lsp_logs();
        } else {
            app.show_output_picker("one");
        }
        app.handle_configured_key("j".parse().unwrap());
        app.handle_configured_key("escape".parse().unwrap());
        for _ in 0..3 {
            if logs {
                app.show_lsp_logs();
            } else {
                app.show_output_picker("one");
            }
            assert_eq!(app.active_input_context(), "picker_normal");
            assert_eq!(app.ui.picker.as_ref().unwrap().query, "j");
            assert!(
                app.ui
                    .picker
                    .as_ref()
                    .unwrap()
                    .input_hint
                    .starts_with("Normal")
            );
        }
        app.show_output_picker("different-source");
        assert_eq!(app.active_input_context(), "picker");
        assert_eq!(app.ui.picker.as_ref().unwrap().query, "");
    }
}

#[test]
fn closed_picker_surfaces_leave_the_content_line_but_keep_their_graph_identity() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for kind in [PickerSurfaceKind::Problems, PickerSurfaceKind::Output] {
        if kind == PickerSurfaceKind::Problems {
            app.open_builtin_picker(PickerKind::Diagnostics);
        } else {
            app.show_output_picker("missing-task");
        }
        app.reconcile_editor_windows();
        let ids = app.window_session.native_ids().picker_for(kind).unwrap();
        assert!(
            app.ui
                .content_tabs
                .iter()
                .any(|entry| entry.identity.0 == ids.content)
        );
        app.close_picker();
        app.reconcile_editor_windows();
        assert!(app.window_session.graph().content(ids.content).is_some());
        assert!(
            !app.ui
                .content_tabs
                .iter()
                .any(|entry| entry.identity.0 == ids.content)
        );
    }
}

#[test]
fn problems_logs_output_and_debug_keep_distinct_graph_surface_identities() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);

    app.open_builtin_picker(PickerKind::Diagnostics);
    app.reconcile_editor_windows();
    let problems = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Problems)
        .expect("problems surface");
    assert_eq!(
        app.window_session
            .graph()
            .content(problems.content)
            .expect("problems content")
            .content_type
            .as_str(),
        "builtin.problems"
    );

    app.show_output_picker("missing-task");
    app.reconcile_editor_windows();
    let output = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Output)
        .expect("output surface");
    assert_ne!(output, problems);
    assert!(
        app.compositor_projection()
            .expect("output projection")
            .window(problems.window)
            .is_none()
    );

    app.process_effect(AppEffect::LspLogs);
    app.reconcile_editor_windows();
    let logs = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Logs)
        .expect("logs surface");
    assert_eq!(
        app.window_session
            .graph()
            .content(logs.content)
            .expect("logs content")
            .content_type
            .as_str(),
        "builtin.logs"
    );
    app.append_lsp_log("fake: live log".into());
    assert!(
        app.ui
            .picker
            .as_ref()
            .is_some_and(|picker| picker.rows.iter().any(|row| row.label == "fake: live log"))
    );
    app.picker_insert("fake");
    app.append_lsp_log("fake: second live log".into());
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.query == "fake"
            && picker
                .rows
                .iter()
                .any(|row| row.label == "fake: second live log")
    }));

    app.open_dap_picker("threads", Vec::new());
    app.reconcile_editor_windows();
    let debug = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Debug)
        .expect("debug surface");
    assert_eq!(
        app.window_session
            .graph()
            .content(debug.content)
            .expect("debug content")
            .content_type
            .as_str(),
        "builtin.debug"
    );
    assert_eq!(
        app.compositor_projection()
            .expect("debug projection")
            .hit_test(50, 15),
        Some(debug.window)
    );

    app.process_effect(AppEffect::WindowHide(format!("window:{}", debug.window.0)));
    assert!(
        app.compositor_projection()
            .expect("hidden debug")
            .window(debug.window)
            .is_none()
    );
    app.process_effect(AppEffect::WindowShow(format!("window:{}", debug.window.0)));
    assert!(
        app.compositor_projection()
            .expect("shown debug")
            .window(debug.window)
            .is_some()
    );
    app.process_effect(AppEffect::WindowClose(format!("window:{}", debug.window.0)));
    assert!(app.window_session.graph().window(debug.window).is_none());
    assert!(app.ui.picker.is_none());

    app.close_picker();
    app.reconcile_editor_windows();
    for ids in [problems, output, logs] {
        assert!(app.window_session.graph().window(ids.window).is_some());
        assert!(
            app.compositor_projection()
                .expect("hidden retained surfaces")
                .window(ids.window)
                .is_none()
        );
    }
    let reconciliation_count = app.window_session.editor_ids().reconciliation_count();
    let idle_graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliation_count
    );
    assert_eq!(app.window_session.graph(), &idle_graph);
}

#[test]
fn protected_prompt_region_blocks_ordinary_compositor_hit_testing() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    app.reconcile_editor_windows();
    let geometry = crate::ui::prompt_geometry(
        app.window_session.width(),
        app.workspace_render_height(),
        false,
    );
    let point = (
        geometry.left.saturating_add(geometry.width / 2),
        geometry.top.saturating_add(geometry.height / 2),
    );
    let projection = app.compositor_projection().expect("protected projection");
    assert_eq!(projection.protected_regions.len(), 1);
    assert_eq!(projection.hit_test(point.0, point.1), None);

    app.close_prompt();
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection()
            .expect("ordinary projection")
            .hit_test(point.0, point.1)
            .is_some()
    );
}

#[test]
fn normalized_mouse_position_invokes_real_cursor_placement() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let view = app.state.active_view.expect("view");
    let workspace = app.workspace_bounds().expect("workspace");
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        workspace.width,
        app.workspace_render_height(),
    )
    .expect("editor viewport");
    let gutter = crate::ui::editor_state_gutter_width(
        &app.state,
        &app.state.buffers[app.state.views[view].buffer].text,
        &app.configuration.settings().editor,
        viewport.width,
    );
    app.input.observe_mouse(
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 2),
        workspace.y.saturating_add(viewport.y),
    );
    app.apply_mouse_cursor(false);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    app.input.observe_mouse(
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 4),
        workspace.y.saturating_add(viewport.y),
    );
    app.apply_mouse_cursor(true);
    let selection = app.state.views[view].selection.expect("mouse selection");
    assert_eq!(selection.anchor, crate::editor::text::ByteOffset(2));
    assert_eq!(selection.head, crate::editor::text::ByteOffset(4));
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
    let first = app.state.active_view.expect("first view");
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    assert_ne!(app.state.active_view, Some(first));
    let flash_view = app.state.active_view.unwrap();
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
    assert_eq!(app.state.active_view, Some(first));
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
fn health_virtual_dogfood_opens_refreshes_and_remains_read_only() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::HealthVirtualOpen);
    let buffer = app.state.active_buffer_id().expect("health buffer");
    assert!(
        app.state.buffers[buffer]
            .text
            .to_string()
            .contains("# Dust health")
    );
    assert!(app.state.buffers[buffer].is_virtual_read_only());
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == VIRTUAL_ICON
                && entry.closeable
                && entry.kind == ContentTabKind::VirtualDocument)
    );
    let virtual_content = app
        .window_session
        .graph()
        .content_for_window(
            app.window_session
                .graph()
                .focused()
                .expect("virtual window"),
        )
        .expect("virtual content");
    assert_eq!(
        virtual_content.adapter,
        crate::ui::window::ContentAdapterKind::VirtualDocument
    );
    assert!(virtual_content.capabilities.persist_descriptor);
    let persisted = app.capture_session().expect("virtual descriptor session");
    assert!(persisted.tabs.is_empty());
    assert_eq!(persisted.content_descriptors.len(), 1);
    let descriptor = &persisted.content_descriptors[0];
    assert_eq!(descriptor.semantic_id, "virtual:0");
    assert_eq!(descriptor.content_type, "builtin.virtual-document");
    assert_eq!(descriptor.adapter, "virtual-document");
    assert_eq!(descriptor.provider.as_deref(), Some("builtin.health"));
    assert_eq!(descriptor.source.as_deref(), Some("builtin.health.virtual"));
    assert_eq!(descriptor.key.as_deref(), Some("health"));
    assert_eq!(
        persisted.unavailable_virtual_descriptors(),
        std::collections::BTreeSet::from(["builtin.health:builtin.health.virtual:health".into()])
    );
    let encoded = serde_json::to_value(descriptor).expect("virtual descriptor JSON");
    for forbidden in [
        "generation",
        "load",
        "revision",
        "callback",
        "cells",
        "text",
    ] {
        assert!(encoded.get(forbidden).is_none(), "serialized {forbidden}");
    }
    app.report_unavailable_persisted_content(&persisted);
    assert!(app.ui.config_diagnostic.as_deref().is_some_and(|message| {
        message.contains("production API 0.2")
            && message.contains("builtin.health:builtin.health.virtual:health")
            && message.contains("continuing without")
    }));
    let mut unavailable_restore = persisted.clone();
    unavailable_restore
        .window_rules
        .push(crate::persistence::PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: crate::persistence::PersistedWindowPlacement::Floating {
                x: 7,
                y: 5,
                width: 40,
                height: 9,
            },
        });
    app.persistence
        .fixture_restore_source(crate::app::persistence::RestoreContext {
            editor: &app.state,
            workspace: &app.workspace,
            input: &app.input,
            windows: &app.window_session,
            configuration: app.configuration.binding_revision(),
        });
    app.restore_session(unavailable_restore);
    assert!(app.terminal_session.descriptions().is_empty());
    assert!(app.window_session.native_ids().terminal(1).is_none());
    assert!(
        app.window_session
            .native_ids()
            .semantic_defaults(None)
            .contains(&(
                "builtin.terminal",
                WindowPlacement::Floating(WindowRect {
                    x: 7,
                    y: 5,
                    width: 40,
                    height: 9,
                })
            ))
    );
    let ids = (
        buffer,
        app.state.active_view,
        app.window_session.graph().focused(),
    );
    app.process_effect(AppEffect::HealthVirtualRefresh);
    assert_eq!(
        (
            app.state.active_buffer_id().unwrap(),
            app.state.active_view,
            app.window_session.graph().focused()
        ),
        ids
    );
    let revision = app.state.buffers[buffer].text.revision();
    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::ApplyEdits {
            buffer,
            edits: crate::editor::text::TextEditSet::new(
                revision,
                vec![crate::editor::text::TextEdit::new(
                    crate::editor::text::ByteRange::new(
                        crate::editor::text::ByteOffset(0),
                        crate::editor::text::ByteOffset(0),
                    )
                    .unwrap(),
                    "x",
                )],
            ),
        });
    assert!(matches!(
        app.state.apply_transaction(&transaction),
        Err(crate::editor::TransactionError::ReadOnly)
    ));
    for _ in 0..256 {
        app.reconcile_editor_windows();
        assert_eq!(app.window_session.graph().focused(), ids.2);
    }
    app.remove_plugin_contribution_generation("builtin.health", PluginLoadId(1));
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(app.state.active_view.is_none());
    assert!(app.window_session.dashboard_visible());
    app.window_session.graph().validate().unwrap();
}

#[test]
fn accepted_virtual_snapshot_publishes_through_the_tiled_editor_bridge() {
    let (_app_dirs, mut app) = app();
    let snapshot = crate::extensions::PluginVirtualDocumentSnapshot {
        plugin: "demo".into(),
        load: PluginLoadId(7),
        source: "plugin.demo.virtual.help".into(),
        document: 9,
        key: "help".into(),
        title: "Help".into(),
        language: "markdown".into(),
        text: "# Héllo\nvirtual".into(),
        revision: 1,
        closed: false,
    };
    let mut draft = app.state.draft();
    let buffer = app
        .extension
        .fixture_virtual_publish(&mut draft, &snapshot)
        .expect("publish");
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(
        app.window_session
            .editor_ids()
            .content_for(buffer)
            .is_some()
    );
    assert!(app.window_session.graph().focused().is_some());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "# Héllo\nvirtual"
    );
    let ids = (app.state.active_view, app.window_session.graph().focused());
    let mut refresh = snapshot.clone();
    refresh.revision = 2;
    refresh.text = "# Héllo\nrefreshed".into();
    let mut draft = app.state.draft();
    assert_eq!(
        app.extension
            .fixture_virtual_publish(&mut draft, &refresh)
            .unwrap(),
        buffer
    );
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.state.buffers[buffer].text.to_string(), refresh.text);
    assert_eq!(
        app.state.buffers[buffer].origin,
        crate::editor::BufferOrigin::VirtualReadOnly {
            owner: refresh.plugin.clone(),
            generation: refresh.load.0,
            document: refresh.document,
            revision: refresh.revision,
        }
    );
    assert_eq!(
        (app.state.active_view, app.window_session.graph().focused()),
        ids
    );
    assert!(matches!(
        app.window_session
            .graph()
            .window(app.window_session.graph().focused().unwrap())
            .unwrap()
            .placement,
        WindowPlacement::Tiled
    ));
}

#[test]
fn universal_window_clone_uses_editor_owned_independent_text_and_virtual_views() {
    let (_app_dirs, mut app) = app();
    let (buffer, original_view) = app
        .state
        .open_text(TextBuffer::from_text("αbc\nline"), None)
        .unwrap();
    {
        let view = &mut app.state.views[original_view];
        view.cursor = crate::editor::text::ByteOffset(2);
        view.selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(0),
            head: crate::editor::text::ByteOffset(2),
            kind: crate::editor::SelectionKind::Character,
        });
        view.scroll_line = 1;
        view.scroll_column = 3;
        view.mode = crate::editor::EditorMode::Insert;
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let original_window = app.window_session.graph().focused().expect("text window");
    let original_content = app
        .window_session
        .graph()
        .content_for_window(original_window)
        .expect("text content")
        .id;
    let before_counts = app.window_session.graph().counts();

    let invocation = crate::editor::CommandInvocation::new(
        "window.clone",
        crate::editor::InvocationSource::Test,
    )
    .expect("clone command")
    .with_argument(
        "target",
        crate::editor::CommandValue::String(format!("window:{}", original_window.0)),
    )
    .with_argument(
        "axis",
        crate::editor::CommandValue::String("vertical".into()),
    );
    let outcome = app
        .commands
        .execute(&app.state, &invocation)
        .expect("typed clone command");
    let [effect] = outcome.effects.as_slice() else {
        panic!("clone command must emit one effect");
    };
    assert!(matches!(
        effect,
        AppEffect::WindowClone {
            target,
            axis: crate::editor::Axis::Vertical,
        } if target == &format!("window:{}", original_window.0)
    ));
    app.process_effect(outcome.effects.into_iter().next().expect("clone effect"));

    let cloned_view = app.state.active_view.expect("cloned editor view");
    let cloned_window = app.window_session.graph().focused().expect("cloned window");
    assert_ne!(cloned_view, original_view);
    assert_ne!(cloned_window, original_window);
    assert_eq!(app.state.views[cloned_view].buffer, buffer);
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(cloned_window)
            .expect("cloned text content")
            .id,
        original_content
    );
    assert_eq!(
        app.window_session.graph().counts(),
        (
            before_counts.0,
            before_counts.1 + 1,
            before_counts.2 + 1,
            before_counts.3,
        )
    );
    assert_eq!(
        app.state.views[cloned_view].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[cloned_view].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(app.state.views[cloned_view].scroll_line, 1);
    assert_eq!(app.state.views[cloned_view].scroll_column, 3);
    assert_eq!(
        app.state.views[cloned_view].mode,
        crate::editor::EditorMode::Insert
    );

    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::SetCursor {
            view: cloned_view,
            position: crate::editor::text::ByteOffset(3),
        })
        .with_operation(crate::editor::EditorOperation::SetScroll {
            view: cloned_view,
            first_line: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetScrollColumn {
            view: cloned_view,
            first_column: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetSelection {
            view: cloned_view,
            selection: None,
        })
        .with_operation(crate::editor::EditorOperation::SetMode {
            view: cloned_view,
            mode: crate::editor::EditorMode::Normal,
        });
    app.apply_authoritative_editor_transaction(&transaction)
        .expect("update only cloned view");
    assert_eq!(
        app.state.views[original_view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    assert_eq!(app.state.views[original_view].scroll_line, 1);
    assert_eq!(app.state.views[original_view].scroll_column, 3);
    assert!(app.state.views[original_view].selection.is_some());
    assert_eq!(
        app.state.views[original_view].mode,
        crate::editor::EditorMode::Insert
    );

    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", original_window.0),
        axis: crate::editor::Axis::Horizontal,
    });
    let exact_clone = app.state.active_view.expect("exact source clone");
    assert_ne!(exact_clone, cloned_view);
    assert_eq!(app.state.views[exact_clone].buffer, buffer);
    assert_eq!(
        app.state.views[exact_clone].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[exact_clone].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_line,
        app.state.views[original_view].scroll_line
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_column,
        app.state.views[original_view].scroll_column
    );
    assert_eq!(
        app.state.views[exact_clone].mode,
        app.state.views[original_view].mode
    );
    assert_ne!(
        app.state.views[exact_clone].cursor,
        app.state.views[cloned_view].cursor
    );
    assert_ne!(
        app.state.views[exact_clone].selection,
        app.state.views[cloned_view].selection
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    let virtual_buffer = virtual_app.state.active_buffer_id().expect("health buffer");
    let virtual_view = virtual_app.state.active_view.expect("health view");
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("health window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("health content")
        .id;
    virtual_app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", virtual_window.0),
        axis: crate::editor::Axis::Horizontal,
    });
    let cloned_virtual_view = virtual_app.state.active_view.expect("cloned health view");
    let cloned_virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("cloned health window");
    assert_ne!(cloned_virtual_view, virtual_view);
    assert_eq!(
        virtual_app.state.views[cloned_virtual_view].buffer,
        virtual_buffer
    );
    assert!(virtual_app.state.buffers[virtual_buffer].is_virtual_read_only());
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(cloned_virtual_window)
            .expect("cloned health content")
            .id,
        virtual_content
    );
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
        axis: crate::editor::Axis::Vertical,
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
    let first_tab = closed_app.state.active_tab.expect("first tab");
    let first_pane = closed_app.state.tabs[first_tab].focused;
    closed_app.state.new_tab().expect("remaining tab");
    closed_app.set_dashboard_visibility(false);
    closed_app.reconcile_editor_windows();
    let closed = closed_app
        .window_session
        .editor_ids()
        .window_for(first_pane)
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
        axis: crate::editor::Axis::Horizontal,
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
    let panes_before = app.state.panes.len();
    let active_before = app.state.active_view;

    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", window.0),
        axis: crate::editor::Axis::Horizontal,
    });

    assert_eq!(app.window_session.graph(), &graph_before);
    assert_eq!(app.state.window_version().revision(), None);
    assert_eq!(app.state.panes.len(), panes_before);
    assert_eq!(app.state.active_view, active_before);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("source revision is exhausted"))
    );
}

#[test]
fn virtual_read_only_origin_never_queues_save_reload_or_recovery_io() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("virtual"), None)
        .unwrap();
    app.state.buffers[buffer].read_only = true;
    app.state.buffers[buffer].origin = crate::editor::BufferOrigin::VirtualReadOnly {
        owner: "demo".into(),
        generation: 7,
        document: 9,
        revision: 1,
    };
    let path = app.state.workspace_root.join("forbidden");
    fs::write(&path, b"unchanged").unwrap();
    app.state.buffers[buffer].path = Some(path.clone());
    let before = app.documents.pending_count();
    app.start_save(false, Some(path.clone()), false);
    assert_eq!(app.documents.pending_count(), before);
    app.start_reload(true);
    assert_eq!(app.documents.pending_count(), before);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"unchanged");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "virtual");
    app.state.buffers[buffer].dirty = true;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.schedule_recovery_work();
    assert!(!app.recovery.scheduled().contains_key(&buffer));
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.capture_session().is_none());
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

fn gate_document_worker(app: &mut App) -> std::sync::mpsc::Sender<()> {
    let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(8);
    app.documents = crate::app::documents::DocumentSession::new(
        worker,
        &StartupTarget::Dashboard {
            root: app.state.workspace_root.clone(),
        },
        app.state.window_version(),
        app.workspace.root_revision(),
        8,
    );
    gate
}

#[test]
fn a_pending_open_reserves_its_save_destination() {
    for alias in [false, true] {
        let (_app_dirs, mut app) = app();
        let root = app.state.workspace_root.clone();
        let destination = root.join("b.txt");
        fs::write(&destination, "original B").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        app.state
            .open_text(TextBuffer::from_text("A"), Some(root.join("a.txt")))
            .unwrap();
        let gate = gate_document_worker(&mut app);
        app.start_open(if alias {
            root.join("sub/../b.txt")
        } else {
            destination.clone()
        });
        app.start_save(false, Some(destination.clone()), true);
        gate.send(()).unwrap();
        wait_vfs(&mut app);
        assert_eq!(fs::read(&destination).unwrap(), b"original B");
        assert!(app.state.buffers.values().any(|b| {
            b.path.as_ref() == Some(&destination) && b.text.to_string() == "original B"
        }));
    }
}

#[test]
fn saves_and_reloads_wait_for_the_same_buffers_completion() {
    for reload_first in [false, true] {
        let (_app_dirs, mut app) = app();
        let root = app.state.workspace_root.clone();
        let original = root.join("a.txt");
        fs::write(&original, "disk").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("buffer"), Some(original.clone()))
            .unwrap();
        let gate = gate_document_worker(&mut app);
        if reload_first {
            app.start_reload(true);
        } else {
            app.start_save(false, Some(root.join("b.txt")), false);
        }
        app.start_save(false, Some(root.join("c.txt")), true);
        assert!(
            app.ui
                .notification
                .as_deref()
                .unwrap()
                .contains("already pending")
        );
        app.start_reload(true);
        assert!(
            app.ui
                .notification
                .as_deref()
                .unwrap()
                .contains("already pending")
        );
        gate.send(()).unwrap();
        wait_vfs(&mut app);
        assert!(!root.join("c.txt").exists());
        if reload_first {
            assert_eq!(app.state.buffers[buffer].text.to_string(), "disk");
            assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&original));
        } else {
            assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"buffer");
            assert_eq!(
                app.state.buffers[buffer].path.as_ref(),
                Some(&root.join("b.txt"))
            );
        }
        // Admission resumes after the first result has been reconciled.
        app.start_save(false, Some(root.join("c.txt")), false);
        wait_vfs(&mut app);
        assert_eq!(
            app.state.buffers[buffer].path.as_ref(),
            Some(&root.join("c.txt"))
        );
        assert_eq!(
            fs::read(root.join("c.txt")).unwrap(),
            app.state.buffers[buffer].text.to_bytes()
        );
    }
}

#[test]
fn an_alternate_spelling_of_the_current_file_uses_its_saved_bytes() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let path = root.join("file.txt");
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    app.invoke_insert_text("new ".into());
    app.start_save(false, Some(root.join("sub/../file.txt")), false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"new old");
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    // The resolved identity must not bypass external-modification protection.
    fs::write(&path, "external").unwrap();
    app.start_save(false, Some(root.join("sub/../file.txt")), false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"external");
}

#[cfg(unix)]
#[test]
fn save_alias_identity_is_resolved_by_the_worker_before_writing() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let original = root.join("original.txt");
    let other = root.join("other.txt");
    let alias = root.join("alias.txt");
    fs::write(&original, "old").unwrap();
    fs::write(&other, "unrelated").unwrap();
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(original.clone()))
        .unwrap();
    app.invoke_insert_text("new ".into());
    let gate = gate_document_worker(&mut app);
    app.start_save(false, Some(alias.clone()), false);
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    gate.send(()).unwrap();
    wait_vfs(&mut app);
    assert_eq!(fs::read(&original).unwrap(), b"old");
    assert_eq!(fs::read(&other).unwrap(), b"unrelated");
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&original));
}

#[test]
fn saved_line_ending_policy_updates_the_live_buffer_without_losing_new_edits() {
    for edit_during_save in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("file.txt");
        fs::write(&path, "a\nb\n").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("a\nb\n"), Some(path.clone()))
            .unwrap();
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::CrLf;
        app.configuration.install_fixture(fixture);
        let gate = gate_document_worker(&mut app);
        app.start_save(false, None, false);
        if edit_during_save {
            app.invoke_insert_text("new\n".into());
        }
        gate.send(()).unwrap();
        wait_vfs(&mut app);
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\n");
        assert_eq!(app.state.buffers[buffer].dirty, edit_during_save);
        assert_eq!(
            app.state.buffers[buffer]
                .text
                .format()
                .preferred_line_ending,
            crate::editor::text::LineEnding::CrLf
        );
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::Preserve;
        app.configuration.install_fixture(fixture);
        app.start_save(false, None, false);
        wait_vfs(&mut app);
        let expected = if edit_during_save {
            b"new\r\na\r\nb\r\n".as_slice()
        } else {
            b"a\r\nb\r\n".as_slice()
        };
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert!(!app.state.buffers[buffer].dirty);
    }
}

#[test]
fn shutdown_saves_normalized_text_with_resumable_undo_history() {
    for (source, policy, saved) in [
        ("base", crate::config::FinalNewlinePolicy::Insert, "Xbase\n"),
        (
            "base\n\n",
            crate::config::FinalNewlinePolicy::Remove,
            "Xbase",
        ),
    ] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("normalized.txt");
        fs::write(&path, source).unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(source), Some(path.clone()))
            .unwrap();
        app.set_dashboard_visibility(false);
        let directory = app.persistence.undo_directory();
        app.state.undo_directory = Some(directory.clone());
        app.state.undo_maximum_bytes = 1 << 20;
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.final_newline = policy;
        app.configuration.install_fixture(fixture);
        app.invoke_insert_text("X".into());
        app.start_save(true, None, false);
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        assert_eq!(app.state.buffers[buffer].text.to_string(), saved);
        assert!(!app.persistence.undo_pending());
        let mut reopened = EditorState::new(app.state.workspace_root.clone());
        reopened.undo_directory = Some(directory);
        reopened.undo_maximum_bytes = 1 << 20;
        let (id, _) = reopened
            .open_text(TextBuffer::from_text(saved), Some(path))
            .unwrap();
        for expected in [format!("X{source}"), source.into()] {
            reopened
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(1))
                        .with_operation(EditorOperation::Undo { buffer: id }),
                )
                .unwrap();
            assert_eq!(reopened.buffers[id].text.to_string(), expected);
        }
    }
}

#[test]
fn undo_history_failures_are_notified_and_fail_shutdown() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("history.txt");
    fs::write(&path, "base").unwrap();
    app.state
        .open_text(TextBuffer::from_text("base"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    let invalid = app.state.workspace_root.join("not-a-directory");
    fs::write(&invalid, "occupied").unwrap();
    app.state.undo_directory = Some(invalid);
    app.state.undo_maximum_bytes = 1 << 20;
    app.invoke_insert_text("X".into());
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.persistence.undo_pending() {
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        app.ui
            .notification
            .as_ref()
            .is_some_and(|text| text.contains("could not save undo history"))
    );
    assert!(app.finish_shutdown_before(deadline).is_err());
}

#[test]
fn configured_file_format_policy_is_applied_at_save_boundary() {
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::Lf;
    fixture.settings.editor.final_newline = crate::config::FinalNewlinePolicy::Insert;
    app.configuration.install_fixture(fixture);
    let text = crate::editor::text::TextBuffer::from_bytes(b"a\r\nb").expect("CRLF text");
    let (buffer, _) = app.state.open_text(text, None).unwrap();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("formatted.txt");
    app.start_save(false, Some(path.clone()), false);
    assert_eq!(app.documents.pending_count(), 1);
    wait_vfs(&mut app);
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(fs::read(path).unwrap(), b"a\nb\n");
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
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("shared split");
    app.state.new_tab().expect("shared tab");
    let tab_count = app.state.tabs.len();
    app.apply_layout_action(crate::app::LayoutAction::CloseTab);
    assert_eq!(
        app.state.tabs.len(),
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
fn dirty_buffer_close_save_discard_cancel_and_stale_ack_are_source_bound() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("dirty-close.txt");
    std::fs::write(&path, "saved\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("saved\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.buffers[buffer]
        .text
        .restore_text("discard me\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    let arrangement = (
        app.state.active_tab,
        app.state.active_view,
        app.state.tabs.len(),
        app.state.panes.len(),
        app.state.views.len(),
    );

    app.close_active_buffer(false);
    let prompt = app.input.prompt().expect("dirty close prompt");
    let confirmation = prompt.confirmation.as_ref().unwrap();
    assert_eq!(prompt.kind, crate::ui::PromptKind::DirtyClose);
    assert_eq!(confirmation.positive_label, "Save");
    assert_eq!(confirmation.negative_label, "Discard");
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::No);
    app.close_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    assert_eq!(
        (
            app.state.active_tab,
            app.state.active_view,
            app.state.tabs.len(),
            app.state.panes.len(),
            app.state.views.len(),
        ),
        arrangement
    );

    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    assert!(app.documents.file_work_pending());
    app.state.buffers[buffer]
        .text
        .restore_text("newer edit\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    crate::app::tests::wait_vfs(&mut app);
    assert!(app.state.buffers.contains_key(buffer));
    assert!(app.state.buffers[buffer].dirty);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("close was cancelled"))
    );

    app.close_active_buffer(false);
    app.submit_prompt();
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "discard me\n");
}

#[test]
fn exact_dirty_window_close_rejects_stale_focus_then_discards_only_its_target() {
    let (_app_dirs, mut app) = app();
    let (keeper, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("keeper"), None)
        .unwrap();
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .unwrap();
    let (dirty, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None)
        .unwrap();
    app.state.set_buffer_dirty(dirty, true);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    let window = app.window_session.editor_ids().window_for(pane).unwrap();
    let target = format!("window:{}", window.0);

    app.close_graph_window(&target).unwrap();
    app.apply_layout_action(crate::app::LayoutAction::FocusPane(-1));
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
fn dirty_close_save_waits_for_exact_vfs_ack_and_failure_preserves_arrangement() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("save-before-close.txt");
    std::fs::write(&path, "old\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("old\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.buffers[buffer]
        .text
        .restore_text("new\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    crate::app::tests::wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");

    let directory = app.state.workspace_root.join("not-a-file");
    std::fs::create_dir(&directory).unwrap();
    let (failed, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("dirty\n"),
            Some(directory),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.set_buffer_dirty(failed, true);
    app.reconcile_editor_windows();
    let arrangement = (
        app.state.active_tab,
        app.state.active_view,
        app.state.tabs.len(),
        app.state.panes.len(),
        app.state.views.len(),
    );
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    crate::app::tests::wait_vfs(&mut app);
    assert!(app.state.buffers.contains_key(failed));
    assert!(app.state.buffers[failed].dirty);
    assert_eq!(
        (
            app.state.active_tab,
            app.state.active_view,
            app.state.tabs.len(),
            app.state.panes.len(),
            app.state.views.len(),
        ),
        arrangement
    );
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
        .split_active(crate::editor::Axis::Horizontal, 500)
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
        crate::app::LspRuntimeState {
            root: app.workspace.active_root().unwrap(),
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::app::LspLaunch::Direct {
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
    let tabs = app.state.tabs.len();

    app.apply_layout_action(crate::app::LayoutAction::CloseTab);
    assert_eq!(app.state.tabs.len(), tabs);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert_eq!(
        app.state.tabs.len(),
        tabs,
        "save admission is not close ACK"
    );
    crate::app::tests::wait_vfs(&mut app);
    assert_eq!(
        app.state.tabs.len(),
        tabs,
        "second dirty target needs review"
    );
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::DirtyClose
    );
    app.submit_prompt();
    assert_eq!(app.state.tabs.len(), tabs - 1);
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
        .split_active(crate::editor::Axis::Vertical, 500)
        .unwrap();
    let (b, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("b"), None)
        .unwrap();
    app.state.set_buffer_dirty(a, true);
    app.state.set_buffer_dirty(b, true);
    app.reconcile_editor_windows();
    let source = (
        app.state.active_tab,
        app.state.active_view,
        app.state.tabs.len(),
        app.state.panes.len(),
        app.state.views.len(),
    );
    app.apply_layout_action(crate::app::LayoutAction::CloseTab);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_some(),
        "second dirty target is reviewed"
    );
    app.close_prompt();
    assert_eq!(
        (
            app.state.active_tab,
            app.state.active_view,
            app.state.tabs.len(),
            app.state.panes.len(),
            app.state.views.len(),
        ),
        source
    );
    assert!(app.state.buffers.contains_key(a));
    assert!(app.state.buffers.contains_key(b));
}

#[test]
fn dashboard_selection_tracks_the_actions_after_configuration_refresh() {
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
fn documentation_scrolling_uses_the_final_completion_box_and_reaches_its_tail() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text(&"line\n".repeat(30)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_event(crate::ui::UiEvent::Resize(80, 24));
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        typed: 0,
        items: (0..16)
            .map(|_| crate::ui::CompletionItem {
                icon: None,
                color: None,
                label: "completion".into(),
                detail: "detail".repeat(12),
            })
            .collect(),
        selected: 0,
        scroll: 0,
    });
    app.input.present_documentation(
        crate::ui::DocumentationPopupKind::Completion,
        "Completion".into(),
        String::new(),
        None,
        [(
            crate::ui::DocumentationMarkupKind::PlainText,
            format!("{}THE-END", "x".repeat(2000)),
        )],
        app.configuration.settings(),
        80,
    );
    app.project_input_documentation();
    for _ in 0..100 {
        app.process_effect(AppEffect::LspDocumentationScroll(1));
    }
    let (frame, geometry) = crate::ui::render::render_with_documentation_geometry(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    let (visible, total) = geometry.unwrap();
    assert_eq!(app.input.documentation().unwrap().scroll, total - visible);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("THE-END")));
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

#[test]
fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    let first_revision = app.state.buffers[buffer].text.revision();
    app.invoke_insert_text("B".into());
    app.schedule_recovery_work();
    let latest_revision = app.state.buffers[buffer].text.revision();
    assert_ne!(first_revision, latest_revision);
    assert_eq!(app.recovery.scheduled().len(), 1);
    assert_eq!(app.recovery.scheduled()[&buffer].0, latest_revision);
    assert!(!app.recovery.writing(None));
    std::thread::sleep(Duration::from_millis(
        crate::app::recovery::WRITE_DEBOUNCE_MS + 10,
    ));
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    let journal = app.recovery.path(buffer);
    let recovered = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest_revision.0);

    app.state.buffers[buffer].mark_saved(latest_revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!journal.exists());
    assert!(!app.recovery.revisions().contains_key(&buffer));

    app.invoke_insert_text("X".into());
    let revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    app.state.buffers[buffer].mark_saved(revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery.path(buffer).exists());
}

#[test]
fn recovery_retention_keeps_all_live_dirty_journals() {
    let (_app_dirs, mut app) = app();
    let mut journals = Vec::new();
    for index in 0..101 {
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(&format!("draft {index}")), None)
            .unwrap();
        app.state.set_buffer_dirty(buffer, true);
        journals.push(app.recovery.path(buffer));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while app
        .recovery
        .unwritten(&app.state, &app.configuration.settings().recovery)
        > 0
    {
        assert!(Instant::now() < deadline);
        app.queue_dirty_recovery_before_exit();
        wait_vfs(&mut app);
    }
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(
        journals.iter().all(|path| path.exists()),
        "retention must not delete live drafts"
    );
    assert_eq!(
        app.recovery
            .unwritten(&app.state, &app.configuration.settings().recovery),
        0
    );
}

#[test]
fn recovery_save_and_close_removes_the_older_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let path = app.state.workspace_root.join("close-recovery.txt");
    fs::write(&path, "saved").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("saved"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let journal = app.recovery.path(buffer);
    assert!(journal.exists());
    app.invoke_insert_text("B".into());
    let expected = app.state.buffers[buffer].text.to_string();
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(fs::read_to_string(path).unwrap(), expected);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!journal.exists());
}

#[test]
fn recovery_sessions_keep_distinct_journals_for_the_same_buffer() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let first = app.recovery.path(buffer);
    let directory = app.recovery.directory().to_path_buf();
    app.recovery = crate::app::recovery::RecoverySession::new(directory.clone());
    app.recovery.skip_maintenance();
    let second = app.recovery.path(buffer);
    assert_ne!(first, second);
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    assert_eq!(crate::files::discover_recovery(&directory).records.len(), 2);
    let revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer].mark_saved(revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(first.exists());
    assert!(!second.exists());
}

#[test]
fn recovery_rewrites_the_path_without_a_text_revision_change() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let revision = app.state.buffers[buffer].text.revision();
    let renamed = app.state.workspace_root.join("renamed.txt");
    app.state.buffers[buffer].path = Some(renamed.clone());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.revision(), revision);
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(discovery.records.len(), 1);
    assert_eq!(discovery.records[0].1.path(), Some(renamed));
}

#[test]
fn shutdown_returns_the_reason_dirty_buffers_cannot_be_recovered() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&"x".repeat(12 * 1024 * 1024)),
            None,
        )
        .unwrap();
    let buffer = app.state.active_buffer_id().unwrap();
    app.state.buffers[buffer].dirty = true;
    let error = app
        .finish_shutdown_before(Instant::now())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exceed the recovery journal size limit"),
        "{error}"
    );
    assert!(
        error.contains("unsaved contents cannot be recovered"),
        "{error}"
    );
}

#[test]
fn shutdown_retries_recovery_after_document_admission_drains() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let root = app.state.workspace_root.clone();
    let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(8);
    app.documents = crate::app::documents::DocumentSession::new(
        worker,
        &StartupTarget::Dashboard { root: root.clone() },
        app.state.window_version(),
        app.workspace.root_revision(),
        8,
    );
    let file = root.join("source");
    std::fs::write(&file, "source").unwrap();
    for _ in 0..8 {
        app.documents.read(file.clone()).unwrap();
    }
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.recovery.scheduled().contains_key(&buffer));
    let directory = app.recovery.directory().to_path_buf();
    gate.send(()).unwrap();
    app.finish_shutdown_before(Instant::now() + Duration::from_secs(2))
        .unwrap();
    let records = crate::files::discover_recovery(&directory).records;
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].1.content,
        app.state.buffers[buffer].text.to_string()
    );
}

#[test]
fn recovery_dialog_discards_one_journal_and_recovers_another() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let directory = app.recovery.directory().to_owned();
    std::fs::create_dir_all(&directory).unwrap();
    let kept = directory.join("buffer-kept.toml");
    let dropped = directory.join("buffer-dropped.toml");
    std::fs::write(&kept, "").unwrap();
    std::fs::write(&dropped, "").unwrap();
    let notes = app.state.workspace_root.join("notes.md");
    std::fs::write(&notes, "saved\n").unwrap();
    app.recovery.fixture_records(vec![
        (
            kept.clone(),
            crate::files::RecoveryRecord::new(Some(&notes), 3, "draft\nsecond\n".into()),
        ),
        (
            dropped.clone(),
            crate::files::RecoveryRecord {
                version: crate::files::RECOVERY_VERSION,
                original_path: None,
                revision: 1,
                content: "scratch".into(),
            },
        ),
    ]);

    app.invoke_mapped_command("recovery.inspect");
    let picker = app.ui.picker.as_ref().expect("recovery dialog");
    assert_eq!(picker.title, "Recover Unsaved Work");
    assert_eq!(picker.rows.len(), 2);
    assert_eq!(picker.rows[0].detail, "1 line");
    assert!(picker.rows[1].label.contains("notes.md"));

    app.invoke_mapped_command("picker.remove");
    wait_vfs(&mut app);
    assert!(!dropped.exists());
    let picker = app.ui.picker.as_ref().expect("the dialog stays open");
    assert_eq!(picker.rows.len(), 1);
    assert!(
        picker
            .preview
            .as_ref()
            .and_then(crate::ui::PickerPreview::as_text)
            .is_some_and(|text| text.lines() == ["draft", "second"])
    );

    app.accept_picker();
    wait_vfs(&mut app);
    assert!(kept.exists(), "the durable journal survives recovery");
    assert!(app.recovery.records().is_empty());
    // The work comes back as an unsaved change to its own file; undo goes back to the disk.
    let buffer = app.state.active_buffer_id().expect("recovered buffer");
    assert_eq!(
        app.state.buffers[buffer].path.as_deref(),
        Some(notes.as_path())
    );
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_bytes(),
        b"draft\nsecond\n"
    );
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_bytes(), b"saved\n");
}

#[test]
fn recovery_delete_completion_cannot_forget_a_newer_queued_write() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    let journal = app.recovery.directory().join("recovered-draft.toml");
    app.recovery.retain_recovered(buffer, journal.clone());
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    std::thread::sleep(Duration::from_millis(
        crate::app::recovery::WRITE_DEBOUNCE_MS + 10,
    ));
    app.schedule_recovery_work();
    wait_vfs(&mut app);

    let saved_revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer].mark_saved(saved_revision);
    app.schedule_recovery_work();
    assert!(app.recovery.deleting());

    app.invoke_insert_text("B".into());
    let latest = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    std::thread::sleep(Duration::from_millis(
        crate::app::recovery::WRITE_DEBOUNCE_MS + 10,
    ));
    app.schedule_recovery_work();
    assert_eq!(app.recovery.revisions().get(&buffer), Some(&latest));
    app.recovery.check_foreign_completions();
    wait_vfs(&mut app);

    let recovered = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest.0);
    assert_eq!(app.recovery.revisions().get(&buffer), Some(&latest));
    assert_eq!(app.recovery.path(buffer), journal);
    app.schedule_recovery_work();
    assert!(!app.recovery.writing(None));
    app.state.buffers[buffer].mark_saved(latest);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(
        !journal.exists(),
        "saving removes the recovered journal, not a new session path"
    );
}

#[test]
fn disabled_recovery_policy_schedules_no_work() {
    let (_app_dirs, mut app) = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.invoke_insert_text("X".into());
    app.schedule_recovery_work();
    assert!(app.recovery.scheduled().is_empty());
    assert_eq!(app.documents.legacy_count(), 0);
}

#[test]
fn bundled_upper_h_and_l_cycle_open_buffers_in_normal_mode() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/first.rs")),
        )
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/second.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    assert_eq!(app.state.active_buffer_id(), Some(second));

    app.handle_key_in_context("editor.normal", KeyEvent::character('H'));
    assert_eq!(app.state.active_buffer_id(), Some(first));
    app.handle_key_in_context("editor.normal", KeyEvent::character('L'));
    assert_eq!(app.state.active_buffer_id(), Some(second));
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
    let target = app.window_session.editor_ids().content_for(first).unwrap();
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
    let row = bounds.y + u16::from(app.state.tabs.len() > 1);
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
fn breakpoint_gutter_signs_distinguish_disabled_breakpoints() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("source.rs");
    app.state
        .open_text(
            TextBuffer::from_text(&"source\n".repeat(15)),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.debugger_session
        .breakpoint(
            crate::debugger::DapSourceBreakpoint {
                path: path.clone(),
                line: 10,
                column: None,
                condition: Some("ready".into()),
                hit_condition: None,
                log_message: None,
            },
            false,
        )
        .unwrap();
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 9);
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::ConditionalBreakpoint
    );
    app.debugger_session
        .set_breakpoint_disabled(&path, 10, true);
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 9);
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::DisabledBreakpoint
    );
    app.debugger_session
        .set_breakpoint_disabled(&path, 10, false);
    app.project_debug_panes();
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::ConditionalBreakpoint
    );
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
fn buffer_line_mouse_selects_and_right_click_closes_with_dirty_safety() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/first.rs")),
        )
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/second.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let target = app.window_session.editor_ids().content_for(first).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.tabs.len() > 1);
    let left = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseLeftDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&left, bounds.x + local, row));
    assert_eq!(app.state.active_buffer_id(), Some(first));
    app.state.set_buffer_dirty(first, true);
    app.reconcile_editor_windows();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let right = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseRightDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(app.state.buffers.contains_key(first));
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::DirtyClose)
    );
    app.close_prompt();
    app.state.set_buffer_dirty(first, false);
    app.reconcile_editor_windows();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(
        !app.state.buffers.contains_key(first),
        "{:?}",
        app.ui.notification
    );
    assert_eq!(app.state.active_buffer_id(), Some(second));
}

#[test]
fn content_line_uses_filename_icons_for_extensionless_files() {
    let (_app_dirs, mut app) = app();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        app.state
            .open_text(
                TextBuffer::from_text("file"),
                Some(PathBuf::from("/workspace").join(name)),
            )
            .unwrap();
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        let entry = app
            .ui
            .content_tabs
            .iter()
            .find(|entry| entry.title == name)
            .unwrap();
        assert_eq!(entry.icon, crate::files::file_icon(name).unwrap().glyph);
        assert_ne!(entry.icon, SCRATCH_ICON);
    }
}

#[test]
fn content_line_pins_reorder_middle_mouse_and_survive_bridge_reset() {
    let (_app_dirs, mut app) = app();
    let (a, _) = app
        .state
        .open_text(
            TextBuffer::from_text("a"),
            Some(PathBuf::from("/workspace/a.rs")),
        )
        .unwrap();
    let (b, _) = app
        .state
        .open_text(
            TextBuffer::from_text("b"),
            Some(PathBuf::from("/workspace/b.rs")),
        )
        .unwrap();
    let (c, _) = app
        .state
        .open_text(
            TextBuffer::from_text("c"),
            Some(PathBuf::from("/workspace/c.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state
        .open_text(TextBuffer::from_text("scratch"), None)
        .unwrap();
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == SCRATCH_ICON && entry.kind == ContentTabKind::Text)
    );
    app.state.show_buffer(b).unwrap();
    app.reconcile_editor_windows();
    app.toggle_active_buffer_pin();
    assert_eq!(
        app.ui.content_tabs.first().map(|entry| entry.pinned),
        Some(true),
        "{:?} active={:?} pins={:?}",
        app.ui.content_tabs,
        app.state.active_buffer_id(),
        app.window_session.pins()
    );
    app.state.show_buffer(a).unwrap();
    app.reconcile_editor_windows();
    app.toggle_active_buffer_pin();
    app.state.show_buffer(b).unwrap();
    app.reconcile_editor_windows();
    app.move_active_buffer_line(-1);
    let pinned = app
        .ui
        .content_tabs
        .iter()
        .filter(|entry| entry.pinned)
        .map(|entry| entry.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(pinned, vec!["b.rs", "a.rs"]);
    let before = app.window_session.content_order().to_vec();
    app.move_active_buffer_line(-1);
    assert_eq!(app.window_session.content_order(), before);
    let bounds = app.workspace_bounds().unwrap();
    let target = app.window_session.editor_ids().content_for(c).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.tabs.len() > 1);
    let middle = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseMiddleDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&middle, bounds.x + local, row));
    assert!(app.window_session.pins().contains(&c));
    let pins = app.window_session.pins().clone();
    let order = app.window_session.content_order().to_vec();
    app.window_session.reset_runtime(&[]).unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.pins(), &pins);
    assert_eq!(app.window_session.content_order(), order);
    let views = app
        .state
        .views
        .iter()
        .filter_map(|(view, state)| (state.buffer == c).then_some(view))
        .collect::<Vec<_>>();
    for view in views {
        let _ = app.state.close_view(view);
    }
    app.state.remove_buffer(c);
    app.reconcile_editor_windows();
    assert!(
        !app.window_session.pins().contains(&c) && !app.window_session.content_order().contains(&c)
    );
}

#[test]
fn content_line_diagnostics_custom_entries_and_idle_projection_are_pure() {
    let (_app_dirs, mut app) = app();
    let path = PathBuf::from("/workspace/diagnostic.rs");
    app.state
        .open_text(TextBuffer::from_text("bad"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("fixture").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    let uri = crate::language::lsp::file_uri(&path);
    app.diagnostics
        .publish(
            crate::app::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: crate::app::diagnostics::DiagnosticDocument::capture(
                    &app.state,
                    app.state.active_buffer_id().unwrap(),
                ),
                validity:
                    crate::app::diagnostics::DiagnosticValidity::RequestedSnapshot,
                items: vec![crate::workspace::Diagnostic {
                    uri,
                    version: None,
                    range: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                    severity: DiagnosticSeverity::Error,
                    source: "rustc".into(),
                    code: Some("E0001".into()),
                    message: "bad".into(),
                    related: Vec::new(),
                    tags: Vec::new(),
                    fixes: Vec::new(),
                    data: None,
                    timestamp: std::time::SystemTime::UNIX_EPOCH,
                }],
            },
            &app.state,
        )
        .unwrap();
    app.sync_ui_diagnostics();
    let buffer_count = app.state.buffers.len();
    let mut draft = app
        .window_session
        .begin_plugin(
            "demo",
            1,
            (
                &app.state.window_version(),
                &app.extension.contributions().version(),
            ),
        )
        .unwrap();
    let custom = draft
        .create_content_with_contract(
            crate::ui::window::ContentTypeId::new("plugin.demo-custom").unwrap(),
            crate::ui::window::ContentContract::adapter_defaults(
                crate::ui::window::ContentAdapterKind::DeclarativeUi,
            ),
            "Custom surface".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    app.window_session
        .commit_plugin(
            draft,
            &app.state.window_version(),
            &app.extension.contributions().version(),
        )
        .unwrap();
    app.reconcile_editor_windows();
    let text = app
        .ui
        .content_tabs
        .iter()
        .find(|entry| entry.kind == ContentTabKind::Text)
        .unwrap();
    assert_eq!(text.icon, crate::files::file_icon("a.rs").unwrap().glyph);
    assert_eq!(
        text.diagnostic,
        Some(ContentTabDiagnostic {
            severity: ContentTabDiagnosticSeverity::Error,
            count: 1
        })
    );
    assert_eq!(app.state.buffers.len(), buffer_count);
    let custom_entry = app
        .ui
        .content_tabs
        .iter()
        .find(|entry| entry.identity.0 == custom)
        .unwrap();
    assert_eq!(
        (custom_entry.icon.as_str(), custom_entry.closeable),
        (CUSTOM_ICON, false)
    );
    app.window_session
        .observe_size((300, app.window_session.height()));
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == custom)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.tabs.len() > 1);
    let right = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseRightDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(
        app.ui.notification_error
            && app
                .window_session
                .graph()
                .content_snapshots()
                .iter()
                .any(|entry| entry.id == custom)
    );
    app.window_session
        .remove_plugin_generation("demo", 2)
        .unwrap();
    assert!(
        app.window_session
            .graph()
            .content_snapshots()
            .iter()
            .any(|entry| entry.id == custom)
    );
    app.window_session
        .remove_plugin_generation("demo", 1)
        .unwrap();
    app.reconcile_editor_windows();
    assert!(
        !app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.identity.0 == custom)
    );
    let signature = app.window_session.content_stamp();
    let tabs = app.ui.content_tabs.clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
        let frame = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        assert_eq!(frame.grid.width(), app.window_session.width());
        assert_eq!(app.window_session.content_stamp(), signature);
        assert_eq!(app.ui.content_tabs, tabs);
    }
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
    let before = app.window_session.editor_ids().reconciliation_count();
    app.state
        .set_workspace_root(PathBuf::from("/workspace/src"));
    app.reconcile_editor_windows();
    assert!(app.window_session.editor_ids().reconciliation_count() > before);
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.title == "main.rs ·1")
    );
    assert_eq!(
        app.ui.content_tabs[1].identity,
        crate::ui::ContentTabIdentity(
            app.window_session
                .editor_ids()
                .content_for(second)
                .expect("second content")
        )
    );

    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 3, 3, 1));
    let focused = app.window_session.graph().focused().expect("focused split");

    let second_workspace_tab = app.state.new_tab().expect("new workspace tab");
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 4, 4, 2));
    assert_eq!(
        app.window_session.graph().active_tab(),
        app.window_session
            .editor_ids()
            .tab_for(second_workspace_tab)
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
    let row = u16::from(app.state.tabs.len() > 1);
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
fn editor_layout_commands_commit_state_bridge_and_graph_as_one_authoritative_transaction() {
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

    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::Split(crate::editor::Axis::Horizontal),
    ));
    assert_eq!(app.state.panes.len(), 2);
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
    app.process_effect(crate::app::AppEffect::WindowSwap {
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
    app.process_effect(crate::app::AppEffect::WindowResize {
        target: format!("window:{}", focused.0),
        amount: 50,
    });
    assert_ne!(app.window_session.graph(), &before_resize);
    assert_eq!(app.state.panes.len(), 2);

    app.process_effect(crate::app::AppEffect::WindowClose(format!(
        "window:{}",
        focused.0
    )));
    assert_eq!(app.state.panes.len(), 1);
    assert!(app.window_session.graph().window(initial_window).is_some());
    let state_revision_before_rejected = app.state.window_revision();
    let active_view_before_rejected = app.state.active_view;
    let pane_count_before_rejected = app.state.panes.len();
    let graph_before_rejected = app.window_session.graph().clone();
    app.process_effect(crate::app::AppEffect::WindowClose("focused".into()));
    assert_eq!(app.state.window_revision(), state_revision_before_rejected);
    assert_eq!(app.state.active_view, active_view_before_rejected);
    assert_eq!(app.state.panes.len(), pane_count_before_rejected);
    assert_eq!(app.window_session.graph(), &graph_before_rejected);
    assert!(app.ui.notification_error);
}

#[test]
fn text_and_virtual_windows_project_input_and_preserve_identity_across_ordinary_placements() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(
            TextBuffer::from_text("floating editor contents"),
            Some(PathBuf::from("/workspace/floating.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let tab = app.state.active_tab.expect("editor tab");
    let source_pane = app.state.tabs[tab].focused;
    let source_view = app.state.panes[source_pane].view;
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let source_window = app
        .window_session
        .editor_ids()
        .window_for(source_pane)
        .expect("source window");
    let source_content = app
        .window_session
        .graph()
        .content_for_window(source_window)
        .expect("source content")
        .id;

    app.process_effect(AppEffect::WindowFloat {
        target: format!("window:{}", source_window.0),
        x: 7,
        y: 4,
        width: 31,
        height: 9,
    });
    let floating = app
        .compositor_projection()
        .and_then(|projection| projection.window(source_window))
        .expect("floating editor projection");
    assert_eq!(floating.layer, crate::ui::window::CompositorLayer::Floating);
    assert_eq!(
        floating.rect,
        WindowRect {
            x: 7,
            y: 4,
            width: 31,
            height: 9,
        }
    );
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    assert!(
        (4..13).any(|row| (7..38).any(|column| {
            frame
                .grid
                .get(column, row)
                .is_some_and(|cell| cell.symbol == "f")
        })),
        "floating editor content is painted in its projected rectangle"
    );
    app.input.observe_mouse(12, 5);
    app.focus_mouse_pane();
    assert_eq!(app.state.tabs[tab].focused, source_pane);
    assert_eq!(app.state.active_view, Some(source_view));

    app.process_effect(AppEffect::WindowMove {
        target: format!("window:{}", source_window.0),
        x: 11,
        y: 6,
    });
    app.process_effect(AppEffect::WindowResize {
        target: format!("window:{}", source_window.0),
        amount: 3,
    });
    let moved_source = WindowPlacement::Floating(WindowRect {
        x: 11,
        y: 6,
        width: 34,
        height: 12,
    });
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("moved editor")
            .placement,
        moved_source
    );
    let other_pane = app
        .state
        .panes
        .keys()
        .find(|pane| *pane != source_pane)
        .expect("other pane");
    let other_window = app
        .window_session
        .editor_ids()
        .window_for(other_pane)
        .expect("other editor window");
    let other_placement = WindowPlacement::Floating(WindowRect {
        x: 48,
        y: 2,
        width: 25,
        height: 7,
    });
    app.place_graph_window(&format!("window:{}", other_window.0), other_placement)
        .expect("float second editor");
    app.swap_graph_windows(
        &format!("window:{}", source_window.0),
        &format!("window:{}", other_window.0),
    )
    .expect("swap non-tiled editor placements");
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("swapped source")
            .placement,
        other_placement
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(other_window)
            .expect("swapped other")
            .placement,
        moved_source
    );

    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 18,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        app.place_graph_window(&format!("window:{}", source_window.0), placement)
            .expect("supported editor placement");
        assert_eq!(
            app.window_session
                .graph()
                .window(source_window)
                .expect("same editor window")
                .placement,
            placement
        );
        assert_eq!(
            app.window_session
                .graph()
                .content_for_window(source_window)
                .expect("same content")
                .id,
            source_content
        );
        assert_eq!(
            app.window_session.editor_ids().window_for(source_pane),
            Some(source_window)
        );
        if placement == WindowPlacement::Hidden {
            assert_ne!(app.state.tabs[tab].focused, source_pane);
            assert_eq!(
                app.window_session.graph().focused(),
                app.window_session
                    .editor_ids()
                    .window_for(app.state.tabs[tab].focused)
            );
        }
    }
    app.show_graph_window(&format!("window:{}", source_window.0))
        .expect("show hidden editor");
    assert_eq!(app.state.active_view, Some(source_view));
    assert_eq!(app.state.tabs[tab].focused, source_pane);
    assert_eq!(app.window_session.graph().focused(), Some(source_window));
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("restored editor")
            .placement,
        WindowPlacement::Fullscreen
    );
    app.place_graph_window(
        &format!("window:{}", source_window.0),
        WindowPlacement::Tiled,
    )
    .expect("return editor to exact logical tile");
    assert!(
        app.window_session
            .graph()
            .tiled_layout_snapshot(app.window_session.graph().active_tab().expect("active tab"))
            .expect("restored tiled layout")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 24,
                },
                1,
                1,
            )
            .windows
            .iter()
            .any(|window| window.window == source_window)
    );
    app.close_graph_window(&format!("window:{}", other_window.0))
        .expect("close exact non-tiled editor view");
    assert!(app.window_session.graph().window(other_window).is_none());
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(source_window)
            .expect("shared text content survives view close")
            .id,
        source_content
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("virtual content")
        .clone();
    assert_eq!(
        virtual_content.adapter,
        crate::ui::window::ContentAdapterKind::VirtualDocument
    );
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Floating(WindowRect {
                x: 4,
                y: 3,
                width: 40,
                height: 10,
            }),
        )
        .expect("float virtual document");
    virtual_app
        .move_graph_window(&format!("window:{}", virtual_window.0), 6, 5)
        .expect("move virtual document");
    virtual_app
        .resize_graph_window(&format!("window:{}", virtual_window.0), 2)
        .expect("resize virtual document");
    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 16,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        virtual_app
            .place_graph_window(&format!("window:{}", virtual_window.0), placement)
            .expect("place virtual document");
        assert_eq!(
            virtual_app
                .window_session
                .graph()
                .window(virtual_window)
                .expect("same virtual window")
                .placement,
            placement
        );
    }
    virtual_app
        .show_graph_window(&format!("window:{}", virtual_window.0))
        .expect("show virtual document");
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Tiled,
        )
        .expect("tile virtual document");
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(virtual_window)
            .expect("same virtual content")
            .id,
        virtual_content.id
    );
    let virtual_pane = virtual_app
        .window_session
        .editor_ids()
        .pane_for_window(virtual_window)
        .expect("virtual pane");
    virtual_app
        .clone_graph_window(
            &format!("window:{}", virtual_window.0),
            crate::editor::Axis::Horizontal,
        )
        .expect("clone virtual view for close qualification");
    let virtual_clone = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual clone window");
    assert_ne!(virtual_clone, virtual_window);
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_clone.0),
            WindowPlacement::Floating(WindowRect {
                x: 8,
                y: 4,
                width: 30,
                height: 8,
            }),
        )
        .expect("float virtual clone");
    virtual_app
        .close_graph_window(&format!("window:{}", virtual_clone.0))
        .expect("close exact virtual view");
    assert!(
        virtual_app
            .window_session
            .graph()
            .window(virtual_clone)
            .is_none()
    );
    assert_eq!(
        virtual_app
            .window_session
            .editor_ids()
            .window_for(virtual_pane),
        Some(virtual_window)
    );
    assert_eq!(virtual_app.state.buffers.len(), 1);
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
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let pane = app.state.tabs[app.state.active_tab.expect("tab")].focused;
    let editor = app
        .window_session
        .editor_ids()
        .window_for(pane)
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
        app.state.tabs[app.state.active_tab.expect("tab")].focused,
        pane
    );
    assert_eq!(app.window_session.graph().focused(), Some(editor));
    assert_eq!(app.active_input_context(), "editor.normal");
    app.handle_configured_key("i".parse::<KeyEvent>().expect("insert mode"));
    app.handle_configured_key("x".parse::<KeyEvent>().expect("insert text"));
    let view = app.state.panes[pane].view;
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
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
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
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::NewTab));
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
    let initial_count = app.window_session.editor_ids().reconciliation_count();
    let initial_graph = app.window_session.graph().clone();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        initial_count
    );
    assert_eq!(app.window_session.graph(), &initial_graph);

    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs[0].dirty);
    let after_dirty = app.window_session.editor_ids().reconciliation_count();

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
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        after_dirty + 1
    );

    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    let split_count = app.window_session.editor_ids().reconciliation_count();
    let first_pane = match &app.state.tabs[app.state.active_tab.expect("tab")].root {
        crate::editor::LayoutNode::Split { first, .. } => match first.as_ref() {
            crate::editor::LayoutNode::Pane(pane) => *pane,
            crate::editor::LayoutNode::Split { .. } => panic!("unexpected nested split"),
        },
        crate::editor::LayoutNode::Pane(_) => panic!("expected split"),
    };
    app.state.focus_pane_id(first_pane).expect("focus");
    app.reconcile_editor_windows();
    app.state.resize_active_pane(50).expect("resize");
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        split_count + 2
    );

    let before_reload = app.window_session.editor_ids().reconciliation_count();
    {
        let fixture = app.configuration.fixture_snapshot();
        app.configuration.install_fixture(fixture);
    }
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        before_reload
    );
    app.window_session
        .graph()
        .validate()
        .expect("revision-driven graph is valid");
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
            edge: crate::ui::window::DockEdge::Left,
            ..
        }
    ));
    let rect = app.explorer_window_rect().expect("projected explorer");
    // The status line keeps the last row of the screen.
    assert!(rect.width > 1 && rect.height == app.window_session.height() - 1);
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
            .process_effect(crate::app::AppEffect::TerminalToggle);
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
fn picker_title_changes_update_the_retained_window_content() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(PickerKind::Buffers);
    app.reconcile_editor_windows();
    let files = app.window_session.native_ids().picker().unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .content(files.content)
            .unwrap()
            .title,
        "Buffers"
    );
    app.open_builtin_picker(PickerKind::Commands);
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.native_ids().picker(), Some(files));
    assert_eq!(
        app.window_session
            .graph()
            .content(files.content)
            .unwrap()
            .title,
        "Commands"
    );
    app.process_effect(AppEffect::WindowList);
    assert!(
        app.ui
            .picker
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.label.contains("Commands"))
    );
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
        crate::ui::window::ContentLifecycle::Running
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

    app.process_effect(crate::app::AppEffect::WindowSwap {
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
    app.process_effect(crate::app::AppEffect::WindowSwap {
        first: "explorer".into(),
        second: "terminal".into(),
    });

    app.process_effect(crate::app::AppEffect::WindowFullscreen(
        "explorer".into(),
    ));
    let terminal_before = app
        .window_session
        .graph()
        .window(terminal.window)
        .expect("terminal")
        .placement;
    app.process_effect(crate::app::AppEffect::WindowFullscreen(
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

    app.process_effect(crate::app::AppEffect::WindowDock {
        target: "explorer".into(),
        edge: "left".into(),
        size: 31,
    });
    app.process_effect(crate::app::AppEffect::WindowHide("explorer".into()));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("hidden")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::app::AppEffect::WindowShow("explorer".into()));
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
            crate::app::NativeSurfaceTarget::Terminal(7),
            crate::editor::FocusDirection::Up
        )
        .is_err()
    );
    assert_eq!(app.window_session.terminal_height(), height_before);
    app.process_effect(crate::app::AppEffect::WindowHide("terminal".into()));
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
    app.process_effect(crate::app::AppEffect::WindowHide("terminal".into()));
    assert_eq!(app.window_session.graph(), &before_stale);
    assert!(app.ui.notification_error);
    app.reconcile_editor_windows();
    assert!(app.compositor_projection().is_some());
    let idle_graph = app.window_session.graph().clone();
    let idle_reconciliations = app.window_session.editor_ids().reconciliation_count();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.graph(), &idle_graph);
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        idle_reconciliations
    );

    app.process_effect(crate::app::AppEffect::WindowClose("explorer".into()));
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
fn persisted_window_rules_validate_the_final_fullscreen_configuration() {
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
        .accept_current(candidate, &app.state.window_version())
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

#[test]
fn semantic_window_layout_persistence_is_strict_transactional_and_id_free() {
    let (_source_dirs, mut source) = app();
    let path = source.state.workspace_root.join("editor.rs");
    source
        .state
        .open_text(TextBuffer::from_text("editor"), Some(path))
        .unwrap();
    source.set_dashboard_visibility(false);
    source.open_explorer();
    source.process_effect(crate::app::AppEffect::WindowDock {
        target: "explorer".into(),
        edge: "right".into(),
        size: 27,
    });
    let session = source.capture_session().expect("session");
    assert_eq!(session.window_rules.len(), 1);
    assert_eq!(session.window_rules[0].content_type, "builtin.explorer");
    let encoded = serde_json::to_string(&session.window_rules).expect("encode rules");
    assert!(!encoded.contains("content_id"));
    assert!(!encoded.contains("view_id"));
    assert!(!encoded.contains("window_id"));
    assert!(!encoded.contains("pty"));

    let (_restored_dirs, mut restored) = app();
    restored
        .state
        .open_text(TextBuffer::from_text("restored"), None)
        .unwrap();
    restored.set_dashboard_visibility(false);
    restored.open_explorer();
    let ids = restored
        .window_session
        .native_ids()
        .explorer()
        .expect("fresh runtime ids");
    restored
        .apply_persisted_window_rules(&session.window_rules)
        .expect("apply semantic rules");
    assert_eq!(
        restored
            .window_session
            .graph()
            .window(ids.window)
            .expect("restored")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 27,
        }
    );

    let before = restored.window_session.native_ids().semantic_defaults(None);
    let hostile = vec![
        PersistedWindowRule {
            content_type: "builtin.explorer".into(),
            placement: PersistedWindowPlacement::Fullscreen,
        },
        PersistedWindowRule {
            content_type: "unknown.surface".into(),
            placement: PersistedWindowPlacement::Hidden,
        },
    ];
    assert!(restored.apply_persisted_window_rules(&hostile).is_err());
    assert_eq!(
        restored.window_session.native_ids().semantic_defaults(None),
        before
    );
    assert_eq!(restored.window_session.native_ids().explorer(), Some(ids));

    let two_fullscreen = vec![
        PersistedWindowRule {
            content_type: "builtin.explorer".into(),
            placement: PersistedWindowPlacement::Fullscreen,
        },
        PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: PersistedWindowPlacement::Fullscreen,
        },
    ];
    let graph_before = restored.window_session.graph().clone();
    assert!(
        restored
            .apply_persisted_window_rules(&two_fullscreen)
            .is_err()
    );
    assert_eq!(restored.window_session.graph(), &graph_before);
    assert_eq!(
        restored.window_session.native_ids().semantic_defaults(None),
        before
    );

    let candidate = restored
        .prepare_configured_window_rules(&crate::config::WindowsConfig {
            rules: vec![crate::config::WindowRuleConfig {
                content_type: "builtin.dashboard".into(),
                placement: crate::config::WindowPlacementConfig::Fullscreen,
            }],
        })
        .expect("configured fullscreen candidate");
    restored
        .window_session
        .accept_current(candidate, &restored.state.window_version())
        .expect("one configured fullscreen");
    let merged_conflict = [PersistedWindowRule {
        content_type: "builtin.explorer".into(),
        placement: PersistedWindowPlacement::Fullscreen,
    }];
    let graph_before_merged = restored.window_session.graph().clone();
    let defaults_before_merged = restored.window_session.native_ids().semantic_defaults(None);
    assert!(
        restored
            .apply_persisted_window_rules(&merged_conflict)
            .is_err()
    );
    assert_eq!(restored.window_session.graph(), &graph_before_merged);
    assert_eq!(
        restored.window_session.native_ids().semantic_defaults(None),
        defaults_before_merged
    );
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
        .accept_current(candidate, &app.state.window_version())
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
    let mut foreign = crate::app::terminal::TerminalSession::new(
        &app.configuration.settings().terminals,
    );
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

    app.process_effect(crate::app::AppEffect::TerminalUnfocus);
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(!app.window_session.terminal_focused());
    assert!(app.ui.terminal_panel.is_some());
    assert_eq!(app.active_input_context(), "editor.normal");
    app.process_effect(crate::app::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    app.process_effect(crate::app::AppEffect::TerminalToggle);
    assert!(!app.window_session.terminal_focused());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::app::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    assert!(app.terminal_window_rect().is_some());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    app.process_effect(crate::app::AppEffect::TerminalUnfocus);

    app.process_effect(crate::app::AppEffect::TerminalFocus(2));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.window_session.terminal_focused());
    app.process_effect(crate::app::AppEffect::TerminalResize(5));
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
    app.process_effect(crate::app::AppEffect::TerminalCycle(-1));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_session.fixture_lines(PtyId(2))[0].contains("second"));

    app.process_effect(crate::app::AppEffect::TerminalResize(1));
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
    app.process_effect(crate::app::AppEffect::TerminalFloat(Some(
        crate::ui::window::WindowRect {
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
    app.process_effect(crate::app::AppEffect::WindowMove {
        target: "terminal".into(),
        x: 7,
        y: 4,
    });
    app.process_effect(crate::app::AppEffect::WindowResize {
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
    app.process_effect(crate::app::AppEffect::TerminalFullscreen);
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
    app.process_effect(crate::app::AppEffect::TerminalHide);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert!(app.terminal_session.contains(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::app::AppEffect::WindowShow("terminal".into()));
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
    app.process_effect(crate::app::AppEffect::TerminalDock {
        edge: "top".into(),
        size: 5,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 4));
    app.process_effect(crate::app::AppEffect::TerminalDock {
        edge: "right".into(),
        size: 10,
    });
    // The column left of a right dock is its border.
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (9, 18));
    app.process_effect(crate::app::AppEffect::TerminalDock {
        edge: "left".into(),
        size: 8,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (8, 18));
    app.process_effect(crate::app::AppEffect::TerminalDock {
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
    let first_editor_tab = app.state.active_tab.expect("first editor tab");
    let terminal_two_ids = app
        .window_session
        .native_ids()
        .terminal(2)
        .expect("second terminal resources");
    let terminal_state_before_rehost = (
        app.terminal_session.fixture_dimensions(PtyId(1)),
        app.terminal_session.fixture_lines(PtyId(1)),
    );
    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::NewTab,
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
    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::FocusTab(-1),
    ));
    assert_eq!(app.state.active_tab, Some(first_editor_tab));
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
    app.process_effect(crate::app::AppEffect::TerminalFocus(2));
    app.process_effect(crate::app::AppEffect::TerminalFloat(Some(
        crate::ui::window::WindowRect {
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
    app.process_effect(crate::app::AppEffect::TerminalFocus(1));

    let visible_placement = app
        .window_session
        .graph()
        .window(terminal_ids.window)
        .expect("terminal window")
        .placement;
    app.process_effect(crate::app::AppEffect::TerminalClose);
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

    app.process_effect(crate::app::AppEffect::TerminalClose);
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
    app.process_effect(crate::app::AppEffect::TerminalToggle);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );

    app.process_effect(crate::app::AppEffect::TerminalClose);
    assert!(app.terminal_session.select(PtyId(2)));
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(!app.terminal_session.contains(PtyId(1)));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.ui.terminal_panel.is_some());
    app.process_effect(crate::app::AppEffect::TerminalClose);
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
    app.process_effect(AppEffect::Agent(crate::app::AgentCommand::Ask));
    let agent = app.terminal_session.selected().unwrap();
    assert_ne!(shell, agent, "an ordinary shell is not reused as an agent");
    assert!(app.agent_terminal(agent));
    assert!(app.terminal_session.side(agent));
    app.process_effect(AppEffect::TerminalUnfocus);
    app.process_effect(AppEffect::Agent(crate::app::AgentCommand::Ask));
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
    app.process_effect(crate::app::AppEffect::TerminalUnfocus);
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
    app.process_effect(crate::app::AppEffect::TerminalUnfocus);
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
    app.process_effect(crate::app::AppEffect::TerminalUnfocus);
    app.process_effect(AppEffect::Agent(AgentCommand::Toggle));
    assert!(app.terminal_window_rect().is_none());
    assert_eq!(app.terminal_session.descriptions().len(), 1);
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
    assert!(driver.application_mut().state.active_view.is_some());
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
    app.process_effect(crate::app::AppEffect::TerminalFocus(agent.0));
    assert_eq!(app.window_session.focused_terminal(), Some(agent.0));
    assert!(app.terminal_rect(shell).is_some(), "the shell stays below");
    app.resize_focused_window(crate::editor::FocusDirection::Left, 1)
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
    app.process_effect(crate::app::AppEffect::TerminalResize(-1));
    assert_eq!(
        app.terminal_rect(agent).map(|rect| rect.width),
        Some(agent_rect.width)
    );
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

#[cfg(unix)]
#[test]
fn ctrl_arrows_in_a_lone_editor_resize_the_agent_docked_beside_it() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
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
    let start = app.terminal_rect(agent).expect("agent").width;

    // The editor has no split, so ctrl-left grows the code into the agent's side.
    driver.key("ctrl-left");
    let app = driver.application_mut();
    let step = crate::ui::render::TERMINAL_PANEL_RESIZE_STEP;
    assert_eq!(
        app.terminal_rect(agent).map(|rect| rect.width),
        Some(start + step)
    );
    assert_eq!(app.window_session.focused_terminal(), None);
    driver.key("ctrl-right");
    let app = driver.application_mut();
    assert_eq!(app.terminal_rect(agent).map(|rect| rect.width), Some(start));
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
    let retired_source = driver.terminal_geometry_source().unwrap();
    assert_eq!(driver.terminal_pending_resizes(), Some(1));
    assert_eq!(driver.terminal_screen_dimensions(), Some(before_rehost));
    driver.reset_terminal_window_runtime().unwrap();
    let identity = driver.terminal_window_identity().unwrap();
    let original_tab = driver.terminal_window_tab().unwrap();
    assert_ne!(identity, initial_identity);
    let current_source = driver.terminal_geometry_source().unwrap();
    assert_ne!(current_source, retired_source);
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
        "close before the terminal starts retires the intent"
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
                *id == debug.0 && *state == crate::ui::window::ContentLifecycle::Exited
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
fn retired_debugger_terminal_preflight_never_launches_the_program() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let mut program = loaded.config.terminals.profiles["shell"].clone();
    program.program = "/bin/sh".into();
    program.arguments = vec!["-c".into(), "printf started > retired-debug-started".into()];
    loaded
        .config
        .terminals
        .profiles
        .insert("retired-debug".into(), program);
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    let (_service, adapter) = crate::debugger::test_session();
    let (_request, reply) = adapter.terminal_reply();
    let app = driver.application_mut();
    app.request_terminal_open("retired-debug", Some(reply));
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
    assert!(!root.join("retired-debug-started").exists());
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

#[test]
fn a_late_result_updates_its_own_record_and_leaves_a_newer_draft_untouched() {
    use crate::debugger::DapControl;
    use crate::debugger::DapEvent;
    use crate::debugger::DapResponse;
    use crate::debugger::DapStackFrame;
    use crate::debugger::DapVariable;
    use crate::app::debugger::DebugResponse;
    use crate::app::debugger::tests::attach_adapter;

    let (_app_dirs, mut app) = app();
    let owner = &mut app.debugger_session;
    owner.begin_stop();
    let stop = owner.stop_epoch();
    let adapter = attach_adapter(owner);
    let DebugResponse::Picker { rows, .. } =
        owner.response(DapResponse::Stack(vec![DapStackFrame {
            id: 42,
            name: "main".into(),
            path: None,
            line: 1,
            column: 1,
            source_reference: None,
        }]))
    else {
        panic!("expected a frames picker");
    };
    assert_eq!(rows.len(), 1);
    let _frame = owner.take_selection(&rows[0].key).unwrap();

    // Leave an older request pending so the reply must be matched by request ID, not FIFO.
    app.process_effect(AppEffect::DapEvaluate("older".into()));
    app.process_effect(AppEffect::DapEvaluate("first".into()));
    let older_request = adapter.control().expect("the older evaluation was sent");
    assert!(matches!(
        &older_request,
        DapControl::Evaluate { expression, frame_id: Some(42), context }
            if expression == "older" && context == "repl"
    ));
    let request = adapter.control().expect("the evaluation was sent");
    assert!(matches!(
        &request,
        DapControl::Evaluate { expression, frame_id: Some(42), context }
            if expression == "first" && context == "repl"
    ));

    app.open_prompt(crate::ui::PromptKind::Command, "dap.evaluate ".into());
    app.prompt_insert("second");
    let draft = app.input.prompt().unwrap().clone();

    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_generation: 1,
            request_id: 2,
            request: Some(request),
            response: DapResponse::Evaluate(DapVariable {
                name: "result".into(),
                value: "1".into(),
                type_name: None,
                variables_reference: 0,
            }),
        },
    );
    app.process_dap_events();

    let entries = app.debugger_session.console().cloned().collect::<Vec<_>>();
    let first = entries.iter().find(|entry| entry.text == "first").unwrap();
    let older = entries.iter().find(|entry| entry.text == "older").unwrap();
    let answer = entries
        .iter()
        .find(|entry| entry.text == "result = 1")
        .unwrap();
    assert_eq!(first.actor.as_deref(), Some("same-name"));
    assert_eq!(first.stop, Some(stop));
    assert_eq!(first.frame, Some(42));
    assert!(first.submission.is_some());
    assert_ne!(first.submission, older.submission);
    assert_eq!(first.result.as_deref(), Some("result = 1"));
    assert_eq!(answer.submission, first.submission);
    assert_eq!(answer.actor, first.actor);
    assert_eq!(answer.stop, first.stop);
    assert_eq!(answer.frame, first.frame);
    assert_eq!(
        older.result, None,
        "the reply must not update the oldest request"
    );
    assert_eq!(entries.len(), 3, "the draft has not been submitted");

    let prompt = app.input.prompt().expect("the draft stays open");
    assert_eq!(prompt.kind, draft.kind);
    assert_eq!(prompt.input, "dap.evaluate second");
    assert_eq!(prompt.input, draft.input);
    assert_eq!(prompt.cursor, draft.cursor);

    // The older request remains independently answerable after the out-of-order reply.
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_generation: 1,
            request_id: 1,
            request: Some(older_request),
            response: DapResponse::Evaluate(DapVariable {
                name: "result".into(),
                value: "0".into(),
                type_name: None,
                variables_reference: 0,
            }),
        },
    );
    app.process_dap_events();
    let older = app
        .debugger_session
        .console()
        .find(|entry| entry.text == "older")
        .unwrap();
    assert_eq!(older.result.as_deref(), Some("result = 0"));
    let prompt = app
        .input
        .prompt()
        .expect("the draft stays open after both replies");
    assert_eq!(prompt.input, draft.input);
    assert_eq!(prompt.cursor, draft.cursor);
}

#[cfg(unix)]
#[test]
fn debugger_protected_launch_source_capture_refusal_and_fresh_recovery() {
    use crate::app::tests::driver::TestSession;
    let files = tempfile::tempdir().unwrap();
    let adapter = files.path().join("adapter.py");
    let starts = files.path().join("starts");
    let code = format!(
        "import pathlib\npathlib.Path({}).open('a').write('x')\n{}",
        serde_json::to_string(&starts).unwrap(),
        crate::debugger::TEST_ADAPTER
    );
    std::fs::write(&adapter, code).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.debug.adapters.insert(
        "owned-adapter".into(),
        crate::config::DebugAdapterConfig {
            program: crate::debugger::test_python(),
            arguments: vec!["-u".into(), adapter.display().to_string()],
            environment: Default::default(),
        },
    );
    loaded.config.debug.sessions.insert(
        "owned".into(),
        crate::config::DebugSessionConfig {
            adapter: "owned-adapter".into(),
            request: crate::config::DebugRequestKind::Launch,
            cwd: None,
            program: Some("debuggee".into()),
            run_in_terminal_profile: None,
            arguments: Default::default(),
            path_maps: Default::default(),
        },
    );
    let replacement = loaded.clone();
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.debug_program_fixture();
    driver.trust_owned_terminal_fixture();
    driver.debug_request("owned");
    driver.debug_stop();
    assert!(!starts.exists(), "stop before launch started the adapter");
    let drain = |driver: &mut TestSession| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while driver.debug_preflights() != 0 {
            driver.pump_debug();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    drain(&mut driver);
    assert!(!starts.exists(), "stopped request launched adapter");
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(!starts.exists(), "retired preflight launched adapter");
    // Configured adapters may run scripts; exercise executable preflight through a native tool.
    driver.debug_start_native_program_fixture();
    let program_deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_program_pending() {
        driver.pump_debug();
        assert!(
            Instant::now() < program_deadline,
            "program preflight not admitted: {:?}",
            driver.projection().notification
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(
        !starts.exists(),
        "retired program preflight launched adapter"
    );
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    driver.apply_candidate(replacement).unwrap();
    drain(&mut driver);
    assert!(
        !starts.exists(),
        "replacement configuration adopted old preflight"
    );
    driver.debug_start("owned");
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_text().is_none() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "stopped generated source did not open automatically"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.debug_source_text().as_deref(),
        Some("original adapter source\n")
    );
    driver.new_buffer();
    assert!(driver.debug_source_text().is_none());
    let picker = |driver: &mut TestSession, needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            driver.pump_debug();
            if driver
                .projection()
                .picker
                .as_ref()
                .is_some_and(|p| p.rows.iter().any(|row| row.label == needle))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "debug picker {needle}: {:?}",
                driver.projection().notification
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    driver.open_debug_threads_after_stop();
    picker(&mut driver, "original-thread");
    assert_eq!(std::fs::read_to_string(&starts).unwrap(), "x");
    assert!(
        driver.debug_program_wire_path(),
        "launch JSON lost original preflighted program"
    );
    driver.accept_debug_picker();
    picker(&mut driver, "original-frame");
    driver.accept_debug_picker();
    driver.open_command_prompt();
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_pending() {
        assert!(
            Instant::now() < deadline,
            "original source response not drained"
        );
        driver.pump_debug();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        driver.debug_source_text().is_none(),
        "late source stole command capture"
    );
    driver.close_prompt();
    driver.debug_threads();
    picker(&mut driver, "original-thread");
    driver.accept_debug_picker();
    picker(&mut driver, "original-frame");
    driver.accept_debug_picker();
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_text().is_none() {
        driver.pump_debug();
        assert!(Instant::now() < deadline, "fresh source did not publish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.debug_source_text().as_deref(),
        Some("original adapter source\n")
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_reverse_refusal() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "original reverse refusal did not reach owned adapter"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    driver.debug_stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while driver.debug_pending() != 0 {
        driver.pump_debug();
        assert!(Instant::now() < deadline, "owned adapter cleanup");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn session_restore_reads_more_files_than_the_document_queue_capacity() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    for index in 0..12 {
        if index > 0 {
            app.process_effect(AppEffect::Layout(crate::app::LayoutAction::NewTab));
        }
        let path = app
            .state
            .workspace_root
            .join(format!("restore-{index}.txt"));
        fs::write(&path, format!("file {index}\n")).unwrap();
        app.start_open(path);
        wait_vfs(&mut app);
    }
    let session = app.capture_session().unwrap();
    assert_eq!(session.tabs.len(), 12);
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    wait_vfs(&mut app);
    assert_eq!(app.ui.config_diagnostic, None);
    assert_eq!(app.state.tabs.len(), 12);
    assert!(app.persistence.next_restore_path().is_none());
    for index in 0..12 {
        let path = app
            .state
            .workspace_root
            .join(format!("restore-{index}.txt"));
        assert!(app.state.buffers.values().any(|buffer| {
            buffer.path.as_ref() == Some(&path)
                && buffer.text.to_string() == format!("file {index}\n")
        }));
    }
}

#[test]
fn session_restore_keeps_the_focused_split_when_paths_are_identical() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("same.txt");
    fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    app.set_dashboard_visibility(false);
    let first = app.state.active_view.unwrap();
    let second = app
        .state
        .split_active(crate::editor::Axis::Vertical, 500)
        .unwrap();
    app.state.views[first].cursor = ByteOffset(0);
    app.state.views[second].cursor = ByteOffset(11);
    app.reconcile_editor_windows();
    let session = app.capture_session().unwrap();
    assert_eq!(session.tabs[0].focused_pane, Some(1));
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    wait_vfs(&mut app);
    assert_eq!(app.ui.config_diagnostic, None);
    assert_eq!(
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(11)
    );
}

#[test]
fn session_restore_focuses_the_saved_pane_when_panes_share_a_file() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("shared-panes.txt");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    let first = app.state.active_view.unwrap();
    app.state.views[first].cursor = ByteOffset(1);
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .unwrap();
    let second = app.state.active_view.unwrap();
    assert_ne!(first, second);
    app.state.views[second].cursor = ByteOffset(8);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let session = app.capture_session().unwrap();
    assert_eq!(session.tabs[0].focused_pane, Some(1));
    // Exercise serialization as well as capture and asynchronous restoration.
    let session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    wait_vfs(&mut app);
    assert_eq!(app.ui.config_diagnostic, None);
    let active = app.state.active_view.unwrap();
    assert_eq!(app.state.views[active].cursor, ByteOffset(8));
    assert_eq!(app.state.views.len(), 2);
    assert!(
        app.state
            .views
            .values()
            .any(|view| view.cursor == ByteOffset(1))
    );
    assert_eq!(app.capture_session().unwrap().tabs[0].focused_pane, Some(1));
}

#[test]
fn session_restore_keeps_tab_order_after_the_first_tab_was_closed() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    let paths = ["one.txt", "two.txt", "three.txt"].map(|name| {
        let path = app.state.workspace_root.join(name);
        std::fs::write(&path, format!("{name}\n")).expect("fixture file");
        path
    });
    app.set_dashboard_visibility(false);
    app.start_open(paths[0].clone());
    wait_vfs(&mut app);
    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::NewTab,
    ));
    app.start_open(paths[1].clone());
    wait_vfs(&mut app);
    let session = app.capture_session().expect("captured session");

    // Closing the first tab frees the slot that a restore would otherwise fill last.
    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::FocusTab(-1),
    ));
    app.process_effect(crate::app::AppEffect::Layout(
        crate::app::LayoutAction::CloseTab,
    ));
    app.start_open(paths[2].clone());
    wait_vfs(&mut app);
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    wait_vfs(&mut app);

    assert_eq!(app.ui.config_diagnostic, None);
    let order = app
        .state
        .tabs
        .values()
        .map(|tab| {
            let view = app.state.panes[tab.focused].view;
            app.state.buffers[app.state.views[view].buffer].path.clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(order, vec![Some(paths[0].clone()), Some(paths[1].clone())]);
}

#[test]
fn semantic_content_restore_uses_fresh_ids_and_preserves_unclamped_desired_placement() {
    use crate::app::persistence::RestoreContext;
    use crate::persistence::PersistedWindowPlacement;

    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("descriptor-restore.txt");
    std::fs::write(&path, "restored descriptor\n").expect("fixture file");
    app.state
        .open_text(
            TextBuffer::from_text("restored descriptor\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let old_window = app.window_session.graph().focused().expect("old window");
    let old_view = app
        .window_session
        .graph()
        .window(old_window)
        .expect("old descriptor")
        .view;
    let old_content = app
        .window_session
        .graph()
        .view(old_view)
        .expect("old view")
        .content;

    let mut session = app.capture_session().expect("captured session");
    assert_eq!(session.content_descriptors.len(), 1);
    let desired = PersistedWindowPlacement::Floating {
        x: 90,
        y: 30,
        width: 73,
        height: 19,
    };
    session.content_descriptors[0].desired_placement = desired.clone();
    let encoded = serde_json::to_string(&session.content_descriptors).expect("descriptor JSON");
    for forbidden in [
        "content_id",
        "view_id",
        "window_id",
        "generation",
        "capabilities",
    ] {
        assert!(!encoded.contains(forbidden));
    }

    macro_rules! context {
        () => {
            RestoreContext {
                editor: &app.state,
                workspace: &app.workspace,
                input: &app.input,
                windows: &app.window_session,
                configuration: app.configuration.binding_revision(),
            }
        };
    }
    app.persistence.fixture_restore_source(context!());
    app.restore_session(session);
    wait_vfs(&mut app);

    let new_window = app.window_session.graph().focused().expect("fresh window");
    let new_descriptor = app
        .window_session
        .graph()
        .window(new_window)
        .expect("fresh descriptor");
    let new_view = new_descriptor.view;
    let new_content = app
        .window_session
        .graph()
        .view(new_view)
        .expect("fresh view")
        .content;
    assert!(new_window.0 > old_window.0);
    assert!(new_view.0 > old_view.0);
    assert!(new_content.0 > old_content.0);
    // The graph retains the unclamped desire while the compositor publishes a
    // bounded effective rectangle for rendering and input.
    assert_eq!(
        new_descriptor.placement,
        WindowPlacement::Floating(WindowRect {
            x: 90,
            y: 30,
            width: 73,
            height: 19,
        })
    );
    app.reconcile_editor_windows();
    assert_eq!(
        app.compositor_projection()
            .and_then(|projection| projection.window(new_window))
            .expect("clamped restored editor projection")
            .rect,
        WindowRect {
            x: 7,
            y: 5,
            width: 73,
            height: 19,
        }
    );
    assert_eq!(
        app.capture_session()
            .expect("recaptured")
            .content_descriptors[0]
            .desired_placement,
        desired
    );
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().expect("buffer")]
            .path
            .as_ref(),
        Some(&path)
    );
    app.window_session.graph().validate().expect("valid graph");
}

#[test]
fn malformed_content_descriptor_restore_is_atomic_before_io_or_publication() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("descriptor-hostile.txt");
    std::fs::write(&path, "preserved\n").expect("fixture file");
    app.state
        .open_text(TextBuffer::from_text("preserved\n"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut session = app.capture_session().expect("session");
    session.content_descriptors[0].adapter = "terminal-session".into();
    session.content_descriptors[0].content_type = "builtin.terminal".into();
    let state_before = app.state.version();
    let graph_before = app.window_session.graph().clone();
    let pending_before = app.persistence.pending();
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    assert_eq!(app.state.version(), state_before);
    assert_eq!(app.window_session.graph(), &graph_before);
    assert_eq!(app.persistence.pending(), pending_before);
    assert!(
        app.ui
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("forbidden content type"))
    );
}

#[test]
fn live_marks_follow_command_edits_before_saving_and_jumping() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("marked.txt");
    fs::write(&path, "abc").unwrap();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.views[view].cursor = ByteOffset(2);
    app.process_effect(AppEffect::MarkSet("a".into()));
    app.state.views[view].cursor = ByteOffset(0);
    app.handle_configured_key(KeyEvent::character('i'));
    // More edits than TextBuffer retains, without a save or lookup between them.
    for _ in 0..100 {
        app.handle_configured_key(KeyEvent::character('x'));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.save_persistence();
    assert_eq!(app.persistence.marks()[0].location.byte, 102);
    app.state.views[view].cursor = ByteOffset(0);
    app.handle_configured_key(KeyEvent::character('i'));
    for _ in 0..80 {
        app.handle_configured_key(KeyEvent::character('y'));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.process_effect(AppEffect::MarkJump("a".into()));
    wait_vfs(&mut app);
    assert_eq!(
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(182)
    );
    assert_eq!(app.persistence.marks()[0].location.anchor, 182);
}

#[test]
fn cancelled_session_restore_reads_are_discarded_before_a_later_restore() {
    use crate::app::persistence::RestoreContext;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("cancelled-restore.txt");
    fs::write(&path, "restored\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    app.set_dashboard_visibility(false);
    let session = app.capture_session().unwrap();
    for _ in 0..12 {
        app.persistence.fixture_restore_source(RestoreContext {
            editor: &app.state,
            workspace: &app.workspace,
            input: &app.input,
            windows: &app.window_session,
            configuration: app.configuration.binding_revision(),
        });
        app.restore_session(session.clone());
        app.cancel_session_restore();
        assert_eq!(app.documents.legacy_count(), 0);
        assert!(app.persistence.next_restore_path().is_none());
        wait_vfs(&mut app);
    }
    app.persistence.fixture_restore_source(RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    });
    app.restore_session(session);
    wait_vfs(&mut app);
    assert_eq!(app.ui.config_diagnostic, None);
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "restored\n");
}

#[test]
fn startup_plugin_publication_waits_for_session_restoration() {
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedPane;
    use crate::persistence::PersistedSession;
    use crate::persistence::PersistedState;
    use crate::persistence::PersistedTab;
    use crate::persistence::PersistenceService;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("restored.txt");
    fs::write(&path, "restored text\n").unwrap();
    let session = PersistedSession {
        roots: vec![app.state.workspace_root.clone()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 3,
                    anchor: 3,
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
    let saved = tempdir().unwrap();
    let state_path = saved.path().join("session.json");
    fs::write(
        &state_path,
        serde_json::to_vec(&PersistedState {
            session: Some(session),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let (worker, release) = PersistenceService::gated_for_test(1);
    app.persistence =
        crate::app::persistence::PersistenceSession::new(worker, state_path);
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.load_persistence();
    fixture.indexed(
        crate::extensions::index_manifest_contributions(
            &plugin_manifest("demo", "plugin.demo.test", "x y"),
            1,
            "startup".into(),
        )
        .unwrap(),
    );
    let revision = app.configuration.binding_revision();
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_none());
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.persistence.pending() {
        app.process_plugin_events();
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(app.persistence.restoring());
    app.process_plugin_events();
    assert_eq!(app.configuration.binding_revision(), revision);
    wait_vfs(&mut app);
    assert!(!app.persistence.restoring());
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "restored text\n"
    );
    assert_eq!(
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(3)
    );
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_some());
    assert!(app.configuration.binding_revision() > revision);
}

#[test]
fn restore_reads_preserve_newer_edits_capture_and_original_editor_identity() {
    use crate::app::persistence::RestoreContext;
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedLocation;
    use crate::persistence::PersistedMark;
    use crate::persistence::PersistedPane;
    use crate::persistence::PersistedSession;
    use crate::persistence::PersistedState;
    use crate::persistence::PersistedTab;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("restored.txt");
    std::fs::write(&path, "héllo\n").unwrap();
    let session = PersistedSession {
        roots: vec![app.state.workspace_root.clone()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 2,
                    anchor: 3,
                    scroll_line: 0,
                    scroll_column: 17,
                },
            },
            focused_path: Some(path.clone()),
            focused_pane: None,
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
        pinned: vec![path.clone()],
    };
    macro_rules! context {
        () => {
            RestoreContext {
                editor: &app.state,
                workspace: &app.workspace,
                input: &app.input,
                windows: &app.window_session,
                configuration: app.configuration.binding_revision(),
            }
        };
    }
    app.persistence.fixture_restore_source(context!());
    app.restore_session(session.clone());
    let (newer, view) = app
        .state
        .open_text(TextBuffer::from_text("newer unsaved"), None)
        .unwrap();
    app.state.buffers[newer].dirty = true;
    wait_vfs(&mut app);
    assert_eq!(app.state.active_view, Some(view));
    assert_eq!(app.state.buffers[newer].text.to_string(), "newer unsaved");
    assert_eq!(app.state.buffers.len(), 1);
    app.state.buffers[newer].dirty = false;
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.persistence.fixture_restore_source(context!());
    app.restore_session(session.clone());
    app.input.capture_changed();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_view, Some(view));
    let identity = app.state.window_version();
    let (manager, plugin) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = plugin.take();
    app.persistence.fixture_restore_source(context!());
    app.session_restore_notice = true;
    app.notify_message("restoring".into(), false);
    app.restore_session(session.clone());
    assert_eq!(app.ui.notification.as_deref(), Some("restoring"));
    wait_vfs(&mut app);
    assert_eq!(
        app.ui.notification.as_deref(),
        Some(app.message("session_restored", &[]).as_str())
    );
    plugin.reply_with(&invocation, |execution| {
        execution.messages.push("obsolete plugin reply".into());
    });
    app.process_plugin_events();
    assert!(
        !app.notices
            .history()
            .any(|(_, message)| message == "obsolete plugin reply")
    );
    let buffer = app.state.active_buffer_id().unwrap();
    assert!(!app.state.buffers.contains_key(newer));
    assert_ne!(buffer, newer);
    assert!(identity.same_editor(&app.state.window_version()));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "héllo\n");
    assert!(app.window_session.pins().contains(&buffer));
    assert_eq!(
        app.capture_session().unwrap().pinned.as_slice(),
        std::slice::from_ref(&path)
    );
    let restored = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(restored.cursor.get(), 1);
    assert!(restored.selection.is_none());
    assert_eq!(restored.scroll_column, 0); // default wrapping keeps the restored cursor visible
    app.state.validate_layout().unwrap();
    app.window_session.graph().validate().unwrap();
    app.open_explorer();
    let before = app.state.version();
    let graph_before = app.window_session.graph().clone();
    let defaults_before = app.window_session.native_ids().semantic_defaults(None);
    let mut missing = session;
    missing
        .window_rules
        .push(crate::persistence::PersistedWindowRule {
            content_type: "builtin.explorer".into(),
            placement: crate::persistence::PersistedWindowPlacement::Fullscreen,
        });
    if let PersistedLayout::Pane { pane } = &mut missing.tabs[0].layout {
        pane.path = app.state.workspace_root.join("missing.txt");
    }
    app.persistence.fixture_restore_source(context!());
    app.session_restore_notice = true;
    app.notify_message("waiting for missing file".into(), false);
    app.restore_session(missing);
    wait_vfs(&mut app);
    assert_ne!(
        app.ui.notification.as_deref(),
        Some(app.message("session_restored", &[]).as_str())
    );
    assert!(!app.session_restore_notice);
    assert_eq!(app.state.version(), before);
    assert_eq!(app.window_session.graph(), &graph_before);
    assert_eq!(
        app.window_session.native_ids().semantic_defaults(None),
        defaults_before
    );
    // A delayed metadata load cannot overwrite newer marks or recent ordering.
    let location = |byte| PersistedLocation {
        path: path.clone(),
        byte,
        anchor: byte,
        scroll_line: 0,
        scroll_column: 0,
    };
    app.persistence
        .set_mark("x".into(), location(3), 8, &app.state);
    let live = app.state.workspace_root.join("live.txt");
    app.persistence.record_recent(live.clone(), 8);
    let mut saved = PersistedState {
        marks: vec![PersistedMark {
            name: "x".into(),
            location: location(0),
        }],
        recent_files: vec![path],
        ..PersistedState::default()
    };
    app.persistence.restore_history(&mut saved);
    assert_eq!(
        app.persistence
            .marks()
            .iter()
            .find(|mark| mark.name == "x")
            .unwrap()
            .location
            .byte,
        3
    );
    assert_eq!(app.persistence.recent_files()[0], live);
}

fn logging_adapter_fixture(
    request: crate::config::DebugRequestKind,
) -> (tempfile::TempDir, PathBuf, crate::app::tests::driver::TestSession) {
    let files = tempfile::tempdir().unwrap();
    let adapter = files.path().join("logging-adapter.py");
    // The adapter runs in the workspace, not in the fixture directory.
    let log_path = files.path().join("requests.log");
    let source = crate::debugger::TEST_LOGGING_ADAPTER
        .replace("'requests.log'", &serde_json::to_string(&log_path).unwrap());
    fs::write(&adapter, source).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.debug.adapters.insert(
        "logging".into(),
        crate::config::DebugAdapterConfig {
            program: crate::debugger::test_python(),
            arguments: vec!["-u".into(), adapter.display().to_string()],
            environment: Default::default(),
        },
    );
    let attach = matches!(request, crate::config::DebugRequestKind::Attach);
    loaded.config.debug.sessions.insert(
        if attach { "attached" } else { "logging" }.into(),
        crate::config::DebugSessionConfig {
            adapter: "logging".into(),
            request,
            cwd: None,
            program: (!attach).then(|| "debuggee".into()),
            run_in_terminal_profile: None,
            arguments: if attach {
                BTreeMap::from([("processId".into(), serde_json::json!(4242))])
            } else {
                BTreeMap::new()
            },
            path_maps: Default::default(),
        },
    );
    let mut driver = crate::app::tests::driver::TestSession::new(loaded);
    driver.new_buffer();
    if !attach {
        driver.debug_program_fixture();
    }
    driver.trust_owned_terminal_fixture();
    (files, log_path, driver)
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
fn hiding_an_inspector_sends_no_disconnect_and_leaves_the_session_running() {
    // W-25 acceptance, verbatim: "proof that hiding inspectors does not terminate a session".
    // The stronger half is MEASURED: the adapter itself records every request it receives, so the
    // test can show that hiding sent no disconnect and no terminate, which "still looks alive"
    // could never distinguish.
    let (_files, log_path, mut driver) =
        logging_adapter_fixture(crate::config::DebugRequestKind::Launch);

    // The start sequence the passing debugger test uses: start, then PUMP until the adapter's
    // answer arrives as a picker.
    driver.debug_start("logging");
    driver.open_debug_threads_after_stop();
    assert!(
        driver.application().debugger_session.is_running(),
        "the session is running before hiding"
    );

    // HIDE: close the surface, which runs the session's own dismiss_choices path.
    driver.application_mut().close_picker();
    let deadline = Instant::now() + Duration::from_secs(6);
    while driver.projection().picker.is_some() {
        driver.pump_debug();
        assert!(Instant::now() < deadline, "the inspector never closed");
        std::thread::sleep(Duration::from_millis(2));
    }

    // The session SURVIVED hiding.
    assert!(
        driver.application().debugger_session.is_running(),
        "hiding an inspector must not terminate the session"
    );
    // A fresh threads response acknowledges a request ordered after hiding.
    // The adapter has consumed any earlier disconnect before this response.
    driver.debug_threads();
    let deadline = Instant::now() + Duration::from_secs(6);
    while driver.projection().picker.is_none() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "the post-hide threads request was not acknowledged"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(driver.application().debugger_session.is_running());
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("launch"),
        "the adapter really ran and logged its requests: {log:?}"
    );
    let stopping: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("disconnect") || line.starts_with("terminate"))
        .collect();
    assert!(
        stopping.is_empty(),
        "hiding must send NO disconnect and NO terminate, but the adapter received {stopping:?}"
    );
}

#[test]
fn a_started_session_makes_the_debug_arrangement_appear_with_its_own_identity() {
    let (_files, _log_path, mut driver) =
        logging_adapter_fixture(crate::config::DebugRequestKind::Launch);

    assert!(
        driver.application().ui.debug_panes.is_none(),
        "no session, no arrangement: the panes are absent before anything starts"
    );

    driver.debug_start("logging");
    driver.open_debug_threads_after_stop();

    // (1) THE ARRANGEMENT APPEARED, projected on the debug pump, naming the session's OWN identity.
    let panes = driver
        .application()
        .ui
        .debug_panes
        .as_ref()
        .expect("starting a session makes the arrangement appear");
    assert!(
        panes.target.contains("logging"),
        "the panes name the adapter that is running: {}",
        panes.target
    );
    assert!(
        panes.target.contains("launched"),
        "a LAUNCH is named as a launch, not as an attachment: {}",
        panes.target
    );

    // (2) THE SELECTION IS SWITCHABLE by the command, which is what "selectable inspectors" means.
    let before = panes.selected;
    driver.application_mut().invoke_mapped(
        crate::editor::CommandInvocation::new(
            "dap.inspector.next",
            crate::editor::InvocationSource::Test,
        )
        .expect("static command"),
    );
    let after = driver
        .application()
        .ui
        .debug_panes
        .as_ref()
        .expect("the arrangement survives the switch")
        .selected;
    assert_ne!(before, after, "the command selects the NEXT inspector");

    // THE LAUNCH COUNTER-CASE: this session LAUNCHED, so its status must not claim attachment.
    driver.application_mut().invoke_mapped(
        crate::editor::CommandInvocation::new(
            "dap.session.status",
            crate::editor::InvocationSource::Test,
        )
        .expect("static command"),
    );
    let status = driver
        .application()
        .ui
        .notification
        .clone()
        .unwrap_or_default();
    assert!(
        status.contains("debug adapter: logging"),
        "the launch status names the adapter: {status:?}"
    );
    assert!(
        !status.contains("attached to"),
        "a LAUNCH must not claim to be attached to anything: {status:?}"
    );
}

#[test]
fn an_attached_session_names_the_target_it_was_given_in_its_status() {
    // W-24's residual: the attach status was wired but unasserted. The launch counter-case lives
    // in `a_started_session_makes_the_debug_arrangement_appear_with_its_own_identity`, so a single
    // branch cannot satisfy both.
    let (_files, _log_path, mut driver) =
        logging_adapter_fixture(crate::config::DebugRequestKind::Attach);
    driver.debug_start("attached");
    driver.open_debug_threads_after_stop();

    driver.application_mut().invoke_mapped(
        crate::editor::CommandInvocation::new(
            "dap.session.status",
            crate::editor::InvocationSource::Test,
        )
        .expect("static command"),
    );
    let status = driver
        .application()
        .ui
        .notification
        .clone()
        .unwrap_or_default();
    assert!(
        status.contains("attached to processId 4242"),
        "an attached session names the TARGET it was given: {status:?}"
    );
    assert!(
        status.contains("debug adapter: logging"),
        "and it still names the adapter: {status:?}"
    );
    assert!(
        !status.contains("launched"),
        "an attach must not be described as a program the editor launched: {status:?}"
    );
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
    let origin_tab = app.state.active_tab.expect("editor tab");
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let origin_view = app.state.active_view.expect("split view");
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Zoom));
    assert!(app.state.is_zoomed());
    assert_ne!(app.state.active_tab, Some(origin_tab));
    let zoomed = app.state.active_view.unwrap();
    app.state.views[zoomed].cursor = crate::editor::text::ByteOffset(4);
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Zoom));
    assert!(!app.state.is_zoomed());
    assert_eq!(app.state.active_tab, Some(origin_tab));
    assert_eq!(app.state.active_view, Some(origin_view));
    assert_eq!(app.state.tabs.len(), 1);
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

    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Zoom));
    let zoomed_tab = app.state.active_tab;
    app.process_effect(AppEffect::Zen);
    app.process_effect(AppEffect::Zen);
    assert!(
        app.state.is_zoomed(),
        "zen must preserve a pre-existing zoom"
    );
    assert_eq!(app.state.active_tab, zoomed_tab);
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Zoom));

    let wrap = app.configuration.settings().editor.wrap;
    app.process_effect(AppEffect::ViewToggle(
        crate::app::ViewToggle::Wrap,
    ));
    assert_eq!(app.configuration.settings().editor.wrap, !wrap);
    app.process_effect(AppEffect::ViewToggle(
        crate::app::ViewToggle::LineNumbers,
    ));
    app.process_effect(AppEffect::ViewToggle(
        crate::app::ViewToggle::LineNumbers,
    ));
    assert_eq!(app.configuration.settings().editor.line_numbers, numbers);
}

#[test]
fn an_agent_edit_of_a_file_with_unsaved_changes_warns_in_its_approval() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("draft.txt");
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("draft"),
            Some(path.clone()),
        )
        .unwrap();
    let root = app.state.workspace_root.clone();
    let buffers = vec![(buffer, path.clone())];
    let changes = |request: &serde_json::Value| {
        crate::agent::permission_changes(request)
            .iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>()
    };
    let request =
        serde_json::json!({"toolCall": {"kind": "edit", "rawInput": {"file_path": path}}});
    assert_eq!(
        app.unsaved_agent_changes(&changes(&request), &buffers),
        "",
        "a clean file needs no warning"
    );
    app.state.buffers[buffer].dirty = true;
    assert_eq!(
        app.unsaved_agent_changes(&changes(&request), &buffers),
        "! draft.txt has unsaved changes; this changes the saved copy\n"
    );
    let relative =
        serde_json::json!({"toolCall": {"kind": "edit", "locations": [{"path": "draft.txt"}]}});
    assert!(
        app.unsaved_agent_changes(&changes(&relative), &buffers)
            .contains("draft.txt")
    );
    let read = serde_json::json!({"toolCall": {"kind": "read", "rawInput": {"file_path": path}}});
    assert_eq!(
        app.unsaved_agent_changes(&changes(&read), &buffers),
        "",
        "a read changes nothing"
    );
}

#[test]
fn a_name_no_worktree_matches_is_the_name_of_a_new_one() {
    let (_app_dirs, mut app) = app();
    let new_row = crate::workspace::PickerItem {
        badge: String::new(),
        kind: crate::workspace::PickerItemKind::Entry,
        id: crate::workspace::PickerItemId("git-worktree-new".into()),
        label: "New worktree…".into(),
        detail: String::new(),
        source: String::new(),
        action: crate::workspace::PickerAction::None,
    };
    let payload = crate::app::search::PickerPayload::Command(
        "workspace.git.worktree.create".into(),
    );
    app.install_picker("Git worktrees".into(), vec![(new_row, payload)]);
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "the row itself is selected"
    );
    for key in ["x", "y", "z"] {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
    assert_eq!(app.search.new_worktree_name().as_deref(), Some("xyz"));
    app.install_picker("Files".into(), Vec::new());
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "only the worktree list creates"
    );
}

#[test]
fn a_saved_location_reads_as_file_and_line() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("marked-location.txt");
    std::fs::write(&path, "one\nline two\nline three\n").unwrap();
    let location = |byte| crate::persistence::PersistedLocation {
        path: path.clone(),
        byte,
        anchor: byte,
        scroll_line: 0,
        scroll_column: 0,
    };
    assert!(
        app.persisted_location_label(&location(13))
            .ends_with("marked-location.txt:3")
    );
    assert!(
        app.persisted_location_label(&location(0))
            .ends_with("marked-location.txt:1")
    );
    std::fs::write(&path, b"\xef\xbb\xbfa\r\nb\r\nc\r\n").unwrap();
    assert!(
        app.persisted_location_label(&location(4))
            .ends_with("marked-location.txt:3")
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        app.persisted_location_label(&location(13))
            .ends_with("marked-location.txt")
    );
    let _ = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\nline two\nline three\n"),
            Some(path.clone()),
        )
        .unwrap();
    assert!(
        app.persisted_location_label(&location(5))
            .ends_with("marked-location.txt:2")
    );
}

#[test]
fn buffer_cycling_follows_the_buffer_line_order() {
    let (_app_dirs, mut app) = app();
    let mut buffers = Vec::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        let path = app.state.workspace_root.join(name);
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(name), Some(path))
            .unwrap();
        buffers.push(buffer);
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.project_window_content_tabs();
    app.process_effect(AppEffect::MoveBufferLine(-1));
    app.process_effect(AppEffect::MoveBufferLine(-1));
    app.project_window_content_tabs();
    assert_eq!(
        app.window_session.content_tab_buffers(),
        [buffers[2], buffers[0], buffers[1]]
    );
    // c is first on the line now, so next goes to a, then b, then wraps to c.
    for (offset, expected) in [(1, 0), (1, 1), (1, 2), (-1, 1)] {
        app.process_effect(AppEffect::CycleBuffer(offset));
        assert_eq!(app.state.active_buffer_id(), Some(buffers[expected]));
    }
}

#[test]
fn a_restored_session_keeps_the_buffer_line_in_window_order() {
    use crate::app::persistence::RestoreContext;

    let (_app_dirs, mut app) = app();
    let zeta = app.state.workspace_root.join("zeta.txt");
    let alpha = app.state.workspace_root.join("alpha.txt");
    std::fs::write(&zeta, "zeta\n").expect("zeta");
    std::fs::write(&alpha, "alpha\n").expect("alpha");
    app.state
        .open_text(TextBuffer::from_text("zeta\n"), Some(zeta.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.state
        .open_text(TextBuffer::from_text("alpha\n"), Some(alpha.clone()))
        .unwrap();
    app.reconcile_editor_windows();
    let session = app.capture_session().expect("captured session");
    let context = RestoreContext {
        editor: &app.state,
        workspace: &app.workspace,
        input: &app.input,
        windows: &app.window_session,
        configuration: app.configuration.binding_revision(),
    };
    app.persistence.fixture_restore_source(context);
    app.restore_session(session);
    wait_vfs(&mut app);
    app.project_window_content_tabs();
    let order = app
        .window_session
        .content_order()
        .iter()
        .filter_map(|buffer| app.state.buffers.get(*buffer)?.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, [zeta, alpha]);
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

#[test]
fn remote_files_outside_the_workspace_open_writable_at_the_requested_line() {
    let (_app_dirs, mut app) = app();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let server = crate::remote::RemoteServer::start(directory.path()).unwrap();
    crate::remote::send(server.path(), std::slice::from_ref(&path), Some(2)).unwrap();
    app.remote = Some(server);
    for _ in 0..200 {
        app.process_remote_opens();
        app.process_vfs_results();
        if app
            .state
            .active_buffer_id()
            .is_some_and(|buffer| app.state.buffers[buffer].path.as_ref() == Some(&path))
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let view = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
    assert!(!app.state.buffers[view.buffer].read_only);
    assert_eq!(view.cursor.get(), 4);
}

#[test]
fn nested_border_drag_uses_the_border_axis_on_both_sides() {
    fn drag(right: bool) -> u16 {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("top"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.state
            .split_active(crate::editor::Axis::Vertical, 500)
            .unwrap();
        app.state
            .split_active(crate::editor::Axis::Horizontal, 500)
            .unwrap();
        app.handle_event(UiEvent::Resize(120, 40));
        app.reconcile_editor_windows();
        let projection = app.compositor_projection().unwrap();
        let border = projection
            .borders
            .iter()
            .find(|border| {
                border.axis == crate::ui::window::TileAxis::Vertical && border.length > 100
            })
            .unwrap();
        let (x, y) = (
            border.x + border.length * if right { 3 } else { 1 } / 4,
            border.y,
        );
        app.handle_event(UiEvent::Mouse {
            key: "mouse-left-down".parse().unwrap(),
            column: x,
            row: y,
        });
        app.handle_event(UiEvent::Mouse {
            key: "mouse-left-drag".parse().unwrap(),
            column: x,
            row: y + 3,
        });
        match &app.state.tabs[app.state.active_tab.unwrap()].root {
            crate::editor::LayoutNode::Split { ratio, .. } => {
                assert!(
                    *ratio > 500,
                    "dragging down must enlarge the top pane (right={right}): {ratio}"
                );
                *ratio
            }
            _ => panic!("split"),
        }
    }
    assert_eq!(drag(false), drag(true));
}

#[test]
fn clipped_dirty_close_cannot_save_or_discard() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("unsaved"), None)
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((80, 24));
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.window_session.observe_size((10, 3));
    for choice in [
        crate::ui::ConfirmationChoice::No,
        crate::ui::ConfirmationChoice::Yes,
    ] {
        app.input.select_confirmation(choice);
        app.submit_prompt();
        assert!(app.state.buffers.contains_key(buffer));
        assert!(app.input.prompt().is_some());
    }
    app.close_prompt();
    assert!(app.input.prompt().is_none());
}

#[test]
fn recovered_missing_file_can_be_saved_without_force() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let path = app.state.workspace_root.join("deleted.txt");
    let journal = app.recovery.directory().join("deleted.toml");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(&journal, "durable copy").unwrap();
    app.recovery.fixture_records(vec![(
        journal.clone(),
        crate::files::RecoveryRecord::new(Some(&path), 1, "recovered\n".into()),
    )]);
    app.open_recovery_picker();
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "recovered\n");
    assert!(app.state.buffers[buffer].dirty);
    assert!(app.state.buffers[buffer].new_file);
    assert!(journal.exists());
    app.invoke_mapped_command("file.save");
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"recovered\n");
    assert!(!app.state.buffers[buffer].dirty);
    assert!(!app.state.buffers[buffer].new_file);
}

#[test]
fn recovered_scratch_keeps_its_unsaved_baseline_and_durable_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let journal = app.recovery.directory().join("old-scratch.toml");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let record = crate::files::RecoveryRecord::new(None, 1, "recovered".into());
    crate::files::write_recovery(&journal, &record).unwrap();
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert!(discovery.diagnostics.is_empty());
    assert_eq!(discovery.records, vec![(journal.clone(), record.clone())]);
    app.recovery.fixture_records(discovery.records);
    app.open_recovery_picker();
    app.accept_picker();
    let buffer = app.state.active_buffer_id().unwrap();
    wait_vfs(&mut app);
    assert_eq!(
        crate::files::discover_recovery(app.recovery.directory()).records,
        vec![(journal.clone(), record)],
    );
    assert_eq!(app.recovery.path(buffer), journal);
    app.replace_with_recovered(buffer, "changed".into())
        .unwrap();
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "recovered");
    assert!(app.state.buffers[buffer].dirty);
    app.schedule_recovery_work();
    // Flush the debounced write as well as any queued deletion before checking
    // what a fresh editor would discover after a restart.
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert!(discovery.diagnostics.is_empty());
    assert_eq!(
        discovery.records,
        vec![(
            journal,
            crate::files::RecoveryRecord::new(
                None,
                app.state.buffers[buffer].text.revision().0,
                "recovered".into(),
            ),
        )]
    );
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "");
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
