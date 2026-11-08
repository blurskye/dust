//! Project debug launch regression tests using the existing DAP fixture.
use super::*;
use crate::app::tests::app;
use crate::editor::text::TextBuffer;
use crate::tools::{ToolEntry, ToolStatus};
use std::fs;
use std::time::{Duration, Instant};

fn source(app: &mut App, name: &str, text: &str) -> PathBuf {
    let path = app.state.workspace_root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    app.state
        .open_text(TextBuffer::from_text(text), Some(path.clone()))
        .unwrap();
    path
}

fn trust(app: &mut App) {
    crate::app::tests::prepare_trust_identity(app);
    let identity = app.workspace.roots()[0].identity.clone();
    app.workspace.fixture_trust_identity(identity);
    app.reconcile_workspace_trust();
}

fn adapter(app: &mut App, id: &str, status: ToolStatus) {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == id)
        .unwrap();
    app.tooling
        .fixture_retain(|entry| entry.definition.id != id);
    app.tooling.fixture_insert(ToolEntry { definition, status });
}

#[test]
fn presets_reference_catalog_adapters_and_unique_ids() {
    let presets = bundled_presets();
    let catalog = crate::tools::bundled_tool_catalog();
    let mut ids = std::collections::BTreeSet::new();
    for preset in presets {
        assert!(ids.insert(preset.id));
        assert!(!preset.languages.is_empty());
        assert!(!preset.adapters.is_empty());
        assert!(preset.adapters.iter().all(|id| {
            catalog
                .iter()
                .any(|tool| tool.id == *id && tool.kind == crate::tools::ToolKind::Dap)
        }));
    }
}

#[test]
fn rust_dashboard_lists_cargo_presets_and_marks_missing_adapters() {
    let (_dirs, mut app) = app();
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n",
    )
    .unwrap();
    adapter(&mut app, "codelldb", ToolStatus::Available);
    adapter(&mut app, "lldb-dap", ToolStatus::Available);
    app.start_dap(None, None);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.detail.contains("codelldb missing")
                && row.detail.contains("Enter: install and start"))
    );
}

#[test]
fn installed_lldb_is_preferred_to_downloading_codelldb() {
    let (_dirs, mut app) = app();
    source(&mut app, "main.rs", "fn main() {}\n");
    fs::write(app.state.workspace_root.join("Cargo.toml"), "").unwrap();
    adapter(&mut app, "codelldb", ToolStatus::Available);
    adapter(
        &mut app,
        "lldb-dap",
        ToolStatus::Installed("/fixture/lldb-dap".into()),
    );
    assert!(
        app.detected_debug_presets()
            .iter()
            .all(|launch| launch.adapter == "lldb-dap")
    );
}

#[test]
fn saved_configuration_picker_uses_literal_names() {
    let (dirs, _) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.debug.adapters.insert(
        "fixture".into(),
        crate::config::DebugAdapterConfig {
            program: crate::debugger::test_python(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
        },
    );
    loaded.config.debug.sessions.insert(
        "name with spaces and =".into(),
        crate::config::DebugSessionConfig {
            adapter: "fixture".into(),
            request: crate::config::DebugRequestKind::Attach,
            cwd: None,
            program: None,
            run_in_terminal_profile: None,
            arguments: BTreeMap::new(),
            path_maps: BTreeMap::new(),
        },
    );
    let mut app = App::new(
        crate::app::startup::StartupTarget::Dashboard {
            root: dirs.0.path().into(),
        },
        loaded,
        false,
        dirs.1.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.open_debug_launch_picker();
    assert_eq!(
        app.ui.picker.as_ref().unwrap().rows[0].label,
        "name with spaces and ="
    );
    app.accept_picker();
    assert!(app.ui.notification.as_deref().unwrap().contains("trust"));
}

#[test]
fn untrusted_launch_does_not_install_or_build() {
    let (_dirs, mut app) = app();
    source(&mut app, "test_demo.py", "def test_demo(): pass\n");
    adapter(&mut app, "debugpy", ToolStatus::Available);
    let launch = app.detected_debug_presets().remove(0);
    app.start_debug_preset(launch);
    assert!(app.ui.notification.as_deref().unwrap().contains("trust"));
    assert!(app.debugger_session.install_launch.is_none());
    assert!(app.debugger_session.cargo_build.is_none());
}

#[test]
fn project_detection_stops_at_workspace_boundary_and_uses_nearest_marker() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir_all(workspace.join("nested/src")).unwrap();
    let document = workspace.join("nested/src/main.rs");
    let markers = ["Cargo.toml".into()];
    assert_eq!(project_root(&document, &workspace, &markers), None);
    fs::write(workspace.join("nested/Cargo.toml"), "").unwrap();
    assert_eq!(
        project_root(&document, &workspace, &markers),
        Some(workspace.join("nested"))
    );
}

#[test]
fn cargo_artifacts_ignore_libraries_and_deduplicate_executables() {
    let bytes = br#"{"reason":"compiler-artifact","target":{"name":"lib"},"executable":null}
{"reason":"compiler-artifact","target":{"name":"demo"},"executable":"/project/target/demo"}
{"reason":"compiler-artifact","target":{"name":"demo"},"executable":"/project/target/demo"}
{"reason":"build-finished","success":true}
"#;
    assert_eq!(
        cargo_executables(bytes).unwrap(),
        vec![("demo".into(), PathBuf::from("/project/target/demo"))]
    );
    assert!(cargo_executables(b"not json").is_err());
}

#[test]
fn cursor_tests_use_enclosing_function_and_python_class() {
    use crate::editor::{SyntaxObject, SyntaxObjectKind};
    let source = "class TestDemo:\n    def test_value(self):\n        assert True\n";
    let objects = [
        SyntaxObject {
            kind: SyntaxObjectKind::Class,
            outer: (0, source.len()),
            inner: (15, source.len()),
            parent: None,
        },
        SyntaxObject {
            kind: SyntaxObjectKind::Function,
            outer: (19, source.len()),
            inner: (48, source.len()),
            parent: None,
        },
    ];
    assert_eq!(
        cursor_test("python", source, 50, &objects),
        Some(("python-tests", "TestDemo::test_value".into()))
    );
    assert_eq!(cursor_test("python", source, 2, &objects), None);
    let source = "func TestValue(t *testing.T) { t.Fatal(1) }";
    let objects = [SyntaxObject {
        kind: SyntaxObjectKind::Function,
        outer: (0, source.len()),
        inner: (29, source.len()),
        parent: None,
    }];
    assert_eq!(
        cursor_test("go", source, 34, &objects),
        Some(("go-tests", "TestValue".into()))
    );
    assert_eq!(cursor_test("go", source, source.len(), &objects), None);
}

#[test]
fn installation_completion_launches_original_python_module_with_fixture() {
    let (_dirs, mut app) = app();
    let path = source(&mut app, "test_demo.py", "def test_demo(): pass\n");
    trust(&mut app);
    adapter(
        &mut app,
        "debugpy",
        ToolStatus::Installing {
            phase: "fixture".into(),
            completed: 0,
            total: None,
        },
    );
    let launch = app
        .detected_debug_presets()
        .into_iter()
        .find(|p| p.preset.id == "python-tests")
        .unwrap();
    app.start_debug_preset(launch);
    assert!(app.debugger_session.install_launch.is_some());
    source(&mut app, "other.py", "print('other')\n");
    let script = app.state.workspace_root.join("adapter.py");
    let fixture = crate::debugger::TEST_LOGGING_ADAPTER.replace("c=m['command']", "c=m['command']\n if c=='launch': pathlib.Path('launch.json').write_text(json.dumps(m['arguments']))");
    fs::write(&script, fixture).unwrap();
    let mut definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "debugpy")
        .unwrap();
    definition.launch_args = vec!["-u".into(), script.display().to_string()];
    app.tooling
        .fixture_retain(|entry| entry.definition.id != "debugpy");
    app.tooling.fixture_insert(ToolEntry {
        definition,
        status: ToolStatus::Installed(crate::debugger::test_python()),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let log = app.state.workspace_root.join("launch.json");
    let value = loop {
        app.process_dap_events();
        app.workspace.poll(&app.state);
        app.process_dap_project_root();
        app.process_execution_preflight();
        if let Ok(bytes) = fs::read(&log)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "launch did not reach fixture: {:?}",
            app.ui.notification
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(value["module"], "pytest");
    assert_eq!(value["args"], serde_json::json!([path]));
    assert!(value.get("program").is_none());
    assert_eq!(value["console"], "integratedTerminal");
    app.debugger_session.stop().unwrap();
}

#[test]
fn node_file_preset_uses_installable_server_adapter() {
    let (_dirs, mut app) = app();
    let path = source(&mut app, "demo.ts", "const answer: number = 42;\n");
    let launch = app
        .detected_debug_presets()
        .into_iter()
        .find(|launch| launch.preset.id == "node-file")
        .unwrap();
    assert_eq!(launch.document, path);
    assert_eq!(launch.preset.runtime.as_deref(), Some("node"));
    assert_eq!(launch.preset.arguments["runtimeExecutable"], "node");
    let entry = app
        .tooling
        .entries()
        .iter()
        .find(|entry| entry.definition.id == launch.adapter)
        .unwrap();
    assert!(matches!(
        entry.definition.install,
        Some(crate::tools::ToolInstall::JavascriptArchive { .. })
    ));
    assert!(
        entry
            .definition
            .launch_args
            .iter()
            .any(|arg| arg == "{listen_port}")
    );
}

#[test]
fn adapter_terminal_resolves_path_executables_before_preflight() {
    let (_dirs, mut app) = app();
    app.debugger_session.adapter_runs_terminal = true;
    // Windows installs Python 3 as `python`.
    let python = String::from(if cfg!(windows) { "python" } else { "python3" });
    let profile = app
        .debugger_session
        .adapter_terminal(
            &app.state.workspace_root,
            &[python.clone(), "main.py".into()],
            &Default::default(),
        )
        .unwrap();
    assert_eq!(
        profile.program,
        crate::tools::discover_executable(&python).unwrap()
    );
    assert!(profile.program.is_absolute());
    assert_eq!(profile.arguments, ["main.py"]);
}

#[test]
fn missing_preset_runtime_names_the_next_action() {
    let (_dirs, mut app) = app();
    source(&mut app, "demo.js", "console.log(42);\n");
    trust(&mut app);
    let mut launch = app
        .detected_debug_presets()
        .into_iter()
        .find(|launch| launch.preset.id == "node-file")
        .unwrap();
    launch.preset.runtime = Some("dust-missing-debug-runtime-fixture".into());
    app.start_debug_preset(launch);
    let message = app.ui.notification.as_deref().unwrap();
    assert!(message.contains("dust-missing-debug-runtime-fixture is missing"));
    assert!(message.contains("PATH") && message.contains("Space d s"));
    assert!(app.debugger_session.install_launch.is_none());
}

#[test]
fn rust_cursor_names_are_exact_and_keep_ambiguous_modules_separate() {
    assert_eq!(
        rust_test_names(
            b"one::works: test\ntwo::works: test\nworks_more: test\nworks: benchmark\n",
            "works"
        ),
        ["one::works", "two::works"]
    );
}

#[test]
fn cargo_build_and_exact_cursor_test_reach_the_dap_fixture() {
    let (_dirs, mut app) = app();
    source(
        &mut app,
        "src/lib.rs",
        "#[cfg(test)] mod tests { #[test] fn works() {} #[test] fn works_more() {} }\n",
    );
    fs::write(
        app.state.workspace_root.join("Cargo.toml"),
        "[package]\nname='dust-debug-fixture'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    trust(&mut app);
    let script = app.state.workspace_root.join("adapter.py");
    fs::write(&script, crate::debugger::TEST_LOGGING_ADAPTER.replace("c=m['command']", "c=m['command']\n if c=='launch': pathlib.Path('launch.json').write_text(json.dumps(m['arguments']))")).unwrap();
    let mut definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "codelldb")
        .unwrap();
    definition.launch_args = vec!["-u".into(), script.display().to_string()];
    app.tooling
        .fixture_retain(|entry| entry.definition.id != "codelldb");
    app.tooling.fixture_insert(ToolEntry {
        definition,
        status: ToolStatus::Installed(crate::debugger::test_python()),
    });
    let mut launch = app
        .detected_debug_presets()
        .into_iter()
        .find(|p| p.preset.id == "rust-tests")
        .unwrap();
    launch.test = Some("works".into());
    // The builder exports a shared target directory; this disposable project's binaries
    // must remain within its own workspace, just like a user's debug program.
    launch.preset.build.extend([
        "--target-dir".into(),
        app.state
            .workspace_root
            .join("target")
            .display()
            .to_string(),
    ]);
    app.start_debug_preset(launch);
    let deadline = Instant::now() + Duration::from_secs(60);
    let log = app.state.workspace_root.join("launch.json");
    let value = loop {
        app.process_dap_events();
        app.workspace.poll(&app.state);
        app.process_dap_project_root();
        app.process_execution_preflight();
        if let Ok(bytes) = fs::read(&log)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "Cargo test did not launch: {:?}",
            app.ui.notification
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        value["args"],
        serde_json::json!(["tests::works", "--exact", "--nocapture"])
    );
    assert!(Path::new(value["program"].as_str().unwrap()).is_file());
    app.debugger_session.stop().unwrap();
}

#[test]
fn stopped_install_does_not_start_after_completion() {
    let (_dirs, mut app) = app();
    source(&mut app, "demo.py", "print('demo')\n");
    trust(&mut app);
    adapter(
        &mut app,
        "debugpy",
        ToolStatus::Installing {
            phase: "fixture".into(),
            completed: 0,
            total: None,
        },
    );
    let launch = app.detected_debug_presets().remove(0);
    app.start_debug_preset(launch);
    app.terminate_debug_session();
    assert!(app.debugger_session.install_launch.is_none());
    adapter(
        &mut app,
        "debugpy",
        ToolStatus::Installed(crate::debugger::test_python()),
    );
    app.poll_debug_presets();
    assert!(app.debugger_session.launch.is_none());
}
