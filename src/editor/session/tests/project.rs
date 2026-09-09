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
    app.close_prompt();
    assert!(!app.explorer.mutation_pending());
    app.confirm_explorer_delete("delete".into());
    assert!(target.exists());
    ready_delete(&mut app);
    let original = target.with_extension("original");
    fs::rename(&target, &original).unwrap();
    fs::write(&target, "replacement must survive").unwrap();
    app.prompt_insert("delete");
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
    app.prompt_insert("delete");
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
fn workspace_trust_requires_exact_confirmation_cancels_on_focus_loss_and_persists() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    app.notify_message("stale notification".into(), true);
    app.request_workspace_trust();
    assert!(app.ui.notification.is_none());
    assert!(!app.ui.notification_error);
    assert_eq!(
        app.ui.prompt.as_ref().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::WorkspaceTrust)
    );
    let prompt = app.ui.prompt.as_ref().expect("trust prompt");
    assert!(prompt.error.is_none());
    assert!(
        prompt
            .help
            .as_deref()
            .is_some_and(|help| help.contains("Allows project commands"))
    );
    app.prompt_insert("wrong");
    app.submit_prompt();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    assert!(app.input.search_history().is_empty() && app.input.command_history().is_empty());

    app.request_workspace_trust();
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );

    app.request_workspace_trust();
    let phrase = app
        .configuration
        .settings()
        .workspace
        .trust_confirmation_phrase
        .clone();
    app.prompt_insert(&phrase);
    app.submit_prompt();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    wait_trust(&mut app);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
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
    app.confirm_workspace_trust(
        app.configuration
            .settings()
            .workspace
            .trust_confirmation_phrase
            .clone(),
    );
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
    assert!(
        app.explorer
            .tree()
            .node(directory_id)
            .is_some_and(|node| node.expanded)
    );
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
    let pending = app.pending_vfs.len();
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
    assert_eq!(app.pending_vfs.len(), pending);
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
    while app.execution_validation.is_some() {
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
    assert!(app.execution_validation.is_none());
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
    assert_eq!(
        app.ui.picker.as_ref().map(|p| p.title.as_str()),
        Some("Git status")
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
            .mutate(false, true, snapshot.root_revision + 1)
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
                    && preview.lines.first().map(String::as_str) == Some("found")
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
            preview.title == "match.txt"
                && preview.highlight_line == Some(1_087)
                && preview
                    .highlight_line
                    .is_some_and(|line| line.saturating_sub(preview.first_line) <= 8)
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
    assert!(app.pending_vfs.is_empty());
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
