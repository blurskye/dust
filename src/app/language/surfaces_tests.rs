use crate::app::tests::*;

use crate::app::documents::surfaces_tests::gate_document_worker;


#[test]
pub(in crate::app) fn lsp_log_selection_keeps_the_same_message_when_old_entries_are_evicted() {
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
pub(in crate::app) fn saves_and_reloads_wait_for_the_same_buffers_completion() {
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
pub(in crate::app) fn documentation_scrolling_uses_the_final_completion_box_and_reaches_its_tail() {
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
pub(in crate::app) fn recovery_delete_completion_cannot_forget_a_newer_queued_write() {
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
        crate::app::documents::recovery::WRITE_DEBOUNCE_MS + 10,
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
        crate::app::documents::recovery::WRITE_DEBOUNCE_MS + 10,
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
pub(in crate::app) fn semantic_window_layout_persistence_is_strict_transactional_and_id_free() {
    let (_source_dirs, mut source) = app();
    let path = source.state.workspace_root.join("editor.rs");
    source
        .state
        .open_text(TextBuffer::from_text("editor"), Some(path))
        .unwrap();
    source.set_dashboard_visibility(false);
    source.open_explorer();
    source.process_effect(crate::app::commands::AppEffect::WindowDock {
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
pub(in crate::app) fn semantic_content_restore_uses_fresh_ids_and_preserves_unclamped_desired_placement() {
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
use crate::app::persistence::RestoreContext;
use std::time::Duration;
use crate::editor::text::TextBuffer;
use crate::persistence::PersistedWindowRule;
use crate::ui::DocumentationMarkupKind;
use crate::ui::DocumentationPopupKind;
use crate::ui::UiEvent;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::fs;
use std::thread;
use crate::ui::render;
use crate::app::AppEffect;
use crate::persistence::PersistedWindowPlacement;

