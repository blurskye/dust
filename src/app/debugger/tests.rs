//! Debugger behavior and application integration tests.

use crate::app::AppEffect;
use crate::app::PickerKind;
use crate::app::debugger::*;
use crate::app::tests::*;
use crate::editor::EditorState;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::files::RootIdentity;
use crate::ui::PickerSurfaceKind;
use crate::workspace::WorkspaceId;
use crate::workspace::WorkspaceState;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[test]
fn breakpoint_toggle_uses_disabled_state_after_another_breakpoint_is_added() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("main.rs");
    app.state
        .open_text(TextBuffer::from_text("one\ntwo\n"), Some(path.clone()))
        .unwrap();
    let at = |line| crate::debugger::DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    app.toggle_breakpoint_disabled();
    assert!(app.debugger_session.persisted_breakpoints().is_empty());
    app.debugger_session.breakpoint(at(1), false).unwrap();
    app.toggle_breakpoint_disabled();
    app.debugger_session.breakpoint(at(2), false).unwrap();
    app.toggle_breakpoint_disabled();
    assert_eq!(
        app.debugger_session.breakpoint_disabled(&path, 1),
        Some(false)
    );
}

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
    let reconciliation_count = app.window_session.test_publications();
    let idle_graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.test_publications(), reconciliation_count);
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
    let buffer = app.state.active_buffer_id().unwrap();
    let revision = app.state.buffers[buffer].text.revision();
    app.state
        .apply_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1)).with_operation(
                crate::editor::EditorOperation::ApplyEdits {
                    buffer,
                    edits: crate::editor::text::TextEditSet::new(
                        revision,
                        vec![crate::editor::text::TextEdit::new(
                            crate::editor::text::ByteRange::empty(crate::editor::text::ByteOffset(
                                0,
                            )),
                            "new\n",
                        )],
                    ),
                },
            ),
        )
        .unwrap();
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 10);
    app.state
        .apply_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::Undo { buffer }),
        )
        .unwrap();
    app.project_debug_panes();
    assert_eq!(app.ui.debug_signs[0].line, 9);
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
    assert!(!starts.exists(), "cancelled preflight launched adapter");
    // Configured adapters may run scripts; exercise executable preflight through a native tool.
    driver.debug_start_native_program_fixture();
    let program_deadline = Instant::now() + Duration::from_secs(3);
    while !driver.debug_program_pending() {
        driver.pump_debug();
        assert!(
            Instant::now() < program_deadline,
            "program preflight not accepted: {:?}",
            driver.projection().notification
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(driver.debug_preflights(), 1);
    driver.debug_stop();
    drain(&mut driver);
    assert!(
        !starts.exists(),
        "cancelled program preflight launched adapter"
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
) -> (
    tempfile::TempDir,
    PathBuf,
    crate::app::tests::driver::TestSession,
) {
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

#[test]
fn debugger_owner_keeps_original_actor_stop_context_and_private_selection() {
    crate::app::debugger::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = Debugger::new(&DebugConfig::default(), workspace.version());
    owner.add_watch("value".into());
    assert_eq!(owner.watches(), ["value"]);
    let row = DapSourceBreakpoint {
        path: root.path().join("a"),
        line: 1,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    owner.breakpoint(row, false).unwrap();
    assert_eq!(owner.persisted_breakpoints().len(), 1);
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 1,
            request_id: 0,
            request: None,
            response: crate::debugger::DapResponse::Threads(Vec::new()),
        },
    );
    assert!(owner.try_event().is_some());
    let keys = owner.controls(vec![("thread".into(), String::new(), DapControl::Threads)]);
    let editor = EditorState::new(root.path().into());
    let windows = crate::app::windows::Windows::new(false, 10);
    let target = || SourceTarget {
        navigation: windows.navigation_stamp(editor.window_version(), editor.active_view()),
        capture: Arc::new(()),
        line: 1,
        column: 1,
    };
    let selection = owner.take_selection(&keys[0].key).unwrap();
    owner.activate(selection, target()).unwrap();
    assert!(matches!(adapter.control(), Some(DapControl::Threads)));
    let pending = owner.controls(vec![("pending".into(), String::new(), DapControl::Threads)]);
    let pending = owner.take_selection(&pending[0].key).unwrap();
    adapter.advance(2);
    assert!(matches!(
        owner.activate(pending, target()),
        Err(DapError::StaleContext)
    ));
    assert!(matches!(
        owner.take_selection(&keys[0].key),
        Err(DapError::StaleContext)
    ));
    assert!(adapter.control().is_none());
    // An event drained at stop 2 cannot be relabelled with stop 3 during row building.
    adapter.event(
        2,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 2,
            request_id: 0,
            request: None,
            response: crate::debugger::DapResponse::Threads(Vec::new()),
        },
    );
    assert!(owner.try_event().is_some());
    adapter.advance(3);
    assert!(
        owner
            .controls(vec![("late".into(), String::new(), DapControl::Threads)])
            .is_empty()
    );
    let old = keys[0].key.clone();
    owner.stop().unwrap();
    let (replacement, new_adapter) = crate::debugger::test_session();
    owner.service = replacement;
    assert!(owner.take_selection(&old).is_err());
    assert!(new_adapter.control().is_none());
    let new = owner.controls(vec![("fresh".into(), String::new(), DapControl::Threads)]);
    let selection = owner.take_selection(&new[0].key).unwrap();
    owner.activate(selection, target()).unwrap();
    assert!(matches!(new_adapter.control(), Some(DapControl::Threads)));
    let frame = DapStackFrame {
        id: 1,
        name: "source".into(),
        path: None,
        line: 1,
        column: 1,
        source_reference: Some(7),
    };
    let frames = owner.frames(vec![frame]);
    let selection = owner.take_selection(&frames[0].key).unwrap();
    owner.activate(selection, target()).unwrap();
    assert!(matches!(
        new_adapter.control(),
        Some(DapControl::Source { reference: 7 })
    ));
    assert!(
        owner.take_selection(&frames[0].key).is_err(),
        "accepted choice was not consumed"
    );
    new_adapter.advance(1);
    assert!(owner.take_source().is_none());
    let closed = owner.controls(vec![("closed".into(), String::new(), DapControl::Threads)]);
    owner.dismiss_choices();
    assert!(owner.take_selection(&closed[0].key).is_err());
    let mut terminals = TerminalsConfig::default();
    terminals.profiles.get_mut("shell").unwrap().program = "/bin/sh".into();
    owner.terminal_profile = Some(TerminalPolicy {
        name: "shell".into(),
        profile: terminals.profiles["shell"].clone(),
        cwd: root.path().into(),
    });
    let args = vec!["/bin/sh".into()];
    assert_eq!(
        owner
            .terminal_request(root.path(), &args, &HashMap::new(), &terminals)
            .unwrap(),
        "shell"
    );
    terminals
        .profiles
        .get_mut("shell")
        .unwrap()
        .arguments
        .push("replacement".into());
    assert!(
        owner
            .terminal_request(root.path(), &args, &HashMap::new(), &terminals)
            .is_err()
    );
    let current = owner.service.context().unwrap();
    owner.service.control(DapControl::Stop).unwrap();
    assert!(
        !owner.service.current(&current),
        "cancelled actor still offered source authority"
    );
    assert!(
        owner
            .controls(vec![("late".into(), String::new(), DapControl::Threads)])
            .is_empty()
    );
    owner.begin_shutdown();
    assert!(
        owner
            .service
            .start_absolute(crate::debugger::DapLaunch {
                adapter: "same-name".into(),
                executable: "/bin/sh".into(),
                arguments: vec![],
                environment: HashMap::new(),
                path_maps: BTreeMap::new(),
                root: root.path().into(),
                request: "launch".into(),
                request_arguments: serde_json::json!({}),
                breakpoints: Vec::new(),
            })
            .is_err()
    );
}

#[test]
fn cancelled_debugger_terminal_preflight_keeps_its_rejected_owner() {
    let (mut owner, adapter) = request_fixture();
    let id = PtyId(1);
    assert!(
        owner.terminal_current(&id),
        "ordinary terminal has no debugger owner"
    );
    adapter.with_terminal_reply(|reply| {
        owner.register_terminal(id, reply.clone());
        assert!(owner.terminal_current(&id));
        owner.stop().unwrap();
        assert!(
            !owner.terminal_current(&id),
            "cancelled preflight must not launch"
        );
        assert!(!reply.current());
        assert!(owner.take_terminal(&id).is_some());
        assert!(owner.terminal_replies.is_empty());
    });
}

#[test]
fn breakpoint_states_are_session_state_driven_by_the_adapters_own_answer() {
    use crate::debugger::DapBreakpointState;
    use crate::debugger::DapResponse;
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = Debugger::new(&policy, workspace.version());
    let path = root.path().join("main.rs");
    let at = |line: u64| DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };

    // SENT BUT UNANSWERED: the breakpoint is a REQUEST, not a fact.
    owner.breakpoint(at(10), false).unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Requested { accepted: true }),
        "a breakpoint the adapter has not answered for is only a request"
    );
    assert!(
        !owner
            .breakpoint_state(&path, 10)
            .expect("a state")
            .is_verified(),
        "and it is NEVER verified before an answer arrives"
    );

    // THE ADAPTER'S ANSWER verifies exactly what it verified.
    owner.apply_breakpoints(
        &path,
        &[at(10)],
        &DapResponse::Breakpoints(vec![(true, Some(11), None)]),
    );
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Verified { line: 11 }),
        "verified at the ADAPTER's line, which it may have moved"
    );

    // AN ANSWER THAT DECLINES: still not verified, and the state says which case it is.
    owner.breakpoint(at(20), false).unwrap();
    owner.apply_breakpoints(
        &path,
        &[at(10), at(20)],
        &DapResponse::Breakpoints(vec![(true, Some(20), None), (false, None, None)]),
    );
    // The queue holds one pending request per send, so the answer above applied to line 20's
    // request and line 20 was NOT verified.
    let declined = owner
        .breakpoint_state(&path, 20)
        .expect("line 20 has a state");
    assert!(
        !declined.is_verified(),
        "an adapter that did not verify a breakpoint must not be shown as verifying it: {declined:?}"
    );

    // DISABLING IS SESSION STATE, and it survives an adapter answer that says otherwise.
    owner.set_breakpoint_disabled(&path, 10, true);
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Disabled)
    );
    owner.apply_breakpoints(
        &path,
        &[at(10)],
        &DapResponse::Breakpoints(vec![(true, Some(10), None)]),
    );
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Disabled),
        "a disabled breakpoint stays disabled even when the adapter verified it"
    );

    // Editing another breakpoint must not change disabled state or the next toggle's meaning.
    owner.breakpoint(at(30), false).unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Disabled)
    );
    assert!(
        !owner.startup_breakpoints()[0]
            .1
            .iter()
            .any(|row| row.line == 10)
    );
    assert_eq!(owner.breakpoint_disabled(&path, 10), Some(true));
    owner.breakpoint(at(30), true).unwrap();

    // An empty line cannot retain disabled state for a future breakpoint.
    owner.set_breakpoint_disabled(&path, 40, true);
    assert_eq!(owner.breakpoint_state(&path, 40), None);
    owner.breakpoint(at(40), false).unwrap();
    assert_eq!(owner.breakpoint_disabled(&path, 40), Some(false));
    owner.breakpoint(at(40), true).unwrap();

    // RE-ENABLING makes it a request again rather than silently verified.
    let disabled = owner.breakpoint_state(&path, 10) == Some(DapBreakpointState::Disabled);
    owner.set_breakpoint_disabled(&path, 10, !disabled);
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Requested { accepted: true }),
        "a re-enabled breakpoint is a request until the adapter answers for it again"
    );

    // REMOVAL LEAVES NO STALE VERDICT: a line that no longer has a breakpoint claims nothing.
    owner.breakpoint(at(20), true).unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 20),
        None,
        "a removed breakpoint keeps no verdict at all"
    );
    owner.breakpoint(at(10), true).unwrap();
    assert_eq!(owner.breakpoint_state(&path, 10), None);
}

#[test]
fn detaching_and_terminating_are_different_session_facts() {
    use crate::debugger::DapSessionEnd;
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();

    // DETACH: the session reports that it LEFT, and that it did not ask for the program to stop.
    let mut detached = Debugger::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    detached.service = service;
    detached.execution = Some((root.path().join("main.rs"), 2));
    detached.stop_reason = Some("breakpoint".into());
    detached.variables.push("value = 1".into());
    detached.detach_debuggee().expect("detach is accepted");
    assert_eq!(detached.end_state(), None);
    let request = adapter.control().expect("detach sends a control");
    let DapControl::Disconnect { terminate_debuggee } = request else {
        panic!("detach must send Disconnect");
    };
    assert!(!terminate_debuggee, "detach leaves the program running");
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 1,
            request: Some(request),
            response: DapResponse::Disconnected { terminate_debuggee },
        },
    );
    detached.try_event().unwrap();
    assert!(
        detached.active_adapter().is_none(),
        "disconnect stops the service"
    );
    assert!(detached.execution.is_none());
    assert!(detached.stop_reason.is_none());
    assert!(detached.variables.is_empty());
    assert_eq!(
        detached.end_state(),
        Some(DapSessionEnd::Detached),
        "detaching is leaving, not killing"
    );

    // TERMINATE: a DIFFERENT reported fact.
    let mut terminated = Debugger::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    terminated.service = service;
    terminated.execution = Some((root.path().join("main.rs"), 2));
    terminated.stop_reason = Some("breakpoint".into());
    terminated.variables.push("value = 1".into());
    terminated
        .terminate_debuggee()
        .expect("terminate is accepted");
    assert_eq!(terminated.end_state(), None);
    let request = adapter.control().expect("terminate sends a control");
    let DapControl::Disconnect { terminate_debuggee } = request else {
        panic!("terminate must send Disconnect");
    };
    assert!(terminate_debuggee, "terminate asks the program to end");
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 1,
            request: Some(request),
            response: DapResponse::Disconnected { terminate_debuggee },
        },
    );
    terminated.try_event().unwrap();
    assert!(
        terminated.active_adapter().is_none(),
        "disconnect stops the service"
    );
    assert!(terminated.execution.is_none());
    assert!(terminated.stop_reason.is_none());
    assert!(terminated.variables.is_empty());
    assert_eq!(
        terminated.end_state(),
        Some(DapSessionEnd::Terminated),
        "terminating asks for the program to end"
    );
    assert_ne!(
        detached.end_state(),
        terminated.end_state(),
        "the two are distinguished by what the session reports, not by the command name"
    );
}

#[test]
fn output_chunks_join_partial_lines_and_preserve_blank_lines() {
    let (mut owner, adapter) = request_fixture();
    for output in ["hel", "lo\n\n", " \n", "tail\r", "\n"] {
        adapter.event(
            0,
            DapEvent::Output {
                adapter: "same-name".into(),
                output: output.into(),
            },
        );
        assert!(owner.try_event().is_some());
    }
    assert_eq!(
        owner
            .console()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        ["hello", "", " ", "tail"]
    );
}

#[test]
fn inspected_values_do_not_consume_console_submissions() {
    let (mut owner, _) = request_fixture();
    owner.record_console_command("slow()".into());
    owner.record_inspected_value("leaf = 7".into());
    assert_eq!(owner.pending_submissions.len(), 1);
    assert_eq!(owner.console().last().unwrap().submission, None);
    owner.record_console_evaluation("answer".into());
    assert_eq!(
        owner.console().next().unwrap().result.as_deref(),
        Some("answer")
    );
    assert!(owner.pending_submissions.is_empty());
}

#[test]
fn the_console_keeps_program_output_evaluation_and_typed_commands_apart() {
    use crate::debugger::DapConsoleOrigin;
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = Debugger::new(&policy, workspace.version());

    // The program writes, the user asks, the adapter answers - in that order.
    owner.record_console_command("count".into());
    owner.record_console_evaluation("count = 3".into());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    adapter.event(
        0,
        DapEvent::Output {
            adapter: "same-name".into(),
            output: "hello\n".into(),
        },
    );
    assert!(matches!(owner.try_event(), Some(DapEvent::Output { .. })));

    let entries = owner.console().cloned().collect::<Vec<_>>();
    assert_eq!(entries.len(), 3, "all three origins are kept: {entries:?}");
    assert_eq!(
        entries.iter().map(|entry| entry.origin).collect::<Vec<_>>(),
        vec![
            DapConsoleOrigin::Command,
            DapConsoleOrigin::Evaluation,
            DapConsoleOrigin::ProgramOutput,
        ],
        "each entry carries its ORIGIN, and the order they happened is preserved"
    );
    // The separation the clause asks for, asserted in both directions.
    assert_ne!(
        entries[1].origin, entries[2].origin,
        "an evaluation result is not program output"
    );
    assert_ne!(
        entries[0].origin, entries[1].origin,
        "and the command the user typed is neither of them"
    );
    assert_eq!(entries[2].text, "hello", "the program's own text is intact");
    assert_eq!(entries[1].text, "count = 3");
    assert_eq!(
        entries[2].origin.label(),
        "program output",
        "and a surface can name what it is showing"
    );
    assert_eq!(entries[1].origin.label(), "evaluation result");
    assert_eq!(entries[0].origin.label(), "console command");
}

#[test]
fn a_value_from_a_previous_stop_can_never_be_presented_as_current() {
    let (mut owner, adapter) = request_fixture();
    for value in ["1", "2"] {
        owner.begin_stop();
        owner.stop_follow = StopFollow::Variables;
        owner.follow_control(DapControl::Variables {
            reference: 7,
            start: 0,
            count: 10,
        });
        let request_id = owner.follow_request.unwrap();
        let request = adapter.control().unwrap();
        assert!(matches!(
            answer(
                &mut owner,
                &adapter,
                request_id,
                request,
                DapResponse::Variables(vec![crate::debugger::DapVariable {
                    name: "count".into(),
                    value: value.into(),
                    type_name: None,
                    variables_reference: 42,
                }])
            ),
            DebugResponse::Absorbed
        ));
        assert_eq!(owner.variables(), [format!("count = {value}")]);
        owner.begin_running();
        assert!(owner.variables().is_empty());
        owner.begin_stop();
        assert!(owner.variables().is_empty());
    }
}

#[test]
fn running_and_repeated_stops_clear_inspector_values_and_picker_choices() {
    let (mut owner, adapter) = paginated_owner();
    for (value, running) in [("1", true), ("2", false)] {
        adapter.event(
            1,
            DapEvent::Stopped {
                adapter: "same-name".into(),
                reason: "breakpoint".into(),
                thread_id: Some(1),
            },
        );
        assert!(owner.try_event().is_some());
        let epoch = owner.stop_epoch();
        // Follow the stop through the same responses that populate the inspector.
        for response in [
            DapResponse::Threads(vec![crate::debugger::DapThread {
                id: 1,
                name: "main".into(),
            }]),
            DapResponse::Stack(vec![DapStackFrame {
                id: 42,
                name: "main".into(),
                path: None,
                line: 1,
                column: 1,
                source_reference: None,
            }]),
            DapResponse::Scopes(vec![crate::debugger::DapScope {
                name: "locals".into(),
                variables_reference: 7,
                expensive: false,
            }]),
            DapResponse::Variables(vec![crate::debugger::DapVariable {
                name: "count".into(),
                value: value.into(),
                type_name: None,
                variables_reference: 0,
            }]),
        ] {
            let request_id = owner.follow_request.unwrap();
            let request = if request_id == 0 {
                DapControl::Threads
            } else {
                adapter.control().unwrap()
            };
            assert!(matches!(
                answer(&mut owner, &adapter, request_id, request, response),
                DebugResponse::Absorbed
            ));
        }
        assert_eq!(owner.variables(), [format!("count = {value}")]);

        // The variables picker also offers the current value and a usable selection.
        prime_for(&mut owner, &adapter);
        let DebugResponse::Picker { rows, .. } =
            owner.response(DapResponse::Variables(vec![crate::debugger::DapVariable {
                name: "count".into(),
                value: value.into(),
                type_name: None,
                variables_reference: 0,
            }]))
        else {
            panic!("expected variables picker");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, format!("count = {value}"));
        let key = rows[0].key.clone();
        assert!(owner.choices.is_some());

        adapter.event(
            1,
            if running {
                DapEvent::Running {
                    adapter: "same-name".into(),
                }
            } else {
                DapEvent::Stopped {
                    adapter: "same-name".into(),
                    reason: "step".into(),
                    thread_id: Some(1),
                }
            },
        );
        assert!(owner.try_event().is_some());
        assert_ne!(owner.stop_epoch(), epoch);
        assert!(
            owner.variables().is_empty(),
            "old locals must leave the inspector"
        );
        assert!(owner.choices.is_none(), "old picker rows must be cleared");
        assert!(owner.take_selection(&key).is_err());
    }
}

#[test]
fn a_variables_answer_to_an_earlier_stop_is_refused_rather_than_presented_as_current() {
    crate::app::debugger::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = Debugger::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;

    let variable = |value: &str| crate::debugger::DapVariable {
        name: "count".into(),
        value: value.into(),
        type_name: None,
        variables_reference: 7,
    };
    let request = || DapControl::Variables {
        reference: 7,
        start: 0,
        count: 1000,
    };

    // (ii) SAME STOP, through the REAL issue path: the request is recorded as it goes out, so its
    // answer belongs to this stop, and the value IS presentable.
    owner.begin_stop();
    owner
        .control(request())
        .expect("the fake adapter accepts a variables request");
    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("1")]));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a stale or bare result")
    };
    assert_eq!(rows.len(), 1, "the current answer IS presented");
    assert_eq!(rows[0].label, "count = 1");
    let old_key = rows[0].key.clone();

    // (i) EARLIER STOP: the request is issued at one stop, execution RUNS ON, and only then does
    // the answer arrive. It answers the earlier stop, so it can never be presented as current.
    owner.begin_stop();
    owner
        .control(request())
        .expect("the fake adapter accepts a variables request");
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 1,
            request_id: 2,
            request: Some(request()),
            response: DapResponse::Variables(vec![variable("1")]),
        },
    );
    owner.begin_running();
    adapter.advance(2);
    assert!(
        owner.try_event().is_none(),
        "the transport invalidates the original stopped request"
    );
    assert!(owner.variables().is_empty());
    assert!(matches!(
        owner.take_selection(&old_key),
        Err(DapError::StaleContext)
    ));

    // A RE-STOP then answers for the NEW stop, and that value IS current.
    owner.begin_stop();
    owner
        .control(request())
        .expect("the fake adapter accepts a variables request");
    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("2")]));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a stale or bare result")
    };
    assert_eq!(rows.len(), 1, "the fresh answer is presented");
    assert_eq!(rows[0].label, "count = 2");
}

#[test]
fn a_late_result_updates_its_own_record_and_leaves_a_newer_draft_untouched() {
    let (mut owner, adapter) = paginated_owner();
    let stop = owner.stop_epoch();
    prime_for(&mut owner, &adapter);
    let DebugResponse::Picker { rows, .. } =
        owner.response(DapResponse::Stack(vec![DapStackFrame {
            id: 42,
            name: "main".into(),
            path: None,
            line: 1,
            column: 1,
            source_reference: None,
        }]))
    else {
        panic!("expected a frames picker");
    };
    assert_eq!(rows.len(), 1, "the frame is offered");
    owner
        .take_selection(&rows[0].key)
        .expect("the frame is selectable");

    owner.record_console_command("first".into());
    owner.record_console_command("second".into());
    owner.record_console_evaluation("first = 1".into());
    let entries = owner.console().collect::<Vec<_>>();
    let first = entries.iter().find(|entry| entry.text == "first").unwrap();
    let second = entries.iter().find(|entry| entry.text == "second").unwrap();
    let answer = entries
        .iter()
        .find(|entry| entry.text == "first = 1")
        .unwrap();
    assert_eq!(first.actor.as_deref(), Some("same-name"));
    assert_eq!(first.stop, Some(stop));
    assert_eq!(first.frame, Some(42));
    assert!(first.submission.is_some());
    assert_ne!(first.submission, second.submission);
    assert_eq!(first.result.as_deref(), Some("first = 1"));
    assert_eq!(answer.submission, first.submission);
    assert_eq!(answer.frame, Some(42));
    assert_eq!(second.result, None, "the newer draft is untouched");
    assert_eq!(owner.pending_submissions.len(), 1);
}

pub(in crate::app) fn attach_adapter(owner: &mut Debugger) -> crate::debugger::TestAdapter {
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    prime_for(owner, &adapter);
    adapter
}

fn paginated_owner() -> (Debugger, crate::debugger::TestAdapter) {
    crate::app::debugger::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = Debugger::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    owner.begin_stop();
    (owner, adapter)
}

/// Prime the actor: an answer is only accepted while one is open, and presenting CONSUMES it.
fn prime_for(owner: &mut Debugger, adapter: &crate::debugger::TestAdapter) {
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 1,
            request_id: 0,
            request: None,
            response: crate::debugger::DapResponse::Threads(Vec::new()),
        },
    );
    assert!(
        owner.try_event().is_some(),
        "the session accepts an event from its actor"
    );
}

fn leaf_variables(count: u64) -> Vec<crate::debugger::DapVariable> {
    (0..count)
        .map(|i| crate::debugger::DapVariable {
            name: format!("v{i}"),
            value: format!("{i}"),
            type_name: None,
            variables_reference: 0,
        })
        .collect()
}

#[test]
fn inspecting_a_leaf_leaves_pending_console_submissions_for_their_answers() {
    let (mut owner, adapter) = paginated_owner();
    prime_for(&mut owner, &adapter);
    let DebugResponse::Picker { rows, .. } =
        owner.response(DapResponse::Variables(leaf_variables(1)))
    else {
        panic!("variables picker");
    };
    owner.record_console_command("pending expression".into());
    let selection = owner.take_selection(&rows[0].key).unwrap();
    owner.record_inspected_value(selection.value().unwrap().to_owned());
    assert_eq!(owner.pending_submissions.len(), 1);
    let inspected = owner.console().last().unwrap();
    assert_eq!(inspected.text, "v0 = 0");
    assert_eq!(inspected.submission, None);
    owner.record_console_evaluation("answer".into());
    assert!(owner.pending_submissions.is_empty());
    let command = owner
        .console()
        .find(|entry| entry.text == "pending expression")
        .unwrap();
    assert_eq!(command.result.as_deref(), Some("answer"));
}

#[test]
fn a_full_page_of_variables_offers_an_exact_continuation_that_carries_the_next_offset() {
    let (mut owner, adapter) = paginated_owner();
    let page = crate::app::debugger::VARIABLE_PAGE;
    owner
        .control(DapControl::Variables {
            reference: 7,
            start: 0,
            count: page,
        })
        .unwrap();
    // The FIRST page is observed at the ADAPTER, not taken on the session's word.
    assert!(
        matches!(
            adapter.control(),
            Some(DapControl::Variables { reference: 7, start: 0, count }) if count == page
        ),
        "the first bounded page is what actually goes out"
    );

    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(leaf_variables(
        page,
    )));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker");
    };
    assert_eq!(
        rows.len() as u64,
        page + 1,
        "a FULL page adds exactly one explicit continuation"
    );
    let continuation = rows.last().unwrap();
    assert!(
        continuation
            .detail
            .contains(&format!("continue from {page}")),
        "the continuation carries the exact offset: {}",
        continuation.detail
    );

    let selection = owner.take_selection(&continuation.key).unwrap();
    let Choice::Control(control) = selection.choice else {
        panic!("the continuation is a control");
    };
    assert!(
        matches!(
            control,
            DapControl::Variables { reference: 7, start, count }
                if start == page && count == page
        ),
        "the continuation asks for the NEXT page by offset, not the same one again"
    );
    owner.control(control).unwrap();
    // And the offset is what the ADAPTER actually received.
    assert!(
        matches!(
            adapter.control(),
            Some(DapControl::Variables { reference: 7, start, .. }) if start == page
        ),
        "the adapter observed the advanced offset"
    );

    // A page that is NOT full offers no continuation: nothing is left to ask for.
    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(leaf_variables(3)));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker");
    };
    assert_eq!(rows.len(), 3, "a short page is the whole answer");
}

#[test]
fn pagination_at_the_expansion_limit_does_not_open_another_reference() {
    let (mut owner, adapter) = paginated_owner();
    let mut continuation = None;
    for reference in 1..=MAX_EXPANSION_DEPTH as i64 {
        let request = DapControl::Variables {
            reference,
            start: 0,
            count: 1,
        };
        owner.control(request.clone()).unwrap();
        assert!(adapter.control().is_some());
        adapter.event(
            1,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_id: 1,
                request_id: reference as u64,
                request: Some(request),
                response: DapResponse::Variables(leaf_variables(1)),
            },
        );
        let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
            panic!()
        };
        let DebugResponse::Picker { rows, .. } = owner.response(response) else {
            panic!("variables picker");
        };
        continuation = Some(rows.last().unwrap().key.clone());
    }
    let selection = owner.take_selection(&continuation.unwrap()).unwrap();
    let Choice::Control(control) = selection.choice else {
        panic!("continuation")
    };
    owner.control(control).unwrap();
    assert!(matches!(
        adapter.control(),
        Some(DapControl::Variables { start: 1, .. })
    ));
    assert_eq!(owner.expansion.len(), MAX_EXPANSION_DEPTH);
    owner
        .control(DapControl::Variables {
            reference: 100,
            start: 0,
            count: 1,
        })
        .unwrap();
    assert!(adapter.control().is_none());
}

#[test]
fn a_reference_already_open_is_shown_as_a_reference_and_is_never_requested_again() {
    let (mut owner, adapter) = paginated_owner();
    owner
        .control(DapControl::Variables {
            reference: 7,
            start: 0,
            count: crate::app::debugger::VARIABLE_PAGE,
        })
        .unwrap();
    assert!(adapter.control().is_some(), "the first expansion goes out");

    // The adapter answers with a child that points back at the node already open: a CYCLE.
    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![
        crate::debugger::DapVariable {
            name: "self".into(),
            value: "…".into(),
            type_name: Some("Node".into()),
            variables_reference: 7,
        },
    ]));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker");
    };
    assert_eq!(rows.len(), 1, "the cycle is shown as one reference");
    assert!(
        rows[0].detail.contains("already open"),
        "it is shown AS a reference and says so: {}",
        rows[0].detail
    );

    assert!(
        adapter.control().is_none(),
        "nothing else was sent in between, so the check below is unambiguous"
    );
    let selection = owner.take_selection(&rows[0].key).unwrap();
    let Choice::Control(control) = selection.choice else {
        panic!("an expandable reference is a control");
    };
    owner.control(control).unwrap();
    assert!(
        adapter.control().is_none(),
        "NOTHING is requested for a reference already open, so it cannot recurse"
    );
    assert!(
        owner
            .console()
            .any(|entry| entry.text.contains("already open on this path")),
        "the refusal is visible rather than silent"
    );
}

#[test]
fn the_left_inspector_retains_threads_and_a_stack_that_running_on_invalidates() {
    let (mut owner, adapter) = paginated_owner();
    owner.control(DapControl::Threads).unwrap();
    assert!(matches!(adapter.control(), Some(DapControl::Threads)));
    prime_for(&mut owner, &adapter);
    owner.response(crate::debugger::DapResponse::Threads(vec![
        crate::debugger::DapThread {
            id: 1,
            name: "main".into(),
        },
    ]));
    assert_eq!(
        owner.threads(),
        ["main (1)"],
        "the threads the adapter reported are RETAINED for the inspector"
    );

    owner
        .control(DapControl::StackTrace {
            thread_id: 1,
            start: 0,
            levels: 256,
        })
        .unwrap();
    assert!(matches!(
        adapter.control(),
        Some(DapControl::StackTrace { .. })
    ));
    prime_for(&mut owner, &adapter);
    owner.response(crate::debugger::DapResponse::Stack(vec![
        crate::debugger::DapStackFrame {
            id: 9,
            name: "main".into(),
            path: None,
            line: 1,
            column: 1,
            source_reference: None,
        },
    ]));
    assert_eq!(owner.stack(), ["main:1"], "the stack is retained too");

    // RUNNING ON: the stack belonged to a stop that has ended, so it is NOT presented as current.
    owner.begin_running();
    assert!(
        owner.stack().is_empty(),
        "a stack from a previous stop must never be presented as the current one"
    );
    assert!(
        owner.threads().is_empty(),
        "and the previous stop's thread list is invalidated with it"
    );
}

fn request_fixture() -> (Debugger, crate::debugger::TestAdapter) {
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = Debugger::new(&DebugConfig::default(), workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    (owner, adapter)
}
fn answer(
    owner: &mut Debugger,
    adapter: &crate::debugger::TestAdapter,
    request_id: u64,
    request: DapControl,
    response: DapResponse,
) -> DebugResponse {
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id,
            request: Some(request),
            response,
        },
    );
    let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
        panic!("expected response");
    };
    owner.response(response)
}
fn evaluation(value: &str) -> DapResponse {
    DapResponse::Evaluate(crate::debugger::DapVariable {
        name: "result".into(),
        value: value.into(),
        type_name: None,
        variables_reference: 0,
    })
}
#[test]
fn evaluation_answers_follow_the_original_watch_and_console_submission() {
    let (mut owner, adapter) = request_fixture();
    owner.add_watch("a".into());
    owner.add_watch("b".into());
    let watch = |expression: &str| DapControl::Evaluate {
        expression: expression.into(),
        frame_id: None,
        context: "watch".into(),
    };
    owner.control(watch("a")).unwrap();
    owner.control(watch("b")).unwrap();
    for (id, expression, value) in [(2, "b", "B"), (1, "a", "A")] {
        adapter.event(
            0,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_id: 0,
                request_id: id,
                request: Some(watch(expression)),
                response: evaluation(value),
            },
        );
        let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
            panic!()
        };
        assert!(matches!(owner.response(response), DebugResponse::Absorbed));
    }
    assert_eq!(owner.watch_values.get("a").unwrap(), "A");
    assert_eq!(owner.watch_values.get("b").unwrap(), "B");
    let repl = |expression: &str| DapControl::Evaluate {
        expression: expression.into(),
        frame_id: None,
        context: "repl".into(),
    };
    owner.record_console_command("bad".into());
    owner.control(repl("bad")).unwrap();
    adapter.event(
        0,
        DapEvent::RequestFailed {
            adapter: "same-name".into(),
            stop_id: 0,
            request_seq: 9,
            request_id: 3,
            request: Some(repl("bad")),
            command: "evaluate".into(),
            message: "rejected".into(),
        },
    );
    owner.try_event().unwrap();
    assert!(!owner.watch_request_failed("evaluate"));
    assert!(owner.pending_submissions.is_empty());
    owner.record_console_command("good".into());
    owner.control(repl("good")).unwrap();
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 4,
            request: Some(repl("good")),
            response: evaluation("ok"),
        },
    );
    let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
        panic!()
    };
    assert!(matches!(owner.response(response), DebugResponse::Result(_)));
    owner.record_console_evaluation("ok".into());
    assert!(
        owner
            .console
            .iter()
            .find(|entry| entry.text == "bad")
            .unwrap()
            .result
            .is_none()
    );
    assert_eq!(
        owner
            .console
            .iter()
            .find(|entry| entry.text == "good")
            .unwrap()
            .result
            .as_deref(),
        Some("ok")
    );
    owner.stop().unwrap();
    assert!(owner.pending_submissions.is_empty());
    owner.record_console_command("no session".into());
    assert!(owner.control(repl("no session")).is_err());
    assert!(owner.pending_submissions.is_empty());
}
#[test]
fn stopped_request_bookkeeping_is_cleared_on_continue_and_failure() {
    let (mut owner, _) = request_fixture();
    owner.note_issued(true, Some(3), 0);
    owner.begin_running();
    assert!(owner.issued_stack.is_empty() && owner.issued_scopes.is_empty());
    owner.note_issued(true, Some(3), owner.stop_epoch);
    owner.prepare_answer(
        1,
        &DapControl::StackTrace {
            thread_id: 1,
            start: 0,
            levels: 20,
        },
        true,
    );
    owner.prepare_answer(2, &DapControl::Scopes { frame_id: 3 }, true);
    assert!(owner.issued_stack.is_empty() && owner.issued_scopes.is_empty());
}
#[test]
fn variable_pages_belong_to_the_request_and_picker_activation_keeps_expansion() {
    let (mut owner, adapter) = request_fixture();
    let request = DapControl::Variables {
        reference: 8,
        start: 0,
        count: 1,
    };
    owner.control(request.clone()).unwrap();
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 1,
            request: Some(request),
            response: DapResponse::Variables(vec![crate::debugger::DapVariable {
                name: "current".into(),
                value: "value".into(),
                type_name: None,
                variables_reference: 7,
            }]),
        },
    );
    let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
        panic!()
    };
    let DebugResponse::Picker { rows, .. } = owner.response(response) else {
        panic!()
    };
    assert_eq!(rows.len(), 2, "current child and load-more row");
    let selection = owner.take_selection(&rows[0].key).unwrap();
    owner.dismiss_choices();
    let editor = EditorState::new("/fixture".into());
    let windows = crate::app::windows::Windows::new(false, 10);
    owner
        .activate(
            selection,
            SourceTarget {
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view()),
                capture: Arc::new(()),
                line: 1,
                column: 1,
            },
        )
        .unwrap();
    assert_eq!(owner.expansion, vec![8, 7]);
    assert_eq!(owner.pending_variable_pages.back().unwrap().0, 7);
}
#[test]
fn accepting_a_leaf_does_not_reopen_its_closed_expansion() {
    let (mut owner, adapter) = request_fixture();
    let request = DapControl::Variables {
        reference: 7,
        start: 0,
        count: 10,
    };
    owner.control(request.clone()).unwrap();
    assert!(adapter.control().is_some());
    let DebugResponse::Picker { rows, .. } = answer(
        &mut owner,
        &adapter,
        1,
        request.clone(),
        DapResponse::Variables(leaf_variables(1)),
    ) else {
        panic!("variables picker")
    };
    let selection = owner.take_selection(&rows[0].key).unwrap();
    assert!(selection.value().is_some());
    owner.dismiss_choices();
    let editor = EditorState::new("/fixture".into());
    let windows = crate::app::windows::Windows::new(false, 10);
    owner
        .activate(
            selection,
            SourceTarget {
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view()),
                capture: Arc::new(()),
                line: 1,
                column: 1,
            },
        )
        .unwrap();
    assert!(owner.expansion.is_empty());
    owner.control(request.clone()).unwrap();
    assert!(matches!(
        adapter.control(),
        Some(DapControl::Variables {
            reference: 7,
            start: 0,
            count: 10,
        })
    ));
}

#[test]
fn breakpoint_rejections_update_only_the_latest_requested_rows() {
    use crate::debugger::DapBreakpointState;
    let (mut owner, adapter) = request_fixture();
    let path = crate::files::absolute_test_path("/source.rs");
    let at = |line| DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    owner.set_breakpoint_disabled(&path, 10, true);
    assert!(owner.disabled_breakpoints.is_empty());
    assert_eq!(owner.breakpoint_state(&path, 10), None);
    assert!(adapter.control().is_none());
    owner.breakpoint(at(10), false).unwrap();
    let first_id = owner.latest_breakpoints[&path];
    let first = adapter.control().unwrap();
    assert!(
        matches!(&first, DapControl::SetBreakpoints { breakpoints, .. } if breakpoints == &[at(10)])
    );
    owner.breakpoint(at(20), false).unwrap();
    let latest_id = owner.latest_breakpoints[&path];
    let latest = adapter.control().unwrap();
    let failure = |request_id, request| DapEvent::RequestFailed {
        adapter: "same-name".into(),
        stop_id: 0,
        request_seq: 9,
        request_id,
        request: Some(request),
        command: "setBreakpoints".into(),
        message: "condition rejected".into(),
    };
    adapter.event(0, failure(first_id, first.clone()));
    owner.try_event().unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Requested { accepted: true })
    );
    adapter.event(0, failure(latest_id, latest));
    owner.try_event().unwrap();
    for line in [10, 20] {
        assert_eq!(
            owner.breakpoint_state(&path, line),
            Some(DapBreakpointState::Requested { accepted: false })
        );
    }
    owner.set_breakpoint_disabled(&path, 20, true);
    let request_id = owner.latest_breakpoints[&path];
    let request = adapter.control().unwrap();
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id,
            request: Some(request),
            response: DapResponse::Breakpoints(vec![(true, Some(12), None)]),
        },
    );
    owner.try_event().unwrap();
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: first_id,
            request: Some(first.clone()),
            response: DapResponse::Breakpoints(vec![(true, Some(11), None)]),
        },
    );
    owner.try_event().unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Verified { line: 12 })
    );
    adapter.event(0, failure(first_id, first));
    owner.try_event().unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Verified { line: 12 })
    );
    assert_eq!(
        owner.breakpoint_state(&path, 20),
        Some(DapBreakpointState::Disabled)
    );
}

#[test]
fn breakpoint_changes_send_only_enabled_rows_and_acknowledge_the_captured_list() {
    let (mut owner, adapter) = request_fixture();
    let path = crate::files::absolute_test_path("/source.rs");
    let at = |line| DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    owner.breakpoint(at(10), false).unwrap();
    let Some(DapControl::SetBreakpoints {
        breakpoints: first, ..
    }) = adapter.control()
    else {
        panic!()
    };
    owner.breakpoint(at(20), false).unwrap();
    adapter.control();
    owner.breakpoint(at(10), true).unwrap();
    adapter.control();
    owner.apply_breakpoints(
        &path,
        &first,
        &DapResponse::Breakpoints(vec![(true, Some(10), None)]),
    );
    assert!(!owner.breakpoint_state(&path, 20).unwrap().is_verified());
    owner.set_breakpoint_disabled(&path, 20, true);
    assert!(
        matches!(adapter.control(), Some(DapControl::SetBreakpoints { breakpoints, .. }) if breakpoints.is_empty())
    );
    owner.set_breakpoint_disabled(&path, 20, false);
    assert!(
        matches!(adapter.control(), Some(DapControl::SetBreakpoints { breakpoints, .. }) if breakpoints.len() == 1)
    );
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 0,
            request: None,
            response: DapResponse::BreakpointChanged {
                path: path.clone(),
                requested: 20,
                verified: true,
                line: Some(21),
                removed: false,
            },
        },
    );
    owner.try_event().unwrap();
    assert_eq!(
        owner.breakpoint_state(&path, 20),
        Some(crate::debugger::DapBreakpointState::Verified { line: 21 })
    );
}
#[test]
fn breakpoint_changes_retry_the_latest_configuration_after_backpressure() {
    let (mut owner, adapter) = request_fixture();
    let path = crate::files::absolute_test_path("/source.rs");
    let at = |line| DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    owner.breakpoint(at(10), false).unwrap();
    adapter.control();
    while owner.control(DapControl::Threads).is_ok() {}
    owner.set_breakpoint_disabled(&path, 10, true);
    owner.breakpoint(at(20), false).unwrap();
    assert_eq!(owner.breakpoint_refresh.len(), 1);
    assert!(!owner.latest_breakpoints.contains_key(&path));
    while adapter.control().is_some() {}
    owner.try_event();
    assert!(
        matches!(adapter.control(), Some(DapControl::SetBreakpoints {
        path: sent, breakpoints,
    }) if sent == path && breakpoints == [at(20)])
    );
    assert!(owner.breakpoint_refresh.is_empty());
    assert!(adapter.control().is_none());
    while owner.control(DapControl::Threads).is_ok() {}
    owner.breakpoint(at(20), true).unwrap();
    owner.breakpoint(at(10), true).unwrap();
    while adapter.control().is_some() {}
    owner.try_event();
    assert!(
        matches!(adapter.control(), Some(DapControl::SetBreakpoints {
        breakpoints, ..
    }) if breakpoints.is_empty())
    );
}

#[test]
fn startup_breakpoints_reset_enabled_verdicts_for_the_new_adapter() {
    use crate::debugger::DapBreakpointState;
    let (mut owner, adapter) = request_fixture();
    let path = crate::files::absolute_test_path("/source.rs");
    let at = |line| DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    for line in [10, 20] {
        owner.breakpoint(at(line), false).unwrap();
        adapter.control();
    }
    owner.set_breakpoint_disabled(&path, 20, true);
    adapter.control();
    owner.apply_breakpoints(
        &path,
        &[at(10)],
        &DapResponse::Breakpoints(vec![(true, Some(11), None)]),
    );
    owner.stop().unwrap();
    let (service, _adapter) = crate::debugger::test_session();
    owner.service = service;
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Verified { line: 11 })
    );
    assert_eq!(owner.startup_breakpoints(), [(path.clone(), vec![at(10)])]);
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Requested { accepted: true })
    );
    assert_eq!(
        owner.breakpoint_state(&path, 20),
        Some(DapBreakpointState::Disabled)
    );
}

#[test]
fn restoring_breakpoints_clears_removed_adapter_paths_and_old_verdicts() {
    use crate::debugger::DapBreakpointState;
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let (mut owner, adapter) = request_fixture();
    let old = root.path().join("old.rs");
    let kept = root.path().join("kept.rs");
    for path in [&old, &kept] {
        owner
            .breakpoint(
                DapSourceBreakpoint {
                    path: path.clone(),
                    line: 10,
                    column: None,
                    condition: None,
                    hit_condition: None,
                    log_message: None,
                },
                false,
            )
            .unwrap();
        adapter.control();
        owner.set_breakpoint_disabled(path, 10, true);
        adapter.control();
    }
    while owner.control(DapControl::Threads).is_ok() {}
    owner.restore(
        vec![PersistedBreakpoint {
            path: kept.clone(),
            line: 10,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        }],
        Vec::new(),
        &workspace,
    );
    assert_eq!(owner.breakpoint_state(&old, 10), None);
    assert_eq!(
        owner.breakpoint_state(&kept, 10),
        Some(DapBreakpointState::Requested { accepted: true })
    );
    assert!(owner.disabled_breakpoints.is_empty());
    while adapter.control().is_some() {}
    owner.try_event();
    let mut sent = BTreeMap::new();
    while let Some(DapControl::SetBreakpoints { path, breakpoints }) = adapter.control() {
        sent.insert(path, breakpoints);
    }
    assert!(sent[&old].is_empty());
    assert_eq!(sent[&kept].len(), 1);
    assert_eq!(sent.len(), 2);
    owner.restore(Vec::new(), Vec::new(), &workspace);
    assert!(
        matches!(adapter.control(), Some(DapControl::SetBreakpoints {
        path, breakpoints,
    }) if path == kept && breakpoints.is_empty())
    );
    assert!(owner.breakpoint_states.is_empty());
}

#[test]
fn new_watches_without_source_paths_retry_after_backpressure() {
    let (mut owner, adapter) = request_fixture();
    owner.begin_stop();
    owner.console_frame = Some(42);
    assert!(owner.execution().is_none());
    while owner.control(DapControl::Threads).is_ok() {}
    owner.add_watch("fresh".into());
    assert_eq!(owner.watch_refresh, ["fresh"]);
    while adapter.control().is_some() {}
    owner.try_event();
    assert!(matches!(adapter.control(), Some(DapControl::Evaluate {
        expression, frame_id: Some(42), context,
    }) if expression == "fresh" && context == "watch"));
    assert!(owner.watch_refresh.is_empty());
}

#[test]
fn removed_watches_drop_queued_cached_and_late_values() {
    let (mut owner, adapter) = request_fixture();
    owner.add_watch("gone".into());
    let request = DapControl::Evaluate {
        expression: "gone".into(),
        frame_id: None,
        context: "watch".into(),
    };
    owner.control(request.clone()).unwrap();
    owner.watch_refresh.push_back("gone".into());
    owner.watch_values.insert("gone".into(), "old".into());
    owner.remove_watch("gone");
    assert!(owner.watch_refresh.is_empty());
    assert_eq!(owner.watch_value("gone"), None);
    adapter.control();
    adapter.event(
        0,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 0,
            request_id: 1,
            request: Some(request.clone()),
            response: evaluation("late"),
        },
    );
    let DapEvent::Response { response, .. } = owner.try_event().unwrap() else {
        panic!()
    };
    assert!(matches!(owner.response(response), DebugResponse::Absorbed));
    assert_eq!(owner.watch_value("gone"), None);
    owner.prepare_answer(1, &request, true);
    assert!(owner.watch_request_failed("evaluate"));
    assert_eq!(owner.watch_value("gone"), None);
    assert!(adapter.control().is_none());
}

#[test]
fn every_accepted_watch_is_refreshed_after_control_queue_backpressure() {
    let (mut owner, adapter) = request_fixture();
    owner.watch_refresh = (0..101).map(|i| format!("watch{i}")).collect();
    owner.refresh_watches();
    owner.refresh_watches();
    owner.refresh_watches();
    assert!(!owner.watch_refresh.is_empty());
    let mut seen = Vec::new();
    for _ in 0..20 {
        while let Some(DapControl::Evaluate { expression, .. }) = adapter.control() {
            seen.push(expression);
        }
        owner.refresh_watches();
        if owner.watch_refresh.is_empty() {
            break;
        }
    }
    while let Some(DapControl::Evaluate { expression, .. }) = adapter.control() {
        seen.push(expression);
    }
    assert_eq!(
        seen,
        (0..101).map(|i| format!("watch{i}")).collect::<Vec<_>>()
    );
    owner.event_source = owner.service.context();
    owner.watch_refresh.push_back("previous stop".into());
    adapter.advance(1);
    owner.refresh_watches();
    assert!(owner.watch_refresh.is_empty());
    assert!(adapter.control().is_none());
}

#[test]
fn automatic_stop_following_only_consumes_its_own_requests() {
    let (mut owner, adapter) = request_fixture();
    adapter.event(
        0,
        DapEvent::Stopped {
            adapter: "same-name".into(),
            reason: "breakpoint".into(),
            thread_id: Some(1),
        },
    );
    owner.try_event().unwrap();
    owner.control(DapControl::Threads).unwrap();
    let user_threads = adapter.control().unwrap();
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            1,
            user_threads,
            DapResponse::Threads(vec![crate::debugger::DapThread {
                id: 2,
                name: "other".into()
            },])
        ),
        DebugResponse::Picker {
            kind: "threads",
            ..
        }
    ));
    assert_eq!(owner.follow_request, Some(0));
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            0,
            DapControl::Threads,
            DapResponse::Threads(vec![crate::debugger::DapThread {
                id: 1,
                name: "stopped".into()
            },])
        ),
        DebugResponse::Absorbed
    ));
    let automatic_stack = adapter.control().unwrap();
    owner
        .control(DapControl::StackTrace {
            thread_id: 2,
            start: 0,
            levels: 256,
        })
        .unwrap();
    let user_stack = adapter.control().unwrap();
    let frame = |id, name: &str| DapStackFrame {
        id,
        name: name.into(),
        path: Some(crate::files::absolute_test_path(&format!("/{name}.rs"))),
        line: 3,
        column: 1,
        source_reference: None,
    };
    let DebugResponse::Picker { kind, rows } = answer(
        &mut owner,
        &adapter,
        3,
        user_stack,
        DapResponse::Stack(vec![frame(20, "other")]),
    ) else {
        panic!("user stack picker")
    };
    assert_eq!(kind, "frames");
    assert_eq!(rows[0].label, "other");
    assert_eq!(owner.current_frame(), None);
    assert_eq!(owner.follow_request, Some(2));
    let DebugResponse::StoppedAt(location) = answer(
        &mut owner,
        &adapter,
        2,
        automatic_stack,
        DapResponse::Stack(vec![frame(10, "stopped")]),
    ) else {
        panic!("automatic navigation")
    };
    assert_eq!(
        location.path,
        crate::files::absolute_test_path("/stopped.rs")
    );
    assert_eq!(owner.current_frame(), Some(10));
    let automatic_scopes = adapter.control().unwrap();
    owner.control(DapControl::Scopes { frame_id: 10 }).unwrap();
    let user_scopes = adapter.control().unwrap();
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            5,
            user_scopes,
            DapResponse::Scopes(Vec::new())
        ),
        DebugResponse::Picker { kind: "scopes", .. }
    ));
    assert_eq!(owner.follow_request, Some(4));
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            4,
            automatic_scopes,
            DapResponse::Scopes(vec![crate::debugger::DapScope {
                name: "locals".into(),
                variables_reference: 7,
                expensive: false
            },])
        ),
        DebugResponse::Absorbed
    ));
    let automatic_variables = adapter.control().unwrap();
    owner
        .control(DapControl::Variables {
            reference: 8,
            start: 0,
            count: 10,
        })
        .unwrap();
    let user_variables = adapter.control().unwrap();
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            7,
            user_variables,
            DapResponse::Variables(leaf_variables(1))
        ),
        DebugResponse::Picker {
            kind: "variables",
            ..
        }
    ));
    assert_eq!(owner.follow_request, Some(6));
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            6,
            automatic_variables,
            DapResponse::Variables(leaf_variables(2))
        ),
        DebugResponse::Absorbed
    ));
    assert_eq!(owner.variables(), ["v0 = 0", "v1 = 1"]);
}

#[test]
fn selecting_a_frame_refreshes_locals_and_watches_and_rejects_old_frame_answers() {
    for source_reference in [None, Some(7)] {
        let (mut owner, adapter) = request_fixture();
        owner.console_frame = Some(1);
        owner.add_watch("x".into());
        let old_watch = adapter.control().unwrap();
        owner
            .control(DapControl::Variables {
                reference: 10,
                start: 0,
                count: 10,
            })
            .unwrap();
        let old_variables = adapter.control().unwrap();
        owner.control(DapControl::Scopes { frame_id: 1 }).unwrap();
        let old_scopes = adapter.control().unwrap();
        owner.variables = vec!["x = 1".into()];
        owner.watch_values.insert("x".into(), "1".into());
        let rows = owner.frames(vec![DapStackFrame {
            id: 2,
            name: "caller".into(),
            path: Some("/caller.rs".into()),
            line: 10,
            column: 2,
            source_reference,
        }]);
        let selection = owner.take_selection(&rows[0].key).unwrap();
        owner.dismiss_choices();
        let editor = EditorState::new("/fixture".into());
        let windows = crate::app::windows::Windows::new(false, 10);
        owner
            .activate(
                selection,
                SourceTarget {
                    navigation: windows
                        .navigation_stamp(editor.window_version(), editor.active_view()),
                    capture: Arc::new(()),
                    line: 1,
                    column: 1,
                },
            )
            .unwrap();
        assert!(owner.variables().is_empty());
        assert_eq!(owner.watch_value("x"), None);
        assert_eq!(owner.current_frame(), Some(2));
        if source_reference.is_some() {
            assert!(matches!(
                adapter.control(),
                Some(DapControl::Source { reference: 7 })
            ));
        }
        let scopes_id = owner.follow_request.unwrap();
        let scopes = adapter.control().unwrap();
        assert!(matches!(scopes, DapControl::Scopes { frame_id: 2 }));
        let watch = adapter.control().unwrap();
        assert!(matches!(
            watch,
            DapControl::Evaluate {
                frame_id: Some(2),
                ..
            }
        ));
        for (id, request, response) in [
            (1, old_watch, evaluation("late 1")),
            (2, old_variables, DapResponse::Variables(leaf_variables(1))),
            (3, old_scopes, DapResponse::Scopes(Vec::new())),
        ] {
            adapter.event(
                0,
                DapEvent::Response {
                    adapter: "same-name".into(),
                    stop_id: 0,
                    request_id: id,
                    request: Some(request),
                    response,
                },
            );
            assert!(
                owner.try_event().is_none(),
                "old frame response was not discarded"
            );
        }
        assert_eq!(owner.watch_value("x"), None);
        assert!(matches!(
            answer(
                &mut owner,
                &adapter,
                scopes_id,
                scopes,
                DapResponse::Scopes(vec![crate::debugger::DapScope {
                    name: "locals".into(),
                    variables_reference: 20,
                    expensive: false
                },])
            ),
            DebugResponse::Absorbed
        ));
        let variables_id = owner.follow_request.unwrap();
        let variables = adapter.control().unwrap();
        assert!(matches!(
            answer(&mut owner, &adapter, scopes_id + 1, watch, evaluation("2")),
            DebugResponse::Absorbed
        ));
        assert_eq!(owner.watch_value("x"), Some("2"));
        assert!(matches!(
            answer(
                &mut owner,
                &adapter,
                variables_id,
                variables,
                DapResponse::Variables(vec![crate::debugger::DapVariable {
                    name: "x".into(),
                    value: "2".into(),
                    type_name: None,
                    variables_reference: 0
                },])
            ),
            DebugResponse::Absorbed
        ));
        assert_eq!(owner.variables(), ["x = 2"]);
    }
}

#[test]
fn selecting_the_same_frame_invalidates_previous_automatic_inspector_requests() {
    for (step, request, response) in [
        (
            StopFollow::Threads(Some(1)),
            DapControl::Threads,
            DapResponse::Threads(Vec::new()),
        ),
        (
            StopFollow::Stack,
            DapControl::StackTrace {
                thread_id: 1,
                start: 0,
                levels: 256,
            },
            DapResponse::Stack(Vec::new()),
        ),
        (
            StopFollow::Scopes,
            DapControl::Scopes { frame_id: 1 },
            DapResponse::Scopes(Vec::new()),
        ),
        (
            StopFollow::Variables,
            DapControl::Variables {
                reference: 10,
                start: 0,
                count: 10,
            },
            DapResponse::Variables(leaf_variables(1)),
        ),
    ] {
        let (mut owner, adapter) = request_fixture();
        owner.console_frame = Some(1);
        owner.stop_follow = step;
        if matches!(step, StopFollow::Threads(_)) {
            owner.follow_request = Some(0);
        } else {
            owner.follow_control(request.clone());
            assert!(adapter.control().is_some());
        }
        let old_id = owner.follow_request.unwrap();
        let rows = owner.frames(vec![DapStackFrame {
            id: 1,
            name: "current".into(),
            path: Some("/source.rs".into()),
            line: 10,
            column: 1,
            source_reference: None,
        }]);
        let selection = owner.take_selection(&rows[0].key).unwrap();
        owner.dismiss_choices();
        let editor = EditorState::new("/fixture".into());
        let windows = crate::app::windows::Windows::new(false, 10);
        owner
            .activate(
                selection,
                SourceTarget {
                    navigation: windows
                        .navigation_stamp(editor.window_version(), editor.active_view()),
                    capture: Arc::new(()),
                    line: 1,
                    column: 1,
                },
            )
            .unwrap();
        let scopes_id = owner.follow_request.unwrap();
        let scopes = adapter.control().unwrap();
        adapter.event(
            0,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_id: 0,
                request_id: old_id,
                request: Some(request),
                response,
            },
        );
        assert!(
            owner.try_event().is_none(),
            "superseded automatic response became a picker"
        );
        assert_eq!(owner.follow_request, Some(scopes_id));
        assert!(matches!(
            answer(
                &mut owner,
                &adapter,
                scopes_id,
                scopes,
                DapResponse::Scopes(vec![crate::debugger::DapScope {
                    name: "locals".into(),
                    variables_reference: 20,
                    expensive: false
                },])
            ),
            DebugResponse::Absorbed
        ));
        let variables_id = owner.follow_request.unwrap();
        let variables = adapter.control().unwrap();
        assert!(matches!(
            answer(
                &mut owner,
                &adapter,
                variables_id,
                variables,
                DapResponse::Variables(leaf_variables(2))
            ),
            DebugResponse::Absorbed
        ));
        assert_eq!(owner.variables(), ["v0 = 0", "v1 = 1"]);
    }
}

#[test]
fn automatic_stop_navigation_fetches_generated_source_and_locals() {
    let (mut owner, adapter) = request_fixture();
    owner.begin_stop();
    owner.stop_follow = StopFollow::Stack;
    owner.follow_control(DapControl::StackTrace {
        thread_id: 1,
        start: 0,
        levels: 256,
    });
    let stack = adapter.control().unwrap();
    let DebugResponse::StoppedSource(frame) = answer(
        &mut owner,
        &adapter,
        1,
        stack,
        DapResponse::Stack(vec![DapStackFrame {
            id: 42,
            name: "generated".into(),
            path: None,
            line: 10,
            column: 2,
            source_reference: Some(7),
        }]),
    ) else {
        panic!("generated source navigation")
    };
    assert_eq!(owner.current_frame(), Some(42));
    let scopes_id = owner.follow_request.unwrap();
    let scopes = adapter.control().unwrap();
    assert!(matches!(scopes, DapControl::Scopes { frame_id: 42 }));
    let editor = EditorState::new("/fixture".into());
    let windows = crate::app::windows::Windows::new(false, 10);
    owner
        .request_source(
            frame.source_reference.unwrap(),
            SourceTarget {
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view()),
                capture: Arc::new(()),
                line: frame.line,
                column: frame.column,
            },
        )
        .unwrap();
    let source_request = adapter.control().unwrap();
    assert!(matches!(
        source_request,
        DapControl::Source { reference: 7 }
    ));
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            scopes_id,
            scopes,
            DapResponse::Scopes(vec![crate::debugger::DapScope {
                name: "locals".into(),
                variables_reference: 7,
                expensive: false
            },])
        ),
        DebugResponse::Absorbed
    ));
    let variables_id = owner.follow_request.unwrap();
    let variables = adapter.control().unwrap();
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            variables_id,
            variables,
            DapResponse::Variables(leaf_variables(1))
        ),
        DebugResponse::Absorbed
    ));
    assert_eq!(owner.variables(), ["v0 = 0"]);
    assert!(matches!(
        answer(
            &mut owner,
            &adapter,
            3,
            source_request,
            DapResponse::Source {
                name: "generated".into(),
                content: "generated source".into(),
                mime_type: None,
            }
        ),
        DebugResponse::Result(DapResponse::Source { .. })
    ));
    let target = owner.take_source().unwrap();
    assert_eq!((target.line, target.column), (10, 2));
}

#[test]
fn virtual_source_requests_retain_the_selected_frame_position() {
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = Debugger::new(&DebugConfig::default(), workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    let editor = EditorState::new(root.path().into());
    let windows = crate::app::windows::Windows::new(false, 10);
    let rows = owner.frames(vec![DapStackFrame {
        id: 1,
        name: "virtual".into(),
        path: None,
        line: 200,
        column: 5,
        source_reference: Some(7),
    }]);
    let selection = owner.take_selection(&rows[0].key).unwrap();
    owner
        .activate(
            selection,
            SourceTarget {
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view()),
                capture: Arc::new(()),
                line: 1,
                column: 1,
            },
        )
        .unwrap();
    assert!(matches!(
        adapter.control(),
        Some(DapControl::Source { reference: 7 })
    ));
    assert!(matches!(
        adapter.control(),
        Some(DapControl::Scopes { frame_id: 1 })
    ));
    assert_eq!(owner.stop_follow, StopFollow::Scopes);
    let source = owner.take_source().unwrap();
    assert_eq!((source.line, source.column), (200, 5));
}
