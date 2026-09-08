use super::*;
use crate::process::PtyPolicy;
use std::{path::Path, time::Instant};

fn request(root: &Path, script: &str) -> PtyRequest {
    PtyRequest {
        program: "/bin/sh".into(),
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
fn drain(service: &mut PtyService, id: PtyId) -> Vec<PtyEvent> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut received = Vec::new();
    loop {
        let batch = service.poll().unwrap();
        let done=batch.iter().any(|e|matches!(e,PtyEvent::Exited{id:source,..}|PtyEvent::Failed{id:source,..} if *source==id));
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
#[cfg(unix)]
#[test]
fn pty_worker_reserves_real_capacity_and_preserves_stream_before_terminal() {
    let root = tempfile::tempdir().unwrap();
    let mut missing = PtyService::start(1);
    let id = missing
        .open(request(&root.path().join("missing"), "printf forbidden"))
        .expect("foreground admits; worker inspects directory");
    let events = drain(&mut missing, id);
    assert!(
        matches!(&events[..],[PtyEvent::Failed{message,..}] if message.contains("working directory"))
    );

    let mut service = PtyService::start(1);
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
    service.resize(id, 61, 9).unwrap();
    service.input(id, b"hello\n".to_vec(), 64).unwrap();
    // A capacity-one undrained output queue must backpressure, not drop escape/text bytes.
    thread::sleep(Duration::from_millis(20));
    let events = drain(&mut service, id);
    let mut resized = false;
    for event in events {
        match event {
            PtyEvent::Output {
                bytes, truncated, ..
            } => {
                assert!(!truncated);
                output.extend(bytes);
            }
            PtyEvent::Resized { columns, rows, .. } => {
                assert_eq!((columns, rows), (61, 9));
                resized = true;
            }
            PtyEvent::Exited { code, .. } => assert_eq!(code, Some(0)),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(resized);
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
    let identity = crate::workspace::RootIdentity::capture(&target).unwrap();
    std::fs::rename(&target, root.path().join("old-scope")).unwrap();
    std::fs::create_dir(&target).unwrap();
    let mut replaced = PtyService::start(1);
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

    let mut cancelled = PtyService::start(1);
    let id = cancelled
        .open(request(
            root.path(),
            "printf 'READY\n'; while :; do :; done",
        ))
        .unwrap();
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
    let mut limited = PtyService::start(1);
    let mut oversized = request(root.path(), "printf 123456789");
    oversized.policy.maximum_output_bytes = 4;
    let id = limited.open(oversized).unwrap();
    assert!(drain(&mut limited, id).iter().any(
        |e| matches!(e,PtyEvent::Failed{message,..} if message.contains("output limit exceeded"))
    ));
    let mut detached = PtyService::start(1);
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
