use super::*;

#[test]
fn installed_language_server_autostarts_after_workspace_trust_is_exact() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(path),
    );
    app.ui.dashboard = false;
    app.tools
        .retain(|entry| entry.definition.kind != crate::tools::ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "automatic-rust".into(),
            name: "Automatic Rust".into(),
            kind: crate::tools::ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
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
    assert!(app.execution_validation.is_none());
    assert!(!app.ui.notification_error);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots() {
        app.trust_store.trust(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.sync_lsp_document();
    assert!(matches!(
        app.execution_validation.as_ref().map(|pending| &pending.action),
        Some(super::DeferredExecution::StartLsp(Some(server)))
            if server == "automatic-rust"
    ));
    assert_eq!(app.workspace.trust(), WorkspaceTrust::Trusted);
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
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "nested-rust".into(),
            name: "Nested Rust".into(),
            kind: ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
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
    for root in app.workspace.roots() {
        app.trust_store.trust(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.execution_revalidated = true;
    app.start_lsp(Some("nested-rust".into()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.pending_lsp_project_root.is_some() {
        app.process_lsp_project_root();
        assert!(
            Instant::now() < deadline,
            "project-root resolution timed out"
        );
        thread::yield_now();
    }
    let action = app
        .pending_execution_preflight
        .values()
        .find_map(|action| match action {
            super::PendingExecutionPreflight::Lsp {
                session_key,
                project_root,
                ..
            } => Some((session_key, project_root)),
            _ => None,
        })
        .expect("nested project must queue an LSP preflight");
    assert_eq!(action.1, &nested);
    let expected_key = crate::language::lsp::LspSessionKey {
        root_id: app.workspace.active_root().expect("root").0,
        project_root: nested,
        server_id: "nested-rust".into(),
    }
    .transport_key();
    assert_eq!(action.0, &expected_key);
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
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Dap);
    let false_executable = crate::tools::discover_executable("false").expect("false executable");
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "nested-rust-dap".into(),
            name: "Nested Rust DAP".into(),
            kind: ToolKind::Dap,
            languages: vec!["rust".into()],
            executable: false_executable.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
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
    for root in app.workspace.roots() {
        app.trust_store.trust(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.execution_revalidated = true;
    app.start_dap(Some("nested-rust-dap".into()), Some(source.clone()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.pending_dap_project_root.is_some() {
        app.process_dap_project_root();
        assert!(
            Instant::now() < deadline,
            "DAP project-root resolution timed out"
        );
        thread::yield_now();
    }
    let action = app
        .pending_execution_preflight
        .values()
        .find_map(|action| match action {
            super::PendingExecutionPreflight::DapToolAdapter {
                project_root,
                program,
                ..
            } => Some((project_root, program)),
            _ => None,
        })
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
    app.tools.push(crate::tools::ToolEntry {
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
    assert!(app.active_lsp_server.is_none());
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("trust"))
    );
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots() {
        app.trust_store.trust(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[package]\nname = \"project-local\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    app.start_lsp(Some("project-local".into()));
    wait_execution_validation(&mut app);
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.pending_lsp_project_root.is_some() && Instant::now() < deadline {
        app.process_lsp_project_root();
        thread::sleep(Duration::from_millis(5));
    }
    while !app.pending_execution_preflight.is_empty() && Instant::now() < deadline {
        app.process_execution_preflight();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        app.active_lsp_server.is_none()
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
    app.picker_insert("health");
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
    while app.workspace_proposal.is_none() && Instant::now() < deadline {
        app.process_vfs_results();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(app.workspace_proposal.is_some());
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
    assert!(
        app.configuration
            .settings
            .ui
            .lsp_progress
            .spinner_frames
            .iter()
            .any(|frame| frame == "⠇")
    );
    app.configuration
        .settings
        .ui
        .lsp_progress
        .spinner_interval_ms = 1_000;
    app.lsp_progress_epoch = Instant::now() - Duration::from_millis(1_500);
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
    let timeout = app
        .configuration
        .settings
        .ui
        .lsp_progress
        .completed_timeout_ms;
    assert_eq!(
        app.configuration.settings.ui.lsp_progress.completed_symbol,
        "✓"
    );
    let completed = app.ui.lsp_progress.first_mut().expect("completed progress");
    assert!(completed.done);
    assert_eq!(completed.message, "Finished");
    completed.completed_at =
        Some(Instant::now() - Duration::from_millis(timeout.saturating_add(1)));
    app.update_lsp_progress_animation();
    assert!(app.ui.lsp_progress.is_empty());
    assert!(app.ui.notification.is_none());
}

#[test]
fn lsp_completion_applies_through_one_validated_transaction() {
    let mut app = app();
    let path = app.state.workspace_root.join("main.rs");
    let (_, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("fn ma"),
        Some(path),
    );
    app.state.views[view].cursor = ByteOffset(5);
    app.ui.dashboard = false;
    app.lsp_capabilities = Some(crate::language::lsp::LspCapabilities {
        position_encoding: crate::editor::text::PositionEncoding::Utf16,
        ..Default::default()
    });
    app.lsp_completion_items = vec![crate::language::lsp::LspCompletionItem {
        label: "main".into(),
        detail: "function".into(),
        insert_text: "in".into(),
        text_edit: None,
        additional_edits: Vec::new(),
        kind: None,
        snippet: false,
        documentation: None,
        resolve_item: None,
        resolved: false,
    }];
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        items: vec![crate::ui::CompletionItem {
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
    app.configuration
        .settings
        .ui
        .command_line
        .completion_max_rows = 3;
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        items: (0..5)
            .map(|index| crate::ui::CompletionItem {
                label: format!("item-{index}"),
                detail: String::new(),
            })
            .collect(),
        selected: 0,
        scroll: 0,
    });
    for _ in 0..4 {
        app.move_lsp_completion(1);
    }
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (4, 2));
    app.move_lsp_completion(1);
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (0, 0));
    app.move_lsp_completion(-1);
    let completion = app.ui.language_completion.as_ref().expect("completion");
    assert_eq!((completion.selected, completion.scroll), (4, 2));
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
    assert!(app.apply_lsp_workspace_edit(edit).is_err());
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
    app.ui.dashboard = false;
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let echo = crate::tools::discover_executable("echo").expect("echo executable");
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "observed-rust".into(),
            name: "Observed Rust".into(),
            kind: ToolKind::Lsp,
            languages: vec!["rust".into()],
            executable: echo.display().to_string(),
            version: Some("managed-9.9.9".into()),
            release: None,
            launch_args: Vec::new(),
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
            assert_eq!(row.lifecycle, "stopped");
            assert!(row.note.contains("uninstall"));
            break;
        }
        assert!(Instant::now() < deadline, "version probe timed out");
        thread::yield_now();
    }
}

#[test]
fn installed_path_server_without_a_qualified_probe_never_says_not_probed() {
    let mut app = app();
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Lsp);
    let echo = crate::tools::discover_executable("echo").expect("echo executable");
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "external-no-version-command".into(),
            name: "External Server".into(),
            kind: ToolKind::Lsp,
            languages: vec!["external-test".into()],
            executable: echo.display().to_string(),
            version: None,
            release: None,
            launch_args: Vec::new(),
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
    assert_eq!(row.version, "version unavailable");
    assert_eq!(row.release, "—");
    assert!(row.note.contains("Installed on PATH"));
}

#[test]
fn lsp_tool_menu_hides_missing_noninstallable_compatibility_entries() {
    let mut app = app();
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Lsp);
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "missing-catalog-server".into(),
            name: "Missing Catalog Server".into(),
            kind: ToolKind::Lsp,
            languages: vec!["catalog-test".into()],
            executable: "dust-certainly-missing-language-server".into(),
            version: None,
            release: None,
            launch_args: Vec::new(),
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
    app.tools
        .retain(|entry| entry.definition.kind != ToolKind::Lsp);
    app.tools.push(crate::tools::ToolEntry {
        definition: crate::tools::ToolDefinition {
            id: "installable-lsp".into(),
            name: "Installable LSP".into(),
            kind: ToolKind::Lsp,
            languages: vec!["install-test".into()],
            executable: "dust-certainly-missing-installable-lsp".into(),
            version: Some("1.0.0".into()),
            release: None,
            launch_args: Vec::new(),
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
    for root in app.workspace.roots() {
        app.trust_store.trust(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.activate_selected_tool();
    assert!(matches!(
        app.execution_validation.as_ref().map(|pending| &pending.action),
        Some(super::DeferredExecution::InstallTool {
            id,
            source: Some(LspRuntimeSource::Managed | LspRuntimeSource::Nix),
        }) if id == "installable-lsp"
    ));
}

#[test]
fn tool_menu_navigation_keeps_the_selected_server_inside_the_rendered_list() {
    let mut app = app();
    app.open_tool_menu("lsp".into());
    let row_count = app.ui.tool_menu.as_ref().expect("menu").rows.len();
    for _ in 0..row_count {
        app.move_tool_menu(1);
        let menu = app.ui.tool_menu.as_ref().expect("menu");
        let configured = &app.configuration.settings.ui.tool_manager;
        let modal_height =
            (u32::from(app.terminal_height) * u32::from(configured.height_percent) / 100) as u16;
        let modal_height = modal_height
            .max(configured.minimum_height.min(app.terminal_height))
            .min(app.terminal_height);
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
        assert!(app.lsp_nix_workspace_preferred.contains("rust-analyzer"));
        assert_eq!(
            app.lsp_managed_versions
                .get("rust-analyzer")
                .map(String::as_str),
            Some("0.3.3025")
        );
        assert_eq!(
            app.ui.tool_menu.as_ref().expect("tool menu").rows[rust].runtime,
            "Dust via Nix"
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
        assert!(app.lsp_managed_preferred.contains("rust-analyzer"));
        assert_eq!(
            app.lsp_managed_versions
                .get("rust-analyzer")
                .map(String::as_str),
            Some("0.3.3008")
        );
        assert_eq!(
            app.ui.tool_menu.as_ref().expect("tool menu").rows[rust].runtime,
            "Dust download"
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
