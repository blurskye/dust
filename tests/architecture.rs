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

fn assert_absent(module: &str, forbidden: &[&str]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(module);
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);
    for path in sources {
        let source = fs::read_to_string(&path).expect("read Rust source");
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} violates `{module}` ownership by importing `{needle}`",
                path.display()
            );
        }
    }
}

#[test]
fn module_dependency_direction_matches_the_architecture_spec() {
    assert_absent(
        "document",
        &[
            "crate::editing",
            "crate::input",
            "crate::configuration",
            "crate::filesystem",
            "crate::terminal_ui",
            "crate::application",
        ],
    );
    assert_absent(
        "editing",
        &[
            "crate::input",
            "crate::configuration",
            "crate::filesystem",
            "crate::terminal_ui",
            "crate::application",
            "crossterm",
            "std::fs",
        ],
    );
    assert_absent("filesystem", &["crate::editing", "crate::application"]);
    assert_absent(
        "workspace",
        &[
            "crate::editing",
            "crate::application",
            "crate::input",
            "crate::terminal_ui",
            "crate::languages",
            "crate::debugging",
            "crate::packages",
        ],
    );
}

#[test]
fn workspace_traversal_and_search_reads_stay_out_of_the_foreground() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let application = fs::read_to_string(manifest.join("src/application/interactive.rs"))
        .expect("read coordinator");
    let production = application
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&application);
    for forbidden in [
        "WalkBuilder",
        "read_dir(",
        "read_to_string(",
        "File::open(",
        "RootIdentity::capture(",
    ] {
        assert!(
            !production.contains(forbidden),
            "foreground coordinator performs workspace traversal/read through `{forbidden}`"
        );
    }
    let filesystem =
        fs::read_to_string(manifest.join("src/filesystem/mod.rs")).expect("read filesystem owner");
    assert!(filesystem.contains("pub struct WorkspaceSearchService"));
    assert!(filesystem.contains("mpsc::sync_channel(policy.event_queue_capacity)"));
    assert!(filesystem.contains("read_bounded_bytes(file, policy.maximum_file_bytes)"));
    let workspace =
        fs::read_to_string(manifest.join("src/workspace/mod.rs")).expect("read workspace owner");
    assert!(workspace.contains("pub struct WorkspaceTrustService"));
    assert!(workspace.contains("TRUST_STATE_BYTE_LIMIT"));
    assert!(workspace.contains("NamedTempFile::new_in(parent)"));
}

#[test]
fn tree_sitter_is_owned_only_by_syntax() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    for path in sources {
        if path.starts_with(manifest.join("src/syntax")) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("source");
        assert!(
            !source.contains("tree_sitter"),
            "{} imports tree-sitter outside syntax",
            path.display()
        );
    }
    assert_absent(
        "syntax",
        &["crate::editing", "crate::application", "crate::terminal_ui"],
    );
}

#[test]
fn logical_keys_are_not_dispatched_to_features_outside_the_input_pipeline() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/application/interactive.rs",
        "src/terminal_ui/render.rs",
        "src/input/grammar.rs",
    ] {
        let path = manifest.join(relative);
        let source = fs::read_to_string(&path).expect("read dispatch source");
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&source);
        assert!(
            !production.contains("match key.code") && !production.contains("KeyCode::"),
            "{} contains direct logical-key feature dispatch",
            path.display()
        );
    }
}

#[test]
fn current_surface_presentation_comes_from_configuration_assets() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let render =
        fs::read_to_string(manifest.join("src/terminal_ui/render.rs")).expect("read renderer");
    let production = render
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&render);
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
            !production.contains(forbidden),
            "renderer embeds configurable presentation literal `{forbidden}`"
        );
    }
    let application = fs::read_to_string(manifest.join("src/application/interactive.rs"))
        .expect("read application coordinator");
    let application_production = application
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&application);
    for forbidden in [
        "unsaved changes; use :q!",
        "new scratch buffer",
        "Dust healthy",
        "opening file…",
        "pattern not found:",
    ] {
        assert!(
            !application_production.contains(forbidden),
            "application embeds configurable user message `{forbidden}`"
        );
    }
    let terminal = fs::read_to_string(manifest.join("src/terminal_ui/terminal.rs"))
        .expect("read terminal writer");
    let style_function = terminal
        .split("fn queue_style")
        .nth(1)
        .and_then(|tail| tail.split("fn detect_color_tier").next())
        .expect("theme writer function");
    assert!(
        !style_function.contains("Color::Cyan") && !style_function.contains("Color::DarkGrey"),
        "terminal style writer must consume the resolved theme"
    );
}

#[test]
fn current_surface_io_and_timing_policy_stay_out_of_the_foreground() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let application = fs::read_to_string(manifest.join("src/application/interactive.rs"))
        .expect("read application coordinator");
    let production = application
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&application);
    for forbidden in [
        "std::fs",
        "fs::read_dir",
        "fs::metadata",
        "Duration::from_millis(250)",
        "Duration::from_millis(100)",
    ] {
        assert!(
            !production.contains(forbidden),
            "foreground coordinator hardcodes I/O/timing policy `{forbidden}`"
        );
    }
    let defaults = fs::read_to_string(manifest.join("assets/defaults/config.toml"))
        .expect("read bundled config");
    let value = toml::from_str::<toml::Value>(&defaults).expect("typed bundled config");
    let required = [
        (
            "recovery.write_debounce_ms",
            value["recovery"]["write_debounce_ms"].is_integer(),
        ),
        (
            "recovery.cleanup_enabled",
            value["recovery"]["cleanup_enabled"].is_bool(),
        ),
        (
            "recovery.maximum_retained_records",
            value["recovery"]["maximum_retained_records"].is_integer(),
        ),
        (
            "recovery.maximum_age_days",
            value["recovery"]["maximum_age_days"].is_integer(),
        ),
        (
            "config_watch.poll_interval_ms",
            value["config_watch"]["poll_interval_ms"].is_integer(),
        ),
        (
            "config_watch.debounce_ms",
            value["config_watch"]["debounce_ms"].is_integer(),
        ),
        (
            "ui.panes.default_ratio",
            value["ui"]["panes"]["default_ratio"].is_integer(),
        ),
        (
            "ui.panes.resize_step",
            value["ui"]["panes"]["resize_step"].is_integer(),
        ),
        (
            "ui.panes.focus_wrap",
            value["ui"]["panes"]["focus_wrap"].is_bool(),
        ),
        (
            "input.completion_select_first",
            value["input"]["completion_select_first"].is_bool(),
        ),
        (
            "input.completion_accept_on_submit",
            value["input"]["completion_accept_on_submit"].is_bool(),
        ),
    ];
    for (path, present_with_type) in required {
        assert!(
            present_with_type,
            "missing or mistyped current-surface policy `{path}`"
        );
    }
    let aliases = value["input"]["command_aliases"]
        .as_table()
        .expect("aliases are declarative");
    assert!(!aliases.is_empty());
    let labels = value["command_labels"]
        .as_table()
        .expect("command labels are declarative");
    assert!(labels.len() > 50 && labels.values().all(|label| label.is_str()));

    for context in [
        "editor_mouse",
        "dashboard_mouse",
        "prompt_mouse",
        "key_release",
    ] {
        assert!(
            production.contains(&format!("\"{context}\"")),
            "application omits independently configurable mouse surface `{context}`"
        );
    }
    let keymaps = fs::read_to_string(manifest.join("assets/defaults/keymaps.toml"))
        .expect("read bundled keymaps");
    for context in [
        "editor_mouse",
        "command_prompt",
        "search_prompt",
        "dashboard",
        "focus",
    ] {
        assert!(
            keymaps.contains(&format!("context = \"{context}\"")),
            "bundled keymaps omit current input surface `{context}`"
        );
    }

    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    for path in sources {
        if path.ends_with("configuration/mod.rs") || path.ends_with("filesystem/mod.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read production source");
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&source);
        for line in production.lines() {
            let compact = line.replace(' ', "");
            assert!(
                !compact.contains("Duration::from_millis(100)")
                    && !compact.contains("Duration::from_millis(250)"),
                "{} embeds watcher/recovery timing policy in production: {line}",
                path.display()
            );
        }
    }
}

#[test]
fn configuration_and_recovery_sources_use_bounded_capture_readers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let configuration = fs::read_to_string(manifest.join("src/configuration/mod.rs"))
        .expect("read configuration source");
    let configuration_production = configuration
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&configuration);
    assert!(!configuration_production.contains("fs::read_to_string("));
    assert!(!configuration_production.contains("fs::read("));
    assert!(configuration_production.contains("WATCH_FINGERPRINT_BYTE_CEILING.saturating_add(1)"));

    let filesystem =
        fs::read_to_string(manifest.join("src/filesystem/mod.rs")).expect("read filesystem source");
    let discovery = filesystem
        .split("pub fn discover_recovery")
        .nth(1)
        .and_then(|source| source.split("pub fn cleanup_recovery").next())
        .expect("recovery discovery implementation");
    assert!(!discovery.contains("fs::read_to_string("));
    assert!(!discovery.contains("fs::read("));
    assert!(discovery.contains("read_recovery_source"));
    let bounded_reader = filesystem
        .split("fn read_bounded_bytes")
        .nth(1)
        .and_then(|source| source.split("fn read_bounded_utf8").next())
        .expect("bounded byte reader");
    assert!(bounded_reader.contains("maximum_bytes.saturating_add(1)"));
    assert!(bounded_reader.contains(".take("));

    for relative in ["src/application/mod.rs", "src/application/interactive.rs"] {
        let source = fs::read_to_string(manifest.join(relative)).expect("read application source");
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&source);
        assert!(!production.contains("fs::read("));
        assert!(!production.contains("fs::read_to_string("));
        assert!(!production.contains("read_to_end("));
    }
}

#[test]
fn obsolete_internal_package_imports_do_not_return() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !manifest.join("crates").exists(),
        "obsolete multi-crate source tree must not return"
    );
    let root = manifest.join("src");
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);
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
