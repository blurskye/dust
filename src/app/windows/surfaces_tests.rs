use crate::app::tests::*;

use crate::app::debugger::surfaces_tests::logging_adapter_fixture;


#[test]
pub(in crate::app) fn placement_swap_moves_editing_focus_to_the_visible_editor() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("oldest"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let (_, first_view) = app
        .state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    let first_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
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
pub(in crate::app) fn fullscreen_editor_swap_moves_editing_focus_to_the_fullscreen_recipient() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("first"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let first_pane = app.state.tabs[app.state.active_tab.unwrap()].focused;
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
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
pub(in crate::app) fn fullscreen_placement_swaps_keep_graph_and_editor_focus_together() {
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
pub(in crate::app) fn tiled_window_resize_accepts_the_minimum_delta() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.resize_graph_window("focused", i16::MIN).unwrap();
    assert!(matches!(
        app.state.tabs[app.state.active_tab.unwrap()].root,
        crate::editor::LayoutNode::Split { ratio: 900, .. }
    ));
}


#[test]
pub(in crate::app) fn nonwrapped_mouse_uses_the_same_snapped_tab_origin_as_rendering() {
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
pub(in crate::app) fn mouse_click_focuses_the_real_target_pane_before_placing_cursor() {
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
pub(in crate::app) fn universal_window_clone_refuses_native_noncloneable_content_without_mutation() {
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
pub(in crate::app) fn window_clone_rejects_an_exhausted_editor_source_before_graph_mutation() {
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
pub(in crate::app) fn ordinary_read_only_files_can_reload_without_becoming_writable() {
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
pub(in crate::app) fn shared_dirty_buffers_do_not_block_view_only_tab_close_or_forced_buffer_close() {
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
    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
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
pub(in crate::app) fn exact_dirty_window_close_rejects_stale_focus_then_discards_only_its_target() {
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
pub(in crate::app) fn tab_close_reviews_every_last_dirty_reference_before_one_publication() {
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
    let tabs = app.state.tabs.len();

    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
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
    app.apply_layout_action(crate::app::windows::LayoutAction::CloseTab);
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
pub(in crate::app) fn editor_lifecycle_reconciles_into_window_graph_and_drives_content_tabs() {
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
pub(in crate::app) fn editor_layout_commands_commit_state_bridge_and_graph_as_one_authoritative_transaction() {
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
        crate::app::windows::LayoutAction::Split(crate::editor::Axis::Horizontal),
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
    assert_eq!(app.state.panes.len(), 2);

    app.process_effect(crate::app::commands::AppEffect::WindowClose(format!(
        "window:{}",
        focused.0
    )));
    assert_eq!(app.state.panes.len(), 1);
    assert!(app.window_session.graph().window(initial_window).is_some());
    let state_revision_before_rejected = app.state.window_revision();
    let active_view_before_rejected = app.state.active_view;
    let pane_count_before_rejected = app.state.panes.len();
    let graph_before_rejected = app.window_session.graph().clone();
    app.process_effect(crate::app::commands::AppEffect::WindowClose("focused".into()));
    assert_eq!(app.state.window_revision(), state_revision_before_rejected);
    assert_eq!(app.state.active_view, active_view_before_rejected);
    assert_eq!(app.state.panes.len(), pane_count_before_rejected);
    assert_eq!(app.window_session.graph(), &graph_before_rejected);
    assert!(app.ui.notification_error);
}


#[test]
pub(in crate::app) fn overlapping_mixed_floats_paint_and_focus_in_compositor_z_order() {
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
pub(in crate::app) fn editor_window_swap_rejects_mixed_and_cross_tab_targets_atomically() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
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
pub(in crate::app) fn editor_window_revision_skips_idle_rebuilds_and_tracks_projection_changes() {
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
pub(in crate::app) fn generic_native_window_operations_are_revision_checked_atomic_and_identity_preserving() {
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
    app.process_effect(crate::app::commands::AppEffect::WindowHide("explorer".into()));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("hidden")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::app::commands::AppEffect::WindowShow("explorer".into()));
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
            crate::app::windows::NativeSurfaceTarget::Terminal(7),
            crate::editor::FocusDirection::Up
        )
        .is_err()
    );
    assert_eq!(app.window_session.terminal_height(), height_before);
    app.process_effect(crate::app::commands::AppEffect::WindowHide("terminal".into()));
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
    app.process_effect(crate::app::commands::AppEffect::WindowHide("terminal".into()));
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

    app.process_effect(crate::app::commands::AppEffect::WindowClose("explorer".into()));
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
pub(in crate::app) fn configured_window_rule_candidate_is_cloned_validated_and_published_atomically() {
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
pub(in crate::app) fn a_stop_fills_the_panes_and_makes_room_for_them_without_opening_anything() {
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
pub(in crate::app) fn zoom_and_zen_show_the_pane_alone_and_put_everything_back() {
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
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let origin_view = app.state.active_view.expect("split view");
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
    assert!(app.state.is_zoomed());
    assert_ne!(app.state.active_tab, Some(origin_tab));
    let zoomed = app.state.active_view.unwrap();
    app.state.views[zoomed].cursor = crate::editor::text::ByteOffset(4);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
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

    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Zoom));
    let zoomed_tab = app.state.active_tab;
    app.process_effect(AppEffect::Zen);
    app.process_effect(AppEffect::Zen);
    assert!(
        app.state.is_zoomed(),
        "zen must preserve a pre-existing zoom"
    );
    assert_eq!(app.state.active_tab, zoomed_tab);
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
use crate::app::language::LspLaunch;
use crate::app::language::LspRuntimeState;
use crate::app::windows::NativeSurfaceTarget;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::editor::FocusDirection;
use crate::process::PtyId;
use crate::tools::discover_executable;
use crate::ui::KeyInputKind;
use crate::ui::PromptKind;
use crate::ui::UiEvent;
use crate::ui::editor_state_gutter_width;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::collections::HashMap;
use std::fs;
use std::thread;
use crate::ui::PtyScreen;
use crate::ui::active_pane_viewport;
use crate::ui::render;
use crate::app::AppEffect;

