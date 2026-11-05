use super::*;

#[test]
fn diagnostic_navigation_filters_severity_wraps_and_expands_the_landed_line() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_bundled_config(),
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    let path = app.state.workspace_root.join("diagnostic.rs");
    let (buffer, view) = app.state.open_text(
        TextBuffer::from_text("ok\nerror\nwarning\n"),
        Some(path.clone()),
    ).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let uri = crate::language::lsp::file_uri(&path);
    let diagnostic = |range, severity, message: &str| crate::workspace::Diagnostic {
        uri: uri.clone(),
        version: None,
        range,
        severity,
        source: "fake".into(),
        code: None,
        message: message.into(),
        related: Vec::new(),
        tags: Vec::new(),
        fixes: Vec::new(),
        data: None,
        timestamp: std::time::SystemTime::UNIX_EPOCH,
    };
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lsp.fake").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    app.diagnostics
        .publish(
            crate::editor::session::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: crate::editor::session::diagnostics::DiagnosticDocument::capture(
                    &app.state, buffer,
                ),
                validity:
                    crate::editor::session::diagnostics::DiagnosticValidity::UnversionedReport,
                items: vec![
                    diagnostic(
                        ByteRange::new(ByteOffset(3), ByteOffset(8)).unwrap(),
                        DiagnosticSeverity::Error,
                        "error",
                    ),
                    diagnostic(
                        ByteRange::new(ByteOffset(9), ByteOffset(16)).unwrap(),
                        DiagnosticSeverity::Warning,
                        "warning",
                    ),
                ],
            },
            &app.state,
        )
        .unwrap();
    app.sync_ui_diagnostics();

    app.invoke_mapped_command("diagnostic.warning.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
    assert_eq!(
        app.ui.expanded_diagnostic.as_ref().unwrap().severities,
        vec![DiagnosticSeverity::Warning]
    );

    app.invoke_mapped_command("diagnostic.error.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(3));
    app.invoke_mapped_command("diagnostic.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
    assert_eq!(app.state.views[view].buffer, buffer);
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert!(
        rows.iter()
            .any(|row| row.label.ends_with("diagnostic.rs:2  error"))
    );
    assert!(
        rows.iter()
            .any(|row| row.label.ends_with("diagnostic.rs:3  warning"))
    );
    let before = app.state.views[view].cursor;
    let replacement = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lsp.fake").unwrap(),
    );
    app.diagnostics.activate(replacement);
    app.accept_picker();
    assert_eq!(
        app.state.views[view].cursor, before,
        "old picker key cannot navigate replacement diagnostics"
    );
    assert!(app.ui.expanded_diagnostic.is_none());

    // A simulated task source uses the production pre-run capture, not a late name lookup.
    let configuration = app.configuration.binding_revision();
    let run = crate::editor::session::tasks::TaskDiagnosticRun::capture(
        "lint",
        &app.state,
        &app.workspace,
        configuration,
    )
    .unwrap();
    app.diagnostics.activate(run.producer().clone());
    assert!(run.current(&app.diagnostics, &app.workspace, configuration));
    assert!(run.document(&path, &app.state).unwrap().is_some());
    app.state.open_text(
        TextBuffer::from_text("unrelated"),
        Some(root.path().join("other")),
    ).unwrap();
    assert!(run.document(&path, &app.state).unwrap().is_some());
    assert!(!run.current(&app.diagnostics, &app.workspace, configuration + 1));
    let next = crate::editor::session::tasks::TaskDiagnosticRun::capture(
        "lint",
        &app.state,
        &app.workspace,
        configuration,
    )
    .unwrap();
    app.diagnostics.activate(next.producer().clone());
    app.diagnostics.clear_source(run.producer());
    assert!(next.current(&app.diagnostics, &app.workspace, configuration));
    assert!(!run.current(&app.diagnostics, &app.workspace, configuration));
    app.state.buffers[buffer].text = TextBuffer::from_text("ok\nerror\nwarning\n");
    assert!(next.document(&path, &app.state).is_err());
}

#[test]
fn accepting_a_disk_diagnostic_opens_its_file_and_validates_the_range() {
    let mut app = app();
    let path = app.state.workspace_root.join("unopened.txt");
    fs::write(&path, "first\nerror\n").unwrap();
    let uri = crate::language::lsp::file_uri(&path);
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lint.test").unwrap());
    app.diagnostics.activate(producer.clone());
    app.diagnostics.publish(crate::editor::session::diagnostics::DiagnosticBatch {
        producer, uri: uri.clone(), document: None,
        validity: crate::editor::session::diagnostics::DiagnosticValidity::DiskReport,
        items: vec![crate::workspace::Diagnostic {
            uri, version: None, range: ByteRange::new(ByteOffset(6), ByteOffset(11)).unwrap(),
            severity: DiagnosticSeverity::Error, source: "test".into(), code: None,
            message: "error".into(), related: vec![], tags: vec![], fixes: vec![],
            data: None, timestamp: std::time::SystemTime::UNIX_EPOCH,
        }],
    }, &app.state).unwrap();
    app.open_builtin_picker(PickerKind::Diagnostics);
    app.accept_picker();
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
    assert_eq!(view.cursor, ByteOffset(6));

    let before = view.buffer;
    let invalid = app.state.workspace_root.join("short.txt");
    fs::write(&invalid, "x").unwrap();
    app.navigate_byte_location(&crate::language::lsp::file_uri(&invalid),
        ByteRange::new(ByteOffset(6), ByteOffset(11)).unwrap());
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(before));
    assert!(app.ui.notification_error);
}

#[test]
fn explorer_command_prompt_creates_a_file_from_the_selected_directory() {
    let mut app = app();
    app.open_explorer();
    app.open_prompt(crate::ui::PromptKind::Command, "exploreradd ".into());
    app.prompt_insert("new.txt");
    app.submit_prompt();
    assert!(app.ui.prompt.is_none(), "{:?}", app.ui.prompt);
    assert!(app.explorer.mutation_pending());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !app.state.workspace_root.join("new.txt").exists() {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.state.workspace_root.join("new.txt").is_file());
}

#[test]
fn explorer_prefilled_rename_keeps_original_target_and_dirty_split_views() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let source = root.join("source.rs");
    let other = root.join("other.rs");
    fs::write(&source, "disk text").unwrap();
    fs::write(&other, "other").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let (buffer, first) = app
        .state
        .open_text(TextBuffer::from_text("dirty text"), Some(source.clone())).unwrap();
    app.state.set_buffer_dirty(buffer, true);
    let second = app
        .state
        .split_active(crate::editor::Axis::Vertical, 500)
        .unwrap();
    app.state.views[first].cursor = ByteOffset(1);
    app.state.views[second].cursor = ByteOffset(4);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    app.handle_configured_key(KeyEvent::character('e'));
    let prompt = app.input.prompt().expect("e opens shared prompt");
    assert_eq!(prompt.kind, crate::ui::PromptKind::ExplorerRename);
    assert_eq!(prompt.input, "source.rs");
    assert_eq!(prompt.cursor, "source.rs".len());
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    let screen = (0..24)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Rename path"));
    assert!(screen.contains("source.rs"));
    app.handle_event(UiEvent::Paste("invalid\nname".into()));
    assert_eq!(app.input.prompt().unwrap().input, "source.rs");
    wait_vfs(&mut app);
    // Selection changes must never retarget the original rename request.
    app.explorer
        .select(app.explorer.tree().id_for_path(&other).unwrap());
    for _ in 0.."source.rs".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("../escape.rs");
    app.submit_prompt();
    assert!(app.input.prompt().unwrap().error.is_some());
    assert!(!app.explorer.mutation_pending());
    for _ in 0.."../escape.rs".len() {
        app.prompt_backspace();
    }
    let value = "nested/new 'é'.rs";
    app.handle_event(UiEvent::Paste(value.into()));
    app.submit_prompt();
    app.submit_prompt(); // a second Enter cannot submit a second mutation
    wait_vfs(&mut app);
    let target = root.join(value);
    assert!(app.input.prompt().is_none());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "disk text");
    assert_eq!(fs::read_to_string(&other).unwrap(), "other");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "dirty text");
    assert_eq!(app.state.views[first].buffer, buffer);
    assert_eq!(app.state.views[second].buffer, buffer);
    assert_eq!(app.state.views[first].cursor, ByteOffset(1));
    assert_eq!(app.state.views[second].cursor, ByteOffset(4));
    assert!(app.input.command_history().is_empty());
    assert!(app.input.search_history().is_empty());
}

#[test]
fn explorer_rename_prompt_refuses_replaced_files_and_retired_or_cancelled_requests() {
    let mut app = app();
    let source = app.state.workspace_root.join("source.txt");
    let target = app.state.workspace_root.join("target.txt");
    fs::write(&source, "original").unwrap();
    fs::write(&target, "collision").unwrap();
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    app.handle_configured_key(KeyEvent::character('r'));
    wait_vfs(&mut app);
    for _ in 0.."source.txt".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("target.txt");
    app.submit_prompt();
    wait_vfs(&mut app);
    assert_eq!(app.input.prompt().unwrap().input, "target.txt");
    assert!(app.input.prompt().unwrap().error.is_some());
    assert_eq!(fs::read_to_string(&target).unwrap(), "collision");
    fs::remove_file(&target).unwrap();
    let parked = source.with_extension("parked");
    fs::rename(&source, &parked).unwrap();
    fs::write(&source, "replacement").unwrap();
    app.submit_prompt();
    wait_vfs(&mut app);
    assert!(app.input.prompt().unwrap().error.is_some());
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    assert_eq!(fs::read_to_string(&parked).unwrap(), "original");
    assert!(!target.exists());
    app.close_prompt();
    // Closing before inspection delivery cannot resurrect a prompt or enable a rename.
    app.open_explorer_rename_prompt();
    app.handle_configured_key("escape".parse().unwrap());
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    assert!(
        app.explorer
            .rename_value(
                "target.txt",
                app.workspace.version(),
                app.configuration.binding_revision(),
                &app.state.window_version()
            )
            .is_err()
    );
    // Cancelling after preparation admission still performs no mutation.
    app.open_explorer_rename_prompt();
    wait_vfs(&mut app);
    for _ in 0.."source.txt".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("target.txt");
    app.submit_prompt();
    app.close_prompt();
    wait_vfs(&mut app);
    assert!(source.exists());
    assert!(!target.exists());
    app.open_explorer_rename_prompt();
    app.handle_event(UiEvent::FocusLost);
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    app.open_explorer_rename_prompt();
    app.process_effect(AppEffect::ExplorerClose);
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    // A root epoch change retires the original prompt, even for equal path strings.
    app.open_explorer_rename_prompt();
    app.explorer
        .reset_roots(app.workspace.roots(), app.workspace.root_revision());
    app.retire_stale_explorer_intents();
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    assert!(!target.exists());
}

#[test]
fn explorer_rename_rejects_destinations_owned_by_unsaved_buffers() {
    for directory in [false, true] {
        let mut app = app();
        let root = app.state.workspace_root.clone();
        let source = root.join("source");
        let target = root.join("target");
        let (source_file, target_file) = if directory {
            fs::create_dir(&source).unwrap();
            (source.join("file.txt"), target.join("file.txt"))
        } else {
            (source.clone(), target.clone())
        };
        fs::write(&source_file, "source text").unwrap();
        let (buffer, _) = app.state.open_text(
            TextBuffer::from_text("source text"), Some(source_file.clone()),
        ).unwrap();
        let (destination, _) = app.state.open_text(
            TextBuffer::from_text("unsaved destination"), Some(target_file.clone()),
        ).unwrap();
        app.state.set_buffer_dirty(destination, true);
        app.open_explorer();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.explorer.tree().id_for_path(&source).is_none() {
            app.process_explorer_result();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        app.explorer.select(app.explorer.tree().id_for_path(&source).unwrap());
        app.rename_explorer_path("target".into());
        wait_vfs(&mut app);
        assert!(!app.explorer.mutation_pending());
        assert_eq!(fs::read_to_string(&source_file).unwrap(), "source text");
        assert!(!target.exists());
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&source_file));
        assert_eq!(app.state.buffers[destination].path.as_ref(), Some(&target_file));
        assert_eq!(app.state.buffers[destination].text.to_string(), "unsaved destination");
        assert!(app.state.buffers[destination].dirty);
    }
}

#[test]
fn explorer_close_reconciles_an_admitted_rename_even_after_disk_completion() {
    use crate::editor::session::documents::DocumentEvent;
    let mut app = app();
    let source = app.state.workspace_root.join("before.txt");
    let target = app.state.workspace_root.join("after.txt");
    fs::write(&source, "kept").unwrap();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("kept"), Some(source.clone())).unwrap();
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    app.explorer.select(app.explorer.tree().id_for_path(&source).unwrap());
    app.rename_explorer_path("after.txt".into());
    // Admit the prepared write, but do not consume its completion yet.
    loop {
        if let Some(DocumentEvent::Explorer(reply)) = app.documents
            .poll(&app.state.window_version(), app.workspace.root_revision()).unwrap()
        {
            app.process_explorer_io(reply);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    while !target.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&source));
    app.process_effect(AppEffect::ExplorerClose);
    app.retire_stale_explorer_intents();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    assert!(!app.explorer.mutation_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "kept");
}

#[test]
fn explorer_rename_refuses_destinations_owned_by_unsaved_buffers_before_writing() {
    for directory in [false, true] {
        let mut app = app();
        let source = app.state.workspace_root.join("source");
        let target = app.state.workspace_root.join("renamed");
        let (source_file, target_file) = if directory {
            fs::create_dir(&source).unwrap();
            (source.join("inside.txt"), target.join("inside.txt"))
        } else {
            (source.clone(), target.clone())
        };
        fs::write(&source_file, "original").unwrap();
        let (original, _) = app.state.open_text(TextBuffer::from_text("original"), Some(source_file.clone())).unwrap();
        let (unsaved, _) = app.state.open_text(TextBuffer::from_text("unsaved"), Some(target_file.clone())).unwrap();
        app.state.set_buffer_dirty(unsaved, true);
        app.open_explorer();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.explorer.tree().id_for_path(&source).is_none() {
            app.process_explorer_result();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        app.explorer.select(app.explorer.tree().id_for_path(&source).unwrap());
        app.rename_explorer_path("renamed".into());
        while app.explorer.mutation_pending() {
            app.process_vfs_results();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fs::read_to_string(&source_file).unwrap(), "original");
        assert!(!target.exists());
        assert_eq!(app.state.buffers[original].path.as_ref(), Some(&source_file));
        assert_eq!(app.state.buffers[unsaved].path.as_ref(), Some(&target_file));
        assert_eq!(app.state.buffers[unsaved].text.to_string(), "unsaved");
        assert!(app.ui.notification_error);
    }
}

#[test]
fn explorer_rename_and_delete_reconcile_open_buffers_without_losing_text() {
    let mut app = app();
    let source = app.state.workspace_root.join("source.txt");
    fs::write(&source, "kept").unwrap();
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("kept plus unsaved"),
        Some(source.clone()),
    ).unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer.select(
        app.explorer
            .tree()
            .id_for_path(&source)
            .expect("source node"),
    );
    app.rename_explorer_path("renamed.txt".into());
    let target = app.state.workspace_root.join("renamed.txt");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.state.buffers[buffer].path.as_ref() != Some(&target) {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.state.buffers[buffer].path.as_ref(),
        Some(&target),
        "notice={:?}; roots={:?}; pending={}; disk target={}",
        app.notices.current(),
        app.workspace.roots(),
        app.explorer.mutation_pending(),
        target.exists()
    );
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&target).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&target).unwrap());
    let ready_delete = |app: &mut InteractiveApplication| {
        app.request_explorer_delete();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app
            .input
            .prompt()
            .is_none_or(|p| p.kind != crate::ui::PromptKind::ExplorerDelete)
        {
            app.process_vfs_results();
            assert!(Instant::now() < deadline, "delete preflight timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    ready_delete(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No
    );
    app.submit_prompt();
    assert!(!app.explorer.mutation_pending());
    assert!(target.exists(), "default Enter/No must not delete");
    app.confirm_explorer_delete();
    assert!(target.exists());
    ready_delete(&mut app);
    let original = target.with_extension("original");
    fs::rename(&target, &original).unwrap();
    fs::write(&target, "replacement must survive").unwrap();
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.mutation_pending() {
        app.process_vfs_results();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "replacement must survive"
    );
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    fs::remove_file(&target).unwrap();
    fs::rename(&original, &target).unwrap();
    ready_delete(&mut app);
    let root_node = app
        .explorer
        .tree()
        .id_for_path(&app.state.workspace_root)
        .expect("workspace root node");
    app.explorer.select(root_node); // the retained operation still targets the original file
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.state.buffers[buffer].path.is_some() {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!target.exists());
    assert!(app.state.buffers[buffer].path.is_none());
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );
    let opened = app.state.workspace_root.join("opened.txt");
    fs::write(&opened, "open through owner").unwrap();
    app.refresh_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&opened).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&opened).unwrap());
    app.explorer_open_file(false);
    app.ui.explorer = None;
    app.reconcile_explorer_watch();
    wait_vfs(&mut app);
    assert!(
        !app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&opened)),
        "closed explorer read resurrected a document"
    );
    app.open_explorer();
    app.explorer
        .select(app.explorer.tree().id_for_path(&opened).unwrap());
    let panes = app.state.panes.len();
    app.explorer_open_file(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.panes.len(), panes + 1);
    let active = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[active].path.as_ref(), Some(&opened));
    assert_eq!(
        app.state.buffers[active].text.to_string(),
        "open through owner"
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );
}

#[test]
fn workspace_trust_is_one_key_cancels_on_focus_loss_and_persists() {
    let key = |name: &str| name.parse::<KeyEvent>().expect("key");
    let mut app = app();
    prepare_trust_identity(&mut app);
    app.notify_message("stale notification".into(), true);
    app.request_workspace_trust();
    assert!(app.ui.notification.is_none());
    assert!(!app.ui.notification_error);
    let prompt = app.ui.prompt.as_ref().expect("trust prompt");
    assert_eq!(prompt.kind, crate::ui::PromptKind::WorkspaceTrust);
    let confirmation = prompt.confirmation.as_ref().expect("yes/no trust prompt");
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::Yes);
    assert!(confirmation.message.contains("LSP servers"));

    // Escape declines and says what stays off.
    app.handle_configured_key(key("escape"));
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("not trusted"))
    );

    app.request_workspace_trust();
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );

    // Enter accepts the preselected Trust.
    app.request_workspace_trust();
    app.handle_configured_key(key("enter"));
    assert!(app.ui.prompt.is_none());
    wait_trust(&mut app);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    assert!(app.input.search_history().is_empty() && app.input.command_history().is_empty());
    let source = fs::read_to_string(app.workspace.state_path()).expect("persisted trust");
    assert!(source.contains("filesystem_identity"));

    app.revoke_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    wait_trust(&mut app);
    let source = fs::read_to_string(app.workspace.state_path()).expect("persisted revocation");
    assert!(source.contains("identities = []"));
    assert!(source.contains("[[seen]]"));
}

#[test]
fn shutdown_waits_for_a_queued_workspace_revocation() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    // Another instance grants trust after this instance loaded its untrusted snapshot.
    let identity = app.workspace.roots()[0].identity.clone();
    let mut other = crate::workspace::WorkspaceTrustService::start(1);
    other.persist(path.clone(), vec![identity.clone()], vec![identity], vec![]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(crate::workspace::TrustEvent::Persisted { result, .. }) = other.try_event().unwrap() {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let lock = crate::files::lock_file(&path.with_extension("lock")).unwrap();
    app.revoke_workspace_trust();
    assert!(app.workspace.persist_pending());
    thread::scope(|scope| {
        scope.spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(lock);
        });
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(2)).unwrap();
    });
    assert!(!app.workspace.persist_pending());
    assert!(fs::read_to_string(path).unwrap().contains("identities = []"));
}

#[test]
fn shutdown_reports_failed_workspace_trust_writes() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    fs::create_dir_all(&path).unwrap();
    app.revoke_workspace_trust();
    assert!(matches!(
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(2)),
        Err(super::super::InteractiveError::Vfs(crate::files::VfsError::Io {
            operation: "finish workspace trust shutdown", ..
        }))
    ));
}

#[test]
fn shutdown_reports_workspace_trust_writes_that_exceed_its_deadline() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = crate::files::lock_file(&path.with_extension("lock")).unwrap();
    app.revoke_workspace_trust();
    assert!(matches!(
        app.finish_shutdown_before(Instant::now()),
        Err(super::super::InteractiveError::Vfs(crate::files::VfsError::Io {
            operation: "finish workspace trust shutdown", ..
        }))
    ));
    assert!(app.workspace.persist_pending());
    drop(lock);
    wait_trust(&mut app);
}

#[test]
fn unseen_workspace_prompts_once_after_identity_and_database_are_ready() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let identity = app.workspace.roots()[0].identity.clone();
    assert!(!app.workspace.seen(&identity));

    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(
        app.ui.prompt.as_ref().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::WorkspaceTrust)
    );
    assert!(app.workspace.seen(&identity));
    app.close_prompt();
    wait_trust(&mut app);
    assert!(
        fs::read_to_string(app.workspace.state_path())
            .expect("seen workspace state")
            .contains("[[seen]]")
    );

    app.maybe_prompt_for_unseen_workspace();
    assert!(app.ui.prompt.is_none());
}

#[test]
fn workspace_history_records_visits_without_trust_prompts() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let first = app.state.workspace_root.clone();
    // Recording the startup visit must not suppress its first trust prompt.
    app.process_trust_events();
    assert!(app.workspace.seen_roots().contains(&first));
    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(app.ui.prompt.as_ref().unwrap().kind, crate::ui::PromptKind::WorkspaceTrust);
    app.close_prompt();
    wait_trust(&mut app);

    let second = tempdir().unwrap();
    let second_path = second.path().canonicalize().unwrap();
    app.switch_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert!(app.ui.prompt.is_none());
    assert_eq!(app.workspace.trust(), crate::workspace::WorkspaceTrust::Untrusted);
    app.switch_workspace_root(first.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert_eq!(app.state.workspace_root, first);
    assert!(app.workspace.seen_roots().contains(&second_path));
    let database: crate::workspace::TrustDatabase = toml::from_str(
        &fs::read_to_string(app.workspace.state_path()).unwrap(),
    ).unwrap();
    assert!(database.seen.iter().any(|identity| identity.canonical_path == second_path));
    assert!(database.identities.is_empty(), "visiting does not grant trust");
}

#[test]
fn automatic_workspace_trust_prompt_never_steals_an_active_prompt_or_safe_mode() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(
        app.ui.prompt.as_ref().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::Command)
    );
    app.close_prompt();
    app.workspace.fixture_safe_mode(true);
    app.maybe_prompt_for_unseen_workspace();
    assert!(app.ui.prompt.is_none());
}

#[test]
fn workspace_trust_never_activates_before_identity_and_database_are_ready() {
    let mut app = app();
    app.confirm_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    assert!(app.ui.notification_error);
}

fn wait_explorer(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.pending() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline, "explorer timed out");
        thread::yield_now();
    }
}

#[test]
fn explorer_lazily_loads_and_opens_scoped_file() {
    let mut app = app();
    let path = app.state.workspace_root.join("explorer.txt");
    fs::write(&path, "explorer content\n").expect("fixture");
    app.open_explorer();
    app.explorer_toggle();
    wait_explorer(&mut app);
    assert_eq!(app.explorer.tree().visible().len(), 2);
    let explorer_ui = app.ui.explorer.as_ref().expect("dedicated explorer ui");
    assert_eq!(explorer_ui.rows.len(), 2, "the root and its file");
    assert!(explorer_ui.rows[1].node_id > 0);
    assert!(explorer_ui.rows[1].selected);
    assert!(
        explorer_ui.rows[1]
            .segments
            .iter()
            .any(|segment| segment.role == crate::ui::ExplorerSegmentRole::File)
    );
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
        app.configuration.install_fixture(fixture);
    }
    app.sync_explorer_ui();
    assert!(app.ui.explorer.as_ref().is_some_and(|ui| {
        ui.rows[1].segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "."
        })
    }));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Unicode;
        app.configuration.install_fixture(fixture);
    }
    app.sync_explorer_ui();
    assert!(app.ui.explorer.as_ref().is_some_and(|ui| {
        ui.rows[1].segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "·"
        })
    }));
    app.explorer.navigate(1);
    app.reconcile_editor_windows();
    app.explorer_open_file(false);
    assert!(app.ui.explorer.is_some());
    assert!(!app.ui.explorer_focused);
    // A normal redraw can reconcile the graph before asynchronous I/O finishes.
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    // Without Nerd Font icons the file's tab uses the same plain glyph as the explorer.
    app.reconcile_editor_windows();
    assert!(!app.ui.content_tabs.is_empty());
    assert!(app.ui.content_tabs.iter().all(|tab| tab.icon == "·"));
    assert_eq!(app.active_input_context(), "editor.normal");
    let tab = app.state.active_tab;
    let view = app.state.active_view;

    // Reopening an existing file switches back to its tab/view, without
    // duplicating the buffer or replacing its in-memory contents from disk.
    app.state.new_tab().unwrap();
    app.process_effect(AppEffect::NewBuffer);
    fs::write(&path, "different disk contents").unwrap();
    let buffers = app.state.buffers.len();
    app.open_explorer();
    app.explorer
        .select(app.explorer.tree().id_for_path(&path).unwrap());
    app.reconcile_editor_windows();
    app.explorer_open_file(false);
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(buffer));
    assert_eq!(app.state.active_tab, tab);
    assert_eq!(app.state.active_view, view);
    assert_eq!(app.state.buffers.len(), buffers);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "explorer content\n"
    );
    assert_eq!(app.active_input_context(), "editor.normal");

    app.open_explorer();
    app.explorer_open_file(true);
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_tab_views().len(), 2);

    // Genuine navigation while the read is outstanding still wins.
    app.open_explorer();
    app.explorer_open_file(false);
    app.process_effect(AppEffect::NewBuffer);
    let newer = app.state.active_buffer_id();
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), newer);
}

#[test]
fn explorer_uses_filename_and_extension_specific_nerd_font_icons() {
    let mut app = app();
    fs::write(app.state.workspace_root.join("README.md"), "# Dust\n").unwrap();
    fs::write(app.state.workspace_root.join("notes.md"), "notes\n").unwrap();
    fs::write(app.state.workspace_root.join("main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(app.state.workspace_root.join("src")).unwrap();

    app.open_explorer();
    app.explorer_toggle();
    wait_explorer(&mut app);
    let rows = &app.ui.explorer.as_ref().expect("explorer ui").rows;
    for (name, glyph, role) in [
        ("README.md", "", crate::ui::ExplorerSegmentRole::IconYellow),
        ("notes.md", "󰍔", crate::ui::ExplorerSegmentRole::IconGrey),
        ("main.rs", "󱘗", crate::ui::ExplorerSegmentRole::IconOrange),
        ("src", "󰴉", crate::ui::ExplorerSegmentRole::IconPurple),
    ] {
        let row = rows
            .iter()
            .find(|row| row.segments.iter().any(|segment| segment.text == name))
            .unwrap_or_else(|| panic!("missing explorer row for {name}"));
        assert!(
            row.segments
                .iter()
                .any(|segment| segment.text == glyph && segment.role == role),
            "wrong explorer icon for {name}"
        );
    }
}

#[test]
fn explorer_follow_lazily_expands_to_active_file() {
    let mut app = app();
    let directory = app.state.workspace_root.join("a/b");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("follow.txt");
    fs::write(&path, "follow").unwrap();
    app.state
        .open_text(TextBuffer::from_text("follow"), Some(path.clone())).unwrap();
    app.set_dashboard_visibility(false);
    app.follow_active_file_in_explorer();
    wait_explorer(&mut app);
    assert_eq!(
        app.explorer.tree().selected_node().map(|node| &node.path),
        Some(&path)
    );
    assert!(!app.explorer.following());
    assert!(
        app.ui
            .explorer
            .as_ref()
            .is_some_and(|ui| ui.rows.iter().any(|row| row.selected))
    );
}

#[test]
fn explorer_watch_batches_reload_through_directory_service_and_preserve_identity() {
    let mut app = app();
    let directory = app.state.workspace_root.join("dir");
    fs::create_dir_all(&directory).unwrap();
    let kept = directory.join("kept.txt");
    fs::write(&kept, "x").unwrap();
    fs::write(app.state.workspace_root.join("root.txt"), "x").unwrap();
    app.open_explorer();
    wait_explorer(&mut app);
    let directory_id = app.explorer.tree().id_for_path(&directory).unwrap();
    app.explorer.select(directory_id);
    app.explorer_toggle();
    wait_explorer(&mut app);
    let kept_id = app.explorer.tree().id_for_path(&kept).unwrap();
    app.explorer.select(kept_id);
    app.sync_explorer_ui();
    let generation = app.explorer.watch_generation().unwrap();
    let root = app.state.workspace_root.clone();
    let stale = ExplorerWatchBatch {
        generation: ExplorerWatchGeneration(generation.0.saturating_sub(1)),
        root_revision: app.workspace.root_revision(),
        directories: vec![root.clone()],
    };
    app.accept_explorer_watch_batch(stale);
    assert!(app.explorer.watch_queue_empty());
    let added = root.join("added.txt");
    fs::write(&added, "x").unwrap();
    let hidden = root.join(".hidden");
    let ignored = root.join("ignored.log");
    fs::write(&hidden, "x").unwrap();
    fs::write(&ignored, "x").unwrap();
    fs::write(root.join(".ignore"), "*.log\n").unwrap();
    app.accept_explorer_watch_batch(ExplorerWatchBatch {
        generation,
        root_revision: app.workspace.root_revision(),
        directories: vec![root.clone()],
    });
    app.start_next_explorer_watch_reload();
    wait_explorer(&mut app);
    assert!(app.explorer.tree().id_for_path(&added).is_some());
    assert!(app.explorer.tree().id_for_path(&hidden).is_none());
    assert!(app.explorer.tree().id_for_path(&ignored).is_none());
    assert_eq!(
        app.explorer.tree().id_for_path(&directory),
        Some(directory_id)
    );
    assert!(app.explorer.tree().is_expanded(directory_id));
    assert_eq!(app.explorer.tree().id_for_path(&kept), Some(kept_id));
    fs::remove_file(&kept).unwrap();
    let generation = app.explorer.watch_generation().unwrap();
    app.accept_explorer_watch_batch(ExplorerWatchBatch {
        generation,
        root_revision: app.workspace.root_revision(),
        directories: vec![directory.clone()],
    });
    app.start_next_explorer_watch_reload();
    wait_explorer(&mut app);
    assert!(app.explorer.tree().id_for_path(&kept).is_none());
    assert_eq!(app.explorer.tree().selected_id(), Some(directory_id));
    let reconciles = app.explorer.watch_reconciles();
    let pending = app.documents.legacy_count();
    for _ in 0..256 {
        app.reconcile_editor_windows();
        let _ = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
    }
    assert_eq!(app.explorer.watch_reconciles(), reconciles);
    assert_eq!(app.documents.legacy_count(), pending);
    app.explorer.select(directory_id);
    app.explorer.toggle_loaded(directory_id);
    app.sync_explorer_ui();
    assert!(
        !app.explorer
            .tree()
            .watched_directories()
            .contains(&directory)
    );
    app.process_effect(AppEffect::ExplorerClose);
    assert!(app.explorer.watch_generation().is_none());
}

#[test]
fn explorer_projection_formats_only_the_terminal_viewport() {
    let mut app = app();
    app.window_session
        .observe_size((app.window_session.width(), 10));
    for index in 0..40 {
        fs::write(
            app.state.workspace_root.join(format!("file{index}.txt")),
            "fixture",
        )
        .expect("fixture");
    }
    app.open_explorer();
    wait_explorer(&mut app);
    let explorer = app.ui.explorer.as_ref().expect("explorer projection");
    assert_eq!(explorer.total, 41);
    assert_eq!(explorer.rows.len(), 7);
    assert!(explorer.rows.len() < explorer.total);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    let first_child = app.explorer.tree().selected_id().expect("first child");
    let projection_generation = app.explorer.tree().projection_generation();

    app.move_explorer_selection(-1);
    let root = app.explorer.tree().selected_id();
    assert_ne!(root, Some(first_child));
    app.move_explorer_selection(1);
    assert_eq!(app.explorer.tree().selected_id(), Some(first_child));
    app.explorer_collapse_or_parent();
    assert_eq!(app.explorer.tree().selected_id(), root);
    app.move_explorer_selection(1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.handle_event(crate::ui::UiEvent::Resize(80, 20));
    let explorer = app.ui.explorer.as_ref().expect("grown projection");
    assert_eq!(explorer.rows.len(), 17);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );
    app.handle_event(crate::ui::UiEvent::Resize(80, 8));
    let explorer = app.ui.explorer.as_ref().expect("shrunk projection");
    assert_eq!(explorer.rows.len(), 5);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.move_explorer_selection(30);
    let explorer = app.ui.explorer.as_ref().expect("scrolled projection");
    assert_eq!(explorer.rows.len(), 5);
    assert!(explorer.scroll > 0);
    assert!(explorer.rows.iter().any(|row| row.selected));
    let before_page = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    app.page_explorer_selection(-1);
    let after_page = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    assert!(after_page < before_page);
    app.page_explorer_selection(1);
    assert!(
        app.explorer
            .tree()
            .visible_index(app.explorer.tree().selected_id().unwrap())
            .unwrap()
            > after_page
    );
    let rect = app.explorer_window_rect().unwrap();
    let wheel = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseScrollUp,
        crate::editor::input::Modifiers::default(),
    );
    let before_wheel = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    app.handle_event(UiEvent::Mouse {
        key: wheel,
        column: rect.x,
        row: rect.y.saturating_add(2),
    });
    assert!(
        app.explorer
            .tree()
            .visible_index(app.explorer.tree().selected_id().unwrap())
            .unwrap()
            < before_wheel
    );
    app.move_explorer_selection(isize::MIN);
    let top = app.explorer.tree().selected_id();
    app.page_explorer_selection(-1);
    assert_eq!(app.explorer.tree().selected_id(), top);
    app.move_explorer_selection(isize::MAX);
    let bottom = app.explorer.tree().selected_id();
    app.page_explorer_selection(1);
    assert_eq!(app.explorer.tree().selected_id(), bottom);
    let first_viewport_node = app.ui.explorer.as_ref().unwrap().rows[0].node_id;
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            crate::editor::input::Modifiers::default(),
        ),
        column: rect.x.saturating_add(1),
        row: rect.y.saturating_add(3),
    });
    assert_eq!(
        app.explorer.tree().selected_id(),
        Some(crate::workspace::ExplorerNodeId(first_viewport_node))
    );
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    let projected_before_rejection = app.ui.explorer.clone();
    let mut invalid = crate::config::load_bundled_config();
    invalid.config.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
    invalid
        .config
        .input
        .text_commands
        .insert("editor.insert".into(), "unknown.command".into());
    assert!(app.apply_configuration_candidate(invalid, false).is_err());
    assert_eq!(app.ui.explorer, projected_before_rejection);
    assert_eq!(
        app.configuration.settings().ui.explorer.icon_mode,
        crate::config::ExplorerIconMode::NerdFont
    );

    let mut valid = crate::config::load_bundled_config();
    valid.config.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
    app.apply_configuration_candidate(valid, false)
        .expect("valid explorer reload");
    let explorer = app.ui.explorer.as_ref().expect("reconfigured projection");
    assert_eq!(explorer.rows.len(), 5);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert!(explorer.rows.iter().all(|row| {
        row.segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "."
        })
    }));
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );
}

#[test]
fn directional_focus_moves_spatially_then_crosses_the_explorer_boundary() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("focus"), None).unwrap();
    app.set_dashboard_visibility(false);
    app.apply_layout_action(crate::editor::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    ));
    let tab = app.state.active_tab.expect("active tab");
    let right = app.state.tabs[tab].focused;
    let left = app
        .state
        .directional_pane(crate::editor::FocusDirection::Left)
        .expect("directional query")
        .expect("left pane");

    app.open_explorer();
    assert!(app.ui.explorer_focused);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert_eq!(app.state.tabs[tab].focused, right);

    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
}

fn wait_root_operation(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "root operation timed out");
        thread::yield_now();
    }
}

#[test]
fn async_root_add_activation_and_dirty_removal_preflight_are_real() {
    let mut app = app();
    let first = app.workspace.active_root().expect("first root");
    let second_directory = tempdir().expect("second root");
    let second_path = fs::canonicalize(second_directory.path()).expect("canonical");
    app.add_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    let second = app.workspace.active_root().expect("added root active");
    assert_eq!(app.state.workspace_root, second_path);
    let first_path = app.workspace.roots()[0].identity.canonical_path.clone();
    // Picking an existing root from :roots activates it instead of replacing the active one.
    app.switch_workspace_root(first_path.clone());
    assert_eq!(app.workspace.active_root(), Some(first));
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(app.state.workspace_root, first_path);
    assert!(!app.ui.notification_error);
    app.switch_workspace_root(second_path.clone());
    assert_eq!(app.workspace.active_root(), Some(second));
    assert_eq!(app.state.workspace_root, second_path);

    let dirty_path = second_path.join("dirty.txt");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("dirty"),
        Some(dirty_path),
    ).unwrap();
    app.state.buffers[buffer].dirty = true;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 2);
    assert!(app.ui.notification_error);
    app.state.buffers[buffer].dirty = false;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 1);
}

#[test]
fn workspace_switch_rechecks_dirty_buffers_before_publishing() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let original = app.state.workspace_root.clone();
    let roots = app.workspace.roots().to_vec();
    let version = app.workspace.version();
    let target = tempdir().unwrap();
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("clean"),
        Some(original.join("notes.txt")),
    ).unwrap();
    app.switch_workspace_root(target.path().to_path_buf());
    assert!(app.workspace.root_operation_pending());
    // The inspection may have finished on its worker, but its result is not published yet.
    app.state.set_buffer_dirty(buffer, true);
    wait_root_operation(&mut app);
    assert_eq!(app.workspace.roots(), roots);
    assert_eq!(app.workspace.version(), version);
    assert_eq!(app.state.workspace_root, original);
    assert!(app.state.buffers[buffer].dirty);
    assert!(app.ui.notification_error);

    app.state.set_buffer_dirty(buffer, false);
    app.switch_workspace_root(target.path().to_path_buf());
    wait_root_operation(&mut app);
    assert_eq!(app.state.workspace_root, target.path().canonicalize().unwrap());
}

#[test]
fn stale_async_root_add_and_pending_service_removal_are_rejected() {
    let mut app = app();
    let second = tempdir().expect("second");
    let third = tempdir().expect("third");
    app.add_workspace_root(second.path().to_path_buf());
    app.workspace
        .fixture_add_root(
            "third".into(),
            crate::workspace::RootIdentity::capture(third.path()).expect("identity"),
        )
        .expect("concurrent model mutation");
    wait_root_operation(&mut app);
    assert_eq!(
        app.workspace.roots().len(),
        2,
        "stale async add did not commit"
    );
    let third_id = app.workspace.active_root().expect("third active");
    app.start_workspace_search("needle".into(), None);
    app.remove_workspace_root(third_id);
    assert_eq!(app.workspace.roots().len(), 2);
    assert!(app.ui.notification_error);
}

fn wait_workspace_search(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.search.pending() || app.workspace_edits.preparing() {
        app.process_workspace_search_events();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "workspace search timed out");
        thread::yield_now();
    }
}

#[test]
fn explorer_auto_git_status_is_silent_outside_a_repository() {
    let mut app = app();
    // An invalid gitfile stops discovery before any repository above the fixture.
    fs::write(app.state.workspace_root.join(".git"), "gitdir: missing-repository\n").unwrap();
    assert!(git2::Repository::discover(&app.state.workspace_root).is_err());
    let path = app.state.workspace_root.join("plain.txt");
    fs::write(&path, "plain\n").unwrap();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("plain\n"),
        Some(path),
    ).unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().is_none());
    assert!(!app.ui.notification_error);
    let generation = app.git.status_generation();
    for _ in 0..256 {
        app.reconcile_git_signs();
        app.process_git_status_events();
        assert_eq!(app.git.status_generation(), generation);
    }
}

#[test]
fn active_tracked_buffer_starts_git_status_and_updates_unsaved_signs_without_explorer() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("live.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("live.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("one\ntwo\nthree\n"),
        Some(path),
    ).unwrap();
    app.set_dashboard_visibility(false);

    app.reconcile_git_signs();
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().is_some());
    assert!(app.ui.explorer.is_none());
    app.handle_configured_key(KeyEvent::character('d'));
    app.handle_configured_key(KeyEvent::character('d'));
    assert!(app.state.buffers[buffer].dirty);
    let revision = app.state.buffers[buffer].text.revision().0;
    app.reconcile_git_signs();
    assert!(app.git.signs_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.ui.git_signs.iter().any(|sign| {
        sign.buffer == buffer
            && sign.buffer_revision == revision
            && sign.kind == crate::ui::UiGitSignKind::Deleted
    }));

    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() || app.git.signs_pending() {
        app.process_git_status_events();
        app.reconcile_git_signs();
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.ui.explorer.is_none());
    assert!(!app.state.buffers[buffer].dirty);
    assert!(app.ui.git_signs.iter().any(|sign| sign.buffer == buffer));

    let path = app.state.buffers[buffer].path.clone().unwrap();
    fs::write(path, "one\ntwo\nthree\n").unwrap();
    app.start_reload(false);
    wait_vfs(&mut app);
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().unwrap().entries.is_empty());
}

#[test]
fn soft_reset_refreshes_status_even_when_index_and_head_files_do_not_change() {
    let (mut app, repo, _) = near_changes_fixture();
    let old = repo.head().unwrap().peel_to_commit().unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("near.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&old]).unwrap();
    app.start_git_status_refresh_with_feedback(false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    let stamp = |name| fs::metadata(repo.path().join(name)).unwrap().modified().unwrap();
    let before = (stamp("index"), stamp("HEAD"));
    repo.reset(old.as_object(), git2::ResetType::Soft, None).unwrap();
    assert_eq!(before, (stamp("index"), stamp("HEAD")));
    app.next_disk_check = None;
    app.process_disk_changes();
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().unwrap().entries.iter().any(|entry| entry.sources.staged));
}

#[test]
fn the_first_hunk_key_reads_the_hunks_and_moves_to_the_next_one() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("hunks.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("hunks.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "one\ntwo changed\nthree\nfour\nfive\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    app.start_git_status_refresh_silent();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }

    app.process_effect(AppEffect::GitHunkNavigate(1));
    assert!(app.git.hunks_pending(), "{:?}", app.ui.notification);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(&mut app);
    assert!(app.git.has_selection(), "{:?}", app.ui.notification);
    let view = &app.state.views[app.state.active_view.expect("view")];
    assert_eq!(
        app.state.buffers[view.buffer]
            .text
            .byte_to_line(view.cursor)
            .expect("line"),
        1,
        "{:?}",
        app.ui.notification
    );

    app.process_effect(AppEffect::GitHunkPreview);
    assert_eq!(
        app.ui.documentation_popup.as_ref().map(|popup| popup.kind),
        Some(crate::ui::DocumentationPopupKind::Git)
    );
}

/// A trusted repository with one file whose two changes are close enough to share a hunk.
fn near_changes_fixture() -> (InteractiveApplication, git2::Repository, std::path::PathBuf) {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("near.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("near.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "fixture",
        &repo.find_tree(tree_id).unwrap(),
        &[],
    )
    .unwrap();
    fs::write(&path, "one\nTWO\nthree\nFOUR\nfive\n").unwrap();
    prepare_trust_identity(&mut app);
    let root = app.workspace.roots()[0].clone();
    app.workspace.fixture_trust_identity(root.identity.clone());
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_status_refresh_silent();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    (app, repo, path)
}

fn settle_git_work(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_status_events();
        app.process_git_hunk_events();
        app.process_git_mutation_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(app);
}

#[test]
fn hunk_preview_and_stage_take_the_change_under_the_cursor() {
    let (mut app, repo, _) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);

    app.process_effect(AppEffect::GitHunkPreview);
    let popup = app.ui.documentation_popup.as_ref().expect("preview");
    let shown = popup.sections[0].lines.join("|");
    assert!(shown.contains("FOUR") && !shown.contains("TWO"), "{shown}");

    app.process_effect(AppEffect::GitHunkStage);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(blob.content()),
        "one\ntwo\nthree\nFOUR\nfive\n",
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn hunk_reset_takes_the_change_under_the_cursor() {
    let (mut app, _repo, path) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);

    app.process_effect(AppEffect::GitHunkReset);
    settle_git_work(&mut app);
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    settle_git_work(&mut app);

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "one\ntwo\nthree\nFOUR\nfive\n",
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn leaving_a_worktree_closes_shared_buffers_without_alternating_fallbacks() {
    let mut app = app();
    let old = app.state.workspace_root.clone();
    let new = old.join(".worktree/topic");
    fs::create_dir_all(&new).unwrap();
    let (a, _) = app.state.open_text(TextBuffer::from_text("a"), Some(old.join("a.txt"))).unwrap();
    let (b, first) = app.state.open_text(TextBuffer::from_text("b"), Some(old.join("b.txt"))).unwrap();
    let middle = app.state.split_active(crate::editor::Axis::Vertical, 500).unwrap();
    app.state.views[middle].buffer = a;
    let last = app.state.split_active(crate::editor::Axis::Vertical, 500).unwrap();
    app.state.views[last].buffer = b;
    assert_eq!(app.state.views[first].buffer, b);
    app.finish_root_switch(new, &old);
    assert!(!app.state.buffers.contains_key(a));
    assert!(!app.state.buffers.contains_key(b));
    assert!(!app.state.views.values().any(|view| view.buffer == a || view.buffer == b));
}

#[test]
fn a_pending_root_switch_rechecks_new_edits_and_terminals() {
    for terminal in [None, Some(false), Some(true)] {
        let mut app = app();
        let old = app.state.workspace_root.clone();
        let new = old.join(".worktree/topic");
        fs::create_dir_all(&new).unwrap();
        prepare_trust_identity(&mut app);
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("clean"), Some(old.join("file.txt"))).unwrap();
        app.switch_workspace_root(new);
        assert!(app.workspace.root_operation_pending());
        if let Some(running) = terminal {
            // A newly opened terminal must also block before its process reports Running.
            app.terminal_session.fixture(PtyId(1), crate::editor::session::terminal::TerminalFixture {
                profile: "shell".into(),
                screen: crate::ui::PtyScreen::new(80, 8, 100),
                running,
                exit_code: None,
                truncated: false,
            }, app.workspace.version());
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
fn breakpoint_toggle_uses_disabled_state_after_another_breakpoint_is_added() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(TextBuffer::from_text("one\ntwo\n"), Some(path.clone())).unwrap();
    let at = |line| crate::debugger::DapSourceBreakpoint {
        path: path.clone(), line, column: None, condition: None, hit_condition: None, log_message: None,
    };
    app.toggle_breakpoint_disabled();
    assert!(app.debugger_session.persisted_breakpoints().is_empty());
    app.debugger_session.breakpoint(at(1), false).unwrap();
    app.toggle_breakpoint_disabled();
    app.debugger_session.breakpoint(at(2), false).unwrap();
    app.toggle_breakpoint_disabled();
    assert_eq!(app.debugger_session.breakpoint_disabled(&path, 1), Some(false));
}

#[test]
fn leaving_a_worktree_closes_every_clean_buffer_from_it() {
    let mut app = app();
    let old = app.state.workspace_root.clone();
    let new = old.join(".worktree/topic");
    fs::create_dir_all(&new).unwrap();
    for name in ["first.txt", "second.txt"] {
        fs::write(old.join(name), "text\n").unwrap();
        app.start_open(old.join(name));
        wait_vfs(&mut app);
    }
    assert_eq!(app.state.buffers.len(), 2);

    app.finish_root_switch(new, &old);
    wait_vfs(&mut app);

    assert!(!app.state.buffers.values().any(|buffer| {
        buffer
            .path
            .as_deref()
            .is_some_and(|path| path.starts_with(&old) && !path.starts_with(old.join(".worktree")))
    }),);
}

#[test]
fn explorer_auto_git_status_colors_names_opens_picker_and_idles() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("tracked.txt");
    fs::write(&path, "normal\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "normal\nadded\n").unwrap();
    app.open_explorer();
    assert!(app.git.status_pending());
    assert!(app.git.silent());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let revision = app.git.status().unwrap().revision;
    assert_eq!(
        app.git.decorations().get(&path),
        Some(&crate::workspace::git::GitFileStatus::Modified)
    );
    assert_eq!(
        app.git.decorations().get(&app.state.workspace_root),
        Some(&crate::workspace::git::GitFileStatus::Modified)
    );
    wait_explorer(&mut app);
    let tracked_row = app
        .ui
        .explorer
        .as_ref()
        .and_then(|explorer| {
            explorer.rows.iter().find(|row| {
                row.segments
                    .iter()
                    .any(|segment| segment.text == "tracked.txt")
            })
        })
        .expect("tracked Git row");
    assert!(
        tracked_row
            .segments
            .iter()
            .any(|segment| segment.role == ExplorerSegmentRole::Git)
    );
    assert!(
        tracked_row
            .right_segments
            .iter()
            .any(|segment| segment.text == "M")
    );
    assert!(!app.git.silent());
    let generation = app.git.status_generation();
    for _ in 0..256 {
        app.process_git_status_events();
        assert_eq!(app.git.status_generation(), generation);
        assert_eq!(app.git.status().unwrap().revision, revision)
    }
    app.open_git_status_picker();
    let title = app
        .ui
        .picker
        .as_ref()
        .map(|p| p.title.clone())
        .expect("title");
    let branch = crate::workspace::git::git_branch_name(&app.state.workspace_root)
        .expect("a branch read")
        .expect("the fixture is on a branch");
    assert_eq!(
        title,
        format!("Changes · {branch} ±1"),
        "the surface titles itself with the branch and the change count the document draws"
    );
    // Accepting the only entry opens it; a section header is not an entry.
    assert!(
        app.ui
            .picker
            .as_ref()
            .is_some_and(|picker| picker.selected.is_some_and(|selected| {
                picker.rows[selected].kind == crate::ui::PickerRowKind::Entry
            })),
        "the initial selection is an entry, never a section header"
    );

    app.accept_picker();
    wait_vfs(&mut app);
    assert_eq!(
        app.state
            .active_buffer_id()
            .and_then(|id| app.state.buffers[id].path.as_ref()),
        Some(&path)
    );
    app.reconcile_git_signs();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(!app.ui.git_signs.is_empty());
    app.window_session.observe_size((100, 30));
    app.reconcile_editor_windows();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    let added_cells = (0..frame.grid.height())
        .flat_map(|y| (0..frame.grid.width()).map(move |x| (x, y)))
        .filter(|(x, y)| {
            frame
                .grid
                .get(*x, *y)
                .is_some_and(|cell| cell.style == crate::ui::Style::GitSignAdded)
        })
        .collect::<Vec<_>>();
    assert_eq!(added_cells.len(), 1);
    let (sign_x, sign_y) = added_cells[0];
    assert_eq!(frame.grid.get(sign_x, sign_y).unwrap().symbol, "│");
    assert!(sign_y > 0);
    let normal_sign_cell = frame.grid.get(sign_x, sign_y - 1).unwrap();
    assert_eq!(normal_sign_cell.symbol, "│");
    assert_eq!(normal_sign_cell.style, crate::ui::Style::Muted);
    let sign_generation = app.git.sign_generation();
    let signs = app.ui.git_signs.clone();
    for _ in 0..256 {
        app.reconcile_git_signs();
        app.process_git_sign_events();
        let _ = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            100,
            30,
        );
        assert_eq!(app.git.sign_generation(), sign_generation);
        assert_eq!(app.ui.git_signs, signs);
    }
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(
        app.ui.picker.as_ref().map(|p| p.title.as_str()),
        Some("Git hunks")
    );
    app.accept_picker();
    assert!(app.git.has_selection());
    app.preview_selected_git_hunk();
    assert_eq!(
        app.ui.documentation_popup.as_ref().map(|popup| popup.kind),
        Some(crate::ui::DocumentationPopupKind::Git)
    );
    app.navigate_git_hunk(1);
    wait_vfs(&mut app);
    assert!(app.git.has_selection());
    app.stage_selected_git_hunk();
    assert!(!app.git.mutation_pending());
    assert!(app.ui.notification_error);

    app.set_explorer_focus(false);
    // The hunk key landed on the added line. Deleting all of it would match the index again.
    app.handle_configured_key(KeyEvent::character('x'));
    let active = app.state.active_buffer_id().unwrap();
    assert!(app.state.buffers[active].dirty);
    let dirty_revision = app.state.buffers[active].text.revision().0;
    app.reconcile_git_signs();
    assert!(app.ui.git_signs.is_empty());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(!app.ui.git_signs.is_empty());
    assert!(
        app.ui
            .git_signs
            .iter()
            .all(|sign| { sign.buffer == active && sign.buffer_revision == dirty_revision })
    );
    let dirty_frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    assert!((0..dirty_frame.grid.height()).any(|y| {
        (0..dirty_frame.grid.width()).any(|x| {
            dirty_frame.grid.get(x, y).is_some_and(|cell| {
                matches!(
                    cell.style,
                    crate::ui::Style::GitSignAdded
                        | crate::ui::Style::GitSignChanged
                        | crate::ui::Style::GitSignDeleted
                )
            })
        })
    }));
}

#[test]
fn trusted_stage_hunk_flow_changes_only_index_and_refreshes_status() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("two-hunks.txt");
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("two-hunks.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.hunks().unwrap().clone();
    assert_eq!(snapshot.hunks.len(), 2);
    assert!(!app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision,
        &path.with_extension("foreign"),
        snapshot.hunks[0].id
    ));
    assert!(!app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision + 1,
        &path,
        snapshot.hunks[0].id
    ));
    assert!(!app.git.has_selection());
    app.accept_picker();
    assert!(app.git.has_selection());
    assert!(
        app.git
            .mutate(&app.state, false, true, snapshot.root_revision + 1)
            .is_err()
    );
    assert!(!app.git.mutation_pending());
    let before = crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    app.stage_selected_git_hunk();
    assert!(app.git.mutation_pending());
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let after = crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    assert_ne!(before, after);
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    assert!(!app.git.has_selection());
    assert!(app.git.hunks().is_none());
    let status = app.git.status().cloned().unwrap();
    let mut policy = crate::workspace::git::GitHunkPolicy {
        scope_root: status.scope_root,
        repository_id: status.repository_id,
        status_revision: status.revision,
        root_revision: status.root_revision,
        file: "two-hunks.txt".into(),
        layer: crate::workspace::git::GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(2),
    };
    assert_eq!(
        crate::workspace::git::read_hunks(&policy)
            .unwrap()
            .hunks
            .len(),
        1
    );
    policy.layer = crate::workspace::git::GitDiffLayer::Staged;
    assert_eq!(
        crate::workspace::git::read_hunks(&policy)
            .unwrap()
            .hunks
            .len(),
        1
    );
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.accept_picker();
    let index_before_reset =
        crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    let worktree_before_reset = fs::read_to_string(&path).unwrap();
    app.request_reset_selected_git_hunk();
    assert_eq!(
        app.ui.prompt.as_ref().map(|p| p.kind),
        Some(crate::ui::PromptKind::GitReset)
    );
    let prompt = app.ui.prompt.as_ref().expect("Git reset prompt");
    assert!(prompt.error.is_none());
    assert!(
        prompt
            .help
            .as_deref()
            .is_some_and(|help| help.contains("Discards worktree changes"))
    );
    app.close_prompt();
    assert_eq!(
        crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap(),
        index_before_reset
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), worktree_before_reset);
    app.request_reset_selected_git_hunk();
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap(),
        index_before_reset
    );
    let reset_text = fs::read_to_string(&path).unwrap();
    assert!(reset_text.contains("changed 2\n") && !reset_text.contains("changed 18\n"));
    policy.layer = crate::workspace::git::GitDiffLayer::Unstaged;
    assert!(
        crate::workspace::git::read_hunks(&policy)
            .unwrap()
            .hunks
            .is_empty()
    );
}

#[test]
fn workspace_file_picker_publishes_the_full_discovery_byte_budget() {
    let mut app = app();
    let files = tempfile::tempdir_in(&app.state.workspace_root).unwrap();
    let mut directory = files.path().to_path_buf();
    for _ in 0..10 {
        directory.push("d".repeat(200));
    }
    fs::create_dir_all(&directory).unwrap();
    let count = 3000;
    for index in 0..count {
        fs::write(directory.join(format!("file-{index:04}")), "").unwrap();
    }
    // Two path copies fit discovery's budget; a third label copy exceeds it.
    let relative_length = directory.strip_prefix(files.path()).unwrap().as_os_str().len() + 10;
    let absolute_length = directory.as_os_str().len() + 10;
    assert!(count * (relative_length + absolute_length) < 16 * 1024 * 1024);
    assert!(count * (2 * relative_length + absolute_length) > 16 * 1024 * 1024);
    app.search.start_files(crate::files::WorkspaceFilePolicy {
        roots: vec![files.path().into()], root_revision: app.workspace.root_revision(),
        include_hidden: false, include_ignored: false, maximum_files: 100_000,
        maximum_path_bytes: 16 * 1024 * 1024, batch_size: 128, event_queue_capacity: 8,
    }, app.workspace.version(), app.configuration.binding_revision()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let picker = app.search.picker().unwrap().presentation();
    assert!(!picker.loading);
    assert_eq!(picker.rows.len(), count);
}

#[test]
fn workspace_file_command_uses_bounded_picker_and_opens_selection() {
    let mut app = app();
    let path = app.state.workspace_root.join("find-me.txt");
    let mut contents = String::from("found\n");
    contents.push_str(&"x".repeat(crate::ui::render::surfaces::PICKER_MAXIMUM_PREVIEW_BYTES));
    fs::write(&path, contents).unwrap();
    assert!(
        fs::metadata(&path).unwrap().len()
            > crate::ui::render::surfaces::PICKER_MAXIMUM_PREVIEW_BYTES as u64
    );
    app.start_workspace_file_picker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(&mut app);
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.title == "Files"
            && picker.preview.as_ref().is_some_and(|preview| {
                preview.title == "find-me.txt"
                    && preview
                        .as_text()
                        .is_some_and(|text| {
                            text.lines().len() == 1
                                && text.lines()[0].contains("preview unavailable:")
                                && text.lines()[0].contains("exceeds")
                        })
            })
    }));
    app.picker_insert("find-me");
    for _ in 0..256 {
        app.sync_picker_ui();
        assert_eq!(app.search.file_requests(), 1)
    }
    app.reconcile_editor_windows();
    app.accept_picker();
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(
        app.state
            .active_buffer_id()
            .and_then(|id| app.state.buffers[id].path.as_ref()),
        Some(&path)
    );
}

#[test]
fn file_preview_retries_after_capacity_returns_without_picker_input() {
    use crate::editor::session::search::{MatcherSource, PickerPayload};
    let mut app = app();
    let path = app.state.workspace_root.join("preview.txt");
    fs::write(&path, "ready\n").unwrap();
    let roots = vec![app.state.workspace_root.clone()];
    let mut requests = Vec::new();
    loop {
        match app.documents.read_scoped_bounded(path.clone(), roots.clone(), 64) {
            Ok(id) => requests.push(id),
            Err(crate::files::VfsError::QueueFull) => break,
            Err(error) => panic!("unexpected admission error: {error}"),
        }
    }
    app.install_picker("Preview".into(), vec![(
        crate::workspace::PickerItem {
            id: crate::workspace::PickerItemId("preview".into()),
            label: "preview.txt".into(),
            detail: String::new(),
            source: String::new(),
            badge: String::new(),
            action: crate::workspace::PickerAction::None,
            kind: crate::workspace::PickerItemKind::Entry,
        },
        PickerPayload::MatcherSource(MatcherSource { path, line: 0, column: 0, encoding: crate::editor::text::PositionEncoding::Utf8 }),
    )]);
    assert!(app.search.preview_needs_retry());
    for id in requests {
        app.documents.cancel_request(id);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.preview_needs_retry() || app.documents.has_work() {
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "preview retry timed out");
        thread::yield_now();
    }
    let text = app.ui.picker.as_ref().unwrap().preview.as_ref().unwrap().as_text().unwrap();
    assert_eq!(text.lines(), ["ready"]);
    assert!(!app.search.preview_needs_retry());
}

#[test]
fn workspace_search_uses_generic_picker_and_opens_unopened_match() {
    let mut app = app();
    let path = app.state.workspace_root.join("match.txt");
    let text = (0..1_200)
        .map(|line| {
            if line == 1_087 {
                "before needle after".into()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let expected_cursor = text.find("needle").expect("needle offset");
    fs::write(&path, text).expect("fixture");
    let departure = app.state.workspace_root.join("departure.txt");
    fs::write(&departure, "origin\n").unwrap();
    let (original, view) = app.state.open_text(TextBuffer::from_text("origin\n"), Some(departure)).unwrap();
    app.state.views[view].cursor = ByteOffset(1);
    app.invoke_mapped_command("workspace.search.prompt");
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.ui.picker.as_ref().map(|picker| picker.title.as_str()),
        Some("Grep")
    );
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    assert_eq!(app.search.matches().len(), 1);
    assert_eq!(
        app.ui.picker.as_ref().map(|picker| picker.title.as_str()),
        Some("Grep")
    );
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.preview.as_ref().is_some_and(|preview| {
            let text = preview.as_text().expect("a text preview");
            preview.title == "match.txt"
                && text.highlight_line == Some(1_087)
                && text
                    .highlight_line
                    .is_some_and(|line| line.saturating_sub(text.first_line) <= 8)
        })
    }));
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened result");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    let view = app.state.active_view.expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(expected_cursor));
    app.walk_jumps(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(original));
    assert_eq!(app.state.views[app.state.active_view.unwrap()].cursor, ByteOffset(1));
}

#[test]
fn workspace_search_accepts_normalized_open_buffers() {
    for open in [false, true] {
        let mut app = app();
        let path = app.state.workspace_root.join("crlf.txt");
        let bytes = b"\xef\xbb\xbffirst\r\nneedle\r\n";
        fs::write(&path, bytes).unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        let found = app.search.matches()[0].clone();
        app.close_picker();
        if open {
            app.state.open_text(TextBuffer::from_bytes(bytes).unwrap(), Some(path.clone())).unwrap();
        }
        app.navigate_workspace_search_match(found);
        wait_vfs(&mut app);
        let view = &app.state.views[app.state.active_view.unwrap()];
        assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
        assert_eq!(view.cursor, ByteOffset(6));
        assert!(!app.ui.notification_error);
    }
}

#[test]
fn workspace_search_rejects_stale_open_and_closed_file_offsets() {
    for open in [false, true] {
        let mut app = app();
        let path = app.state.workspace_root.join("match.txt");
        fs::write(&path, "before needle\n").unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        let found = app.search.matches()[0].clone();
        app.close_picker();
        let (_, view) = if open {
            app.state.open_text(TextBuffer::from_text("new prefix before needle\n"), Some(path.clone())).unwrap()
        } else {
            fs::write(&path, "new prefix before needle\n").unwrap();
            app.state.open_text(TextBuffer::from_text("unchanged active view"), None).unwrap()
        };
        let buffer = app.state.views[view].buffer;
        app.navigate_workspace_search_match(found);
        wait_vfs(&mut app);
        assert_eq!(app.state.active_buffer_id(), Some(buffer));
        assert_eq!(app.state.views[view].cursor, ByteOffset(0));
        assert!(app.ui.notification_error);
    }
}

#[test]
fn workspace_search_streams_exact_batches_and_root_changes_cancel() {
    let mut app = app();
    fs::write(app.state.workspace_root.join("one.txt"), "needle\n").unwrap();
    fs::write(app.state.workspace_root.join("two.txt"), "needle\n").unwrap();
    app.start_workspace_search("needle".into(), None);
    wait_workspace_search(&mut app);
    // Each file publishes its remaining matches before the next file is read.
    assert_eq!(app.search.sequence(), 2);
    let broker = app.search.workflow().unwrap();
    let snapshot = broker.snapshot_if_changed(0).unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.items.len(), 2);
    for _ in 0..256 {
        assert!(broker.snapshot_if_changed(snapshot.revision).is_none())
    }
    app.close_picker();
    app.start_workspace_search("needle".into(), None);
    let extra = tempdir().unwrap();
    app.workspace
        .fixture_add_root(
            "extra".into(),
            crate::workspace::RootIdentity::capture(extra.path()).unwrap(),
        )
        .unwrap();
    app.process_workspace_search_events();
    assert!(!app.search.pending());
    assert!(app.search.workflow().is_none());
    assert!(app.search.matches().is_empty());
}

#[test]
fn workspace_replacement_groups_many_matches_from_shared_snapshots() {
    let mut app = app();
    let path = app.state.workspace_root.join("many.txt");
    let before = "needle\n".repeat(3000);
    fs::write(&path, &before).unwrap();
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    assert_eq!(app.search.matches().len(), 3000);
    assert_eq!(app.workspace_edits.proposal().unwrap().files.len(), 1);
    app.accept_workspace_proposal();
    wait_workspace_approval(&mut app);
    let buffer = app.state.buffers.values().find(|buffer| buffer.path.as_ref() == Some(&path)).unwrap();
    assert_eq!(buffer.text.to_string(), "changed\n".repeat(3000));
    assert_eq!(fs::read_to_string(path).unwrap(), before);
}

#[test]
fn workspace_replacement_previews_without_disk_mutation_and_checked_save_rejects_conflict() {
    let mut app = app();
    let first = app.state.workspace_root.join("a.txt");
    let second = app.state.workspace_root.join("b.txt");
    fs::write(&first, "needle one\n").expect("first");
    fs::write(&second, "two needle\n").expect("second");
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    let proposal = app.workspace_edits.proposal().expect("preview");
    assert_eq!(proposal.files.len(), 2);
    assert_eq!(
        fs::read_to_string(&first).expect("unchanged"),
        "needle one\n"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("unchanged"),
        "two needle\n"
    );

    fs::write(&first, "external\n").expect("external change");
    app.accept_workspace_proposal();
    wait_workspace_approval(&mut app);
    assert_eq!(
        fs::read_to_string(&first).expect("still external"),
        "external\n"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("still source"),
        "two needle\n"
    );
    let (first_buffer, _) = app
        .state
        .buffers
        .iter()
        .find(|(_, buffer)| buffer.path.as_ref() == Some(&first))
        .expect("hidden first buffer");
    app.state.show_buffer(first_buffer).expect("show first");
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert_eq!(
        fs::read_to_string(&first).expect("conflict preserved"),
        "external\n"
    );
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("externally modified"))
    );
}

#[test]
fn workspace_replacement_rejects_open_buffer_that_differs_from_search_snapshot() {
    let mut app = app();
    let path = app.state.workspace_root.join("stale.txt");
    fs::write(&path, "needle\n").expect("disk");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("prefix needle\n"),
        Some(path),
    ).unwrap();
    app.state.buffers[buffer].dirty = true;
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    assert!(app.workspace_edits.proposal().is_none());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "prefix needle\n"
    );
    assert!(app.ui.notification_error);
}

#[test]
fn save_all_retries_documents_beyond_the_request_capacity() {
    let mut app = app();
    let mut saved = Vec::new();
    for index in 0..12 {
        let path = app.state.workspace_root.join(format!("save-all-{index}.txt"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
        app.replace_with_recovered(buffer, format!("new {index}")).unwrap();
        saved.push((buffer, path, format!("new {index}")));
    }
    app.process_effect(AppEffect::SaveAll { quit_after: true });
    assert!(!app.pending_saves.is_empty());
    assert!(!app.should_quit);
    wait_vfs(&mut app);
    assert!(app.pending_saves.is_empty());
    for (buffer, path, expected) in saved {
        assert_eq!(fs::read_to_string(path).unwrap(), expected);
        assert!(!app.state.buffers[buffer].dirty);
    }
    assert!(app.should_quit);
}

#[test]
fn save_as_reload_close_and_external_change_protection_work() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None).unwrap();
    app.set_dashboard_visibility(false);
    app.start_save(false, Some(PathBuf::from("missing/new.txt")), false);
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("can't save new.txt: the folder missing doesn't exist")
    );
    app.start_save(false, Some(PathBuf::from("new.txt")), false);
    wait_vfs(&mut app);
    let path = root.join("new.txt");
    assert_eq!(fs::read(&path).expect("saved"), b"abc");
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));

    fs::write(&path, b"external").expect("external edit");
    app.invoke_insert_text("X".into());
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).expect("not overwritten"), b"external");
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("externally modified"))
    );
    // `:e! new.txt` on the current file takes the disk's text and drops the unsaved edit.
    app.process_effect(AppEffect::ForceOpenPath(PathBuf::from("new.txt")));
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "external");
    app.invoke_insert_text("X".into());

    app.start_reload(false);
    assert_eq!(app.documents.legacy_count(), 0);
    app.start_reload(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "external");
    assert!(!app.state.buffers[buffer].dirty);
    app.close_active_buffer(false);
    assert!(app.ui.dashboard);
    assert!(app.state.active_view.is_none());
}

#[test]
fn dirty_quit_is_blocked_but_forced_quit_succeeds() {
    let mut app = app();
    let (_, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None).unwrap();
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[buffer].dirty = true;
    let quit = crate::editor::CommandInvocation::new("editor.quit", InvocationSource::Test)
        .expect("quit")
        .with_argument("force", CommandValue::Bool(false));
    app.invoke(quit);
    assert!(!app.should_quit);
    let force = crate::editor::CommandInvocation::new("editor.quit", InvocationSource::Test)
        .expect("quit")
        .with_argument("force", CommandValue::Bool(true));
    app.invoke(force);
    assert!(app.should_quit);
}

#[test]
fn search_moves_forward_wraps_and_repeats() {
    let mut app = app();
    let (_, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("one two one"),
        None,
    ).unwrap();
    app.set_dashboard_visibility(false);
    app.ui.search_query = Some("one".into());
    app.perform_search(true);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
    app.perform_search(true);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(0)
    );
    app.perform_search(false);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
}

#[cfg(unix)]
#[test]
fn successful_task_dependencies_publish_their_matched_diagnostics() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "source\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    let dependency = crate::config::TaskConfig {
        kind: crate::config::TaskKind::Task,
        executable: "/bin/sh".into(),
        arguments: vec!["-c".into(), "printf 'a.txt:1:warning:dependency warning\\n'".into()],
        cwd: None, environment: BTreeMap::new(), dependencies: vec![],
        timeout_ms: Some(2000), stdin: false,
        matcher: Some(crate::config::ProblemMatcherConfig {
            pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<severity>[^:]+):(?P<message>[^\n]+)$".into(),
            path_group: "path".into(), line_group: "line".into(), column_group: None,
            column_encoding: None, severity_group: Some("severity".into()),
            message_group: "message".into(), name_group: None, status_group: None,
            duration_group: None,
        }),
    };
    let mut parent = dependency.clone();
    parent.arguments = vec!["-c".into(), "printf 'a.txt:1:warning:parent warning\\n'; printf done > parent-finished".into()];
    parent.dependencies = vec!["dependency".into()];
    loaded.config.tasks.insert("dependency".into(), dependency);
    loaded.config.tasks.insert("parent".into(), parent);
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard { root: root.path().into() }, loaded, false,
        state_dir.path().into(), false, false, std::sync::Arc::new(crate::clock::SystemClock),
    ).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TaskRun("parent".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("parent-finished").exists() || app.diagnostics.choices().count() < 2 {
        app.process_execution_preflight();
        app.process_job_events();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "{:?}", app.ui.notification);
        thread::sleep(Duration::from_millis(2));
    }
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.label.contains("dependency warning")), "{rows:?}");
    assert!(rows.iter().any(|row| row.label.contains("parent warning")), "{rows:?}");
}

#[cfg(unix)]
#[test]
fn task_formatter_uses_authorized_worker_and_refuses_reloaded_snapshot() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "owned-dependency".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".into(),
                "printf dependency-output; printf ok >> dependency-runs".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: None,
        },
    );
    loaded.config.tasks.insert(
        "owned-format".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Formatter,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".into(),
                "IFS= read -r input || :; printf ready > ready; while [ ! -f go ]; do :; done; if [ -f invalid ]; then printf '\\377'; else printf XYZ; fi".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec!["owned-dependency".into()],
            timeout_ms: Some(2000),
            stdin: true,
            matcher: None,
        },
    );
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    let path = root.path().join("source.txt");
    fs::write(&path, "abc").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), Some(path.clone())).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    for stale in [true, false] {
        if stale {
            fs::write(root.path().join("invalid"), "").unwrap();
        }
        app.process_effect(AppEffect::FormatRun("owned-format".into()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root.path().join("ready").exists() && Instant::now() < deadline {
            app.process_execution_preflight();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            root.path().join("ready").exists(),
            "{:?}",
            app.ui.notification
        );
        if stale {
            app.state.buffers[buffer].text = TextBuffer::from_text("abc");
        }
        fs::write(root.path().join("go"), "").unwrap();
        while app.tasks.running_count() != 0 && Instant::now() < deadline {
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.tasks.running_count(), 0);
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            if stale { "abc" } else { "XYZ" },
            "notification: {:?}; history: {:?}",
            app.ui.notification,
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            assert!(
                app.ui
                    .notification
                    .as_ref()
                    .is_some_and(|message| message.contains("formatter document incarnation")),
                "{:?}",
                app.ui.notification
            );
            fs::remove_file(root.path().join("invalid")).unwrap();
        }
        fs::remove_file(root.path().join("ready")).unwrap();
        fs::remove_file(root.path().join("go")).unwrap();
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "abc");
    assert_eq!(
        fs::read_to_string(root.path().join("dependency-runs")).unwrap(),
        "okok"
    );

    // Restart formats the original document's current text, not the newly focused buffer.
    app.state.buffers[buffer].text.restore_text("again").unwrap();
    let (other, view) = app.state.open_text(TextBuffer::from_text("leave alone"), None).unwrap();
    app.reconcile_editor_windows();
    fs::write(root.path().join("go"), "").unwrap();
    app.process_effect(AppEffect::TaskRestart("owned-format".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.is_running("owned-format") && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!app.tasks.is_running("owned-format"));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XYZ");
    assert_eq!(app.state.buffers[other].text.to_string(), "leave alone");
    assert_eq!(app.state.active_view, Some(view));
    assert_eq!(fs::read_to_string(root.path().join("dependency-runs")).unwrap(), "okokok");
}

#[cfg(unix)]
#[test]
fn a_failed_test_is_listed_in_problems_and_jumps_to_its_line() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "one\ntwo\néX\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "unit".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Test,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'FAILED adds a.txt:3:2 wrong sum\nok reads a.txt:1:1 fine\n'".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<status>\S+) (?P<name>\S+) (?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+) (?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: Some(crate::editor::text::PositionEncoding::Utf32),
                severity_group: None,
                message_group: "message".into(),
                name_group: Some("name".into()),
                status_group: Some("status".into()),
                duration_group: None,
            }),
        },
    );
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TestRun("unit".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.test_items("unit").is_empty() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "a.txt:3  adds: wrong sum");
    app.accept_picker();
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view.expect("view")];
    let buffer = &app.state.buffers[view.buffer];
    assert_eq!(
        buffer.path.as_deref(),
        Some(root.path().join("a.txt").as_path())
    );
    assert_eq!(buffer.text.byte_to_line(view.cursor).unwrap(), 2);
    assert_eq!(view.cursor.get(), buffer.text.line_to_byte(2).unwrap().get() + "é".len());

    // Restarting the finished task runs it again without claiming an error from the stop half.
    app.process_effect(AppEffect::TaskRestart("unit".into()));
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(app.tasks.test_items("unit").is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.test_items("unit").is_empty() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.tasks.test_items("unit").len(), 2);
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "a.txt:3  adds: wrong sum");
}

#[cfg(unix)]
#[test]
fn a_task_inherits_the_shell_environment_and_its_own_settings_win() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "env".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf '%s|%s' \"$PATH\" \"$HOME\" > env.tmp; mv env.tmp env.txt".into(),
            ],
            cwd: None,
            environment: BTreeMap::from([("HOME".to_owned(), "/task-home".to_owned())]),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: None,
        },
    );
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TaskRun("env".into()));
    let output = root.path().join("env.txt");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.exists() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    let path = std::env::var("PATH").unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        format!("{path}|/task-home")
    );
}

#[cfg(unix)]
#[test]
fn linter_job_and_vfs_publish_only_original_source_then_recover() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("open.txt"), "abc\n").unwrap();
    fs::write(root.path().join("disk.txt"), "abc\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "owned-lint".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Linter,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'open.txt:1:1:open message\ndisk.txt:1:1:disk message\n'".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+):(?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: None,
                severity_group: None,
                message_group: "message".into(),
                name_group: None,
                status_group: None,
                duration_group: None,
            }),
        },
    );
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("abc\n"),
        Some(root.path().join("open.txt")),
    ).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    for stale in [true, false] {
        app.process_effect(AppEffect::LintRun("owned-lint".into()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.tasks.linter_count() == 0 && Instant::now() < deadline {
            app.process_execution_preflight();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            app.tasks.linter_count(),
            1,
            "{:?}",
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            app.state.buffers[buffer].text = TextBuffer::from_text("abc\n");
        }
        let before = TextBuffer::test_materializations();
        while app.tasks.linter_count() != 0 && Instant::now() < deadline {
            app.process_vfs_results();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.tasks.linter_count(), 0);
        assert_eq!(
            app.diagnostics.choices().count(),
            if stale { 0 } else { 2 },
            "{:?}",
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            assert_eq!(TextBuffer::test_materializations(), before);
        }
    }
    assert!(app.diagnostics.choices().all(|(_, _, validity)| validity
        == crate::editor::session::diagnostics::DiagnosticValidity::DiskReport));
    assert_eq!(
        fs::read_to_string(root.path().join("open.txt")).unwrap(),
        "abc\n"
    );
}

#[test]
fn a_task_with_a_matcher_puts_what_it_matched_in_problems() {
    check_task_diagnostics(false);
}

#[test]
fn successful_dependency_diagnostics_keep_separate_stream_records() {
    check_task_diagnostics(true);
}

fn check_task_diagnostics(with_dependency: bool) {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("notes.txt"), "one\ntwo\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "build".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'notes.txt:2:1: error: broken\n'; exit 1".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+): (?P<severity>\w+): (?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: None,
                severity_group: Some("severity".into()),
                message_group: "message".into(),
                name_group: None,
                status_group: None,
                duration_group: None,
            }),
        },
    );
    if with_dependency {
        let mut dependency = loaded.config.tasks["build"].clone();
        dependency.arguments = vec!["-c".into(), "printf 'notes.txt:1:1: warning: stdout'; exec 1>&-; printf 'notes.txt:2:1: warning: stderr' >&2".into()];
        loaded.config.tasks.insert("check".into(), dependency);
        let build = loaded.config.tasks.get_mut("build").unwrap();
        build.dependencies = vec!["check".into()];
        build.arguments = vec!["-c".into(), "printf 'notes.txt:2:1: warning: main task\\n'".into()];
    }
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TaskRun("build".into()));
    let deadline = Instant::now() + Duration::from_secs(3);
    let expected = if with_dependency { 3 } else { 1 };
    while (app.diagnostics.choices().count() < expected || app.tasks.is_running("build"))
        && Instant::now() < deadline
    {
        app.process_execution_preflight();
        app.process_job_events();
        app.process_vfs_results();
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.diagnostics.choices().count(),
        expected,
        "{:?}",
        app.notices.history().collect::<Vec<_>>()
    );
    // A build that exits 1 still failed, unlike a linter reporting findings.
    let status = if with_dependency { "succeeded" } else { "failed" };
    assert!(
        app.tasks
            .task_items()
            .iter()
            .any(|item| item.label == "build" && item.detail == status)
    );
    if with_dependency {
        let messages = app.diagnostics.choices().map(|(_, diagnostic, _)| diagnostic.message.as_str()).collect::<Vec<_>>();
        assert!(messages.contains(&"stdout"));
        assert!(messages.contains(&"stderr"));
        assert!(messages.contains(&"main task"));
    }
}

fn index_bytes(root: &std::path::Path, name: &str) -> String {
    let repo = git2::Repository::open(root).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
    String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
}

#[test]
fn named_staging_commands_change_only_the_index() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("commands.txt");
    let original = "a\nb\nc\nd\ne\n";
    fs::write(&path, original).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("commands.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = "a\nB\nc\nNEW\nd\ne\n";
    fs::write(&path, changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let invoke = |app: &mut InteractiveApplication, id: &str, args: &[(&str, i64)]| {
        let mut invocation =
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id");
        for (name, value) in args {
            invocation = invocation.with_argument(name, CommandValue::Integer(*value));
        }
        app.invoke(invocation);
    };
    // A completed mutation clears the session's status snapshot, so the follow-up refresh
    // must also be drained before the next command has a captured state to plan against.
    let wait_mutation = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Whole-file stage moves the index and never the worktree.
    invoke(&mut app, "workspace.git.file.stage", &[]);
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        changed
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // Whole-file unstage returns the index entry to HEAD and leaves the worktree alone.
    invoke(&mut app, "workspace.git.file.unstage", &[]);
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        original
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // A line range needs loaded hunks; while they load nothing is written.
    let before = index_bytes(&app.state.workspace_root, "commands.txt");
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", 0), ("end", 0)],
    );
    assert!(!app.git.mutation_pending());
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        before
    );

    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.hunks().unwrap().clone();
    assert_eq!(snapshot.hunks.len(), 1, "fixture must produce one hunk");
    assert!(app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision,
        &path,
        snapshot.hunks[0].id
    ));

    // A reversed range is refused and writes nothing.
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", 3), ("end", 1)],
    );
    assert!(!app.git.mutation_pending());
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        original
    );

    // The chosen line replaces the line it changed, and nothing else reaches the index.
    let add = snapshot.hunks[0]
        .lines
        .iter()
        .find(|line| line.kind == crate::workspace::git::GitDiffLineKind::Add)
        .and_then(|line| line.new_line)
        .expect("fixture adds a line") as i64;
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", add), ("end", add)],
    );
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        "a\nB\nc\nd\ne\n"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[test]
fn staging_an_unsaved_buffer_offers_an_explicit_choice() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("choice.txt");
    fs::write(&path, "disk\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("choice.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The buffer holds text that is not on disk, at a non-zero revision.
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    assert!(app.state.buffers[id].text.revision().0 > 0);

    let wait_for_choice = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            app.process_git_mutation_events();
            if app
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::GitStageUnsaved)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no save-and-stage choice appeared"
            );
            thread::yield_now()
        }
    };

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    wait_for_choice(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No,
        "the choice defaults to No"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );

    // Negative branch: nothing is saved and nothing is staged.
    app.submit_prompt();
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );
    assert!(app.state.buffers[id].dirty);
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");

    // Positive branch: save, then stage the saved revision rather than the old disk bytes.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    wait_for_choice(&mut app);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "buffer\n",
        "the save reached disk"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "buffer\n",
        "the saved revision was staged, not the previous disk content"
    );
    assert!(!app.state.buffers[id].dirty);
}

#[test]
fn status_reports_a_path_whose_only_change_is_an_unsaved_buffer() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("unsaved-only.txt");
    fs::write(&path, "disk\n").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("unsaved-only.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    let revision = app.state.buffers[id].text.revision().0;
    assert!(revision > 0);

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.status().expect("status snapshot").clone();
    // The file matches HEAD on disk, so libgit2 reports no change at all. The entry exists
    // only because the refresh merges unsaved editor content as a third source.
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == std::path::Path::new("unsaved-only.txt"))
        .expect("unsaved-only path must appear in status");
    assert_eq!(entry.sources.unsaved_document_revision, Some(revision));
    assert!(!entry.sources.staged);
    assert!(!entry.sources.unstaged);
}

#[test]
fn git_change_source_labels_cover_every_combination() {
    use crate::workspace::git::{GitChangeSource, GitChangeSources};
    let label = |staged, unstaged, unsaved: Option<u64>| {
        GitChangeSources {
            staged,
            unstaged,
            unsaved_document_revision: unsaved,
        }
        .source_label()
    };
    assert_eq!(label(true, false, None), "staged");
    assert_eq!(label(false, true, None), "unstaged");
    assert_eq!(label(false, false, Some(4)), "unsaved");
    assert_eq!(label(true, true, None), "staged+unstaged");
    assert_eq!(label(true, false, Some(4)), "staged+unsaved");
    assert_eq!(label(false, true, Some(4)), "unstaged+unsaved");
    assert_eq!(label(true, true, Some(4)), "staged+unstaged+unsaved");
    assert_eq!(label(false, false, None), "unchanged");
    assert_eq!(GitChangeSource::Staged.source_label(), "staged");
    assert_eq!(GitChangeSource::Unstaged.source_label(), "unstaged");
    assert_eq!(
        GitChangeSource::Unsaved {
            document_revision: 9
        }
        .source_label(),
        "unsaved"
    );
}

#[test]
fn git_status_rows_label_each_source_without_conflating_them() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let original = "one\n";
    for name in ["staged.txt", "unstaged.txt", "unsaved.txt", "both.txt"] {
        fs::write(app.state.workspace_root.join(name), original).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    {
        let mut index = repo.index().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
    }
    // staged: index differs from HEAD, worktree matches the index.
    fs::write(app.state.workspace_root.join("staged.txt"), "two\n").unwrap();
    // both: staged and then modified again.
    fs::write(app.state.workspace_root.join("both.txt"), "two\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("staged.txt")).unwrap();
        index.add_path(std::path::Path::new("both.txt")).unwrap();
        index.write().unwrap();
    }
    fs::write(app.state.workspace_root.join("both.txt"), "three\n").unwrap();
    // unstaged: index matches HEAD, worktree differs.
    fs::write(app.state.workspace_root.join("unstaged.txt"), "two\n").unwrap();
    // unsaved: nothing differs on disk; only the buffer holds a change.
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let projection = app.search.picker_projection().expect("status picker rows");
    let section = |title: &str| {
        let start = projection
            .rows
            .iter()
            .position(|row| {
                // A header carries its count, as the document draws it: `── Staged 2 ──`.
                row.kind == crate::ui::PickerRowKind::Section
                    && row
                        .label
                        .strip_prefix(title)
                        .is_some_and(|rest| rest.starts_with(' '))
            })
            .unwrap_or_else(|| panic!("no {title} section"));
        projection.rows[start + 1..]
            .iter()
            .take_while(|row| row.kind == crate::ui::PickerRowKind::Entry)
            .collect::<Vec<_>>()
    };
    let provenance_of = |title: &str, name: &str| {
        section(title)
            .into_iter()
            .find(|row| row.label == name)
            .unwrap_or_else(|| panic!("no {name} row in {title}"))
            .detail
            .clone()
    };
    // Every row names the comparison it belongs to, so an unstaged row is never read as
    // unsaved and a staged row is never read as unstaged.
    assert_eq!(provenance_of("Staged", "staged.txt"), "HEAD -> index");
    assert_eq!(provenance_of("Unstaged", "unstaged.txt"), "index -> disk");
    // A path that is both staged and further modified appears in BOTH sections, each row
    // naming its own comparison, rather than one row conflating them.
    assert_eq!(provenance_of("Staged", "both.txt"), "HEAD -> index");
    assert_eq!(provenance_of("Unstaged", "both.txt"), "index -> disk");
    // A staged path is not listed as unstaged.
    assert!(
        !section("Unstaged")
            .iter()
            .any(|row| row.label == "staged.txt")
    );
    assert!(
        !section("Staged")
            .iter()
            .any(|row| row.label == "unsaved.txt")
    );
    // A path with only unsaved edits is still listed, under its OWN provenance rather than
    // as an unstaged change, and carries the badge as its own drawn column.
    let unsaved = section("Unstaged")
        .into_iter()
        .find(|row| row.label == "unsaved.txt")
        .expect("a path with unsaved edits is still listed");
    assert_eq!(
        unsaved.detail, "buffer -> disk",
        "unsaved is not labelled unstaged"
    );
    assert!(
        unsaved
            .cells
            .iter()
            .any(|cell| cell.text.trim() == "unsaved"),
        "an unsaved path carries its badge column: {:?}",
        unsaved
            .cells
            .iter()
            .map(|cell| &cell.text)
            .collect::<Vec<_>>()
    );
    // Sections are drawn in the order the document names and are absent when empty.
    let titles: Vec<&str> = projection
        .rows
        .iter()
        .filter(|row| row.kind == crate::ui::PickerRowKind::Section)
        .map(|row| row.label.split(' ').next().unwrap_or_default())
        .collect();
    // Conflicts, Unstaged and Staged are drawn in this order and are absent when empty.
    let expected: Vec<&str> = ["Conflicts", "Unstaged", "Staged"]
        .into_iter()
        .filter(|title| titles.contains(title))
        .collect();
    assert_eq!(
        titles, expected,
        "sections appear in the drawn order, absent when empty"
    );
}

#[test]
fn staged_hunk_rows_are_labelled_staged_and_unstaged_rows_are_not() {
    fn last_segment(detail: &str) -> String {
        detail.rsplit(" · ").next().unwrap_or_default().to_owned()
    }
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("layers.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("layers.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let open_hunks = |app: &mut InteractiveApplication, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let row_labels = |app: &InteractiveApplication| {
        app.search
            .picker_projection()
            .expect("hunk picker rows")
            .rows
            .iter()
            .map(|row| last_segment(&row.detail))
            .collect::<Vec<_>>()
    };

    // Nothing is staged yet, so every row describes an unstaged change.
    open_hunks(&mut app, "workspace.git.hunks.open");
    let unstaged = row_labels(&app);
    assert_eq!(unstaged.len(), 2);
    assert!(
        unstaged.iter().all(|label| label == "unstaged"),
        "unstaged rows must not claim another source: {unstaged:?}"
    );

    // Stage the whole file, then the same rows describe the staged layer.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("command id"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    open_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = row_labels(&app);
    assert_eq!(staged.len(), 2);
    assert!(
        staged.iter().all(|label| label == "staged"),
        "staged rows must be labelled staged: {staged:?}"
    );
}

#[test]
fn hunk_unstage_returns_one_staged_hunk_to_head_and_refuses_the_rest() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("unstage.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("unstage.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let drain = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let load_hunks = |app: &mut InteractiveApplication, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        invoke(app, id);
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    invoke(&mut app, "workspace.git.file.stage");
    drain(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        changed
    );

    // Unstage exactly the first staged hunk.
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = app.git.hunks().expect("staged hunks").clone();
    assert_eq!(staged.hunks.len(), 2);
    assert!(app.git.select(
        staged.status_revision,
        staged.repository_id,
        staged.root_revision,
        &path,
        staged.hunks[0].id
    ));
    invoke(&mut app, "workspace.git.hunk.unstage");
    drain(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        original.replace("line 38\n", "changed 38\n"),
        "only the selected staged hunk returned to HEAD"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        changed,
        "unstaging never writes the worktree"
    );
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    assert_eq!(
        app.git.hunks().expect("staged hunks").hunks.len(),
        1,
        "the other staged hunk stays staged"
    );

    // Unstaging where only an unstaged hunk is selected finds no staged hunk on the line,
    // refuses and writes nothing.
    let before = index_bytes(&app.state.workspace_root, "unstage.txt");
    load_hunks(&mut app, "workspace.git.hunks.open");
    let unstaged = app.git.hunks().expect("unstaged hunks").clone();
    assert_eq!(unstaged.hunks.len(), 1);
    assert!(app.git.select(
        unstaged.status_revision,
        unstaged.repository_id,
        unstaged.root_revision,
        &path,
        unstaged.hunks[0].id
    ));
    load_hunks(&mut app, "workspace.git.hunk.unstage");
    assert!(!app.git.mutation_pending(), "no plan may start");
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("no staged Git hunk on this line")
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        before
    );

    // Staging a hunk that is already staged is refused, not reapplied.
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = app.git.hunks().expect("staged hunks").clone();
    assert!(app.git.select(
        staged.status_revision,
        staged.repository_id,
        staged.root_revision,
        &path,
        staged.hunks[0].id
    ));
    invoke(&mut app, "workspace.git.hunk.stage");
    assert!(!app.git.mutation_pending(), "no plan may start");
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        before
    );
}

#[test]
fn staging_one_of_two_changed_lines_keeps_the_file_in_order() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("pair.txt");
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("pair.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 10\n", "changed 10\n")
        .replace("line 11\n", "changed 11\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let settle = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_hunk_events();
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    app.start_git_status_refresh();
    settle(&mut app);
    let mut invocation =
        crate::editor::CommandInvocation::new("workspace.git.lines.stage", InvocationSource::Test)
            .expect("command id");
    for (name, value) in [("start", 11), ("end", 11)] {
        invocation
            .arguments
            .insert(name.into(), crate::editor::CommandValue::Integer(value));
    }
    app.invoke(invocation);
    settle(&mut app);
    let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(std::path::Path::new("pair.txt"), 0).unwrap();
    assert_eq!(
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap(),
        original.replace("line 11\n", "changed 11\n"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn the_unstage_key_undoes_the_hunk_just_staged_under_the_cursor() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("undo.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("undo.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let settle = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_hunk_events();
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let indexed = |app: &InteractiveApplication| {
        let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new("undo.txt"), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    };
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    app.start_git_status_refresh();
    settle(&mut app);

    invoke(&mut app, "workspace.git.hunk.stage");
    settle(&mut app);
    assert_eq!(indexed(&app), original.replace("line 2\n", "changed 2\n"));

    invoke(&mut app, "workspace.git.hunk.unstage");
    settle(&mut app);
    assert_eq!(indexed(&app), original, "{:?}", app.ui.notification);
}

/// Build a repository with distinct commit subjects and a configured identity.
fn history_fixture(root: &std::path::Path, subjects: &[&str]) -> git2::Repository {
    let repo = git2::Repository::init(root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config
            .set_str("user.email", "dust@example.invalid")
            .unwrap();
    }
    for (index, subject) in subjects.iter().enumerate() {
        let name = format!("h{index}.txt");
        fs::write(root.join(&name), "x\n").unwrap();
        let mut index_file = repo.index().unwrap();
        index_file.add_path(std::path::Path::new(&name)).unwrap();
        index_file.write().unwrap();
        let tree = repo.find_tree(index_file.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust Fixture", "dust@example.invalid").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, subject, &tree, &parents)
            .unwrap();
    }
    // A clone so the caller keeps a handle whose index is not cached stale.
    git2::Repository::open(root).unwrap()
}

#[test]
fn a_buffer_stays_bound_to_the_worktree_it_was_opened_in() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    // Two linked worktrees, both OUTSIDE the main worktree so the main worktree stays clean.
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let mut roots = Vec::new();
    for name in ["alpha", "beta"] {
        let branch = repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        let path = outside.join(name);
        repo.worktree(name, &path, Some(&options)).unwrap();
        fs::write(path.join("note.txt"), name).unwrap();
        roots.push(path);
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    for (name, root) in ["alpha", "beta"].into_iter().zip(&roots) {
        app.workspace.fixture_add_root(
            name.into(),
            crate::workspace::RootIdentity::capture(root).unwrap(),
        ).unwrap();
    }
    let open_note = |app: &mut InteractiveApplication, root: &std::path::Path| {
        app.open_explorer();
        app.explorer.select(app.explorer.tree().id_for_path(root).expect("worktree root"));
        app.explorer_toggle();
        wait_explorer(app);
        let path = root.join("note.txt");
        app.explorer.select(app.explorer.tree().id_for_path(&path).expect("note in explorer"));
        app.explorer_open_file(false);
        wait_vfs(app);
        let buffer = app.state.active_buffer_id().expect("opened note buffer");
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
        buffer
    };

    let alpha_file = roots[0].join("note.txt");
    let beta_file = roots[1].join("note.txt");
    let alpha_buffer = open_note(&mut app, &roots[0]);
    assert_eq!(app.state.buffers[alpha_buffer].text.to_string(), "alpha");
    // Documents are bound by their stored paths, not a separate Git identity cache.
    let alpha = app.state.buffers[alpha_buffer].path.clone().unwrap();
    assert_eq!(
        alpha.parent().unwrap(),
        std::fs::canonicalize(&roots[0]).unwrap(),
        "the buffer path names the worktree the document was opened in"
    );

    let beta_buffer = open_note(&mut app, &roots[1]);
    assert_ne!(alpha_buffer, beta_buffer);
    assert_eq!(app.state.buffers[beta_buffer].text.to_string(), "beta");
    assert_eq!(app.state.buffers[alpha_buffer].text.to_string(), "alpha");
    assert_eq!(app.state.buffers[alpha_buffer].path.as_ref(), Some(&alpha_file));
    let beta = app.state.buffers[beta_buffer].path.clone().unwrap();
    assert_eq!(beta, beta_file);
    assert_eq!(beta.parent().unwrap(), std::fs::canonicalize(&roots[1]).unwrap());
    assert_ne!(alpha, beta, "two linked worktrees carry distinct buffer paths");
    assert_eq!(
        app.state.buffers[alpha_buffer].path.as_ref(),
        Some(&alpha),
        "opening another worktree never retargets an existing buffer"
    );

    assert!(
        app.state.buffers.values().all(|buffer| {
            buffer.path.as_deref() != Some(std::path::Path::new("/nonexistent-dust/orphan.txt"))
        }),
        "a nonexistent, unopened document has no buffer binding"
    );
    // A file under a repository has no buffer binding until this session opens it.
    let unopened = app.state.workspace_root.join("never-opened.txt");
    fs::write(&unopened, "x").unwrap();
    assert!(
        app.state.buffers.values().all(|buffer| buffer.path.as_ref() != Some(&unopened)),
        "an unopened document has no buffer binding"
    );
}

#[test]
fn switching_to_a_worktree_moves_the_editor_there() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let worktree = outside.join("scoped");
    {
        let branch = repo.branch("scoped", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("scoped", &worktree, Some(&options)).unwrap();
    }
    fs::write(worktree.join("note.txt"), "scoped").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let main_root = std::fs::canonicalize(&app.state.workspace_root).unwrap();
    let main_file = main_root.join("h0.txt");
    let (main_buffer, main_view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_bytes(b"x\n").expect("text"),
        Some(main_file.clone()),
    ).unwrap();
    app.state.active_view = Some(main_view);

    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let open_worktree_row = |app: &mut InteractiveApplication| {
        // A trust reconciliation bumps the workspace version and retires the previous status
        // snapshot, so the picker's own refresh has to be drained on every open.
        let deadline = Instant::now() + Duration::from_secs(10);
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() {
            app.process_git_branch_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        let projection = app
            .search
            .picker_projection()
            .unwrap_or_else(|| panic!("worktrees picker missing: {:?}", app.ui.notification));
        let index = projection
            .rows
            .iter()
            .position(|row| row.label.starts_with("scoped"))
            .unwrap_or_else(|| panic!("no scoped row in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };

    let switch = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(10);
        invoke(app, "workspace.git.worktree.open");
        while app.workspace.root_operation_pending() {
            app.process_trust_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Unsaved changes in the current root block the switch.
    app.state.buffers[main_buffer].dirty = true;
    open_worktree_row(&mut app);
    switch(&mut app);
    assert!(
        app.ui.notification_error,
        "unsaved changes block the switch"
    );
    assert_eq!(
        app.workspace.roots()[0].identity.canonical_path,
        main_root,
        "a blocked switch leaves the root alone"
    );
    app.state.buffers[main_buffer].dirty = false;

    // The worktree replaces the root, and clean buffers from the old root close.
    open_worktree_row(&mut app);
    switch(&mut app);
    let canonical = std::fs::canonicalize(&worktree).unwrap();
    assert_eq!(app.workspace.roots().len(), 1, "a switch replaces the root");
    assert_eq!(app.workspace.roots()[0].identity.canonical_path, canonical);
    assert_eq!(app.state.workspace_root, canonical);
    assert!(
        !app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_deref() == Some(main_file.as_path())),
        "clean buffers from the old root close"
    );
    wait_vfs(&mut app);
    let reopened = canonical.join("h0.txt");
    assert!(
        app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_deref() == Some(reopened.as_path())),
        "the file in front reopens in the new worktree"
    );
    assert!(
        worktree.join("note.txt").is_file() && main_file.is_file(),
        "switching never touches either checkout"
    );
}

#[test]
fn primary_worktree_and_linked_main_have_distinct_picker_entries() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let mut branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    if branch == "main" {
        repo.find_branch("main", git2::BranchType::Local).unwrap()
            .rename("trunk", false).unwrap();
        branch = "trunk".into();
        repo.set_head("refs/heads/trunk").unwrap();
    }
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let linked = outside.path().join("main");
    let reference = repo.branch("side", &head, false).unwrap().into_reference();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repo.worktree("main", &linked, Some(&options)).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.start_git_worktrees();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let rows = app.search.picker_projection().unwrap().rows;
    assert_eq!(rows.iter().filter(|row| row.label == "main").count(), 2);
    let primary = rows.iter().position(|row| row.id == "git-worktree:primary").unwrap();
    assert!(rows.iter().any(|row| row.id == "git-worktree:linked:main"));
    for _ in 0..primary {
        app.invoke(crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test).unwrap());
    }
    app.request_git_worktree_remove();
    assert!(app.ui.notification_error);
    assert!(app.ui.notification.as_deref().unwrap_or_default().contains("primary"));
    assert!(app.git.take_pending_destructive().is_none());
    assert!(!app.git.branch_pending());
    assert!(linked.join("h0.txt").is_file());
    assert!(repo.find_worktree("main").is_ok());
    assert_eq!(repo.head().unwrap().shorthand(), Some(branch.as_str()));
}

#[test]
fn removing_the_main_checkout_cannot_remove_a_linked_worktree_named_main() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let outside = tempdir().unwrap();
    let linked = outside.path().join("linked");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = repo.branch("linked-main", &head, false).unwrap().into_reference();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&branch));
    repo.worktree("main", &linked, Some(&options)).unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.start_git_worktrees();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let rows = app.search.picker_projection().unwrap().rows;
    assert_eq!(rows.iter().filter(|row| row.label == "main").count(), 2);
    let index = rows.iter().position(|row| row.detail.contains(" · main")).unwrap();
    for _ in 0..index {
        app.invoke(crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test).unwrap());
    }
    app.request_git_worktree_remove();
    assert!(app.input.prompt().is_none());
    assert!(app.git.take_pending_destructive().is_none());
    assert!(app.ui.notification.as_ref().unwrap().contains("main checkout"));
    assert!(linked.is_dir());
    assert!(repo.find_worktree("main").is_ok());
}

#[test]
fn a_locked_worktree_refuses_removal() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let worktree = outside.join("locked");
    {
        let branch = repo.branch("locked", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("locked", &worktree, Some(&options)).unwrap();
    }
    let locked = repo.find_worktree("locked").unwrap();
    locked.lock(Some("held by another tool")).unwrap();
    drop(locked);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let select_worktree_row = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(10);
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() {
            app.process_git_branch_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        let projection = app.search.picker_projection().expect("worktrees picker");
        let index = projection
            .rows
            .iter()
            .position(|row| row.label.starts_with("locked"))
            .unwrap_or_else(|| panic!("no locked row in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };
    let drain = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Removal is refused through the command and names the lock.
    select_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.remove");
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    drain(&mut app);
    assert!(
        app.ui.notification_error,
        "a locked worktree refuses removal through the command"
    );
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("locked"),
        "the error names the lock: {notice}"
    );
    assert!(
        worktree.is_dir() && repo.find_worktree("locked").is_ok(),
        "a refused removal leaves the worktree and its registration alone"
    );
    assert!(repo.find_branch("locked", git2::BranchType::Local).is_ok());
}

#[test]
fn help_preserves_a_draft_and_a_pending_confirmation() {
    let mut app = app();
    app.open_prompt(crate::ui::PromptKind::Command, "exploreradd ".into());
    app.prompt_insert("draft-name.txt");
    app.project_input_prompt();
    let typed = app
        .ui
        .prompt
        .as_ref()
        .expect("a prompt is open")
        .input
        .clone();
    assert!(typed.contains("draft-name.txt"));

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    let help = app
        .search
        .picker_projection()
        .expect("contextual help opened");
    assert!(
        help.rows.iter().all(|row| !row.label.is_empty()),
        "the help page lists commands rather than blank rows"
    );
    assert!(
        help.rows
            .iter()
            .any(|row| row.label == "overlay.help.contextual"),
        "help lists itself, so the list comes from the registry: {:?}",
        help.rows.iter().map(|row| &row.label).collect::<Vec<_>>()
    );
    app.search.close_picker();

    app.project_input_prompt();
    let after = app.ui.prompt.as_ref().expect("the prompt survived help");
    assert_eq!(
        after.input, typed,
        "help preserves the draft exactly, character for character"
    );
    // Pending consent survives help too. A confirmation is its own prompt, so it is opened
    // separately rather than assumed to sit alongside the draft above.
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Confirm".into(),
        "Discard the draft?".into(),
        crate::ui::ConfirmationChoice::Yes,
    );
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    app.search.close_picker();
    app.project_input_prompt();
    let pending = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("the pending confirmation survived help");
    assert_eq!(
        pending.selected,
        crate::ui::ConfirmationChoice::Yes,
        "help does not answer the pending question for the user"
    );
    assert!(
        app.ui.prompt.as_ref().expect("prompt").input.is_empty(),
        "the confirmation prompt is a new prompt, so help must not have leaked the old draft into it"
    );
}

#[test]
fn help_preserves_a_buffer_selection_byte_for_byte() {
    let mut app = app();
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("alpha beta gamma delta\n"),
        None,
    ).unwrap();
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;
    app.state.views[view].cursor = crate::editor::text::ByteOffset(15);
    app.state.views[view].mode = crate::editor::EditorMode::VisualChar;
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(6),
        head: crate::editor::text::ByteOffset(15),
        kind: crate::editor::SelectionKind::Character,
    });
    let before = app.state.views[view].selection;
    let mode_before = app.state.views[view].mode;

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(app.search.picker_projection().is_some(), "help opened");
    app.search.close_picker();

    assert_eq!(
        app.state.views[view].selection, before,
        "the selection is identical, anchor and head, after help"
    );
    assert_eq!(
        app.state.views[view].mode, mode_before,
        "help does not drop the editor out of visual mode"
    );
}

#[test]
fn closing_help_returns_the_palette_with_its_query_and_selection_intact() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("overlay.help.");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();
    let selected_before = palette.selected.expect("a row is selected");
    let label_before = palette.rows[selected_before].label.clone();
    let rows_before = palette.rows.len();
    assert!(!query_before.is_empty(), "the palette was filtered");

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let reference = app.search.picker_projection().expect("reference page");
    assert_eq!(
        reference.query, "",
        "the help page is its own surface, not a filtered palette"
    );

    app.search.close_picker();

    // MEASURED: closing help restores the surface help was opened over, with its query text,
    // its row count and its selected row all unchanged.
    let restored = app
        .search
        .picker_projection()
        .expect("closing help returned to the palette");
    assert_eq!(
        restored.query, query_before,
        "the palette's query text survived help"
    );
    assert_eq!(
        restored.rows.len(),
        rows_before,
        "the palette's rows survived help"
    );
    assert_eq!(
        restored.selected,
        Some(selected_before),
        "the palette's selected row survived help"
    );
    assert_eq!(
        restored.rows[selected_before].label, label_before,
        "the row selected before help is the row selected after it"
    );

    // A help page opened with no picker underneath restores nothing: the editor-context case.
    app.search.close_picker();
    assert!(app.search.picker_projection().is_none());
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(
        app.search.picker_projection().is_some(),
        "help opened alone"
    );
    app.search.close_picker();
    assert!(
        app.search.picker_projection().is_none(),
        "closing help that had nothing underneath leaves no surface rather than inventing one"
    );
}

#[test]
fn form_help_captures_keys_without_changing_the_prompt_draft() {
    let mut app = app();
    app.open_prompt(crate::ui::PromptKind::Command, "write draft".into());
    app.handle_configured_key("ctrl-g".parse().unwrap());
    assert!(app.search.help_is_open());
    app.handle_configured_key(KeyEvent::character('j'));
    assert_eq!(app.input.prompt().unwrap().input, "write draft");
    app.handle_configured_key("escape".parse().unwrap());
    app.handle_configured_key("escape".parse().unwrap());
    assert!(!app.search.help_is_open());
    assert_eq!(app.input.prompt().unwrap().input, "write draft");
    app.handle_configured_key(KeyEvent::character('x'));
    assert_eq!(app.input.prompt().unwrap().input, "write draftx");
}

#[test]
fn the_confirmation_help_action_is_focusable_advertised_and_leaves_the_question_pending() {
    let mut app = app();
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Discard changes?".into(),
        "The buffer has unsaved edits.".into(),
        crate::ui::ConfirmationChoice::No,
    );
    app.project_input_prompt();
    let before = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .cloned()
        .expect("a confirmation is pending");
    assert_eq!(before.selected, crate::ui::ConfirmationChoice::No);

    // The action is advertised on the confirmation itself, from the keymap like every other
    // action, so a user can see it without knowing it exists.
    assert!(
        before.hint.contains("? help"),
        "the confirmation advertises the Help action: {:?}",
        before.hint
    );

    // Choosing Help opens help ABOUT THIS QUESTION, not a generic page.
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.form", InvocationSource::Test)
            .expect("command id"),
    );
    let help = app.search.picker_projection().expect("form help opened");
    let rows = help
        .rows
        .iter()
        .map(|row| (row.detail.clone(), row.label.clone()))
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "asks" && value == "Discard changes?"),
        "the help names the question being asked: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "detail" && value == "The buffer has unsaved edits."),
        "the help carries the confirmation's own message: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(kind, value)| kind == "choices" && value == "Yes / No"),
        "the help shows the choices: {rows:?}"
    );
    app.search.close_picker();

    // The question is still pending, with the SAME selection: help is not an answer.
    app.project_input_prompt();
    let after = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .cloned()
        .expect("the confirmation survived help");
    assert_eq!(
        after, before,
        "the confirmation is byte-identical after help: same title, message, choices, hint and selection"
    );
}

#[test]
fn the_advertised_help_key_is_the_key_that_resolves() {
    let mut app = app();
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Discard changes?".into(),
        "The buffer has unsaved edits.".into(),
        crate::ui::ConfirmationChoice::No,
    );
    app.project_input_prompt();
    let hint = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("a confirmation is pending")
        .hint
        .clone();

    // The hint advertises "<key> help"; the keymap must resolve that exact key to the command
    // the hint is advertising, so the advertisement and the dispatch cannot drift apart.
    let advertised = hint
        .split(" · ")
        .find_map(|part| part.strip_suffix(" help"))
        .expect("the hint advertises a help key")
        .to_owned();
    let resolved = app
        .configuration
        .keymaps()
        .inspect_key("confirmation_prompt", &advertised)
        .unwrap_or_else(|| panic!("the advertised key {advertised:?} does not resolve"));
    assert_eq!(
        resolved.command.as_deref(),
        Some("overlay.help.form"),
        "the advertised key {advertised:?} dispatches exactly the command the hint names"
    );

    // And the composer form advertises its own key the same way.
    let composer = app
        .configuration
        .keymaps()
        .binding_for("command_prompt", "overlay.help.form")
        .expect("the form has a help binding");
    assert_eq!(composer, "ctrl-g");
    let resolved = app
        .configuration
        .keymaps()
        .inspect_key("command_prompt", composer)
        .expect("the form's advertised key resolves");
    assert_eq!(resolved.command.as_deref(), Some("overlay.help.form"));
}

#[test]
fn accepting_inside_help_closes_the_page_and_invokes_nothing() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();
    let selected_before = palette.selected.expect("a row is selected");

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(
        app.search.picker_projection().is_some(),
        "help opened over the palette"
    );

    // MEASURED: accepting a help row must close the page and return the palette. It must not
    // run the row's command and must not leave the help page on screen.
    app.accept_picker();
    let restored = app
        .search
        .picker_projection()
        .expect("accepting help returned the palette rather than leaving help up");
    assert_eq!(
        restored.query, query_before,
        "the palette came back with its query"
    );
    assert_eq!(
        restored.selected,
        Some(selected_before),
        "the palette came back with its selected row"
    );
    assert_ne!(
        restored.title, "Help · overlay.help.reference",
        "the help page is gone, not re-opened by its own row payload"
    );
}

#[test]
fn a_surface_installed_over_help_discards_the_snapshot() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(app.search.picker_projection().is_some(), "help is up");

    // A real surface is installed while help is open. The stale palette must not come back
    // when that new surface is closed.
    app.open_builtin_picker(crate::editor::PickerKind::Buffers);
    assert!(
        app.search.picker_projection().is_some(),
        "the new surface is up"
    );
    app.search.close_picker();
    assert!(
        app.search.picker_projection().is_none(),
        "closing the new surface leaves nothing: it did not restore the palette help was over"
    );
}

#[test]
fn help_opened_over_help_keeps_the_original_surface() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let first = app
        .search
        .picker_projection()
        .expect("first help page")
        .title
        .clone();
    // A second help page over the first.
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    let second = app
        .search
        .picker_projection()
        .expect("second help page")
        .title
        .clone();
    assert_ne!(first, second, "a different help page is on screen");

    // MEASURED: closing returns the surface the user started from, not the intermediate help
    // page, and the snapshot never becomes a stack.
    app.search.close_picker();
    let restored = app
        .search
        .picker_projection()
        .expect("closing help-over-help returned the original surface");
    assert_eq!(
        restored.query, query_before,
        "the original palette came back with its query"
    );
    assert_ne!(
        restored.title, first,
        "the intermediate help page was dropped rather than stacked"
    );
}

#[test]
fn the_conflict_surface_shows_the_paused_operation_and_each_side() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let base_tip = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base_tip).unwrap();
    // A branch from the base that changes the same path differently, then a real merge.
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo
            .treebuilder(Some(&base_commit.tree().unwrap()))
            .unwrap();
        let blob = repo.blob(b"their version\n").unwrap();
        builder.insert("h0.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    // Ours changes the same file, so the merge conflicts.
    fs::write(root.join("h0.txt"), "our version\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("h0.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();
    let their_commit = repo.find_commit(their_tip).unwrap();
    let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
    repo.merge(&[&annotated], None, None).unwrap();
    drop(annotated);
    drop(their_commit);
    drop(base_commit);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflicts.open",
            InvocationSource::Test,
        )
        .expect("command id"),
    );
    let page = app
        .search
        .picker_projection()
        .expect("the conflict surface opened");
    // The operation bar names the operation and its phase.
    assert!(
        page.title.contains("merge") && page.title.contains("1 conflicted path"),
        "the surface names the paused operation and its phase: {:?}",
        page.title
    );
    let row = page
        .rows
        .iter()
        .find(|row| row.label == "h0.txt")
        .unwrap_or_else(|| panic!("the conflicted path is listed: {:?}", page.rows));
    // Each side is present with a real OID, and the labels say which side is which.
    for label in ["base ", "ours ", "theirs "] {
        assert!(
            row.detail.contains(label),
            "the row names the {label:?} side: {:?}",
            row.detail
        );
    }
    let index = repo.index().unwrap();
    let ours = index.get_path(std::path::Path::new("h0.txt"), 2).unwrap();
    let theirs = index.get_path(std::path::Path::new("h0.txt"), 3).unwrap();
    let base = index.get_path(std::path::Path::new("h0.txt"), 1).unwrap();
    for side in [ours, theirs, base] {
        let short = &side.id.to_string()[..8];
        assert!(
            row.detail.contains(short),
            "the row shows the real OID {short} read from the index: {:?}",
            row.detail
        );
    }
}

#[test]
fn choosing_a_side_edits_the_result_buffer_and_leaves_save_and_stage_separate() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let base_tip = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base_tip).unwrap();
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo
            .treebuilder(Some(&base_commit.tree().unwrap()))
            .unwrap();
        let blob = repo.blob(b"\xef\xbb\xbfTHEIR VERSION\r\n").unwrap();
        builder.insert("h0.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    fs::write(root.join("h0.txt"), "OUR VERSION\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("h0.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();
    let their_commit = repo.find_commit(their_tip).unwrap();
    let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
    repo.merge(&[&annotated], None, None).unwrap();
    drop(annotated);
    drop(their_commit);
    drop(base_commit);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The RESULT buffer is the working-tree file git wrote with conflict markers.
    let conflicted = root.join("h0.txt");
    let on_disk = fs::read(&conflicted).unwrap();
    assert!(
        String::from_utf8_lossy(&on_disk).contains("<<<<<<<"),
        "the working tree really holds conflict markers: {:?}",
        String::from_utf8_lossy(&on_disk)
    );
    let mut bom_contents = b"\xef\xbb\xbf".to_vec();
    bom_contents.extend_from_slice(&on_disk);
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_bytes(&bom_contents).unwrap(),
        Some(conflicted.clone()),
    ).unwrap();
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;
    let before_choose = app.state.buffers[buffer].text.to_string();
    let index_before = crate::workspace::git::git_index_fingerprint(&root).unwrap();

    // ---- CHOOSING A SIDE.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.choose",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into())),
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    let content = crate::workspace::git::read_side_content(
        &root,
        &conflicted,
        crate::workspace::git::GitConflictChoice::Theirs,
    )
    .expect("the theirs side is readable");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        crate::editor::text::decode_utf8(content.as_bytes()).unwrap().text,
        "the result buffer holds exactly the chosen side's decoded content"
    );
    assert!(
        app.state.buffers[buffer].dirty,
        "the buffer is dirty after the choice"
    );
    assert_eq!(
        configured_file_bytes(&app.state.buffers[buffer].text, &app.configuration.settings().editor),
        b"\xef\xbb\xbfTHEIR VERSION\n",
        "saving keeps one metadata BOM, not the blob's BOM in the text"
    );
    // SAVING IS SEPARATE: the file on disk is byte-identical until an explicit save.
    assert_eq!(
        fs::read(&conflicted).unwrap(),
        on_disk,
        "the file on disk is untouched by choosing a side"
    );
    // STAGING IS SEPARATE: the index is unchanged until an explicit stage.
    assert_eq!(
        crate::workspace::git::git_index_fingerprint(&root).unwrap(),
        index_before,
        "the index is untouched by choosing a side"
    );

    // ---- UNDO goes through the editor's own path, which is what proves this was an ordinary
    // transaction rather than a side door.
    app.invoke(
        crate::editor::CommandInvocation::new("history.undo", InvocationSource::Test)
            .expect("command id"),
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        before_choose,
        "undo restored the conflict markers, so the change was an ordinary transaction"
    );
}

#[test]
fn choosing_a_side_for_an_unconflicted_path_is_refused() {
    // Its own app: opening a second buffer INVALIDATES the earlier view key, so this case must
    // not share a session with the conflict fixture.
    let mut app = app();
    // A real repository, so the git status the command consults exists.
    let _repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    fs::write(root.join("clean.txt"), "clean\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("clean\n"),
        Some(root.join("clean.txt")),
    ).unwrap();
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;

    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.choose",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("ours".into())),
    );
    assert!(
        app.ui.notification_error,
        "choosing a side for an unconflicted path is refused: {:?}",
        app.ui.notification
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("not a conflicted path")),
        "and the refusal names the reason rather than failing for another one: {:?}",
        app.ui.notification
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "clean\n",
        "the unconflicted buffer is untouched"
    );
    assert!(!app.state.buffers[buffer].dirty, "and is not marked dirty");
}

#[test]
fn a_previewed_replacement_writes_nothing_until_it_is_confirmed() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let (our_tip, _their_tip) = {
        let repo = history_fixture(&root, &["base"]);
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let base_tip = repo.head().unwrap().target().unwrap();
        let base_commit = repo.find_commit(base_tip).unwrap();
        repo.branch("theirs", &base_commit, false).unwrap();
        {
            let mut builder = repo
                .treebuilder(Some(&base_commit.tree().unwrap()))
                .unwrap();
            let blob = repo.blob(b"THEIR VERSION\n").unwrap();
            builder.insert("h0.txt", blob, 0o100644).unwrap();
            let tree = repo.find_tree(builder.write().unwrap()).unwrap();
            repo.commit(
                Some("refs/heads/theirs"),
                &sig,
                &sig,
                "theirs",
                &tree,
                &[&base_commit],
            )
            .unwrap();
        }
        fs::write(root.join("h0.txt"), "OUR VERSION\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("h0.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
                .unwrap();
        }
        let our_tip = repo.head().unwrap().target().unwrap();
        let their_tip = repo
            .find_reference("refs/heads/theirs")
            .unwrap()
            .target()
            .unwrap();
        let their_commit = repo.find_commit(their_tip).unwrap();
        let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
        repo.merge(&[&annotated], None, None).unwrap();
        repo.reference("ORIG_HEAD", our_tip, true, "fixture")
            .unwrap();
        (our_tip, their_tip)
    };

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let target = root.join("h0.txt");
    let before = fs::read(&target).unwrap();
    let index_before = crate::workspace::git::git_index_fingerprint(&root).unwrap();

    // PREVIEW: a confirmation naming the file and the side, and nothing written yet.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.project_input_prompt();
    let confirmation = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("the preview asks for confirmation");
    assert!(
        confirmation.message.contains("h0.txt") && confirmation.message.contains("theirs"),
        "the preview names the file and the side: {:?}",
        confirmation.message
    );
    assert_eq!(
        confirmation.selected,
        crate::ui::ConfirmationChoice::No,
        "the confirmation defaults to No"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "nothing is written while the question is open"
    );
    assert!(app.git.has_pending_replacement());

    // CANCEL: answering No writes nothing at all.
    app.submit_prompt();
    assert!(
        !app.git.has_pending_replacement(),
        "the question is answered once"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "a cancelled preview leaves the file byte-identical"
    );
    assert_eq!(
        crate::workspace::git::git_index_fingerprint(&root).unwrap(),
        index_before,
        "and the index untouched"
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("cancelled")),
        "and says so: {:?}",
        app.ui.notification
    );

    // Escape must retire the preview before a worktree-removal confirmation is opened.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.replace", InvocationSource::Test).unwrap()
            .with_argument("side", CommandValue::String("theirs".into()))
            .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.handle_configured_key("escape".parse().unwrap());
    assert!(!app.git.has_pending_replacement());
    app.install_picker("Worktrees".into(), vec![(
        crate::workspace::PickerItem {
            badge: String::new(), kind: crate::workspace::PickerItemKind::Entry,
            id: crate::workspace::PickerItemId("missing-worktree".into()),
            label: "missing-worktree".into(), detail: String::new(), source: "worktrees".into(),
            action: crate::workspace::PickerAction::None,
        },
        crate::editor::session::search::PickerPayload::GitWorktree {
            main: false,
            name: "missing-worktree".into(), path: root.join("missing-worktree"),
        },
    )]);
    app.request_git_worktree_remove();
    app.input.select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(fs::read(&target).unwrap(), before, "removal confirmation must not replace conflicted files");
    assert!(app.ui.notification_error, "the nonexistent worktree removal was attempted");

    // Replacing the confirmation with another prompt must also discard its plan.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.replace", InvocationSource::Test).unwrap()
            .with_argument("side", CommandValue::String("theirs".into()))
            .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    assert!(!app.git.has_pending_replacement());
    app.close_prompt();

    // CONFIRM: the file is replaced with the chosen side's content.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"THEIR VERSION\n",
        "the confirmed replacement wrote exactly the chosen side's content"
    );
    assert!(!app.git.has_pending_replacement());
    let _ = (our_tip, index_before);
}

/// W-26: the Changes surface renders as section 7.1 draws it — labelled sections, status and
/// provenance columns, an unsaved badge and its own footer — proved from the rendered grid.
#[test]
fn the_changes_surface_renders_its_sections_columns_badge_and_footer() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let write = |name: &str, text: &str| {
        fs::write(app.state.workspace_root.join(name), text).unwrap();
    };
    let stage = |name: &str| {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    };
    write("staged.txt", "one\n");
    write("unstaged.txt", "one\n");
    write("unsaved.txt", "one\n");
    write("conflicted.txt", "one\n");
    for name in [
        "staged.txt",
        "unstaged.txt",
        "unsaved.txt",
        "conflicted.txt",
    ] {
        stage(name);
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();

    // A real conflict: both sides change the same line.
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    write("conflicted.txt", "ours\n");
    stage("conflicted.txt");
    let ours = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let ours = repo
        .commit(Some("refs/heads/side"), &sig, &sig, "ours", &ours, &[&base])
        .unwrap();
    write("conflicted.txt", "theirs\n");
    stage("conflicted.txt");
    let theirs = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let theirs = repo
        .commit(
            Some("refs/heads/other"),
            &sig,
            &sig,
            "theirs",
            &theirs,
            &[&base],
        )
        .unwrap();
    let ours_annotated = repo.find_annotated_commit(ours).unwrap();
    let theirs_annotated = repo.find_annotated_commit(theirs).unwrap();
    // Merge into the side that carries `ours`, so the two sides really meet.
    repo.set_head("refs/heads/side").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    let _ = ours_annotated;
    repo.merge(&[&theirs_annotated], None, None)
        .expect("the merge runs");
    assert!(
        repo.index().unwrap().has_conflicts(),
        "both sides changed the same line, so the index is conflicted"
    );

    // One staged change, one unstaged change, and one path with only unsaved edits.
    write("staged.txt", "two\n");
    stage("staged.txt");
    write("unstaged.txt", "two\n");
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    // Render through the application's own path, which places the surface's window.
    app.handle_event(crate::ui::UiEvent::Resize(220, 50));
    app.reconcile_editor_windows();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    let rendered: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    let has = |needle: &str| rendered.iter().any(|row| row.contains(needle));

    // The three sections, in the order the document draws them, each naming its count.
    assert!(
        has("── Conflicts 1"),
        "the Conflicts section header is drawn: {rendered:?}"
    );
    assert!(has("── Unstaged 2"), "the Unstaged section header is drawn");
    assert!(has("── Staged 1"), "the Staged section header is drawn");
    let conflicts = rendered
        .iter()
        .position(|row| row.contains("── Conflicts"))
        .expect("conflicts header");
    let unstaged = rendered
        .iter()
        .position(|row| row.contains("── Unstaged"))
        .expect("unstaged header");
    let staged = rendered
        .iter()
        .position(|row| row.contains("── Staged"))
        .expect("staged header");
    assert!(
        conflicts < unstaged && unstaged < staged,
        "the sections keep the drawn order"
    );

    // The status column carries a two-character code followed by the full path; the section
    // header already names the comparison.
    assert!(
        has("UU conflicted.txt"),
        "a conflicted path shows its code: {rendered:?}"
    );
    assert!(has("M  staged.txt"), "a staged path shows its code");
    assert!(has(" M unstaged.txt"), "an unstaged path shows its code");
    assert!(has("unsaved.txt"), "an unsaved-only path is listed");
    // The badge is its own drawn column with its own style. Asserting merely that the word
    // "unsaved" appears would be satisfied by the path `unsaved.txt`, so this reads the
    // styled run that only the badge column produces.
    let badge_row = u16::try_from(
        rendered
            .iter()
            .enumerate()
            // Not the buffer tab or the "opened …/unsaved.txt" notice above the sections.
            .position(|(row, text)| {
                row > conflicts && text.contains("unsaved.txt") && !text.contains("opened")
            })
            .expect("the unsaved-only row"),
    )
    .expect("row index");
    let mut runs: Vec<String> = Vec::new();
    let mut run = String::new();
    for x in 0..frame.grid.width() {
        let cell = frame.grid.get(x, badge_row);
        if cell.is_some_and(|cell| cell.style == crate::ui::Style::Warning) {
            run.push_str(&cell.expect("a cell").symbol);
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    assert!(
        runs.iter().any(|run| run.trim() == "unsaved"),
        "the badge column carries the badge: {runs:?}"
    );
    // The footer describes the operation rather than the widget's mode.
    assert!(
        has("s Stage"),
        "the footer names what the key does here: {rendered:?}"
    );
    assert!(has("u Unstage"), "the footer names the inverse action");
    assert!(has("Tab Diff"), "the footer names the focus switch");
    assert!(
        !has("Insert · escape normal"),
        "the widget's mode hint is replaced by the surface's own footer"
    );

    // Read only the footer text, not its surrounding border, on a surface that truncates it.
    let legend = app.ui.picker.as_ref().unwrap().footer.clone().unwrap();
    app.handle_event(crate::ui::UiEvent::Resize(55, 24));
    app.reconcile_editor_windows();
    let narrow = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    let rect = app.ui.surface_content.picker.expect("picker geometry").outer;
    let footer_y = rect.y + rect.height - 1;
    let footer = (rect.x + 2..rect.x + rect.width - 1)
        .filter_map(|x| narrow.grid.get(x, footer_y))
        .filter(|cell| cell.style == crate::ui::Style::Muted)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    let entries = footer.trim().split(" · ").collect::<Vec<_>>();
    let full_entries = legend.split(" · ").collect::<Vec<_>>();
    assert!(!entries.is_empty() && entries.len() < full_entries.len(), "{footer:?}");
    assert_eq!(entries, full_entries[..entries.len()], "only complete legend entries: {footer:?}");
}

#[test]
fn changes_rows_display_index_and_worktree_status_independently() {
    let (mut app, repo, _) = near_changes_fixture();
    let path = app.state.workspace_root.join("added.txt");
    fs::write(&path, "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("added.txt")).unwrap();
    index.write().unwrap();
    fs::write(&path, "modified after staging\n").unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.open_git_status_picker();
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    let added = rows.iter().filter(|row| row.label == "added.txt").collect::<Vec<_>>();
    assert_eq!(added.len(), 2);
    assert!(added.iter().all(|row| row.source == "AM"));
}

fn wait_changes_preview(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !app.search.preview().as_ref().is_some_and(|preview| matches!(preview.body, crate::ui::PickerPreviewBody::Diff(_))) {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline, "{:?}", app.search.preview());
        thread::yield_now();
    }
}

/// W-26: the diff pane follows the focused row and shows that path's real diff, and `Tab`
/// switches it between the two comparisons.
#[test]
fn the_changes_diff_pane_follows_focus_and_tab_switches_the_comparison() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    fs::write(app.state.workspace_root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    // Stage one change and then change the file again, so the two comparisons differ.
    fs::write(app.state.workspace_root.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        index.write().unwrap();
    }
    fs::write(app.state.workspace_root.join("f.txt"), "one\nTWO!\nthree\n").unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let frame = |app: &mut InteractiveApplication| {
        app.handle_event(crate::ui::UiEvent::Resize(200, 50));
        app.reconcile_editor_windows();
        let frame = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        (0..frame.grid.height())
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<String>>()
            .join("\n")
    };

    wait_changes_preview(&mut app);
    // The pane shows the focused row's UNSTAGED diff: index to disk. These strings are
    // produced only by the pane, so the row's own provenance column cannot satisfy them.
    let unstaged = frame(&mut app);
    assert!(
        unstaged.contains("@@ -1,3 +1,3 @@"),
        "the pane draws the hunk header: {unstaged}"
    );
    assert!(
        unstaged.contains("- TWO"),
        "the pane shows the index line as removed: {unstaged}"
    );
    assert!(
        unstaged.contains("+ TWO!"),
        "the pane shows the worktree line as added: {unstaged}"
    );

    // Tab switches the pane to the staged comparison, with different content.
    let switch = crate::editor::CommandInvocation::new(
        "workspace.git.diff.layer.switch",
        InvocationSource::Test,
    )
    .expect("static command");
    app.invoke(switch);
    wait_changes_preview(&mut app);
    let staged = frame(&mut app);
    assert!(
        staged.contains("HEAD -> index"),
        "the pane names the staged comparison after the switch: {staged}"
    );
    assert!(
        staged.contains("- two") && staged.contains("+ TWO"),
        "the pane shows the staged change: {staged}"
    );
    assert!(
        !staged.contains("TWO!"),
        "the pane no longer shows the worktree-only line: {staged}"
    );
    assert_ne!(unstaged, staged, "the two comparisons render differently");

    // With Tab undone, moving onto the staged row shows what was staged, without a Tab.
    for id in ["workspace.git.diff.layer.switch", "picker.next"] {
        let command = crate::editor::CommandInvocation::new(id, InvocationSource::Test)
            .expect("static command");
        app.invoke(command);
    }
    wait_changes_preview(&mut app);
    let staged_row = frame(&mut app);
    assert!(
        staged_row.contains("HEAD -> index"),
        "a staged row previews its staged change: {staged_row}"
    );
}

/// A long file's only change sits far below the top of a small diff pane: the pane opens on it,
/// and the page keys move through the rest.
#[test]
fn a_small_changes_pane_opens_on_the_change_and_ctrl_d_scrolls_it() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let lines = |changed: &str| {
        (1..=90)
            .map(|n| match n {
                45 => changed.to_owned(),
                60 => format!("later {changed}"),
                n => format!("line {n}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    fs::write(app.state.workspace_root.join("notes.txt"), lines("before")).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("notes.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    fs::write(app.state.workspace_root.join("notes.txt"), lines("after")).unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let frame = |app: &mut InteractiveApplication| {
        app.handle_event(crate::ui::UiEvent::Resize(80, 24));
        app.reconcile_editor_windows();
        let frame = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        (0..frame.grid.height())
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<String>>()
            .join("\n")
    };

    wait_changes_preview(&mut app);
    let opened = frame(&mut app);
    assert!(
        opened.contains("index -> disk"),
        "the comparison stays on top: {opened}"
    );
    assert!(
        opened.contains("- before"),
        "the pane opens on the change: {opened}"
    );
    assert!(
        opened.contains("+ after"),
        "the pane opens on the change: {opened}"
    );

    assert_eq!(frame(&mut app), opened, "a redraw keeps the pane's place");

    let mut scrolled = opened.clone();
    for _ in 0..8 {
        app.handle_configured_key("ctrl-d".parse::<KeyEvent>().expect("ctrl-d"));
        scrolled = frame(&mut app);
        if scrolled.contains("+ later after") {
            break;
        }
    }
    assert!(
        scrolled.contains("+ later after"),
        "ctrl-d reaches the next change: {scrolled}"
    );
    assert!(
        scrolled.contains("index -> disk"),
        "the comparison stays on top: {scrolled}"
    );

    for _ in 0..12 {
        app.handle_configured_key("pageup".parse::<KeyEvent>().expect("pageup"));
    }
    let top = frame(&mut app);
    assert!(
        top.contains("unchanged lines"),
        "pageup reaches the top of the diff: {top}"
    );
}

/// A commit made outside Dust shows after the explorer's refresh key.
#[test]
fn refreshing_the_explorer_also_refreshes_git_status() {
    let mut app = app();
    git2::Repository::init(&app.state.workspace_root).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.process_effect(AppEffect::ExplorerRefresh);
    assert!(app.git.status_pending(), "the refresh asks Git again");
}

/// `:e` on a path with nothing there starts that file, and the first save creates it.
#[test]
fn editing_a_missing_path_starts_a_new_file_that_saving_creates() {
    let mut app = app();
    let path = app.state.workspace_root.join("new-file.txt");
    app.edit_path(PathBuf::from("new-file.txt"));
    let buffer = app
        .state
        .active_buffer_id()
        .expect("the new file is on screen");
    assert_eq!(
        app.state.buffers[buffer]
            .path
            .as_deref()
            .and_then(std::path::Path::file_name),
        path.file_name()
    );
    assert!(!path.exists(), "nothing is written before the save");
    app.handle_configured_key(KeyEvent::character('i'));
    app.handle_configured_key(KeyEvent::character('o'));
    app.handle_configured_key("escape".parse::<KeyEvent>().expect("escape"));
    let save = crate::editor::CommandInvocation::new("file.save", InvocationSource::Test)
        .expect("static command");
    app.invoke(save);
    super::wait_vfs(&mut app);
    let saved = fs::read_to_string(&path).expect("the save created the file");
    assert!(saved.contains('o'), "{saved:?}");
    assert!(!app.state.buffers[buffer].new_file);
}

#[test]
fn saving_an_existing_empty_file_detects_external_deletion() {
    let mut app = app();
    let path = app.state.workspace_root.join("existing-empty.txt");
    fs::write(&path, b"").unwrap();
    app.edit_path(path.clone());
    super::wait_vfs(&mut app);
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
    super::wait_vfs(&mut app);
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

/// W-26: a refresh does not move the selection or reorder the list under the cursor.
#[test]
fn a_change_refresh_keeps_the_selection_on_the_same_row() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(app.state.workspace_root.join(name), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(app.state.workspace_root.join(name), "two\n").unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let refresh = |app: &mut InteractiveApplication| {
        app.start_git_status_refresh();
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    refresh(&mut app);
    app.open_git_status_picker();
    let selected_id = |app: &InteractiveApplication| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker.selected.map(|index| picker.rows[index].id.clone())
    };
    // Move off the first entry so a reset to position 0 would be visible.
    app.picker_move(1);
    let before = selected_id(&app).expect("a selected row");
    assert!(!before.is_empty(), "the selected row has an identity");

    refresh(&mut app);
    app.open_git_status_picker();
    let after = selected_id(&app).expect("a selected row after the refresh");
    assert_eq!(
        before, after,
        "the refresh left the cursor on the same row rather than moving it"
    );
}

/// W-26: the footer labels `s` and `u` by focus, so the change surface stages the ROW UNDER
/// THE CURSOR rather than whatever buffer happens to be active.
#[test]
fn staging_from_the_change_surface_acts_on_the_focused_row_not_the_active_buffer() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(app.state.workspace_root.join(name), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(app.state.workspace_root.join(name), "two\n").unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    // Open a.txt so the ACTIVE BUFFER is a.txt, while the cursor will be on b.txt.
    app.start_open(app.state.workspace_root.join("a.txt"));
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    // Put the cursor on b.txt by MOVING it, so the picker's own selection changes rather
    // than only the projection.
    let selected_label = |app: &InteractiveApplication| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker
            .selected
            .map(|index| picker.rows[index].label.clone())
    };
    for _ in 0..8 {
        if selected_label(&app).as_deref() == Some("b.txt") {
            break;
        }
        app.picker_move(1);
    }
    assert_eq!(
        selected_label(&app).as_deref(),
        Some("b.txt"),
        "the cursor is on the focused row"
    );

    let stage =
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command");
    app.invoke(stage);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The staged diff names b.txt and not a.txt.
    let staged = crate::workspace::git::read_hunks(&crate::workspace::git::GitHunkPolicy {
        scope_root: app.state.workspace_root.clone(),
        repository_id: app.git.status().expect("status").repository_id,
        status_revision: app.git.status().expect("status").revision,
        root_revision: app.workspace.root_revision(),
        file: std::path::PathBuf::from("b.txt"),
        layer: crate::workspace::git::GitDiffLayer::Staged,
        maximum_hunks: 100,
        maximum_lines: 1_000,
        maximum_bytes: 1024 * 1024,
        deadline: Duration::from_secs(5),
    })
    .expect("b.txt hunks");
    assert!(!staged.hunks.is_empty(), "the FOCUSED row b.txt was staged");
    let a_staged = crate::workspace::git::read_hunks(&crate::workspace::git::GitHunkPolicy {
        scope_root: app.state.workspace_root.clone(),
        repository_id: app.git.status().expect("status").repository_id,
        status_revision: app.git.status().expect("status").revision,
        root_revision: app.workspace.root_revision(),
        file: std::path::PathBuf::from("a.txt"),
        layer: crate::workspace::git::GitDiffLayer::Staged,
        maximum_hunks: 100,
        maximum_lines: 1_000,
        maximum_bytes: 1024 * 1024,
        deadline: Duration::from_secs(5),
    })
    .expect("a.txt hunks");
    assert!(
        a_staged.hunks.is_empty(),
        "the ACTIVE BUFFER a.txt was left unstaged"
    );
}

/// W-26: staging a path whose buffer has unsaved edits is refused with the explicit
/// save-and-stage choice, and the index is not written — for the FOCUSED row, which is not
/// the active buffer.
#[test]
fn staging_a_row_moves_it_to_the_staged_section_of_the_open_list() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let path = app.state.workspace_root.join("changed.txt");
    fs::write(&path, "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("changed.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "changed\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let labels = |app: &InteractiveApplication| -> Vec<String> {
        app.ui
            .picker
            .as_ref()
            .expect("picker")
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    };
    assert!(
        labels(&app).iter().any(|label| label == "Unstaged 1"),
        "{:?}",
        labels(&app)
    );
    app.input.set_picker_normal(true);
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while !labels(&app).iter().any(|label| label == "Staged 1") {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline, "{:?}", labels(&app));
        thread::yield_now()
    }
    assert!(
        !labels(&app).iter().any(|label| label == "Unstaged 1"),
        "{:?}",
        labels(&app)
    );
    assert!(
        app.ui.picker.as_ref().expect("picker").selected.is_some(),
        "a row stays selected"
    );
    assert!(
        app.input.picker_normal(),
        "the next s or u still acts instead of searching"
    );
}

#[test]
fn switching_up_from_a_nested_worktree_is_blocked_by_its_unsaved_file() {
    let main = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let main = main.path().canonicalize().unwrap();
    let nested = main.join(".worktree/feature");
    fs::create_dir_all(&nested).unwrap();
    let mut app = InteractiveApplication::new(
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
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("unsaved\n"),
        Some(nested.join("notes.txt")),
    ).unwrap();
    app.state.set_buffer_dirty(buffer, true);

    app.switch_workspace_root(main.clone());
    assert!(app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|text| text.contains("unsaved changes")),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn staging_an_unsaved_focused_row_offers_the_choice_and_writes_nothing() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["unsaved.txt", "other.txt"] {
        fs::write(app.state.workspace_root.join(name), "disk\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    // Make unsaved.txt's buffer dirty, then make OTHER.txt the active buffer, so the row the
    // cursor sits on is not the buffer the editor is showing.
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    app.start_open(app.state.workspace_root.join("other.txt"));
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let selected_label = |app: &InteractiveApplication| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker
            .selected
            .map(|index| picker.rows[index].label.clone())
    };
    for _ in 0..8 {
        if selected_label(&app).as_deref() == Some("unsaved.txt") {
            break;
        }
        app.picker_move(1);
    }
    assert_eq!(selected_label(&app).as_deref(), Some("unsaved.txt"));

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        app.process_git_mutation_events();
        if app
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::GitStageUnsaved)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the save-and-stage choice did not appear for the focused row"
        );
        thread::yield_now()
    }
    // The refusal wrote nothing: the index still holds the committed bytes.
    let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
    let index = repo.index().unwrap();
    let entry = index
        .get_path(std::path::Path::new("unsaved.txt"), 0)
        .expect("index entry");
    assert_eq!(
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap(),
        "disk\n",
        "the index was not written by the refused stage"
    );
    assert_eq!(
        fs::read_to_string(app.state.workspace_root.join("unsaved.txt")).unwrap(),
        "disk\n",
        "nothing was saved on the user's behalf either"
    );
}

/// W-26: `Ctrl-q` sends the changed-path list to the shared Results surface.
#[test]
fn ctrl_q_sends_the_change_rows_to_the_results_surface() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    fs::write(app.state.workspace_root.join("a.txt"), "one\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(app.state.workspace_root.join("a.txt"), "two\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Changes,
        "the surface starts as the change list"
    );
    let rows_before = app.ui.picker.as_ref().expect("picker").rows.len();
    app.invoke(
        crate::editor::CommandInvocation::new("picker.results", InvocationSource::Test)
            .expect("static command"),
    );
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Problems,
        "the rows now live on the shared Results surface"
    );
    assert_eq!(
        app.ui.picker.as_ref().expect("picker").rows.len(),
        rows_before,
        "sending to Results keeps the rows rather than dropping them"
    );
}

#[test]
fn opening_changes_before_status_arrives_opens_them_when_it_does() {
    let mut app = app();
    git2::Repository::init(&app.state.workspace_root).unwrap();
    fs::write(app.state.workspace_root.join("a.txt"), "one\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.open_git_status_picker();
    assert!(
        app.ui.picker.is_none(),
        "nothing opens before status exists"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Changes,
        "the change list opens once status arrives"
    );
}

fn palette_row_for(app: &mut InteractiveApplication, query: &str) -> String {
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert(query);
    let palette = app.search.picker_projection().expect("palette");
    let selected = palette
        .selected
        .unwrap_or_else(|| panic!("no row for `{query}`"));
    palette.rows[selected].label.clone()
}

#[test]
fn palette_finds_commands_by_the_words_people_type() {
    let mut app = app();
    for (query, expected) in [
        ("theme", "Select Color Theme"),
        ("color", "Select Color Theme"),
        ("colorscheme", "Select Color Theme"),
        ("mason", "Tool Manager"),
        ("quit dust", "Quit Dust"),
        ("close buffer", "Close buffer"),
        ("rename symbol", "Rename symbol"),
        ("open settings", "Open Settings"),
        ("undo", "Undo"),
    ] {
        let row = palette_row_for(&mut app, query);
        assert!(row.contains(expected), "`{query}` picked `{row}`");
    }
}

#[test]
fn palette_runs_flagged_commands_with_the_flag_off() {
    let mut app = app();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("unsaved\n"), None).unwrap();
    app.state.buffers[buffer].dirty = true;
    palette_row_for(&mut app, "quit dust");
    app.accept_picker();
    assert!(!app.should_quit, "ordinary quit refuses unsaved changes");
    assert!(app.ui.notification_error);
}

#[test]
fn palette_rows_that_need_text_open_a_command_line_that_runs_them() {
    let mut app = app();
    palette_row_for(&mut app, "file.open");
    app.accept_picker();
    assert_eq!(app.input.prompt().expect("prompt").input, "e ");
    let path = app.state.workspace_root.join("palette-open.txt");
    fs::write(&path, "opened from the palette\n").unwrap();
    app.prompt_insert("palette-open.txt");
    app.submit_prompt();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened buffer");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "opened from the palette\n");

    let settings = app.configuration.settings();
    for (id, initial) in &settings.command_palette.prompts {
        let invocation = crate::editor::input::parse_ex(
            &format!("{initial}value"),
            &settings.input.command_aliases,
        )
        .unwrap_or_else(|error| panic!("`{id}` prompt `{initial}`: {error}"));
        assert_eq!(invocation.id.as_str(), id, "prompt `{initial}`");
        app.commands
            .validate_invocation_shape(&invocation)
            .unwrap_or_else(|error| panic!("`{id}` prompt `{initial}`: {error}"));
    }
}

#[test]
fn command_line_completes_commands_without_an_alias_and_fills_in_their_arguments() {
    let mut app = app();
    app.open_prompt(crate::ui::PromptKind::Command, "wor".into());
    app.refresh_completions();
    let completions = |app: &InteractiveApplication| -> Vec<String> {
        app.input
            .prompt()
            .unwrap()
            .completions
            .iter()
            .map(|item| item.label.clone())
            .collect()
    };
    let rows = completions(&app);
    assert_eq!(rows[0], "worktree", "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row == "workspace.git.worktree.remove"),
        "{rows:?}"
    );
    app.input.prompt_complete(1);
    assert!(app.input.accept_completion(app.configuration.settings()));
    let prompt = app.input.prompt().expect("completed prompt");
    assert_eq!(prompt.input, "worktree ");
    assert_eq!(prompt.cursor, "worktree ".len());

    app.close_prompt();
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.refresh_completions();
    let rows = completions(&app);
    assert_eq!(rows[0].len(), 1, "short aliases come first: {rows:?}");
    assert!(
        rows.iter().any(|row| row == "workspace.git.hunk.stage"),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row.starts_with("input.")),
        "{rows:?}"
    );
}

#[test]
fn completing_a_path_after_w_keeps_the_save_command() {
    let mut app = app();
    fs::write(app.state.workspace_root.join("q!"), b"").expect("file");
    app.open_prompt(crate::ui::PromptKind::Command, "w q".into());
    wait_vfs(&mut app);
    app.input.prompt_complete(1);
    assert!(app.input.accept_completion(app.configuration.settings()));
    assert_eq!(app.input.prompt().expect("prompt").input, "w q!");
}

#[test]
fn an_empty_plugin_list_says_how_to_get_one() {
    let mut app = app();
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
    let mut app = InteractiveApplication::new(
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
        crate::editor::session::terminal::TerminalFixture {
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
    let mut app = app();
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::editor::session::terminal::TerminalFixture {
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
fn cached_hunk_reset_never_targets_a_previous_buffer() {
    let (mut app, _repo, path) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    let before = fs::read(&path).unwrap();
    let other = app.state.workspace_root.join("other.txt");
    fs::write(&other, "other\n").unwrap();
    app.state
        .open_text(TextBuffer::from_text("other\n"), Some(other)).unwrap();
    app.request_reset_selected_git_hunk();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    settle_git_work(&mut app);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn removing_a_root_preserves_terminals_and_updates_relative_paths() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    let first = app.workspace.active_root().unwrap();
    let path = app.state.workspace_root.clone();
    let extra = tempfile::tempdir().unwrap();
    let second = app
        .workspace
        .fixture_add_root(
            "second".into(),
            crate::workspace::RootIdentity::capture(extra.path()).unwrap(),
        )
        .unwrap();
    app.activate_workspace_root(second);
    app.terminal_session.fixture(
        PtyId(101),
        crate::editor::session::terminal::TerminalFixture {
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
    assert_eq!(app.workspace_relative_label(&second_file), second_file.display().to_string());
    assert_eq!(app.workspace.active_root(), Some(first));
    assert_eq!(app.state.workspace_root, path);
    assert!(app.terminal_session.contains(PtyId(101)));
    app.process_pty_events();
    assert!(app.terminal_session.contains(PtyId(101)));
}

#[test]
fn conflict_navigation_works_in_unnamed_buffers() {
    let mut app = app();
    let text = "intro\n<<<<<<< ours\nleft\n=======\nright\n>>>>>>> theirs\n";
    let (_, view) = app.state.open_text(TextBuffer::from_text(text), None).unwrap();
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));
    app.state.views[view].cursor = ByteOffset(text.len());
    app.process_effect(AppEffect::GitConflictNavigate(-1));
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));
}

#[test]
fn matcher_navigation_uses_the_open_buffer_after_its_file_is_deleted() {
    let mut app = app();
    let path = app.state.workspace_root.join("deleted.txt");
    fs::write(&path, "first\nsecond\n").unwrap();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("first\nsecond\n"), Some(path.clone())).unwrap();
    fs::remove_file(&path).unwrap();
    app.state.open_text(TextBuffer::from_text("other"), None).unwrap();
    app.navigate_matcher_source(crate::editor::session::search::MatcherSource {
        path, line: 1, column: 2,
        encoding: crate::editor::text::PositionEncoding::Utf8,
    });
    assert_eq!(app.state.active_buffer_id(), Some(buffer));
    assert_eq!(app.state.views[app.state.active_view.unwrap()].cursor, ByteOffset(8));
}

#[test]
fn deferred_navigation_does_not_replace_a_newer_buffer_choice() {
    let mut app = app();
    let path = app.state.workspace_root.join("late.txt");
    fs::write(&path, "destination").unwrap();
    app.navigate_matcher_source(crate::editor::session::search::MatcherSource {
        path,
        line: 0,
        column: 0,
        encoding: crate::editor::text::PositionEncoding::Utf8,
    });
    let (newer, _) = app
        .state
        .open_text(TextBuffer::from_text("newer choice"), None).unwrap();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(newer));
}

#[test]
fn location_labels_read_only_the_required_prefix_of_a_large_file() {
    let app = app();
    let path = app.state.workspace_root.join("sparse.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len(4 * 1024 * 1024 * 1024).unwrap();
    let label = app.persisted_location_label(&crate::persistence::PersistedLocation {
        path,
        byte: 1,
        anchor: 1,
        scroll_line: 0,
        scroll_column: 0,
    });
    assert!(label.ends_with(":1"));
}

#[test]
fn help_restores_the_visible_palette_and_keeps_file_search_alive() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
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
    assert!(picker.rows.iter().any(|row| row.label == "surviving-search.txt"));
    app.reconcile_editor_windows();
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened search result");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "found after help\n");
}

#[test]
fn failed_and_cancelled_staging_never_replays_on_an_ordinary_save() {
    let (mut app, _repo, path) = near_changes_fixture();
    let index_before = index_bytes(&app.state.workspace_root, "near.txt");
    app.process_effect(AppEffect::GitLinesStage {
        start: 999,
        end: 999,
    });
    settle_git_work(&mut app);
    assert!(app.input.take_pending_stage().is_none());
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "ordinary save after failure\n".into())
        .unwrap();
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    assert!(!app.git.mutation_pending());
    assert_eq!(fs::read_to_string(&path).unwrap(), "ordinary save after failure\n");
    assert_eq!(index_bytes(&app.state.workspace_root, "near.txt"), index_before);
    app.replace_with_recovered(buffer, "changed unsaved\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input.prompt().unwrap_or_else(|| panic!("missing stage prompt: {:?}", app.ui.notification)).kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.close_prompt();
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert!(!app.git.mutation_pending());
    assert_eq!(fs::read_to_string(path).unwrap(), "changed unsaved\n");
    assert_eq!(index_bytes(&app.state.workspace_root, "near.txt"), index_before);
    assert!(app.input.take_pending_stage().is_none());
}

#[test]
fn save_and_stage_writes_the_refused_file_and_refreshes_its_hunks() {
    let (mut app, repo, path) = near_changes_fixture();
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "one\nNEW\nthree\nFOUR\nfive\n".into())
        .unwrap();
    let other = app.state.workspace_root.join("other.txt");
    fs::write(&other, "other\n").unwrap();
    let (other_buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("other\n"), Some(other.clone())).unwrap();
    app.replace_with_recovered(other_buffer, "other unsaved\n".into())
        .unwrap();
    app.open_git_status_picker();
    for _ in 0..8 {
        if app
            .ui
            .picker
            .as_ref()
            .and_then(|picker| picker.selected.map(|row| picker.rows[row].label.as_str()))
            == Some("near.txt")
        {
            break;
        }
        app.picker_move(1);
    }
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_none(),
        "confirmation was not visible: {:?}",
        app.input.prompt()
    );
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    assert_eq!(fs::read_to_string(&other).unwrap(), "other\n");
    assert_eq!(app.state.active_buffer_id(), Some(other_buffer));
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    let expected = b"one\nNEW\nthree\nFOUR\nfive\n";
    assert_eq!(fs::read(path).unwrap(), expected);
    assert_eq!(blob.content(), expected, "{:?}", app.ui.notification);
}

#[test]
fn failed_conflict_staging_cannot_arm_the_next_save() {
    let mut app = app();
    app.stage_git_conflict();
    assert!(app.input.take_pending_stage().is_none());
}

#[test]
fn disk_polling_retries_reloads_refused_by_document_capacity() {
    let mut app = app();
    let mut buffers = Vec::new();
    for index in 0..9 {
        let path = app.state.workspace_root.join(format!("poll-{index}"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
        buffers.push((buffer, path));
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    for (_, path) in &buffers {
        fs::write(path, "new contents").unwrap();
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert!(
        buffers
            .iter()
            .all(|(buffer, _)| app.state.buffers[*buffer].text.to_string() == "new contents")
    );
}

#[test]
fn save_and_stage_refreshes_the_pre_save_hunk_snapshot() {
    let (mut app, repo, _) = near_changes_fixture();
    app.open_explorer();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "one\nNEW\nthree\nFOUR\nfive\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitHunkStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_none(),
        "confirmation was not visible: {:?}",
        app.input.prompt()
    );
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert!(
        String::from_utf8_lossy(blob.content()).contains("NEW"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn file_picker_can_filter_an_entry_after_the_display_limit() {
    use crate::editor::session::search::{PickerOrigin, PickerPayload};
    let mut app = app();
    app.start_workspace_file_picker();
    let items = (0..10_001)
        .map(|i| {
            (
                crate::workspace::PickerItem {
                    id: crate::workspace::PickerItemId(format!("file-{i}")),
                    label: format!("file-{i}"),
                    detail: String::new(),
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    source: String::new(),
                    action: crate::workspace::PickerAction::None,
                },
                PickerPayload::Command(String::new()),
            )
        })
        .collect();
    app.install_surface_picker_state(
        PickerSurfaceKind::Generic,
        "Files".into(),
        items,
        String::new(),
        true,
        PickerOrigin::Files,
    );
    app.picker_insert("file-10000");
    assert_eq!(app.ui.picker.as_ref().unwrap().rows[0].label, "file-10000");
}

#[test]
fn truncated_workspace_search_does_not_offer_partial_replacement() {
    let mut app = app();
    let path = app.state.workspace_root.join("many.txt");
    fs::write(&path, "needle\n").unwrap();
    app.process_effect(AppEffect::WorkspaceReplace {
        query: "needle".into(),
        replacement: "changed".into(),
    });
    app.search.fixture_search_completion(
        vec![crate::files::WorkspaceSearchMatch {
            id: "match".into(),
            root: app.state.workspace_root.clone(),
            canonical_path: path,
            relative_path: "many.txt".into(),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 6,
            range: 0..6,
            line_text: "needle".into(),
            snapshot: std::sync::Arc::from(b"needle\n".as_slice()),
        }],
        true,
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.search.pending() {
        app.process_workspace_search_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.workspace_edits.proposal().is_none());
    assert!(app.ui.notification_error);
    assert!(
        app.ui.notification.as_ref().unwrap().contains("truncated"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn conflict_picker_resolves_paths_from_the_repository_root() {
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    let path = root.path().join("sub/file.rs");
    let signature = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let commit = |text: &str| {
        fs::write(&path, text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("sub/file.rs")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let base = commit("base\n");
    repo.branch("side", &repo.find_commit(base).unwrap(), false)
        .unwrap();
    commit("ours\n");
    let main = repo.head().unwrap().name().unwrap().to_owned();
    repo.set_head("refs/heads/side").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    let theirs = commit("theirs\n");
    repo.set_head(&main).unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    repo.merge(&[&repo.find_annotated_commit(theirs).unwrap()], None, None)
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().join("sub"),
        },
        crate::config::load_bundled_config(),
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.open_git_conflicts();
    let Some(crate::editor::session::search::PickerPayload::MatcherSource(source)) =
        app.selected_picker_payload()
    else {
        panic!("conflict row missing")
    };
    assert_eq!(source.path, path);
}

#[test]
fn failed_save_and_stage_does_not_leak_into_a_later_save() {
    let (mut app, repo, path) = near_changes_fixture();
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "unsaved\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    fs::write(&path, "external change\n").unwrap();
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.input.prompt().is_none());
    wait_vfs(&mut app);
    assert!(app.ui.notification_error);
    app.start_save(false, None, true);
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert_eq!(blob.content(), b"one\ntwo\nthree\nfour\nfive\n");
}
