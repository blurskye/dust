use super::*;
use crate::editor::text::PositionEncoding;
use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::ui::render::overlays::{
    LSP_PROGRESS_DONE, LSP_PROGRESS_DONE_TIMEOUT_MS, SPINNER_FRAMES, SPINNER_INTERVAL_MS,
};

#[test]
fn installed_language_server_autostarts_after_workspace_trust_is_exact() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    ).unwrap();
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
    let definition = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "automatic-rust").unwrap().definition.clone();
    app.tooling.fixture_retain(|entry| entry.definition.id != "automatic-rust");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition: definition.clone(),
        status: ToolStatus::Installing { phase: "queued".into(), completed: 0, total: None },
    });
    app.sync_lsp_document();
    assert!(!app.language.project_pending(), "a queued runtime change blocks autostart");
    app.start_lsp(Some("automatic-rust".into()));
    assert!(!app.language.project_pending(), "explicit startup cannot race runtime removal either");
    assert!(app.ui.notification_error, "explicit startup explains why it was refused");
    app.notify_message("Checking automatic startup".into(), false);
    app.tooling.fixture_retain(|entry| entry.definition.id != "automatic-rust");
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition,
        status: ToolStatus::Installed(crate::tools::discover_executable("false").unwrap()),
    });
    app.sync_lsp_document();
    assert!(
        app.language.project_pending(),
        "trusting the workspace starts the server after its runtime change finishes"
    );
    assert_eq!(app.workspace.trust(), WorkspaceTrust::Trusted);

    // Switching languages during discovery must neither consume the next
    // document's observation nor repeatedly report a busy error on each frame.
    let mut python = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "automatic-rust").unwrap().clone();
    python.definition.id = "automatic-python".into();
    python.definition.languages = vec!["python".into()];
    app.tooling.fixture_insert(python);
    let path = app.state.workspace_root.join("main.py");
    app.state.open_text(
        TextBuffer::from_text("print('hello')"),
        Some(path),
    ).unwrap();
    for _ in 0..3 {
        app.sync_lsp_document();
        assert!(!app.ui.notification_error, "busy discovery is not an autostart error");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.language.project_pending() {
        app.process_lsp_project_root();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.sync_lsp_document();
    assert!(app.language.launch_pending("automatic-python"), "the unchanged document retries after discovery finishes");
}

#[test]
fn autostart_waits_for_other_languages_and_skips_servers_without_a_project_root() {
    let (_app_dirs, mut app) = app();
    fs::create_dir(app.state.workspace_root.join(".git")).unwrap();
    let template = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "rust-analyzer").unwrap().definition.clone();
    app.tooling.fixture_retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let executable = crate::tools::discover_executable("false").unwrap();
    for (id, languages, marker) in [
        ("rust", vec!["rust"], "Cargo.toml"),
        ("typescript", vec!["typescript"], "package.json"),
        ("tailwind", vec!["typescript", "html"], ".git"),
    ] {
        let mut definition = template.clone();
        definition.id = id.into();
        definition.languages = languages.into_iter().map(str::to_owned).collect();
        definition.executable = executable.display().to_string();
        definition.install = None;
        definition.project_root_markers = vec![marker.into()];
        definition.project_root_strategy = crate::files::ProjectRootStrategy::Marker;
        app.tooling.fixture_insert(crate::tools::ToolEntry {
            definition, status: ToolStatus::Installed(executable.clone()),
        });
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(path)).unwrap();
    app.sync_lsp_document();
    assert!(app.language.launch_pending("rust"));
    let path = app.state.workspace_root.join("main.ts");
    app.state.open_text(TextBuffer::from_text("let value = 1;"), Some(path.clone())).unwrap();
    app.sync_lsp_document();
    assert!(!app.language.launch_pending("typescript"));
    let deadline = Instant::now() + Duration::from_secs(5);
    for expected in ["typescript", "tailwind"] {
        while app.language.project_pending() {
            app.process_lsp_project_root();
            assert!(Instant::now() < deadline, "root discovery timed out");
            thread::yield_now();
        }
        app.sync_lsp_document();
        assert!(app.language.launch_pending(expected), "next candidate {expected} must be attempted");
    }
    while app.language.project_pending() {
        app.process_lsp_project_root();
        assert!(Instant::now() < deadline, "Tailwind discovery timed out");
        thread::yield_now();
    }
    assert!(app.language.project_root_missing("typescript", &path, &app.workspace.version(), app.configuration.binding_revision()));
    assert!(app.language.fixture_preflight_projects().any(|(key, _)| key.contains("tailwind")));
}

#[test]
fn a_server_the_user_stopped_stays_stopped_until_asked_for() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    ).unwrap();
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
    let (_app_dirs, mut app) = app();
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
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source)).unwrap();
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
    app.show_lsp_sessions();
    let item = app.search.picker().unwrap().selected_item().unwrap();
    assert!(item.detail.starts_with("nested ·"));
    assert_eq!(item.source, nested.display().to_string());
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
                command: None,
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
    let (_app_dirs, mut app) = app();
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
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source.clone())).unwrap();
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
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    ).unwrap();
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
    let (_app_dirs, mut app) = app();
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
fn workspace_edit_preparation_retries_unopened_files_after_queue_backpressure() {
    use crate::language::lsp::{LspDocumentEdit, LspPosition, LspRange, LspTextEdit, LspWorkspaceEdit};
    let (_app_dirs, mut app) = app();
    let paths = (0..17)
        .map(|index| app.state.workspace_root.join(format!("file-{index}.rs")))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(path, "old").unwrap();
    }
    app.begin_workspace_proposal(
        "many unopened files".into(),
        LspWorkspaceEdit {
            documents: paths.iter().map(|path| LspDocumentEdit {
                uri: crate::language::lsp::file_uri(path),
                version: None,
                edits: vec![LspTextEdit {
                    range: LspRange {
                        start: LspPosition { line: 0, character: 0 },
                        end: LspPosition { line: 0, character: 3 },
                    },
                    new_text: "new".into(),
                }],
            }).collect(),
            resources: Vec::new(),
        },
        None,
        PositionEncoding::Utf16,
    );
    assert!(app.workspace_edits.preparing());
    assert_eq!(app.documents.pending_count(), 8);
    wait_vfs(&mut app);
    assert!(!app.workspace_edits.preparing());
    assert_eq!(app.workspace_edits.proposal().unwrap().files.len(), paths.len());
    app.accept_workspace_proposal();
    wait_workspace_approval(&mut app);
    for path in &paths {
        let (_, buffer) = app.state.buffers.iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(path)).unwrap();
        assert_eq!(buffer.text.to_string(), "new");
        assert!(buffer.dirty);
        assert_eq!(fs::read_to_string(path).unwrap(), "old");
    }
}

#[test]
fn staged_workspace_resources_survive_queue_backpressure_and_can_be_retried() {
    use crate::language::lsp::{LspResourceOperation, LspWorkspaceEdit};
    let (_app_dirs, mut app) = app();
    let paths = (0..9).map(|index| (
        app.state.workspace_root.join(format!("old-{index}")),
        app.state.workspace_root.join(format!("new-{index}")),
    )).collect::<Vec<_>>();
    // Independent previews can accumulate more resources than one admission batch.
    for (source, target) in &paths {
        fs::write(source, "resource").unwrap();
        app.begin_workspace_proposal(
            "rename".into(),
            LspWorkspaceEdit {
                documents: Vec::new(),
                resources: vec![LspResourceOperation::Rename {
                    old_uri: crate::language::lsp::file_uri(source),
                    new_uri: crate::language::lsp::file_uri(target),
                    overwrite: false,
                    ignore_if_exists: false,
                }],
            },
            None,
            PositionEncoding::Utf16,
        );
        wait_vfs(&mut app);
        assert!(app.workspace_edits.proposal().is_some());
        app.accept_workspace_proposal();
    }
    app.process_effect(AppEffect::WorkspaceResourceApply);
    assert_eq!(app.documents.pending_count(), 8);
    wait_vfs(&mut app);
    for (source, target) in &paths[..8] {
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(target).unwrap(), "resource");
    }
    assert!(paths[8].0.exists() && !paths[8].1.exists());
    app.process_effect(AppEffect::WorkspaceResourceApply);
    assert_eq!(app.documents.pending_count(), 1);
    wait_vfs(&mut app);
    assert!(!paths[8].0.exists());
    assert_eq!(fs::read_to_string(&paths[8].1).unwrap(), "resource");
    app.process_effect(AppEffect::WorkspaceResourceApply);
    assert_eq!(app.documents.pending_count(), 0);
}

#[test]
fn unopened_multifile_workspace_edit_previews_then_applies_only_to_editor_buffers() {
    let (_app_dirs, mut app) = app();
    let first = app.state.workspace_root.join("first.rs");
    let second = app.state.workspace_root.join("second.rs");
    fs::write(&first, "abc").expect("first");
    fs::write(&second, "xyz").expect("second");
    let first = fs::canonicalize(first).expect("canonical first");
    let second = fs::canonicalize(second).expect("canonical second");
    let (first_buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_bytes(b"abc").expect("text"),
        Some(first.clone()),
    ).unwrap();
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
    wait_workspace_approval(&mut app);
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
    let (_app_dirs, mut app) = app();
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
    let now = Instant::now();
    app.language.fixture_progress_epoch(
        now - Duration::from_millis(SPINNER_INTERVAL_MS * 3 / 2),
    );
    app.update_lsp_progress_animation(now);
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
        now - Duration::from_millis(timeout.saturating_add(1)),
    );
    app.update_lsp_progress_animation(now);
    assert!(app.ui.lsp_progress.is_empty());
    assert!(app.ui.notification.is_none());
}

#[test]
fn completion_executes_its_command_on_the_offering_server_only_after_insertion() {
    use crate::language::lsp::*;
    for invalid_edit in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("completion.rs");
        let (buffer, view) = app.state.open_text(TextBuffer::from_text("ex"), Some(path)).unwrap();
        app.state.views[view].cursor = ByteOffset(2);
        app.set_dashboard_visibility(false);
        let offering = fixture_owned_lsp(&mut app, "offering", buffer);
        let other = fixture_owned_lsp(&mut app, "other", buffer);
        app.language.select("other");
        app.refresh_lsp_projection_owner();
        app.mark_lsp_completion_start(false);
        let item = LspCompletionItem {
            label: "example".into(), filter_text: None, sort_text: None, detail: String::new(),
            insert_text: "example".into(),
            text_edit: invalid_edit.then(|| LspTextEdit {
                range: LspRange { start: LspPosition { line: 0, character: 2 }, end: LspPosition { line: 0, character: 1 } },
                new_text: "example".into(),
            }),
            additional_edits: Vec::new(), kind: None, snippet: false, documentation: None,
            command: Some(LspCommand { command: "add.import".into(), arguments: vec![serde_json::json!("example")] }),
            resolve_item: None, resolved: true,
        };
        app.present_lsp_result("offering", app.language.generation(), "textDocument/completion", "", LspFeatureResult::Completion { items: vec![item], incomplete: false });
        assert!(app.ui.language_completion.is_some());
        assert_eq!(offering.executed_command(), None);
        app.accept_lsp_completion();
        if invalid_edit {
            assert_eq!(app.state.buffers[buffer].text.to_string(), "ex");
            assert_eq!(offering.executed_command(), None);
            assert!(app.ui.notification_error);
        } else {
            assert_eq!(app.state.buffers[buffer].text.to_string(), "example");
            assert_eq!(offering.executed_command(), Some("add.import".into()));
        }
        assert_eq!(other.executed_command(), None);
    }
}

#[test]
fn unresolved_code_lenses_are_resolved_and_run_on_the_offering_server() {
    use crate::language::lsp::*;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("lens.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("test"), Some(path.clone())).unwrap();
    let offering = fixture_owned_lsp(&mut app, "offering", buffer);
    let other = fixture_owned_lsp(&mut app, "other", buffer);
    app.language.running("offering", LspCapabilities { code_lens: true, code_lens_resolve: true, execute_command: true, ..Default::default() });
    app.language.select("other");
    app.refresh_lsp_projection_owner();
    let item = serde_json::json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}},"data":{"id":42}});
    let range = LspRange { start: LspPosition { line: 0, character: 0 }, end: LspPosition { line: 0, character: 4 } };
    offering.publish_disk_diagnostics(&file_uri(&path), "test", Vec::new());
    let context = app.language.poll_event().unwrap().context;
    app.present_lsp_result_owned("offering", 1, "textDocument/codeLens", &file_uri(&path), LspFeatureResult::CodeLens(vec![LspCodeLens {
        range: range.clone(), title: String::new(), command: None, resolve_item: Some(item.clone()),
    }]), Some(context.clone()));
    assert!(app.ui.picker.is_some());
    app.accept_picker();
    assert!(matches!(offering.requested_feature(), Some(LspFeature::CodeLensResolve { item: sent }) if sent == item));
    assert_eq!(offering.executed_command(), None);
    assert!(other.requested_feature().is_none());
    app.present_lsp_result_owned("offering", app.language.generation(), "codeLens/resolve", &file_uri(&path), LspFeatureResult::CodeLens(vec![LspCodeLens {
        range, title: "Run test".into(),
        command: Some(LspCommand { command: "run.test".into(), arguments: Vec::new() }), resolve_item: None,
    }]), Some(context));
    assert!(app.ui.picker.as_ref().unwrap().rows.iter().any(|row| row.label == "Run test"));
    assert_eq!(offering.executed_command(), None);
    app.accept_picker();
    assert_eq!(offering.executed_command(), Some("run.test".into()));
    assert_eq!(other.executed_command(), None);
}

#[test]
fn lsp_formatting_captures_the_active_indentation_policy() {
    use crate::language::lsp::*;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("format.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("text"), Some(path)).unwrap();
    let transport = fixture_owned_lsp(&mut app, "formatter", buffer);
    app.language.running("formatter", LspCapabilities { formatting: true, range_formatting: true, ..Default::default() });
    app.language.select("formatter");
    app.state.policy.tab_width = 2;
    app.state.policy.expand_tabs = false;
    app.request_lsp(LspFeature::Formatting { tab_size: 0, insert_spaces: true });
    assert!(matches!(transport.requested_feature(), Some(LspFeature::Formatting { tab_size: 2, insert_spaces: false })));
    app.request_lsp(LspFeature::RangeFormatting {
        range: LspRange { start: LspPosition { line: 0, character: 0 }, end: LspPosition { line: 0, character: 4 } },
        tab_size: 0, insert_spaces: true,
    });
    assert!(matches!(transport.requested_feature(), Some(LspFeature::RangeFormatting { tab_size: 2, insert_spaces: false, .. })));
}

#[test]
fn empty_document_highlights_replace_previous_highlights() {
    use crate::language::lsp::*;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("highlights.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("name"), Some(path.clone())).unwrap();
    let _transport = fixture_owned_lsp(&mut app, "highlight", buffer);
    app.language.select("highlight");
    let uri = file_uri(&path);
    let range = LspRange { start: LspPosition { line: 0, character: 0 }, end: LspPosition { line: 0, character: 4 } };
    app.present_lsp_result("highlight", 1, "textDocument/documentHighlight", &uri, LspFeatureResult::Ranges(vec![range]));
    assert_eq!(app.ui.lsp_highlight_spans.len(), 1);
    app.present_lsp_result("highlight", 2, "textDocument/documentHighlight", &uri, LspFeatureResult::Ranges(Vec::new()));
    assert!(app.ui.lsp_highlight_spans.is_empty());
}

#[test]
fn lsp_completion_applies_through_one_validated_transaction() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn ma"),
        Some(path),
    ).unwrap();
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
            command: None,
            insert_text: "in".into(),
            text_edit: None,
            additional_edits: vec![crate::language::lsp::LspTextEdit {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition { line: 0, character: 0 },
                    end: crate::language::lsp::LspPosition { line: 0, character: 0 },
                },
                new_text: "// imported\n".into(),
            }],
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
    assert_eq!(app.state.buffers[buffer].text.to_string(), "// imported\nfn main");
    assert!(app.ui.language_completion.is_none());
    assert!(app.state.buffers[buffer].dirty);
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "fn ma");
}

#[test]
fn lsp_completion_selection_scrolls_and_wraps_inside_the_visible_window() {
    let (_app_dirs, mut app) = app();
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
                command: None,
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
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("abc"),
        Some(path.clone()),
    ).unwrap();
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

#[test]
fn rustup_working_directory_follows_the_selected_document() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let project = root.join("nested");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("rust-toolchain.toml"), "[toolchain]\nchannel = 'nightly'\n").unwrap();
    app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(project.join("main.rs"))).unwrap();
    assert_eq!(app.tool_probe_directory(), Some(project));
    app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(root.join("main.rs"))).unwrap();
    assert_eq!(app.tool_probe_directory(), Some(root));
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
fn background_tool_install_updates_preserve_the_selected_row() {
    use std::os::unix::fs::PermissionsExt;
    let (_app_dirs, mut app) = app();
    let directory = tempdir().unwrap();
    let rustup = directory.path().join("rustup");
    fs::write(&rustup, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).unwrap();
    let definition = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "rust-analyzer").unwrap().definition.clone();
    app.rebuild_tool_menu("lsp", None);
    let menu = app.ui.tool_menu.as_mut().unwrap();
    menu.selected = menu.rows.iter().position(|row| row.id != "rust-analyzer").unwrap();
    let selected = menu.rows[menu.selected].id.clone();
    app.tooling.install(crate::editor::session::tooling::ToolInstall::Rustup {
        definition, rustup, proxy, cwd: directory.path().into(),
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.process_tool_progress();
        let menu = app.ui.tool_menu.as_ref().unwrap();
        assert_eq!(menu.rows[menu.selected].id, selected, "progress must preserve selection");
        app.process_tool_results();
        let menu = app.ui.tool_menu.as_ref().unwrap();
        assert_eq!(menu.rows[menu.selected].id, selected, "completion must preserve selection");
        if !app.tooling.entries().iter().any(|entry| entry.definition.id == "rust-analyzer" && matches!(entry.status, ToolStatus::Installing { .. })) {
            break;
        }
        assert!(Instant::now() < deadline, "installation timed out");
        thread::yield_now();
    }
}

#[cfg(unix)]
#[test]
fn finished_tool_install_progress_cannot_restore_the_installing_state() {
    use std::os::unix::fs::PermissionsExt;

    let (_app_dirs, mut app) = app();
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
    let (_app_dirs, mut app) = app();
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
fn reference_labels_cache_only_bounded_excerpts() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("references.rs");
    let location = |line| {
        let position = crate::language::lsp::LspPosition { line, character: 0 };
        crate::language::lsp::LspLocation {
            uri: crate::language::lsp::file_uri(&path),
            range: crate::language::lsp::LspRange { start: position.clone(), end: position },
        }
    };
    fs::write(&path, "x".repeat(900_000) + "\nshort line\n").unwrap();
    let cache = app.reference_excerpts(&[location(0), location(1)]);
    assert_eq!(cache[&(path.clone(), 1)], "short line");
    assert_eq!(cache[&(path.clone(), 0)].len(), 1024);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.values().map(String::len).sum::<usize>(), 1024 + "short line".len());

    let text = ("🦀".repeat(1024) + "\n").repeat(300);
    app.state.open_text(TextBuffer::from_text(&text), Some(path.clone())).unwrap();
    let locations = (0..300).map(location).collect::<Vec<_>>();
    let cache = app.reference_excerpts(&locations);
    assert_eq!(cache.len(), 256);
    assert_eq!(cache.values().map(String::len).sum::<usize>(), 1024 * 1024);
}

#[test]
fn symbol_picker_uses_the_session_key_to_resolve_position_encoding() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("symbol.rs");
    let (_, view) = app.state.open_text(TextBuffer::from_text("éx"), Some(path.clone())).unwrap();
    let definition = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "rust-analyzer").unwrap().definition.clone();
    let key = "rust-analyzer@nested-project";
    app.language.fixture_session(key.into(), crate::editor::session::dispatch::LspRuntimeState {
        root: app.workspace.active_root().unwrap(), root_path: app.state.workspace_root.clone(),
        server_id: definition.id.clone(), definition,
        launch: crate::editor::session::dispatch::LspLaunch::Direct {
            executable: crate::tools::discover_executable("false").unwrap(), managed: false,
        },
        capabilities: Some(crate::language::lsp::LspCapabilities {
            position_encoding: crate::editor::text::PositionEncoding::Utf8,
            ..Default::default()
        }),
        documents: HashMap::new(), restart_count: 0, restart_due: None,
        circuit_open: false, desired_running: true, activate_on_restart: true,
    });
    let position = crate::language::lsp::LspPosition { line: 0, character: 2 };
    app.open_lsp_symbols_picker(key, vec![crate::language::lsp::LspSymbol {
        name: "x".into(), kind: None, extent: None,
        location: Some(crate::language::lsp::LspLocation {
            uri: crate::language::lsp::file_uri(&path),
            range: crate::language::lsp::LspRange { start: position.clone(), end: position },
        }),
    }]);
    app.accept_picker();
    assert_eq!(app.state.views[view].cursor, ByteOffset(2));
}

#[test]
fn typing_narrows_the_completion_menu_instead_of_closing_it() {
    use crate::editor::text::{ByteOffset, ByteRange, TextEdit, TextEditSet};
    let (_app_dirs, mut app) = app();
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
    ).unwrap();
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
        command: None,
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
    let (_app_dirs, mut app) = app();
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
fn tool_manager_probes_require_trust_and_follow_category_changes() {
    for safe in [false, true] {
        let (_app_dirs, mut app) = app();
        app.tooling.fixture_retain(|_| false);
        let echo = crate::tools::discover_executable("echo").unwrap();
        for (id, kind) in [("probe-lsp", ToolKind::Lsp), ("probe-dap", ToolKind::Dap)] {
            let mut definition = crate::tools::bundled_tool_catalog().remove(0);
            definition.id = id.into();
            definition.kind = kind;
            definition.executable = echo.display().to_string();
            definition.probe_args = vec![id.into()];
            definition.install = None;
            definition.pins.clear();
            app.tooling.fixture_insert(crate::tools::ToolEntry { definition, status: ToolStatus::Installed(echo.clone()) });
        }
        app.open_tool_menu("lsp".into());
        let definition = app.tooling.entries()[0].definition.clone();
        let key = app.tool_version_key(&definition, LspRuntimeSource::Path, Some(&echo));
        assert_eq!(app.observed_tool_version(&definition, LspRuntimeSource::Path, Some(&echo)).0, "detecting…");
        assert!(app.tooling.version(&key).is_none());
        prepare_trust_identity(&mut app);
        for root in app.workspace.roots().to_vec() {
            app.workspace.fixture_trust_identity(root.identity);
        }
        app.workspace.fixture_safe_mode(safe);
        app.reconcile_workspace_trust();
        app.open_tool_menu("lsp".into());
        app.cycle_tool_category(1);
        assert_eq!(app.ui.tool_menu.as_ref().unwrap().category, "dap");
        let definition = app.tooling.entries()[1].definition.clone();
        let key = app.tool_version_key(&definition, LspRuntimeSource::Path, Some(&echo));
        if safe {
            assert_eq!(app.observed_tool_version(&definition, LspRuntimeSource::Path, Some(&echo)).0, "detecting…");
            assert!(app.tooling.version(&key).is_none());
        } else {
            let deadline = Instant::now() + Duration::from_secs(2);
            while app.tooling.version(&key).is_none() {
                app.process_tool_version_results();
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            assert_eq!(app.tooling.version(&key).unwrap().as_ref().unwrap(), "probe-dap");
        }
    }
}

#[test]
fn lsp_tool_menu_reports_real_path_version_executable_and_lifecycle() {
    let (_app_dirs, mut app) = app();
    let source = app.state.workspace_root.join("main.rs");
    app.state
        .open_text(TextBuffer::from_text("fn main() {}"), Some(source)).unwrap();
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

    assert!(app.workspace.execution_allowed().is_err());
    app.open_tool_menu("lsp".into());
    let definition = app.tooling.entries().iter()
        .find(|entry| entry.definition.id == "observed-rust")
        .unwrap().definition.clone();
    assert_eq!(
        app.observed_tool_version(&definition, LspRuntimeSource::Path, Some(&echo)).0,
        "detecting…",
        "opening the tool menu must not probe an untrusted workspace"
    );
    app.queue_tool_version_probe(&definition, LspRuntimeSource::Path, echo.clone());
    assert_eq!(
        app.observed_tool_version(&definition, LspRuntimeSource::Path, Some(&echo)).0,
        "detecting…",
        "individual probes must also respect workspace trust"
    );
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    app.workspace.fixture_safe_mode(true);
    app.open_tool_menu("lsp".into());
    assert_eq!(
        app.observed_tool_version(&definition, LspRuntimeSource::Path, Some(&echo)).0,
        "detecting…",
        "safe mode must not run probes even in a trusted workspace"
    );
    app.workspace.fixture_safe_mode(false);
    app.reconcile_workspace_trust();
    assert!(app.workspace.execution_allowed().is_ok());
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
    let (_app_dirs, mut app) = app();
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
    let (_app_dirs, mut app) = app();
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
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("tools");
    let artifact = directory.path().join("fixture-lsp");
    fs::write(&artifact, "#!/bin/sh\nexit 0\n").unwrap();
    fs::create_dir(&store).unwrap();
    // NixOS must also fail locally, before trying to realize a compatibility runtime.
    fs::write(store.join("nix-lsp-workspace"), "occupied by fixture").unwrap();
    let (_app_dirs, mut app) = app();
    app.tooling.fixture_installer(store.clone());
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
            // A local URL is deliberately rejected by the HTTPS-only installer after
            // admission: this test checks the menu action, not artifact downloading.
            install: Some(crate::tools::ToolInstall::Archive {
                url: format!("file://{}", artifact.display()),
                sha256: "0".repeat(64),
                archive: crate::tools::ToolArchiveFormat::Raw,
                executable_path: "fixture-lsp".into(),
                tree: None,
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Some((_, result)) = app.tooling.poll_install() {
            break result;
        }
        assert!(Instant::now() < deadline, "fixture installation timed out");
        thread::yield_now();
    };
    let (operation, diagnostic) = if is_nixos_host() {
        (crate::tools::ToolOperation::InstallNix, "cannot create tool store")
    } else {
        (crate::tools::ToolOperation::InstallManaged, "HTTPS")
    };
    assert_eq!(result.operation, operation);
    let error = result.result.unwrap_err();
    assert!(error.contains(diagnostic), "{error}");
    assert!(store.join(".installer.lock").is_file());
    assert_eq!(fs::read_to_string(store.join("nix-lsp-workspace")).unwrap(), "occupied by fixture");
    assert_eq!(fs::read_dir(store).unwrap().count(), 2);
}

#[test]
fn tool_menu_navigation_keeps_the_selected_server_inside_the_rendered_list() {
    let (_app_dirs, mut app) = app();
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
    let (_app_dirs, mut app) = app();
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
    ).unwrap();
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
    let (_app_dirs, mut app) = app();
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
    let (_app_dirs, mut app) = app();
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
    ).unwrap();
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
fn completion_anchor_measures_wide_identifiers_in_terminal_cells() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("wide.rs");
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("obj.变量"), Some(path)).unwrap();
    app.state.views[view].cursor = ByteOffset("obj.变量".len());
    let owner = super::LspProjectionOwner {
        server: "server".into(), buffer, view,
        revision: app.state.buffers[buffer].text.revision(),
        text: app.state.buffers[buffer].text.source_version(),
        uri: crate::language::lsp::file_uri(app.state.buffers[buffer].path.as_ref().unwrap()),
        language: "rust".into(),
    };
    app.language.fixture_projection(Some(owner.clone()));
    app.language.fixture_completion_source(Some((7, owner)));
    app.mark_lsp_completion_start(false);
    app.language.install_completions("server", vec![crate::language::lsp::LspCompletionItem {
        label: "变量名称".into(), insert_text: "变量名称".into(), command: None,
        filter_text: None, sort_text: None, detail: String::new(),
        text_edit: None, additional_edits: Vec::new(), kind: None,
        snippet: false, documentation: None, resolve_item: None, resolved: true,
    }], false, 7);
    app.narrow_lsp_completion();
    assert_eq!(app.ui.language_completion.as_ref().unwrap().typed, 4);
}

#[test]
fn completion_narrows_as_you_type_and_replaces_what_was_typed() {
    use crate::language::lsp::LspCompletionItem;
    let item = |label: &str| LspCompletionItem {
        label: label.into(),
        command: None,
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
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("person.na"),
        Some(path),
    ).unwrap();
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

pub(super) fn fixture_owned_lsp(
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
                hover: true,
                completion: true,
                formatting: true,
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
fn overlapping_formatting_saves_wait_for_their_own_results() {
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.lsp.format_on_save = true;
    app.configuration.install_fixture(fixture);
    app.set_dashboard_visibility(false);
    let mut pending = Vec::new();
    let mut transports = Vec::new();
    for index in 0..2 {
        let path = app.state.workspace_root.join(format!("format-{index}.rs"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
        app.replace_with_recovered(buffer, "a  b".into()).unwrap();
        let server = format!("formatter-{index}");
        transports.push(fixture_owned_lsp(&mut app, &server, buffer));
        app.language.running(&server, crate::language::lsp::LspCapabilities {
            formatting: true,
            ..Default::default()
        });
        app.language.select(&server);
        app.process_effect(AppEffect::Save { quit_after: false, path: None, force: false });
        let request = app.format_on_save.get(&buffer).unwrap().request.clone().unwrap();
        pending.push((buffer, path, request));
    }
    assert_eq!(app.format_on_save.len(), 2);
    assert!(pending.iter().all(|(_, path, _)| fs::read_to_string(path).unwrap() == "old"));
    for (transport, (buffer, path, (server, generation))) in transports.into_iter().rev().zip(pending.into_iter().rev()) {
        app.present_lsp_result_owned(&server, generation, "textDocument/formatting",
            &crate::language::lsp::file_uri(&path),
            crate::language::lsp::LspFeatureResult::WorkspaceEdit(crate::language::lsp::LspWorkspaceEdit {
                resources: Vec::new(),
                documents: vec![crate::language::lsp::LspDocumentEdit {
                    uri: crate::language::lsp::file_uri(&path),
                    version: None,
                    edits: vec![crate::language::lsp::LspTextEdit {
                        range: crate::language::lsp::LspRange {
                            start: crate::language::lsp::LspPosition { line: 0, character: 1 },
                            end: crate::language::lsp::LspPosition { line: 0, character: 3 },
                        },
                        new_text: " ".into(),
                    }],
                }],
            }), Some(transport.context()));
        wait_vfs(&mut app);
        assert_eq!(fs::read_to_string(path).unwrap(), "a b");
        assert!(!app.state.buffers[buffer].dirty);
    }
    assert!(app.format_on_save.is_empty());
}

#[test]
fn save_notifications_describe_written_text_not_newer_unsaved_edits() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("saved-snapshot.rs");
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.final_newline = crate::config::FinalNewlinePolicy::Insert;
    app.configuration.install_fixture(fixture);
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
    let transport = fixture_owned_lsp(&mut app, "owner", buffer);
    app.replace_with_recovered(buffer, "written".into()).unwrap();
    let revision = app.state.buffers[buffer].text.revision().0;
    app.start_save(false, None, false);
    app.replace_with_recovered(buffer, "newer unsaved text".into()).unwrap();
    wait_vfs(&mut app);
    assert_eq!(fs::read_to_string(path).unwrap(), "written\n");
    let saved = transport.saved_document().unwrap();
    assert_eq!(saved.text, "written\n");
    assert_eq!(saved.version, revision);
    assert_eq!(app.language.sessions()["owner"].documents[&buffer].text, "newer unsaved text");
    assert!(app.language.synced("owner", buffer, &app.state.buffers[buffer].text.source_version()));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "newer unsaved text");
    assert!(app.state.buffers[buffer].dirty);
}

#[test]
fn active_document_retries_preserve_automatic_completion_after_typing() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("completion.rs");
    let (buffer, view) = app.state.open_text(TextBuffer::from_text(""), Some(path)).unwrap();
    app.set_dashboard_visibility(false);
    let transport = fixture_owned_lsp(&mut app, "server", buffer);
    app.language.select("server");
    let mut config = app.configuration.fixture_snapshot();
    config.settings.lsp.auto_completion = true;
    config.settings.lsp.auto_start = false;
    app.configuration.install_fixture(config);
    app.state.views[view].mode = crate::editor::EditorMode::Insert;
    app.sync_lsp_document();
    assert!(transport.requested_feature().is_none());

    for typed in ['a', 'b'] {
        app.handle_key_in_context("editor.insert", KeyEvent::character(typed));
        app.sync_lsp_document();
        assert!(matches!(
            transport.requested_feature(),
            Some(crate::language::lsp::LspFeature::Completion)
        ));
        app.cancel_lsp_completion();
    }
}

#[test]
fn waiting_lsp_requests_resume_when_their_server_finishes_even_without_progress_ui() {
    use crate::language::lsp::{LspFeature, LspProgressKind, LspRange, LspPosition};
    for display_progress in [true, false] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("loading.rs");
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(path)).unwrap();
        app.set_dashboard_visibility(false);
        let first = fixture_owned_lsp(&mut app, "first", buffer);
        let second = fixture_owned_lsp(&mut app, "second", buffer);
        app.language.select("first");
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.lsp_progress.enabled = display_progress;
        app.configuration.install_fixture(fixture);
        let feature = LspFeature::CodeAction { range: LspRange { start: LspPosition { line: 0, character: 0 }, end: LspPosition { line: 0, character: 0 } }, diagnostics: Vec::new() };
        app.request_lsp(feature);
        assert!(matches!(first.requested_feature(), Some(LspFeature::CodeAction { .. })));
        assert!(matches!(second.requested_feature(), Some(LspFeature::CodeAction { .. })));
        for server in ["first", "second"] {
            app.update_lsp_progress(server.into(), "index".into(), LspProgressKind::Begin, "Indexing".into(), String::new(), None);
        }
        app.show_lsp_no_result("first");
        assert!(app.lsp_waiting.is_some());
        assert!(app.ui.lsp_loading.is_some());
        if !display_progress { assert!(app.ui.lsp_progress.is_empty()); }
        app.update_lsp_progress("first".into(), "index".into(), LspProgressKind::End, String::new(), String::new(), None);
        assert!(app.language.server_loading("second"));
        assert!(app.lsp_waiting.is_none());
        assert!(matches!(first.requested_feature(), Some(LspFeature::CodeAction { .. })));
    }
}

#[test]
fn waiting_lsp_requests_stay_queued_while_their_server_is_starting() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("starting.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(path)).unwrap();
    app.set_dashboard_visibility(false);
    let endpoint = fixture_owned_lsp(&mut app, "starting", buffer);
    app.language.select("starting");
    let capabilities = app.language.sessions()["starting"].capabilities.clone().unwrap();
    app.language.stopped("starting");
    app.request_lsp(crate::language::lsp::LspFeature::CodeAction {
        range: crate::language::lsp::LspRange {
            start: crate::language::lsp::LspPosition { line: 0, character: 0 },
            end: crate::language::lsp::LspPosition { line: 0, character: 0 },
        },
        diagnostics: Vec::new(),
    });
    app.ui.lsp_loading = Some("still starting".into());
    app.update_lsp_progress_animation(app.clock.monotonic());
    assert_eq!(app.ui.lsp_loading.as_deref(), Some("still starting"));
    assert!(app.lsp_waiting.is_some());
    assert!(endpoint.requested_feature().is_none());
    app.language.running("starting", capabilities);
    app.update_lsp_progress_animation(app.clock.monotonic());
    assert!(app.lsp_waiting.is_none());
    assert!(matches!(endpoint.requested_feature(), Some(crate::language::lsp::LspFeature::CodeAction { .. })));
}

#[test]
fn save_notifications_reach_every_document_owner_after_buffer_switch() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("saved.rs");
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
    let first = fixture_owned_lsp(&mut app, "first", buffer);
    let second = fixture_owned_lsp(&mut app, "second", buffer);
    app.replace_with_recovered(buffer, "saved".into()).unwrap();
    app.start_save(false, None, false);
    let other_path = app.state.workspace_root.join("other.rs");
    let (other, _) = app
        .state
        .open_text(TextBuffer::from_text("other"), Some(other_path)).unwrap();
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
    for server in ["first", "second"] {
        assert_eq!(app.language.sessions()[server].documents[&buffer].text, "saved");
        assert!(app.language.synced(server, buffer, &app.state.buffers[buffer].text.source_version()));
    }
}

#[test]
fn save_as_detaches_previous_project_ownership_before_sync() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("old.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("text"), Some(path)).unwrap();
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
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("problem.rs");
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("text"), Some(path.clone())).unwrap();
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
fn code_lens_commands_keep_their_originating_session() {
    use crate::language::lsp::*;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("lens.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("fn f() {}"), Some(path.clone())).unwrap();
    let first = fixture_owned_lsp(&mut app, "first", buffer);
    let second = fixture_owned_lsp(&mut app, "second", buffer);
    app.language.select("second");
    let uri = file_uri(&path);
    first.publish_disk_diagnostics(&uri, "fn f() {}", Vec::new());
    let context = app.language.poll_event().unwrap().context;
    let lens = || LspFeatureResult::CodeLens(vec![LspCodeLens {
        range: LspRange {
            start: LspPosition { line: 0, character: 0 },
            end: LspPosition { line: 0, character: 2 },
        },
        title: "Run".into(),
        resolve_item: None,
        command: Some(LspCommand { command: "run-lens".into(), arguments: vec![serde_json::json!(42)] }),
    }]);
    app.present_lsp_result_owned("first", 1, "textDocument/codeLens", &uri, lens(), Some(context.clone()));
    app.accept_picker();
    assert_eq!(first.executed_command().as_deref(), Some("run-lens"));
    assert_eq!(second.executed_command(), None);

    app.present_lsp_result_owned("first", 2, "textDocument/codeLens", &uri, lens(), Some(context));
    let replacement = fixture_owned_lsp(&mut app, "first", buffer);
    app.accept_picker();
    assert_eq!(replacement.executed_command(), None);
    assert_eq!(second.executed_command(), None);
    assert_eq!(app.ui.notification.as_deref(), Some("code-lens source retired"));
}

#[test]
fn code_action_command_waits_for_approval_of_its_edits() {
    code_action_command_waits_for_approval_and_closes_the_consumed_preview_on_failure();
}

#[test]
fn code_action_command_waits_for_approval_and_closes_the_consumed_preview_on_failure() {
    use crate::language::lsp::*;
    for disconnect in [false, true] {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("action.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
    let transport = fixture_owned_lsp(&mut app, "action-server", buffer);
    app.language.select("action-server");
    let unrelated = app.state.open_text(TextBuffer::from_text("other"),
        Some(path.with_file_name("unrelated.rs"))).unwrap().0;
    drop(fixture_owned_lsp(&mut app, "stopped-server", unrelated));
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
    let transport = if disconnect { drop(transport); None } else { Some(transport) };
    app.accept_workspace_proposal();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new");
    assert!(app.ui.picker.is_none());
    if let Some(transport) = transport {
        assert_eq!(transport.synced_document().unwrap().text, "new");
        assert_eq!(transport.executed_command(), Some("finish-action".into()));
    } else {
        assert!(app.ui.notification_error);
    }
    }
}

#[test]
fn code_action_command_waits_for_resource_application_and_stops_on_failure() {
    use crate::language::lsp::*;
    for fail in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("action.rs");
        let target = path.with_file_name("renamed.rs");
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
        app.set_dashboard_visibility(false);
        let transport = fixture_owned_lsp(&mut app, "action-server", buffer);
        app.language.select("action-server");
        app.present_lsp_result("action-server", 1, "textDocument/codeAction", &file_uri(&path),
            LspFeatureResult::CodeActions(vec![LspCodeAction {
                title: "rename and command".into(), kind: None, preferred: false,
                edit: Some(LspWorkspaceEdit {
                    documents: Vec::new(),
                    resources: vec![LspResourceOperation::Rename {
                        old_uri: file_uri(&path), new_uri: file_uri(&target),
                        overwrite: false, ignore_if_exists: false,
                    }],
                }),
                command: Some(LspCommand { command: "after-rename".into(), arguments: Vec::new() }),
            }]));
        app.accept_picker();
        wait_vfs(&mut app);
        app.accept_workspace_proposal();
        assert!(app.ui.picker.is_none());
        assert!(path.exists() && !target.exists());
        assert_eq!(transport.executed_command(), None, "resource is still staged");
        // Accepting a resource edit must release its source, and must not delay an
        // independent text-only action behind the explicitly staged rename.
        app.present_lsp_result("action-server", 2, "textDocument/codeAction", &file_uri(&path),
            LspFeatureResult::CodeActions(vec![LspCodeAction {
                title: "independent".into(), kind: None, preferred: false,
                edit: Some(LspWorkspaceEdit::default()),
                command: Some(LspCommand { command: "independent".into(), arguments: Vec::new() }),
            }]));
        app.accept_picker();
        app.accept_workspace_proposal();
        assert!(app.ui.picker.is_none());
        assert_eq!(transport.executed_command(), Some("independent".into()));
        if fail { fs::write(&target, "conflict").unwrap(); }
        app.process_effect(AppEffect::WorkspaceResourceApply);
        wait_vfs(&mut app);
        if fail {
            assert!(app.ui.notification_error);
            assert_eq!(transport.executed_command(), None);
            assert!(path.exists());
        } else {
            assert!(!path.exists());
            assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
            assert_eq!(transport.closed_uri(), Some(file_uri(&path)));
            assert_eq!(transport.synced_document().unwrap().path, target);
            assert_eq!(transport.executed_command(), Some("after-rename".into()));
        }
    }
}

#[test]
fn deferred_code_action_commands_belong_to_their_own_resource_proposals() {
    use crate::language::lsp::*;
    let (_app_dirs, mut app) = app();
    let mut fixtures = Vec::new();
    for name in ["first", "second"] {
        let path = app.state.workspace_root.join(format!("{name}.rs"));
        let target = path.with_file_name(format!("{name}-renamed.rs"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path.clone())).unwrap();
        let transport = fixture_owned_lsp(&mut app, name, buffer);
        app.language.select(name);
        app.present_lsp_result(name, 1, "textDocument/codeAction", &file_uri(&path),
            LspFeatureResult::CodeActions(vec![LspCodeAction {
                title: name.into(), kind: None, preferred: false,
                edit: Some(LspWorkspaceEdit {
                    documents: Vec::new(),
                    resources: vec![LspResourceOperation::Rename {
                        old_uri: file_uri(&path), new_uri: file_uri(&target),
                        overwrite: false, ignore_if_exists: false,
                    }],
                }),
                command: Some(LspCommand { command: name.into(), arguments: Vec::new() }),
            }]));
        app.accept_picker();
        wait_vfs(&mut app);
        app.accept_workspace_proposal();
        assert!(app.ui.picker.is_none());
        assert_eq!(transport.executed_command(), None);
        fixtures.push((transport, path, target));
    }
    // The first resource fails; the second command must still run after its rename.
    fs::write(&fixtures[0].2, "conflict").unwrap();
    app.process_effect(AppEffect::WorkspaceResourceApply);
    wait_vfs(&mut app);
    assert_eq!(fixtures[0].0.executed_command(), None);
    assert!(fixtures[0].1.exists());
    assert_eq!(fixtures[1].0.executed_command(), Some("second".into()));
    assert!(!fixtures[1].1.exists());
    assert!(fixtures[1].2.exists());
}

#[test]
fn selection_expansion_requests_ranges_and_finishes_when_they_arrive() {
    use crate::language::lsp::{LspFeatureResult, LspPosition, LspRange};
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("selection.rs");
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc def;"), Some(path.clone())).unwrap();
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
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    let path = app.state.workspace_root.join("detach.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("text"), Some(path)).unwrap();
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
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    ).unwrap();
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
