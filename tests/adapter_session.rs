#![cfg(unix)]

use std::process::Command;

#[test]
fn an_adapter_started_through_dust_leads_its_own_session() {
    let script = "import os; print(os.getsid(0) == os.getpid())";
    let output = Command::new(env!("CARGO_BIN_EXE_dust"))
        .args([dust::debugger::ADAPTER_SESSION_ARGUMENT, "python3", "-c", script])
        .output()
        .expect("run dust");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "True");
}

#[test]
fn a_missing_adapter_is_a_failure_rather_than_the_editor_starting() {
    let output = Command::new(env!("CARGO_BIN_EXE_dust"))
        .args([dust::debugger::ADAPTER_SESSION_ARGUMENT, "/nonexistent/adapter"])
        .output()
        .expect("run dust");
    assert!(!output.status.success());
}
