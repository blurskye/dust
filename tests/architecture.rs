use std::{
    fs,
    path::{Path, PathBuf},
};

fn rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read source module") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

fn module_source(relative: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let mut sources = Vec::new();
    if root.is_dir() {
        rust_sources(&root, &mut sources);
        let facade = root.with_extension("rs");
        if facade.is_file() {
            sources.push(facade);
        }
    } else {
        sources.push(root);
    }
    sources.sort();
    sources
        .into_iter()
        .filter(|path| {
            path.file_name().is_none_or(|name| name != "tests.rs")
                && !path
                    .components()
                    .any(|component| component.as_os_str() == "tests")
        })
        .map(|path| {
            let source = fs::read_to_string(&path).expect("read Rust source");
            source
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(&source)
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_absent(relative: &str, forbidden: &[&str]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);
    for path in sources {
        if path.file_name().is_some_and(|name| name == "tests.rs")
            || path
                .components()
                .any(|component| component.as_os_str() == "tests")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read Rust source");
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} violates `{relative}` ownership by importing `{needle}`",
                path.display()
            );
        }
    }
}

#[test]
fn interactive_coordinator_has_only_bounded_owner_references() {
    let source = module_source("src/editor/session/dispatch/mod.rs");
    let body = source
        .split_once("struct InteractiveApplication {")
        .expect("interactive coordinator")
        .1
        .split_once("\n}")
        .expect("interactive coordinator body")
        .0;
    let fields = body
        .lines()
        .filter(|line| {
            line.strip_prefix("    ")
                .is_some_and(|line| !line.starts_with(' ') && line.contains(':'))
        })
        .count();
    assert!(fields <= 26, "interactive coordinator owns {fields} fields");
    for forbidden in ["pending_vfs", "execution_validation"] {
        assert!(
            !body.contains(forbidden),
            "interactive coordinator retains `{forbidden}`"
        );
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let session = root.join("src/editor/session");
    let allowed = [
        "src/editor/session/dispatch/mod.rs",
        "src/editor/session/dispatch/startup.rs",
        "src/editor/session/dispatch/events.rs",
        "src/editor/session/dispatch/effects.rs",
        "src/editor/session/dispatch/render.rs",
        "src/editor/session/dispatch/shutdown.rs",
    ];
    for relative in allowed {
        assert!(root.join(relative).is_file(), "missing `{relative}`");
    }
    let mut sources = Vec::new();
    rust_sources(&session, &mut sources);
    for path in sources {
        let source = fs::read_to_string(&path).expect("read session source");
        if source.contains("impl InteractiveApplication") {
            let relative = path.strip_prefix(&root).expect("repository source");
            assert!(
                allowed.iter().any(|allowed| relative == Path::new(allowed)),
                "{} implements the application outside dispatch composition",
                relative.display()
            );
        }
    }
}

#[test]
fn module_dependency_direction_matches_the_architecture_spec() {
    assert_absent(
        "src/editor/text",
        &[
            "crate::editor::state",
            "crate::editor::input",
            "crate::config",
            "crate::files",
            "crate::ui",
            "crossterm",
            "std::fs",
        ],
    );
    assert_absent(
        "src/editor/state",
        &[
            "crate::editor::input",
            "crate::config",
            "crate::files",
            "crate::ui",
            "crossterm",
            "std::fs",
        ],
    );
    assert_absent(
        "src/files",
        &[
            "crate::editor::state",
            "crate::editor::session",
            "crate::ui",
        ],
    );
    assert_absent(
        "src/workspace",
        &[
            "crate::editor::session",
            "crate::editor::input",
            "crate::ui",
            "crate::language::lsp",
            "crate::debugger",
            "crate::tools",
        ],
    );
}

#[test]
fn workspace_traversal_and_search_reads_stay_out_of_the_foreground() {
    let session = module_source("src/editor/session");
    for forbidden in [
        "WalkBuilder",
        "read_dir(",
        "read_to_string(",
        "File::open(",
        "RootIdentity::capture(",
    ] {
        assert!(
            !session.contains(forbidden),
            "foreground session performs workspace traversal/read through `{forbidden}`"
        );
    }

    let files = module_source("src/files");
    assert!(files.contains("pub struct WorkspaceSearchService"));
    assert!(files.contains("mpsc::sync_channel(policy.event_queue_capacity)"));
    assert!(files.contains("read_bounded_bytes(file, policy.maximum_file_bytes)"));

    let workspace = module_source("src/workspace");
    assert!(workspace.contains("pub struct WorkspaceTrustService"));
    assert!(workspace.contains("TRUST_STATE_BYTE_LIMIT"));
    assert!(workspace.contains("NamedTempFile::new_in(parent)"));
}

#[test]
fn tree_sitter_is_owned_only_by_language_syntax() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let syntax = manifest.join("src/language/syntax");
    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    for path in sources {
        if path.starts_with(&syntax) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("source");
        assert!(
            !source.contains("tree_sitter"),
            "{} imports tree-sitter outside language::syntax",
            path.display()
        );
    }
    assert_absent(
        "src/language/syntax",
        &["crate::editor::session", "crate::ui"],
    );
}

#[test]
fn editor_geometry_is_projected_from_the_universal_window_snapshot() {
    let render = module_source("src/ui/render");
    assert!(render.contains("let compositor = active_compositor(state, ui, width, height);"));
    assert!(render.contains("compositor.projection.window("));
    assert!(render.contains("Some(compositor.graph_revision) == ui.window_graph_revision"));
    assert!(render.contains("place_workspace_frame("));
    assert!(
        render
            .contains("render_picker(&mut frame, picker, config, rect, ui.surface_content.picker)")
    );
    assert!(render.contains("let layout = ui.editor_window_layout.as_ref()?;"));
    assert!(render.contains("layout.source_revision != source_revision"));
    assert!(!render.contains("crate::editor::LayoutNode"));
    assert!(!render.contains("collect_pane_rects"));
    assert!(!render.contains("WindowId(u64::MAX)"));
    assert!(!render.contains("unwrap_or(&fallback)"));

    let bridge = module_source("src/editor/session/windows/bridge.rs");
    assert!(bridge.contains("state.window_revision()"));
    assert!(!bridge.contains("EditorWindowSignature"));
    assert!(!bridge.contains("editor_window_signature"));

    let session = module_source("src/editor/session");
    assert!(session.contains("editor_pane_viewport_at("));
    assert!(session.contains("active_editor_pane_viewport("));
    assert!(session.contains("self.terminal_window_rect()"));
    assert!(session.contains("self.window_session.content().explorer"));
    assert!(session.contains("apply_authoritative_editor_transaction"));
    let windows = module_source("src/editor/session/windows.rs");
    assert!(windows.contains("source == Some(layout.source_revision)"));
    assert!(windows.contains("self.core.graph.revision() == Some(layout.graph_revision)"));
    assert!(windows.contains("project_with_tiled_insets("));
    assert!(windows.contains("picker_content_geometry(picker, config, rect)"));
    assert!(!windows.contains("use super::*"));
    for forbidden in ["EditorState", "UiState", "InteractiveApplication"] {
        assert!(
            !windows
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|token| token == forbidden),
            "window projection owner imports {forbidden}"
        );
    }
}

#[test]
fn logical_keys_are_not_dispatched_by_effect_or_render_owners() {
    for relative in [
        "src/editor/session/dispatch/effects.rs",
        "src/ui/render",
        "src/editor/input/grammar.rs",
    ] {
        let source = module_source(relative);
        assert!(
            !source.contains("match key.code") && !source.contains("KeyCode::"),
            "{relative} contains direct logical-key feature dispatch"
        );
    }
}

#[test]
fn current_surface_presentation_comes_from_configuration_assets() {
    let render = module_source("src/ui/render");
    for forbidden in [
        "D U S T",
        "Dust — resize terminal",
        "[scratch]",
        "Search Forward",
        "Search Backward",
        "\"NORMAL\"",
        "\"INSERT\"",
    ] {
        assert!(
            !render.contains(forbidden),
            "renderer embeds configurable presentation literal `{forbidden}`"
        );
    }

    let session = module_source("src/editor/session");
    for forbidden in [
        "unsaved changes; use :q!",
        "new scratch buffer",
        "Dust healthy",
        "opening file…",
        "pattern not found:",
    ] {
        assert!(
            !session.contains(forbidden),
            "session embeds configurable user message `{forbidden}`"
        );
    }

    let terminal = module_source("src/ui/terminal.rs");
    let style_function = terminal
        .split("fn queue_style")
        .nth(1)
        .and_then(|tail| tail.split("fn detect_color_tier").next())
        .expect("theme writer function");
    assert!(!style_function.contains("Color::Cyan"));
    assert!(!style_function.contains("Color::DarkGrey"));
}

#[test]
fn current_surface_io_and_timing_policy_stay_out_of_the_foreground() {
    let session = module_source("src/editor/session");
    for forbidden in [
        "std::fs",
        "fs::read_dir",
        "fs::metadata",
        "Duration::from_millis(250)",
        "Duration::from_millis(100)",
    ] {
        assert!(
            !session.contains(forbidden),
            "foreground session hardcodes I/O/timing policy `{forbidden}`"
        );
    }

    let defaults = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/defaults/config.toml"),
    )
    .expect("read bundled config");
    let value = toml::from_str::<toml::Value>(&defaults).expect("typed bundled config");
    assert!(value["recovery"]["write_debounce_ms"].is_integer());
    assert!(value["recovery"]["cleanup_enabled"].is_bool());
    assert!(value["config_watch"]["poll_interval_ms"].is_integer());
    assert!(value["config_watch"]["debounce_ms"].is_integer());
    assert!(value["ui"]["panes"]["default_ratio"].is_integer());
    assert!(value["input"]["completion_select_first"].is_bool());
    assert!(value["input"]["completion_accept_on_submit"].is_bool());
    assert!(
        value["command_labels"]
            .as_table()
            .is_some_and(|labels| labels.len() > 50 && labels.values().all(toml::Value::is_str))
    );

    for context in [
        "editor_mouse",
        "dashboard_mouse",
        "prompt_mouse",
        "key_release",
    ] {
        assert!(session.contains(&format!("\"{context}\"")));
    }
}

#[test]
fn configuration_and_recovery_sources_use_bounded_capture_readers() {
    let configuration = module_source("src/config");
    assert!(!configuration.contains("fs::read_to_string("));
    assert!(!configuration.contains("fs::read("));
    assert!(configuration.contains("WATCH_FINGERPRINT_BYTE_CEILING.saturating_add(1)"));

    let recovery = module_source("src/files/recovery.rs");
    let discovery = recovery
        .split("pub fn discover_recovery")
        .nth(1)
        .and_then(|source| source.split("pub fn cleanup_recovery").next())
        .expect("recovery discovery implementation");
    assert!(!discovery.contains("fs::read_to_string("));
    assert!(!discovery.contains("fs::read("));
    assert!(discovery.contains("read_recovery_source"));
    assert!(recovery.contains("maximum_bytes.saturating_add(1)"));
    assert!(recovery.contains(".take("));

    let editor = module_source("src/editor");
    assert!(!editor.contains("fs::read_to_string("));
    assert!(!editor.contains("read_to_end("));
}

#[test]
fn obsolete_and_ambiguous_source_owners_do_not_return() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(!manifest.join("crates").exists());
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
        "test_support",
        "editor/session/window_bridge.rs",
        "editor/session/window_bridge",
    ] {
        assert!(
            !manifest.join("src").join(removed).exists(),
            "removed source owner `{removed}` returned"
        );
    }

    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    for path in sources {
        let source = fs::read_to_string(&path).expect("read Rust source");
        for obsolete in [
            "dust_core",
            "dust_text",
            "dust_input",
            "dust_config",
            "dust_vfs",
            "dust_tui",
            "dust_app",
        ] {
            assert!(
                !source.contains(obsolete),
                "{} imports obsolete `{obsolete}`",
                path.display()
            );
        }
    }
}
