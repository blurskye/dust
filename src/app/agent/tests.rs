//! Tests for agent conversations and permissions.

use crate::app::agent::*;

fn test_app() -> ((tempfile::TempDir, tempfile::TempDir), App) {
    test_app_with(|_| {})
}

fn test_app_with(
    change: impl FnOnce(&mut crate::config::Config),
) -> ((tempfile::TempDir, tempfile::TempDir), App) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.persistence.enabled = false;
    change(&mut loaded.config);
    let mut app = App::new(
        crate::app::startup::StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    ((root, state), app)
}

#[cfg(unix)]
#[test]
fn add_tools_runs_once_with_this_dust_and_only_when_allowed() {
    let marker = tempfile::tempdir().unwrap();
    let file = marker.path().join("added");
    let command = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "printf '%s\\n' \"$1\" >> \"$0\"".to_owned(),
        file.display().to_string(),
        "{dust}".to_owned(),
    ];
    let set = |allowed: bool| {
        let command = command.clone();
        move |config: &mut crate::config::Config| {
            config.agents.add_tools = allowed;
            config.agents.profiles.get_mut("pi").unwrap().add_tools = command;
        }
    };
    let (_dirs, mut app) = test_app_with(set(true));
    app.add_agent_tools("pi");
    app.add_agent_tools("pi");
    app.add_agent_tools("claude");
    let dust = std::env::current_exe().unwrap().display().to_string();
    // The command runs in the background.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{dust}\n"));

    std::fs::remove_file(&file).unwrap();
    let (_dirs, mut app) = test_app_with(set(false));
    app.add_agent_tools("pi");
    assert!(!file.exists(), "nothing runs when add_tools is off");
}

#[test]
fn the_agent_picker_starts_on_the_default_agent() {
    let (_dirs, mut app) = test_app_with(|config| config.agents.default = "pi".into());
    app.open_agent_picker();
    let selected = app
        .search
        .picker()
        .and_then(|picker| picker.selected_item());
    assert_eq!(selected.map(|item| item.id.0.as_str()), Some("agent:pi"));
}

#[cfg(unix)]
#[test]
fn opening_the_panel_starts_the_default_agent_before_anything_is_asked() {
    let fixture = |config: &mut crate::config::Config| {
        config.agents.default = "claude".into();
        config.agents.add_tools = false;
        let profile = config.agents.profiles.get_mut("claude").unwrap();
        profile.program = "cat".into();
        profile.arguments.clear();
    };
    let (_dirs, mut app) = test_app_with(fixture);
    app.begin_agent_question();
    assert_eq!(app.agent.sessions.len(), 1);
    assert_eq!(app.agent.sessions[0].name, "claude");
    assert!(app.agent.sessions[0].process.is_some());
    app.toggle_agent_panel();
    app.toggle_agent_panel();
    assert_eq!(
        app.agent.sessions.len(),
        1,
        "one session, not one per opening"
    );

    let (_dirs, mut app) = test_app_with(|config| {
        fixture(config);
        config.agents.default = String::new();
    });
    app.begin_agent_question();
    assert!(
        app.agent.sessions.is_empty(),
        "Ask every time waits for the pick"
    );
}

#[test]
fn ctrl_t_starts_the_default_agent_and_asks_only_without_one() {
    let (_dirs, mut app) = test_app_with(|config| {
        config.agents.default = "claude".into();
        config.agents.add_tools = false;
        config.agents.profiles.get_mut("claude").unwrap().program =
            "/nonexistent/dust-test-agent".into();
    });
    app.new_agent_tab();
    assert!(app.search.picker().is_none(), "the default needs no asking");

    let (_dirs, mut app) = test_app_with(|config| config.agents.default = String::new());
    app.new_agent_tab();
    assert_eq!(
        app.search
            .picker()
            .map(|picker| picker.presentation().title),
        Some("New Agent Session".to_owned())
    );
}

fn tab(app: &mut App) -> usize {
    app.add_agent_tab(
        "fixture".into(),
        app.state.workspace_root.clone(),
        Vec::new(),
        None,
        None,
    )
    .unwrap()
}

fn child(app: &mut App, parent: u64, phase: Phase) -> usize {
    let index = tab(app);
    let session = &mut app.agent.sessions[index];
    session.parent = Some(parent);
    session.phase = phase;
    session
        .queued
        .push_back(vec![PromptPart::Text("task".into())]);
    index
}

fn permission(number: u64) -> PermissionRequest {
    PermissionRequest {
        session: number,
        id: json!("permission"),
        action: "run a tool",
        title: String::new(),
        options: Vec::new(),
    }
}

#[cfg(unix)]
fn subagent_reply() -> (
    crate::remote::RemoteServer,
    Reply,
    std::io::BufReader<crate::remote::UnixStream>,
) {
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let server = crate::remote::RemoteServer::start(directory.path()).unwrap();
    let mut stream = crate::remote::UnixStream::connect(server.path()).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "agent\t{}", json!({"parent": 1, "prompt": "task"})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(crate::remote::RemoteRequest::Agent(request)) = server.try_request() {
            return (server, request.reply, std::io::BufReader::new(stream));
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn echo_agent(app: &mut App) -> usize {
    let index = tab(app);
    let session = &mut app.agent.sessions[index];
    session.process =
        Some(AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap());
    session.id = Some("s".into());
    index
}

#[cfg(unix)]
fn file_response(app: &mut App, index: usize) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.process_vfs_results();
        if let Some(AgentMessage::Response { result, .. }) = app.agent.sessions[index]
            .process
            .as_ref()
            .unwrap()
            .try_message()
        {
            return result;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn frame(app: &App) -> crate::ui::Frame {
    crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    )
}

#[test]
fn agent_fixture_removes_workspace_and_state_after_application_shutdown() {
    let (directories, app) = test_app();
    let root = directories.0.path().to_path_buf();
    let state = directories.1.path().to_path_buf();
    assert!(root.is_dir() && state.is_dir());
    drop(app);
    drop(directories);
    assert!(!root.exists() && !state.exists());
}

#[test]
fn agent_narrow_transcript_uses_the_panes_content_width() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    app.agent.active = index;
    app.agent.sessions[index].entries = vec![Entry::Agent("abcdefghijklmnop".into())];
    app.show_agent_panel();
    app.window_session.observe_size((10, 24));
    app.reconcile_editor_windows();
    app.follow_agent_pane();
    let panel = app.ui.agent_panel.as_ref().unwrap();
    let viewport = crate::ui::view_pane_viewport(&app.state, &app.ui, panel.view).unwrap();
    assert_eq!(viewport.width, 10);
    assert_eq!(app.agent.width, 8);
    assert_eq!(panel.transcript.text, "abcdefgh\nijklmnop\n");
    let frame = frame(&app);
    assert_eq!(frame.grid.row_text(viewport.y + 1).trim(), "abcdefgh");
    assert_eq!(frame.grid.row_text(viewport.y + 2).trim(), "ijklmnop");
    app.follow_agent_pane();
    assert_eq!(app.agent.width, 8);
}

#[test]
fn agent_streaming_keeps_scrolled_rows_in_place_and_follows_at_the_bottom() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    app.agent.active = index;
    app.agent.sessions[index].entries = vec![Entry::Agent(
        (0..80).map(|row| format!("row {row:02}\n")).collect(),
    )];
    app.show_agent_panel();
    app.scroll_agent(40);
    let panel = app.ui.agent_panel.as_ref().unwrap();
    let viewport = crate::ui::view_pane_viewport(&app.state, &app.ui, panel.view).unwrap();
    let first = frame(&app).grid.row_text(viewport.y + 1);
    apply_update(
        &mut app.agent.sessions[index],
        SessionUpdate::AgentText((80..90).map(|row| format!("row {row:02}\n")).collect()),
    );
    app.refresh_agent_transcript();
    assert_eq!(app.agent.scroll, 50);
    assert_eq!(frame(&app).grid.row_text(viewport.y + 1), first);
    app.scroll_agent(i64::MIN);
    apply_update(
        &mut app.agent.sessions[index],
        SessionUpdate::AgentText("last row\n".into()),
    );
    app.refresh_agent_transcript();
    assert_eq!(app.agent.scroll, 0);
}

#[test]
fn agent_scroll_top_has_no_invisible_excess_offset() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    app.agent.active = index;
    app.agent.sessions[index].entries = vec![Entry::Agent(
        (0..100).map(|row| format!("row {row:02}\n")).collect(),
    )];
    app.show_agent_panel();
    let viewport =
        crate::ui::view_pane_viewport(&app.state, &app.ui, app.agent_view().unwrap()).unwrap();
    let maximum = 100 - usize::from(viewport.height - 5);
    app.scroll_agent(i64::MAX);
    assert_eq!(app.agent.scroll, maximum);
    app.scroll_agent(-3);
    assert_eq!(app.agent.scroll, maximum - 3);
    assert_eq!(frame(&app).grid.row_text(viewport.y + 1).trim(), "row 03");
}

#[test]
fn agent_input_moves_and_deletes_whole_graphemes() {
    let (_directories, mut app) = test_app();
    for grapheme in ["👩‍💻", "e\u{301}"] {
        app.agent.input = format!("{grapheme}x");
        app.agent.cursor = app.agent.input.len();
        app.agent_input_edit("left");
        assert_eq!(app.agent.cursor, grapheme.len());
        app.agent_input_edit("left");
        assert_eq!(app.agent.cursor, 0);
        app.agent_input_edit("right");
        assert_eq!(app.agent.cursor, grapheme.len());
        app.agent_input_edit("backspace");
        assert_eq!(app.agent.input, "x");
        app.agent.input = format!("{grapheme}x");
        app.agent.cursor = 0;
        app.agent_input_edit("delete");
        assert_eq!(app.agent.input, "x");
        app.agent.input = format!("{grapheme}x\n{grapheme}y");
        app.agent.cursor = grapheme.len();
        app.agent_input_edit("down");
        assert_eq!(app.agent.cursor, 2 * grapheme.len() + 2);
        app.agent_input_edit("up");
        assert_eq!(app.agent.cursor, grapheme.len());
        app.agent_input_edit("normal");
        assert_eq!(app.agent.cursor, 0);
        app.agent_input_edit("append");
        assert_eq!(app.agent.cursor, grapheme.len());
        app.agent_input_edit("delete_word");
        assert_eq!(app.agent.input, format!("x\n{grapheme}y"));
        assert_eq!(app.agent.cursor, 0);
    }
}

#[test]
fn agent_normal_input_stays_on_line_characters_and_preserves_separators() {
    let (_directories, mut app) = test_app();
    app.agent.input = "ab\ncd".into();
    app.agent.cursor = 1;
    app.agent.normal = true;
    app.agent_input_edit("right");
    assert_eq!(app.agent.cursor, 1);
    app.agent_input_edit("delete");
    assert_eq!(app.agent.input, "a\ncd");
    assert_eq!(app.agent.cursor, 0);
    app.agent_input_edit("left");
    assert_eq!(app.agent.cursor, 0);
    app.agent_input_edit("down");
    app.agent_input_edit("backspace");
    assert_eq!(app.agent.input, "a\ncd");
    assert_eq!(app.agent.cursor, 2);
    app.agent_input_edit("end");
    assert_eq!(app.agent.cursor, 3);
    app.agent_input_edit("delete_to_end");
    assert_eq!(app.agent.input, "a\nc");
    assert_eq!(app.agent.cursor, 2);
    app.agent.input = "\nx".into();
    app.agent.cursor = 0;
    app.agent_input_edit("delete");
    assert_eq!(app.agent.input, "\nx");
    app.agent_input_edit("right");
    assert_eq!(app.agent.cursor, 0);
}

#[test]
fn agent_insert_home_starts_after_indentation_but_home_stays_at_zero() {
    let (_directories, mut app) = test_app();
    app.agent.input = "  foo".into();
    app.agent.cursor = 4;
    app.agent.normal = true;
    app.agent_input_edit("home");
    assert_eq!(app.agent.cursor, 0);
    app.agent_input_edit("insert_home");
    assert!(!app.agent.normal);
    assert_eq!(app.agent.cursor, 2);
    app.agent_input_insert("X");
    assert_eq!(app.agent.input, "  Xfoo");
}

#[test]
fn agent_vim_word_motions_stop_at_punctuation_and_graphemes() {
    let (_directories, mut app) = test_app();
    app.agent.input = "foo.bar baz".into();
    app.agent.normal = true;
    for expected in [3, 4, 8] {
        app.agent_input_edit("word_forward");
        assert_eq!(app.agent.cursor, expected);
    }
    for expected in [4, 3, 0] {
        app.agent_input_edit("word_back");
        assert_eq!(app.agent.cursor, expected);
    }
    for expected in [2, 3, 6, 10] {
        app.agent_input_edit("word_end");
        assert_eq!(app.agent.cursor, expected);
    }
    app.agent.input = "e\u{301}.👩‍💻 x".into();
    app.agent.cursor = 0;
    app.agent_input_edit("word_forward");
    assert_eq!(app.agent.cursor, 3);
    app.agent_input_edit("word_back");
    assert_eq!(app.agent.cursor, 0);
    app.agent_input_edit("word_end");
    assert_eq!(app.agent.cursor, 4);
}

#[cfg(unix)]
#[test]
fn startup_text_comes_before_a_question_typed_while_the_agent_started() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    app.agent.active = index;
    app.agent.sessions[index].phase = Phase::Creating(7);
    app.agent_ask("hello".into());
    app.handle_agent_response(index, 7, Ok(json!({"sessionId": "s"})));
    let session = &mut app.agent.sessions[index];
    apply_update(session, SessionUpdate::AgentText("pi v1.4.0\n".into()));
    assert_eq!(session.phase, Phase::Ready, "the question waits");
    std::thread::sleep(STARTUP_TEXT_WAIT);
    session.send_next_prompt();
    assert!(matches!(session.phase, Phase::Prompting(_)));
    apply_update(session, SessionUpdate::AgentText("Hi!".into()));
    assert_eq!(
        session.entries,
        vec![
            Entry::Agent("pi v1.4.0\n".into()),
            Entry::User {
                text: "hello".into(),
                context: vec![],
                attachments: vec![]
            },
            Entry::Agent("Hi!".into()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn agent_queued_questions_start_new_transcript_turns_only_when_sent() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    app.agent.active = index;
    app.agent_ask("first".into());
    let Phase::Prompting(first) = app.agent.sessions[index].phase else {
        panic!("prompting");
    };
    apply_update(
        &mut app.agent.sessions[index],
        SessionUpdate::AgentText("answer".into()),
    );
    app.agent.context = vec![PromptPart::Text("attached context".into())];
    app.agent_ask("second".into());
    apply_update(
        &mut app.agent.sessions[index],
        SessionUpdate::AgentText(" one".into()),
    );
    assert_eq!(app.agent.sessions[index].entries.len(), 2);
    app.handle_agent_response(index, first, Ok(json!({"stopReason": "end_turn"})));
    apply_update(
        &mut app.agent.sessions[index],
        SessionUpdate::AgentText("answer two".into()),
    );
    assert_eq!(
        app.agent.sessions[index].entries,
        vec![
            Entry::User {
                text: "first".into(),
                context: vec![],
                attachments: vec![]
            },
            Entry::Agent("answer one".into()),
            Entry::User {
                text: "second".into(),
                context: vec![context_label(&PromptPart::Text("attached context".into()))],
                attachments: vec![PromptPart::Text("attached context".into())]
            },
            Entry::Agent("answer two".into()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn agent_cancel_full_writer_stops_the_parent_instead_of_losing_cancel() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    app.agent.active = index;
    let session = &mut app.agent.sessions[index];
    session.id = Some("s".into());
    session.phase = Phase::Prompting(1);
    session.process = Some(
        AgentProcess::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 60".into()],
            &BTreeMap::new(),
            &session.root,
        )
        .unwrap(),
    );
    let process = session.process.as_mut().unwrap();
    assert!((0..32).any(|_| {
        process
            .notify("busy", json!({"text": "x".repeat(128 * 1024)}))
            .is_err()
    }));
    app.cancel_agent_turn();
    assert!(app.agent.sessions[index].process.is_none());
    assert_eq!(app.agent.sessions[index].phase, Phase::Stopped);
    assert!(
        app.ui
            .notification
            .as_ref()
            .unwrap()
            .contains("could not send cancellation")
    );
}

#[cfg(unix)]
#[test]
fn agent_cancel_rejects_late_subagent_requests() {
    use std::io::BufRead;
    for cancel in [false, true] {
        let (_server, reply, mut answer) = subagent_reply();
        let (_directories, mut app) = test_app();
        let index = echo_agent(&mut app);
        app.agent.active = index;
        if cancel {
            app.agent.sessions[index].phase = Phase::Prompting(1);
            app.cancel_agent_turn();
        }
        app.start_subagent(AgentRequest {
            parent: app.agent.sessions[index].number,
            description: String::new(),
            prompt: "task".into(),
            agent: None,
            model: None,
            reply,
        });
        assert_eq!(app.agent.sessions.len(), 1);
        let mut line = String::new();
        answer.read_line(&mut line).unwrap();
        assert!(
            serde_json::from_str::<Value>(&line).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("no longer active")
        );
    }
}

#[cfg(unix)]
#[test]
fn agent_subagent_partial_answers_do_not_hide_failed_turns() {
    use std::io::BufRead;
    for outcome in ["error", "exit", "success"] {
        let (_server, reply, mut answer) = subagent_reply();
        let (_directories, mut app) = test_app();
        let index = echo_agent(&mut app);
        let session = &mut app.agent.sessions[index];
        session.parent = Some(99);
        session.reply = Some(reply);
        session.phase = Phase::Prompting(10);
        session.entries = vec![
            Entry::User {
                text: "task".into(),
                context: vec![],
                attachments: vec![],
            },
            Entry::Agent("partial answer".into()),
        ];
        match outcome {
            "error" => {
                app.handle_agent_response(index, 10, Err("prompt failed".into()));
            }
            "exit" => {
                app.handle_agent_message(index, AgentMessage::Exited("unexpected exit".into()))
            }
            _ => {
                app.handle_agent_response(index, 10, Ok(json!({"stopReason": "end_turn"})));
            }
        }
        assert!(app.settle_subagent(index));
        let mut line = String::new();
        answer.read_line(&mut line).unwrap();
        let result: Value = serde_json::from_str(&line).unwrap();
        if outcome == "success" {
            assert_eq!(result["text"], "partial answer");
        } else {
            assert!(result["error"].is_string(), "{result}");
        }
    }
}

#[cfg(unix)]
#[test]
fn each_new_agent_tab_is_its_own_even_while_the_others_are_empty() {
    let (_directories, mut app) = test_app();
    let mut config = app.configuration.fixture_snapshot();
    let mut profile = config
        .settings
        .agents
        .profiles
        .values()
        .next()
        .unwrap()
        .clone();
    profile.program = "cat".into();
    profile.arguments.clear();
    config
        .settings
        .agents
        .profiles
        .insert("fixture".into(), profile);
    app.configuration.install_fixture(config);
    for _ in 0..3 {
        app.open_agent_tab("fixture").unwrap();
    }
    assert_eq!(app.agent.sessions.len(), 3);
    assert_eq!(app.agent.active, 2, "the newest tab is shown");
}

#[cfg(unix)]
#[test]
fn agent_tab_passes_its_remote_socket_to_the_mcp_server() {
    let directory = tempfile::tempdir().unwrap();
    let (_app_dirs, mut app) = test_app();
    let server = crate::remote::RemoteServer::start(directory.path()).unwrap();
    let socket = server.path().to_str().unwrap().to_owned();
    app.remote = Some(server);
    let index = tab(&mut app);
    let tools = &app.agent.sessions[index].tools;
    assert_eq!(tools[0]["env"][0]["value"], socket);
    assert_eq!(
        tools[0]["args"][1],
        app.agent.sessions[index].number.to_string()
    );
}

#[cfg(unix)]
#[test]
fn agent_resume_reuses_tabs_and_reports_unreadable_history() {
    let (_app_dirs, mut app) = test_app();
    let index = tab(&mut app);
    let conversation = app.agent.sessions[index].conversation();
    assert_eq!(app.reopen_conversation(&conversation.id).unwrap(), index);
    assert_eq!(app.agent.sessions.len(), 1);
    history::save(
        app.agent.directory.clone(),
        conversation.clone(),
        Vec::new(),
    );
    history::flush(
        &app.agent.directory,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .unwrap();
    app.agent.sessions.clear();
    let index = app.reopen_conversation(&conversation.id).unwrap();
    assert_eq!(app.agent.sessions[index].history, conversation.id);
    assert_eq!(app.agent.sessions[index].root, conversation.root);
    let started = app.agent.started;
    assert_eq!(
        app.reopen_conversation("missing").unwrap_err(),
        "that conversation could not be read"
    );
    assert_eq!(app.agent.sessions.len(), 1);
    assert_eq!(app.agent.started, started);
}

#[cfg(unix)]
#[test]
fn agent_mcp_cancellation_stops_only_the_requested_child() {
    let (_server, reply, connection) = subagent_reply();
    let (_other_server, other_reply, _other_connection) = subagent_reply();
    let (_app_dirs, mut app) = test_app();
    let parent = tab(&mut app);
    let number = app.agent.sessions[parent].number;
    let cancelled = child(&mut app, number, Phase::Prompting(2));
    let other = child(&mut app, number, Phase::Creating(3));
    app.agent.sessions[cancelled].reply = Some(reply);
    app.agent.sessions[other].reply = Some(other_reply);
    for index in [cancelled, other] {
        app.agent.sessions[index].process = Some(
            AgentProcess::spawn(
                Path::new("cat"),
                &[],
                &BTreeMap::new(),
                &app.state.workspace_root,
            )
            .unwrap(),
        );
    }
    let cancelled_number = app.agent.sessions[cancelled].number;
    app.agent
        .permissions
        .push_back(permission(cancelled_number));
    app.agent.asking = Some(permission(cancelled_number));
    connection
        .get_ref()
        .shutdown(std::net::Shutdown::Both)
        .unwrap();
    app.process_agent_events();
    let session = &app.agent.sessions[cancelled];
    assert_eq!(session.phase, Phase::Stopped);
    assert!(session.process.is_none());
    assert!(session.queued.is_empty());
    assert!(session.reply.is_none());
    assert!(app.agent.permissions.is_empty());
    assert!(app.agent.asking.is_none());
    assert_eq!(app.agent.sessions[other].phase, Phase::Creating(3));
    assert!(app.agent.sessions[other].reply.is_some());
    assert_eq!(app.agent.sessions[parent].phase, Phase::Ready);
}

#[cfg(unix)]
#[test]
fn agent_cancel_stops_starting_children_and_their_permissions() {
    use std::io::BufRead;
    let (_server, reply, mut answer) = subagent_reply();
    let (_directories, mut app) = test_app();
    let parent = tab(&mut app);
    let number = app.agent.sessions[parent].number;
    let first = child(&mut app, number, Phase::Initializing(1));
    let second = child(&mut app, number, Phase::Creating(2));
    app.agent.sessions[first].reply = Some(reply);
    let finished = child(&mut app, number, Phase::Ready);
    app.agent.sessions[finished].queued.clear();
    app.agent
        .permissions
        .push_back(permission(app.agent.sessions[finished].number));
    let unrelated = tab(&mut app);
    let unrelated_number = app.agent.sessions[unrelated].number;
    app.agent
        .permissions
        .push_back(permission(app.agent.sessions[first].number));
    app.agent
        .permissions
        .push_back(permission(unrelated_number));
    app.agent.asking = Some(permission(app.agent.sessions[second].number));
    app.agent.active = parent;
    // Even a parent between turns may have children that are still starting.
    app.cancel_agent_turn();
    for index in [first, second] {
        assert!(app.agent.sessions[index].queued.is_empty());
        assert_eq!(app.agent.sessions[index].phase, Phase::Stopped);
    }
    assert!(app.agent.asking.is_none());
    assert_eq!(app.agent.sessions[finished].phase, Phase::Ready);
    assert_eq!(app.agent.permissions.len(), 1);
    assert_eq!(app.agent.permissions[0].session, unrelated_number);
    let mut line = String::new();
    answer.read_line(&mut line).unwrap();
    assert!(
        serde_json::from_str::<Value>(&line).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("parent")
    );
    assert!(app.agent.sessions[first].reply.is_none());
}

#[test]
fn tutor_exit_closes_practice_agents_and_preserves_other_sessions() {
    let (_app_dirs, mut app) = test_app();
    let home = app.state.workspace_root.clone();
    let unrelated = tab(&mut app);
    let number = app.agent.sessions[unrelated].number;
    app.open_tutor();
    app.handle_configured_key("enter".parse().unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    app.tutor_tick();
    let practice = app.state.workspace_root.clone();
    assert_ne!(practice, home);
    let parent = tab(&mut app);
    let parent_number = app.agent.sessions[parent].number;
    let child = child(&mut app, parent_number, Phase::Initializing(1));
    app.agent
        .permissions
        .push_back(permission(app.agent.sessions[child].number));
    app.agent.active = unrelated;
    app.tutor_quit();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    app.tutor_tick();
    assert_eq!(app.state.workspace_root, home);
    assert!(!practice.exists());
    assert_eq!(app.agent.sessions.len(), 1);
    assert_eq!(app.agent.sessions[app.agent.active].number, number);
    assert!(app.agent.permissions.is_empty());
}

#[cfg(unix)]
#[test]
fn agent_close_and_exit_stop_children() {
    use std::io::BufRead;
    for close in [true, false] {
        let (_server, reply, mut answer) = subagent_reply();
        let (_directories, mut app) = test_app();
        let parent = tab(&mut app);
        let number = app.agent.sessions[parent].number;
        let child = child(&mut app, number, Phase::Prompting(2));
        let child_number = app.agent.sessions[child].number;
        app.agent.sessions[child].reply = Some(reply);
        app.agent.permissions.push_back(permission(child_number));
        app.agent.active = parent;
        if close {
            app.close_agent_session();
        } else {
            app.handle_agent_message(parent, AgentMessage::Exited("fixture exited".into()));
        }
        let child = app
            .agent
            .sessions
            .iter()
            .find(|session| session.number == child_number)
            .unwrap();
        assert_eq!(child.phase, Phase::Stopped);
        assert!(child.process.is_none());
        assert!(child.queued.is_empty());
        assert!(app.agent.permissions.is_empty());
        assert!(child.reply.is_none());
        let mut line = String::new();
        answer.read_line(&mut line).unwrap();
        assert!(
            serde_json::from_str::<Value>(&line).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("parent")
        );
    }
}

#[cfg(unix)]
#[test]
fn agent_response_transport_failure_stops_children_and_permissions() {
    let (_directories, mut app) = test_app();
    let parent = tab(&mut app);
    let number = app.agent.sessions[parent].number;
    let child = child(&mut app, number, Phase::Creating(2));
    app.agent.permissions.push_back(permission(number));
    app.agent
        .permissions
        .push_back(permission(app.agent.sessions[child].number));
    let mut process = AgentProcess::spawn(
        &crate::debugger::test_python(),
        &["-c".into(), "import os, sys, time; os.close(0); print('ready', file=sys.stderr, flush=True); time.sleep(60)".into()],
        &BTreeMap::new(),
        &app.state.workspace_root,
    ).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if matches!(process.try_message(), Some(AgentMessage::Log(line)) if line == "ready") {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    process.notify("fixture", Value::Null).unwrap();
    loop {
        if matches!(process.try_message(), Some(AgentMessage::Exited(_))) {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.agent.sessions[parent].process = Some(process);
    app.agent.sessions[parent].phase = Phase::Prompting(1);
    app.handle_agent_message(
        parent,
        AgentMessage::Request {
            id: json!(3),
            method: "unsupported".into(),
            params: Value::Null,
        },
    );
    assert!(app.agent.sessions[parent].process.is_none());
    assert_eq!(app.agent.sessions[parent].phase, Phase::Stopped);
    assert!(
        matches!(app.agent.sessions[parent].entries.last(), Some(Entry::Notice(error)) if error.contains("input is closed"))
    );
    assert_eq!(app.agent.sessions[child].phase, Phase::Stopped);
    assert!(app.agent.sessions[child].queued.is_empty());
    assert!(app.agent.permissions.is_empty());
}

#[cfg(unix)]
#[test]
fn agent_full_writer_preserves_prompt_context_and_recap_until_retry() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    let session = &mut app.agent.sessions[index];
    session.id = Some("s".into());
    session.recap = true;
    session.entries = vec![
        Entry::Agent("earlier answer".into()),
        Entry::User {
            text: "question".into(),
            context: vec![],
            attachments: vec![],
        },
    ];
    let parts = vec![
        PromptPart::File {
            path: session.root.join("file.rs"),
            text: Some("selection".into()),
        },
        PromptPart::Text("question".into()),
    ];
    session.queued.push_back(parts.clone());
    session.process = Some(
        AgentProcess::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 60".into()],
            &BTreeMap::new(),
            &session.root,
        )
        .unwrap(),
    );
    let process = session.process.as_mut().unwrap();
    let mut full = false;
    for _ in 0..32 {
        if process
            .notify("busy", json!({"text": "x".repeat(128 * 1024)}))
            .is_err()
        {
            full = true;
            break;
        }
    }
    assert!(full);
    session.send_next_prompt();
    assert_eq!(session.phase, Phase::Ready);
    assert_eq!(session.queued.front(), Some(&parts));
    assert!(session.recap);
    assert!(
        matches!(session.entries.last(), Some(Entry::Notice(error)) if error.contains("queue is full"))
    );
    session.process =
        Some(AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap());
    session.send_next_prompt();
    assert!(matches!(session.phase, Phase::Prompting(_)));
    assert!(session.queued.is_empty());
    assert!(!session.recap);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(AgentMessage::Request { method, params, .. }) =
            session.process.as_ref().unwrap().try_message()
        {
            assert_eq!(method, "session/prompt");
            assert!(
                params["prompt"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("earlier answer")
            );
            assert_eq!(params["prompt"][1]["resource"]["text"], "selection");
            assert_eq!(params["prompt"][2]["text"], "question");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn agent_recap_survives_failed_session_creation_and_reattachment() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    let session = &mut app.agent.sessions[index];
    session.earlier = Some("old".into());
    session.process =
        Some(AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap());
    session.open_session();
    let Phase::Creating(request) = session.phase else {
        panic!("new session");
    };
    assert!(session.recap && session.earlier.is_none());
    app.handle_agent_response(index, request, Err("startup failed".into()));
    let session = &mut app.agent.sessions[index];
    assert!(session.process.is_none());
    let process =
        AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap();
    session.attach((process, Phase::Initializing(10), BTreeMap::new(), None));
    assert!(session.recap);
    assert!(session.earlier.is_none());
}

#[test]
fn agent_unsupported_profile_options_are_reported() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    let session = &mut app.agent.sessions[index];
    session.id = Some("s".into());
    session.preferred.insert("mode".into(), "plan".into());
    session.apply_preferred();
    assert!(
        matches!(session.entries.last(), Some(Entry::Notice(error)) if error.contains("has no option `mode`"))
    );
}

#[test]
fn agent_empty_question_keeps_attached_context() {
    let (_directories, mut app) = test_app();
    let context = vec![PromptPart::File {
        path: app.state.workspace_root.join("file.rs"),
        text: Some("selection".into()),
    }];
    app.agent.context = context.clone();
    app.agent_ask("  \n ".into());
    assert_eq!(app.agent.context, context);
    assert!(app.agent.sessions.is_empty());
}

#[cfg(unix)]
#[test]
fn agent_explicit_subagent_model_replaces_profile_alias_and_actual_id() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    let session = &mut app.agent.sessions[index];
    session.parent = Some(99);
    session.process =
        Some(AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap());
    session.phase = Phase::Creating(10);
    session.preferred = BTreeMap::from([
        (SUBAGENT_MODEL.into(), "a".into()),
        ("model".into(), "b".into()),
        ("provider_model".into(), "b".into()),
    ]);
    session
        .queued
        .push_back(vec![PromptPart::Text("task".into())]);
    app.handle_agent_response(
        index,
        10,
        Ok(json!({"sessionId": "s", "configOptions": [{
            "id": "provider_model", "name": "Model", "category": "model", "type": "select",
            "currentValue": "a", "options": [{"value": "a"}, {"value": "b"}],
        }]})),
    );
    // The task waits for any startup text, then goes on a later tick.
    std::thread::sleep(STARTUP_TEXT_WAIT);
    app.agent.sessions[index].send_next_prompt();
    let session = &app.agent.sessions[index];
    assert!(matches!(session.phase, Phase::Prompting(_)));
    assert!(session.requests.is_empty());
    assert!(session.queued.is_empty());
    assert_eq!(session.option("model").unwrap().current, "a");
}

#[cfg(unix)]
#[test]
fn agent_model_changes_finish_before_subagent_prompts() {
    for accepted in [true, false] {
        let (_directories, mut app) = test_app();
        let index = tab(&mut app);
        let session = &mut app.agent.sessions[index];
        session.parent = Some(99);
        session.process = Some(
            AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap(),
        );
        session.phase = Phase::Creating(10);
        session.preferred.insert(SUBAGENT_MODEL.into(), "b".into());
        session
            .queued
            .push_back(vec![PromptPart::Text("task".into())]);
        app.handle_agent_response(
            index,
            10,
            Ok(json!({"sessionId": "s", "configOptions": [{
                "id": "model", "name": "Model", "type": "select",
                "currentValue": "a", "options": [{"value": "a"}, {"value": "b"}],
            }]})),
        );
        let session = &mut app.agent.sessions[index];
        assert_eq!(session.phase, Phase::Ready);
        assert_eq!(session.queued.len(), 1);
        let request = session.requests[0].0;
        let result = if accepted {
            Ok(json!({}))
        } else {
            Err("model rejected".into())
        };
        std::thread::sleep(STARTUP_TEXT_WAIT);
        app.handle_agent_message(
            index,
            AgentMessage::Response {
                id: request,
                result,
            },
        );
        let session = &app.agent.sessions[index];
        assert!(session.queued.is_empty());
        if accepted {
            assert!(matches!(session.phase, Phase::Prompting(_)));
            assert_eq!(session.option("model").unwrap().current, "b");
        } else {
            assert_eq!(session.phase, Phase::Stopped);
            assert!(session.process.is_none());
            assert!(
                matches!(session.entries.last(), Some(Entry::Notice(error)) if error == "model rejected")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn invalid_workspace_paths_stop_session_startup_and_answer_subagents() {
    use std::io::BufRead;
    use std::os::unix::ffi::OsStrExt;

    for capabilities in [
        json!({}),
        json!({"sessionCapabilities": {"resume": {}}}),
        json!({"loadSession": true}),
    ] {
        let (_app_dirs, mut app) = test_app();
        let index = child(&mut app, 99, Phase::Initializing(1));
        let (_server, reply, mut answer) = subagent_reply();
        let session = &mut app.agent.sessions[index];
        session.process = Some(
            AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap(),
        );
        session.root = Path::new(std::ffi::OsStr::from_bytes(b"/workspace/\xff")).into();
        session.earlier = Some("previous".into());
        session.reply = Some(reply);

        app.handle_agent_response(index, 1, Ok(json!({"agentCapabilities": capabilities})));

        let session = &app.agent.sessions[index];
        assert_eq!(session.phase, Phase::Stopped);
        assert!(session.process.is_none());
        assert!(session.queued.is_empty());
        assert!(session.reply.is_none());
        assert!(!session.working());
        assert!(matches!(session.entries.last(), Some(Entry::Notice(error))
            if error.contains("UTF-8 workspace path")));
        let mut line = String::new();
        answer.read_line(&mut line).unwrap();
        assert!(
            serde_json::from_str::<Value>(&line).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("UTF-8 workspace path")
        );
    }
}

#[test]
fn agent_restart_failure_keeps_the_submitted_draft() {
    let (_directories, mut app) = test_app();
    let mut config = app.configuration.fixture_snapshot();
    let name = config.settings.agents.default.clone();
    config
        .settings
        .agents
        .profiles
        .get_mut(&name)
        .unwrap()
        .program = "dust-test-missing-adapter".into();
    app.configuration.install_fixture(config);
    let index = app
        .add_agent_tab(
            name,
            app.state.workspace_root.clone(),
            Vec::new(),
            None,
            None,
        )
        .unwrap();
    app.agent.active = index;
    app.show_agent_panel();
    app.agent.context.push(PromptPart::Text("context".into()));
    app.agent.input = "draft question".into();
    app.agent.cursor = app.agent.input.len();
    app.agent_input_edit("submit");
    assert_eq!(app.agent.input, "draft question");
    assert_eq!(app.agent.cursor, app.agent.input.len());
    assert_eq!(app.agent.context, [PromptPart::Text("context".into())]);
    assert_eq!(app.ui.agent_panel.as_ref().unwrap().input, "draft question");
    assert!(
        !app.agent.sessions[index]
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::User { .. }))
    );
}

#[test]
fn agent_fallback_prompt_keeps_saved_attachments() {
    let (_app_dirs, mut app) = test_app();
    let mut config = app.configuration.fixture_snapshot();
    config.settings.persistence.enabled = true;
    app.configuration.install_fixture(config);
    let index = tab(&mut app);
    app.agent.active = index;
    let session = &mut app.agent.sessions[index];
    session.process =
        Some(AgentProcess::spawn(Path::new("cat"), &[], &BTreeMap::new(), &session.root).unwrap());
    session.phase = Phase::Ready;
    session.id = Some("first".into());
    let history = session.history.clone();
    let attachments = vec![
        PromptPart::File {
            path: app.state.workspace_root.join("unsaved.rs"),
            text: Some("let unsaved = 42;".into()),
        },
        PromptPart::Text("Diagnostics:\nmissing semicolon".into()),
    ];
    app.agent.context = attachments.clone();
    app.agent_ask("Explain this fragment".into());
    assert!(app.agent.context.is_empty());
    assert!(matches!(
        app.agent.sessions[index].entries.last(),
        Some(Entry::User { attachments: saved, .. }) if saved == &attachments
    ));
    app.finish_agent_history(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .unwrap();
    app.agent.sessions.clear();
    let index = app.reopen_conversation(&history).unwrap();
    let session = &mut app.agent.sessions[index];
    // Echo the actual ACP prompt to stderr, where AgentProcess exposes it as a log.
    session.process = Some(
        AgentProcess::spawn(
            Path::new("sh"),
            &["-c".into(), "cat >&2".into()],
            &BTreeMap::new(),
            &session.root,
        )
        .unwrap(),
    );
    session.id = Some("fresh".into());
    session.phase = Phase::Ready;
    session.recap = true;
    app.agent_ask("What next?".into());
    let process = app.agent.sessions[index].process.as_ref().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let prompt = loop {
        if let Some(AgentMessage::Log(line)) = process.try_message() {
            break serde_json::from_str::<Value>(&line).unwrap();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(prompt["method"], "session/prompt");
    let recap = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
    for expected in [
        "Explain this fragment",
        "unsaved.rs (1 lines selected)",
        "let unsaved = 42;",
        "Diagnostics:\nmissing semicolon",
    ] {
        assert!(recap.contains(expected), "missing {expected}: {recap}");
    }
    assert_eq!(prompt["params"]["prompt"][1]["text"], "What next?");
}

#[test]
fn agent_shutdown_saves_an_unfinished_turn() {
    let (_directories, mut app) = test_app();
    let directory = app.agent.directory.clone();
    let mut config = app.configuration.fixture_snapshot();
    config.settings.persistence.enabled = true;
    app.configuration.install_fixture(config);
    let index = tab(&mut app);
    let session = &mut app.agent.sessions[index];
    session.phase = Phase::Prompting(2);
    session.entries = vec![
        Entry::User {
            text: "question".into(),
            context: Vec::new(),
            attachments: Vec::new(),
        },
        Entry::Agent("streamed answer".into()),
    ];
    let history = session.history.clone();
    let entries = session.entries.clone();
    assert!(history::load(&directory, &history).is_none());
    app.finish_shutdown().unwrap();
    assert_eq!(history::load(&directory, &history).unwrap().1, entries);
}

#[test]
fn agent_rejoining_keeps_command_and_option_updates() {
    let (_directories, mut app) = test_app();
    let index = tab(&mut app);
    app.agent.sessions[index].phase = Phase::Rejoining(1);
    app.agent.sessions[index]
        .entries
        .push(Entry::Agent("saved answer".into()));
    for update in [
        json!({"sessionUpdate": "agent_message_chunk", "content": {"text": "replayed answer"}}),
        json!({"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "review", "description": "Review"}]}),
        json!({"sessionUpdate": "config_option_update", "configOptions": [{"id": "mode", "category": "mode", "currentValue": "plan", "options": [{"value": "plan"}]}]}),
    ] {
        app.handle_agent_message(
            index,
            AgentMessage::Notification {
                method: "session/update".into(),
                params: json!({"update": update}),
            },
        );
    }
    let session = &app.agent.sessions[index];
    assert_eq!(session.entries, [Entry::Agent("saved answer".into())]);
    assert_eq!(session.commands, [("review".into(), "Review".into())]);
    assert_eq!(session.option("mode").unwrap().current, "plan");
}

#[test]
fn agent_block_attachments_use_the_selected_display_columns() {
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    let mut state = crate::editor::EditorState::new("/work".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("abc\nDEF\n"), None)
        .unwrap();
    state.views[view].selection = Some(Selection {
        anchor: ByteOffset(1),
        head: ByteOffset(5),
        kind: SelectionKind::Block,
    });
    state.views[view].cursor = ByteOffset(5);
    assert_eq!(
        selected_text(
            &state.buffers[buffer].text,
            &state.views[view],
            state.policy
        ),
        "b\nE"
    );
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("a界z\nx\tQ\n"), None)
        .unwrap();
    state.views[view].selection = Some(Selection {
        anchor: ByteOffset(1),
        head: ByteOffset(7),
        kind: SelectionKind::Block,
    });
    state.views[view].cursor = ByteOffset(7);
    assert_eq!(
        selected_text(
            &state.buffers[buffer].text,
            &state.views[view],
            state.policy
        ),
        "界\n\t"
    );
}

#[cfg(unix)]
#[test]
fn agent_file_requests_use_unsaved_aliases_and_update_buffers_before_acknowledging() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    let root = app.state.workspace_root.clone();
    let real = root.join("real.txt");
    let alias = root.join("alias.txt");
    std::fs::write(&real, "disk").unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("draft"), Some(real.clone()))
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(100),
            method: "fs/read_text_file".into(),
            params: json!({"path": alias}),
        },
    );
    assert!(
        !app.agent.files.is_empty(),
        "file resolution must be deferred to VFS"
    );
    assert_eq!(file_response(&mut app, index).unwrap()["content"], "draft");
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(101),
            method: "session/request_permission".into(),
            params: json!({"toolCall": {"kind": "edit", "locations": [{"path": alias}]}}),
        },
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.agent.permissions.is_empty() {
        app.process_vfs_results();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        app.agent
            .permissions
            .pop_front()
            .unwrap()
            .title
            .contains("unsaved changes")
    );
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(102),
            method: "fs/write_text_file".into(),
            params: json!({"path": alias, "content": "new"}),
        },
    );
    assert!(
        file_response(&mut app, index)
            .unwrap_err()
            .contains("unsaved changes")
    );
    app.state.buffers[buffer].dirty = false;
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(103),
            method: "fs/write_text_file".into(),
            params: json!({"path": alias, "content": "new"}),
        },
    );
    file_response(&mut app, index).unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new");
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(104),
            method: "fs/read_text_file".into(),
            params: json!({"path": alias}),
        },
    );
    assert_eq!(file_response(&mut app, index).unwrap()["content"], "new");
    let outside = tempfile::tempdir().unwrap();
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(105),
            method: "fs/write_text_file".into(),
            params: json!({"path": outside.path().join("escape"), "content": "bad"}),
        },
    );
    assert!(
        file_response(&mut app, index)
            .unwrap_err()
            .contains("outside")
    );
    assert!(!outside.path().join("escape").exists());
}

#[cfg(unix)]
#[test]
fn agent_write_refreshes_buffers_opened_during_io_and_preserves_file_format() {
    use crate::app::documents::DocumentEvent;
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    let path = app.state.workspace_root.join("written.txt");
    std::fs::write(&path, "old").unwrap();
    let content = "\u{feff}new\r\n";
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(100),
            method: "fs/write_text_file".into(),
            params: json!({"path": path, "content": content}),
        },
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let completed = loop {
        if let Some(DocumentEvent::Legacy(result)) = app
            .documents
            .poll(&app.state.window_version(), app.workspace.root_revision())
            .unwrap()
        {
            if matches!(result, crate::files::VfsResult::Write { .. }) {
                break result;
            }
            app.accept_agent_file(result);
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    // This open raced with the worker's write and still carries the old text.
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    app.accept_agent_file(completed);
    file_response(&mut app, index).unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new\n");
    assert_eq!(
        app.state.buffers[buffer].text.to_bytes(),
        content.as_bytes()
    );
    assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
    assert!(!app.state.buffers[buffer].dirty);
}

#[cfg(unix)]
#[test]
fn cancelling_a_turn_cancels_permissions_still_resolving_paths() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    app.agent.active = index;
    app.agent.sessions[index].phase = Phase::Prompting(1);
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(100),
            method: "session/request_permission".into(),
            params: json!({"toolCall": {"kind": "edit"}}),
        },
    );
    assert_eq!(app.agent.files.len(), 1);
    app.cancel_agent_turn();
    assert!(app.agent.files.is_empty());
    assert_eq!(
        file_response(&mut app, index).unwrap(),
        agent::permission_cancelled()
    );
    // A later worker completion must be discarded, not presented as a new prompt.
    let id = app.documents.resolve_agent_paths(Vec::new()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(crate::app::documents::DocumentEvent::Legacy(
            crate::files::VfsResult::PathsResolved { id: received, .. },
        )) = app
            .documents
            .poll(&app.state.window_version(), app.workspace.root_revision())
            .unwrap()
        {
            assert_eq!(received, id);
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.process_agent_events();
    assert!(app.agent.permissions.is_empty());
    assert!(app.agent.asking.is_none());
    assert!(app.ui.prompt.is_none());
}

#[cfg(unix)]
#[test]
fn agent_write_checks_every_alias_of_a_not_yet_created_file() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    let root = app.state.workspace_root.clone();
    std::fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    let target = root.join("real/missing/file.txt");
    // The clean alias must not conceal a second, dirty buffer for this destination.
    app.state
        .open_text(TextBuffer::from_text("clean"), Some(target.clone()))
        .unwrap();
    let (dirty, _) = app
        .state
        .open_text(
            TextBuffer::from_text("draft"),
            Some(root.join("link/missing/file.txt")),
        )
        .unwrap();
    app.state.buffers[dirty].dirty = true;
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(100),
            method: "fs/write_text_file".into(),
            params: json!({"path": target, "content": "overwrite"}),
        },
    );
    assert!(
        file_response(&mut app, index)
            .unwrap_err()
            .contains("unsaved changes")
    );
    assert!(!target.exists());
    app.state.buffers[dirty].dirty = false;
    app.handle_agent_message(
        index,
        AgentMessage::Request {
            id: json!(101),
            method: "fs/write_text_file".into(),
            params: json!({"path": target, "content": "created"}),
        },
    );
    file_response(&mut app, index).unwrap();
    assert_eq!(app.state.buffers[dirty].text.to_string(), "created");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "created");
}

#[cfg(unix)]
#[test]
fn agent_persistent_permission_is_named_explicitly_before_acceptance() {
    let (_directories, mut app) = test_app();
    let index = echo_agent(&mut app);
    let mut request = permission(app.agent.sessions[index].number);
    request.id = json!(100);
    request.options = vec![PermissionOption {
        id: "always".into(),
        name: "Always allow".into(),
        kind: "allow_always".into(),
    }];
    app.agent.permissions.push_back(request);
    app.process_agent_events();
    assert_eq!(
        app.ui
            .prompt
            .as_ref()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .positive_label,
        "Allow always"
    );
    app.answer_agent_permission(ConfirmationChoice::Yes);
    assert_eq!(
        file_response(&mut app, index).unwrap(),
        agent::permission_selected("always")
    );
}

#[test]
fn agent_panel_hides_in_place_when_it_is_the_only_pane() {
    let (_directories, mut app) = test_app();
    tab(&mut app);
    app.show_agent_panel();
    let pane = app.agent_pane().unwrap();
    let others: Vec<_> = app
        .state
        .editor_windows()
        .map(|window| window.id)
        .filter(|other| *other != pane)
        .collect();
    for other in others {
        app.state.focus_pane_id(other).unwrap();
        app.apply_layout_action(LayoutAction::ClosePane);
    }
    app.state.focus_pane_id(pane).unwrap();
    assert!(app.state.is_last_pane());
    app.toggle_agent_panel();
    assert!(app.state.is_last_pane());
    assert!(app.agent_pane().is_none());
    assert!(app.ui.agent_panel.is_none());
}

#[test]
fn agent_panel_opens_in_the_requested_tab_without_switching_tabs() {
    let (_directories, mut app) = test_app();
    tab(&mut app);
    app.show_agent_panel();
    let first = app.state.active_tab().unwrap();
    app.state.new_tab().unwrap();
    app.state.replace_active_view(None).unwrap();
    let second = app.state.active_tab().unwrap();
    app.reconcile_editor_windows();
    app.show_agent_panel();
    assert_eq!(app.state.active_tab(), Some(second));
    assert!(app.agent_pane().is_some());
    assert_eq!(app.state.active_tab_views().len(), 2);
    app.state.activate_tab(first).unwrap();
    assert!(app.agent_pane().is_some());
    app.state.validate_layout().unwrap();
}

#[test]
fn replacing_a_permission_prompt_cancels_the_original_request() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = App::new(
        crate::app::startup::StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_bundled_config(),
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.open_prompt(PromptKind::AgentPermission, String::new());
    app.agent.asking = Some(PermissionRequest {
        session: 99,
        id: json!(1),
        action: "write",
        title: "permission".into(),
        options: Vec::new(),
    });
    app.open_prompt(PromptKind::ExplorerDelete, String::new());
    assert!(app.agent.asking.is_none());
    assert_eq!(app.input.prompt().unwrap().kind, PromptKind::ExplorerDelete);
}
// This fixture exercises the Unix shebang/private-runtime installation layout.
#[cfg(unix)]
#[test]
fn managed_pi_starts_with_its_private_node_on_path() {
    // A child test process keeps the tool-store and PATH overrides out of other tests.
    if std::env::var_os("DUST_TEST_MANAGED_PI").is_none() {
        let directory = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::agent::tests::managed_pi_starts_with_its_private_node_on_path",
                "--nocapture",
            ])
            .env("DUST_TEST_MANAGED_PI", "1")
            .env("DUST_STATE_HOME", directory.path())
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "child must run the managed-pi test: {output:?}"
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let bin = crate::tools::tool_store_root().join("pi-acp/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, script) in [
        ("pi-acp", "#!/usr/bin/env node\n"),
        (
            "node",
            "#!/bin/sh\nprintf '%s\\n' \"$PATH\" >&2\nwhile IFS= read -r line; do :; done\n",
        ),
    ] {
        let path = bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (_app_dirs, mut app) = crate::app::tests::app();
    app.tooling
        .fixture_retain(|entry| entry.definition.kind != ToolKind::Agent);
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "pi-acp")
        .unwrap();
    app.tooling.fixture_insert(crate::tools::ToolEntry {
        definition,
        status: ToolStatus::Installed(bin.join("pi-acp")),
    });
    let (process, ..) = app
        .spawn_agent("pi", &app.state.workspace_root, None)
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match process.try_message() {
            Some(AgentMessage::Log(path)) => {
                assert_eq!(std::env::split_paths(&path).next(), Some(bin));
                break;
            }
            Some(AgentMessage::Exited(reason)) => panic!("{reason}"),
            _ => {}
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_slash_command_typed_in_full_comes_first() {
    let names = |word: &str| {
        let commands = PANEL_COMMANDS
            .iter()
            .map(|(name, description)| ((*name).to_owned(), (*description).to_owned()));
        slash_matches(word, commands)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("mode").first().map(String::as_str), Some("mode"));
    assert!(names("mode").contains(&"model".to_owned()));
    assert_eq!(names("mod").first().map(String::as_str), Some("model"));
}

#[test]
fn the_users_message_is_a_band_and_the_answer_follows() {
    let entries = vec![
        Entry::Started {
            root: PathBuf::from("/work/repo"),
        },
        Entry::User {
            text: "why".into(),
            context: vec!["main.rs".into()],
            attachments: Vec::new(),
        },
        Entry::Tool {
            id: "t".into(),
            title: "Read main.rs".into(),
            status: "completed".into(),
        },
        Entry::Agent("Because.".into()),
    ];
    let mut out = Styled::default();
    let mut bands = Vec::new();
    transcript(&mut out, &mut bands, "claude", &entries, 80);
    assert_eq!(
        out.text,
        "\nwhy\n+ main.rs\n\n\n● Read main.rs\n\nBecause.\n"
    );
    let banded = bands
        .iter()
        .map(|band| &out.text[band.clone()])
        .collect::<Vec<_>>();
    assert_eq!(banded, ["\nwhy\n+ main.rs\n\n"]);
}

#[test]
fn a_subagent_runs_on_the_model_asked_for_or_says_which_ones_there_are() {
    let session = |wanted: Option<&str>, options: Value| AgentSession {
        number: 2,
        name: "pi".into(),
        root: PathBuf::from("/r"),
        process: None,
        phase: Phase::Ready,
        cancelling: false,
        turn_result: None,
        id: Some("s".into()),
        queued: VecDeque::new(),
        created: None,
        last_log: String::new(),
        entries: Vec::new(),
        options: agent::session_options(&options),
        auth_methods: Vec::new(),
        requests: Vec::new(),
        preferred: wanted
            .map(|model| (SUBAGENT_MODEL.to_owned(), model.to_owned()))
            .into_iter()
            .collect(),
        history: "2".into(),
        rejoin: Rejoin::default(),
        earlier: None,
        recap: false,
        parent: Some(1),
        task: String::new(),
        reply: None,
        commands: Vec::new(),
        tools: json!([]),
        meta: None,
    };
    let models = json!({"configOptions": [{
        "id": "model", "name": "Model", "category": "model", "type": "select",
        "currentValue": "openai/gpt-6.1",
        "options": [
            {"value": "openai/gpt-6.1", "name": "GPT 6.1"},
            {"value": "anthropic/claude-opus", "name": "Claude Opus"},
        ],
    }]});
    let mut by_name = session(Some("claude opus"), models.clone());
    assert_eq!(by_name.choose_subagent_model(), Ok(()));
    assert_eq!(
        by_name.preferred.get("model").map(String::as_str),
        Some("anthropic/claude-opus")
    );
    let mut by_id = session(Some("anthropic/claude-opus"), models.clone());
    assert_eq!(by_id.choose_subagent_model(), Ok(()));
    assert_eq!(
        by_id.preferred.get("model").map(String::as_str),
        Some("anthropic/claude-opus")
    );
    assert_eq!(
        session(Some("gpt-5"), models.clone()).choose_subagent_model(),
        Err("pi has no model `gpt-5`; it offers: openai/gpt-6.1, anthropic/claude-opus".into())
    );
    assert_eq!(
        session(Some("gpt-5"), json!({})).choose_subagent_model(),
        Err("pi does not let Dust choose its model".into())
    );
    let mut default = session(None, models);
    assert_eq!(default.choose_subagent_model(), Ok(()));
    assert!(default.preferred.is_empty());
}

#[test]
fn streamed_chunks_join_and_option_updates_replace_the_list() {
    let mut session = AgentSession {
        number: 1,
        name: "claude".into(),
        root: PathBuf::from("/r"),
        process: None,
        phase: Phase::Ready,
        cancelling: false,
        turn_result: None,
        id: Some("s".into()),
        queued: VecDeque::new(),
        created: None,
        last_log: String::new(),
        entries: Vec::new(),
        options: agent::session_options(&json!({
            "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "Default"}, {"id": "plan", "name": "Plan"}]}
        })),
        auth_methods: Vec::new(),
        requests: Vec::new(),
        preferred: BTreeMap::new(),
        history: "1".into(),
        rejoin: Rejoin::default(),
        earlier: None,
        recap: false,
        parent: None,
        task: String::new(),
        reply: None,
        commands: Vec::new(),
        tools: json!([]),
        meta: None,
    };
    assert_eq!(session.status(false), "saved");
    session.phase = Phase::Stopped;
    assert_eq!(session.status(false), "stopped");
    apply_update(&mut session, SessionUpdate::AgentText("Hel".into()));
    apply_update(&mut session, SessionUpdate::AgentText("lo".into()));
    apply_update(
        &mut session,
        SessionUpdate::ToolCall {
            id: "1".into(),
            title: "Edit".into(),
            status: "pending".into(),
        },
    );
    apply_update(
        &mut session,
        SessionUpdate::ToolCallUpdate {
            id: "1".into(),
            title: None,
            status: Some("completed".into()),
        },
    );
    assert!(matches!(&session.entries[0], Entry::Agent(text) if text == "Hello"));
    assert!(matches!(&session.entries[1], Entry::Tool { status, .. } if status == "completed"));
    apply_update(&mut session, SessionUpdate::Mode("plan".into()));
    assert_eq!(value_name(session.option("mode").unwrap()), "Plan");
    assert!(session.set_option("mode", "missing").is_err());

    // An agent that cannot pick up its earlier session gets the conversation instead.
    session.earlier = Some("old".into());
    session.open_session();
    assert!(session.recap && session.earlier.is_none());
}

#[cfg(unix)]
#[test]
fn a_new_file_under_a_symlinked_directory_resolves_to_where_it_will_be_written() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).expect("link");
    let mut worker = crate::files::VfsWorker::start(1);
    worker
        .resolve_paths(vec![root.path().join("link/new/file.txt")])
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let resolved = loop {
        if let Some(crate::files::VfsReply::Public(crate::files::VfsResult::PathsResolved {
            paths,
            ..
        })) = worker.try_reply().unwrap()
        {
            break paths[0].clone();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(
        resolved,
        crate::files::canonicalize(outside.path())
            .unwrap()
            .join("new/file.txt")
    );
    assert!(!resolved.starts_with(crate::files::canonicalize(root.path()).unwrap()));
}
