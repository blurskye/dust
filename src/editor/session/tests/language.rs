use super::*;
use crate::editor::text::PositionEncoding;
use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::ui::render::overlays::{
    LSP_PROGRESS_DONE, LSP_PROGRESS_DONE_TIMEOUT_MS, SPINNER_FRAMES, SPINNER_INTERVAL_MS,
};

#[test]
fn installed_language_server_autostarts_after_workspace_trust_is_exact() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    );
    app.set_dashboard_visibility(false);
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != crate::tools::ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "automatic-rust".into(),
            name: "Automatic Rust".into(),
            kind: crate::tools::ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: crate::tools::ToolStatus::Installed(false_executable),
    });

    app.sync_lsp_document();
    assert!(!app.language.project_pending());
    assert!(!app.ui.notification_error);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.sync_lsp_document();
    assert!(
        app.language.project_pending(),
        "trusting the workspace starts the server"
    );
    assert_eq!(app.workspace.trust(), WorkspaceTrust::Trusted);
}

#[test]
fn a_server_the_user_stopped_stays_stopped_until_asked_for() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    );
    app.set_dashboard_visibility(false);
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != crate::tools::ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "automatic-rust".into(),
            name: "Automatic Rust".into(),
            kind: crate::tools::ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: crate::tools::ToolStatus::Installed(false_executable),
    });
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();

    app.language.mark_stopped("automatic-rust");
    app.sync_lsp_document();
    assert!(
        !app.language.project_pending(),
        "a stopped server does not autostart"
    );

    app.language.clear_stopped("automatic-rust");
    app.sync_lsp_document();
    assert!(
        app.language.project_pending(),
        "starting it again allows autostart"
    );
}

#[test]
fn nested_marker_backed_project_gets_its_own_lsp_preflight_root() {
    let mut app = app();
    let nested = app.state.workspace_root.join("nested");
    fs::create_dir_all(nested.join("src")).unwrap();
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"nested\"]\n",
    )
    .unwrap();
    fs::write(
        nested.join("Cargo.toml"),
        "[package]\nname = \"nested\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source = nested.join("src/main.rs");
    fs::write(&source, "fn main() {}\n").unwrap();
    let source = fs::canonicalize(source).unwrap();
    let nested = fs::canonicalize(nested).unwrap();
    app.state
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source));
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "nested-rust".into(),
            name: "Nested Rust".into(),
            kind: ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: ToolStatus::Installed(false_executable),
    });
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.start_lsp(Some("nested-rust".into()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.language.project_pending() {
        app.process_lsp_project_root();
        assert!(
            Instant::now() < deadline,
            "project-root resolution timed out"
        );
        thread::yield_now();
    }
    let action = app
        .language
        .fixture_preflight_projects()
        .next()
        .expect("nested project must queue an LSP preflight");
    assert_eq!(action.1, &nested);
    let expected_key = crate::language::lsp::LspSessionKey {
        root_id: app.workspace.active_root().expect("root").0,
        project_root: nested.clone(),
        server_id: "nested-rust".into(),
    }
    .transport_key();
    assert_eq!(action.0, &expected_key);
    while app.pending_preflight_count() != 0 {
        app.process_execution_preflight();
        assert!(Instant::now() < deadline, "LSP startup admission timed out");
        thread::yield_now();
    }
    let registered = app
        .language
        .sessions()
        .get(&expected_key)
        .expect("actual transport-admitted session");
    assert_eq!(registered.root_path, nested);
    assert!(
        registered
            .documents
            .contains_key(&app.state.active_buffer_id().unwrap())
    );
    assert!(app.language.capabilities().is_none()); // `false` is not an initialized language server.
    // Synthetic Running publication over the actual admitted transport: idle
    // document sync must not start a fresh completion request every frame.
    app.language.running(
        &expected_key,
        crate::language::lsp::LspCapabilities::default(),
    );
    app.refresh_lsp_projection_owner();
    let generation = app.language.generation();
    app.present_lsp_result(
        &expected_key,
        generation,
        "textDocument/completion",
        "",
        crate::language::lsp::LspFeatureResult::Completion {
            incomplete: false,
            items: vec![crate::language::lsp::LspCompletionItem {
                label: "main".into(),
                filter_text: None,
                sort_text: None,
                detail: String::new(),
                insert_text: "main".into(),
                text_edit: None,
                additional_edits: Vec::new(),
                kind: None,
                snippet: false,
                documentation: None,
                resolve_item: None,
                resolved: false,
            }],
        },
    );
    for _ in 0..3 {
        app.sync_lsp_document();
    }
    assert_eq!(
        app.language.generation(),
        generation,
        "idle frames restarted completion"
    );
    assert!(app.ui.language_completion.is_some());
    let (buffer, document) = app.active_lsp_document().unwrap();
    let source = app.state.buffers[buffer].text.source_version();
    assert!(matches!(
        app.language
            .sync_owned("foreign", document.clone(), (buffer, source.clone())),
        Err(crate::language::lsp::LspError::NotRunning(_))
    ));
    assert!(!app.language.synced("foreign", buffer, &source));
    app.language.stop(&expected_key).unwrap();
    app.language.invalidate_document(&expected_key, buffer);
    assert!(matches!(
        app.language
            .sync_owned(&expected_key, document, (buffer, source.clone())),
        Err(crate::language::lsp::LspError::NotRunning(_))
    ));
    assert!(!app.language.synced(&expected_key, buffer, &source));
}

#[test]
fn nested_marker_backed_project_gets_its_own_dap_preflight_root() {
    let mut app = app();
    let nested = app.state.workspace_root.join("nested-debug");
    fs::create_dir_all(nested.join("src")).unwrap();
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"nested-debug\"]\n",
    )
    .unwrap();
    fs::write(
        nested.join("Cargo.toml"),
        "[package]\nname = \"nested-debug\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source = nested.join("src/main.rs");
    fs::write(&source, "fn main() {}\n").unwrap();
    let source = fs::canonicalize(source).unwrap();
    let nested = fs::canonicalize(nested).unwrap();
    app.state
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source.clone()));
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Dap);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "nested-rust-dap".into(),
            name: "Nested Rust DAP".into(),
            kind: ToolKind::Dap,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: ToolStatus::Installed(false_executable),
    });
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.start_dap(Some("nested-rust-dap".into()), Some(source.clone()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.debugger_session.debug_discovering() {
        app.process_dap_project_root();
        assert!(
            Instant::now() < deadline,
            "DAP project-root resolution timed out"
        );
        thread::yield_now();
    }
    let action = app
        .debugger_session
        .debug_preflight_paths()
        .expect("nested project must queue a DAP preflight");
    assert_eq!(action.0, &nested);
    assert_eq!(action.1, &source);
}

#[test]
fn untrusted_workspace_blocks_project_local_language_server_launch() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    );
    fs::create_dir_all(app.state.workspace_root.join("bin")).expect("server bin");
    fs::write(
        app.state.workspace_root.join("bin/server"),
        b"not executable",
    )
    .expect("project-local server");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            app.state.workspace_root.join("bin/server"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "project-local".into(),
            name: "Project local".into(),
            kind: crate::tools::ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: app
                .state
                .workspace_root
                .join("bin/server")
                .display()
                .to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: crate::tools::ToolStatus::Installed(app.state.workspace_root.join("bin/server")),
    });
    app.start_lsp(Some("project-local".into()));
    assert!(app.language.active().is_none());
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("trust"))
    );
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[package]\nname = \"project-local\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            app.state.workspace_root.join("bin/server"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    app.start_lsp(Some("project-local".into()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.language.project_pending() && Instant::now() < deadline {
        app.process_lsp_project_root();
        thread::sleep(Duration::from_millis(5));
    }
    while app.pending_preflight_count() != 0 && Instant::now() < deadline {
        app.process_execution_preflight();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        app.language.active().is_none()
            && app.ui.notification_error
            && app
                .ui
                .notification
                .as_deref()
                .is_some_and(|message| !message.is_empty())
    );
}

#[test]
fn generic_picker_filters_and_invokes_registry_commands() {
    let mut app = app();
    app.open_builtin_picker(crate::editor::PickerKind::Commands);
    assert!(
        app.ui
            .picker
            .as_ref()
            .is_some_and(|picker| !picker.rows.is_empty())
    );
    app.picker_insert("show health");
    let picker = app.ui.picker.as_ref().expect("picker");
    assert!(picker.rows.iter().any(|row| {
        format!("{} {}", row.label, row.detail)
            .to_lowercase()
            .contains("health")
    }));
    app.accept_picker();
    assert!(app.ui.picker.is_none());
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("Dust health"))
    );
}

#[test]
fn unopened_multifile_workspace_edit_previews_then_applies_only_to_editor_buffers() {
    let mut app = app();
    let first = app.state.workspace_root.join("first.rs");
    let second = app.state.workspace_root.join("second.rs");
    fs::write(&first, "abc").expect("first");
    fs::write(&second, "xyz").expect("second");
    let first = fs::canonicalize(first).expect("canonical first");
    let second = fs::canonicalize(second).expect("canonical second");
    let (first_buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_bytes(b"abc").expect("text"),
        Some(first.clone()),
    );
    let range = |start, end| crate::language::lsp::LspRange {
        start: crate::language::lsp::LspPosition {
            line: 0,
            character: start,
        },
        end: crate::language::lsp::LspPosition {
            line: 0,
            character: end,
        },
    };
    let edit = crate::language::lsp::LspWorkspaceEdit {
        documents: vec![
            crate::language::lsp::LspDocumentEdit {
                uri: crate::language::lsp::file_uri(&first),
                version: Some(0),
                edits: vec![crate::language::lsp::LspTextEdit {
                    range: range(1, 2),
                    new_text: "B".into(),
                }],
            },
            crate::language::lsp::LspDocumentEdit {
                uri: crate::language::lsp::file_uri(&second),
                version: None,
                edits: vec![crate::language::lsp::LspTextEdit {
                    range: range(1, 2),
                    new_text: "Y".into(),
                }],
            },
        ],
        resources: Vec::new(),
    };
    app.begin_workspace_proposal(
        "rename preview".into(),
        edit,
        None,
        crate::editor::text::PositionEncoding::Utf16,
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace_edits.proposal().is_none() && Instant::now() < deadline {
        app.process_vfs_results();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(app.workspace_edits.proposal().is_some());
    assert_eq!(app.state.buffers[first_buffer].text.to_string(), "abc");
    assert_eq!(fs::read_to_string(&second).expect("disk"), "xyz");
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[first_buffer].text.to_string(), "aBc");
    let second_buffer = app
        .state
        .buffers
        .iter()
        .find(|(_, buffer)| buffer.path.as_ref() == Some(&second))
        .map(|(buffer, _)| buffer)
        .expect("hidden second buffer");
    assert_eq!(app.state.buffers[second_buffer].text.to_string(), "xYz");
    assert!(app.state.buffers[first_buffer].dirty && app.state.buffers[second_buffer].dirty);
    assert_eq!(fs::read_to_string(&second).expect("disk remains"), "xyz");

    let target = first.with_file_name("renamed.rs");
    let rename = || crate::language::lsp::LspWorkspaceEdit {
        documents: Vec::new(),
        resources: vec![crate::language::lsp::LspResourceOperation::Rename {
            old_uri: crate::language::lsp::file_uri(&first),
            new_uri: crate::language::lsp::file_uri(&target),
            overwrite: false,
            ignore_if_exists: false,
        }],
    };
    app.begin_workspace_proposal(
        "cancelled resource".into(),
        rename(),
        None,
        PositionEncoding::Utf16,
    );
    app.reject_workspace_proposal();
    app.begin_workspace_proposal("resource".into(), rename(), None, PositionEncoding::Utf16);
    wait_vfs(&mut app);
    assert!(app.workspace_edits.proposal().is_some());
    app.accept_workspace_proposal();
    app.process_effect(AppEffect::WorkspaceResourceApply);
    wait_vfs(&mut app);
    assert!(
        first.exists() && !target.exists(),
        "dirty resource source must not reach disk apply"
    );
    assert_eq!(app.state.buffers[first_buffer].path.as_ref(), Some(&first));
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert!(!app.state.buffers[first_buffer].dirty);
    assert_eq!(fs::read_to_string(&first).unwrap(), "aBc");
    app.begin_workspace_proposal(
        "fresh resource".into(),
        rename(),
        None,
        PositionEncoding::Utf16,
    );
    wait_vfs(&mut app);
    assert!(app.workspace_edits.proposal().is_some());
    app.accept_workspace_proposal();
    app.apply_configuration_candidate(crate::config::load_bundled_config(), false)
        .unwrap();
    app.process_effect(AppEffect::WorkspaceResourceApply);
    wait_vfs(&mut app);
    assert!(
        first.exists() && !target.exists(),
        "configuration retirement must discard unsubmitted resources"
    );
    app.begin_workspace_proposal(
        "after configuration".into(),
        rename(),
        None,
        PositionEncoding::Utf16,
    );
    wait_vfs(&mut app);
    app.accept_workspace_proposal();
    app.process_effect(AppEffect::WorkspaceResourceApply);
    wait_vfs(&mut app);
    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "aBc");
    assert_eq!(app.state.buffers[first_buffer].path.as_ref(), Some(&target));
    assert!(!app.state.buffers[first_buffer].dirty);
}

#[test]
fn lsp_progress_hides_transport_keys_preserves_begin_titles_and_animates() {
    let mut app = app();
    let server = "root:1:rust-analyzer".to_owned();
    let token = "cargo-check".to_owned();
    app.update_lsp_progress(
        server.clone(),
        token.clone(),
        crate::language::lsp::LspProgressKind::Begin,
        "Building".into(),
        "workspace".into(),
        Some(0),
    );
    app.update_lsp_progress(
        server.clone(),
        token.clone(),
        crate::language::lsp::LspProgressKind::Report,
        String::new(),
        "ring (lib)".into(),
        Some(72),
    );
    let progress = app.ui.lsp_progress.first().expect("active progress");
    assert_eq!(progress.server, "rust-analyzer");
    assert_eq!(progress.title, "Building");
    assert_eq!(progress.message, "ring (lib)");
    assert_eq!(progress.percentage, Some(72));
    assert!(!progress.server.contains("root:1"));
    assert!(SPINNER_FRAMES.contains(&"⠇"));
    app.language.fixture_progress_epoch(
        Instant::now() - Duration::from_millis(SPINNER_INTERVAL_MS * 3 / 2),
    );
    app.update_lsp_progress_animation();
    assert_eq!(app.ui.lsp_progress_frame, 1);

    app.update_lsp_progress(
        server,
        token,
        crate::language::lsp::LspProgressKind::End,
        String::new(),
        "Finished".into(),
        None,
    );
    let timeout = LSP_PROGRESS_DONE_TIMEOUT_MS;
    assert_eq!(LSP_PROGRESS_DONE, "✓");
    let completed = app.ui.lsp_progress.first_mut().expect("completed progress");
    assert!(completed.done);
    assert_eq!(completed.message, "Finished");
    app.language.fixture_progress_completed(
        Instant::now() - Duration::from_millis(timeout.saturating_add(1)),
    );
    app.update_lsp_progress_animation();
    assert!(app.ui.lsp_progress.is_empty());
    assert!(app.ui.notification.is_none());
}

#[test]
fn lsp_completion_applies_through_one_validated_transaction() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn ma"),
        Some(path),
    );
    app.state.views[view].cursor = ByteOffset(5);
    app.set_dashboard_visibility(false);
    let owner = super::LspProjectionOwner {
        server: "server".into(),
        buffer,
        view,
        revision: app.state.buffers[buffer].text.revision(),
        text: app.state.buffers[buffer].text.source_version(),
        uri: crate::language::lsp::file_uri(app.state.buffers[buffer].path.as_ref().unwrap()),
        language: "rust".into(),
    };
    app.language.fixture_projection(Some(owner.clone()));
    app.language.fixture_completion_source(Some((7, owner)));

    app.language
        .fixture_capabilities(crate::language::lsp::LspCapabilities {
            position_encoding: crate::editor::text::PositionEncoding::Utf16,
            ..Default::default()
        });
    app.language
        .fixture_completion_items(vec![crate::language::lsp::LspCompletionItem {
            label: "main".into(),
            filter_text: None,
            sort_text: None,
            detail: "function".into(),
            insert_text: "in".into(),
            text_edit: None,
            additional_edits: Vec::new(),
            kind: None,
            snippet: false,
            documentation: None,
            resolve_item: None,
            resolved: false,
        }]);
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        typed: 0,
        items: vec![crate::ui::CompletionItem {
            icon: None,
            color: None,
            label: "main".into(),
            detail: "function".into(),
        }],
        selected: 0,
        scroll: 0,
    });
    app.accept_lsp_completion();
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "fn main");
    assert!(app.ui.language_completion.is_none());
    assert!(app.state.buffers[buffer].dirty);
}

#[test]
fn lsp_completion_selection_scrolls_and_wraps_inside_the_visible_window() {
    let mut app = app();
    let count = usize::from(COMPLETION_MAX_ROWS) + 2;
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        typed: 0,
        items: (0..count)
            .map(|index| crate::ui::CompletionItem {
                icon: None,
                color: None,
                label: format!("item-{index}"),
                detail: String::new(),
            })
            .collect(),
        selected: 0,
        scroll: 0,
    });
    app.language.fixture_completion_items(
        (0..count)
            .map(|index| crate::language::lsp::LspCompletionItem {
                label: format!("item-{index}"),
                filter_text: None,
                sort_text: None,
                detail: String::new(),
                insert_text: String::new(),
                text_edit: None,
                additional_edits: Vec::new(),
                kind: None,
                snippet: false,
                documentation: None,
                resolve_item: None,
                resolved: false,
            })
            .collect(),
    );
    for _ in 1..count {
        app.move_lsp_completion(1);
    }
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (count - 1, 2));
    app.move_lsp_completion(1);
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (0, 0));
    app.move_lsp_completion(-1);
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (count - 1, 2));
}

#[test]
fn stale_lsp_workspace_edit_is_rejected_without_mutation() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("abc"),
        Some(path.clone()),
    );
    let edit = crate::language::lsp::LspWorkspaceEdit {
        documents: vec![crate::language::lsp::LspDocumentEdit {
            uri: crate::language::lsp::file_uri(&path),
            version: Some(99),
            edits: vec![crate::language::lsp::LspTextEdit {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 1,
                    },
                },
                new_text: "z".into(),
            }],
        }],
        resources: Vec::new(),
    };
    assert!(
        app.apply_lsp_workspace_edit(edit, crate::editor::text::PositionEncoding::Utf16)
            .is_err()
    );
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
}

#[test]
fn rust_analyzer_toolchain_and_standalone_versions_are_not_conflated() {
    assert_eq!(
        InteractiveApplication::rust_analyzer_path_version("1.96.1 (31fca3a 2026-08-21)", true,),
        ("rustup bundled".into(), "2026-08-21".into())
    );
    assert_eq!(
        InteractiveApplication::rust_analyzer_path_version("0.3.3025-standalone", false),
        ("0.3.3025".into(), "standalone build".into())
    );
}

#[cfg(unix)]
#[test]
fn rust_analyzer_rustup_proxy_detection_accepts_symlinks_and_hardlinks() {
    let directory = tempdir().expect("temporary proxy directory");
    let rustup = directory.path().join("rustup");
    fs::write(&rustup, b"rustup").expect("rustup fixture");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o700)).unwrap();
    let hardlink = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &hardlink).expect("rustup hardlink proxy");
    assert!(InteractiveApplication::rust_analyzer_is_rustup_proxy(
        &hardlink
    ));
    fs::remove_file(&hardlink).expect("remove hardlink proxy");
    std::os::unix::fs::symlink(&rustup, &hardlink).expect("rustup symlink proxy");
    assert!(InteractiveApplication::rust_analyzer_is_rustup_proxy(
        &hardlink
    ));
}

#[cfg(unix)]
#[test]
fn finished_tool_install_progress_cannot_restore_the_installing_state() {
    use std::os::unix::fs::PermissionsExt;

    let mut app = app();
    let directory = tempdir().expect("temporary rustup directory");
    let rustup = directory.path().join("rustup");
    fs::write(&rustup, "#!/bin/sh\nexit 0\n").expect("rustup fixture");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("rustup fixture permissions");
    let proxy = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).expect("rustup proxy");
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .definition
        .clone();
    app.tooling
        .install(crate::editor::session::tooling::ToolInstall::Rustup {
            definition,
            rustup,
            proxy,
            cwd: directory.path().to_path_buf(),
        })
        .expect("queue rustup install");

    // Consume completion before polling the worker's separate progress queue.
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Some((_, result)) = app.tooling.poll_install() {
            break result;
        }
        assert!(Instant::now() < deadline, "rustup install timed out");
        thread::yield_now();
    };
    assert!(result.result.is_ok(), "{:?}", result.result);
    let _ = app.tooling.poll_progress();
    let entry = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("completed tool entry");
    assert!(
        !matches!(entry.status, crate::tools::ToolStatus::Installing { .. }),
        "queued progress must not overwrite a completed install: {:?}",
        entry.status
    );
}

#[cfg(unix)]
#[test]
fn failed_rustup_proxy_probe_is_not_a_launchable_path_runtime() {
    let mut app = app();
    let directory = tempdir().expect("temporary proxy directory");
    let rustup = directory.path().join("rustup");
    fs::write(&rustup, b"rustup").expect("rustup fixture");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o700)).unwrap();
    let proxy = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).expect("rustup proxy");
    let mut definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .definition
        .clone();
    definition.executable = proxy.display().to_string();
    assert!(app.selected_lsp_runtime_available(&definition));
    let key = app.tool_version_key(&definition, LspRuntimeSource::Path, Some(&proxy));
    app.tooling
        .fixture_version(key, Err("active toolchain has no rust-analyzer".into()));
    assert!(!app.selected_lsp_runtime_available(&definition));
    let path_runtime = app
        .runtime_options_for(&definition)
        .into_iter()
        .find(|option| option.source == LspRuntimeSource::Path.key())
        .expect("PATH runtime");
    assert!(!path_runtime.installed);
    assert!(path_runtime.installable);
    assert_eq!(path_runtime.status, "not working");
    assert!(path_runtime.note.contains("i installs"));
    let message = InteractiveApplication::unavailable_rust_analyzer_message();
    assert!(message.contains("Press i in :lsp"));
    assert!(!message.contains("version probe exited"));
    app.tooling
        .check_probe_worker_replacement(&definition, proxy);
}

#[test]
fn typing_narrows_the_completion_menu_instead_of_closing_it() {
    use crate::editor::text::{ByteOffset, ByteRange, TextEdit, TextEditSet};
    let mut app = app();
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .definition
        .clone();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("console.l"),
        Some(path),
    );
    app.state.views[view].cursor = ByteOffset(9);
    let (_, document) = app.active_lsp_document().expect("document");
    let key = "typing".to_owned();
    app.language.fixture_session(
        key.clone(),
        crate::editor::session::dispatch::LspRuntimeState {
            root: app.workspace.active_root().expect("workspace root"),
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::editor::session::dispatch::LspLaunch::Direct {
                executable: crate::tools::discover_executable("false").expect("false executable"),
                managed: false,
            },
            capabilities: None,
            documents: HashMap::from([(buffer, document)]),
            restart_count: 0,
            restart_due: None,
            circuit_open: false,
            desired_running: true,
            activate_on_restart: true,
        },
    );
    app.language.select(&key);
    app.refresh_lsp_projection_owner();
    app.mark_lsp_completion_start(false);
    let item = |label: &str| crate::language::lsp::LspCompletionItem {
        label: label.into(),
        filter_text: None,
        sort_text: None,
        detail: String::new(),
        insert_text: label.into(),
        text_edit: None,
        additional_edits: Vec::new(),
        kind: None,
        snippet: false,
        documentation: None,
        resolve_item: None,
        resolved: true,
    };
    let generation = app.language.generation();
    app.language
        .install_completions(&key, vec![item("log"), item("list")], false, generation);
    app.narrow_lsp_completion();
    assert_eq!(
        app.ui
            .language_completion
            .as_ref()
            .map(|menu| menu.items.len()),
        Some(2)
    );

    let revision = app.state.buffers[buffer].text.revision();
    let at_end = ByteRange::new(ByteOffset(9), ByteOffset(9)).expect("range");
    app.state.buffers[buffer]
        .text
        .apply_edits(&TextEditSet::new(
            revision,
            vec![TextEdit::new(at_end, "o")],
        ))
        .expect("edit");
    app.state.views[view].cursor = ByteOffset(10);
    app.refresh_lsp_projection_owner();
    app.narrow_lsp_completion();

    let labels = app.ui.language_completion.as_ref().map(|menu| {
        menu.items
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(labels, Some(vec!["log".to_owned()]));
}

#[test]
fn lsp_status_never_labels_failed_or_backing_off_sessions_as_starting() {
    let mut app = app();
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .definition
        .clone();
    let executable = crate::tools::discover_executable("false").expect("false executable");
    let root = app.workspace.active_root().expect("workspace root");
    let key = "failed-rust-analyzer".to_owned();
    app.language.fixture_session(
        key.clone(),
        crate::editor::session::dispatch::LspRuntimeState {
            root,
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::editor::session::dispatch::LspLaunch::Direct {
                executable,
                managed: false,
            },
            capabilities: None,
            documents: HashMap::new(),
            restart_count: 1,
            restart_due: None,
            circuit_open: true,
            desired_running: true,
            activate_on_restart: true,
        },
    );
    app.language.select(&key);
    app.sync_lsp_status_ui();
    assert_eq!(app.ui.lsp_server.as_deref(), Some("failed rust-analyzer"));
    app.language
        .request_start(&key, Some(Instant::now() + Duration::from_secs(1)));
    app.sync_lsp_status_ui();
    assert_eq!(app.ui.lsp_server.as_deref(), Some("backoff rust-analyzer"));
    assert!(!app.language.select("foreign-session"));
    assert_eq!(app.language.active(), Some(&key));
    app.language.running(&key, Default::default());
    assert!(app.language.capabilities().is_some());
    assert!(app.language.stopped(&key));
    assert!(app.language.capabilities().is_none());
    let now = Instant::now();
    app.language.request_start(&key, None);
    let mut due = now;
    for attempt in 0..crate::editor::session::lsp::LSP_RESTART_LIMIT {
        assert!(app.language.schedule_restart(&key, due).is_none());
        assert!(app.language.restart_due(due).is_none());
        due +=
            Duration::from_millis(crate::editor::session::lsp::LSP_RESTART_BACKOFF_MS << attempt);
        assert_eq!(app.language.restart_due(due), Some(key.clone()));
    }
    assert_eq!(
        app.language.schedule_restart(&key, due),
        Some("rust-analyzer".into())
    );
    app.language.request_stop(&key);
    assert!(
        app.language
            .restart_due(now + Duration::from_secs(60))
            .is_none()
    );
}

#[test]
fn lsp_installation_mechanisms_are_host_specific() {
    assert!(lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Path,
        true
    ));
    assert!(lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Path,
        false
    ));
    assert!(!lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Managed,
        true
    ));
    assert!(lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Managed,
        false
    ));
    assert!(lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Nix,
        true
    ));
    assert!(!lsp_runtime_source_allowed_on_host(
        LspRuntimeSource::Nix,
        false
    ));
}

#[test]
fn lsp_tool_menu_reports_real_path_version_executable_and_lifecycle() {
    let mut app = app();
    let source = app.state.workspace_root.join("main.rs");
    app.state
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source));
    app.set_dashboard_visibility(false);
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let echo = crate::tools::discover_executable("echo").expect("echo executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "observed-rust".into(),
            name: "Observed Rust".into(),
            kind: ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: echo.display().to_string(),
            version: Some("managed-9.9.9".into()),
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: vec!["path-1.2.3".into()],
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: ToolStatus::Installed(echo.clone()),
    });

    app.open_tool_menu("lsp".into());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.process_tool_version_results();
        let row = &app.ui.tool_menu.as_ref().expect("menu").rows[0];
        if row.version != "probing…" {
            assert_eq!(row.version, "path-1.2.3");
            assert_eq!(row.runtime, "PATH");
            assert_eq!(row.executable, echo.display().to_string());
            assert_eq!(row.lifecycle, "installed");
            assert!(row.note.contains("Found on PATH"));
            break;
        }
        assert!(Instant::now() < deadline, "version probe timed out");
        thread::yield_now();
    }
}

#[test]
fn installed_path_server_without_a_qualified_probe_never_says_not_probed() {
    let mut app = app();
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let echo = crate::tools::discover_executable("echo").expect("echo executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "external-no-version-command".into(),
            name: "External Server".into(),
            kind: ToolKind::Lsp,
            languages: vec!["external-test".into()],
            executable: echo.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["external.test".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Marker,
            install: None,
            pins: Vec::new(),
        },
        status: ToolStatus::Installed(echo),
    });

    app.open_tool_menu("lsp".into());
    let row = &app.ui.tool_menu.as_ref().expect("menu").rows[0];
    assert_eq!(row.version, "—");
    assert_eq!(row.release, "—");
    assert_eq!(row.lifecycle, "installed");
    assert!(row.note.contains("Found on PATH"));
}

#[test]
fn lsp_tool_menu_hides_missing_noninstallable_compatibility_entries() {
    let mut app = app();
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "missing-catalog-server".into(),
            name: "Missing Catalog Server".into(),
            kind: ToolKind::Lsp,
            languages: vec!["catalog-test".into()],
            executable: "dust-certainly-missing-language-server".into(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["catalog.test".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Marker,
            install: None,
            pins: Vec::new(),
        },
        status: ToolStatus::Available,
    });

    app.open_tool_menu("lsp".into());
    assert!(app.ui.tool_menu.as_ref().expect("menu").rows.is_empty());
}

#[test]
fn enter_installs_a_missing_managed_lsp_instead_of_trying_to_start_it() {
    let mut app = app();
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "installable-lsp".into(),
            name: "Installable LSP".into(),
            kind: ToolKind::Lsp,
            languages: vec!["install-test".into()],
            executable: "dust-certainly-missing-installable-lsp".into(),
            version: Some("1.0.0".into()),
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["install.test".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Marker,
            install: Some(crate::tools::ToolInstall::NpmPackage {
                packages: vec!["example-lsp@1.0.0".into()],
            }),
            pins: Vec::new(),
        },
        status: ToolStatus::Available,
    });

    app.open_tool_menu("lsp".into());
    let row = &app.ui.tool_menu.as_ref().expect("menu").rows[0];
    assert_eq!(row.lifecycle, "not installed");
    assert!(row.can_install);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.activate_selected_tool();
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.starts_with("installing Installable LSP")),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn tool_menu_navigation_keeps_the_selected_server_inside_the_rendered_list() {
    let mut app = app();
    app.open_tool_menu("lsp".into());
    let row_count = app.ui.tool_menu.as_ref().expect("menu").rows.len();
    for _ in 0..row_count {
        app.move_tool_menu(1);
        let menu = app.ui.tool_menu.as_ref().expect("menu");
        let modal_height = (u32::from(app.window_session.height())
            * u32::from(crate::ui::render::surfaces::TOOL_MANAGER_HEIGHT_PERCENT)
            / 100) as u16;
        let modal_height = modal_height
            .max(
                crate::ui::render::surfaces::TOOL_MANAGER_MINIMUM_HEIGHT
                    .min(app.window_session.height()),
            )
            .min(app.window_session.height());
        let visible = usize::from(modal_height.saturating_sub(18)).max(1);
        assert!(menu.selected >= menu.scroll);
        assert!(menu.selected < menu.scroll.saturating_add(visible));
    }
}

#[test]
fn lsp_tool_menu_is_registry_driven_filterable_and_configurable() {
    let mut app = app();
    app.process_effect(crate::editor::AppEffect::OpenToolMenu("lsp".into()));
    let menu = app.ui.tool_menu.as_ref().expect("tool menu");
    assert_eq!(menu.category, "lsp");
    assert!(menu.rows.iter().all(|row| row.kind == "LSP"));
    assert!(menu.rows.iter().any(|row| row.id == "rust-analyzer"));
    let rust = menu
        .rows
        .iter()
        .position(|row| row.id == "rust-analyzer")
        .expect("Rust analyzer row");
    app.ui.tool_menu.as_mut().expect("tool menu").selected = rust;
    app.handle_configured_key(KeyEvent::character('p'));
    let picker = app
        .ui
        .tool_menu
        .as_ref()
        .and_then(|menu| menu.runtime_picker.as_ref())
        .expect("runtime picker");
    assert_eq!(picker.options.first().unwrap().source, "path");
    if is_nixos_host() {
        assert!(
            picker
                .options
                .iter()
                .all(|option| option.source != "managed")
        );
        assert_eq!(
            picker
                .options
                .iter()
                .filter(|option| option.source == "nix")
                .filter_map(|option| option.version_pin.as_deref())
                .collect::<Vec<_>>(),
            vec!["0.3.3025", "0.3.3016", "0.3.3008", "0.3.2997", "0.3.2989"]
        );
        assert_eq!(
            picker
                .options
                .iter()
                .filter(|option| option.source == "nix")
                .map(|option| option.release.as_str())
                .collect::<Vec<_>>(),
            vec![
                "2026-08-24",
                "2026-08-17.4",
                "2026-08-10.1",
                "2026-08-03",
                "2026-07-27",
            ]
        );
        app.ui
            .tool_menu
            .as_mut()
            .unwrap()
            .runtime_picker
            .as_mut()
            .unwrap()
            .selected = 1;
        app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Enter));
        assert!(
            app.tooling
                .metadata()
                .get("lsp.runtime.rust-analyzer")
                .is_some_and(|s| s == "nix")
        );
        assert_eq!(
            app.tooling
                .managed_version("rust-analyzer")
                .map(String::as_str),
            Some("0.3.3025")
        );
        assert_eq!(
            app.ui.tool_menu.as_ref().expect("tool menu").rows[rust].runtime,
            "Dust (Nix)"
        );
    } else {
        assert!(picker.options.iter().all(|option| option.source != "nix"));
        assert_eq!(
            picker
                .options
                .iter()
                .filter(|option| option.source == "managed")
                .filter_map(|option| option.version_pin.as_deref())
                .collect::<Vec<_>>(),
            vec!["0.3.3025", "0.3.3016", "0.3.3008", "0.3.2997", "0.3.2989",]
        );
        assert_eq!(
            picker
                .options
                .iter()
                .filter(|option| option.source == "managed")
                .map(|option| option.release.as_str())
                .collect::<Vec<_>>(),
            vec![
                "2026-08-24",
                "2026-08-17.4",
                "2026-08-10.1",
                "2026-08-03",
                "2026-07-27",
            ]
        );
        let older = picker
            .options
            .iter()
            .position(|option| option.version_pin.as_deref() == Some("0.3.3008"))
            .expect("older exact pin");
        app.ui
            .tool_menu
            .as_mut()
            .unwrap()
            .runtime_picker
            .as_mut()
            .unwrap()
            .selected = older;
        app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Enter));
        assert!(
            app.tooling
                .metadata()
                .get("lsp.runtime.rust-analyzer")
                .is_some_and(|s| s == "managed")
        );
        assert_eq!(
            app.tooling
                .managed_version("rust-analyzer")
                .map(String::as_str),
            Some("0.3.3008")
        );
        assert_eq!(
            app.ui.tool_menu.as_ref().expect("tool menu").rows[rust].runtime,
            "Dust"
        );
    }
    app.handle_configured_key(KeyEvent::character('j'));
    assert_eq!(
        app.ui.tool_menu.as_ref().expect("menu").selected,
        rust.saturating_add(1)
    );
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Tab));
    assert_eq!(app.ui.tool_menu.as_ref().expect("menu").category, "dap");
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert!(app.ui.tool_menu.is_none());
}

#[test]
fn plugin_workspace_preview_uses_invocation_source_and_retires_on_dismissal() {
    use crate::extensions::{ExecutionFixture, PluginEdit, PluginEditProposal};
    use slotmap::Key;
    let root = tempfile::tempdir().unwrap();
    let saved = tempfile::tempdir().unwrap();
    let mut app = InteractiveApplication::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_bundled_config(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    let path = root.path().join("source.txt");
    fs::write(&path, "éab").unwrap();
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("éab"),
        Some(path.clone()),
    );
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let (manager, endpoint) = ExecutionFixture::new(2);
    app.extension.fixture_manager(manager);
    let execute = AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    };
    let proposed = |revision| {
        vec![PluginEditProposal {
            buffer: buffer.data().as_ffi(),
            revision,
            edits: vec![PluginEdit {
                start: 2,
                end: 3,
                replacement: "X".into(),
            }],
        }]
    };
    app.process_effect(execute.clone());
    let captured = endpoint.take();
    endpoint.reply(&captured, proposed(0));
    app.process_plugin_events();
    assert!(app.workspace_edits.proposal().is_some());
    assert_eq!(app.active_input_context(), "workspace_edit");
    assert!(
        app.ui
            .picker
            .as_ref()
            .unwrap()
            .input_hint
            .starts_with("Review")
    );
    for key in ["i", "j", "k"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    app.process_effect(AppEffect::PickerInsertMode);
    assert_eq!(app.active_input_context(), "workspace_edit");
    assert_eq!(app.ui.picker.as_ref().unwrap().query, "");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "éab");
    // The picker API toggles without dismissing the source-bound preview;
    // the review keymap retains its explicit apply/reject commands.
    app.accept_picker();
    assert!(!app.workspace_edits.proposal().unwrap().files[0].selected);
    app.accept_picker();
    assert!(app.workspace_edits.proposal().unwrap().files[0].selected);
    app.process_effect(AppEffect::PickerCancel);
    assert!(app.workspace_edits.proposal().is_none());
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "éab");
    endpoint.reply(&captured, proposed(0));
    app.process_plugin_events();
    assert!(
        app.workspace_edits.proposal().is_none(),
        "duplicate cannot resurrect closed preview"
    );
    app.process_effect(execute.clone());
    let configured = endpoint.take();
    endpoint.reply(&configured, proposed(0));
    app.process_plugin_events();
    app.apply_configuration_candidate(crate::config::load_bundled_config(), false)
        .unwrap();
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "éab");
    app.process_effect(execute.clone());
    let fresh = endpoint.take();
    endpoint.reply(&fresh, proposed(0));
    app.process_plugin_events();
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "éXb");
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "éab",
        "approval edits buffers, not disk"
    );
    app.process_effect(execute.clone());
    let replaced = endpoint.take();
    endpoint.reply(
        &replaced,
        proposed(app.state.buffers[buffer].text.revision().0),
    );
    app.process_plugin_events();
    app.install_picker("other".into(), Vec::new());
    assert!(
        app.workspace_edits.proposal().is_none(),
        "another picker retires the old preview source"
    );
    app.process_effect(execute);
    let late = endpoint.take();
    app.extension.disable("demo").unwrap();
    endpoint.reply(&late, proposed(app.state.buffers[buffer].text.revision().0));
    app.process_plugin_events();
    assert!(app.workspace_edits.proposal().is_none());
    app.extension.close_edits();
}

#[test]
fn slash_searches_the_tool_manager_by_name_and_language() {
    let mut app = app();
    app.open_tool_menu("all".into());
    app.handle_configured_key(KeyEvent::character('/'));
    assert_eq!(app.active_input_context(), "tool_manager_search");
    for key in "tailwind".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let menu = app.ui.tool_menu.as_ref().expect("menu");
    assert_eq!(menu.query, "tailwind");
    assert!(!menu.rows.is_empty());
    assert!(
        menu.rows
            .iter()
            .all(|row| row.id.contains("tailwind") || row.name.to_lowercase().contains("tailwind"))
    );
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Enter));
    assert_eq!(app.active_input_context(), "tool_manager");
    // A search reaches servers outside the default list, as Mason does.
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert!(
        app.ui
            .tool_menu
            .as_ref()
            .is_some_and(|menu| menu.query.is_empty())
    );
    app.handle_configured_key(KeyEvent::character('/'));
    for key in "zls".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert!(
        app.ui
            .tool_menu
            .as_ref()
            .expect("menu")
            .rows
            .iter()
            .any(|row| row.id == "zls")
    );
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert!(app.ui.tool_menu.is_none());
}

#[test]
fn completion_keeps_its_start_when_the_request_takes_ownership_of_the_buffer() {
    let mut app = app();
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .definition
        .clone();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("person.na"),
        Some(path),
    );
    app.state.views[view].cursor = ByteOffset(9);
    let (_, document) = app.active_lsp_document().expect("document");
    let key = "owner".to_owned();
    app.language.fixture_session(
        key.clone(),
        crate::editor::session::dispatch::LspRuntimeState {
            root: app.workspace.active_root().expect("workspace root"),
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::editor::session::dispatch::LspLaunch::Direct {
                executable: crate::tools::discover_executable("false").expect("false executable"),
                managed: false,
            },
            capabilities: Some(crate::language::lsp::LspCapabilities {
                completion: true,
                ..Default::default()
            }),
            documents: HashMap::from([(buffer, document)]),
            restart_count: 0,
            restart_due: None,
            circuit_open: false,
            desired_running: true,
            activate_on_restart: true,
        },
    );
    app.mark_lsp_completion_start(false);
    app.request_lsp(crate::language::lsp::LspFeature::Completion);
    assert_eq!(app.language.completion_start(), Some(ByteOffset(7)));
    assert!(app.ui.lsp_loading.is_some());
}

#[test]
fn completion_narrows_as_you_type_and_replaces_what_was_typed() {
    use crate::language::lsp::LspCompletionItem;
    let item = |label: &str| LspCompletionItem {
        label: label.into(),
        filter_text: None,
        sort_text: None,
        detail: String::new(),
        insert_text: label.into(),
        text_edit: None,
        additional_edits: Vec::new(),
        kind: None,
        snippet: false,
        documentation: None,
        resolve_item: None,
        resolved: true,
    };
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("person.na"),
        Some(path),
    );
    app.state.views[view].cursor = ByteOffset(9);
    app.set_dashboard_visibility(false);
    let owner = super::LspProjectionOwner {
        server: "server".into(),
        buffer,
        view,
        revision: app.state.buffers[buffer].text.revision(),
        text: app.state.buffers[buffer].text.source_version(),
        uri: crate::language::lsp::file_uri(app.state.buffers[buffer].path.as_ref().unwrap()),
        language: "rust".into(),
    };
    app.language.fixture_projection(Some(owner.clone()));
    app.language.fixture_completion_source(Some((7, owner)));

    app.language
        .fixture_capabilities(crate::language::lsp::LspCapabilities {
            position_encoding: crate::editor::text::PositionEncoding::Utf16,
            ..Default::default()
        });
    app.mark_lsp_completion_start(false);
    assert_eq!(app.language.completion_start(), Some(ByteOffset(7)));
    app.language.install_completions(
        "server",
        vec![item("age"), item("name"), item("nickname")],
        false,
        7,
    );
    app.narrow_lsp_completion();

    let labels = |app: &InteractiveApplication| {
        app.ui.language_completion.as_ref().map(|menu| {
            menu.items
                .iter()
                .map(|item| item.label.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        labels(&app),
        Some(vec!["name".to_owned(), "nickname".to_owned()]),
        "`na` drops `age` and ranks the whole-word match first"
    );

    app.accept_lsp_completion();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "person.name");
    assert!(app.ui.language_completion.is_none());
}

fn fixture_owned_lsp(
    app: &mut InteractiveApplication,
    key: &str,
    buffer: crate::editor::BufferId,
) -> crate::language::lsp::EditSourceFixture {
    let definition = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == "rust-analyzer")
        .unwrap()
        .definition
        .clone();
    let state = &app.state.buffers[buffer];
    let document = crate::language::lsp::LspDocument {
        path: state.path.clone().unwrap(),
        language_id: "rust".into(),
        version: state.text.revision().0,
        text: state.text.to_string(),
    };
    let endpoint =
        app.language
            .fixture_transport(key, &document, (buffer, state.text.source_version()));
    app.language.fixture_session(
        key.into(),
        crate::editor::session::lsp::LspRuntimeState {
            root: app.workspace.active_root().unwrap(),
            root_path: app.state.workspace_root.clone(),
            server_id: definition.id.clone(),
            definition,
            launch: crate::editor::session::lsp::LspLaunch::Direct {
                executable: "/bin/false".into(),
                managed: false,
            },
            capabilities: Some(crate::language::lsp::LspCapabilities {
                code_action: true,
                execute_command: true,
                selection_ranges: true,
                ..Default::default()
            }),
            documents: HashMap::from([(buffer, document)]),
            restart_count: 0,
            restart_due: None,
            circuit_open: false,
            desired_running: true,
            activate_on_restart: true,
        },
    );
    endpoint
}

#[test]
fn save_notifications_reach_every_document_owner_after_buffer_switch() {
    let mut app = app();
    let path = app.state.workspace_root.join("saved.rs");
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()));
    let first = fixture_owned_lsp(&mut app, "first", buffer);
    let second = fixture_owned_lsp(&mut app, "second", buffer);
    app.replace_with_recovered(buffer, "saved".into()).unwrap();
    app.start_save(false, None, false);
    let other_path = app.state.workspace_root.join("other.rs");
    let (other, _) = app
        .state
        .open_text(TextBuffer::from_text("other"), Some(other_path));
    let unrelated = fixture_owned_lsp(&mut app, "unrelated", other);
    app.language.select("unrelated");
    wait_vfs(&mut app);
    assert_eq!(
        first.saved_uri(),
        Some(crate::language::lsp::file_uri(&path))
    );
    assert_eq!(
        second.saved_uri(),
        Some(crate::language::lsp::file_uri(&path))
    );
    assert_eq!(unrelated.saved_uri(), None);
}

#[test]
fn save_as_detaches_previous_project_ownership_before_sync() {
    let mut app = app();
    let path = app.state.workspace_root.join("old.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("text"), Some(path));
    let _transport = fixture_owned_lsp(&mut app, "old-project", buffer);
    app.language.select("old-project");
    fs::create_dir(app.state.workspace_root.join("new-project")).unwrap();
    app.start_save(
        false,
        Some(app.state.workspace_root.join("new-project/new.rs")),
        false,
    );
    wait_vfs(&mut app);
    app.sync_lsp_document();
    assert!(
        !app.language.sessions()["old-project"]
            .documents
            .contains_key(&buffer)
    );
}

#[test]
fn explicit_lsp_stop_clears_push_and_pull_diagnostic_sources() {
    use crate::editor::session::diagnostics::{
        DiagnosticBatch, DiagnosticDocument, DiagnosticValidity,
    };
    for effect in [AppEffect::LspStop, AppEffect::LspDetach, AppEffect::StopSelectedLsp] {
        let mut app = app();
        let path = app.state.workspace_root.join("problem.rs");
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("text"), Some(path.clone()));
        let _transport = fixture_owned_lsp(&mut app, "owned", buffer);
        app.language.select("owned");
        for prefix in ["lsp.", "lsp-pull:"] {
            let producer = app.language.diagnostic_producer("owned", prefix).unwrap();
            app.diagnostics.activate(producer.clone());
            app.diagnostics
                .publish(
                    DiagnosticBatch {
                        producer,
                        uri: crate::language::lsp::file_uri(&path),
                        document: DiagnosticDocument::capture(&app.state, buffer),
                        validity: DiagnosticValidity::UnversionedReport,
                        items: vec![crate::workspace::Diagnostic {
                            uri: crate::language::lsp::file_uri(&path),
                            version: None,
                            range: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                            severity: DiagnosticSeverity::Error,
                            source: "server".into(),
                            code: None,
                            message: "problem".into(),
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
        }
        assert_eq!(app.diagnostics.choices().count(), 2);
        if matches!(effect, AppEffect::StopSelectedLsp) {
            app.stop_lsp_sessions_for_server("rust-analyzer");
        } else {
            app.process_effect(effect);
        }
        assert_eq!(app.diagnostics.choices().count(), 0);
    }
}

#[test]
fn code_action_command_waits_for_approval_of_its_edits() {
    use crate::language::lsp::*;
    let mut app = app();
    let path = app.state.workspace_root.join("action.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()));
    let transport = fixture_owned_lsp(&mut app, "action-server", buffer);
    app.language.select("action-server");
    let uri = file_uri(&path);
    app.present_lsp_result(
        "action-server",
        1,
        "textDocument/codeAction",
        &uri,
        LspFeatureResult::CodeActions(vec![LspCodeAction {
            title: "edit and command".into(),
            kind: None,
            preferred: false,
            edit: Some(LspWorkspaceEdit {
                resources: Vec::new(),
                documents: vec![LspDocumentEdit {
                    uri: uri.clone(),
                    version: None,
                    edits: vec![LspTextEdit {
                        range: LspRange {
                            start: LspPosition {
                                line: 0,
                                character: 0,
                            },
                            end: LspPosition {
                                line: 0,
                                character: 3,
                            },
                        },
                        new_text: "new".into(),
                    }],
                }],
            }),
            command: Some(LspCommand {
                command: "finish-action".into(),
                arguments: Vec::new(),
            }),
        }]),
    );
    app.accept_picker();
    assert_eq!(transport.executed_command(), None);
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new");
    assert_eq!(transport.executed_command(), Some("finish-action".into()));
}

#[test]
fn selection_expansion_requests_ranges_and_finishes_when_they_arrive() {
    use crate::language::lsp::{LspFeatureResult, LspPosition, LspRange};
    let mut app = app();
    let path = app.state.workspace_root.join("selection.rs");
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc def;"), Some(path.clone()));
    app.set_dashboard_visibility(false);
    let _transport = fixture_owned_lsp(&mut app, "owned", buffer);
    app.language.select("owned");
    app.state.views[view].cursor = ByteOffset(5);
    let generation = app.language.generation();
    app.process_effect(AppEffect::LspSelectionExpand);
    assert!(app.language.generation() > generation);
    assert!(app.state.views[view].selection.is_none());
    app.present_lsp_result("owned", app.language.generation(), "textDocument/selectionRange",
        &crate::language::lsp::file_uri(&path), LspFeatureResult::Ranges(vec![
            LspRange { start: LspPosition { line: 0, character: 4 }, end: LspPosition { line: 0, character: 7 } },
            LspRange { start: LspPosition { line: 0, character: 0 }, end: LspPosition { line: 0, character: 7 } },
        ]));
    assert_eq!(app.state.views[view].selection.unwrap().anchor, ByteOffset(4));
    assert_eq!(app.state.views[view].selection.unwrap().head, ByteOffset(6));
    app.process_effect(AppEffect::LspSelectionExpand);
    assert_eq!(app.state.views[view].selection.unwrap().anchor, ByteOffset(0));
    app.state.views[view].selection = None;
    app.state.views[view].cursor = ByteOffset(1);
    let generation = app.language.generation();
    app.process_effect(AppEffect::LspSelectionExpand);
    assert!(app.language.generation() > generation, "ranges for another cursor must be requested again");
}

#[test]
fn explicit_detach_survives_autostart_until_attach() {
    let mut app = app();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    let path = app.state.workspace_root.join("detach.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("text"), Some(path));
    let _transport = fixture_owned_lsp(&mut app, "owned", buffer);
    app.language.select("owned");
    app.process_effect(AppEffect::LspDetach);
    app.sync_lsp_document();
    assert!(!app.language.project_pending());
    assert!(
        !app.language.sessions()["owned"]
            .documents
            .contains_key(&buffer)
    );
    app.language.select("owned");
    app.process_effect(AppEffect::LspAttach);
    assert!(
        app.language.sessions()["owned"]
            .documents
            .contains_key(&buffer)
    );
}

#[test]
fn stopping_a_starting_server_retires_its_launch() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    );
    app.set_dashboard_visibility(false);
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != crate::tools::ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "automatic-rust".into(),
            name: "Automatic Rust".into(),
            kind: crate::tools::ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
            format_arguments: Vec::new(),
            probe_args: Vec::new(),
            debug_program_policy: crate::tools::DebugProgramPolicy::default(),
            project_root_markers: vec!["Cargo.toml".into()],
            project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
            install: None,
            pins: Vec::new(),
        },
        status: crate::tools::ToolStatus::Installed(false_executable),
    });
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();

    app.sync_lsp_document();
    assert!(app.language.project_pending());
    assert!(app.language.active().is_none());
    app.process_effect(AppEffect::LspStop);
    assert!(app.language.is_stopped("automatic-rust"));
    assert!(!app.language.project_pending());
    assert!(!app.language.launch_pending("automatic-rust"));
    wait_vfs(&mut app);
    app.sync_lsp_document();
    assert!(!app.language.project_pending());
}
