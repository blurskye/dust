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
    let source_root = root.join("src");
    let mut paths = Vec::new();
    collect(&source_root, &mut paths)?;
    paths.sort();
    if paths.len() > 10_000 {
        return Err(ArchitectureError::Ceiling);
    }

    let architecture_root = source_root.join("architecture");
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
        if path.starts_with(&architecture_root) {
            continue;
        }
        let contents = fs::read_to_string(path).map_err(ArchitectureError::Io)?;
        for (pattern, rule) in [
            ("Command::new(\"sh\")", "implicit shell execution"),
            ("Command::new(\"bash\")", "implicit shell execution"),
            ("Command::new(\"git\")", "Git CLI package fetching"),
            ("wasmtime_wasi", "ambient WASI linkage"),
            ("libloading::", "native plugin/parser dynamic loading"),
        ] {
            if contents.contains(pattern) {
                return Err(ArchitectureError::Violation {
                    path: path.clone(),
                    rule,
                });
            }
        }
    }

    for removed in [
        "application",
        "runtime",
        "workbench",
        "interactive",
        "coordinator",
        "document",
        "editing",
        "terminal_ui",
        "plugins",
        "packages",
        "processes",
        "configuration",
        "filesystem",
        "languages",
        "debugging",
        "observability",
        "assistant",
        "ai",
        "test_support",
    ] {
        let path = source_root.join(removed);
        if path.exists() {
            return Err(ArchitectureError::Violation {
                path,
                rule: "removed or ambiguous source owner must not return",
            });
        }
    }

    if let Err(diagnostic) = crate::language::syntax::validate_builtin_queries() {
        return Err(ArchitectureError::Syntax(diagnostic));
    }

    let session = read_owner(&source_root.join("editor/session"))?;
    let language = read_owner(&source_root.join("language/lsp"))?;
    let debugger = read_owner(&source_root.join("debugger"))?;
    let tools = read_owner(&source_root.join("tools"))?;
    let extensions = read_owner(&source_root.join("extensions"))?;
    let files = read_owner(&source_root.join("files"))?;
    let workspace = read_owner(&source_root.join("workspace"))?;
    let ui = read_owner(&source_root.join("ui"))?;
    let editor_state = read_owner(&source_root.join("editor/state"))?;
    let editor_transaction = read_owner(&source_root.join("editor/transaction"))?;
    let registry =
        fs::read_to_string(source_root.join("tools/registry.rs")).map_err(ArchitectureError::Io)?;
    let wit =
        fs::read_to_string(root.join("wit/dust-plugin.wit")).map_err(ArchitectureError::Io)?;
    let external_tui =
        fs::read_to_string(root.join("tests/external_tui.rs")).map_err(ArchitectureError::Io)?;

    if !session.contains("tool_installer.install")
        || !session.contains("PendingExecutionPreflight::Lsp")
        || !session.contains("PendingExecutionPreflight::DapToolAdapter")
        || !language.contains("start_session_absolute_with_policy")
        || !language.contains("PreflightRequired")
        || debugger.contains("discover_executable")
        || !tools.contains("download_checked")
        || !tools.contains("env_clear()")
        || !registry.contains("https_only(true)")
        || !registry.contains("Policy::none()")
        || !registry.contains("OFFICIAL_REGISTRY")
        || !registry.contains("take(maximum as u64 + 1)")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("editor/session"),
            rule: "process launch paths must retain absolute execution-preflight ownership",
        });
    }

    if !files.contains("ExplorerWatchService")
        || !files.contains("diff_explorer_fingerprints")
        || !files.contains("WorkspaceSearchService")
        || !session.contains("process_explorer_watch_events")
        || !session.contains("ExplorerPage")
        || !session.contains("TerminalMouseInput::ScrollUp")
        || session.contains("WalkBuilder")
        || session.contains("fs::read_dir")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("files"),
            rule: "workspace traversal and explorer watching must remain bounded worker-owned effects",
        });
    }

    if !ui.contains("content_tab_segments")
        || !ui.contains("content_tab_width")
        || !ui.contains("ContentTabDiagnosticSeverity")
        || !ui.contains("pub struct VisualRowModel")
        || !ui.contains("pub struct VisualRowSlice")
        || !ui.contains("cursor_projection")
        || !ui.contains("byte_at_cell")
        || !session.contains("content_line_projection_signature")
        || !session.contains("apply_authoritative_editor_transaction")
        || !session.contains("SetScrollWrapRow")
        || !language.contains("LspMarkupKind")
        || !language.contains("pub struct LspHover")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("ui/render"),
            rule: "render geometry visual rows content lines and documentation must share immutable typed projections",
        });
    }

    if !extensions.contains("wasmtime::component::bindgen!")
        || !extensions.contains("ComponentHostState")
        || !extensions.contains("PluginLoadId")
        || !extensions.contains("unload_generation")
        || !extensions.contains("pending_disables")
        || !extensions.contains("PluginHostEventQueue")
        || !extensions.contains("PluginVirtualDocumentBroker")
        || !extensions.contains("PluginProviderBroker")
        || !extensions.contains("dust-plugin-storage")
        || extensions.contains("crate::ui::window")
        || !wit.contains("queue-ui-request")
        || !wit.contains("take-ui-events")
        || !wit.contains("queue-storage-request")
        || !wit.contains("take-picker-provider-requests")
        || !wit.contains("take-virtual-document-requests")
        || wit.contains("framebuffer")
        || wit.contains("render-callback")
        || wit.contains("input-hook")
        || wit.contains("window-graph")
    {
        return Err(ArchitectureError::Violation {
            path: root.join("wit/dust-plugin.wit"),
            rule: "extension host must remain queued capability-bounded and free of editor/window authority",
        });
    }

    if !editor_state.contains("VirtualReadOnly")
        || !editor_transaction.contains("if target.read_only")
        || !session.contains("virtual document is read-only")
        || !session.contains("VirtualDocumentEditorBridge")
        || !session.contains("replace_virtual_read_only")
        || !session.contains("builtin.health.virtual")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("editor"),
            rule: "virtual documents must remain read-only revision-owned editor projections",
        });
    }

    if !ui.contains("primary_saved")
        || !ui.contains("encode_paste")
        || !ui.contains("PtyColor::Rgb")
        || !ui.contains("scroll_up_region")
        || !ui.contains("origin_mode")
        || !session.contains("encode_terminal_mouse_event")
        || !external_tui.contains("ExecutionPreflightService")
        || !external_tui.contains("PtyService")
        || !external_tui.contains("CARGO_BIN_EXE_dust")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("ui"),
            rule: "terminal and external TUI paths must retain bounded PTY state and canonical preflight",
        });
    }

    if !workspace.contains("SearchWorkflowBroker")
        || !workspace.contains("GitStatusService")
        || !workspace.contains("GitHunkService")
        || !workspace.contains("GitMutationService")
        || workspace.contains("Command::new")
        || !session.contains("compose_workspace_file_items")
        || !session.contains("compose_plugin_picker_items")
        || !session.contains("process_git_hunk_events")
        || !session.contains("stage_selected_git_hunk")
        || !session.contains("WorkspaceTrust::Trusted")
    {
        return Err(ArchitectureError::Violation {
            path: source_root.join("workspace"),
            rule: "workspace search trust and Git remain bounded worker-owned typed operations",
        });
    }

    Ok(ArchitectureAudit {
        files: paths.len(),
        bytes,
        rules: vec![
            "one Rust package with editor-rooted ownership",
            "no removed or ambiguous source owners",
            "no implicit shell",
            "no Git CLI fetch",
            "no ambient WASI",
            "no native dynamic plugin/parser loading",
            "absolute execution-preflight ownership",
            "official bounded no-redirect HTTPS registry",
            "compiled pinned built-in syntax registry",
            "single editor-session composition boundary",
            "service owners do not mutate editor state",
            "worker-owned bounded filesystem traversal",
            "worker-owned explorer metadata watching",
            "worker-owned root-scoped Git status and mutations",
            "typed bounded composable search workflows",
            "immutable renderer snapshots",
            "shared fold-aware visual rows",
            "identity-owned content line projection",
            "typed revision-owned LSP features",
            "typed generation-owned DAP sessions",
            "Dust-owned verified tool installation",
            "generated Component host bindings",
            "generation-owned Component loads",
            "queued retained/private extension UI",
            "worker-owned atomic extension storage",
            "queued generation-owned extension providers",
            "queued read-only virtual documents",
            "virtual read-only editor origins",
            "bounded mode-aware PTY state",
        ],
    })
}

fn read_owner(path: &Path) -> Result<String, ArchitectureError> {
    let mut paths = Vec::new();
    if path.is_dir() {
        collect(path, &mut paths)?;
        let facade = path.with_extension("rs");
        if facade.is_file() {
            paths.push(facade);
        }
    } else {
        paths.push(path.to_path_buf());
    }
    paths.sort();
    let mut source = String::new();
    for path in paths {
        if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs")
            || path
                .components()
                .any(|component| component.as_os_str() == "tests")
        {
            continue;
        }
        source.push_str(&fs::read_to_string(path).map_err(ArchitectureError::Io)?);
        source.push('\n');
    }
    Ok(source)
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
