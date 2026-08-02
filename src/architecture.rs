use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArchitectureAudit {
    pub files: usize,
    pub bytes: u64,
    pub rules: Vec<&'static str>,
}
pub fn audit_source(root: &Path) -> Result<ArchitectureAudit, ArchitectureError> {
    let source = root.join("src");
    let mut paths = Vec::new();
    collect(&source, &mut paths)?;
    paths.sort();
    if paths.len() > 10_000 {
        return Err(ArchitectureError::Ceiling);
    }
    let mut bytes = 0u64;
    for path in &paths {
        let metadata = fs::symlink_metadata(path).map_err(ArchitectureError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 4 * 1024 * 1024
        {
            return Err(ArchitectureError::Invalid(path.clone()));
        }
        bytes = bytes.saturating_add(metadata.len());
        if bytes > 64 * 1024 * 1024 {
            return Err(ArchitectureError::Ceiling);
        }
        let source = fs::read_to_string(path).map_err(ArchitectureError::Io)?;
        if path.file_name().and_then(|name| name.to_str()) == Some("architecture.rs") {
            continue;
        }
        for (pattern, rule) in [
            ("Command::new(\"sh\")", "implicit shell execution"),
            ("Command::new(\"bash\")", "implicit shell execution"),
            ("Command::new(\"git\")", "Git CLI package fetching"),
            ("wasmtime_wasi", "ambient WASI linkage"),
            ("libloading::", "native plugin/parser dynamic loading"),
        ] {
            if source.contains(pattern) {
                return Err(ArchitectureError::Violation {
                    path: path.clone(),
                    rule,
                });
            }
        }
    }
    let interactive = fs::read_to_string(source.join("application/interactive.rs"))
        .map_err(ArchitectureError::Io)?;
    let languages =
        fs::read_to_string(source.join("languages/mod.rs")).map_err(ArchitectureError::Io)?;
    let debugging =
        fs::read_to_string(source.join("debugging/mod.rs")).map_err(ArchitectureError::Io)?;
    let packages =
        fs::read_to_string(source.join("packages/mod.rs")).map_err(ArchitectureError::Io)?;
    let registry =
        fs::read_to_string(source.join("packages/registry.rs")).map_err(ArchitectureError::Io)?;
    if !interactive.contains("PendingExecutionPreflight::ToolInstall")
        || !interactive.contains("PendingExecutionPreflight::Lsp")
        || !interactive.contains("PendingExecutionPreflight::DapToolAdapter")
        || !languages.contains("start_session_absolute_with_policy")
        || !languages.contains("PreflightRequired")
        || debugging.contains("discover_executable")
        || !packages.contains("install_absolute")
        || !packages.contains("env_clear()")
        || !registry.contains("https_only(true)")
        || !registry.contains("Policy::none()")
        || !registry.contains("OFFICIAL_REGISTRY")
        || !registry.contains("take(maximum as u64 + 1)")
    {
        return Err(ArchitectureError::Violation {
            path: source.clone(),
            rule: "process launch paths must retain absolute execution-preflight ownership",
        });
    }
    if let Err(diagnostic) = crate::syntax::validate_builtin_queries() {
        return Err(ArchitectureError::Syntax(diagnostic));
    }
    let plugin =
        fs::read_to_string(source.join("plugins/mod.rs")).map_err(ArchitectureError::Io)?;
    let plugin_windows = fs::read_to_string(source.join("application/plugin_windows.rs"))
        .map_err(ArchitectureError::Io)?;
    let plugin_storage =
        fs::read_to_string(source.join("plugins/storage.rs")).map_err(ArchitectureError::Io)?;
    let plugin_providers =
        fs::read_to_string(source.join("plugins/providers.rs")).map_err(ArchitectureError::Io)?;
    let plugin_virtual_documents = fs::read_to_string(source.join("plugins/virtual_documents.rs"))
        .map_err(ArchitectureError::Io)?;
    let virtual_document_editor =
        fs::read_to_string(source.join("application/virtual_documents.rs"))
            .map_err(ArchitectureError::Io)?;
    let git_status =
        fs::read_to_string(source.join("workspace/git.rs")).map_err(ArchitectureError::Io)?;
    let search_workflow = fs::read_to_string(source.join("workspace/search_workflow.rs"))
        .map_err(ArchitectureError::Io)?;
    let ai_core = fs::read_to_string(source.join("ai/mod.rs")).map_err(ArchitectureError::Io)?;
    for prohibited in [
        "EditorState",
        "crate::application",
        "crate::windows",
        "reqwest",
        "TcpStream",
        "Command::new",
        "std::process",
        "std::fs",
    ] {
        if ai_core.contains(prohibited) {
            return Err(ArchitectureError::Violation {
                path: source.join("ai/mod.rs"),
                rule: "AI core must remain queued data-only without editor graph network process or filesystem authority",
            });
        }
    }
    if !ai_core.contains("AiProviderService")
        || !ai_core.contains("let mut candidate = active.clone()")
        || !ai_core.contains("snapshot_if_changed")
        || !ai_core.contains("RemoteTransportUnsupported")
        || !ai_core.contains("[REDACTED]")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("ai/mod.rs"),
            rule: "AI core must retain typed bounded redacted exact-revision provider ownership",
        });
    }
    let ai_begin =
        interactive
            .find("AI_APPLICATION_DOGFOOD_BEGIN")
            .ok_or(ArchitectureError::Violation {
                path: source.join("application/interactive.rs"),
                rule: "AI application bridge audit markers are required",
            })?;
    let ai_end =
        interactive
            .find("AI_APPLICATION_DOGFOOD_END")
            .ok_or(ArchitectureError::Violation {
                path: source.join("application/interactive.rs"),
                rule: "AI application bridge audit markers are required",
            })?;
    let ai_application = &interactive[ai_begin..ai_end];
    for prohibited in [
        "std::env",
        "Command::new",
        "TcpStream",
        "reqwest",
        "PtyService",
        "VfsWorker",
        "fs::",
    ] {
        if ai_application.contains(prohibited) {
            return Err(ArchitectureError::Violation {
                path: source.join("application/interactive.rs"),
                rule: "AI application dogfood must not gain environment network process PTY or filesystem authority",
            });
        }
    }
    let ai_poll = interactive.find("application.process_ai_events();");
    let render_call = interactive.find("let frame = render(");
    if ai_poll.is_none()
        || render_call.is_none()
        || ai_poll >= render_call
        || !ai_application.contains("VirtualDocumentEditorBridge")
            && !ai_application.contains("plugin_virtual_editor")
        || !ai_application.contains("WorkspaceEditProposal::build")
        || !ai_application.contains("show_workspace_proposal")
        || !ai_application.contains("UiGhostText")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("application/interactive.rs"),
            rule: "AI polling must precede rendering and publish only through read-only virtual ghost and proposal gates",
        });
    }
    let filesystem_surface =
        fs::read_to_string(source.join("filesystem/mod.rs")).map_err(ArchitectureError::Io)?;
    let watch_begin = interactive.find("EXPLORER_WATCH_FOREGROUND_BEGIN").ok_or(
        ArchitectureError::Violation {
            path: source.join("application/interactive.rs"),
            rule: "explorer watch foreground audit markers are required",
        },
    )?;
    let watch_end =
        interactive
            .find("EXPLORER_WATCH_FOREGROUND_END")
            .ok_or(ArchitectureError::Violation {
                path: source.join("application/interactive.rs"),
                rule: "explorer watch foreground audit markers are required",
            })?;
    let watch_foreground = &interactive[watch_begin..watch_end];
    if ["fs::", "read_dir", "symlink_metadata", "canonicalize("]
        .iter()
        .any(|pattern| watch_foreground.contains(pattern))
        || !filesystem_surface.contains("ExplorerWatchService")
        || !filesystem_surface.contains("diff_explorer_fingerprints")
        || !interactive.contains("process_explorer_watch_events")
        || !interactive.contains("ExplorerPage")
        || !interactive.contains("TerminalMouseInput::ScrollUp")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("filesystem/mod.rs"),
            rule: "explorer watch scanning must remain bounded worker-owned with typed foreground scroll and reload coordination",
        });
    }
    let render_surface =
        fs::read_to_string(source.join("terminal_ui/render.rs")).map_err(ArchitectureError::Io)?;
    let app_commands = fs::read_to_string(source.join("application/commands.rs"))
        .map_err(ArchitectureError::Io)?;
    let language_surface =
        fs::read_to_string(source.join("languages/mod.rs")).map_err(ArchitectureError::Io)?;
    if !render_surface.contains("content_tab_segments")
        || !render_surface.contains("content_tab_width")
        || !render_surface.contains("ContentTabDiagnosticSeverity")
        || !interactive.contains("content_line_projection_signature")
        || !interactive.contains("TerminalMouseInput::MiddleDown")
        || !interactive.contains("extension_icons")
        || !app_commands.contains("buffer.pin.toggle")
        || !app_commands.contains("buffer.move_left")
        || !app_commands.contains("buffer.move_right")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("terminal_ui/render.rs"),
            rule: "content line must share typed bounded segment widths and identity-owned pin order diagnostics and mouse policy",
        });
    }
    if !render_surface.contains("pub struct VisualRowModel")
        || !render_surface.contains("pub struct VisualRowSlice")
        || !render_surface.contains("line_rows")
        || !render_surface.contains("cursor_projection")
        || !render_surface.contains("byte_at_cell")
        || !render_surface.contains("scroll_wrap_row")
        || !interactive.contains("VisualRowModel::new")
        || !interactive.contains("visual_rows.byte_at_cell")
        || interactive.contains("wrap_row.saturating_mul")
        || !interactive.contains("SetScrollWrapRow")
        || !render_surface.contains("DocumentationSection")
        || !language_surface.contains("LspMarkupKind")
        || !language_surface.contains("pub struct LspHover")
        || !interactive.contains("lsp_completion_owner")
        || !app_commands.contains("lsp.completion.documentation.open")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("terminal_ui/render.rs"),
            rule: "visual-row navigation and typed documentation must share immutable revision-owned projections",
        });
    }
    let pty_screen = fs::read_to_string(source.join("terminal_ui/pty_screen.rs"))
        .map_err(ArchitectureError::Io)?;
    let external_tui_test =
        fs::read_to_string(root.join("tests/external_tui.rs")).map_err(ArchitectureError::Io)?;
    let editing_state =
        fs::read_to_string(source.join("editing/state.rs")).map_err(ArchitectureError::Io)?;
    let editing_transaction =
        fs::read_to_string(source.join("editing/transaction.rs")).map_err(ArchitectureError::Io)?;
    let persistence_surface = fs::read_to_string(source.join("application/interactive.rs"))
        .map_err(ArchitectureError::Io)?;
    let git_sign_begin = persistence_surface
        .find("GIT_SIGN_FOREGROUND_BEGIN")
        .ok_or(ArchitectureError::Violation {
            path: source.join("application/interactive.rs"),
            rule: "Git-sign foreground audit markers are required",
        })?;
    let git_sign_end = persistence_surface.find("GIT_SIGN_FOREGROUND_END").ok_or(
        ArchitectureError::Violation {
            path: source.join("application/interactive.rs"),
            rule: "Git-sign foreground audit markers are required",
        },
    )?;
    let git_sign_foreground = &persistence_surface[git_sign_begin..git_sign_end];
    if ["git2::", "std::fs", "read_hunks", "git_index_fingerprint"]
        .iter()
        .any(|pattern| git_sign_foreground.contains(pattern))
    {
        return Err(ArchitectureError::Violation {
            path: source.join("application/interactive.rs"),
            rule: "Git-sign foreground coordination must not perform repository or filesystem I/O",
        });
    }
    let plugin_wit =
        fs::read_to_string(root.join("wit/dust-plugin.wit")).map_err(ArchitectureError::Io)?;
    if !plugin.contains("wasmtime::component::bindgen!")
        || !plugin.contains("ComponentHostState")
        || !plugin.contains("PluginLoadId")
        || !plugin.contains("unload_generation")
        || !plugin.contains("pending_disables")
        || !plugin.contains("AlreadyActive")
        || !plugin.contains("maximum_loads")
        || !plugin.contains("fail_stopped_loads")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("plugins/mod.rs"),
            rule: "plugins must use generated Component Model host bindings",
        });
    }
    if !plugin_wit.contains("queue-ui-request")
        || !plugin_wit.contains("take-ui-events")
        || !plugin_windows.contains("expected_graph_revision")
        || !plugin_windows.contains("validate_tree")
        || !plugin_windows.contains("validate_private_snapshot")
        || !plugin_wit.contains("replace-private")
        || !plugin_wit.contains("patch-private")
        || !plugin_wit.contains("dimensions-revision")
        || !plugin_wit.contains("take-host-events")
        || !plugin_wit.contains("acknowledge-host-events")
        || !plugin.contains("PluginHostEventQueue")
        || !plugin_wit.contains("queue-storage-request")
        || !plugin_wit.contains("take-storage-results")
        || !plugin_storage.contains("dust-plugin-storage")
        || !plugin_storage.contains("create_new(true)")
        || !plugin_storage.contains("sync_all()")
        || !plugin_storage.contains("fs::rename")
        || !plugin_storage.contains("Sha256::digest(plugin.as_bytes())")
        || !plugin_wit.contains("take-picker-provider-requests")
        || !plugin_wit.contains("queue-picker-provider-chunk")
        || !plugin_providers.contains("PluginProviderBroker")
        || !plugin_providers.contains("apply_chunk")
        || !plugin_providers.contains("expected_sequence")
        || plugin_wit.contains("provider-callback")
        || plugin_wit.contains("provider-command")
        || plugin_wit.contains("action-command")
        || !plugin_wit.contains("take-virtual-document-requests")
        || !plugin_wit.contains("queue-virtual-document-proposal")
        || !plugin_virtual_documents.contains("PluginVirtualDocumentBroker")
        || !plugin_virtual_documents.contains("let mut candidate = self")
        || plugin_wit.contains("virtual-document-callback")
        || plugin_wit.contains("storage-path")
        || plugin_wit.contains("storage-root")
        || plugin_wit.contains("storage-plugin")
        || plugin_wit.contains("framebuffer")
        || plugin_wit.contains("global-cells")
        || plugin_wit.contains("render-now")
        || plugin_wit.contains("render-callback")
        || plugin_wit.contains("input-hook")
        || plugin_wit.contains("input-callback")
        || plugin_wit.contains("event-callback")
        || plugin_wit.contains("provider-callback")
        || plugin_wit.contains("protected-layer")
        || plugin_wit.contains("global-origin")
        || plugin_wit.contains("window-graph")
        || plugin_wit.contains("content-id")
        || plugin.contains("crate::windows")
        || !editing_state.contains("VirtualReadOnly")
        || !editing_transaction.contains("if target.read_only")
        || !persistence_surface.contains("!buffer.is_virtual_read_only()")
        || !persistence_surface.contains("virtual document is read-only")
        || !virtual_document_editor.contains("VirtualDocumentEditorBridge")
        || !virtual_document_editor.contains("replace_virtual_read_only")
        || !persistence_surface.contains("builtin.health.virtual")
        || !persistence_surface.contains("# Dust health")
        || !pty_screen.contains("primary_saved")
        || !pty_screen.contains("encode_paste")
        || !pty_screen.contains("PtyColor::Rgb")
        || !pty_screen.contains("scroll_up_region")
        || !pty_screen.contains("origin_mode")
        || !persistence_surface.contains("encode_key")
        || !persistence_surface.contains("encode_paste")
        || !persistence_surface.contains("encode_terminal_mouse_event")
        || !persistence_surface
            .contains("Some(layout.source_revision) != self.state.window_revision()")
        || !external_tui_test.contains("ExecutionPreflightService")
        || !external_tui_test.contains("PtyService")
        || !external_tui_test.contains("CARGO_BIN_EXE_dust")
        || !search_workflow.contains("SearchWorkflowBroker")
        || !search_workflow.contains("InvokeDeclaredProviderCommand")
        || !search_workflow.contains("snapshot_if_changed")
        || !persistence_surface.contains("compose_command_picker_items")
        || !persistence_surface.contains("compose_plugin_picker_items")
        || !persistence_surface.contains("workspace_search_workflow")
        || !persistence_surface.contains("compose_workspace_file_items")
        || !persistence_surface.contains("WorkspaceFileService")
        || !persistence_surface.contains("SearchWorkflowChunk")
        || !search_workflow.contains("preview_request")
        || !git_status.contains("GitStatusService")
        || !git_status.contains("GitHunkService")
        || !git_status.contains("GitMutationService")
        || !git_status.contains("git_index_fingerprint")
        || !git_status.contains("read_hunks_owned")
        || !persistence_surface.contains("start_git_status_refresh")
        || !persistence_surface.contains("open_git_status_picker")
        || !persistence_surface.contains("process_git_hunk_events")
        || !persistence_surface.contains("stage_selected_git_hunk")
        || !persistence_surface.contains("WorkspaceTrust::Trusted")
        || !persistence_surface.contains("reset this hunk")
        || !persistence_surface.contains("handle_content_tab_mouse")
        || !persistence_surface.contains("content_tab_at_column")
        || !persistence_surface.contains("vertical_margin")
        || !persistence_surface.contains("continue_explorer_follow")
        || !git_status.contains("GitResetHunkRequest")
        || !git_status.contains("GitMutationRequest")
        || !git_status.contains("ResetUnstagedHunk")
        || !git_status.contains("aggregate_git_status")
        || !git_status.contains("git_hunk_signs")
        || !persistence_surface.contains("reconcile_git_signs")
        || !persistence_surface.contains("process_git_sign_events")
        || !git_status.contains("root.starts_with(&workdir)")
        || git_status.contains("Command::new")
    {
        return Err(ArchitectureError::Violation {
            path: root.join("wit/dust-plugin.wit"),
            rule: "plugin UI must remain retained, queued, revision-checked, and framebuffer-free",
        });
    }
    Ok(ArchitectureAudit {
        files: paths.len(),
        bytes,
        rules: vec![
            "no implicit shell",
            "no Git CLI fetch",
            "no ambient WASI",
            "no native dynamic plugin/parser loading",
            "generated Component host bindings",
            "absolute execution-preflight ownership",
            "official bounded no-redirect HTTPS registry",
            "compiled pinned built-in syntax registry",
            "generation-owned Component loads",
            "queued retained/private plugin UI without global cells or synchronous hooks",
            "generation-owned coalesced plugin events without synchronous callbacks",
            "worker-owned atomic plugin-private storage without guest paths",
            "queued generation-owned picker providers without guest command authority",
            "queued read-only virtual-document proposals without editor or graph authority",
            "explicit virtual buffer origin guarded from edits saves recovery and persistence",
            "host-private tiled virtual-document publication preserves editor identities",
            "built-in health dogfood uses the bounded read-only virtual bridge",
            "bounded alternate-screen PTY state and pure mode-aware input encoders",
            "scroll-region PTY operations and terminal-focus-gated live key paste focus input",
            "compositor-local SGR mouse and revision-validated no-op PTY resizing",
            "canonical-preflight no-shell external TUI fixture through portable PTY",
            "typed bounded composable search broker with provider command confinement",
            "production file grep command and plugin-provider pickers compose through bounded search contracts",
            "worker-owned read-only root-scoped Git status without shell or Git CLI",
            "exact worker-owned Git mutation enum with immutable gutter signs and directory aggregation",
            "typed bounded redacted AI provider core without editor graph network process or filesystem authority",
            "fake-only AI application dogfood polls outside render and uses virtual ghost and proposal gates",
            "typed content line shares segment widths with identity-owned pins order diagnostics and mouse policy",
            "bounded explorer metadata watch scanning is worker-owned with typed foreground reload and scrolling",
            "shared fold-aware visual rows and exact typed documentation/completion projection",
        ],
    })
}
fn collect(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), ArchitectureError> {
    for entry in fs::read_dir(directory).map_err(ArchitectureError::Io)? {
        let entry = entry.map_err(ArchitectureError::Io)?;
        let kind = entry.file_type().map_err(ArchitectureError::Io)?;
        if kind.is_symlink() {
            return Err(ArchitectureError::Invalid(entry.path()));
        }
        if kind.is_dir() {
            collect(&entry.path(), out)?
        } else if kind.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
        {
            out.push(entry.path())
        }
    }
    Ok(())
}
#[derive(Debug, Error)]
pub enum ArchitectureError {
    #[error("architecture audit I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("invalid architecture source path {0}")]
    Invalid(PathBuf),
    #[error("architecture audit ceiling exceeded")]
    Ceiling,
    #[error("built-in syntax registry is invalid: {0}")]
    Syntax(String),
    #[error("architecture violation in {path}: {rule}")]
    Violation { path: PathBuf, rule: &'static str },
}
