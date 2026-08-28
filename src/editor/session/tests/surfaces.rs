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
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.keymaps = keymaps;
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
    app.terminal_width = 100;
    app.terminal_height = 30;
    app.reconcile_editor_windows();
    let dashboard = app
        .surface_windows
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
    app.ui.dashboard = false;
    app.ui.picker = Some(crate::ui::PickerUiState {
        surface: crate::ui::PickerSurfaceKind::Generic,
        title: "Search".into(),
        query: "needle".into(),
        rows: Vec::new(),
        selected: None,
        scroll: 0,
        loading: false,
    });
    app.reconcile_editor_windows();
    let picker = app.surface_windows.picker().expect("picker resources");
    let expected = crate::ui::picker_geometry(
        &app.configuration.settings,
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
    assert_eq!(app.surface_windows.dashboard(), Some(dashboard));
    assert_eq!(
        app.surface_windows.picker_for(PickerSurfaceKind::Generic),
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
    assert_eq!(app.surface_windows.dashboard(), Some(dashboard));
    assert!(
        app.compositor_projection()
            .expect("dashboard after last buffer")
            .window(dashboard.window)
            .is_some()
    );
}

#[test]
fn problems_logs_output_and_debug_keep_distinct_graph_surface_identities() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;

    app.open_builtin_picker(PickerKind::Diagnostics);
    app.reconcile_editor_windows();
    let problems = app
        .surface_windows
        .picker_for(PickerSurfaceKind::Problems)
        .expect("problems surface");
    assert_eq!(
        app.windows
            .content(problems.content)
            .expect("problems content")
            .content_type
            .as_str(),
        "builtin.problems"
    );

    app.show_output_picker("missing-task");
    app.reconcile_editor_windows();
    let output = app
        .surface_windows
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
        .surface_windows
        .picker_for(PickerSurfaceKind::Logs)
        .expect("logs surface");
    assert_eq!(
        app.windows
            .content(logs.content)
            .expect("logs content")
            .content_type
            .as_str(),
        "builtin.logs"
    );

    app.open_dap_picker("threads", Vec::new());
    app.reconcile_editor_windows();
    let debug = app
        .surface_windows
        .picker_for(PickerSurfaceKind::Debug)
        .expect("debug surface");
    assert_eq!(
        app.windows
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
    assert!(app.windows.window(debug.window).is_none());
    assert!(app.ui.picker.is_none());

    app.close_picker();
    app.reconcile_editor_windows();
    for ids in [problems, output, logs] {
        assert!(app.windows.window(ids.window).is_some());
        assert!(
            app.compositor_projection()
                .expect("hidden retained surfaces")
                .window(ids.window)
                .is_none()
        );
    }
    let reconciliation_count = app.editor_windows.reconciliation_count();
    let idle_graph = app.windows.clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.editor_windows.reconciliation_count(),
        reconciliation_count
    );
    assert_eq!(app.windows, idle_graph);
}

#[test]
fn protected_prompt_region_blocks_ordinary_compositor_hit_testing() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    app.reconcile_editor_windows();
    let geometry = crate::ui::prompt_geometry(
        app.terminal_width,
        app.workspace_render_height(),
        &app.configuration.settings.ui.command_line,
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
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    let view = app.state.active_view.expect("view");
    let workspace = app.workspace_bounds().expect("workspace");
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        &app.configuration.settings,
        workspace.width,
        app.workspace_render_height(),
    )
    .expect("editor viewport");
    let gutter = crate::ui::editor_state_gutter_width(
        &app.state,
        &app.state.buffers[app.state.views[view].buffer].text,
        &app.configuration.settings.editor,
        viewport.width,
    );
    app.mouse_position = Some((
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 2),
        workspace.y.saturating_add(viewport.y),
    ));
    app.apply_mouse_cursor(false);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    app.mouse_position = Some((
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 4),
        workspace.y.saturating_add(viewport.y),
    ));
    app.apply_mouse_cursor(true);
    let selection = app.state.views[view].selection.expect("mouse selection");
    assert_eq!(selection.anchor, crate::editor::text::ByteOffset(2));
    assert_eq!(selection.head, crate::editor::text::ByteOffset(4));
}

#[test]
fn nonwrapped_mouse_uses_the_same_snapped_tab_origin_as_rendering() {
    let mut app = app();
    app.terminal_width = 6;
    app.terminal_height = 5;
    app.configuration.settings.editor.wrap = false;
    app.configuration.settings.editor.line_numbers = crate::config::LineNumbers::Off;
    app.configuration.settings.ui.status_line.enabled = false;
    let (_, view) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("a\tb"), None);
    app.ui.dashboard = false;
    app.state.views[view].scroll_column = 2;
    app.reconcile_editor_windows();
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        &app.configuration.settings,
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    for cell in 0..3 {
        app.state.views[view].scroll_column = 2;
        app.mouse_position = Some((viewport.x + cell, viewport.y));
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
        &app.configuration.settings,
        app.terminal_width,
        app.workspace_render_height(),
    )
    .into_iter()
    .find(|viewport| viewport.view == first)
    .expect("generic first-window viewport");
    let workspace = app.workspace_bounds().expect("workspace");
    let gutter = crate::ui::editor_state_gutter_width(
        &app.state,
        &app.state.buffers[app.state.views[first].buffer].text,
        &app.configuration.settings.editor,
        viewport.width,
    );
    app.mouse_position = Some((
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 1),
        workspace.y.saturating_add(viewport.y),
    ));
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
    let ids = (buffer, app.state.active_view, app.windows.focused());
    app.process_effect(AppEffect::HealthVirtualRefresh);
    assert_eq!(
        (
            app.state.active_buffer_id().unwrap(),
            app.state.active_view,
            app.windows.focused()
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
        assert_eq!(app.windows.focused(), ids.2);
    }
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
    let buffer = app
        .plugin_virtual_editor
        .publish(&mut app.state, &snapshot)
        .expect("publish");
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    assert!(app.editor_windows.content_for(buffer).is_some());
    assert!(app.windows.focused().is_some());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "# Héllo\nvirtual"
    );
    let ids = (app.state.active_view, app.windows.focused());
    let mut refresh = snapshot.clone();
    refresh.revision = 2;
    refresh.text = "# Héllo\nrefreshed".into();
    assert_eq!(
        app.plugin_virtual_editor
            .publish(&mut app.state, &refresh)
            .unwrap(),
        buffer
    );
    app.reconcile_editor_windows();
    assert_eq!((app.state.active_view, app.windows.focused()), ids);
    assert!(matches!(
        app.windows
            .window(app.windows.focused().unwrap())
            .unwrap()
            .placement,
        WindowPlacement::Tiled
    ));
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
    let before = app.pending_vfs.len();
    app.start_save(
        false,
        Some(app.state.workspace_root.join("forbidden")),
        false,
    );
    app.start_reload(true);
    assert_eq!(app.pending_vfs.len(), before);
    app.state.buffers[buffer].dirty = true;
    app.configuration.settings.recovery.enabled = true;
    app.recovery_discovery_queued = true;
    app.configuration.settings.recovery.cleanup_enabled = false;
    app.schedule_recovery_work();
    assert!(!app.recovery_scheduled.contains_key(&buffer));
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
    app.ui.dashboard = false;
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
    app.ui.dashboard_actions = super::dashboard_actions(&app.configuration);
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
    app.configuration.settings.history.command.maximum_entries = 1;
    app.configuration.settings.history.search.maximum_entries = 2;
    app.record_prompt_history(crate::ui::PromptKind::Command, "first");
    app.record_prompt_history(crate::ui::PromptKind::Command, "second");
    app.record_prompt_history(crate::ui::PromptKind::SearchForward, "needle");
    assert_eq!(app.command_history, vec!["second"]);
    assert_eq!(app.search_history, vec!["needle"]);
    app.configuration.settings.history.search.enabled = false;
    app.record_prompt_history(crate::ui::PromptKind::SearchBackward, "ignored");
    assert_eq!(app.search_history, vec!["needle"]);
}

#[test]
fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
    let mut app = app();
    app.configuration.settings.recovery.write_debounce_ms = 10;
    app.recovery_discovery_queued = true;
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    app.ui.dashboard = false;
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    let first_revision = app.state.buffers[buffer].text.revision();
    app.invoke_insert_text("B".into());
    app.schedule_recovery_work();
    let latest_revision = app.state.buffers[buffer].text.revision();
    assert_ne!(first_revision, latest_revision);
    assert_eq!(app.recovery_scheduled.len(), 1);
    assert_eq!(app.recovery_scheduled[&buffer].0, latest_revision);
    assert!(
        !app.pending_vfs
            .values()
            .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
    );
    std::thread::sleep(Duration::from_millis(12));
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    let journal = app.recovery_path(buffer);
    let recovered = crate::files::discover_recovery(&app.recovery_directory);
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest_revision.0);

    app.state.buffers[buffer].mark_saved(latest_revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!journal.exists());
    assert!(!app.recovery_revisions.contains_key(&buffer));

    app.configuration.settings.recovery.write_debounce_ms = 0;
    app.invoke_insert_text("X".into());
    let revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    app.state.buffers[buffer].mark_saved(revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery_path(buffer).exists());
}

#[test]
fn recovery_delete_completion_cannot_forget_a_newer_queued_write() {
    let mut app = app();
    app.configuration.settings.recovery.write_debounce_ms = 0;
    app.configuration.settings.recovery.cleanup_enabled = false;
    app.recovery_discovery_queued = true;
    app.recovery_cleanup_queued = true;
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    app.ui.dashboard = false;
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    wait_vfs(&mut app);

    let saved_revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer].mark_saved(saved_revision);
    app.schedule_recovery_work();
    assert!(
        app.pending_vfs
            .values()
            .any(|pending| { matches!(pending, super::PendingVfs::RecoveryDelete { .. }) })
    );

    app.invoke_insert_text("B".into());
    let latest = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    assert_eq!(app.recovery_revisions.get(&buffer), Some(&latest));
    wait_vfs(&mut app);

    let recovered = crate::files::discover_recovery(&app.recovery_directory);
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest.0);
    assert_eq!(app.recovery_revisions.get(&buffer), Some(&latest));
    app.schedule_recovery_work();
    assert!(
        !app.pending_vfs
            .values()
            .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
    );
}

#[test]
fn retention_pruning_suppresses_only_the_pruned_live_revision_without_churn() {
    let mut app = app();
    app.configuration.settings.recovery.write_debounce_ms = 0;
    app.configuration.settings.recovery.maximum_retained_records = 0;
    app.configuration.settings.recovery.maximum_age_days = 0;
    app.recovery_discovery_queued = true;
    app.recovery_cleanup_queued = true;
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    let pruned_revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery_path(buffer).exists());
    assert_eq!(
        app.recovery_pruned_revisions.get(&buffer),
        Some(&pruned_revision)
    );
    app.schedule_recovery_work();
    assert!(
        !app.pending_vfs
            .values()
            .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
    );

    app.invoke_insert_text("B".into());
    let newer = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    assert_ne!(newer, pruned_revision);
    assert!(app.pending_vfs.values().any(|pending| {
        matches!(pending, super::PendingVfs::RecoveryWrite { revision, .. } if *revision == newer)
    }));
}

#[test]
fn disabled_recovery_policy_schedules_no_work() {
    let mut app = app();
    app.configuration.settings.recovery.enabled = false;
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None);
    app.invoke_insert_text("X".into());
    app.schedule_recovery_work();
    assert!(app.recovery_scheduled.is_empty());
    assert!(app.pending_vfs.is_empty());
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
    app.ui.dashboard = false;
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
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let target = app.editor_windows.content_for(first).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                &app.configuration.settings,
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y
        + u16::from(
            app.configuration.settings.ui.tab_line.enabled
                && (!app.configuration.settings.ui.tab_line.hide_when_single
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
                &app.configuration.settings,
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
                &app.configuration.settings,
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(!app.state.buffers.contains_key(first));
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
    app.ui.dashboard = false;
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
        app.content_line_pins
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
    let before = app.content_line_order.clone();
    app.move_active_buffer_line(-1);
    assert_eq!(app.content_line_order, before);
    let bounds = app.workspace_bounds().unwrap();
    let target = app.editor_windows.content_for(c).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                &app.configuration.settings,
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y
        + u16::from(
            app.configuration.settings.ui.tab_line.enabled
                && (!app.configuration.settings.ui.tab_line.hide_when_single
                    || app.state.tabs.len() > 1),
        );
    let middle = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseMiddleDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&middle, bounds.x + local, row));
    assert!(app.content_line_pins.contains(&c));
    let pins = app.content_line_pins.clone();
    let order = app.content_line_order.clone();
    app.editor_windows.reset(&mut app.windows).unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.content_line_pins, pins);
    assert_eq!(app.content_line_order, order);
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
    assert!(!app.content_line_pins.contains(&c) && !app.content_line_order.contains(&c));
}

#[test]
fn content_line_diagnostics_custom_entries_and_idle_projection_are_pure() {
    let mut app = app();
    let path = PathBuf::from("/workspace/diagnostic.rs");
    app.state
        .open_text(TextBuffer::from_text("bad"), Some(path.clone()));
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    app.ui.diagnostics = vec![crate::ui::UiDiagnostic {
        path: path.clone(),
        range: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
        severity: DiagnosticSeverity::Error,
        message: "bad".into(),
    }];
    app.content_line_diagnostic_revision += 1;
    app.content_line_projection_signature = None;
    let buffer_count = app.state.buffers.len();
    let custom = app
        .windows
        .create_content(
            crate::ui::window::ContentTypeId::new("plugin.demo-custom").unwrap(),
            crate::ui::window::ContentAdapterKind::DeclarativeUi,
            "Custom surface".into(),
            false,
            "plugin.demo",
            1,
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
    app.terminal_width = 300;
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                &app.configuration.settings,
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == custom)
        })
        .unwrap();
    let row = bounds.y
        + u16::from(
            app.configuration.settings.ui.tab_line.enabled
                && (!app.configuration.settings.ui.tab_line.hide_when_single
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
                .windows
                .content_snapshots()
                .iter()
                .any(|entry| entry.id == custom)
    );
    app.windows
        .remove_owner_generation("plugin.demo", 2)
        .unwrap();
    assert!(
        app.windows
            .content_snapshots()
            .iter()
            .any(|entry| entry.id == custom)
    );
    app.windows
        .remove_owner_generation("plugin.demo", 1)
        .unwrap();
    app.reconcile_editor_windows();
    assert!(
        !app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.identity.0 == custom)
    );
    let signature = app.content_line_projection_signature;
    let tabs = app.ui.content_tabs.clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
        let frame = render(
            &app.state,
            &app.ui,
            &app.configuration.settings,
            &app.configuration.dashboard,
            app.terminal_width,
            app.terminal_height,
        );
        assert_eq!(frame.grid.width(), app.terminal_width);
        assert_eq!(app.content_line_projection_signature, signature);
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
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    assert_eq!(app.windows.counts(), (3, 2, 2, 1));
    assert_eq!(app.ui.content_tabs.len(), 2);
    assert_ne!(app.ui.content_tabs[0].title, app.ui.content_tabs[1].title);
    assert!(app.ui.content_tabs[0].dirty);
    assert!(app.ui.content_tabs[1].active);
    assert_eq!(
        app.ui.content_tabs[1].identity,
        crate::ui::ContentTabIdentity(
            app.editor_windows
                .content_for(second)
                .expect("second content")
        )
    );

    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    assert_eq!(app.windows.counts(), (3, 3, 3, 1));
    let focused = app.windows.focused().expect("focused split");

    let second_workspace_tab = app.state.new_tab().expect("new workspace tab");
    app.reconcile_editor_windows();
    assert_eq!(app.windows.counts(), (3, 4, 4, 2));
    assert_eq!(
        app.windows.active_tab(),
        app.editor_windows.tab_for(second_workspace_tab)
    );
    assert_ne!(app.windows.focused(), Some(focused));

    app.state.close_active_tab().expect("close workspace tab");
    app.reconcile_editor_windows();
    assert_eq!(app.windows.counts(), (3, 3, 3, 1));
    assert_eq!(app.ui.content_tabs.len(), 2);
    app.windows.validate().expect("valid reconciled graph");
}

#[test]
fn editor_layout_commands_commit_state_bridge_and_graph_as_one_authoritative_transaction() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    let initial_revision = app.windows.revision().expect("graph revision");
    let initial_window = app.windows.focused().expect("initial window");

    app.process_effect(crate::editor::AppEffect::Layout(
        crate::editor::LayoutAction::Split(crate::editor::Axis::Horizontal),
    ));
    assert_eq!(app.state.panes.len(), 2);
    assert_eq!(
        app.windows
            .tiled_layout_snapshot(app.windows.active_tab().expect("tab"))
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
    assert!(app.windows.revision().expect("split revision") > initial_revision);
    assert!(app.windows.window(initial_window).is_some());

    let focused = app.windows.focused().expect("focused split");
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
    assert_eq!(app.windows.focused(), Some(focused));

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

    let before_resize = app.windows.clone();
    app.process_effect(crate::editor::AppEffect::WindowResize {
        target: format!("window:{}", focused.0),
        amount: 50,
    });
    assert_ne!(app.windows, before_resize);
    assert_eq!(app.state.panes.len(), 2);

    app.process_effect(crate::editor::AppEffect::WindowClose(format!(
        "window:{}",
        focused.0
    )));
    assert_eq!(app.state.panes.len(), 1);
    assert!(app.windows.window(initial_window).is_some());
    let state_revision_before_rejected = app.state.window_revision();
    let active_view_before_rejected = app.state.active_view;
    let pane_count_before_rejected = app.state.panes.len();
    let graph_before_rejected = app.windows.clone();
    app.process_effect(crate::editor::AppEffect::WindowClose("focused".into()));
    assert_eq!(app.state.window_revision(), state_revision_before_rejected);
    assert_eq!(app.state.active_view, active_view_before_rejected);
    assert_eq!(app.state.panes.len(), pane_count_before_rejected);
    assert_eq!(app.windows, graph_before_rejected);
    assert!(app.ui.notification_error);
}

#[test]
fn editor_window_swap_rejects_mixed_and_cross_tab_targets_atomically() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    app.open_explorer();
    let tab = app.windows.active_tab().expect("active tab");
    let editor_windows = app
        .windows
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
    let explorer = app.surface_windows.explorer().expect("explorer").window;
    let graph_before_mixed = app.windows.clone();
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", editor_windows[0].0),
        second: format!("window:{}", explorer.0),
    });
    assert_eq!(app.windows, graph_before_mixed);
    assert!(app.ui.notification_error);

    let old_editor = editor_windows[0];
    app.process_effect(AppEffect::Layout(crate::editor::LayoutAction::NewTab));
    let current_editor = app.windows.focused().expect("new tab editor");
    let graph_before_cross_tab = app.windows.clone();
    let revision_before_cross_tab = app.state.window_revision();
    app.process_effect(AppEffect::WindowSwap {
        first: format!("window:{}", old_editor.0),
        second: format!("window:{}", current_editor.0),
    });
    assert_eq!(app.windows, graph_before_cross_tab);
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
    app.ui.dashboard = false;
    app.reconcile_editor_windows();
    let initial_count = app.editor_windows.reconciliation_count();
    let initial_graph = app.windows.clone();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.editor_windows.reconciliation_count(), initial_count);
    assert_eq!(app.windows, initial_graph);

    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs[0].dirty);
    let after_dirty = app.editor_windows.reconciliation_count();

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
    assert_eq!(app.editor_windows.reconciliation_count(), after_dirty + 1);

    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    app.reconcile_editor_windows();
    let split_count = app.editor_windows.reconciliation_count();
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
    assert_eq!(app.editor_windows.reconciliation_count(), split_count + 2);

    let before_label = app.editor_windows.reconciliation_count();
    app.configuration.settings.ui.buffer_line.scratch_label = "[untitled]".into();
    app.reconcile_editor_windows();
    assert_eq!(app.editor_windows.reconciliation_count(), before_label + 1);
    app.windows
        .validate()
        .expect("revision-driven graph is valid");
}

#[test]
fn explorer_is_a_graph_dock_and_bundled_disable_is_transactional() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;
    app.open_explorer();
    let ids = app.surface_windows.explorer().expect("explorer resources");
    assert!(matches!(
        app.windows
            .window(ids.window)
            .expect("explorer window")
            .placement,
        WindowPlacement::Docked {
            edge: crate::ui::window::DockEdge::Left,
            ..
        }
    ));
    let rect = app.explorer_window_rect().expect("projected explorer");
    assert!(rect.width > 1 && rect.height == app.terminal_height);

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
        app.windows
            .window(ids.window)
            .expect("hidden explorer")
            .placement,
        WindowPlacement::Hidden
    );

    let enabled = crate::config::load_bundled_config();
    app.apply_configuration_candidate(enabled, false)
        .expect("re-enable bundled explorer");
    app.open_explorer();
    assert_eq!(app.surface_windows.explorer(), Some(ids));
    assert!(matches!(
        app.windows
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
    app.ui.dashboard = false;
    app.terminal_width = 100;
    app.terminal_height = 30;
    app.open_explorer();
    app.terminals.insert(
        PtyId(7),
        super::TerminalRuntime {
            profile: "shell".into(),
            root_revision: app.workspace.root_revision(),
            screen: PtyScreen::new(100, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
            mode: TerminalPanelMode::Input,
            normal_cursor: None,
            visual_anchor: None,
            visual_linewise: false,
            scroll_offset: 0,
        },
    );
    app.active_terminal = Some(PtyId(7));
    app.terminal_set_revision = app.terminal_set_revision.saturating_add(1);
    app.sync_terminal_panel();
    let explorer = app.surface_windows.explorer().expect("explorer");
    let terminal = app.surface_windows.terminal(7).expect("terminal");
    let original_explorer = app
        .windows
        .window(explorer.window)
        .expect("explorer")
        .placement;
    let original_terminal = app
        .windows
        .window(terminal.window)
        .expect("terminal")
        .placement;

    app.process_effect(crate::editor::AppEffect::WindowSwap {
        first: "explorer".into(),
        second: "terminal".into(),
    });
    assert_eq!(app.surface_windows.explorer(), Some(explorer));
    assert_eq!(app.surface_windows.terminal(7), Some(terminal));
    assert_eq!(
        app.windows
            .window(explorer.window)
            .expect("swapped")
            .placement,
        original_terminal
    );
    assert_eq!(
        app.windows
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
        .windows
        .window(terminal.window)
        .expect("terminal")
        .placement;
    app.process_effect(crate::editor::AppEffect::WindowFullscreen(
        "terminal".into(),
    ));
    assert_eq!(
        app.windows
            .window(explorer.window)
            .expect("fullscreen")
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(
        app.windows
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
        app.windows
            .window(explorer.window)
            .expect("hidden")
            .placement,
        WindowPlacement::Hidden
    );
    app.process_effect(crate::editor::AppEffect::WindowShow("explorer".into()));
    assert_eq!(
        app.windows
            .window(explorer.window)
            .expect("restored")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 31,
        }
    );

    let focus_target = if app.windows.focused() == Some(explorer.window) {
        terminal.window
    } else {
        explorer.window
    };
    app.windows
        .focus(focus_target)
        .expect("graph-only focus change");
    assert!(app.compositor_projection().is_none());
    let graph_stale = app.windows.clone();
    app.process_effect(crate::editor::AppEffect::WindowHide("terminal".into()));
    assert_eq!(app.windows, graph_stale);
    app.reconcile_editor_windows();
    assert!(app.compositor_projection().is_some());

    let before_stale = app.windows.clone();
    app.ui
        .compositor_layout
        .as_mut()
        .expect("compositor")
        .source_revision = app
        .state
        .window_revision()
        .expect("source revision")
        .saturating_add(1);
    app.process_effect(crate::editor::AppEffect::WindowHide("terminal".into()));
    assert_eq!(app.windows, before_stale);
    assert!(app.ui.notification_error);
    app.ui
        .compositor_layout
        .as_mut()
        .expect("compositor")
        .source_revision = app.state.window_revision().expect("source revision");
    let idle_graph = app.windows.clone();
    let idle_reconciliations = app.editor_windows.reconciliation_count();
    for _ in 0..128 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.windows, idle_graph);
    assert_eq!(
        app.editor_windows.reconciliation_count(),
        idle_reconciliations
    );

    app.process_effect(crate::editor::AppEffect::WindowClose("explorer".into()));
    assert!(app.windows.content(explorer.content).is_none());
    assert!(app.surface_windows.explorer().is_none());
    assert!(app.ui.explorer.is_none());
    assert!(app.terminals.contains_key(&PtyId(7)));
}

#[test]
fn semantic_window_layout_persistence_is_strict_transactional_and_id_free() {
    let mut source = app();
    source.state.open_text(
        TextBuffer::from_text("editor"),
        Some(PathBuf::from("/workspace/editor.rs")),
    );
    source.ui.dashboard = false;
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
    restored.ui.dashboard = false;
    restored.open_explorer();
    let ids = restored
        .surface_windows
        .explorer()
        .expect("fresh runtime ids");
    restored
        .apply_persisted_window_rules(&session.window_rules)
        .expect("apply semantic rules");
    assert_eq!(
        restored
            .windows
            .window(ids.window)
            .expect("restored")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 27,
        }
    );

    let before = restored.surface_windows.semantic_defaults(None);
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
    assert_eq!(restored.surface_windows.semantic_defaults(None), before);
    assert_eq!(restored.surface_windows.explorer(), Some(ids));

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
    let graph_before = restored.windows.clone();
    assert!(
        restored
            .apply_persisted_window_rules(&two_fullscreen)
            .is_err()
    );
    assert_eq!(restored.windows, graph_before);
    assert_eq!(restored.surface_windows.semantic_defaults(None), before);

    restored
        .surface_windows
        .set_configured_placements(BTreeMap::from([(
            "builtin.dashboard".into(),
            WindowPlacement::Fullscreen,
        )]))
        .expect("one configured fullscreen");
    let merged_conflict = [PersistedWindowRule {
        content_type: "builtin.explorer".into(),
        placement: PersistedWindowPlacement::Fullscreen,
    }];
    let graph_before_merged = restored.windows.clone();
    let defaults_before_merged = restored.surface_windows.semantic_defaults(None);
    assert!(
        restored
            .apply_persisted_window_rules(&merged_conflict)
            .is_err()
    );
    assert_eq!(restored.windows, graph_before_merged);
    assert_eq!(
        restored.surface_windows.semantic_defaults(None),
        defaults_before_merged
    );
}

#[test]
fn configured_window_rule_candidate_is_cloned_validated_and_published_atomically() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;
    app.open_explorer();
    let explorer = app.surface_windows.explorer().expect("explorer");
    let original = app.windows.clone();
    let rules = crate::config::WindowsConfig {
        rules: vec![crate::config::WindowRuleConfig {
            content_type: "builtin.explorer".into(),
            placement: crate::config::WindowPlacementConfig::Docked {
                edge: "right".into(),
                size: 31,
            },
        }],
    };
    let (graph, mut surfaces) = app
        .prepare_configured_window_rules(&rules)
        .expect("configured candidate");
    assert_eq!(app.windows, original);
    assert_eq!(
        graph
            .window(explorer.window)
            .expect("candidate explorer")
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 31,
        }
    );
    surfaces.invalidate();
    app.windows = graph;
    app.surface_windows = surfaces;
    app.reconcile_editor_windows();
    assert_eq!(
        app.windows
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
    let graph_before = app.windows.clone();
    let surfaces_before = app.surface_windows.clone();
    assert!(app.prepare_configured_window_rules(&hostile).is_err());
    assert_eq!(app.windows, graph_before);
    assert_eq!(app.surface_windows, surfaces_before);
}

#[test]
fn terminal_panel_focus_cycle_resize_and_close_preserve_independent_sessions() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.ui.dashboard = false;
    app.terminal_width = 80;
    app.terminal_height = 24;
    app.configuration.settings.ui.terminal_panel.height = 10;
    app.configuration.settings.ui.terminal_panel.minimum_height = 4;
    app.configuration.settings.ui.terminal_panel.maximum_height = 20;
    app.configuration.settings.ui.terminal_panel.resize_step = 2;
    app.terminal_panel_height = 10;
    let mut first_screen = PtyScreen::new(80, 8, 100);
    first_screen.feed("first terminal");
    let mut second_screen = PtyScreen::new(80, 8, 100);
    second_screen.feed("second terminal");
    app.terminals.insert(
        PtyId(1),
        super::TerminalRuntime {
            profile: "first".into(),
            root_revision: app.workspace.root_revision(),
            screen: first_screen,
            running: true,
            exit_code: None,
            truncated: false,
            mode: TerminalPanelMode::Input,
            normal_cursor: None,
            visual_anchor: None,
            visual_linewise: false,
            scroll_offset: 0,
        },
    );
    app.terminals.insert(
        PtyId(2),
        super::TerminalRuntime {
            profile: "second".into(),
            root_revision: app.workspace.root_revision(),
            screen: second_screen,
            running: false,
            exit_code: Some(7),
            truncated: true,
            mode: TerminalPanelMode::Input,
            normal_cursor: None,
            visual_anchor: None,
            visual_linewise: false,
            scroll_offset: 0,
        },
    );
    app.active_terminal = Some(PtyId(1));
    app.terminal_focused = true;
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
    assert!(!app.terminal_focused, "Ctrl-k returns to the editor above");
    app.handle_configured_key("ctrl-j".parse::<KeyEvent>().expect("Ctrl-j"));
    assert!(app.terminal_focused, "Ctrl-j returns to the terminal below");

    app.process_effect(crate::editor::AppEffect::TerminalUnfocus);
    assert_eq!(app.active_terminal, Some(PtyId(1)));
    assert!(!app.terminal_focused);
    assert!(app.ui.terminal_panel.is_some());
    assert_eq!(app.active_input_context(), "editor.normal");
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(app.terminal_focused);
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(!app.terminal_focused);
    assert_eq!(app.active_terminal, Some(PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::editor::AppEffect::TerminalToggle);
    assert!(app.terminal_focused);
    assert!(app.terminal_window_rect().is_some());
    assert_eq!(app.active_terminal, Some(PtyId(1)));
    app.process_effect(crate::editor::AppEffect::TerminalUnfocus);

    app.process_effect(crate::editor::AppEffect::TerminalFocus(2));
    assert_eq!(app.active_terminal, Some(PtyId(2)));
    assert!(app.terminal_focused);
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
    assert_eq!(app.active_terminal, Some(PtyId(1)));
    assert!(app.terminals[&PtyId(2)].screen.lines()[0].contains("second"));

    app.process_effect(crate::editor::AppEffect::TerminalResize(1));
    assert_eq!(app.terminal_panel_height, 12);
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (80, 10));
    app.process_effect(crate::editor::AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Up,
    ));
    assert_eq!(app.terminal_panel_height, 14);
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (80, 12));
    app.process_effect(crate::editor::AppEffect::WindowResizeDirectional(
        crate::editor::FocusDirection::Down,
    ));
    assert_eq!(app.terminal_panel_height, 12);
    assert_eq!(app.workspace_render_height(), 12);
    app.handle_event(crate::ui::UiEvent::Resize(60, 20));
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (60, 10));
    assert_eq!(app.workspace_render_height(), 8);

    let terminal_ids = app.surface_windows.terminal(1).expect("terminal resources");
    app.process_effect(crate::editor::AppEffect::TerminalFloat {
        x: 5,
        y: 3,
        width: 40,
        height: 10,
    });
    assert_eq!(app.surface_windows.terminal(1), Some(terminal_ids));
    assert_eq!(
        app.windows
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
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (40, 8));
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
        app.windows
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
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (42, 10));
    app.process_effect(crate::editor::AppEffect::TerminalFullscreen);
    assert_eq!(app.surface_windows.terminal(1), Some(terminal_ids));
    assert_eq!(
        app.windows
            .window(terminal_ids.window)
            .expect("fullscreen")
            .placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (60, 18));
    app.process_effect(crate::editor::AppEffect::TerminalHide);
    assert_eq!(app.surface_windows.terminal(1), Some(terminal_ids));
    assert!(app.terminals.contains_key(&PtyId(1)));
    assert!(app.terminal_window_rect().is_none());
    app.process_effect(crate::editor::AppEffect::WindowShow("terminal".into()));
    assert_eq!(app.surface_windows.terminal(1), Some(terminal_ids));
    assert_eq!(
        app.windows
            .window(terminal_ids.window)
            .expect("shown")
            .placement,
        WindowPlacement::Fullscreen
    );
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "top".into(),
        size: 5,
    });
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (60, 3));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "right".into(),
        size: 10,
    });
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (10, 18));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "left".into(),
        size: 8,
    });
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (8, 18));
    app.process_effect(crate::editor::AppEffect::TerminalDock {
        edge: "bottom".into(),
        size: 12,
    });
    assert_eq!(app.surface_windows.terminal(1), Some(terminal_ids));
    assert_eq!(app.terminals[&PtyId(1)].screen.dimensions(), (60, 10));
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

    app.process_effect(crate::editor::AppEffect::TerminalClose);
    assert!(!app.terminals.contains_key(&PtyId(1)));
    assert_eq!(app.active_terminal, Some(PtyId(2)));
    assert!(app.ui.terminal_panel.is_some());
    app.process_effect(crate::editor::AppEffect::TerminalClose);
    assert!(app.terminals.is_empty());
    assert!(app.surface_windows.terminal(1).is_none());
    assert!(app.surface_windows.terminal(2).is_none());
    assert!(app.windows.content(terminal_ids.content).is_none());
    assert!(app.active_terminal.is_none());
    assert!(!app.terminal_focused);
    assert!(app.ui.terminal_panel.is_none());
}
