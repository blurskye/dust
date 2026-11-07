//! Tests for pty sessions and worker queues.

use crate::process::PtyPolicy;
use crate::process::pty::*;
use std::path::Path;
use std::time::Instant;

fn request(root: &Path, script: &str) -> PtyRequest {
    PtyRequest {
        program: crate::process::discover_executable("sh").expect("test shell on PATH"),
        argv0: None,
        arguments: vec!["-c".into(), script.into()],
        cwd: root.into(),
        environment: Default::default(),
        columns: 80,
        rows: 8,
        root_revision: 3,
        policy: PtyPolicy {
            event_capacity: 1,
            ..Default::default()
        },
    }
}
fn drain(service: &mut PtyWorker, id: PtyId) -> Vec<PtyEvent> {
    // Generous for loaded CI machines; a passing run returns far sooner.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut received = Vec::new();
    loop {
        let batch = service.poll().unwrap();
        let done=batch.iter().any(|e|matches!(e,PtyEvent::IoComplete{id:source,..}|PtyEvent::Failed{id:source,..}|PtyEvent::Unknown{id:source,..} if *source==id));
        received.extend(batch);
        if done {
            return received;
        }
        assert!(
            Instant::now() < deadline,
            "PTY terminal timed out: {received:?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
// ConPTY waits for its cursor query to be answered, which the terminal panel does and this
// bare worker does not, and Git's `sh` prints MSYS-style paths.
#[cfg(unix)]
#[test]
fn pty_starts_in_the_requested_directory() {
    let root = tempfile::tempdir().unwrap();
    let canonical = crate::files::canonicalize(root.path()).unwrap();
    let mut service = PtyWorker::start(1);
    let id = service.open(request(&canonical, "pwd -P")).unwrap();
    let events = drain(&mut service, id);
    let output: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            PtyEvent::Output { bytes, .. } => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    assert_eq!(
        String::from_utf8(output).unwrap().trim(),
        canonical.to_str().unwrap()
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PtyEvent::IoComplete { result: Ok(()), .. }))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_pty_cwd_cannot_be_retargeted_or_fall_back_to_home() {
    use portable_pty::CommandBuilder;
    use portable_pty::PtySize;
    use portable_pty::native_pty_system;
    use std::os::fd::AsRawFd;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let other = root.path().join("other");
    let alias = root.path().join("alias");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let identity = crate::files::RootIdentity::capture(root.path()).unwrap();
    let cwd = open_working_directory(&alias, Some(&identity)).unwrap();
    let pinned = format!("/proc/self/fd/{}", cwd.as_raw_fd());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    for removed in [false, true] {
        if removed {
            std::fs::remove_dir(&original).unwrap();
        }
        // Spawn through portable-pty itself, including its cwd fallback check.
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 8,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "test . -ef \"$1\"", "sh"]);
        command.arg(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            cwd.as_raw_fd()
        ));
        command.cwd(&pinned);
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let deadline = Instant::now() + Duration::from_secs(4);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("cwd probe timed out");
            }
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(
            status.exit_code(),
            0,
            "spawn left the pinned cwd (removed: {removed})"
        );
        assert!(std::fs::metadata(&pinned).unwrap().is_dir());
    }
}

#[cfg(unix)]
#[test]
fn pty_worker_reserves_real_capacity_and_preserves_stream_before_terminal() {
    let root = tempfile::tempdir().unwrap();
    let mut missing = PtyWorker::start(1);
    let id = missing
        .open(request(&root.path().join("missing"), "printf forbidden"))
        .expect("foreground accepts; worker inspects directory");
    let events = drain(&mut missing, id);
    assert!(
        matches!(&events[..],[PtyEvent::Failed{message,..}] if message.contains("working directory"))
    );

    let mut service = PtyWorker::start(1);
    drop(service.reserve().unwrap());
    assert_eq!(service.residents.load(Ordering::Acquire), 0);
    let id=service.open(request(root.path(),"printf 'READY\n'; read value; printf 'ACK:%s\n' \"$value\"; i=0; while [ $i -lt 3000 ]; do printf 'ROW:%s\n' \"$i\"; i=$((i+1)); done")).unwrap();
    assert!(matches!(service.reserve(), Err(PtyError::QueueFull)));
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut output = Vec::new();
    let mut started = false;
    while !String::from_utf8_lossy(&output).contains("READY") {
        for event in service.poll().unwrap() {
            match event {
                PtyEvent::Started { .. } => started = true,
                PtyEvent::Output { bytes, .. } => {
                    assert!(started);
                    output.extend(bytes);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let loan = service.reserve_input(id).unwrap();
    assert!(
        matches!(service.reserve_input(id), Err(PtyError::QueueFull)),
        "reservation excludes a second producer before encoding"
    );
    drop(loan);
    service.resize_checked(id, 17, 61, 9).unwrap();
    service.input(id, b"hello\n".to_vec(), 64).unwrap();
    // A capacity-one undrained output queue must backpressure, not drop escape/text bytes.
    thread::sleep(Duration::from_millis(20));
    let events = drain(&mut service, id);
    let mut resized = false;
    let mut exited = false;
    let mut io_complete = false;
    for event in events {
        match event {
            PtyEvent::Output {
                bytes, truncated, ..
            } => {
                assert!(!truncated);
                output.extend(bytes);
            }
            PtyEvent::Resized {
                sequence,
                columns,
                rows,
                ..
            } => {
                assert_eq!((sequence, columns, rows), (17, 61, 9));
                resized = true;
            }
            PtyEvent::Exited { code, .. } => {
                assert_eq!(code, Some(0));
                exited = true;
            }
            PtyEvent::IoComplete { result, .. } => {
                assert!(exited, "I/O completion preceded child exit");
                result.expect("successful PTY I/O completion");
                io_complete = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(resized);
    assert!(exited && io_complete);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("ACK:hello"));
    let rows = output
        .lines()
        .filter_map(|s| {
            s.strip_prefix("ROW:")
                .and_then(|s| s.trim().parse::<usize>().ok())
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, (0..3000).collect::<Vec<_>>());
    assert!(
        service.poll().unwrap().is_empty(),
        "no output follows successful terminal"
    );

    let target = root.path().join("scope");
    std::fs::create_dir(&target).unwrap();
    let identity = crate::files::RootIdentity::capture(&target).unwrap();
    std::fs::rename(&target, root.path().join("old-scope")).unwrap();
    std::fs::create_dir(&target).unwrap();
    let mut replaced = PtyWorker::start(1);
    let id = replaced
        .reserve()
        .unwrap()
        .submit(
            request(&target, "printf forbidden > marker"),
            Some(identity),
        )
        .unwrap();
    assert!(drain(&mut replaced,id).iter().any(|e|matches!(e,PtyEvent::Failed{message,..} if message.contains("filesystem identity changed"))));
    assert!(!target.join("marker").exists());

    let mut cancelled = PtyWorker::start(1);
    let id = cancelled
        .open(request(
            root.path(),
            "printf 'READY\n'; while :; do :; done",
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while !cancelled
        .poll()
        .unwrap()
        .iter()
        .any(|e| matches!(e, PtyEvent::Started { .. }))
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let loan = cancelled.reserve_input(id).unwrap();
    assert!(matches!(
        cancelled.reserve_input(id),
        Err(PtyError::QueueFull)
    ));
    let before = Instant::now();
    cancelled.close(id).unwrap();
    assert!(before.elapsed() < Duration::from_millis(500));
    assert!(matches!(
        loan.submit(b"unused".to_vec()),
        Err(PtyError::Stopped)
    ));
    drain(&mut cancelled, id);
    let deadline = Instant::now() + Duration::from_secs(4);
    while cancelled.residents.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    cancelled.next = None;
    assert!(matches!(cancelled.reserve(), Err(PtyError::Exhausted)));
    cancelled.begin_shutdown();
    assert!(matches!(cancelled.reserve(), Err(PtyError::Stopped)));
    let before = Instant::now();
    drop(cancelled);
    assert!(before.elapsed() < Duration::from_millis(500));
    let mut limited = PtyWorker::start(1);
    let mut oversized = request(root.path(), "printf 123456789");
    oversized.policy.maximum_output_bytes = 4;
    let id = limited.open(oversized).unwrap();
    let limited = drain(&mut limited, id);
    let exit = limited
        .iter()
        .position(|event| matches!(event, PtyEvent::Exited { .. }))
        .expect("output failure still reaps the child");
    let io = limited
        .iter()
        .position(|event| matches!(event, PtyEvent::IoComplete { result: Err(message), .. } if message.contains("output limit exceeded")))
        .expect("reader failure is a distinct I/O completion");
    assert!(exit < io);

    let mut detached = PtyWorker::start(1);
    let _ = detached
        .open(request(root.path(), "while :; do :; done"))
        .unwrap();
    let residents = detached.residents.clone();
    let before = Instant::now();
    drop(detached);
    assert!(before.elapsed() < Duration::from_millis(500));
    let deadline = Instant::now() + Duration::from_secs(4);
    while residents.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    crate::debugger::terminal_reply_source_fixture();
}

#[test]
fn reaped_pty_child_is_never_signalled_again() {
    #[derive(Debug, Clone)]
    struct Process(Arc<AtomicUsize>);
    impl portable_pty::ChildKiller for Process {
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }
    impl portable_pty::Child for Process {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            self.0.fetch_add(1, Ordering::Relaxed);
            None
        }
        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }
    let signals = Arc::new(AtomicUsize::new(0));
    let mut child = Child {
        process: Box::new(Process(signals.clone())),
        reaped: false,
    };
    assert!(child.try_wait().unwrap().is_some());
    child.kill();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 8,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    child.terminate(pair.master.as_ref());
    drop(child);
    assert_eq!(signals.load(Ordering::Relaxed), 0);
}

#[cfg(unix)]
#[test]
fn closing_pty_kills_hup_resistant_descendants_and_releases_workers() {
    let root = tempfile::tempdir().unwrap();
    let mut service = PtyWorker::start(1);
    let id = service
        .open(request(
            root.path(),
            "trap '' HUP TERM; sleep 30 & printf 'REA'; while [ ! -f continue ]; do :; done; printf 'DY\\n'; wait",
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut output = Vec::new();
    while !output.windows(5).any(|text| text == b"READY") {
        for event in service.poll().unwrap() {
            if let PtyEvent::Output { bytes, .. } = event {
                output.extend(bytes);
            }
        }
        // Release the rest only after consuming the first fragment in a separate poll.
        if output == b"REA" {
            std::fs::write(root.path().join("continue"), b"").unwrap();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    service.close(id).unwrap();
    drain(&mut service, id);
    while service.residents.load(Ordering::Acquire) != 0 {
        assert!(
            Instant::now() < deadline,
            "PTY descendants retained I/O workers"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn closing_reaped_pty_cancels_reads_while_another_process_holds_the_slave() {
    use std::os::unix::process::CommandExt;
    struct Holder(std::process::Child);
    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut service = PtyWorker::start(1);
    let id = service
        .open(request(root.path(), "tty > slave; read value"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let slave = loop {
        service.poll().unwrap();
        if let Ok(path) = std::fs::read_to_string(root.path().join("slave"))
            && !path.trim().is_empty()
        {
            break std::fs::File::open(path.trim()).unwrap();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    };
    let mut holder = Holder(
        std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .stdin(slave)
            .spawn()
            .unwrap(),
    );
    service.input(id, b"done\n".to_vec(), 64).unwrap();
    loop {
        if service.poll().unwrap().iter().any(|event| matches!(event, PtyEvent::Exited { id: source, code: Some(0) } if *source == id)) {
            break;
        }
        assert!(Instant::now() < deadline, "original PTY child did not exit");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(holder.0.try_wait().unwrap().is_none());
    service.close(id).unwrap();
    let events = drain(&mut service, id);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PtyEvent::IoComplete { result: Ok(()), .. }))
    );
    assert!(
        holder.0.try_wait().unwrap().is_none(),
        "closing a reaped PTY must not signal its old process group"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while service.residents.load(Ordering::Acquire) != 0 {
        assert!(
            Instant::now() < deadline,
            "cancelled reader retained its slot"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let next = service.open(request(root.path(), "printf next")).unwrap();
    drain(&mut service, next);
}

#[cfg(unix)]
#[test]
fn interactive_pty_keeps_invocation_name() {
    let root = tempfile::tempdir().unwrap();
    let invocation = root.path().join("custom-sh");
    std::os::unix::fs::symlink("/bin/sh", &invocation).unwrap();
    let mut service = PtyWorker::start(1);
    let mut request = request(root.path(), "printf '%s' \"$0\"");
    request.program = crate::files::canonicalize(&invocation).unwrap();
    request.argv0 = Some(invocation.clone());
    request.policy.maximum_output_bytes = 0;
    let id = service.open(request).unwrap();
    let mut output = Vec::new();
    for event in drain(&mut service, id) {
        match event {
            PtyEvent::Output { bytes, .. } => output.extend(bytes),
            PtyEvent::IoComplete { result, .. } => result.unwrap(),
            _ => {}
        }
    }
    assert_eq!(
        String::from_utf8(output).unwrap().trim_end(),
        invocation.to_str().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn interactive_pty_output_has_no_lifetime_quota() {
    let root = tempfile::tempdir().unwrap();
    let mut service = PtyWorker::start(256);
    // 4224 blocks of 8192 bytes exceed the former 32 MiB lifetime limit.
    let mut request = request(
        root.path(),
        "block=x; i=0; while [ $i -lt 13 ]; do block=$block$block; i=$((i+1)); done; i=0; while [ $i -lt 4224 ]; do printf '%s' \"$block\"; i=$((i+1)); done",
    );
    request.policy.maximum_output_bytes = 0;
    request.policy.event_capacity = 256;
    let id = service.open(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut received = 0;
    loop {
        for event in service.poll().unwrap() {
            match event {
                PtyEvent::Output {
                    bytes, truncated, ..
                } => {
                    assert!(!truncated);
                    assert!(
                        bytes.iter().all(|byte| *byte == b'x'),
                        "unexpected PTY output: {:?}",
                        String::from_utf8_lossy(&bytes)
                    );
                    received += bytes.len();
                }
                PtyEvent::Exited { code, .. } => assert_eq!(code, Some(0)),
                PtyEvent::IoComplete { id: source, result } => {
                    assert_eq!(source, id);
                    result.unwrap();
                    assert_eq!(received, 33 * 1024 * 1024);
                    return;
                }
                PtyEvent::Started { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(Instant::now() < deadline, "received only {received} bytes");
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
#[test]
fn interactive_pty_output_has_no_lifetime_quota_and_keeps_invocation_name() {
    let root = tempfile::tempdir().unwrap();
    let invocation = root.path().join("custom-sh");
    std::os::unix::fs::symlink("/bin/sh", &invocation).unwrap();
    let mut service = PtyWorker::start(1);
    let mut request = request(root.path(), "printf '%s' \"$0\"");
    request.program = crate::files::canonicalize(&invocation).unwrap();
    request.argv0 = Some(invocation.clone());
    request.policy.maximum_output_bytes = 0;
    let id = service.open(request).unwrap();
    let mut output = Vec::new();
    for event in drain(&mut service, id) {
        match event {
            PtyEvent::Output { bytes, .. } => output.extend(bytes),
            PtyEvent::IoComplete { result, .. } => result.unwrap(),
            _ => {}
        }
    }
    assert_eq!(
        String::from_utf8(output).unwrap().trim_end(),
        invocation.to_str().unwrap()
    );
}
