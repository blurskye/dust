use crate::app::tests::*;




#[test]
pub(in crate::app) fn dashboard_and_picker_are_stable_graph_overlays_with_projected_geometry() {
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
pub(in crate::app) fn live_logs_and_output_refresh_preserve_picker_normal_without_leaking_it_to_new_sources() {
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
pub(in crate::app) fn closed_picker_surfaces_leave_the_content_line_but_keep_their_graph_identity() {
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
pub(in crate::app) fn picker_title_changes_update_the_retained_window_content() {
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
use crate::editor::text::TextBuffer;
use crate::ui::PickerSurfaceKind;
use crate::windows::WindowRect;
use crate::app::AppEffect;
use crate::app::PickerKind;

