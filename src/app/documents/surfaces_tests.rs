use crate::app::tests::*;
use crate::app::AgentCommand;
use crate::editor::EditorOperation;
use crate::editor::EditorState;
use crate::editor::EditorTransaction;
use crate::editor::TransactionId;
use crate::ui::render::overlays::CUSTOM_ICON;
use crate::ui::render::overlays::SCRATCH_ICON;




#[test]
pub(in crate::app) fn perpendicular_docked_editor_resizes_do_nothing() {
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
pub(in crate::app) fn directional_editor_resize_skips_hidden_splits_and_reaches_the_visible_dock() {
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
pub(in crate::app) fn notifications_expire_but_remain_available_in_bounded_history() {
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
pub(in crate::app) fn log_selection_survives_eviction_of_an_earlier_entry() {
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
pub(in crate::app) fn normalized_mouse_position_invokes_real_cursor_placement() {
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


pub(in crate::app) fn gate_document_worker(app: &mut App) -> std::sync::mpsc::Sender<()> {
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
pub(in crate::app) fn a_pending_open_reserves_its_save_destination() {
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
pub(in crate::app) fn an_alternate_spelling_of_the_current_file_uses_its_saved_bytes() {
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
pub(in crate::app) fn save_alias_identity_is_resolved_by_the_worker_before_writing() {
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
pub(in crate::app) fn saved_line_ending_policy_updates_the_live_buffer_without_losing_new_edits() {
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
pub(in crate::app) fn shutdown_saves_normalized_text_with_resumable_undo_history() {
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
pub(in crate::app) fn undo_history_failures_are_notified_and_fail_shutdown() {
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
pub(in crate::app) fn configured_file_format_policy_is_applied_at_save_boundary() {
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
pub(in crate::app) fn dirty_buffer_close_save_discard_cancel_and_stale_ack_are_source_bound() {
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
pub(in crate::app) fn dirty_close_save_waits_for_exact_vfs_ack_and_failure_preserves_arrangement() {
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
pub(in crate::app) fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
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
        crate::app::documents::recovery::WRITE_DEBOUNCE_MS + 10,
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
pub(in crate::app) fn recovery_retention_keeps_all_live_dirty_journals() {
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
pub(in crate::app) fn recovery_save_and_close_removes_the_older_journal() {
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
pub(in crate::app) fn recovery_rewrites_the_path_without_a_text_revision_change() {
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
pub(in crate::app) fn shutdown_returns_the_reason_dirty_buffers_cannot_be_recovered() {
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
pub(in crate::app) fn shutdown_retries_recovery_after_document_admission_drains() {
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
pub(in crate::app) fn recovery_dialog_discards_one_journal_and_recovers_another() {
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
pub(in crate::app) fn disabled_recovery_policy_schedules_no_work() {
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
pub(in crate::app) fn bundled_upper_h_and_l_cycle_open_buffers_in_normal_mode() {
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
pub(in crate::app) fn buffer_line_mouse_selects_and_right_click_closes_with_dirty_safety() {
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
pub(in crate::app) fn content_line_pins_reorder_middle_mouse_and_survive_bridge_reset() {
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
pub(in crate::app) fn content_line_diagnostics_custom_entries_and_idle_projection_are_pure() {
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
            crate::windows::ContentTypeId::new("plugin.demo-custom").unwrap(),
            crate::windows::ContentContract::adapter_defaults(
                crate::windows::ContentAdapterKind::DeclarativeUi,
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


#[cfg(unix)]
#[test]
pub(in crate::app) fn ctrl_arrows_in_a_lone_editor_resize_the_agent_docked_beside_it() {
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


#[test]
pub(in crate::app) fn a_late_result_updates_its_own_record_and_leaves_a_newer_draft_untouched() {
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


#[test]
pub(in crate::app) fn live_marks_follow_command_edits_before_saving_and_jumping() {
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
pub(in crate::app) fn an_agent_edit_of_a_file_with_unsaved_changes_warns_in_its_approval() {
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
pub(in crate::app) fn a_saved_location_reads_as_file_and_line() {
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
pub(in crate::app) fn buffer_cycling_follows_the_buffer_line_order() {
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
pub(in crate::app) fn nested_border_drag_uses_the_border_axis_on_both_sides() {
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
                border.axis == crate::windows::TileAxis::Vertical && border.length > 100
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
pub(in crate::app) fn clipped_dirty_close_cannot_save_or_discard() {
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
pub(in crate::app) fn recovered_missing_file_can_be_saved_without_force() {
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
pub(in crate::app) fn recovered_scratch_keeps_its_unsaved_baseline_and_durable_journal() {
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