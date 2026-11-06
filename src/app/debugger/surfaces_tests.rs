use crate::app::tests::*;




#[test]
fn breakpoint_condition_prompt_preserves_its_original_target_and_other_settings() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("original.rs");
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("first\nsecond\n"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    let row = crate::debugger::DapSourceBreakpoint {
        path,
        line: 1,
        column: Some(2),
        condition: Some("old".into()),
        hit_condition: Some("3".into()),
        log_message: Some("value={value}".into()),
    };
    app.debugger_session.breakpoint(row.clone(), false).unwrap();
    app.process_effect(AppEffect::DapBreakpointConfigure {
        condition: None,
        hit_condition: None,
        log_message: None,
    });
    assert_eq!(app.input.prompt().unwrap().input, "old");
    // An asynchronous stop can navigate while the condition question is open.
    app.state.views[view].cursor = ByteOffset(6);
    let another = app.state.workspace_root.join("another.rs");
    app.state
        .open_text(TextBuffer::from_text("another"), Some(another))
        .unwrap();
    app.input.prompt_insert(" && ready");
    app.submit_prompt();
    let mut expected = row;
    expected.condition = Some("old && ready".into());
    assert_eq!(app.debugger_session.persisted_breakpoints().len(), 1);
    assert_eq!(
        app.debugger_session
            .breakpoint_at(&expected.path, expected.line),
        Some(&expected)
    );
}


#[test]
fn saved_watches_are_listed_without_a_debugger() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::DapWatchAdd("counter".into()));
    app.process_effect(AppEffect::DapWatchList);
    let picker = app.ui.picker.as_ref().unwrap();
    assert_eq!(picker.rows.len(), 1);
    assert_eq!(picker.rows[0].label, "counter");
    app.accept_picker();
    assert_eq!(app.debugger_session.watches(), ["counter"]);
}


#[test]
fn problems_logs_output_and_debug_keep_distinct_graph_surface_identities() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);

    app.open_builtin_picker(PickerKind::Diagnostics);
    app.reconcile_editor_windows();
    let problems = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Problems)
        .expect("problems surface");
    assert_eq!(
        app.window_session
            .graph()
            .content(problems.content)
            .expect("problems content")
            .content_type
            .as_str(),
        "builtin.problems"
    );

    app.show_output_picker("missing-task");
    app.reconcile_editor_windows();
    let output = app
        .window_session
        .native_ids()
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
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Logs)
        .expect("logs surface");
    assert_eq!(
        app.window_session
            .graph()
            .content(logs.content)
            .expect("logs content")
            .content_type
            .as_str(),
        "builtin.logs"
    );
    app.append_lsp_log("fake: live log".into());
    assert!(
        app.ui
            .picker
            .as_ref()
            .is_some_and(|picker| picker.rows.iter().any(|row| row.label == "fake: live log"))
    );
    app.picker_insert("fake");
    app.append_lsp_log("fake: second live log".into());
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.query == "fake"
            && picker
                .rows
                .iter()
                .any(|row| row.label == "fake: second live log")
    }));

    app.open_dap_picker("threads", Vec::new());
    app.reconcile_editor_windows();
    let debug = app
        .window_session
        .native_ids()
        .picker_for(PickerSurfaceKind::Debug)
        .expect("debug surface");
    assert_eq!(
        app.window_session
            .graph()
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
    assert!(app.window_session.graph().window(debug.window).is_none());
    assert!(app.ui.picker.is_none());

    app.close_picker();
    app.reconcile_editor_windows();
    for ids in [problems, output, logs] {
        assert!(app.window_session.graph().window(ids.window).is_some());
        assert!(
            app.compositor_projection()
                .expect("hidden retained surfaces")
                .window(ids.window)
                .is_none()
        );
    }
    let reconciliation_count = app.window_session.editor_ids().reconciliation_count();
    let idle_graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliation_count
    );
    assert_eq!(app.window_session.graph(), &idle_graph);
}


#[test]
fn breakpoint_gutter_signs_distinguish_disabled_breakpoints() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("source.rs");
    app.state
        .open_text(
            TextBuffer::from_text(&"source\n".repeat(15)),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.debugger_session
        .breakpoint(
            crate::debugger::DapSourceBreakpoint {
                path: path.clone(),
                line: 10,
                column: None,
                condition: Some("ready".into()),
                hit_condition: None,
                log_message: None,
            },
            false,
        )
        .unwrap();
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 9);
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::ConditionalBreakpoint
    );
    app.debugger_session
        .set_breakpoint_disabled(&path, 10, true);
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 9);
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::DisabledBreakpoint
    );
    app.debugger_session
        .set_breakpoint_disabled(&path, 10, false);
    app.project_debug_panes();
    assert_eq!(
        app.ui.debug_signs[0].kind,
        crate::ui::DebugSignKind::ConditionalBreakpoint
    );
}


#[cfg(unix)]
#[test]
fn debugger_protected_launch_source_capture_refusal_and_fresh_recovery() {
    use crate::app::tests::driver::TestSession;
    let files = tempfile::tempdir().unwrap();
    let adapter = files.path().join("adapter.py");
    let starts = files.path().join("starts");
    let code = format!(
        "import pathlib\npathlib.Path({}).open('a').write('x')\n{}",
        serde_json::to_string(&starts).unwrap(),
        crate::debugger::TEST_ADAPTER
    );
    std::fs::write(&adapter, code).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.debug.adapters.insert(
        "owned-adapter".into(),
        crate::config::DebugAdapterConfig {
            program: crate::debugger::test_python(),
            arguments: vec!["-u".into(), adapter.display().to_string()],
            environment: Default::default(),
        },
    );
    loaded.config.debug.sessions.insert(
        "owned".into(),
        crate::config::DebugSessionConfig {
            adapter: "owned-adapter".into(),
            request: crate::config::DebugRequestKind::Launch,
            cwd: None,
            program: Some("debuggee".into()),
            run_in_terminal_profile: None,
            arguments: Default::default(),
            path_maps: Default::default(),
        },
    );
    let replacement = loaded.clone();
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.debug_program_fixture();
    driver.trust_owned_terminal_fixture();
    driver.debug_request("owned");
    driver.debug_stop();
    assert!(!starts.exists(), "stop before launch started the adapter");
    let drain = |driver: &mut TestSession| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while driver.debug_preflights() != 0 {
            driver.pump_debug();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    drain(&mut driver);
    assert!(!starts.exists(), "stopped request launched adapter");
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(!starts.exists(), "retired preflight launched adapter");
    // Configured adapters may run scripts; exercise executable preflight through a native tool.
    driver.debug_start_native_program_fixture();
    let program_deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_program_pending() {
        driver.pump_debug();
        assert!(
            Instant::now() < program_deadline,
            "program preflight not admitted: {:?}",
            driver.projection().notification
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(
        !starts.exists(),
        "retired program preflight launched adapter"
    );
    driver.debug_start("owned");
    assert_eq!(driver.debug_preflights(), 1);
    driver.apply_candidate(replacement).unwrap();
    drain(&mut driver);
    assert!(
        !starts.exists(),
        "replacement configuration adopted old preflight"
    );
    driver.debug_start("owned");
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_text().is_none() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "stopped generated source did not open automatically"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.debug_source_text().as_deref(),
        Some("original adapter source\n")
    );
    driver.new_buffer();
    assert!(driver.debug_source_text().is_none());
    let picker = |driver: &mut TestSession, needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            driver.pump_debug();
            if driver
                .projection()
                .picker
                .as_ref()
                .is_some_and(|p| p.rows.iter().any(|row| row.label == needle))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "debug picker {needle}: {:?}",
                driver.projection().notification
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    driver.open_debug_threads_after_stop();
    picker(&mut driver, "original-thread");
    assert_eq!(std::fs::read_to_string(&starts).unwrap(), "x");
    assert!(
        driver.debug_program_wire_path(),
        "launch JSON lost original preflighted program"
    );
    driver.accept_debug_picker();
    picker(&mut driver, "original-frame");
    driver.accept_debug_picker();
    driver.open_command_prompt();
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_pending() {
        assert!(
            Instant::now() < deadline,
            "original source response not drained"
        );
        driver.pump_debug();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        driver.debug_source_text().is_none(),
        "late source stole command capture"
    );
    driver.close_prompt();
    driver.debug_threads();
    picker(&mut driver, "original-thread");
    driver.accept_debug_picker();
    picker(&mut driver, "original-frame");
    driver.accept_debug_picker();
    let deadline = Instant::now() + Duration::from_secs(4);
    while driver.debug_source_text().is_none() {
        driver.pump_debug();
        assert!(Instant::now() < deadline, "fresh source did not publish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        driver.debug_source_text().as_deref(),
        Some("original adapter source\n")
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_reverse_refusal() {
        driver.pump_debug();
        assert!(
            Instant::now() < deadline,
            "original reverse refusal did not reach owned adapter"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    driver.debug_stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while driver.debug_pending() != 0 {
        driver.pump_debug();
        assert!(Instant::now() < deadline, "owned adapter cleanup");
        std::thread::sleep(Duration::from_millis(2));
    }
}


pub(in crate::app) fn logging_adapter_fixture(
    request: crate::config::DebugRequestKind,
) -> (tempfile::TempDir, PathBuf, crate::app::tests::driver::TestSession) {
    let files = tempfile::tempdir().unwrap();
    let adapter = files.path().join("logging-adapter.py");
    // The adapter runs in the workspace, not in the fixture directory.
    let log_path = files.path().join("requests.log");
    let source = crate::debugger::TEST_LOGGING_ADAPTER
        .replace("'requests.log'", &serde_json::to_string(&log_path).unwrap());
    fs::write(&adapter, source).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.debug.adapters.insert(
        "logging".into(),
        crate::config::DebugAdapterConfig {
            program: crate::debugger::test_python(),
            arguments: vec!["-u".into(), adapter.display().to_string()],
            environment: Default::default(),
        },
    );
    let attach = matches!(request, crate::config::DebugRequestKind::Attach);
    loaded.config.debug.sessions.insert(
        if attach { "attached" } else { "logging" }.into(),
        crate::config::DebugSessionConfig {
            adapter: "logging".into(),
            request,
            cwd: None,
            program: (!attach).then(|| "debuggee".into()),
            run_in_terminal_profile: None,
            arguments: if attach {
                BTreeMap::from([("processId".into(), serde_json::json!(4242))])
            } else {
                BTreeMap::new()
            },
            path_maps: Default::default(),
        },
    );
    let mut driver = crate::app::tests::driver::TestSession::new(loaded);
    driver.new_buffer();
    if !attach {
        driver.debug_program_fixture();
    }
    driver.trust_owned_terminal_fixture();
    (files, log_path, driver)
}


#[test]
fn a_started_session_makes_the_debug_arrangement_appear_with_its_own_identity() {
    let (_files, _log_path, mut driver) =
        logging_adapter_fixture(crate::config::DebugRequestKind::Launch);

    assert!(
        driver.application().ui.debug_panes.is_none(),
        "no session, no arrangement: the panes are absent before anything starts"
    );

    driver.debug_start("logging");
    driver.open_debug_threads_after_stop();

    // (1) THE ARRANGEMENT APPEARED, projected on the debug pump, naming the session's OWN identity.
    let panes = driver
        .application()
        .ui
        .debug_panes
        .as_ref()
        .expect("starting a session makes the arrangement appear");
    assert!(
        panes.target.contains("logging"),
        "the panes name the adapter that is running: {}",
        panes.target
    );
    assert!(
        panes.target.contains("launched"),
        "a LAUNCH is named as a launch, not as an attachment: {}",
        panes.target
    );

    // (2) THE SELECTION IS SWITCHABLE by the command, which is what "selectable inspectors" means.
    let before = panes.selected;
    driver.application_mut().invoke_mapped(
        crate::editor::CommandInvocation::new(
            "dap.inspector.next",
            crate::editor::InvocationSource::Test,
        )
        .expect("static command"),
    );
    let after = driver
        .application()
        .ui
        .debug_panes
        .as_ref()
        .expect("the arrangement survives the switch")
        .selected;
    assert_ne!(before, after, "the command selects the NEXT inspector");

    // THE LAUNCH COUNTER-CASE: this session LAUNCHED, so its status must not claim attachment.
    driver.application_mut().invoke_mapped(
        crate::editor::CommandInvocation::new(
            "dap.session.status",
            crate::editor::InvocationSource::Test,
        )
        .expect("static command"),
    );
    let status = driver
        .application()
        .ui
        .notification
        .clone()
        .unwrap_or_default();
    assert!(
        status.contains("debug adapter: logging"),
        "the launch status names the adapter: {status:?}"
    );
    assert!(
        !status.contains("attached to"),
        "a LAUNCH must not claim to be attached to anything: {status:?}"
    );
}