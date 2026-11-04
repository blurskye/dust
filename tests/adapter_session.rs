#![cfg(unix)]

use std::process::Command;

#[test]
fn an_adapter_started_through_dust_leads_its_own_session() {
    let script = "import os; print(os.getsid(0) == os.getpid())";
    let output = Command::new(env!("CARGO_BIN_EXE_dust"))
        .args([
            dust::debugger::ADAPTER_SESSION_ARGUMENT,
            "python3",
            "-c",
            script,
        ])
        .output()
        .expect("run dust");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "True");
}

#[test]
fn a_missing_adapter_is_a_failure_rather_than_the_editor_starting() {
    let output = Command::new(env!("CARGO_BIN_EXE_dust"))
        .args([
            dust::debugger::ADAPTER_SESSION_ARGUMENT,
            "/nonexistent/adapter",
        ])
        .output()
        .expect("run dust");
    assert!(!output.status.success());
}

#[test]
fn an_adapter_that_only_speaks_over_a_socket_is_carried_over_stdio() {
    use std::io::{Read, Write};
    let script = "import socket, sys; host, port = sys.argv[1].rsplit(':', 1); \
        s = socket.create_connection((host, int(port))); s.sendall(s.recv(64).upper()); s.close()";
    let mut bridge = Command::new(env!("CARGO_BIN_EXE_dust"))
        .args([
            dust::debugger::ADAPTER_BRIDGE_ARGUMENT,
            "python3",
            "-c",
            script,
            "{address}",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("run dust");
    let mut stdin = bridge.stdin.take().expect("stdin");
    stdin.write_all(b"no newline").expect("write");
    let mut answer = String::new();
    bridge
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut answer)
        .expect("read");
    assert_eq!(answer, "NO NEWLINE");
    assert!(bridge.wait().expect("wait").success());
}
