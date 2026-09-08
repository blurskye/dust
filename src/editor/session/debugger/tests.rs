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
    let policy = DebugConfig {
        maximum_breakpoints: 1,
        maximum_watches: 1,
        ..DebugConfig::default()
    };
    let mut owner = DebugSession::new(&policy, workspace.version());
    owner.add_watch("value".into());
    owner.add_watch("extra".into());
    assert_eq!(owner.watches(), ["value"]);
    let row = DapSourceBreakpoint {
        path: root.path().join("a"),
        line: 1,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    };
    owner.breakpoint(row.clone(), false).unwrap();
    let mut other = row;
    other.path = root.path().join("b");
    assert!(owner.breakpoint(other, false).is_err());
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
                request_arguments: serde_json::json!({})
            })
            .is_err()
    );
}
