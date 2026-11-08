//! Persistence behavior and application integration tests.

use crate::app::AppEffect;
use crate::app::debugger::tests::logging_adapter_fixture;
use crate::app::persistence::RestoreContext;
use crate::app::tests::*;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::persistence::PersistedLocation;
use crate::persistence::PersistedSession;
use crate::persistence::PersistedState;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn help_restores_the_visible_palette_and_keeps_file_search_alive() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("file");
    let before = app.ui.picker.as_ref().unwrap().query.clone();
    app.open_contextual_help();
    app.close_picker();
    assert_eq!(app.ui.picker.as_ref().unwrap().query, before);
    let path = app.state.workspace_root.join("surviving-search.txt");
    fs::write(&path, "found after help\n").unwrap();
    app.start_workspace_file_picker();
    app.open_contextual_help();
    app.process_workspace_file_events();
    assert!(app.search.help_is_open());
    app.close_picker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.picker_insert("surviving-search");
    let picker = app.ui.picker.as_ref().expect("restored file picker");
    assert_eq!(picker.title, "Files");
    assert!(
        picker
            .rows
            .iter()
            .any(|row| row.label == "surviving-search.txt")
    );
    app.reconcile_editor_windows();
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened search result");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "found after help\n"
    );
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
    app.recovery = crate::app::documents::recovery::Recovery::new(directory.clone());
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
fn session_restore_reads_more_files_than_the_document_queue_capacity() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    for index in 0..12 {
        if index > 0 {
            app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::NewTab));
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
    assert_eq!(app.state.editor_tabs().count(), 12);
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
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("same.txt");
    fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    app.set_dashboard_visibility(false);
    let first = app.state.active_view().unwrap();
    let second = app
        .state
        .split_active(crate::windows::TileAxis::Vertical, 500)
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
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(11)
    );
}

#[test]
fn session_restore_focuses_the_saved_pane_when_panes_share_a_file() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("shared-panes.txt");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    let first = app.state.active_view().unwrap();
    app.state.views[first].cursor = ByteOffset(1);
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let second = app.state.active_view().unwrap();
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
    let active = app.state.active_view().unwrap();
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
    let (_app_dirs, mut app) = app();
    let paths = ["one.txt", "two.txt", "three.txt"].map(|name| {
        let path = app.state.workspace_root.join(name);
        std::fs::write(&path, format!("{name}\n")).expect("fixture file");
        path
    });
    app.set_dashboard_visibility(false);
    app.start_open(paths[0].clone());
    wait_vfs(&mut app);
    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::NewTab,
    ));
    app.start_open(paths[1].clone());
    wait_vfs(&mut app);
    let session = app.capture_session().expect("captured session");

    // Closing the first tab frees the slot that a restore would otherwise fill last.
    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::FocusTab(-1),
    ));
    app.process_effect(crate::app::commands::AppEffect::Layout(
        crate::app::windows::LayoutAction::CloseTab,
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
        .editor_tabs()
        .map(|tab| {
            let view = app
                .state
                .window_view(app.state.tab_focused_window(tab.id).unwrap())
                .unwrap();
            app.state.buffers[app.state.views[view].buffer].path.clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(order, vec![Some(paths[0].clone()), Some(paths[1].clone())]);
}

#[test]
fn malformed_content_descriptor_restore_is_atomic_before_io_or_publication() {
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
fn cancelled_session_restore_reads_are_discarded_before_a_later_restore() {
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
fn restore_reads_preserve_newer_edits_capture_and_original_editor_identity() {
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedMark;
    use crate::persistence::PersistedPane;
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
    assert_eq!(app.state.active_view(), Some(view));
    assert_eq!(app.state.buffers[newer].text.to_string(), "newer unsaved");
    assert_eq!(app.state.buffers.len(), 1);
    app.state.buffers[newer].dirty = false;
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.persistence.fixture_restore_source(context!());
    app.restore_session(session.clone());
    app.input.capture_changed();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_view(), Some(view));
    let identity = app.state.window_version();
    let (manager, plugin) = crate::plugins::ExecutionFixture::new(4);
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
    let restored = &app.state.views[app.state.active_view().unwrap()];
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

#[test]
fn hiding_an_inspector_sends_no_disconnect_and_leaves_the_session_running() {
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
fn an_attached_session_names_the_target_it_was_given_in_its_status() {
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
fn a_restored_session_keeps_the_buffer_line_in_window_order() {
    let (_app_dirs, mut app) = app();
    let zeta = app.state.workspace_root.join("zeta.txt");
    let alpha = app.state.workspace_root.join("alpha.txt");
    std::fs::write(&zeta, "zeta\n").expect("zeta");
    std::fs::write(&alpha, "alpha\n").expect("alpha");
    app.state
        .open_text(TextBuffer::from_text("zeta\n"), Some(zeta.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
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

use crate::app::persistence::*;
pub(in crate::app) fn drain(owner: &mut Persistence) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while owner.pending() {
        owner.poll();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[test]
fn successful_undo_retry_clears_only_that_files_failure() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = Persistence::new(PersistenceWorker::start(1), root.path().join("state.json"));
    for path in ["a", "b"] {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Err("write failed".into())).unwrap();
        owner.track_undo_save(path.into(), receiver);
    }
    assert_eq!(owner.poll_undo_saves().len(), 2);
    for path in ["a", "b"] {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Ok(())).unwrap();
        owner.track_undo_save(path.into(), receiver);
        assert!(owner.poll_undo_saves().is_empty());
        assert_eq!(owner.undo_shutdown_result().is_ok(), path == "b");
    }
}
#[test]
fn save_notice_waits_for_the_latest_queued_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let (worker, release) = PersistenceWorker::gated_for_test(1);
    let mut owner = Persistence::new(worker, root.path().join("state.json"));
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    owner.request_save_notice();
    owner.request_save();
    drop(release);
    drain(&mut owner);
    assert!(!owner.take_saved_notice());
    assert!(owner.take_save_request());
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    drain(&mut owner);
    assert!(owner.take_saved_notice());
}
#[test]
fn state_save_requests_coalesce_without_consuming_a_pending_write() {
    let root = tempfile::tempdir().unwrap();
    let (worker, release) = PersistenceWorker::gated_for_test(1);
    let mut owner = Persistence::new(worker, root.path().join("state.json"));
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    owner.request_save();
    owner.request_save();
    assert!(!owner.take_save_request());
    drop(release);
    drain(&mut owner);
    assert!(owner.take_save_request());
    assert!(!owner.take_save_request());
    owner.begin_shutdown(true);
    owner.request_save();
    assert!(!owner.take_save_request());
    assert!(
        owner.shutdown_result().is_err(),
        "an earlier write is not the exit snapshot"
    );
}
#[test]
fn full_and_stopped_exit_queues_are_not_successful_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (mut worker, release) = PersistenceWorker::gated_for_test(1);
    worker
        .read(path.clone(), PersistencePolicy::default())
        .unwrap();
    let mut full = Persistence::new(worker, path.clone());
    full.begin_shutdown(true);
    assert!(
        full.save(PersistedState::default(), PersistencePolicy::default())
            .is_err()
    );
    assert!(full.shutdown_result().is_err());
    drop(full);
    drop(release);
    let mut stopped = Persistence::new(PersistenceWorker::stopped_for_test(), path);
    stopped.begin_shutdown(true);
    assert!(
        stopped
            .save(PersistedState::default(), PersistencePolicy::default())
            .is_err()
    );
    assert!(stopped.shutdown_result().is_err());
}
#[test]
fn earlier_save_finishing_during_exit_does_not_acknowledge_the_exit_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let (worker, release) = PersistenceWorker::gated_for_test(1);
    let mut owner = Persistence::new(worker, directory.path().join("state.json"));
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    owner.begin_shutdown(true);
    release.send(()).unwrap();
    drain(&mut owner);
    assert!(
        owner.shutdown_result().is_err(),
        "earlier save incorrectly acknowledged an exit snapshot that was never accepted"
    );
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    drain(&mut owner);
    owner.shutdown_result().unwrap();
}
#[test]
fn exit_requires_a_completed_snapshot_not_idle_or_failed_submission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let mut owner = Persistence::new(PersistenceWorker::start(1), path.clone());
    owner.load(PersistencePolicy::default()).unwrap();
    owner.begin_shutdown(true);
    drain(&mut owner);
    assert!(
        owner.shutdown_result().is_err(),
        "a completed load is not an exit save"
    );
    owner
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    assert!(owner.shutdown_result().is_err());
    drain(&mut owner);
    owner.shutdown_result().unwrap();
    assert!(path.is_file());
    let mut rejected =
        Persistence::new(PersistenceWorker::start(1), directory.path().to_path_buf());
    rejected.begin_shutdown(true);
    rejected
        .save(PersistedState::default(), PersistencePolicy::default())
        .unwrap();
    drain(&mut rejected);
    assert!(
        rejected.shutdown_result().is_err(),
        "failed write must not look idle/successful"
    );
}

#[test]
fn scratch_jump_uses_the_newest_entry_and_live_marks_follow_edits() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = Persistence::new(PersistenceWorker::start(1), root.path().join("state"));
    let path = root.path().join("file");
    let location = crate::persistence::PersistedLocation {
        path: path.clone(),
        byte: 2,
        anchor: 2,
        scroll_line: 0,
        scroll_column: 0,
    };
    owner.record_location(location.clone(), false, 10);
    assert_eq!(owner.walk_jumps(None, true, 10).unwrap().byte, 2);
    let mut editor = crate::editor::EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(
            crate::editor::text::TextBuffer::from_text("abc"),
            Some(path),
        )
        .unwrap();
    owner.set_mark("a".into(), location, 10, &editor);
    let text = &mut editor.buffers[buffer].text;
    text.apply_edits(&crate::editor::text::TextEditSet::new(
        text.revision(),
        vec![crate::editor::text::TextEdit::new(
            crate::editor::text::ByteRange::empty(crate::editor::text::ByteOffset(0)),
            "xx",
        )],
    ))
    .unwrap();
    owner.reconcile_marks(&editor);
    assert_eq!(owner.marks()[0].location.byte, 4);
}
