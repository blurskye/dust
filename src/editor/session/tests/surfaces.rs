use super::*;

#[test]
fn prompt_mouse_context_is_independently_configurable() {
    let mut app = app();
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
    let mut app = app();
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
        .open_text(crate::editor::text::TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.ui.picker = Some(crate::ui::PickerUiState {
        input_hint: String::new(),
        surface: crate::ui::PickerSurfaceKind::Generic,
        title: "Search".into(),
        query: "needle".into(),
        rows: Vec::new(),
        selected: None,
        scroll: 0,
        loading: false,
        preview: None,
    });
    app.reconcile_editor_windows();
    let picker = app
        .window_session
        .native_ids()
        .picker()
        .expect("picker resources");
    let expected = crate::ui::picker_geometry(
        app.configuration.settings(),
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
    )
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
    let mut app = app();
    app.notify_message("first".into(), false);
    app.update_notification_timer();
    app.notify_message("second".into(), true);
    app.update_notification_timer();
    app.invoke_mapped_command("application.notifications.open");
    let picker = app.ui.picker.as_ref().expect("notification history");
    assert_eq!(picker.title, "Notifications");
    assert_eq!(picker.rows.len(), 2);
    assert_eq!(picker.rows[0].label, "second");
    assert_eq!(picker.rows[0].detail, "error");
    assert_eq!(picker.rows[1].label, "first");
}

#[test]
fn live_logs_and_output_refresh_preserve_picker_normal_without_leaking_it_to_new_sources() {
    let mut app = app();
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
fn problems_logs_output_and_debug_keep_distinct_graph_surface_identities() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
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
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    app.reconcile_editor_windows();
    let geometry = crate::ui::prompt_geometry(
        app.window_session.width(),
        app.workspace_render_height(),
        &app.configuration.settings().ui.command_line,
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
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
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
    let mut app = app();
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
        .open_text(crate::editor::text::TextBuffer::from_text("a\tb"), None);
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
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
    let first = app.state.active_view.expect("first view");
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    assert_ne!(app.state.active_view, Some(first));
    app.reconcile_editor_windows();
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
    app.apply_mouse_cursor(false);
    assert_eq!(app.state.active_view, Some(first));
    assert_eq!(
        app.state.views[first].cursor,
        crate::editor::text::ByteOffset(1)
    );
}

#[test]
fn health_virtual_dogfood_opens_refreshes_and_remains_read_only() {
    let mut app = app();
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
    assert!(app.ui.content_tabs.iter().any(|entry| entry.icon == "V"
        && entry.closeable
        && entry.kind == ContentTabKind::VirtualDocument));
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
        .fixture_restore_source(crate::editor::session::persistence::RestoreContext {
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
    let mut app = app();
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
    app.state = draft.accept(&app.state.version()).unwrap();
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
    app.state = draft.accept(&app.state.version()).unwrap();
    app.reconcile_editor_windows();
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
    let mut app = app();
    let (buffer, original_view) = app
        .state
        .open_text(TextBuffer::from_text("αbc\nline"), None);
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

    let mut virtual_app = super::app();
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);

    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::editor::session::terminal::TerminalFixture {
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

    let mut closed_app = super::app();
    closed_app
        .state
        .open_text(TextBuffer::from_text("close then clone"), None);
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("seed"), None);
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
    let mut app = app();
    let (buffer, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("virtual"), None);
    app.state.buffers[buffer].read_only = true;
    app.state.buffers[buffer].origin = crate::editor::BufferOrigin::VirtualReadOnly {
        owner: "demo".into(),
        generation: 7,
        document: 9,
        revision: 1,
    };
    let before = app.documents.legacy_count();
    app.start_save(
        false,
        Some(app.state.workspace_root.join("forbidden")),
        false,
    );
    app.start_reload(true);
    assert_eq!(app.documents.legacy_count(), before);
    app.state.buffers[buffer].dirty = true;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.cleanup_enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.schedule_recovery_work();
    assert!(!app.recovery.scheduled().contains_key(&buffer));
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.capture_session().is_none());
}

#[test]
fn configured_file_format_policy_is_applied_at_save_boundary() {
    let text = crate::editor::text::TextBuffer::from_bytes(b"a\r\nb").expect("CRLF text");
    let configured = crate::config::EditorConfig {
        line_endings: crate::config::LineEndingPolicy::Lf,
        final_newline: crate::config::FinalNewlinePolicy::Insert,
        utf8_bom: crate::config::BomPolicy::Always,
        ..crate::config::EditorConfig::default()
    };
    assert_eq!(
        super::configured_file_bytes(&text, &configured),
        b"\xEF\xBB\xBFa\nb\n"
    );
}

#[test]
fn dirty_layout_subtrees_are_blocked_and_forced_buffer_close_remains_reachable() {
    let mut app = app();
    let (dirty, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None);
    app.set_dashboard_visibility(false);
    app.state.buffers[dirty].dirty = true;
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("shared split");
    app.state.new_tab().expect("shared tab");
    let tab_count = app.state.tabs.len();
    app.apply_layout_action(crate::editor::LayoutAction::CloseTab);
    assert_eq!(
        app.state.tabs.len(),
        tab_count,
        "dirty tab close is blocked"
    );
    app.state
        .validate_layout()
        .expect("blocked layout remains valid");

    app.close_active_buffer(true);
    assert!(app.state.buffers.contains_key(dirty));
    assert!(app.state.buffers[dirty].dirty);
    app.state
        .cycle_buffer(1, true)
        .expect("reopen forced-close buffer");
    assert_eq!(app.state.active_buffer_id(), Some(dirty));
    app.state.validate_layout().expect("reopened layout valid");
}

#[test]
fn dashboard_exposes_and_dispatches_the_file_explorer_without_picker_fallback() {
    let mut app = app();
    app.project_configuration();
    assert!(app.ui.dashboard);
    assert!(
        app.ui.dashboard_actions.iter().any(|action| {
            action.label == "File explorer" && action.key.as_deref() == Some("f")
        }),
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
    let app = app();
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
fn command_and_search_histories_have_independent_configured_bounds() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.history.command.maximum_entries = 1;
        fixture.settings.history.search.maximum_entries = 2;
        app.configuration.install_fixture(fixture);
    }
    app.record_prompt_history(crate::ui::PromptKind::Command, "first");
    app.record_prompt_history(crate::ui::PromptKind::Command, "second");
    app.record_prompt_history(crate::ui::PromptKind::SearchForward, "needle");
    assert_eq!(app.input.command_history(), vec!["second"]);
    assert_eq!(app.input.search_history(), vec!["needle"]);
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.history.search.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.record_prompt_history(crate::ui::PromptKind::SearchBackward, "ignored");
    assert_eq!(app.input.search_history(), vec!["needle"]);
}

#[test]
fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.write_debounce_ms = 10;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
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
    std::thread::sleep(Duration::from_millis(12));
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

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.write_debounce_ms = 0;
        app.configuration.install_fixture(fixture);
    }
    app.invoke_insert_text("X".into());
    let revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    app.state.buffers[buffer].mark_saved(revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery.path(buffer).exists());
}

#[test]
fn recovery_delete_completion_cannot_forget_a_newer_queued_write() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.write_debounce_ms = 0;
        fixture.settings.recovery.cleanup_enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    wait_vfs(&mut app);

    let saved_revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer].mark_saved(saved_revision);
    app.schedule_recovery_work();
    assert!(app.recovery.deleting());

    app.invoke_insert_text("B".into());
    let latest = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    assert_eq!(app.recovery.revisions().get(&buffer), Some(&latest));
    app.recovery.check_foreign_completions();
    wait_vfs(&mut app);

    let recovered = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest.0);
    assert_eq!(app.recovery.revisions().get(&buffer), Some(&latest));
    app.schedule_recovery_work();
    assert!(!app.recovery.writing(None));
}

#[test]
fn retention_pruning_suppresses_only_the_pruned_live_revision_without_churn() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.write_debounce_ms = 0;
        fixture.settings.recovery.maximum_retained_records = 0;
        fixture.settings.recovery.maximum_age_days = 0;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    let pruned_revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery.path(buffer).exists());
    assert_eq!(app.recovery.pruned().get(&buffer), Some(&pruned_revision));
    app.schedule_recovery_work();
    assert!(!app.recovery.writing(None));

    app.invoke_insert_text("B".into());
    let newer = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    assert_ne!(newer, pruned_revision);
    assert!(app.recovery.writing(Some(newer)));
}

#[test]
fn disabled_recovery_policy_schedules_no_work() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    app.invoke_insert_text("X".into());
    app.schedule_recovery_work();
    assert!(app.recovery.scheduled().is_empty());
    assert_eq!(app.documents.legacy_count(), 0);
}

#[test]
fn bundled_upper_h_and_l_cycle_open_buffers_in_normal_mode() {
    let mut app = app();
    let (first, _) = app.state.open_text(
        TextBuffer::from_text("first"),
        Some(PathBuf::from("/workspace/first.rs")),
    );
    let (second, _) = app.state.open_text(
        TextBuffer::from_text("second"),
        Some(PathBuf::from("/workspace/second.rs")),
    );
    app.set_dashboard_visibility(false);
    assert_eq!(app.state.active_buffer_id(), Some(second));

    app.handle_key_in_context("editor.normal", KeyEvent::character('H'));
    assert_eq!(app.state.active_buffer_id(), Some(first));
    app.handle_key_in_context("editor.normal", KeyEvent::character('L'));
    assert_eq!(app.state.active_buffer_id(), Some(second));
}

#[test]
fn buffer_line_mouse_selects_and_right_click_closes_with_dirty_safety() {
    let mut app = app();
    let (first, _) = app.state.open_text(
        TextBuffer::from_text("first"),
        Some(PathBuf::from("/workspace/first.rs")),
    );
    let (second, _) = app.state.open_text(
        TextBuffer::from_text("second"),
        Some(PathBuf::from("/workspace/second.rs")),
    );
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
    let row = bounds.y
        + u16::from(
            app.configuration.settings().ui.tab_line.enabled
                && (!app.configuration.settings().ui.tab_line.hide_when_single
                    || app.state.tabs.len() > 1),
        );
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
fn content_line_pins_reorder_middle_mouse_and_survive_bridge_reset() {
    let mut app = app();
    let (a, _) = app.state.open_text(
        TextBuffer::from_text("a"),
        Some(PathBuf::from("/workspace/a.rs")),
    );
    let (b, _) = app.state.open_text(
        TextBuffer::from_text("b"),
        Some(PathBuf::from("/workspace/b.rs")),
    );
    let (c, _) = app.state.open_text(
        TextBuffer::from_text("c"),
        Some(PathBuf::from("/workspace/c.rs")),
    );
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.open_text(TextBuffer::from_text("scratch"), None);
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == "S" && entry.kind == ContentTabKind::Text)
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
    let row = bounds.y
        + u16::from(
            app.configuration.settings().ui.tab_line.enabled
                && (!app.configuration.settings().ui.tab_line.hide_when_single
                    || app.state.tabs.len() > 1),
        );
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
    let mut app = app();
    let path = PathBuf::from("/workspace/diagnostic.rs");
    app.state
        .open_text(TextBuffer::from_text("bad"), Some(path.clone()));
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("fixture").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    let uri = crate::language::lsp::file_uri(&path);
    app.diagnostics
        .publish(
            crate::editor::session::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: crate::editor::session::diagnostics::DiagnosticDocument::capture(
                    &app.state,
                    app.state.active_buffer_id().unwrap(),
                ),
                validity:
                    crate::editor::session::diagnostics::DiagnosticValidity::RequestedSnapshot,
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
    assert_eq!(text.icon, "R");
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
        ("C", false)
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
    let row = bounds.y
        + u16::from(
            app.configuration.settings().ui.tab_line.enabled
                && (!app.configuration.settings().ui.tab_line.hide_when_single
                    || app.state.tabs.len() > 1),
        );
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
    let mut app = app();
    let (first, _) = app.state.open_text(
        TextBuffer::from_text("first"),
        Some(PathBuf::from("/workspace/src/main.rs")),
    );
    let (second, _) = app.state.open_text(
        TextBuffer::from_text("second"),
        Some(PathBuf::from("/workspace/tests/main.rs")),
    );
    app.state.set_buffer_dirty(first, true);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.graph().counts(), (3, 2, 2, 1));
    assert_eq!(app.ui.content_tabs.len(), 2);
    assert_ne!(app.ui.content_tabs[0].title, app.ui.content_tabs[1].title);
    assert!(app.ui.content_tabs[0].dirty);
    assert!(app.ui.content_tabs[1].active);
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
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

    app.process_effect(crate::editor::AppEffect::Layout(
        crate::editor::LayoutAction::Split(crate::editor::Axis::Horizontal),
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
    app.process_effect(crate::editor::AppEffect::WindowSwap {
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
    app.process_effect(crate::editor::AppEffect::WindowResize {
        target: format!("window:{}", focused.0),
        amount: 50,
    });
    assert_ne!(app.window_session.graph(), &before_resize);
    assert_eq!(app.state.panes.len(), 2);

    app.process_effect(crate::editor::AppEffect::WindowClose(format!(
        "window:{}",
        focused.0
    )));
    assert_eq!(app.state.panes.len(), 1);
    assert!(app.window_session.graph().window(initial_window).is_some());
    let state_revision_before_rejected = app.state.window_revision();
    let active_view_before_rejected = app.state.active_view;
    let pane_count_before_rejected = app.state.panes.len();
    let graph_before_rejected = app.window_session.graph().clone();
    app.process_effect(crate::editor::AppEffect::WindowClose("focused".into()));
    assert_eq!(app.state.window_revision(), state_revision_before_rejected);
    assert_eq!(app.state.active_view, active_view_before_rejected);
    assert_eq!(app.state.panes.len(), pane_count_before_rejected);
    assert_eq!(app.window_session.graph(), &graph_before_rejected);
    assert!(app.ui.notification_error);
}

#[test]
fn text_and_virtual_windows_project_input_and_preserve_identity_across_ordinary_placements() {
    let mut app = app();
    app.window_session.observe_size((80, 24));
    app.state.open_text(
        TextBuffer::from_text("floating editor contents"),
        Some(PathBuf::from("/workspace/floating.rs")),
    );
    app.set_dashboard_visibility(false);
    let tab = app.state.active_tab.expect("editor tab");
    let source_pane = app.state.tabs[tab].focused;
    let source_view = app.state.panes[source_pane].view;
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::Split(
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

    let mut virtual_app = super::app();
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
    let mut app = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(TextBuffer::from_text("TOP_EDITOR"), None);
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
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::Split(
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::Split(
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
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::NewTab));
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
    let mut app = app();
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("first"),
        Some(PathBuf::from("/workspace/first.rs")),
    );
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

    let before_label = app.window_session.editor_ids().reconciliation_count();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.buffer_line.scratch_label = "[untitled]".into();
        app.configuration.install_fixture(fixture);
    }
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        before_label + 1
    );
    app.window_session
        .graph()
        .validate()
        .expect("revision-driven graph is valid");
}

#[test]
fn explorer_is_a_graph_dock_and_bundled_disable_is_transactional() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
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
    assert!(rect.width > 1 && rect.height == app.window_session.height());

    let mut disabled = crate::config::load_bundled_config();
    disabled.config.ui.explorer.bundled_enabled = false;
    app.apply_configuration_candidate(disabled, false)
        .expect("disable bundled explorer");
    assert!(app.ui.explorer.is_none());
    assert!(app.state.active_view.is_some());
    app.open_explorer();
    assert!(app.ui.explorer.is_none());
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("disabled"))
    );
    app.reconcile_editor_windows();
    assert_eq!(
        app.window_session
            .graph()
            .window(ids.window)
            .expect("hidden explorer")
            .placement,
        WindowPlacement::Hidden
    );

    let enabled = crate::config::load_bundled_config();
    app.apply_configuration_candidate(enabled, false)
        .expect("re-enable bundled explorer");
    app.open_explorer();
    assert_eq!(app.window_session.native_ids().explorer(), Some(ids));
    assert!(matches!(
        app.window_session
            .graph()
            .window(ids.window)
            .expect("shown explorer")
            .placement,
        WindowPlacement::Docked { .. }
    ));
}

#[test]
fn generic_native_window_operations_are_revision_checked_atomic_and_identity_preserving() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((100, 30));
    app.open_explorer();
    app.terminal_session.fixture(
        PtyId(7),
        crate::editor::session::terminal::TerminalFixture {
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

    app.process_effect(crate::editor::AppEffect::WindowSwap {
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
    app.process_effect(crate::editor::AppEffect::WindowSwap {
        first: "explorer".into(),
        second: "terminal".into(),
    });

    app.process_effect(crate::editor::AppEffect::WindowFullscreen(
        "explorer".into(),
    ));
    let terminal_before = app
        .window_session
        .graph()
        .window(terminal.window)
        .expect("terminal")
        .placement;
    app.process_effect(crate::editor::AppEffect::WindowFullscreen(
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

    app.process_effect(crate::editor::AppEffect::WindowDock {
        target: "explorer".into(),
        edge: "left".into(),
        size: 31,
    });
    app.process_effect(crate::editor::AppEffect::WindowHide("explorer".into()));
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .expect("hidden")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::editor::AppEffect::WindowShow("explorer".into()));
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
            super::super::NativeSurfaceTarget::Terminal(7),
            crate::editor::FocusDirection::Up
        )
        .is_err()
    );
    assert_eq!(app.window_session.terminal_height(), height_before);
    app.process_effect(crate::editor::AppEffect::WindowHide("terminal".into()));
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
    app.process_effect(crate::editor::AppEffect::WindowHide("terminal".into()));
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

    app.process_effect(crate::editor::AppEffect::WindowClose("explorer".into()));
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
fn semantic_window_layout_persistence_is_strict_transactional_and_id_free() {
    let mut source = app();
    source.state.open_text(
        TextBuffer::from_text("editor"),
        Some(PathBuf::from("/workspace/editor.rs")),
    );
    source.set_dashboard_visibility(false);
    source.open_explorer();
    source.process_effect(crate::editor::AppEffect::WindowDock {
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

    let mut restored = app();
    restored
        .state
        .open_text(TextBuffer::from_text("restored"), None);
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
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
fn terminal_panel_focus_cycle_resize_and_close_preserve_independent_sessions() {
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.runtime.vfs_queue_capacity = 2;
    loaded.config.ui.terminal_panel.height = 10;
    loaded.config.ui.terminal_panel.minimum_height = 4;
    loaded.config.ui.terminal_panel.maximum_height = 20;
    loaded.config.ui.terminal_panel.resize_step = 2;
    let workspace = tempdir().unwrap();
    let state = tempdir().unwrap();
    let mut app = InteractiveApplication::new(
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
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((80, 24));
    let mut first_screen = PtyScreen::new(80, 8, 100);
    first_screen.feed("first terminal");
    let mut second_screen = PtyScreen::new(80, 8, 100);
    second_screen.feed("second terminal");
    app.terminal_session.fixture(
        PtyId(1),
        crate::editor::session::terminal::TerminalFixture {
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
        crate::editor::session::terminal::TerminalFixture {
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
    assert!(
        app.terminal_session
            .resize(PtyId(1), u16::MAX, u16::MAX, &app.workspace)
            .is_err()
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 8));
    let mut foreign = crate::editor::session::terminal::TerminalSession::new(
        &app.configuration.settings().terminals,
    );
    foreign.fixture(
        PtyId(1),
        crate::editor::session::terminal::TerminalFixture {
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
    assert_eq!(app.workspace_render_height(), 14);
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

    app.process_effect(crate::editor::AppEffect::TerminalUnfocus);
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(!app.window_session.terminal_focused());
    assert!(app.ui.terminal_panel.is_some());
    assert_eq!(app.active_input_context(), "editor.normal");
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(!app.window_session.terminal_focused());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(app.window_session.terminal_focused());
    assert!(app.terminal_window_rect().is_some());
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    app.process_effect(crate::editor::AppEffect::TerminalUnfocus);

    app.process_effect(crate::editor::AppEffect::TerminalFocus(2));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.window_session.terminal_focused());
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
    app.process_effect(crate::editor::AppEffect::TerminalCycle(-1));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(1)));
    assert!(app.terminal_session.fixture_lines(PtyId(2))[0].contains("second"));

    app.process_effect(crate::editor::AppEffect::TerminalResize(1));
    assert_eq!(app.window_session.terminal_height(), 12);
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 10));
    app.process_effect(crate::editor::AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Up,
    ));
    assert_eq!(app.window_session.terminal_height(), 14);
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (80, 12));
    app.process_effect(crate::editor::AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Down,
    ));
    assert_eq!(app.window_session.terminal_height(), 12);
    assert_eq!(app.workspace_render_height(), 12);
    app.handle_event(crate::ui::UiEvent::Resize(60, 20));
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 10));
    assert_eq!(app.workspace_render_height(), 8);

    let terminal_ids = app
        .window_session
        .native_ids()
        .terminal(1)
        .expect("terminal resources");
    app.process_effect(crate::editor::AppEffect::TerminalFloat {
        x: 5,
        y: 3,
        width: 40,
        height: 10,
    });
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
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (40, 8));
    app.process_effect(crate::editor::AppEffect::WindowMove {
        target: "terminal".into(),
        x: 7,
        y: 4,
    });
    app.process_effect(crate::editor::AppEffect::WindowResize {
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
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (42, 10));
    app.process_effect(crate::editor::AppEffect::TerminalFullscreen);
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
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 18));
    app.process_effect(crate::editor::AppEffect::TerminalHide);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert!(app.terminal_session.contains(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::editor::AppEffect::WindowShow("terminal".into()));
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
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "top".into(),
        size: 5,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 3));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "right".into(),
        size: 10,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (10, 18));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "left".into(),
        size: 8,
    });
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (8, 18));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "bottom".into(),
        size: 12,
    });
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );
    assert_eq!(app.terminal_session.fixture_dimensions(PtyId(1)), (60, 10));
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
    app.process_effect(crate::editor::AppEffect::Layout(
        crate::editor::LayoutAction::NewTab,
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
    app.process_effect(crate::editor::AppEffect::Layout(
        crate::editor::LayoutAction::FocusTab(-1),
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
    app.process_effect(crate::editor::AppEffect::TerminalFocus(2));
    app.process_effect(crate::editor::AppEffect::TerminalFloat {
        x: 9,
        y: 4,
        width: 33,
        height: 9,
    });
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
    app.process_effect(crate::editor::AppEffect::TerminalFocus(1));

    let visible_placement = app
        .window_session
        .graph()
        .window(terminal_ids.window)
        .expect("terminal window")
        .placement;
    app.process_effect(crate::editor::AppEffect::TerminalClose);
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

    app.process_effect(crate::editor::AppEffect::TerminalClose);
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
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert_eq!(
        app.window_session.native_ids().terminal(1),
        Some(terminal_ids)
    );

    app.process_effect(crate::editor::AppEffect::TerminalClose);
    assert!(app.terminal_session.select(PtyId(2)));
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(!app.terminal_session.contains(PtyId(1)));
    assert_eq!(app.terminal_session.selected(), Some(PtyId(2)));
    assert!(app.ui.terminal_panel.is_some());
    app.process_effect(crate::editor::AppEffect::TerminalClose);
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
    use super::driver::TestSession;
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
fn terminal_preflight_close_and_capture_keep_original_pty_and_geometry() {
    use super::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.terminals.profiles.insert("owned-terminal".into(),crate::config::TerminalProfileConfig {program:"/bin/sh".into(),arguments:vec!["-c".into(),"printf x >> starts; printf 'READY\n'; read value; printf 'ACK:%s\n' \"$value\"; read end".into()],cwd:None,environment:Default::default(),columns:80,rows:8});
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    driver.terminal_request("owned-terminal");
    driver.terminal_close();
    driver.terminal_finish_validation();
    assert!(
        driver.projection().terminal_panel.is_none(),
        "close during validation retires the actual intent"
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
    driver.resize(63, 27);
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
    // Opening AND dismissing a capture during validation must not look unchanged.
    driver.terminal_request("owned-terminal");
    driver.open_command_prompt();
    driver.close_prompt();
    driver.terminal_finish_validation();
    assert!(
        driver.projection().terminal_panel.is_none(),
        "late valid data may start hidden, not seize focus"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.terminal_started_markers() != "xx" {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.keyboard_terminal(), None);
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
fn debugger_protected_launch_source_capture_refusal_and_fresh_recovery() {
    use super::driver::TestSession;
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
    let mut replacement = loaded.clone();
    replacement.config.debug.maximum_watches += 1;
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.debug_program_fixture();
    driver.trust_owned_terminal_fixture();
    driver.debug_request("owned");
    driver.debug_stop();
    driver.terminal_finish_validation();
    assert!(!starts.exists(), "stop during validation launched adapter");
    let drain = |driver: &mut TestSession| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while driver.debug_preflights() != 0 {
            driver.pump_debug();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(!starts.exists(), "retired preflight launched adapter");
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    let program_deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_program_pending() {
        driver.pump_debug();
        assert!(
            Instant::now() < program_deadline,
            "program preflight not admitted"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
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
fn semantic_content_restore_uses_fresh_ids_and_preserves_unclamped_desired_placement() {
    use crate::editor::session::persistence::RestoreContext;
    use crate::persistence::PersistedWindowPlacement;

    let mut app = app();
    let path = app.state.workspace_root.join("descriptor-restore.txt");
    std::fs::write(&path, "restored descriptor\n").expect("fixture file");
    app.state.open_text(
        TextBuffer::from_text("restored descriptor\n"),
        Some(path.clone()),
    );
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
    use crate::editor::session::persistence::RestoreContext;

    let mut app = app();
    let path = app.state.workspace_root.join("descriptor-hostile.txt");
    std::fs::write(&path, "preserved\n").expect("fixture file");
    app.state
        .open_text(TextBuffer::from_text("preserved\n"), Some(path));
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
fn restore_reads_preserve_newer_edits_capture_and_original_editor_identity() {
    use crate::{
        editor::session::persistence::RestoreContext,
        persistence::{
            PersistedLayout, PersistedLocation, PersistedMark, PersistedPane, PersistedSession,
            PersistedState, PersistedTab,
        },
    };
    let mut app = app();
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
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
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
        .open_text(TextBuffer::from_text("newer unsaved"), None);
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
    app.persistence.fixture_restore_source(context!());
    app.restore_session(session.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    assert!(!app.state.buffers.contains_key(newer));
    assert_ne!(buffer, newer);
    assert!(identity.same_editor(&app.state.window_version()));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "héllo\n");
    let restored = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(restored.cursor.get(), 1);
    assert_eq!(restored.selection.unwrap().anchor.get(), 3);
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
    app.restore_session(missing);
    wait_vfs(&mut app);
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
    app.persistence.set_mark("x".into(), location(3), 8);
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
    app.persistence
        .restore_history(&mut saved, &app.configuration.settings().persistence);
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
