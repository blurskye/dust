#![cfg(unix)]

use std::process::Child;

use std::process::Command;

use std::thread;

use std::time::Duration;

use std::time::Instant;

struct AdapterChild(Child);

impl Drop for AdapterChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_adapter_child_is_killed_and_reaped_on_assertion_failure() {
    let child = Command::new("python3")
        .args(["-c", "import time; time.sleep(60)"])
        .spawn()
        .expect("start adapter fixture");
    let pid = nix::unistd::Pid::from_raw(child.id().try_into().unwrap());
    let result = std::panic::catch_unwind(move || {
        let _child = AdapterChild(child);
        panic!("simulated assertion failure");
    });
    assert!(result.is_err());
    assert_eq!(
        nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)),
        Err(nix::errno::Errno::ECHILD),
        "the guard must kill and reap the child before returning"
    );
}

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
    use nix::fcntl::FcntlArg;
    use nix::fcntl::OFlag;
    use nix::fcntl::fcntl;
    use std::io::Read;
    use std::io::Write;

    let script = r#"
import socket, sys
host, port = sys.argv[1].rsplit(':', 1)
with socket.create_connection((host, int(port)), timeout=5) as s:
    request = bytearray()
    while len(request) < len(b'no newline'):
        chunk = s.recv(min(2, len(b'no newline') - len(request)))
        if not chunk:
            raise RuntimeError('request ended early')
        request.extend(chunk)
    s.sendall(request.upper())
"#;
    let mut bridge = AdapterChild(
        Command::new(env!("CARGO_BIN_EXE_dust"))
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
            .expect("run dust"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdin = bridge.0.stdin.take().expect("stdin");
    stdin.write_all(b"no newline").expect("write");
    let mut stdout = bridge.0.stdout.take().expect("stdout");
    let flags = OFlag::from_bits_truncate(fcntl(&stdout, FcntlArg::F_GETFL).unwrap());
    fcntl(&stdout, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
    let mut answer = Vec::new();
    let mut buffer = [0; 64];
    loop {
        assert!(
            Instant::now() < deadline,
            "adapter response timed out: {answer:?}"
        );
        match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => answer.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("read: {error}"),
        }
    }
    assert_eq!(answer, b"NO NEWLINE");
    drop(stdin);
    loop {
        if let Some(status) = bridge.0.try_wait().expect("wait") {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "adapter exit timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
