use crate::app::tests::*;




#[test]
pub(in crate::app) fn breakpoint_toggle_uses_disabled_state_after_another_breakpoint_is_added() {
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