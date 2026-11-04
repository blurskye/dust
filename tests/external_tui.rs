use dust::{
    process::{
        ExecutionPreflightEvent, ExecutionPreflightRequest, ExecutionPreflightService, PtyEvent,
        PtyId, PtyPolicy, PtyRequest, PtyService,
    },
    ui::{
        PtyInputKey, PtyScreen, encode_key, encode_paste,
        window::{
            ContentAdapterKind, ContentLifecycle, ContentTypeId, WindowError, WindowGraph,
            WindowPlacement, WindowRect,
        },
    },
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

// PtyService owns nonblocking cancellation on drop, including assertion unwinds.

fn wait_for_preflight(
    preflight: &ExecutionPreflightService,
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
    let mut preflight = ExecutionPreflightService::start(2);
    preflight
        .request(ExecutionPreflightRequest {
            executable,
            cwd: cwd.clone(),
            roots: vec![cwd],
            root_revision: 9,
        })
        .unwrap();
    let ready = wait_for_preflight(&preflight, deadline);
    let mut ptys = PtyService::start(8);
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
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("{message}"),
                _ => {}
            }
        }
        if code.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(code, Some(Some(0)));
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("version=0.61.1") && output.len() < 4096,
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
    let mut preflight = ExecutionPreflightService::start(2);
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
    let mut ptys = PtyService::start(32);
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
    let mut sent_quit = false;
    let mut exit = None;
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
                    if saw_alternate && !sent_quit {
                        ptys.resize(id, 120, 40).unwrap();
                        screen.resize(120, 40);
                        ptys.input(id, b"q".to_vec(), 16).unwrap();
                        sent_quit = true;
                    }
                }
                PtyEvent::Exited { id: event_id, code } if event_id == id => exit = Some(code),
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("{message}"),
                _ => {}
            }
        }
        if exit.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(saw_alternate && sent_quit);
    assert_eq!(exit, Some(Some(0)));
    assert!(!screen.alternate_active());
    eprintln!("LazyGit 0.61.1 launch/resize/q exit passed");
}

#[test]
fn canonical_preflight_portable_pty_drives_external_tui_fixture_without_shell() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_dust"))
        .canonicalize()
        .unwrap();
    let cwd = std::env::temp_dir().canonicalize().unwrap();
    let mut preflight = ExecutionPreflightService::start(2);
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
    let mut ptys = PtyService::start(16);
    let id = ptys
        .open(PtyRequest {
            program: ready.executable,
            argv0: None,
            arguments: vec![],
            cwd: ready.cwd,
            environment,
            columns: 40,
            rows: 8,
            root_revision: 7,
            policy: PtyPolicy::default(),
        })
        .unwrap();
    assert_eq!(id, PtyId(1), "one external TUI process was admitted");
    let mut screen = PtyScreen::new(40, 8, 1024);
    let mut raw = Vec::new();
    let mut sent = false;
    let mut exited = None;
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
                    if !sent
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
                        let window = graph
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

                        for placement in [
                            WindowPlacement::Tiled,
                            WindowPlacement::Docked {
                                edge: dust::ui::window::DockEdge::Right,
                                size: 12,
                            },
                            WindowPlacement::Fullscreen,
                            WindowPlacement::Hidden,
                            WindowPlacement::Floating(WindowRect {
                                x: 3,
                                y: 2,
                                width: 50,
                                height: 10,
                            }),
                        ] {
                            graph
                                .place(window, placement)
                                .expect("move live external TUI view");
                            assert_eq!(
                                graph.window(window).expect("same live window").placement,
                                placement
                            );
                            assert_eq!(graph.view(view).expect("same live view").content, content);
                            assert_eq!(ptys.pending(), 1, "moving the view never restarts its PTY");
                            assert_eq!(
                                (
                                    screen.cursor(),
                                    screen.alternate_active(),
                                    screen.application_cursor(),
                                    screen.bracketed_paste(),
                                ),
                                (
                                    screen_before.1,
                                    screen_before.3,
                                    screen_before.4,
                                    screen_before.5,
                                ),
                                "window movement does not replace the live screen"
                            );
                        }

                        ptys.resize(id, 50, 10).unwrap();
                        screen.resize(50, 10);
                        ptys.input(id, encode_key(PtyInputKey::Up, true).to_vec(), 1024)
                            .unwrap();
                        ptys.input(id, encode_paste("fixture", true, 1024).unwrap(), 1024)
                            .unwrap();
                        sent = true;
                    }
                }
                PtyEvent::Exited { id: event_id, code } if event_id == id => exited = Some(code),
                PtyEvent::Failed {
                    id: event_id,
                    message,
                } if event_id == id => panic!("pty failed: {message}"),
                _ => {}
            }
        }
        if exited.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(sent);
    assert_eq!(exited, Some(Some(0)));
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("INPUT_OK") && raw.contains("PRIMARY_OK"));
    assert!(!screen.alternate_active());
    assert!(!screen.bracketed_paste());
}
