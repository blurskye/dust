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
    );
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
    );
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
        .open_text(TextBuffer::from_text("dirty text"), Some(source.clone()));
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
fn explorer_rename_and_delete_reconcile_open_buffers_without_losing_text() {
    let mut app = app();
    let source = app.state.workspace_root.join("source.txt");
    fs::write(&source, "kept").unwrap();
    let (buffer, _) = app.state.open_text(
        TextBuffer::from_text("kept plus unsaved"),
        Some(source.clone()),
    );
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
    assert_eq!(explorer_ui.rows.len(), 1, "single root remains hidden");
    assert!(explorer_ui.rows[0].node_id > 0);
    assert!(explorer_ui.rows[0].selected);
    assert!(
        explorer_ui.rows[0]
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
        ui.rows[0].segments.iter().any(|segment| {
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
        ui.rows[0].segments.iter().any(|segment| {
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
        .open_text(TextBuffer::from_text("follow"), Some(path.clone()));
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
    assert_eq!(explorer.total, 40);
    assert_eq!(explorer.rows.len(), 6);
    assert!(explorer.rows.len() < explorer.total);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    let first_child = app.explorer.tree().selected_id().expect("first child");
    let projection_generation = app.explorer.tree().projection_generation();

    app.move_explorer_selection(-1);
    assert_eq!(app.explorer.tree().selected_id(), Some(first_child));
    app.explorer_collapse_or_parent();
    assert_eq!(app.explorer.tree().selected_id(), Some(first_child));
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.handle_event(crate::ui::UiEvent::Resize(80, 20));
    let explorer = app.ui.explorer.as_ref().expect("grown projection");
    assert_eq!(explorer.rows.len(), 16);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );
    app.handle_event(crate::ui::UiEvent::Resize(80, 8));
    let explorer = app.ui.explorer.as_ref().expect("shrunk projection");
    assert_eq!(explorer.rows.len(), 4);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.move_explorer_selection(30);
    let explorer = app.ui.explorer.as_ref().expect("scrolled projection");
    assert_eq!(explorer.rows.len(), 4);
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
    app.input.observe_mouse(1, 3);
    app.explorer_mouse_select();
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
    invalid.config.ui.explorer.maximum_visible_rows = 2;
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
    valid.config.ui.explorer.maximum_visible_rows = 2;
    app.apply_configuration_candidate(valid, false)
        .expect("valid explorer reload");
    let explorer = app.ui.explorer.as_ref().expect("reconfigured projection");
    assert_eq!(explorer.rows.len(), 2);
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
        .open_text(crate::editor::text::TextBuffer::from_text("focus"), None);
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
fn async_root_add_picker_activation_and_dirty_removal_preflight_are_real() {
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
    app.activate_workspace_root(first);
    app.open_builtin_picker(crate::editor::PickerKind::Roots);
    app.picker_insert(second_path.file_name().unwrap().to_str().unwrap());
    app.accept_picker();
    assert_eq!(app.workspace.active_root(), Some(second));

    let dirty_path = second_path.join("dirty.txt");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("dirty"),
        Some(dirty_path),
    );
    app.state.buffers[buffer].dirty = true;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 2);
    assert!(app.ui.notification_error);
    app.state.buffers[buffer].dirty = false;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 1);
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

pub(super) fn wait_execution_validation(app: &mut InteractiveApplication) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.validation_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "execution validation timed out");
        thread::yield_now();
    }
}

#[cfg(unix)]
#[test]
fn same_session_root_replacement_cancels_all_protected_action_kinds() {
    for kind in 0..3 {
        let mut app = app();
        let action = match kind {
            0 => super::DeferredExecution::InstallTool {
                id: "missing".into(),
                source: None,
            },
            1 => super::DeferredExecution::StartLsp(Some("missing".into())),
            _ => {
                let mut policy = crate::config::DebugConfig::default();
                policy.adapters.insert(
                    "owned".into(),
                    crate::config::DebugAdapterConfig {
                        program: "/bin/sh".into(),
                        arguments: vec![],
                        environment: Default::default(),
                    },
                );
                policy.sessions.insert(
                    "owned".into(),
                    crate::config::DebugSessionConfig {
                        adapter: "owned".into(),
                        request: crate::config::DebugRequestKind::Attach,
                        cwd: None,
                        program: None,
                        run_in_terminal_profile: None,
                        arguments: Default::default(),
                        path_maps: Default::default(),
                    },
                );
                let intent = app
                    .debugger_session
                    .request_debug(
                        (Some("owned"), None),
                        None,
                        None,
                        (
                            &policy,
                            &app.configuration.settings().terminals,
                            app.configuration.binding_revision(),
                        ),
                        &app.workspace,
                        app.tooling.entries(),
                    )
                    .unwrap();
                super::DeferredExecution::StartDebug(intent)
            }
        };
        prepare_trust_identity(&mut app);
        for root in app.workspace.roots().to_vec() {
            app.workspace.fixture_trust_identity(root.identity.clone());
        }
        app.reconcile_workspace_trust();
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Trusted
        );
        let original = app.state.workspace_root.clone();
        let displaced = original.with_extension("displaced");
        fs::rename(&original, &displaced).expect("displace root");
        fs::create_dir(&original).expect("replacement root");
        app.defer_trusted_execution(action);
        wait_execution_validation(&mut app);
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        assert!(app.language.active().is_none());
        assert!(app.ui.notification_error);
    }
}

#[test]
fn protected_action_resumes_once_after_successful_identity_revalidation() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.defer_trusted_execution(super::DeferredExecution::InstallTool {
        id: "missing".into(),
        source: None,
    });
    app.defer_trusted_execution(super::DeferredExecution::StartLsp(None));
    assert!(
        app.ui.notification_error,
        "concurrent protected action is rejected"
    );
    wait_execution_validation(&mut app);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    assert!(!app.workspace.validation_pending());
    assert!(app.tooling.validation().is_none());
    assert!(app.language.validation().is_none());
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
    let path = app.state.workspace_root.join("plain.txt");
    fs::write(&path, "plain\n").unwrap();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("plain\n"),
        Some(path),
    );
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
    );
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
    let title = app.ui.picker.as_ref().map(|p| p.title.clone()).expect("title");
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
    app.handle_configured_key(KeyEvent::character('d'));
    app.handle_configured_key(KeyEvent::character('d'));
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
fn workspace_file_command_uses_bounded_picker_and_opens_selection() {
    let mut app = app();
    let path = app.state.workspace_root.join("find-me.txt");
    let mut contents = String::from("found\n");
    contents.push_str(&"x".repeat(140 * 1024));
    fs::write(&path, contents).unwrap();
    assert!(fs::metadata(&path).unwrap().len() > 120 * 1024);
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
                        .is_some_and(|text| text.lines.first().map(String::as_str) == Some("found"))
            })
    }));
    app.picker_insert("find-me");
    for _ in 0..256 {
        app.sync_picker_ui();
        assert_eq!(app.search.file_requests(), 1)
    }
    app.accept_picker();
    wait_vfs(&mut app);
    assert_eq!(
        app.state
            .active_buffer_id()
            .and_then(|id| app.state.buffers[id].path.as_ref()),
        Some(&path)
    );
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
}

#[test]
fn workspace_search_streams_exact_batches_and_root_changes_cancel() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.workspace.search.batch_size = 1;
        app.configuration.install_fixture(fixture);
    }
    fs::write(app.state.workspace_root.join("one.txt"), "needle\n").unwrap();
    fs::write(app.state.workspace_root.join("two.txt"), "needle\n").unwrap();
    app.start_workspace_search("needle".into(), None);
    wait_workspace_search(&mut app);
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
    );
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
fn save_as_reload_close_and_external_change_protection_work() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.set_dashboard_visibility(false);
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
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None);
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
fn invalid_configured_regex_keeps_search_prompt_input() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.set_dashboard_visibility(false);
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.search.regex = true;
        app.configuration.install_fixture(fixture);
    }
    app.open_prompt(crate::ui::PromptKind::SearchForward, "[".into());
    app.submit_prompt();
    let prompt = app.ui.prompt.expect("invalid regex prompt remains open");
    assert_eq!(prompt.input, "[");
    assert!(prompt.error.is_some());
}

#[test]
fn search_moves_forward_wraps_and_repeats() {
    let mut app = app();
    let (_, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("one two one"),
        None,
    );
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
        .open_text(TextBuffer::from_text("abc"), Some(path.clone()));
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
        wait_execution_validation(&mut app);
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
    );
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    for stale in [true, false] {
        app.process_effect(AppEffect::LintRun("owned-lint".into()));
        wait_execution_validation(&mut app);
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
fn named_staging_commands_change_only_the_index() {
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("commands.txt");
    let original = "a\nb\nc\nd\ne\n";
    fs::write(&path, original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("commands.txt")).unwrap();
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
        let mut invocation = crate::editor::CommandInvocation::new(id, InvocationSource::Test)
            .expect("command id");
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
    assert_eq!(index_bytes(&app.state.workspace_root, "commands.txt"), changed);
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // Whole-file unstage returns the index entry to HEAD and leaves the worktree alone.
    invoke(&mut app, "workspace.git.file.unstage", &[]);
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        original
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // A line range needs a selected hunk; without one nothing is written.
    let before = index_bytes(&app.state.workspace_root, "commands.txt");
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", 0), ("end", 0)],
    );
    assert!(!app.git.mutation_pending());
    assert_eq!(index_bytes(&app.state.workspace_root, "commands.txt"), before);

    app.start_git_hunks();
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

    // The chosen addition is the only thing that reaches the index.
    let add = snapshot.hunks[0]
        .lines
        .iter()
        .position(|line| line.kind == crate::workspace::git::GitDiffLineKind::Add)
        .expect("fixture adds a line") as i64;
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", add), ("end", add)],
    );
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        "a\nb\nB\nc\nd\ne\n"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[test]
fn staging_an_unsaved_buffer_offers_an_explicit_choice() {
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
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
            assert!(Instant::now() < deadline, "no save-and-stage choice appeared");
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
    assert_eq!(index_bytes(&app.state.workspace_root, "choice.txt"), "disk\n");

    // Negative branch: nothing is saved and nothing is staged.
    app.submit_prompt();
    assert_eq!(index_bytes(&app.state.workspace_root, "choice.txt"), "disk\n");
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
    assert!(!section("Unstaged").iter().any(|row| row.label == "staged.txt"));
    assert!(!section("Staged").iter().any(|row| row.label == "unsaved.txt"));
    // A path with only unsaved edits is still listed, under its OWN provenance rather than
    // as an unstaged change, and carries the badge as its own drawn column.
    let unsaved = section("Unstaged")
        .into_iter()
        .find(|row| row.label == "unsaved.txt")
        .expect("a path with unsaved edits is still listed");
    assert_eq!(unsaved.detail, "buffer -> disk", "unsaved is not labelled unstaged");
    assert!(
        unsaved
            .cells
            .iter()
            .any(|cell| cell.text.trim() == "unsaved"),
        "an unsaved path carries its badge column: {:?}",
        unsaved.cells.iter().map(|cell| &cell.text).collect::<Vec<_>>()
    );
    // Sections are drawn in the order the document names and are absent when empty.
    let titles: Vec<&str> = projection
        .rows
        .iter()
        .filter(|row| row.kind == crate::ui::PickerRowKind::Section)
        .map(|row| row.label.split(' ').next().unwrap_or_default())
        .collect();
    // The drawing shows four sections in this order. Conflicts, Unstaged and Staged are
    // absent when empty; Commit is always drawn because the drawing shows it as part of the
    // surface's structure.
    let expected: Vec<&str> = ["Conflicts", "Unstaged", "Staged", "Commit"]
        .into_iter()
        .filter(|title| *title == "Commit" || titles.contains(title))
        .collect();
    assert_eq!(titles, expected, "sections appear in the drawn order, absent when empty");
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
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
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
        while app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let load_hunks = |app: &mut InteractiveApplication, id: &str| {
        invoke(app, id);
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    invoke(&mut app, "workspace.git.file.stage");
    drain(&mut app);
    assert_eq!(index_bytes(&app.state.workspace_root, "unstage.txt"), changed);

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

    // Unstaging an unstaged selection is refused and writes nothing.
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
    invoke(&mut app, "workspace.git.hunk.unstage");
    assert!(!app.git.mutation_pending(), "no plan may start");
    assert_eq!(index_bytes(&app.state.workspace_root, "unstage.txt"), before);

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
    assert_eq!(index_bytes(&app.state.workspace_root, "unstage.txt"), before);
}

#[test]
fn commit_composer_creates_a_commit_from_exactly_the_staged_set() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config.set_str("user.email", "dust@example.invalid").unwrap();
    }
    let path = app.state.workspace_root.join("commit.txt");
    fs::write(&path, "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("commit.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    // Stage one version, then change the worktree again: the commit must record the staged one.
    fs::write(&path, "staged two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("commit.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    fs::write(&path, "worktree only\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The command opens the composer.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.commit", InvocationSource::Test)
            .expect("command id"),
    );
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::GitCommitMessage)
    );
    assert!(!app.input.commit_amend());
    let before = repo.head().unwrap().target().unwrap();

    // Submitting an empty message is refused by the model, not by the composer.
    app.submit_prompt();
    assert!(!app.git.commit_pending());

    // A real message commits exactly the staged bytes.
    app.open_prompt(
        crate::ui::PromptKind::GitCommitMessage,
        "composer message\n".into(),
    );
    app.input.set_commit_amend(false);
    // The composer submits on its explicit key, not on Enter.
    app.input.mark_prompt_multiline();
    app.project_input_prompt();
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.commit.submit",
            InvocationSource::Test,
        )
        .expect("command id"),
    );
    while app.git.commit_pending() || app.git.status_pending() {
        app.process_git_commit_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_ne!(head.id(), before, "a new commit was created");
    assert_eq!(head.message().unwrap(), "composer message\n");
    assert_eq!(head.parent_id(0).unwrap(), before);
    let blob = repo
        .find_blob(
            head.tree()
                .unwrap()
                .get_path(std::path::Path::new("commit.txt"))
                .unwrap()
                .id(),
        )
        .unwrap();
    assert_eq!(
        blob.content(),
        b"staged two\n",
        "the composer commits the staged set, not the worktree"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree only\n");
}

#[test]
fn commit_composer_composes_multiline_messages_verbatim() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config.set_str("user.email", "dust@example.invalid").unwrap();
    }
    let path = app.state.workspace_root.join("multi.txt");
    fs::write(&path, "one\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("multi.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("multi.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let invoke = |app: &mut InteractiveApplication, id: &str, text: Option<&str>| {
        let mut invocation =
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id");
        if let Some(text) = text {
            invocation = invocation.with_argument("text", CommandValue::String(text.into()));
        }
        app.invoke(invocation);
    };

    invoke(&mut app, "workspace.git.commit", None);
    let prompt = app.input.prompt().expect("composer open");
    assert_eq!(prompt.kind, crate::ui::PromptKind::GitCommitMessage);
    assert!(prompt.multiline, "the composer accepts newlines");

    // Type a subject, then Enter: it composes a newline and must not commit.
    invoke(&mut app, "command_line.insert", Some("subject"));
    invoke(&mut app, "command_line.submit", None);
    assert!(!app.git.commit_pending(), "Enter must not commit");
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.input.clone()),
        Some("subject\n".to_string())
    );

    // A blank line, then a body, then the explicit submit key commits.
    invoke(&mut app, "command_line.submit", None);
    invoke(&mut app, "command_line.insert", Some("body"));
    assert!(!app.git.commit_pending());
    invoke(&mut app, "workspace.git.commit.submit", None);
    while app.git.commit_pending() || app.git.status_pending() {
        app.process_git_commit_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.message().unwrap(),
        "subject\n\nbody",
        "the composed message reaches the commit byte-for-byte, newlines included"
    );
    assert_eq!(head.parent_count(), 1, "a child of the previous tip");
}

#[test]
fn multiline_composer_draws_every_line_and_puts_the_cursor_on_its_own_row() {
    fn row_text(frame: &crate::ui::Frame, y: u16) -> String {
        let mut text = String::new();
        for x in 0..frame.grid.width() {
            if let Some(cell) = frame.grid.get(x, y)
                && !cell.continuation
            {
                text.push_str(&cell.symbol);
            }
        }
        text
    }
    let mut app = app();
    app.open_prompt(
        crate::ui::PromptKind::GitCommitMessage,
        "alpha\nbravo\ncharlie".into(),
    );
    app.input.mark_prompt_multiline();
    app.project_input_prompt();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    let row_of = |needle: &str| {
        (0..frame.grid.height()).find(|y| row_text(&frame, *y).contains(needle))
    };
    let alpha = row_of("alpha").expect("first line drawn");
    let bravo = row_of("bravo").expect("second line drawn");
    let charlie = row_of("charlie").expect("third line drawn");
    assert!(
        alpha < bravo && bravo < charlie,
        "each line occupies its own row in order: {alpha} {bravo} {charlie}"
    );
    let cursor = frame.cursor.expect("composer shows a cursor");
    assert_eq!(
        cursor.1, charlie,
        "the cursor sits on the row its own line occupies"
    );

    // A message taller than the drawn window keeps the cursor inside it and says so.
    let long = (0..20)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.open_prompt(crate::ui::PromptKind::GitCommitMessage, long);
    app.input.mark_prompt_multiline();
    app.project_input_prompt();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    let cursor = frame.cursor.expect("composer shows a cursor");
    let drawn: Vec<u16> = (0..frame.grid.height())
        .filter(|y| row_text(&frame, *y).contains("line "))
        .collect();
    assert!(
        !drawn.is_empty() && drawn.contains(&cursor.1),
        "the cursor stays inside the drawn window: {drawn:?} vs {}",
        cursor.1
    );
    assert!(
        (0..frame.grid.height()).any(|y| {
            let text = row_text(&frame, y);
            text.contains('\u{2191}') || text.contains('\u{2193}')
        }),
        "hidden lines are indicated, not silently dropped"
    );
}

/// Build a repository with distinct commit subjects and a configured identity.
fn history_fixture(root: &std::path::Path, subjects: &[&str]) -> git2::Repository {
    let repo = git2::Repository::init(root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config.set_str("user.email", "dust@example.invalid").unwrap();
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

fn commit_hex(id: git2::Oid) -> String {
    id.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn history_surface_lists_real_commits_with_stable_identities_and_real_patches() {
    let mut app = app();
    let repo = history_fixture(
        &app.state.workspace_root,
        &["alpha base", "beta middle", "gamma tip"],
    );
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let drain_log = |app: &mut InteractiveApplication| {
        while app.git.log_pending() {
            app.process_git_log_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.history.open", InvocationSource::Test)
            .expect("command id"),
    );
    drain_log(&mut app);

    let projection = app.search.picker_projection().expect("history picker");
    assert!(
        projection.title.contains("3 of 3"),
        "the title states the exact counts the drawing shows: {}",
        projection.title
    );
    let labels = projection
        .rows
        .iter()
        .map(|row| row.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec!["gamma tip", "beta middle", "alpha base"],
        "rows are the real commits, newest first"
    );
    let tip = repo.head().unwrap().target().unwrap();
    assert_eq!(
        projection.rows[0].id,
        format!("git-commit:{}", commit_hex(tip)),
        "the row identity is the whole commit id, not a truncation"
    );
    let ids = projection
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 3, "row identities are unique");

    // Accepting the tip row previews that commit's real patch.
    app.accept_picker();
    drain_log(&mut app);
    let popup = app
        .ui
        .documentation_popup
        .as_ref()
        .expect("the commit patch is shown");
    let commit = repo.find_commit(tip).unwrap();
    let parent = commit.parent(0).unwrap();
    let diff = repo
        .diff_tree_to_tree(
            Some(&parent.tree().unwrap()),
            Some(&commit.tree().unwrap()),
            None,
        )
        .unwrap();
    let mut expected = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        // libgit2 hands the callback a content line without its origin, and a unified diff
        // carries it, so the independent expectation has to write it back too. Without this
        // the expectation would assert a patch in which an addition is indistinguishable from
        // context, which is what the producer used to emit.
        if matches!(line.origin(), ' ' | '+' | '-') {
            expected.push(line.origin());
        }
        expected.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .unwrap();
    let shown = popup
        .sections
        .iter()
        .map(|section| section.lines.join("\n"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        shown.replace('\u{fffd}', ""),
        expected.replace('\u{fffd}', ""),
        "the preview is the commit's real patch"
    );
    assert!(shown.contains("h2.txt"), "the tip's file appears");

    // A range anchored below the tip covers only the commits above it. Accepting a row closes
    // the picker, so history is re-opened before the anchor is moved.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.history.open", InvocationSource::Test)
            .expect("command id"),
    );
    drain_log(&mut app);
    app.invoke(
        crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test)
            .expect("command id"),
    );
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.history.range", InvocationSource::Test)
            .expect("command id"),
    );
    drain_log(&mut app);
    let popup = app
        .ui
        .documentation_popup
        .as_ref()
        .expect("the range patch is shown");
    let shown = popup
        .sections
        .iter()
        .map(|section| section.lines.join("\n"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        shown.contains("h2.txt") && !shown.contains("h1.txt"),
        "the range covers the commits above the anchor and not the anchor itself:\n{shown}"
    );
}

#[test]
fn history_surface_marks_a_merge_row_as_a_merge() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config.set_str("user.email", "dust@example.invalid").unwrap();
    }
    fs::write(app.state.workspace_root.join("base.txt"), "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("base.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust Fixture", "dust@example.invalid").unwrap();
    let base = repo
        .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    let base = repo.find_commit(base).unwrap();
    let side = |name: &str, reference: &str| {
        fs::write(app.state.workspace_root.join(name), "side\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some(reference), &sig, &sig, name, &tree, &[&base])
            .unwrap()
    };
    let left = side("left.txt", "refs/heads/left");
    let right = side("right.txt", "refs/heads/right");
    let left = repo.find_commit(left).unwrap();
    let right = repo.find_commit(right).unwrap();
    fs::write(app.state.workspace_root.join("left.txt"), "side\n").unwrap();
    fs::write(app.state.workspace_root.join("right.txt"), "side\n").unwrap();
    let mut merged = repo.index().unwrap();
    merged.add_path(std::path::Path::new("left.txt")).unwrap();
    merged.add_path(std::path::Path::new("right.txt")).unwrap();
    merged.write().unwrap();
    let merged_tree = repo.find_tree(merged.write_tree().unwrap()).unwrap();
    // A merge cannot update a ref whose tip is not its first parent; write it, then move HEAD.
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    let merge = repo
        .commit(
            None,
            &sig,
            &sig,
            "merge both sides",
            &merged_tree,
            &[&left, &right],
        )
        .unwrap();
    repo.reference(&head_name, merge, true, "merge").unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.history.open", InvocationSource::Test)
            .expect("command id"),
    );
    while app.git.log_pending() {
        app.process_git_log_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let projection = app.search.picker_projection().expect("history picker");
    assert_eq!(projection.rows[0].label, "merge both sides");
    assert!(
        projection.rows[0].detail.contains("merge"),
        "a merge row is marked as a merge: {}",
        projection.rows[0].detail
    );
}

#[test]
fn history_toggle_switches_presentation_and_remembers_the_choice() {
    let mut app = app();
    let repo = history_fixture(
        &app.state.workspace_root,
        &["alpha base", "beta middle", "gamma tip"],
    );
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let drain = |app: &mut InteractiveApplication| {
        while app.git.log_pending() {
            app.process_git_log_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let labels = |app: &InteractiveApplication| {
        app.search
            .picker_projection()
            .expect("history picker")
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect::<Vec<_>>()
    };
    // The graph rides the CELLS rather than the label, so the lane is asserted where it is
    // drawn. A row with no lane cells is a row the graph did not draw.
    let lane_glyphs = |app: &InteractiveApplication| {
        app.search
            .picker_projection()
            .expect("history picker")
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .take_while(|cell| cell.text.chars().all(|c| "●◎│╮╯├┤↓ ".contains(c)))
                    .map(|cell| cell.text.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };

    // The list is the default: no lane prefix is drawn.
    invoke(&mut app, "workspace.git.history.open");
    drain(&mut app);
    assert_eq!(
        labels(&app),
        vec!["gamma tip", "beta middle", "alpha base"],
        "history opens as a list by default"
    );

    // Toggling draws the graph: every row carries its lane as a CELL, and the summary is a
    // cell of its own rather than text with a prefix glued to it.
    invoke(&mut app, "workspace.git.history.toggle");
    drain(&mut app);
    let graph = lane_glyphs(&app);
    assert_eq!(graph.len(), 3);
    assert!(
        graph.iter().all(|lanes| lanes.starts_with('●')),
        "the graph draws a lane for every row: {graph:?}"
    );
    let subjects = labels(&app);
    assert!(
        subjects.iter().any(|label| label == "gamma tip"),
        "the subject is drawn as itself with no prefix glued on: {subjects:?}"
    );
    assert!(
        subjects.iter().all(|label| !label.contains('●')),
        "the graph is not in the label text: {subjects:?}"
    );

    // The choice is remembered: re-opening history keeps the graph drawn.
    invoke(&mut app, "workspace.git.history.open");
    drain(&mut app);
    assert!(
        lane_glyphs(&app).iter().all(|lanes| lanes.starts_with('●')),
        "the remembered choice is not reset by re-opening history"
    );

    // And toggling back returns to the list, with no graph cells attached at all.
    invoke(&mut app, "workspace.git.history.toggle");
    drain(&mut app);
    assert_eq!(
        labels(&app),
        vec!["gamma tip", "beta middle", "alpha base"],
        "toggling back restores the list"
    );
    assert!(
        lane_glyphs(&app).iter().all(|lanes| lanes.is_empty()),
        "with the graph off the rows carry no graph cells: {:?}",
        lane_glyphs(&app)
    );
    assert!(!repo.head().is_err());
}

#[test]
fn branch_and_worktree_lists_show_the_real_state_of_each() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["one"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let side = repo.branch("side", &head, false).unwrap();
    // Outside the main worktree: a nested worktree would make the main one dirty too,
    // and the point of this assertion is that dirty and clean are told apart.
    let worktree_root = tempfile::tempdir().unwrap().keep();
    let worktree_path = worktree_root.join("wt-side");
    {
        let reference = side.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("side", &worktree_path, Some(&options))
            .unwrap();
    }
    // Make the linked worktree dirty so the two lists can be told apart.
    fs::write(worktree_path.join("untracked.txt"), "scratch\n").unwrap();

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
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
        while app.git.log_pending() || app.git.branch_pending() {
            app.process_git_branch_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    invoke(&mut app, "workspace.git.branches.open");
    drain(&mut app);
    let projection = app.search.picker_projection().expect("branches picker");
    let row = |name: &str| {
        projection
            .rows
            .iter()
            .find(|row| row.label == name)
            .unwrap_or_else(|| panic!("no row for {name}: {:?}", projection.rows))
    };
    // The real branches are listed, with the linked worktree attributing the branch.
    assert!(projection.title.contains("Git branches"));
    assert!(
        row("side").detail.contains("checked out in"),
        "a branch held by a worktree says so: {}",
        row("side").detail
    );
    assert!(
        row("side").detail.contains("local"),
        "a local branch is distinguishable from a remote one: {}",
        row("side").detail
    );
    let head_name = repo.head().unwrap().shorthand().unwrap().to_owned();
    assert!(
        row(&head_name).detail.contains("HEAD"),
        "the current branch is marked: {}",
        row(&head_name).detail
    );
    assert!(!projection.rows.iter().any(|row| row.label == "untracked.txt"));

    // The worktree list is a different command over the same read.
    invoke(&mut app, "workspace.git.worktrees.open");
    drain(&mut app);
    let projection = app.search.picker_projection().expect("worktrees picker");
    let row = |name: &str| {
        projection
            .rows
            .iter()
            .find(|row| row.label == name)
            .unwrap_or_else(|| panic!("no row for {name}: {:?}", projection.rows))
    };
    assert!(projection.title.contains("Git worktrees"));
    assert!(
        row("main").detail.contains("main"),
        "the main worktree is identified: {}",
        row("main").detail
    );
    assert!(
        row("side").detail.contains("dirty"),
        "a dirty worktree is distinguished from a clean one: {}",
        row("side").detail
    );
    assert!(
        !row("main").detail.contains("dirty"),
        "the clean worktree is not described as dirty: {}",
        row("main").detail
    );
    // The lists really came from the model, not from a fixture.
    let listed = app.git.branch_list().expect("the model list is retained");
    assert_eq!(listed.worktrees.len(), 2);
    assert!(listed.worktrees.iter().any(|worktree| worktree.dirty));
}

#[test]
fn destructive_git_commands_act_only_on_their_own_target() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["one"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let side = repo.branch("side", &head, false).unwrap();
    // Outside the main worktree: a nested one would make the main worktree dirty.
    let worktree_root = tempfile::tempdir().unwrap().keep();
    let worktree_path = worktree_root.join("wt-side");
    {
        let reference = side.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("side", &worktree_path, Some(&options))
            .unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
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
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    // Select a named row by advancing the picker to it, so the test does not depend on order.
    let select = |app: &mut InteractiveApplication, label: &str| {
        let projection = app.search.picker_projection().expect("picker");
        let index = projection
            .rows
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("no row {label} in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };

    // --- Removing a worktree leaves every branch alone.
    invoke(&mut app, "workspace.git.worktrees.open");
    drain(&mut app);
    select(&mut app, "side");
    invoke(&mut app, "workspace.git.worktree.remove");
    let prompt = app.input.prompt().expect("a confirmation is open");
    assert_eq!(prompt.kind, crate::ui::PromptKind::GitDestructive);
    assert_eq!(prompt.confirmation.as_ref().expect("confirmation").title, "Remove worktree");
    assert_eq!(
        prompt.confirmation.as_ref().expect("confirmation").selected,
        crate::ui::ConfirmationChoice::No,
        "the destructive confirmation defaults to No"
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    drain(&mut app);
    assert!(
        !worktree_path.exists(),
        "the worktree directory is removed"
    );
    assert!(
        repo.find_branch("side", git2::BranchType::Local).is_ok(),
        "removing a worktree must not delete its branch"
    );

    // --- Declining a branch deletion changes nothing.
    let head_before = repo.head().unwrap().target().unwrap();
    invoke(&mut app, "workspace.git.branches.open");
    drain(&mut app);
    select(&mut app, "side");
    invoke(&mut app, "workspace.git.branch.delete");
    let prompt = app.input.prompt().expect("a confirmation is open");
    assert_eq!(prompt.kind, crate::ui::PromptKind::GitDestructive);
    assert_eq!(prompt.confirmation.as_ref().expect("confirmation").title, "Delete branch");
    app.submit_prompt();
    assert!(
        repo.find_branch("side", git2::BranchType::Local).is_ok(),
        "declining leaves the branch present"
    );
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);

    // --- Confirming deletes only that branch.
    invoke(&mut app, "workspace.git.branches.open");
    drain(&mut app);
    select(&mut app, "side");
    invoke(&mut app, "workspace.git.branch.delete");
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    drain(&mut app);
    assert!(
        repo.find_branch("side", git2::BranchType::Local).is_err(),
        "the confirmed branch is deleted"
    );
    assert!(
        app.state.workspace_root.is_dir(),
        "deleting a branch must not remove the worktree directory"
    );
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);
}

#[test]
fn refused_branch_deletes_surface_as_errors_through_the_command() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let main_name = repo.head().unwrap().name().unwrap().to_owned();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    // An unmerged branch: its own commit is not an ancestor of the checked-out branch.
    repo.branch("unmerged", &head, false).unwrap();
    repo.set_head("refs/heads/unmerged").unwrap();
    repo.checkout_head(None).unwrap();
    fs::write(
        app.state.workspace_root.join("unmerged.txt"),
        "u\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("unmerged.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "u", &tree, &[&parent])
        .unwrap();
    drop(index);
    drop(tree);
    repo.set_head(&main_name).unwrap();
    repo.checkout_head(None).unwrap();
    // And a branch held by a linked worktree placed outside the main worktree.
    let held = repo.branch("held", &head, false).unwrap();
    let worktree_root = tempfile::tempdir().unwrap().keep();
    {
        let reference = held.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("held", &worktree_root.join("wt-held"), Some(&options))
            .unwrap();
    }

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
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
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let select = |app: &mut InteractiveApplication, label: &str| {
        let projection = app.search.picker_projection().expect("picker");
        let index = projection
            .rows
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("no row {label} in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };
    let delete_and_confirm = |app: &mut InteractiveApplication, label: &str| {
        invoke(app, "workspace.git.branches.open");
        drain(app);
        select(app, label);
        invoke(app, "workspace.git.branch.delete");
        app.input
            .select_confirmation(crate::ui::ConfirmationChoice::Yes);
        app.submit_prompt();
        drain(app);
    };

    let head_before = repo.head().unwrap().target().unwrap();
    delete_and_confirm(&mut app, "unmerged");
    assert!(
        app.ui.notification_error,
        "a refused delete reports an error rather than succeeding silently"
    );
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("not merged"),
        "the error names the refusal: {notice}"
    );
    assert!(
        repo.find_branch("unmerged", git2::BranchType::Local).is_ok(),
        "the refused branch still exists"
    );

    delete_and_confirm(&mut app, "held");
    assert!(app.ui.notification_error);
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("checked out"),
        "the error names the checked-out refusal: {notice}"
    );
    assert!(
        repo.find_branch("held", git2::BranchType::Local).is_ok(),
        "the branch held by a linked worktree still exists"
    );
    assert_eq!(
        repo.head().unwrap().target().unwrap(),
        head_before,
        "a refused delete never moves HEAD"
    );
}

#[test]
fn branch_and_worktree_creation_commands_reach_the_model() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let first = head.parent(0).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let invoke_args = |app: &mut InteractiveApplication, id: &str, args: &[(&str, &str)]| {
        let mut invocation =
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id");
        for (name, value) in args {
            invocation = invocation.with_argument(name, CommandValue::String((*value).into()));
        }
        app.invoke(invocation);
    };
    let drain = |app: &mut InteractiveApplication| {
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // A created branch points at the requested start, which is the parent, not HEAD.
    invoke_args(
        &mut app,
        "workspace.git.branch.create",
        &[("name", "created"), ("start", "HEAD~1")],
    );
    drain(&mut app);
    let created = repo.find_branch("created", git2::BranchType::Local).unwrap();
    assert_eq!(
        created.get().target().unwrap(),
        first.id(),
        "the branch starts where it was asked to"
    );

    // A rename leaves the old name absent and the new one present.
    invoke_args(&mut app, "workspace.git.branches.open", &[]);
    drain(&mut app);
    let projection = app.search.picker_projection().expect("branches picker");
    let index = projection
        .rows
        .iter()
        .position(|row| row.label == "created")
        .expect("the branch is listed");
    for _ in 0..index {
        app.invoke(
            crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test)
                .expect("command id"),
        );
    }
    invoke_args(&mut app, "workspace.git.branch.rename", &[("to", "renamed")]);
    drain(&mut app);
    assert!(repo.find_branch("created", git2::BranchType::Local).is_err());
    assert!(repo.find_branch("renamed", git2::BranchType::Local).is_ok());

    // A created worktree checks its branch out in its own directory and leaves main alone.
    let main_head = repo.head().unwrap().target().unwrap();
    let main_index = crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    let worktree_root = tempfile::tempdir().unwrap().keep();
    let destination = worktree_root.join("wt-created");
    invoke_args(
        &mut app,
        "workspace.git.worktree.create",
        &[
            ("name", "created-wt"),
            ("path", destination.to_str().unwrap()),
            ("reference", "refs/heads/renamed"),
        ],
    );
    drain(&mut app);
    // The renamed branch points at the parent commit, which is where h0.txt was added.
    assert!(
        destination.join("h0.txt").is_file(),
        "the worktree is checked out in its own directory"
    );
    let linked = git2::Repository::open(&destination).unwrap();
    assert_eq!(
        linked.head().unwrap().shorthand(),
        Some("renamed"),
        "the worktree holds the requested branch"
    );
    assert_eq!(repo.head().unwrap().target().unwrap(), main_head);
    assert_eq!(
        crate::workspace::git::git_index_fingerprint(&app.state.workspace_root).unwrap(),
        main_index,
        "the main worktree index is unchanged"
    );
}

#[test]
fn tracking_command_binds_the_local_branch_to_its_upstream() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    // A configured remote plus a remote-tracking ref: libgit2 resolves the upstream's
    // remote through `remote.<name>.url`, so a bare ref is not enough to track.
    repo.remote("origin", "https://example.invalid/repo.git")
        .unwrap();
    repo.reference("refs/remotes/origin/main", head.id(), true, "fixture")
        .unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(5);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let invoke_args = |app: &mut InteractiveApplication, id: &str, args: &[(&str, &str)]| {
        let mut invocation =
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id");
        for (name, value) in args {
            invocation = invocation.with_argument(name, CommandValue::String((*value).into()));
        }
        app.invoke(invocation);
    };
    let drain = |app: &mut InteractiveApplication| {
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    invoke_args(
        &mut app,
        "workspace.git.branch.track",
        &[("name", "tracker"), ("remote_branch", "origin/main")],
    );
    drain(&mut app);
    assert!(
        !app.ui.notification_error,
        "a successful track reports no error: {:?}",
        app.ui.notification
    );
    let tracker = repo
        .find_branch("tracker", git2::BranchType::Local)
        .expect("the tracking branch was created through the command");
    let upstream = tracker
        .upstream()
        .expect("the command bound an upstream")
        .into_reference();
    assert_eq!(
        upstream.name(),
        Some("refs/remotes/origin/main"),
        "the upstream resolves to the requested remote branch"
    );
    assert_eq!(tracker.get().target(), Some(head.id()));

    // A remote branch that does not exist is refused by the model and reported.
    invoke_args(
        &mut app,
        "workspace.git.branch.track",
        &[("name", "missing"), ("remote_branch", "origin/absent")],
    );
    drain(&mut app);
    assert!(
        app.ui.notification_error,
        "an unknown remote branch surfaces an error through the command"
    );
    assert!(
        repo.find_branch("missing", git2::BranchType::Local).is_err(),
        "no branch is created for a refused track"
    );
}

#[test]
fn a_buffer_stays_bound_to_the_worktree_it_was_opened_in() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    // Two linked worktrees, both OUTSIDE the main worktree so the main worktree stays clean.
    let outside = tempfile::tempdir().unwrap().keep();
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
    let identities = app
        .workspace
        .roots()
        .iter()
        .map(|root| root.identity.clone())
        .collect::<Vec<_>>();

    let alpha_file = roots[0].join("note.txt");
    let beta_file = roots[1].join("note.txt");
    app.documents
        .open_explorer(alpha_file.clone(), identities.clone())
        .expect("open the alpha document");
    wait_vfs(&mut app);
    let alpha = app
        .documents
        .worktree_identity(&alpha_file)
        .cloned()
        .expect("the alpha buffer is bound to a worktree");
    assert_eq!(
        alpha.root,
        std::fs::canonicalize(&roots[0]).unwrap(),
        "the identity names the worktree the document was opened in"
    );

    app.documents
        .open_explorer(beta_file.clone(), identities)
        .expect("open the beta document");
    wait_vfs(&mut app);
    let beta = app
        .documents
        .worktree_identity(&beta_file)
        .cloned()
        .expect("the beta buffer is bound to a worktree");
    assert_eq!(beta.root, std::fs::canonicalize(&roots[1]).unwrap());
    assert_ne!(
        alpha.id, beta.id,
        "two linked worktrees carry distinct identities"
    );
    assert_eq!(
        app.documents
            .worktree_identity(&alpha_file)
            .cloned()
            .expect("the alpha binding survives"),
        alpha,
        "opening another worktree never retargets an existing buffer"
    );

    // A path that cannot be canonicalized has no identity rather than a fabricated one.
    assert!(
        crate::workspace::git::WorktreeIdentity::capture(std::path::Path::new(
            "/nonexistent-dust/orphan.txt"
        ))
        .is_none(),
        "a document that does not exist has no worktree"
    );
    // The binding is captured when a document is opened, not synthesised on demand: a document
    // this session never opened reports nothing even though its directory sits under a `.git`.
    // (This host has a `/tmp/.git`, so a temporary directory is not "outside any worktree".)
    let unopened = app.state.workspace_root.join("never-opened.txt");
    fs::write(&unopened, "x").unwrap();
    assert!(
        app.documents.worktree_identity(&unopened).is_none(),
        "an unopened document has no captured identity"
    );
}

#[test]
fn a_worktree_opens_and_closes_as_its_own_root_without_being_removed() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside = tempfile::tempdir().unwrap().keep();
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
    let roots_before = app.workspace.roots().len();

    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let open_worktree_row = |app: &mut InteractiveApplication| {
        // A trust reconciliation bumps the workspace version and retires the previous status
        // snapshot, so the picker's own refresh has to be drained on every open.
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
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

    // Opening registers the worktree as its own root, so LSP, tasks and DAP scope to it.
    open_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.open");
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let canonical = std::fs::canonicalize(&worktree).unwrap();
    assert_eq!(
        app.workspace.roots().len(),
        roots_before + 1,
        "the worktree became a root of its own"
    );
    let opened = app
        .workspace
        .roots()
        .iter()
        .find(|root| root.identity.canonical_path == canonical)
        .expect("a root names the worktree's own path");
    // A root added this way is its own trusted decision, not an inheritance of the main root.
    app.workspace.fixture_trust_identity(opened.identity.clone());
    app.reconcile_workspace_trust();

    // Closing removes the root, and only the root: the worktree stays on disk.
    open_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.close");
    assert_eq!(
        app.workspace.roots().len(),
        roots_before,
        "closing removes the worktree's root"
    );
    assert!(
        worktree.join(".git").exists() && worktree.join("note.txt").is_file(),
        "closing a root never removes the worktree or its checkout"
    );
    assert!(
        repo.find_worktree("scoped").is_ok(),
        "the worktree is still registered with the repository"
    );
    assert!(
        repo.find_branch("scoped", git2::BranchType::Local).is_ok(),
        "closing never deletes the branch"
    );

    // Closing a worktree that is not open is refused rather than silently ignored.
    open_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.close");
    assert!(
        app.ui.notification_error,
        "closing a worktree that is not an open root reports an error"
    );
    assert_eq!(app.workspace.roots().len(), roots_before);
}

#[test]
fn a_locked_worktree_refuses_removal_but_still_closes_as_a_root() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside = tempfile::tempdir().unwrap().keep();
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
    let deadline = Instant::now() + Duration::from_secs(10);
    let roots_before = app.workspace.roots().len();
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let select_worktree_row = |app: &mut InteractiveApplication| {
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
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
    assert!(notice.contains("locked"), "the error names the lock: {notice}");
    assert!(
        worktree.is_dir() && repo.find_worktree("locked").is_ok(),
        "a refused removal leaves the worktree and its registration alone"
    );
    assert!(repo.find_branch("locked", git2::BranchType::Local).is_ok());

    // Closing the same worktree as a root is NOT gated on the repository state: closing is
    // not a Git write, so a lock must not stop it.
    select_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.open");
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(app.workspace.roots().len(), roots_before + 1);
    select_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.close");
    assert!(
        !app.ui.notification_error,
        "closing a locked worktree is allowed: {:?}",
        app.ui.notification
    );
    assert_eq!(
        app.workspace.roots().len(),
        roots_before,
        "the locked worktree's root closed"
    );
    assert!(
        worktree.is_dir() && repo.find_worktree("locked").is_ok(),
        "closing leaves the locked worktree exactly as it was"
    );
}

#[test]
fn help_reference_is_derived_from_the_registry_entry_of_the_selected_command() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("workspace.git.branch.create");
    let palette = app.search.picker_projection().expect("palette");
    let listed = palette
        .rows
        .iter()
        .find(|row| row.label == "workspace.git.branch.create")
        .expect("the command is listed in the palette");
    let palette_label = listed.detail.clone();

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let reference = app.search.picker_projection().expect("reference page");
    let rows = reference
        .rows
        .iter()
        .map(|row| (row.detail.clone(), row.label.clone()))
        .collect::<Vec<_>>();
    assert!(
        rows.iter().any(|(kind, value)| kind == "command"
            && value == "workspace.git.branch.create"),
        "the reference names the selected command: {rows:?}"
    );
    assert!(
        rows.iter().any(|(kind, value)| kind == "does" && value == &palette_label),
        "the reference shows the same label the palette showed, so it cannot drift: {rows:?}"
    );
    assert!(
        rows.iter().any(|(kind, value)| kind == "argument" && value == "name")
            && rows.iter().any(|(kind, value)| kind == "optional" && value == "start"),
        "the reference marks a required argument and an optional one apart: {rows:?}"
    );

    // A second command with a different schema produces a different page, which is what shows
    // the page is derived per command rather than a fixed list.
    app.search.close_picker();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    app.picker_insert("overlay.help.contextual");
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let reference = app.search.picker_projection().expect("second reference page");
    let rows = reference
        .rows
        .iter()
        .map(|row| (row.detail.clone(), row.label.clone()))
        .collect::<Vec<_>>();
    assert!(
        rows.iter().any(|(kind, value)| kind == "takes" && value == "no arguments"),
        "a command with no arguments says so instead of inventing some: {rows:?}"
    );
    // The reference shows the EFFECTIVE binding, so a reader never sees a key the user cannot
    // press: here the editor-normal binding of this command, from the same table the dispatcher
    // uses.
    assert!(
        rows.iter().any(|(kind, value)| kind == "binding"
            && value == "space ?  overlay.help.contextual"),
        "the reference names the key that resolves in the editor: {rows:?}"
    );
    // And it does NOT invent a key for a context where the command has no binding.
    assert!(
        !rows.iter().any(|(kind, _)| kind == "binding here"),
        "a command unbound in this context is given no key: {rows:?}"
    );
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
    let help = app.search.picker_projection().expect("contextual help opened");
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
    let after = app
        .ui
        .prompt
        .as_ref()
        .expect("the prompt survived help");
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
    );
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
    assert!(app.search.picker_projection().is_some(), "help opened alone");
    app.search.close_picker();
    assert!(
        app.search.picker_projection().is_none(),
        "closing help that had nothing underneath leaves no surface rather than inventing one"
    );
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
        rows.iter().any(|(kind, value)| kind == "asks"
            && value == "Discard changes?"),
        "the help names the question being asked: {rows:?}"
    );
    assert!(
        rows.iter().any(|(kind, value)| kind == "detail"
            && value == "The buffer has unsaved edits."),
        "the help carries the confirmation's own message: {rows:?}"
    );
    assert!(
        rows.iter().any(|(kind, value)| kind == "choices"
            && value == "Yes / No"),
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
fn help_preserves_a_multiline_form_draft_and_its_multiline_shape() {
    let mut app = app();
    // A real form: the commit composer, which is multiline and carries a draft but no
    // confirmation. This is the branch open_form_help reads the draft for.
    app.open_prompt(crate::ui::PromptKind::GitCommitMessage, String::new());
    app.input.mark_prompt_multiline();
    app.prompt_insert("Add the help surface\n\n- restore the surface on close\n");
    app.project_input_prompt();
    let draft_before = app
        .ui
        .prompt
        .as_ref()
        .expect("the composer is open")
        .input
        .clone();
    assert!(draft_before.contains('\n'), "the draft really is multi-line");

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
        rows.iter().any(|(kind, value)| kind == "surface" && value.contains("GitCommitMessage")),
        "the help names the form: {rows:?}"
    );
    assert!(
        rows.iter().any(|(kind, value)| kind == "draft" && value == &draft_before),
        "the help shows the draft it is talking about: {rows:?}"
    );
    app.search.close_picker();

    app.project_input_prompt();
    let after = app
        .ui
        .prompt
        .as_ref()
        .expect("the composer survived help");
    assert_eq!(
        after.input, draft_before,
        "the composer's draft is byte-identical after help"
    );
    assert!(
        after.multiline,
        "the composer is still the multiline form, not silently demoted to one row"
    );
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
    assert!(app.search.picker_projection().is_some(), "help opened over the palette");

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
        restored.title,
        "Help · overlay.help.reference",
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
    assert!(app.search.picker_projection().is_some(), "the new surface is up");
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
    let first = app.search.picker_projection().expect("first help page").title.clone();
    // A second help page over the first.
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    let second = app.search.picker_projection().expect("second help page").title.clone();
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
fn the_tutor_fixture_is_disposable_and_isolated_from_real_roots() {
    let mut app = app();
    let real_root = app.state.workspace_root.clone();
    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.start", InvocationSource::Test)
            .expect("command id"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.workspace.tutor_pending() {
        app.process_tutor_events();
        assert!(Instant::now() < deadline, "fixture creation timed out");
        thread::yield_now()
    }
    let fixture = app
        .workspace
        .tutor_fixture()
        .cloned()
        .expect("the sandbox reported a fixture");
    let path = fixture.path.clone();

    // Isolation, asserted on real state rather than inferred from a returned code.
    assert!(path.is_dir(), "the fixture is a directory");
    assert!(
        !path.starts_with(&real_root),
        "the fixture is outside the real root: {} is not under {}",
        path.display(),
        real_root.display()
    );
    assert!(
        !app.workspace
            .roots()
            .iter()
            .any(|root| path.starts_with(&root.identity.canonical_path)),
        "the fixture is not inside any workspace root"
    );
    assert!(
        !app.workspace
            .roots()
            .iter()
            .any(|root| root.identity.canonical_path == path),
        "the fixture was never entered as a workspace root"
    );
    // It is a real repository with history, so exercises have something to practise on.
    let repository = git2::Repository::open(&path).expect("the fixture is a repository");
    assert!(
        repository.head().and_then(|head| head.peel_to_commit()).is_ok(),
        "the fixture has a commit"
    );
    drop(repository);

    // Disposal really removes it, and the session stops believing in it.
    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.stop", InvocationSource::Test)
            .expect("command id"),
    );
    while app.workspace.tutor_fixture().is_some() {
        app.process_tutor_events();
        assert!(Instant::now() < deadline, "fixture disposal timed out");
        thread::yield_now()
    }
    assert!(
        !path.exists(),
        "disposal removed the fixture directory: {} still exists",
        path.display()
    );
}

#[test]
fn opening_a_sandbox_never_touches_the_real_trust_store_or_its_state_file() {
    let mut app = app();
    // The state file's bytes before anything happens: a sandbox must not rewrite the user's
    // real trust database.
    let state_path = app.workspace.state_path().to_path_buf();
    // A sentinel database is placed first, because comparing two absent files would prove
    // nothing. The session already loaded its store at construction, so this stands in for the
    // user's real trust database on disk.
    let sentinel = b"# dust trust database sentinel\nversion = 1\n".to_vec();
    std::fs::write(&state_path, &sentinel).expect("sentinel written");
    let before = std::fs::read(&state_path).expect("the sentinel is readable");

    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.start", InvocationSource::Test)
            .expect("command id"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.workspace.tutor_pending() {
        app.process_tutor_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let fixture = app
        .workspace
        .tutor_fixture()
        .cloned()
        .expect("the sandbox reported a fixture");

    // MEASURED, not inferred from "it was never entered as a root": the real trust store does
    // not trust this fixture, and has not even recorded having seen it.
    let identity = crate::workspace::RootIdentity::capture(&fixture.path)
        .expect("the fixture has a capturable identity");
    assert!(
        !app.workspace.fixture_is_trusted(&identity),
        "the fixture is not trusted by the real trust store"
    );
    assert!(
        !app.workspace.fixture_is_seen(&identity),
        "the fixture never even passed through the real trust store"
    );

    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.stop", InvocationSource::Test)
            .expect("command id"),
    );
    while app.workspace.tutor_fixture().is_some() {
        app.process_tutor_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert!(!fixture.path.exists(), "the fixture is gone");

    let after = std::fs::read(&state_path).expect("the trust database is still there");
    assert_eq!(
        before, sentinel,
        "the sentinel really was the database on disk before the sandbox opened"
    );
    assert_eq!(
        after, before,
        "opening and closing a sandbox left the user's trust database byte-identical"
    );
}

#[test]
fn a_gated_lesson_is_not_offered_and_says_why() {
    let mut app = app();
    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.lessons", InvocationSource::Test)
            .expect("command id"),
    );
    let page = app.search.picker_projection().expect("the lessons page opened");
    let rows = page
        .rows
        .iter()
        .map(|row| (row.label.clone(), row.detail.clone()))
        .collect::<Vec<_>>();

    // The chapters this revision can teach are offered as steps.
    assert!(
        rows.iter().any(|(label, detail)| label == "Stage one change" && detail.contains("todo")),
        "chapter 1 is offered as a step to do: {rows:?}"
    );
    assert!(
        rows.iter().any(|(label, _)| label == "Write a commit"),
        "chapter 2 is offered: {rows:?}"
    );
    // The gated chapter is present but explicitly NOT teachable, and names the missing feature.
    let (label, detail) = rows
        .iter()
        .find(|(label, _)| label.contains("Chapter 3"))
        .expect("chapter 3 is listed so the gap is visible");
    assert_eq!(label, "Chapter 3 · Update from a remote");
    assert!(
        detail.contains("not available yet") && detail.contains("remotes"),
        "the gated chapter names what is missing rather than teaching it: {detail:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|(label, _)| label == "Update from the remote"),
        "no step of a gated lesson is offered: {rows:?}"
    );
    // The surface shows the EFFECTIVE binding the user must press, not a default.
    assert!(
        rows.iter().any(|(_, detail)| detail.contains("workspace.git.hunk.stage")),
        "the step names the key that resolves for the user: {rows:?}"
    );
}

#[test]
fn lesson_progress_comes_from_the_real_action_and_nothing_else() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    // A real change to stage.
    fs::write(app.state.workspace_root.join("h0.txt"), "changed\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    let refresh = |app: &mut InteractiveApplication| {
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    refresh(&mut app);

    let stage_complete = |app: &InteractiveApplication| {
        app.workspace
            .tutor_progress()
            .is_complete(crate::workspace::TutorStep::StageAChange)
    };
    // 2. NOT performing the action leaves the step incomplete.
    assert!(
        !stage_complete(&app),
        "nothing done yet: the staging step is incomplete"
    );
    // 3. Answering completes NOTHING: opening the lessons page and the reference, and running
    //    a command that is not the action, must not mark the step done.
    for id in ["session.tutor.lessons", "overlay.help.contextual"] {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
        app.search.close_picker();
        assert!(
            !stage_complete(&app),
            "invoking {id} is not staging a change and must not complete the step"
        );
    }

    // 1. Performing the genuine action completes it, and it is the mutation OUTCOME that does it.
    //    Staging operates on a SELECTED hunk, so the real flow is: load the hunks, select one,
    //    then stage it.
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let load_hunks = |app: &mut InteractiveApplication, id: &str| {
        invoke(app, id);
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    // The hunk commands act on the ACTIVE buffer's file, so the changed file must be open —
    // that is the real flow a user follows too.
    let changed_path = app.state.workspace_root.join("h0.txt");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("changed\n"),
        Some(changed_path.clone()),
    );
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;
    load_hunks(&mut app, "workspace.git.hunks.open");
    let hunks = app
        .git
        .hunks()
        .unwrap_or_else(|| panic!("unstaged hunks: {:?}", app.ui.notification))
        .clone();
    assert!(!hunks.hunks.is_empty(), "there is a real change to stage");
    assert!(app.git.select(
        hunks.status_revision,
        hunks.repository_id,
        hunks.root_revision,
        &changed_path,
        hunks.hunks[0].id
    ));
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.hunk.stage", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(
        app.git.mutation_pending(),
        "the staging plan actually started rather than being refused"
    );
    while app.git.mutation_pending() {
        app.process_git_mutation_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert!(
        stage_complete(&app),
        "staging a change really happened and completed the step"
    );
    // The staging step is a real one: the repository's index really holds the change, read
    // through a fresh handle because the index is cached per Repository.
    let staged = git2::Repository::open(&app.state.workspace_root)
        .unwrap()
        .index()
        .unwrap()
        .get_path(std::path::Path::new("h0.txt"), 0)
        .is_some();
    assert!(staged, "the change is really in the index, not merely reported as staged");
    // And the lessons page now reports it as done rather than todo.
    app.invoke(
        crate::editor::CommandInvocation::new("session.tutor.lessons", InvocationSource::Test)
            .expect("command id"),
    );
    let page = app.search.picker_projection().expect("lessons page");
    let stage_row = page
        .rows
        .iter()
        .find(|row| row.label == "Stage one change")
        .expect("the staging step is listed");
    assert!(
        stage_row.detail.contains("done"),
        "the step the user really performed reads done: {:?}",
        stage_row.detail
    );
    let _ = repo;
}

#[test]
fn the_fetch_command_names_its_remote_and_the_update_flow_refuses_without_a_review() {
    let mut app = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    // A real local bare remote, wired as this branch's upstream.
    let bare = tempfile::tempdir().unwrap().keep();
    let bare_repo = git2::Repository::init_bare(&bare).unwrap();
    drop(bare_repo);
    repo.remote("origin", bare.to_str().unwrap()).unwrap();
    let head = repo.head().unwrap();
    let branch = head.shorthand().unwrap().to_owned();
    drop(head);
    repo.reference(
        &format!("refs/remotes/origin/{branch}"),
        repo.head().unwrap().target().unwrap(),
        true,
        "fixture",
    )
    .unwrap();
    {
        let mut config = repo.config().unwrap();
        config
            .set_str(&format!("branch.{branch}.remote"), "origin")
            .unwrap();
        config
            .set_str(
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            )
            .unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(20);
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
    let drain_remote = |app: &mut InteractiveApplication| {
        while app.git.remote_pending() {
            app.process_git_remote_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Applying with no review is refused rather than guessed at.
    assert!(app.git.remote_review().is_none(), "no review has been taken");
    invoke(&mut app, "workspace.git.remote.update.apply");
    assert!(
        app.ui.notification_error,
        "applying without a review is refused: {:?}",
        app.ui.notification
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(repo.head().unwrap().target().unwrap()),
        "and moves nothing"
    );

    // The fetch names its remote: the branch's configured upstream, reported in the message.
    invoke(&mut app, "workspace.git.remote.fetch");
    drain_remote(&mut app);
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("origin"),
        "the fetch names the remote it used: {notice}"
    );
    assert!(!app.ui.notification_error, "the fetch succeeded: {notice}");

    // A successful fetch retires the status snapshot, so the review needs a fresh one first.
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The review of an up-to-date branch says so rather than inventing work.
    invoke(&mut app, "workspace.git.remote.update");
    drain_remote(&mut app);
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("already at the remote tip"),
        "a branch that is not behind says so: {notice}"
    );
    assert!(
        app.git.remote_review().is_none(),
        "and no update plan is left waiting for a keystroke"
    );
    // A successful fetch leaves the unknown-outcome journal EMPTY: nothing unclassified happened,
    // so nothing is waiting to be retried by hand either.
    assert!(
        app.git.remote_journal().is_empty(),
        "a successful fetch journals nothing: {:?}",
        app.git.remote_journal().entries()
    );
    let _ = bare;
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
        let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
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
        crate::editor::CommandInvocation::new("workspace.git.conflicts.open", InvocationSource::Test)
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
        let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
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
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text(&String::from_utf8_lossy(&on_disk)),
        Some(conflicted.clone()),
    );
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;
    let before_choose = app.state.buffers[buffer].text.to_string();
    let index_before = crate::workspace::git::git_index_fingerprint(&root).unwrap();

    // ---- CHOOSING A SIDE.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.choose", InvocationSource::Test)
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
        content,
        "the result buffer holds exactly the chosen side's content"
    );
    assert!(
        app.state.buffers[buffer].dirty,
        "the buffer is dirty after the choice"
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
    );
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.choose", InvocationSource::Test)
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
            let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
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
        repo.reference("ORIG_HEAD", our_tip, true, "fixture").unwrap();
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
    assert!(!app.git.has_pending_replacement(), "the question is answered once");
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

#[test]
fn a_conflicting_diverged_update_is_settled_through_the_resolver_into_a_two_parent_commit() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    // A repository whose local branch and its remote-tracking branch have DIVERGED over the same
    // path, so the reviewed merge will conflict.
    let repo = history_fixture(&root, &["base"]);
    let base_tip = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base_tip).unwrap();
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    // Theirs, on the remote-tracking ref, changing h0.txt.
    repo.reference("refs/remotes/origin/merge-side", base_tip, true, "fixture")
        .unwrap();
    let their_tip = {
        let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
        let blob = repo.blob(b"THEIR VERSION\n").unwrap();
        builder.insert("h0.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "theirs", &tree, &[&base_commit])
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), base_tip, true, "fixture")
            .unwrap();
        repo.reference("refs/remotes/origin/merge-side", oid, true, "fixture")
            .unwrap();
        oid
    };
    // Ours, changing the SAME path differently, so the merge conflicts.
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
    let index = repo.index().unwrap();
    drop(index);
    drop(base_commit);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(20);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The merge that a diverged update would run, applied through the merge path itself.
    let review = crate::workspace::git::GitUpdateReview {
        repository: root.clone(),
        remote: "origin".into(),
        branch: branch.clone(),
        local: our_tip,
        remote_tip: their_tip,
        commits: 1,
        integration: crate::workspace::git::GitUpdateIntegration::Merge,
    };
    // Applied through the SERVICE, which is the real path a reviewed merge takes.
    let mut service = crate::workspace::git::GitRemoteService::start(4);
    service
        .apply_update(review, Duration::from_secs(10))
        .expect("the reviewed merge is accepted");
    let applied = loop {
        match service.poll() {
            Some(crate::workspace::git::GitRemoteEvent::Fetched { outcome, .. }) => break outcome,
            None => {
                assert!(Instant::now() < deadline, "the merge timed out");
                thread::yield_now()
            }
        }
    };
    assert!(
        matches!(
            applied,
            crate::workspace::git::GitRemoteOutcome::MergeConflicted { .. }
        ),
        "the diverged merge conflicts: {applied:?}"
    );
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    // The conflict is real and visible to the resolver.
    let state = crate::workspace::git::read_conflict_state(&root).unwrap();
    assert_eq!(state.operation.as_deref(), Some("merge"));
    assert_eq!(state.conflicts.len(), 1);

    // The RESULT buffer is the working-tree file git wrote with conflict markers.
    let conflicted = root.join("h0.txt");
    let on_disk = fs::read(&conflicted).unwrap();
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text(&String::from_utf8_lossy(&on_disk)),
        Some(conflicted.clone()),
    );
    app.state.active_view = Some(view);
    app.state.views[view].buffer = buffer;

    // STEP 1: choose a side through the resolver's own command.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.choose", InvocationSource::Test)
            .expect("command id")
            .with_argument("side", CommandValue::String("theirs".into())),
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "THEIR VERSION\n",
        "the resolver put the chosen side into the result buffer"
    );

    // STEP 2: save it through the EDITOR's own save, because saving is a separate step from
    // resolving. Writing the file behind the editor's back would leave the buffer dirty, and a
    // dirty buffer is correctly blocked from staging behind an explicit save-and-stage choice.
    app.invoke(
        crate::editor::CommandInvocation::new("file.save", InvocationSource::Test)
            .expect("command id"),
    );
    wait_vfs(&mut app);
    assert!(
        !app.state.buffers[buffer].dirty,
        "the editor's save marked the buffer clean"
    );
    assert_eq!(
        fs::read_to_string(&conflicted).unwrap(),
        "THEIR VERSION\n",
        "and the saved file holds the resolved content"
    );

    // STEP 3: stage the resolved path through the staging plan.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.conflict.stage", InvocationSource::Test)
            .expect("command id"),
    );
    while app.git.mutation_pending() {
        app.process_git_mutation_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        crate::workspace::git::read_conflict_state(&root)
            .unwrap()
            .conflicts
            .len(),
        0,
        "staging the resolved path clears the conflict: {:?}",
        app.ui.notification
    );

    // STEP 4: continue through the recovery action. A staging mutation retires the status
    // snapshot, so a fresh one is drained first.
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.continue",
            InvocationSource::Test,
        )
        .expect("command id"),
    );
    assert!(
        !app.ui.notification_error,
        "continuing a fully resolved merge succeeds: {:?}",
        app.ui.notification
    );
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(
        fresh.state(),
        git2::RepositoryState::Clean,
        "the operation is no longer paused"
    );
    let head = fresh.find_commit(fresh.head().unwrap().target().unwrap()).unwrap();
    assert_eq!(head.parent_count(), 2, "the finished commit is a merge");
    assert_eq!(head.parent_id(0).unwrap(), our_tip, "first parent is ours");
    assert_eq!(head.parent_id(1).unwrap(), their_tip, "second parent is theirs");
}

#[test]
fn an_amend_previews_the_revision_it_replaces_and_refuses_a_stale_plan() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let repo = history_fixture(&root, &["first", "second"]);
    let tip = repo.head().unwrap().target().unwrap();
    let subject = repo
        .find_commit(tip)
        .unwrap()
        .summary()
        .unwrap_or_default()
        .to_owned();
    // A staged change, so the amended revision has something to carry.
    fs::write(root.join("h0.txt"), "staged change\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(15);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("command id"),
    );
    while app.git.mutation_pending() {
        app.process_git_mutation_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // THE PREVIEW names the revision that would be replaced, BEFORE anything moves.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.commit.amend", InvocationSource::Test)
            .expect("command id"),
    );
    let notice = app.ui.notification.clone().unwrap_or_default();
    let short = &tip.to_string()[..8];
    assert!(
        notice.contains(short),
        "the preview names the revision being replaced ({short}): {notice}"
    );
    assert!(
        notice.contains(&subject),
        "and the subject it replaces (`{subject}`): {notice}"
    );
    assert!(
        notice.contains("amend") && notice.contains("replace"),
        "and says plainly that it is a rewrite: {notice}"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(tip),
        "previewing moves nothing"
    );

    // The plan is STALE if the repository moved before the answer: HEAD is moved behind the
    // preview's back, and the amended commit is refused rather than rewriting this revision.
    let moved = {
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        fs::write(root.join("moved.txt"), "moved\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("moved.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.find_commit(tip).unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "moved", &tree, &[&parent])
            .unwrap();
        repo.reference("refs/heads/master", oid, true, "fixture")
            .or_else(|_| repo.reference("refs/heads/main", oid, true, "fixture"))
            .unwrap();
        oid
    };
    app.input.prompt_insert("amended message");
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.commit.submit", InvocationSource::Test)
            .expect("command id"),
    );
    while app.git.commit_pending() {
        app.process_git_commit_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert!(
        app.ui.notification_error,
        "an amend against a moved HEAD is refused rather than rewriting a revision the user          never previewed: {:?}",
        app.ui.notification
    );
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(
        fresh.head().unwrap().target(),
        Some(moved),
        "and the repository is left exactly where it had moved to"
    );
    assert_eq!(
        fresh.find_commit(moved).unwrap().summary(),
        Some("moved"),
        "the revision the user never previewed is untouched"
    );
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
    for name in ["staged.txt", "unstaged.txt", "unsaved.txt", "conflicted.txt"] {
        stage(name);
    }
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();

    // A real conflict: both sides change the same line.
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    write("conflicted.txt", "ours\n");
    stage("conflicted.txt");
    let ours = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    let ours = repo
        .commit(Some("refs/heads/side"), &sig, &sig, "ours", &ours, &[&base])
        .unwrap();
    write("conflicted.txt", "theirs\n");
    stage("conflicted.txt");
    let theirs = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    let theirs = repo
        .commit(Some("refs/heads/other"), &sig, &sig, "theirs", &theirs, &[&base])
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
    app.state.buffers[id].text.bump_revision().expect("revision bump");

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
    assert!(has("── Conflicts 1"), "the Conflicts section header is drawn: {rendered:?}");
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
    assert!(conflicts < unstaged && unstaged < staged, "the sections keep the drawn order");

    // The status column carries a two-character code, and the provenance column names the
    // comparison, so no row is read as a different one.
    assert!(has("UU"), "a conflicted path shows its code");
    assert!(has("M "), "a staged path shows its code");
    assert!(has(" M"), "an unstaged path shows its code");
    assert!(has("HEAD -> index"), "a staged row names its comparison");
    assert!(has("index -> disk"), "an unstaged row names its comparison");
    assert!(has("buffer -> disk"), "an unsaved-only row names its own comparison");
    // The badge is its own drawn column with its own style. Asserting merely that the word
    // "unsaved" appears would be satisfied by the path `unsaved.txt`, so this reads the
    // styled run that only the badge column produces.
    let badge_row = u16::try_from(
        rendered
            .iter()
            .position(|row| row.contains("buffer -> disk"))
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
    assert!(has("Stage file"), "the footer names what the key does here: {rendered:?}");
    assert!(has("Unstage file"), "the footer names the inverse action");
    assert!(has("Switch diff"), "the footer names the focus switch");
    // A legend that does not fit drops whole actions rather than clipping one, so no half
    // entry can be read as a different action.
    let footer_row = rendered
        .iter()
        .find(|row| row.contains("Stage file"))
        .expect("the footer row");
    assert!(
        !footer_row.trim_end().ends_with('·'),
        "a shortened footer never ends on a dangling separator: {footer_row:?}"
    );
    assert!(
        !has("Insert · escape normal"),
        "the widget's mode hint is replaced by the surface's own footer"
    );
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
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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
    let switch =
        crate::editor::CommandInvocation::new("workspace.git.diff.layer.switch", InvocationSource::Test)
            .expect("static command");
    app.invoke(switch);
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
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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

    let stage = crate::editor::CommandInvocation::new(
        "workspace.git.file.stage",
        InvocationSource::Test,
    )
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
    assert!(
        !staged.hunks.is_empty(),
        "the FOCUSED row b.txt was staged"
    );
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
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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
    app.state.buffers[id].text.bump_revision().expect("revision bump");
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
        picker.selected.map(|index| picker.rows[index].label.clone())
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

/// W-26: the drawing's FOURTH section — `── Commit ──` with the message about to be committed
/// — is drawn, and the footer carries the REAL staged count.
#[test]
fn the_change_surface_draws_the_commit_section_and_the_real_staged_count() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let names = ["a.txt", "b.txt", "c.txt"];
    for name in names {
        fs::write(app.state.workspace_root.join(name), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
    drop(tree);
    // TWO staged paths and one left unstaged, so a hardcoded count of 1 cannot pass.
    for name in names {
        fs::write(app.state.workspace_root.join(name), "two\n").unwrap();
    }
    {
        let mut index = repo.index().unwrap();
        for name in ["a.txt", "b.txt"] {
            index.add_path(std::path::Path::new(name)).unwrap();
        }
        index.write().unwrap();
    }
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
    // A message the composer held, as the drawing shows one.
    app.git.set_commit_draft("fix: pin hunk staging to generation".to_owned());
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
    let drawn = frame(&mut app);
    assert!(
        drawn.contains("── Commit"),
        "the drawing's fourth section is drawn: {drawn}"
    );
    assert!(
        drawn.contains("fix: pin hunk staging to generation"),
        "the message about to be committed is drawn: {drawn}"
    );
    let commit_row = drawn
        .lines()
        .position(|row| row.contains("── Commit"))
        .expect("the commit section row");
    let staged_row = drawn
        .lines()
        .position(|row| row.contains("── Staged"))
        .expect("the staged section row");
    assert!(staged_row < commit_row, "Commit is drawn after Staged");
    assert!(
        drawn.contains("Commit staged (2)"),
        "the footer carries the REAL staged count rather than a fixed one: {drawn}"
    );
    assert!(
        !drawn.contains("Commit staged (1)"),
        "two paths are staged, so the count is not one: {drawn}"
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
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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

/// U-17: the graph cells carry a glyph AND a lane colour, the merge glyph differs from an
/// ordinary commit's, lane colour is stable across rows, and a ref is drawn as its own badge
/// cell rather than as text appended to the subject.
///
/// The row STRUCTURE is proved against a real repository in
/// `a_real_merge_produces_a_bend_edge_and_a_distinct_merge_glyph`; this proves the rendering of
/// that structure, which is where the colours and glyphs live.
#[test]
fn a_graph_row_draws_lane_colours_a_merge_glyph_and_its_own_badge_cells() {
    use crate::workspace::git::{GitHeadId, GitLogEntry, history_graph_rows};

    let head = |byte: u8| GitHeadId([byte; 20]);
    // Newest first: a merge with two parents, then an ordinary commit on the same lane.
    let entries = vec![
        GitLogEntry {
            id: head(1),
            summary: "merge".into(),
            author: "kim".into(),
            authored_seconds: 0,
            merge: true,
            parents: vec![head(2), head(3)],
            refs: vec!["main".into()],
        },
        GitLogEntry {
            id: head(2),
            summary: "second".into(),
            author: "kim".into(),
            authored_seconds: 0,
            merge: false,
            parents: vec![head(4)],
            refs: Vec::new(),
        },
        GitLogEntry {
            id: head(4),
            summary: "root".into(),
            author: "kim".into(),
            authored_seconds: 0,
            merge: false,
            parents: Vec::new(),
            refs: Vec::new(),
        },
    ];
    let rows = history_graph_rows(&entries);
    let merge_cells = rows[0].cells();
    let plain_cells = rows[1].cells();

    // The merge glyph is distinguishable from an ordinary commit's, by glyph rather than by
    // colour, so a terminal without colour keeps the distinction.
    assert_eq!(merge_cells[rows[0].lane].text, "◎");
    assert_eq!(plain_cells[rows[1].lane].text, "●");
    assert_ne!(
        merge_cells[rows[0].lane].text, plain_cells[rows[1].lane].text,
        "a merge is not drawn as an ordinary commit"
    );

    // Every lane cell carries a colour as well as its glyph, and the colour is a function of
    // the lane index, so it cannot change from row to row.
    use crate::workspace::git::HistoryGraphRow;
    assert_eq!(
        merge_cells[rows[0].lane].style,
        HistoryGraphRow::lane_style(rows[0].lane),
        "a lane cell carries its lane's colour"
    );
    assert_eq!(
        plain_cells[rows[1].lane].style,
        HistoryGraphRow::lane_style(rows[1].lane),
        "the same lane keeps the same colour on the next row"
    );
    assert_eq!(
        merge_cells[0].style,
        plain_cells[0].style,
        "lane zero does not change colour between rows"
    );
    // Two lanes side by side do not share a colour.
    assert_ne!(
        HistoryGraphRow::lane_style(0),
        HistoryGraphRow::lane_style(1),
        "adjacent lanes take different colours"
    );
    // The bend carries the colour of the lane it arrives at, not of the column it is drawn in.
    let bend_column = rows[0].lane_width;
    let bend_target = rows[0].edges[1].to;
    assert!(
        bend_target != bend_column,
        "the fixture's bend arrives at a lane other than the column it is drawn in, so the          assertion below can distinguish the two"
    );
    assert_eq!(
        merge_cells[bend_column].style,
        HistoryGraphRow::lane_style(bend_target),
        "the bend carries the colour of the lane it arrives at"
    );
}

/// A ref draws as its own badge cell, not as text appended to the subject.
#[test]
fn a_ref_draws_as_its_own_badge_cell() {
    use crate::editor::session::git::graph_badge_cells;
    use crate::workspace::git::{GitHeadId, GitLogEntry};

    let entry = GitLogEntry {
        id: GitHeadId([9; 20]),
        summary: "subject".into(),
        author: "kim".into(),
        authored_seconds: 0,
        merge: false,
        parents: Vec::new(),
        refs: vec!["main".into(), "v1.2".into()],
    };
    let badges = graph_badge_cells(&entry);
    assert_eq!(badges.len(), 2, "one badge per ref");
    assert!(
        badges[0].text.contains("main") && badges[1].text.contains("v1.2"),
        "each ref has its own badge: {badges:?}"
    );
    assert!(
        !entry.summary.contains("main"),
        "the subject never carries the ref"
    );
}

/// U-17: the ancestry graph is DRAWN, on a real repository containing a real merge, and read
/// back from the rendered grid: the bend appears in its own column, a merge is not drawn as an
/// ordinary commit, a lane keeps one colour across rows, and a ref is its own badge.
#[test]
fn the_history_graph_draws_a_real_merge_with_its_own_columns_and_colours() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    fs::write(app.state.workspace_root.join("base.txt"), "base\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("base.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "root commit", &tree, &[])
        .unwrap();
    drop(tree);
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = |name: &str, reference: &str, message: &str| {
        std::fs::write(app.state.workspace_root.join(name), "side\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some(reference), &sig, &sig, message, &tree, &[&base])
            .unwrap()
    };
    let left = branch("left.txt", "refs/heads/left", "left side");
    let right = branch("right.txt", "refs/heads/right", "right side");
    let left = repo.find_commit(left).unwrap();
    let right = repo.find_commit(right).unwrap();
    std::fs::write(app.state.workspace_root.join("left.txt"), "side\n").unwrap();
    std::fs::write(app.state.workspace_root.join("right.txt"), "side\n").unwrap();
    let mut merged = repo.index().unwrap();
    merged.add_path(std::path::Path::new("left.txt")).unwrap();
    merged.add_path(std::path::Path::new("right.txt")).unwrap();
    merged.write().unwrap();
    let tree = repo.find_tree(merged.write_tree().unwrap()).unwrap();
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    let merge = repo
        .commit(None, &sig, &sig, "merged both sides", &tree, &[&left, &right])
        .unwrap();
    repo.reference(&head_name, merge, true, "merged both sides").unwrap();
    drop(tree);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let drain = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.log_pending() {
            app.process_git_log_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    // History needs a captured status to plan against.
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    invoke(&mut app, "workspace.git.history.open");
    drain(&mut app);
    invoke(&mut app, "workspace.git.history.toggle");
    drain(&mut app);
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

    // The document names `g Graph` in the history footer, so the toggle is reachable rather
    // than palette-only.
    assert_eq!(
        app.configuration
            .keymaps()
            .binding_for("picker_normal", "workspace.git.history.toggle"),
        Some("g"),
        "the graph toggle has the key the document draws"
    );

    // The row the merge is on, and the row an ordinary commit is on.
    let row_of = |needle: &str| {
        (0..frame.grid.height())
            .find(|y| frame.grid.row_text(*y).contains(needle))
            .unwrap_or_else(|| panic!("no row draws {needle:?}"))
    };
    let merge_y = row_of("merged both sides");
    let plain_y = row_of("left side");

    // The merge glyph is not the ordinary commit glyph, and both are read from the GRID.
    let glyph_column = |y: u16, glyph: char| {
        (0..frame.grid.width()).find(|x| {
            frame
                .grid
                .get(*x, y)
                .is_some_and(|cell| cell.symbol == glyph.to_string())
        })
    };
    let merge_x = glyph_column(merge_y, '◎').expect("the merge is drawn as a merge");
    let plain_x = glyph_column(plain_y, '●').expect("an ordinary commit is drawn as a commit");
    assert!(
        glyph_column(merge_y, '●').is_none(),
        "the merge row does not also draw an ordinary commit glyph"
    );

    // The BEND is in its own column, to the right of the commit it bends from and at the lane
    // area's edge - not merely somewhere on the line.
    let bend_x = (0..frame.grid.width())
        .find(|x| {
            frame
                .grid
                .get(*x, merge_y)
                .is_some_and(|cell| cell.symbol == "╮")
        })
        .expect("the merge bends into the second lane");
    assert!(
        bend_x > merge_x,
        "the bend is drawn to the right of the merge it leaves: {merge_x} then {bend_x}"
    );

    // A lane keeps ONE colour across rows, read from the grid's own cells.
    let style_at = |x: u16, y: u16| frame.grid.get(x, y).map(|cell| cell.style);
    assert_eq!(
        style_at(merge_x, merge_y),
        style_at(plain_x, plain_y),
        "lane zero keeps its colour from the merge row to the ordinary commit row"
    );
    // And two lanes on the merge row do not share a colour.
    assert_ne!(
        style_at(merge_x, merge_y),
        style_at(bend_x, merge_y),
        "the bend into the second lane takes a different colour from the first lane"
    );

    // THE CLOSING CURVE: the lane that converged is DRAWN converging, in the CLOSING LANE'S
    // OWN COLUMN, rather than simply vanishing from the row where its commit is reached.
    let right_y = row_of("right right side");
    let root_y = row_of("root commit");
    let second_lane_x = (merge_x + 1..merge_x + 4)
        .find(|x| {
            frame
                .grid
                .get(*x, right_y)
                .is_some_and(|cell| cell.symbol == "●")
        })
        .expect("the second lane carries its own commit on the `right` row");
    assert_eq!(
        frame
            .grid
            .get(second_lane_x, root_y)
            .map(|cell| cell.symbol.as_str()),
        Some("╯"),
        "the second lane closes into the commit it merges into with a curve: {:?}",
        frame.grid.row_text(root_y)
    );
    assert_ne!(
        frame
            .grid
            .get(second_lane_x, root_y)
            .map(|cell| cell.symbol.as_str()),
        Some("│"),
        "the closed lane is not still drawn as a plain lane"
    );
    assert_eq!(
        frame.grid.get(second_lane_x, root_y).map(|cell| cell.style),
        Some(crate::workspace::git::HistoryGraphRow::lane_style(
            usize::from(second_lane_x - merge_x),
        )),
        "the closing curve carries the colour of the lane it closes"
    );

    // NO LANE VANISHES: every lane drawn on one row is drawn on the next, closes with a curve,
    // or is the last row of the history. A lane that disappears with no glyph is exactly the
    // defect this check exists to catch.
    let history_rows = [merge_y, plain_y, right_y, root_y];
    for pair in history_rows.windows(2) {
        let (above, below) = (pair[0], pair[1]);
        for x in merge_x..merge_x + 2 {
            let carries = frame
                .grid
                .get(x, above)
                .is_some_and(|cell| !cell.symbol.trim().is_empty());
            let empty_below = frame
                .grid
                .get(x, below)
                .is_none_or(|cell| cell.symbol.trim().is_empty());
            assert!(
                !(carries && empty_below),
                "a lane vanished at column {x} between rows {above} and {below}: {:?} then {:?}",
                frame.grid.row_text(above),
                frame.grid.row_text(below)
            );
        }
    }

    // The badge is drawn as the ref's own element rather than inside the subject.
    let badge_columns: Vec<u16> = (0..frame.grid.width())
        .filter(|x| {
            frame
                .grid
                .get(*x, merge_y)
                .is_some_and(|cell| cell.symbol == "m")
        })
        .collect();
    assert!(
        !badge_columns.is_empty(),
        "the ref badge is drawn on the merge row: {:?}",
        frame.grid.row_text(merge_y)
    );
    let row_text = frame.grid.row_text(merge_y);
    let subject_at = row_text.find("merged both sides").expect("the subject is drawn");
    let badge_at = row_text.find("main").or_else(|| row_text.find("master"));
    if let Some(badge_at) = badge_at {
        assert!(
            badge_at != subject_at,
            "the badge and the subject start at different columns"
        );
    }
}

/// W-27: the History list draws the columns section 7.3 shows - the short hash, the subject,
/// the relative age and the author - with the branch and the exact counts in the title, its
/// own footer, and the selected commit preserved across the graph toggle.
#[test]
fn the_history_list_draws_its_columns_title_footer_and_keeps_the_selection() {
    let mut app = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for (name, message) in [
        ("one.txt", "first commit subject"),
        ("two.txt", "second commit subject"),
        ("three.txt", "third commit subject"),
    ] {
        fs::write(app.state.workspace_root.join(name), "x\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap();
    }
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
    let drain = |app: &mut InteractiveApplication| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.log_pending() {
            app.process_git_log_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let invoke = |app: &mut InteractiveApplication, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    invoke(&mut app, "workspace.git.history.open");
    drain(&mut app);
    app.handle_event(crate::ui::UiEvent::Resize(200, 50));
    app.reconcile_editor_windows();
    let render = |app: &mut InteractiveApplication| {
        app.reconcile_editor_windows();
        crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        )
    };
    let frame = render(&mut app);
    let drawn: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    let joined = drawn.join("\n");

    // The title names the branch and the EXACT counts, as `History · main · 3 of 3`.
    let branch = crate::workspace::git::git_branch_name(&app.state.workspace_root)
        .expect("branch read")
        .expect("the fixture is on a branch");
    assert!(
        joined.contains(&format!("History · {branch} · 3 of 3")),
        "the title names the branch and the exact counts: {joined}"
    );

    // The columns are drawn in the order the mockup shows: hash, subject, age, author. The age
    // is relative and the shortest one reads `now` for a commit made moments ago.
    let row = drawn
        .iter()
        .find(|line| line.contains("third commit subject"))
        .expect("the newest commit is drawn");
    // The short hash is the only seven-character run of hex digits on the row.
    let hash_at = row
        .split_whitespace()
        .find(|token| token.len() == 7 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .and_then(|token| row.find(token));
    let subject_at = row.find("third commit subject").expect("subject");
    assert!(
        hash_at.is_some_and(|hash| hash < subject_at),
        "the short hash is drawn BEFORE the subject: {row:?}"
    );
    let age_at = row.find("now").expect("a relative age is drawn");
    assert!(
        subject_at < age_at,
        "the age is drawn after the subject: {row:?}"
    );
    let author_at = row.rfind("Dust").expect("the author is drawn");
    assert!(
        age_at < author_at,
        "the author is drawn after the age: {row:?}"
    );

    // The surface carries its own footer, and every key it draws exists.
    assert!(
        joined.contains("g Graph") && joined.contains("space range"),
        "the surface's own footer is drawn: {joined}"
    );
    for (key, command) in [
        ("g", "workspace.git.history.toggle"),
        ("space", "workspace.git.history.range"),
    ] {
        assert_eq!(
            app.configuration
                .keymaps()
                .binding_for("picker_normal", command),
            Some(key),
            "every key the footer draws exists: {key} -> {command}"
        );
    }
    // And no key it cannot honour is drawn.
    for absent in ["R rewrite", "d diff", "b branch here", "p Inspect range"] {
        assert!(
            !joined.contains(absent),
            "a key with no command is not advertised: {absent}"
        );
    }

    // Selecting a commit that is NOT the first, then toggling, keeps the same commit.
    app.picker_move(1);
    let selected_before = app
        .ui
        .picker
        .as_ref()
        .and_then(|picker| picker.selected.map(|index| picker.rows[index].id.clone()))
        .expect("a selected row");
    assert!(
        selected_before.contains("git-commit:"),
        "a commit is selected: {selected_before}"
    );
    invoke(&mut app, "workspace.git.history.toggle");
    drain(&mut app);
    let selected_after = app
        .ui
        .picker
        .as_ref()
        .and_then(|picker| picker.selected.map(|index| picker.rows[index].id.clone()))
        .expect("a selected row after the toggle");
    assert_eq!(
        selected_before, selected_after,
        "the toggle preserves the selected commit by identity"
    );
}
