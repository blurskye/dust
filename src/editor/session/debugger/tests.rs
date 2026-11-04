use super::*;
use crate::{
    editor::EditorState,
    workspace::{RootIdentity, WorkspaceId, WorkspaceState},
};

#[test]
fn debugger_owner_keeps_original_actor_stop_context_and_private_selection() {
    super::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = DebugSession::new(&DebugConfig::default(), workspace.version());
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
            stop_generation: 1,
            request_id: 0,
            request: None,
            response: crate::debugger::DapResponse::Threads(Vec::new()),
        },
    );
    assert!(owner.try_event().is_some());
    let keys = owner.controls(vec![("thread".into(), String::new(), DapControl::Threads)]);
    let editor = EditorState::new(root.path().into());
    let windows = super::super::windows::WindowSession::new(false, 10);
    let target = || SourceTarget {
        navigation: windows.navigation_stamp(editor.window_version(), editor.active_view),
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
        Err(DapError::Retired)
    ));
    assert!(matches!(
        owner.take_selection(&keys[0].key),
        Err(DapError::Retired)
    ));
    assert!(adapter.control().is_none());
    // An event drained at stop 2 cannot be relabelled with stop 3 during row building.
    adapter.event(
        2,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_generation: 2,
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
fn breakpoint_states_are_session_state_driven_by_the_adapters_own_answer() {
    use crate::debugger::{DapBreakpointState, DapResponse};
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = DebugSession::new(&policy, workspace.version());
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

    // RE-ENABLING makes it a request again rather than silently verified.
    owner.set_breakpoint_disabled(&path, 10, false);
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
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();

    // DETACH: the session reports that it LEFT, and that it did not ask for the program to stop.
    let mut detached = DebugSession::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    detached.service = service;
    detached.detach_debuggee().expect("detach is admitted");
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
            stop_generation: 0,
            request_id: 1,
            request: Some(request),
            response: DapResponse::Disconnected { terminate_debuggee },
        },
    );
    detached.try_event().unwrap();
    assert_eq!(
        detached.end_state(),
        Some(DapSessionEnd::Detached),
        "detaching is leaving, not killing"
    );

    // TERMINATE: a DIFFERENT reported fact.
    let mut terminated = DebugSession::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    terminated.service = service;
    terminated
        .terminate_debuggee()
        .expect("terminate is admitted");
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
            stop_generation: 0,
            request_id: 1,
            request: Some(request),
            response: DapResponse::Disconnected { terminate_debuggee },
        },
    );
    terminated.try_event().unwrap();
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
fn the_console_keeps_program_output_evaluation_and_typed_commands_apart() {
    use crate::debugger::DapConsoleOrigin;
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = DebugSession::new(&policy, workspace.version());

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
            assert!(matches!(owner.response(response), DebugResponse::Absorbed));
        }
        assert_eq!(owner.variables(), [format!("count = {value}")]);

        // The variables picker also offers the current value and a usable selection.
        prime_for(&mut owner, &adapter);
        let DebugResponse::Picker { rows, .. } = owner.response(DapResponse::Variables(vec![
            crate::debugger::DapVariable {
                name: "count".into(),
                value: value.into(),
                type_name: None,
                variables_reference: 0,
            },
        ])) else {
            panic!("expected variables picker");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, format!("count = {value}"));
        let key = rows[0].key.clone();
        assert!(owner.choices.is_some());

        adapter.event(
            1,
            if running {
                DapEvent::Running { adapter: "same-name".into() }
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
        assert!(owner.variables().is_empty(), "old locals must leave the inspector");
        assert!(owner.choices.is_none(), "old picker rows must be retired");
        assert!(owner.take_selection(&key).is_err());
    }
}

#[test]
fn a_variables_answer_to_an_earlier_stop_is_refused_rather_than_presented_as_current() {
    // W-24: "Continuing, restarting or re-stopping invalidates stopped data and pending variable
    // references so a previous value can never be presented as current."
    super::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = DebugSession::new(&policy, workspace.version());
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
        panic!("expected a variables picker, not a retired or bare result")
    };
    assert_eq!(rows.len(), 1, "the current answer IS presented");
    assert_eq!(rows[0].label, "count = 1");
    let previous_key = rows[0].key.clone();

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
            stop_generation: 1,
            request_id: 2,
            request: Some(request()),
            response: DapResponse::Variables(vec![variable("1")]),
        },
    );
    owner.begin_running();
    adapter.advance(2);
    assert!(
        owner.try_event().is_none(),
        "the transport retires the original stopped request"
    );
    assert!(owner.variables().is_empty());
    assert!(owner.take_selection(&previous_key).is_err());

    // A RE-STOP then answers for the NEW stop, and that value IS current.
    owner.begin_stop();
    owner
        .control(request())
        .expect("the fake adapter accepts a variables request");
    prime_for(&mut owner, &adapter);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("2")]));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a retired or bare result")
    };
    assert_eq!(rows.len(), 1, "the fresh answer is presented");
    assert_eq!(rows[0].label, "count = 2");
}

pub(in crate::editor::session) fn attach_adapter(owner: &mut DebugSession) -> crate::debugger::TestAdapter {
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    prime_for(owner, &adapter);
    adapter
}

fn paginated_owner() -> (DebugSession, crate::debugger::TestAdapter) {
    super::launch::original_launch_fixture();
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = DebugSession::new(&policy, workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    owner.begin_stop();
    (owner, adapter)
}

/// Prime the actor: an answer is only accepted while one is open, and presenting CONSUMES it.
fn prime_for(owner: &mut DebugSession, adapter: &crate::debugger::TestAdapter) {
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_generation: 1,
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
fn a_full_page_of_variables_offers_an_exact_continuation_that_carries_the_next_offset() {
    // W-25: "Variables and collections load lazily and paginate."
    let (mut owner, adapter) = paginated_owner();
    let page = super::VARIABLE_PAGE;
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
fn a_reference_already_open_is_shown_as_a_reference_and_is_never_requested_again() {
    // W-25: "cyclic and repeated references are shown as bounded expandable references and are
    // never materialized recursively."
    let (mut owner, adapter) = paginated_owner();
    owner
        .control(DapControl::Variables {
            reference: 7,
            start: 0,
            count: super::VARIABLE_PAGE,
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
    // W-25 names "sessions, THREADS, STACK and breakpoints on one side", and a stack is stopped
    // data: showing it after execution ran on would present a previous value as current.
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

fn request_fixture() -> (DebugSession, crate::debugger::TestAdapter) {
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = DebugSession::new(&DebugConfig::default(), workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    (owner, adapter)
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
                stop_generation: 0,
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
            stop_generation: 0,
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
            stop_generation: 0,
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
fn stopped_request_bookkeeping_is_retired_on_continue_and_failure() {
    let (mut owner, _) = request_fixture();
    owner.note_issued(true, Some(3), Some(7), 0);
    owner.begin_running();
    assert!(
        owner.issued_stack.is_empty()
            && owner.issued_scopes.is_empty()
            && owner.issued_variables.is_empty()
    );
    owner.note_issued(true, Some(3), Some(7), owner.stop_epoch);
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
    owner.issued_variables.insert(7, u64::MAX);
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
            stop_generation: 0,
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
    let windows = super::super::windows::WindowSession::new(false, 10);
    owner
        .activate(
            selection,
            SourceTarget {
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view),
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
fn breakpoint_changes_send_only_enabled_rows_and_acknowledge_the_captured_list() {
    let (mut owner, adapter) = request_fixture();
    let path = PathBuf::from("/source.rs");
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
            stop_generation: 0,
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
fn every_admitted_watch_is_refreshed_after_control_queue_backpressure() {
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
    assert_eq!(seen, (0..101).map(|i| format!("watch{i}")).collect::<Vec<_>>());
    owner.event_source = owner.service.context();
    owner.watch_refresh.push_back("retired stop".into());
    adapter.advance(1);
    owner.refresh_watches();
    assert!(owner.watch_refresh.is_empty());
    assert!(adapter.control().is_none());
}

#[test]
fn virtual_source_requests_retain_the_selected_frame_position() {
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let mut owner = DebugSession::new(&DebugConfig::default(), workspace.version());
    let (service, adapter) = crate::debugger::test_session();
    owner.service = service;
    let editor = EditorState::new(root.path().into());
    let windows = super::super::windows::WindowSession::new(false, 10);
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
                navigation: windows.navigation_stamp(editor.window_version(), editor.active_view),
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
    let source = owner.take_source().unwrap();
    assert_eq!((source.line, source.column), (200, 5));
}
