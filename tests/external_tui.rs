use dust::process::ExecutionPreflightEvent;
use dust::process::ExecutionPreflightRequest;
use dust::process::ExecutionPreflightWorker;
use dust::process::PtyEvent;
use dust::process::PtyId;
use dust::process::PtyPolicy;
use dust::process::PtyRequest;
use dust::process::PtyWorker;
use dust::ui::PtyInputKey;
use dust::ui::PtyScreen;
use dust::ui::encode_key;
use dust::ui::encode_paste;
use dust::windows::ContentAdapterKind;
use dust::windows::ContentLifecycle;
use dust::windows::ContentTypeId;
use dust::windows::WindowError;
use dust::windows::WindowGraph;
use dust::windows::WindowPlacement;
use dust::windows::WindowRect;
use std::collections::BTreeMap;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;

// PtyWorker owns nonblocking cancellation on drop, including assertion unwinds.

fn wait_for_preflight(
    preflight: &ExecutionPreflightWorker,
    deadline: Instant,
) -> dust::process::ExecutionPreflightResult {
    loop {
        if let Some(event) = preflight.poll().unwrap().into_iter().next() {
            match event {
                ExecutionPreflightEvent::Ready(result) => return result,
                ExecutionPreflightEvent::Failed { message, .. } => panic!("preflight: {message}"),
            }
        }
        assert!(Instant::now() < deadline, "preflight timeout");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "operator probe: requires NixOS LazyGit absolute path"]
fn lazygit_version_uses_canonical_preflight_and_portable_pty_without_shell() {
    let executable = PathBuf::from("/run/current-system/sw/bin/lazygit");
    let cwd = std::env::temp_dir().canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut preflight = ExecutionPreflightWorker::start(2);
    preflight
        .request(ExecutionPreflightRequest {
            executable,
            cwd: cwd.clone(),
            roots: vec![cwd],
            root_revision: 9,
        })
        .unwrap();
    let ready = wait_for_preflight(&preflight, deadline);
    let mut ptys = PtyWorker::start(8);
    let id = ptys
        .open(PtyRequest {
            program: ready.executable,
            argv0: None,
            arguments: vec!["--version".into()],
            cwd: ready.cwd,
            environment: BTreeMap::new(),
            columns: 80,
            rows: 8,
            root_revision: 9,
            policy: PtyPolicy::default(),
        })
        .unwrap();
    let mut output = Vec::new();
    let mut code = None;
    let mut io_complete = false;
    while Instant::now() < deadline {
        for event in ptys.poll().unwrap() {
            match event {
                PtyEvent::Output {
                    id: event_id,
                    bytes,
                    ..
                } if event_id == id => output.extend(bytes),
                PtyEvent::Exited {
                    id: event_id,
                    code: c,
                } if event_id == id => code = Some(c),
                PtyEvent::IoComplete {
                    id: event_id,
                    result,
                } if event_id == id => {
                    result.expect("version probe I/O");
                    io_complete = true;
                }
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("{message}"),
                _ => {}
            }
        }
        if io_complete {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(io_complete, "version probe I/O timed out");
    assert_eq!(code, Some(Some(0)));
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("version=") && output.contains("os=") && output.len() < 4096,
        "{output}"
    );
    eprintln!("{}", output.trim());
}

#[test]
#[ignore = "operator probe: requires NixOS LazyGit absolute path"]
fn lazygit_launch_resize_and_quit_uses_preflight_pty() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("file.txt"), "fixture\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let executable = PathBuf::from("/run/current-system/sw/bin/lazygit");
    let cwd = dir.path().canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut preflight = ExecutionPreflightWorker::start(2);
    preflight
        .request(ExecutionPreflightRequest {
            executable,
            cwd: cwd.clone(),
            roots: vec![cwd],
            root_revision: 11,
        })
        .unwrap();
    let ready = wait_for_preflight(&preflight, deadline);
    let mut env = BTreeMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    let mut ptys = PtyWorker::start(32);
    let id = ptys
        .open(PtyRequest {
            program: ready.executable,
            argv0: None,
            arguments: vec![],
            cwd: ready.cwd,
            environment: env,
            columns: 100,
            rows: 30,
            root_revision: 11,
            policy: PtyPolicy::default(),
        })
        .unwrap();
    let mut screen = PtyScreen::new(100, 30, 4096);
    let mut saw_alternate = false;
    let mut requested_resize = false;
    let mut resized = false;
    let mut sent_quit = false;
    let mut exit = None;
    let mut io_complete = false;
    while Instant::now() < deadline {
        for event in ptys.poll().unwrap() {
            match event {
                PtyEvent::Output {
                    id: event_id,
                    bytes,
                    ..
                } if event_id == id => {
                    screen.feed_bytes(&bytes);
                    saw_alternate |= screen.alternate_active();
                    if saw_alternate && !requested_resize {
                        ptys.resize(id, 120, 40).unwrap();
                        requested_resize = true;
                    }
                    // LazyGit places its footer at the terminal's bottom edge. A
                    // redraw there confirms the child observed the larger height.
                    if resized && !sent_quit && !screen.lines()[39].trim().is_empty() {
                        ptys.input(id, b"q".to_vec(), 16).unwrap();
                        sent_quit = true;
                    }
                }
                PtyEvent::Resized {
                    id: event_id,
                    sequence: 0,
                    columns: 120,
                    rows: 40,
                } if event_id == id => {
                    assert!(requested_resize);
                    screen.resize(120, 40);
                    resized = true;
                }
                PtyEvent::Exited { id: event_id, code } if event_id == id => exit = Some(code),
                PtyEvent::IoComplete {
                    id: event_id,
                    result,
                } if event_id == id => {
                    result.expect("LazyGit I/O");
                    io_complete = true;
                }
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("{message}"),
                _ => {}
            }
        }
        if io_complete {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        saw_alternate && resized && sent_quit,
        "LazyGit resize redraw timed out"
    );
    assert!(io_complete, "LazyGit I/O timed out");
    assert_eq!(exit, Some(Some(0)));
    assert!(!screen.alternate_active());
    eprintln!("LazyGit launch/resize/q exit passed");
}

#[test]
fn external_tui_fixture() {
    if std::env::var("DUST_TEST_EXTERNAL_TUI_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    crossterm::terminal::enable_raw_mode().unwrap();
    let mut out = std::io::stdout();
    out.write_all(b"PRIMARY_START\r\n\x1b[?1049hALT_READY\r\n\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1003h\x1b[?1006h\x1b[38;5;255;48;2;1;2;3;1;4mX\x1b[0m").unwrap();
    out.flush().unwrap();
    let mut input = [0u8; 4096];
    let mut used = 0;
    loop {
        assert!(used < input.len(), "fixture input exceeded its limit");
        let read = std::io::stdin().read(&mut input[used..]).unwrap();
        assert_ne!(read, 0, "fixture input closed");
        used += read;
        let bytes = &input[..used];
        if bytes.windows(3).any(|w| w == b"\x1bOA")
            && bytes.windows(19).any(|w| w == b"\x1b[200~fixture\x1b[201~")
        {
            break;
        }
    }
    let (columns, rows) = crossterm::terminal::size().unwrap();
    write!(out, "SIZE:{columns}x{rows}\r\n").unwrap();
    out.write_all(b"INPUT_OK\r\n\x1b[?1006l\x1b[?1003l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b[?1049lPRIMARY_OK\r\n").unwrap();
    out.flush().unwrap();
    crossterm::terminal::disable_raw_mode().unwrap();
}

#[test]
fn production_binary_ignores_external_tui_fixture_environment() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dust"))
        .arg("--version")
        .env("DUST_TEST_EXTERNAL_TUI_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("dust "));
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn canonical_preflight_portable_pty_drives_external_tui_fixture_without_shell() {
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let cwd = std::env::temp_dir().canonicalize().unwrap();
    let mut preflight = ExecutionPreflightWorker::start(2);
    preflight
        .request(ExecutionPreflightRequest {
            executable: executable.clone(),
            cwd: cwd.clone(),
            roots: vec![cwd.clone()],
            root_revision: 7,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let ready = wait_for_preflight(&preflight, deadline);
    let mut environment = BTreeMap::new();
    environment.insert("DUST_TEST_EXTERNAL_TUI_FIXTURE".into(), "1".into());
    environment.insert("TERM".into(), "xterm-256color".into());
    let mut ptys = PtyWorker::start(16);
    let id = ptys
        .open(PtyRequest {
            program: ready.executable,
            argv0: None,
            arguments: vec![
                "--exact".into(),
                "external_tui_fixture".into(),
                "--nocapture".into(),
            ],
            cwd: ready.cwd,
            environment,
            columns: 40,
            rows: 8,
            root_revision: 7,
            policy: PtyPolicy::default(),
        })
        .unwrap();
    assert_eq!(id, PtyId(1), "one external TUI process was accepted");
    let mut screen = PtyScreen::new(40, 8, 1024);
    let mut raw = Vec::new();
    let mut requested_resize = false;
    let mut resized = false;
    let mut sent = false;
    let mut exited = None;
    let mut io_complete = false;
    while Instant::now() < deadline {
        for event in ptys.poll().unwrap() {
            match event {
                PtyEvent::Output {
                    id: event_id,
                    bytes,
                    ..
                } if event_id == id => {
                    raw.extend_from_slice(&bytes);
                    screen.feed_bytes(&bytes);
                    if !requested_resize
                        && raw.windows(9).any(|w| w == b"ALT_READY")
                        && screen.application_cursor()
                        && screen.bracketed_paste()
                        && screen.focus_reporting()
                        && screen.sgr_mouse()
                    {
                        assert!(screen.alternate_active());
                        assert!(screen.application_cursor());
                        assert!(screen.bracketed_paste());
                        assert!(screen.focus_reporting());
                        assert!(screen.sgr_mouse());

                        let mut graph = WindowGraph::default();
                        let tab = graph
                            .create_workspace_tab("external-tui-test", 1)
                            .expect("external TUI tab");
                        let content = graph
                            .create_content(
                                ContentTypeId::new("terminal.external-tui").expect("content type"),
                                ContentAdapterKind::ExternalTui,
                                "external TUI fixture".into(),
                                false,
                                "external-tui-test",
                                1,
                            )
                            .expect("external TUI content");
                        graph
                            .update_content_lifecycle(
                                content,
                                "external TUI fixture".into(),
                                false,
                                ContentLifecycle::Running,
                            )
                            .expect("running external TUI lifecycle");
                        let view = graph
                            .create_view(content, "external-tui-test", 1)
                            .expect("authoritative external TUI view");
                        graph
                            .open_window(
                                tab,
                                view,
                                WindowPlacement::Floating(WindowRect {
                                    x: 1,
                                    y: 1,
                                    width: 40,
                                    height: 8,
                                }),
                                true,
                                "external-tui-test",
                                1,
                            )
                            .expect("focused external TUI window");
                        let graph_before = graph.clone();
                        let screen_before = (
                            screen.dimensions(),
                            screen.cursor(),
                            screen.lines(),
                            screen.alternate_active(),
                            screen.application_cursor(),
                            screen.bracketed_paste(),
                            screen.focus_reporting(),
                            screen.sgr_mouse(),
                        );
                        assert_eq!(ptys.pending(), 1);
                        assert_eq!(
                            graph.require_clone_view(content),
                            Err(WindowError::Unsupported("clone-view"))
                        );
                        assert_eq!(
                            graph.create_view(content, "external-tui-test", 1),
                            Err(WindowError::Unsupported("clone-view"))
                        );
                        assert_eq!(graph, graph_before);
                        assert_eq!(
                            (
                                screen.dimensions(),
                                screen.cursor(),
                                screen.lines(),
                                screen.alternate_active(),
                                screen.application_cursor(),
                                screen.bracketed_paste(),
                                screen.focus_reporting(),
                                screen.sgr_mouse(),
                            ),
                            screen_before
                        );
                        assert_eq!(ptys.pending(), 1);

                        // Live window movement is exercised through TestSession in
                        // real_pty_geometry_ack_focus_placement_and_cross_tab_keep_one_process.

                        ptys.resize(id, 50, 10).unwrap();
                        requested_resize = true;
                    }
                }
                PtyEvent::Resized {
                    id: event_id,
                    sequence: 0,
                    columns: 50,
                    rows: 10,
                } if event_id == id => {
                    assert!(requested_resize);
                    resized = true;
                    screen.resize(50, 10);
                    ptys.input(id, encode_key(PtyInputKey::Up, true).to_vec(), 1024)
                        .unwrap();
                    ptys.input(id, encode_paste("fixture", true, 1024).unwrap(), 1024)
                        .unwrap();
                    sent = true;
                }
                PtyEvent::Exited { id: event_id, code } if event_id == id => exited = Some(code),
                PtyEvent::IoComplete {
                    id: event_id,
                    result,
                } if event_id == id => {
                    result.expect("fixture I/O");
                    io_complete = true;
                }
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("pty failed: {message}"),
                _ => {}
            }
        }
        if io_complete {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(resized && sent, "fixture resize acknowledgement timed out");
    assert!(io_complete, "fixture I/O timed out");
    assert_eq!(exited, Some(Some(0)));
    let raw = String::from_utf8_lossy(&raw);
    assert!(
        raw.contains("SIZE:50x10"),
        "child did not observe resize: {raw}"
    );
    assert!(
        raw.contains("INPUT_OK") && raw.contains("PRIMARY_OK"),
        "{raw}"
    );
    assert!(!screen.alternate_active());
    assert!(!screen.bracketed_paste());
}

#[cfg(unix)]
#[test]
fn pty_resize_ack_precedes_the_childs_resize_output() {
    let root = tempfile::tempdir().unwrap();
    let mut service = PtyWorker::start(1);
    let script = root.path().join("resize.py");
    std::fs::write(&script, "import os,signal,time\ndef resized(*_): os.write(1, b'\\x1b[1;999HX')\nsignal.signal(signal.SIGWINCH, resized)\nos.write(1, b'\\x1b[?1049hREADY')\ntime.sleep(30)\n").unwrap();
    let mut request = PtyRequest {
        program: "/bin/sh".into(),
        argv0: None,
        arguments: Vec::new(),
        cwd: root.path().into(),
        environment: Default::default(),
        columns: 80,
        rows: 8,
        root_revision: 3,
        policy: PtyPolicy {
            event_capacity: 1,
            ..Default::default()
        },
    };
    request.program =
        dust::tools::discover_executable("python3").expect("PTY resize fixture needs Python 3");
    request.arguments = vec![script.as_os_str().to_owned()];
    let id = service.open(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut screen = dust::ui::PtyScreen::new(80, 8, 8);
    while !screen.lines().iter().any(|line| line.contains("READY")) {
        for event in service.poll().unwrap() {
            if let PtyEvent::Output { bytes, .. } = event {
                screen.feed_bytes(&bytes);
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    service.resize(id, 40, 8).unwrap();
    let mut acknowledged = false;
    loop {
        for event in service.poll().unwrap() {
            match event {
                PtyEvent::Resized { columns, rows, .. } => {
                    acknowledged = true;
                    screen.resize(columns, rows);
                }
                PtyEvent::Output { bytes, .. } => {
                    assert!(acknowledged, "resize output overtook the acknowledgement");
                    screen.feed_bytes(&bytes);
                }
                _ => {}
            }
        }
        if screen.history_row(0).unwrap()[39].text == "X" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing resize frame: {:?}",
            screen.lines()
        );
        thread::sleep(Duration::from_millis(2));
    }
    service.close(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let batch = service.poll().unwrap();
        if batch.iter().any(|event| {
            matches!(event,
            PtyEvent::IoComplete { id: source, .. }
            | PtyEvent::Failed { id: source, .. }
            | PtyEvent::Unknown { id: source, .. } if *source == id)
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "PTY did not close: {batch:?}");
        thread::sleep(Duration::from_millis(1));
    }
}
