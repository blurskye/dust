use super::*;

#[test]
fn first_frame_renders_before_startup_file_is_queued_and_vfs_then_opens_it() {
    let mut app = app();
    let path = app.state.workspace_root.join("startup.txt");
    fs::write(&path, b"startup\n").expect("startup file");
    app.ui.dashboard = false;
    app.startup_reads.push_back((path.clone(), true));
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        &app.configuration.settings,
        &app.configuration.dashboard,
        80,
        24,
    );
    assert_eq!(frame.grid.width(), 80);
    assert!(app.pending_vfs.is_empty());
    assert!(app.state.buffers.is_empty());

    app.queue_startup_reads();
    assert!(
        app.pending_vfs
            .values()
            .any(|pending| { matches!(pending, super::PendingVfs::StartupOpen { primary: true }) })
    );
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("startup buffer");
    assert_eq!(
        app.state.buffers[buffer].path.as_deref(),
        Some(path.as_path())
    );
    assert_eq!(app.state.buffers[buffer].text.to_string(), "startup\n");
    assert!(app.startup_reads.is_empty());
    assert_eq!(app.startup_reads_pending, 0);
}

#[test]
fn deferred_startup_additional_files_restore_the_primary_buffer() {
    let mut app = app();
    let primary = app.state.workspace_root.join("primary.txt");
    let additional = app.state.workspace_root.join("additional.txt");
    fs::write(&primary, b"primary").expect("primary");
    fs::write(&additional, b"additional").expect("additional");
    app.startup_reads.push_back((primary.clone(), true));
    app.startup_reads.push_back((additional.clone(), false));
    app.queue_startup_reads();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers.len(), 2);
    let active = app.state.active_buffer_id().expect("active primary");
    assert_eq!(
        app.state.buffers[active].path.as_deref(),
        Some(primary.as_path())
    );
    assert!(
        app.state
            .buffers
            .iter()
            .any(|(_, buffer)| { buffer.path.as_deref() == Some(additional.as_path()) })
    );
    app.state.validate_layout().expect("valid startup layout");
}

#[test]
fn path_completion_is_bounded_async_and_rejects_stale_generations() {
    let mut app = app();
    fs::write(app.state.workspace_root.join("alpha.txt"), b"a").expect("alpha");
    app.open_prompt(crate::ui::PromptKind::Command, "e a".into());
    wait_vfs(&mut app);
    assert!(
        app.ui
            .prompt
            .as_ref()
            .expect("prompt")
            .completions
            .iter()
            .any(|item| item.label == "alpha.txt")
    );

    app.prompt_insert("z");
    wait_vfs(&mut app);
    assert!(
        app.ui
            .prompt
            .as_ref()
            .expect("prompt")
            .completions
            .is_empty()
    );
}

#[test]
fn command_completion_accepts_registry_items_and_preserves_invalid_input() {
    let mut app = app();
    app.configuration
        .settings
        .command_labels
        .insert("health.open".into(), "Vitals".into());
    app.open_prompt(crate::ui::PromptKind::Command, "hea".into());
    let index = app
        .ui
        .prompt
        .as_ref()
        .expect("prompt")
        .completions
        .iter()
        .position(|item| item.label == "health" && item.detail == "Vitals")
        .expect("health completion");
    app.ui.prompt.as_mut().expect("prompt").selected_completion = Some(index);
    app.submit_prompt();
    assert_eq!(app.ui.prompt.as_ref().expect("prompt").input, "health");
    app.prompt_insert("-not-real");
    app.submit_prompt();
    let prompt = app.ui.prompt.as_ref().expect("invalid prompt stays open");
    assert_eq!(prompt.input, "health-not-real");
    assert!(prompt.error.is_some());
}

#[test]
fn completion_selection_and_submit_acceptance_follow_configuration() {
    let mut app = app();
    app.configuration.settings.input.completion_select_first = true;
    app.configuration.settings.input.completion_accept_on_submit = false;
    app.open_prompt(crate::ui::PromptKind::Command, "hea".into());
    assert_eq!(
        app.ui.prompt.as_ref().expect("prompt").selected_completion,
        Some(0)
    );
    app.submit_prompt();
    let prompt = app.ui.prompt.as_ref().expect("prompt retained");
    assert_eq!(prompt.input, "hea");
    assert!(prompt.error.is_some());
}

#[test]
fn dot_repeats_semantic_insert_session() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("one"), None);
    app.ui.dashboard = false;
    for key in ['i', 'X'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXone");
    assert_eq!(app.last_change.len(), 3);
}

#[test]
fn normalized_characterwise_and_linewise_changes_repeat_without_selection() {
    let mut character = app();
    character
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
    character.ui.dashboard = false;
    for key in ['v', 'l', 'd', 'l', '.'] {
        character.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = character
        .state
        .active_buffer_id()
        .expect("character buffer");
    assert_eq!(character.state.buffers[buffer].text.to_string(), "cf");

    let mut line = app();
    line.state.open_text(
        crate::editor::text::TextBuffer::from_text("a\nb\nc\nd\n"),
        None,
    );
    line.ui.dashboard = false;
    for key in ['V', 'j', 'd', '.'] {
        line.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = line.state.active_buffer_id().expect("line buffer");
    assert_eq!(line.state.buffers[buffer].text.to_string(), "");
}

#[test]
fn normalized_blockwise_change_repeats_without_a_stale_selection() {
    let mut app = app();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\njkl"),
        None,
    );
    app.ui.dashboard = false;
    app.handle_configured_key("ctrl-v".parse().expect("block key"));
    app.handle_configured_key(KeyEvent::character('l'));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('c'));
    app.handle_configured_key(KeyEvent::character('Z'));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Zc\nZf\nZi\nZl");
}

#[test]
fn normalized_block_repeat_pads_new_ragged_and_empty_rows_by_display_column() {
    let mut app = app();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("abcd\nx\npqrs\n----\ny\n----"),
        None,
    );
    app.ui.dashboard = false;
    let view = app.state.active_view.expect("view");
    app.state.views[view].cursor = ByteOffset(10);
    app.state.views[view].mode = crate::editor::EditorMode::VisualBlock;
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: ByteOffset(2),
        head: ByteOffset(10),
        kind: crate::editor::SelectionKind::Block,
    });
    for key in [
        KeyEvent::character('c'),
        KeyEvent::character('Z'),
        KeyEvent::plain(crate::editor::input::KeyCode::Escape),
    ] {
        app.handle_configured_key(key);
    }
    app.state.views[view].cursor = ByteOffset(14);
    app.state.views[view].selection = None;
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "abZ\nx Z\npqZ\n--Z\ny Z\n--Z"
    );
}

#[test]
fn replace_mode_session_is_semantically_repeatable() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.ui.dashboard = false;
    app.handle_configured_key(KeyEvent::character('R'));
    app.handle_configured_key(KeyEvent::character('X'));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('l'));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXc");
}

#[test]
fn blockwise_visual_commands_record_and_replay_as_typed_macro_invocations() {
    let mut app = app();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\njkl"),
        None,
    );
    app.ui.dashboard = false;
    app.handle_configured_key(KeyEvent::character('q'));
    app.handle_configured_key(KeyEvent::character('a'));
    app.handle_configured_key("ctrl-v".parse().expect("block key"));
    for key in ['l', 'j', 'd', 'q', 'j', 'j', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "c\nf\ni\nl");
}

#[test]
fn macro_replay_honors_configured_counts() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcde"), None);
    app.ui.dashboard = false;
    for key in ['q', 'a', 'x', 'q', '3', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "e");
}

#[test]
fn key_hint_lifecycle_tracks_exact_prefix_delay_and_cancellation() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("hint"), None);
    app.ui.dashboard = false;
    app.configuration.settings.ui.key_hints.delay_ms = 0;
    app.handle_configured_key("space".parse().expect("leader"));
    assert_eq!(
        app.pending_keymap
            .as_ref()
            .map(|pending| pending.prefix.as_str()),
        Some("space")
    );
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space")
    );
    app.handle_configured_key(KeyEvent::character('f'));
    assert_eq!(
        app.pending_keymap
            .as_ref()
            .map(|pending| pending.prefix.as_str()),
        Some("space f")
    );
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space f")
    );
    app.handle_configured_key(KeyEvent::character('b'));
    app.update_key_hint_ui();
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());

    app.close_picker();
    app.configuration.settings.ui.key_hints.delay_ms = 500;
    app.handle_configured_key("space".parse().expect("leader"));
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    app.pending_keymap.as_mut().expect("pending").started_at =
        Instant::now() - Duration::from_millis(600);
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_some());
    app.configuration.settings.ui.key_hints.enabled = false;
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    app.configuration.settings.ui.key_hints.enabled = true;
    app.handle_configured_paste("x".into());
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());

    app.handle_configured_key("space".parse().expect("leader"));
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());
    app.close_prompt();
    app.handle_configured_key("space".parse().expect("leader"));
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());
}

#[test]
fn key_hint_timeout_reload_and_pass_through_never_leave_stale_actions() {
    let mut app = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "user.test.ambiguous-exact".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("x".into()),
            command: Some("health.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.ambiguous-long".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("x y".into()),
            command: Some("file.new".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.hidden-pass-through".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f p".into()),
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: true,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.keymaps = keymaps;
    assert!(
        app.configuration
            .keymaps
            .hints_for_prefix(
                "editor.normal",
                "space f",
                &app.configuration.settings.command_labels,
            )
            .is_some_and(|projection| { !projection.rows.iter().any(|row| row.next_key == "p") })
    );

    app.configuration.settings.ui.key_hints.delay_ms = 0;
    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    app.update_key_hint_ui();
    assert!(app.pending_keymap.is_some());
    assert!(app.ui.key_hints.is_some());
    app.pending_keymap.as_mut().expect("pending").deadline =
        Instant::now() - Duration::from_millis(1);
    app.flush_keymap_timeout();
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());

    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    assert!(app.pending_keymap.is_some());
    let mut candidate = crate::config::load_bundled_config();
    candidate.config.input.keymaps.leader = ",".into();
    app.apply_configuration_candidate(candidate, false)
        .expect("accepted configuration");
    assert!(app.pending_keymap.is_none());
    assert!(app.ui.key_hints.is_none());
}
