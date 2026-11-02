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
    assert!(
        owner.apply_breakpoint_answer(&DapResponse::Breakpoints(vec![(true, Some(11), None)])),
        "the answer is matched to the request that was waiting"
    );
    assert_eq!(
        owner.breakpoint_state(&path, 10),
        Some(DapBreakpointState::Verified { line: 11 }),
        "verified at the ADAPTER's line, which it may have moved"
    );

    // AN ANSWER THAT DECLINES: still not verified, and the state says which case it is.
    owner.breakpoint(at(20), false).unwrap();
    owner.apply_breakpoint_answer(&DapResponse::Breakpoints(vec![
        (true, Some(20), None),
        (false, None, None),
    ]));
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
    owner.apply_breakpoint_answer(&DapResponse::Breakpoints(vec![(true, Some(10), None)]));
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
    detached.detach_debuggee().expect("detach is accepted");
    assert_eq!(
        detached.end_state(),
        Some(DapSessionEnd::Detached),
        "detaching is leaving, not killing"
    );

    // TERMINATE: a DIFFERENT reported fact.
    let mut terminated = DebugSession::new(&policy, workspace.version());
    terminated.terminate_debuggee().expect("terminate is accepted");
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
    // An adapter Output event is program output; driven through the same path the session uses.
    owner.record_program_output_for_test("hello".into());

    let entries = owner.console().cloned().collect::<Vec<_>>();
    assert_eq!(entries.len(), 3, "all three origins are kept: {entries:?}");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.origin)
            .collect::<Vec<_>>(),
        vec![
            DapConsoleOrigin::Command,
            DapConsoleOrigin::Evaluation,
            DapConsoleOrigin::ProgramOutput,
        ],
        "each entry carries its ORIGIN, and the order they happened is preserved"
    );
    // The separation the clause asks for, asserted in both directions.
    assert_ne!(
        entries[1].origin,
        entries[2].origin,
        "an evaluation result is not program output"
    );
    assert_ne!(
        entries[0].origin,
        entries[1].origin,
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
    // W-24 verbatim: "Continuing, restarting or re-stopping invalidates stopped data and pending
    // variable references so a previous value can never be presented as current."
    let root = tempfile::tempdir().unwrap();
    let state = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = WorkspaceSession::new(state, root.path().join("trust"), 1, false, false, 1);
    let policy = DebugConfig::default();
    let mut owner = DebugSession::new(&policy, workspace.version());

    // A STOP happens and a value is read: it IS current.
    owner.begin_stop();
    let first_epoch = owner.stop_epoch();
    owner.record_stopped_value(42, "count = 1".into());
    assert_eq!(
        owner.current_stopped_value(42),
        Some("count = 1"),
        "a value read at the current stop is presentable"
    );

    // CONTINUE: execution runs on, and the value read while stopped is NOT current any more.
    owner.begin_running();
    assert_ne!(
        owner.stop_epoch(),
        first_epoch,
        "running advances the stop identity"
    );
    assert_eq!(
        owner.current_stopped_value(42),
        None,
        "a value from a previous stop CANNOT be presented as current"
    );

    // RE-STOP: a NEW stop with fresh data, and the old value is still not presentable.
    owner.begin_stop();
    let second_epoch = owner.stop_epoch();
    assert_ne!(second_epoch, first_epoch, "a re-stop is a different stop");
    assert_eq!(
        owner.current_stopped_value(42),
        None,
        "the previous stop's value is still unpresentable at the new stop, before anything is read"
    );
    // The adapter reports a DIFFERENT value at this stop, which is what makes "fresh" measurable.
    owner.record_stopped_value(42, "count = 2".into());
    assert_eq!(
        owner.current_stopped_value(42),
        Some("count = 2"),
        "the value read at THIS stop is current"
    );
    assert_ne!(
        owner.current_stopped_value(42),
        Some("count = 1"),
        "and it is not the value from the previous stop"
    );

    // RESTART is the same rule: it also invalidates.
    owner.begin_running();
    assert_eq!(owner.current_stopped_value(42), None);
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

    // An answer is only accepted while the session has an open actor, and presenting one CONSUMES
    // it, so each answer below is preceded by the event that re-opens it, exactly as the app does.
    let prime = |owner: &mut DebugSession| {
        adapter.event(
            1,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_generation: 1,
                response: crate::debugger::DapResponse::Threads(Vec::new()),
            },
        );
        assert!(owner.try_event().is_some(), "the session accepts an event from its actor");
    };

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
    owner.control(request()).expect("the fake adapter accepts a variables request");
    prime(&mut owner);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("1")]));
    assert_eq!(
        owner.current_stopped_value(7),
        Some("count = 1"),
        "an answer to the stop that asked for it IS current"
    );
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a retired or bare result")
    };
    assert_eq!(rows.len(), 1, "the current answer IS presented");

    // (i) EARLIER STOP: the request is issued at one stop, execution RUNS ON, and only then does
    // the answer arrive. It answers the earlier stop, so it can never be presented as current.
    owner.begin_stop();
    owner.control(request()).expect("the fake adapter accepts a variables request");
    owner.begin_running();
    prime(&mut owner);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("1")]));
    assert_eq!(
        owner.current_stopped_value(7),
        None,
        "a value answering an EARLIER stop can never be presented as current"
    );
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a retired or bare result")
    };
    assert!(rows.is_empty(), "the stale answer presents NOTHING");
    assert!(
        owner
            .console
            .iter()
            .any(|entry| entry.text.contains("earlier stop")),
        "the refusal is a visible notice rather than silence"
    );

    // A RE-STOP then answers for the NEW stop, and that value IS current.
    owner.begin_stop();
    owner.control(request()).expect("the fake adapter accepts a variables request");
    prime(&mut owner);
    let presented = owner.response(crate::debugger::DapResponse::Variables(vec![variable("2")]));
    assert_eq!(
        owner.current_stopped_value(7),
        Some("count = 2"),
        "a value read at the new stop is current"
    );
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a variables picker, not a retired or bare result")
    };
    assert_eq!(rows.len(), 1, "the fresh answer is presented");
}

#[test]
fn a_late_result_updates_its_own_record_and_leaves_a_newer_draft_untouched() {
    // W-25: "Console submission captures the actor, stop and frame that produced it, and a late
    // result updates its own record rather than an unrelated draft."
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

    let prime = |owner: &mut DebugSession| {
        adapter.event(
            1,
            DapEvent::Response {
                adapter: "same-name".into(),
                stop_generation: 1,
                response: crate::debugger::DapResponse::Threads(Vec::new()),
            },
        );
        assert!(owner.try_event().is_some(), "the session accepts an event from its actor");
    };

    // A stop, and a FRAME selected from that stop: selecting it binds the console to it.
    owner.begin_stop();
    let stop = owner.stop_epoch();
    prime(&mut owner);
    let presented = owner.response(crate::debugger::DapResponse::Stack(vec![
        crate::debugger::DapStackFrame {
            id: 42,
            name: "main".into(),
            path: None,
            line: 1,
            column: 1,
            source_reference: None,
        },
    ]));
    let DebugResponse::Picker { rows, .. } = &presented else {
        panic!("expected a frames picker");
    };
    assert_eq!(rows.len(), 1, "the frame is offered");
    let _frame = owner
        .take_selection(&rows[0].key)
        .expect("the frame is selectable");

    // The user submits an expression, then types a NEWER one before the first answer arrives.
    owner.record_console_command("first".into());
    owner.record_console_command("second".into());

    // The answer to the FIRST arrives LATE, after the newer draft exists.
    owner.record_console_evaluation("first = 1".into());

    let entries: Vec<&crate::debugger::DapConsoleEntry> = owner.console().collect();
    let first = entries
        .iter()
        .find(|entry| entry.text == "first")
        .expect("the first submission is a record");
    let second = entries
        .iter()
        .find(|entry| entry.text == "second")
        .expect("the newer draft is a record");
    let answer = entries
        .iter()
        .find(|entry| entry.text == "first = 1")
        .expect("the answer is a record");

    // (1) The submission captured the ACTOR, the STOP and the FRAME that produced it.
    assert_eq!(first.actor.as_deref(), Some("same-name"), "the actor is captured");
    assert_eq!(first.stop, Some(stop), "the stop is captured");
    assert_eq!(first.frame, Some(42), "the frame the console is bound to is captured");
    assert!(first.submission.is_some(), "a submission has its own identity");
    assert_ne!(
        first.submission, second.submission,
        "the newer draft has its OWN identity, not the first one's"
    );

    // (2) The late result updated ITS OWN record, and names the submission it answers.
    assert_eq!(
        first.result.as_deref(),
        Some("first = 1"),
        "the late result lands on the record that ASKED for it"
    );
    assert_eq!(answer.submission, first.submission, "the answer names its submission");
    assert_eq!(answer.frame, Some(42), "the answer carries the frame that produced it");

    // (3) The NEWER draft is untouched: no result, and still waiting for its own.
    assert_eq!(second.result, None, "the newer draft is NOT updated by a late result");
    assert_eq!(
        owner.pending_submissions.len(),
        1,
        "the newer submission is still the one waiting for an answer"
    );
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
            response: crate::debugger::DapResponse::Threads(Vec::new()),
        },
    );
    assert!(owner.try_event().is_some(), "the session accepts an event from its actor");
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
    let presented = owner.response(crate::debugger::DapResponse::Variables(
        leaf_variables(page),
    ));
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
        continuation.detail.contains(&format!("continue from {page}")),
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
