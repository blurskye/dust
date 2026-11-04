use super::*;

#[test]
fn first_frame_renders_before_startup_file_is_queued_and_vfs_then_opens_it() {
    let mut app = app();
    let path = app.state.workspace_root.join("startup.txt");
    fs::write(&path, b"startup\n").expect("startup file");
    app.set_dashboard_visibility(false);
    app.documents.enqueue_startup_fixture(path.clone(), true);
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    assert_eq!(frame.grid.width(), 80);
    assert_eq!(app.documents.legacy_count(), 0);
    assert!(app.state.buffers.is_empty());

    app.queue_startup_reads();
    assert_eq!(app.documents.startup_pending(), 1);
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("startup buffer");
    assert_eq!(
        app.state.buffers[buffer].path.as_deref(),
        Some(path.as_path())
    );
    assert_eq!(app.state.buffers[buffer].text.to_string(), "startup\n");
    assert!(!app.documents.has_startup_work());
    assert_eq!(app.documents.startup_pending(), 0);
}

#[test]
fn deferred_startup_additional_files_restore_the_primary_buffer() {
    let mut app = app();
    let primary = app.state.workspace_root.join("primary.txt");
    let additional = app.state.workspace_root.join("additional.txt");
    fs::write(&primary, b"primary").expect("primary");
    fs::write(&additional, b"additional").expect("additional");
    app.documents.enqueue_startup_fixture(primary.clone(), true);
    app.documents
        .enqueue_startup_fixture(additional.clone(), false);
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
fn typing_a_path_keeps_one_listing_in_flight_and_completes_the_latest_input() {
    let mut app = app();
    fs::write(app.state.workspace_root.join("alpha.txt"), b"a").expect("alpha");
    fs::write(app.state.workspace_root.join("beta.txt"), b"b").expect("beta");
    wait_vfs(&mut app);
    app.open_prompt(crate::ui::PromptKind::Command, "e ".into());
    for character in ["a", "l", "p", "h", "a"] {
        app.prompt_insert(character);
    }
    assert!(
        app.documents.pending_count() <= 1,
        "keystrokes must not queue one directory listing each"
    );
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert!(completions.iter().any(|item| item.label == "alpha.txt"));
    assert!(!completions.iter().any(|item| item.label == "beta.txt"));
}

#[test]
fn command_completion_accepts_registry_items_and_preserves_invalid_input() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture
            .settings
            .command_labels
            .insert("health.open".into(), "Vitals".into());
        app.configuration.install_fixture(fixture);
    }
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
    for _ in 0..app.ui.prompt.as_ref().expect("prompt").completions.len() {
        if app.ui.prompt.as_ref().expect("prompt").selected_completion == Some(index) {
            break;
        }
        app.prompt_complete(1);
    }
    assert_eq!(
        app.ui.prompt.as_ref().expect("prompt").selected_completion,
        Some(index)
    );
    // Enter on a chosen command runs it.
    app.submit_prompt();
    assert!(app.ui.prompt.is_none(), "the chosen command ran");
    app.open_prompt(crate::ui::PromptKind::Command, "health-not-real".into());
    app.submit_prompt();
    let prompt = app.ui.prompt.as_ref().expect("invalid prompt stays open");
    assert_eq!(prompt.input, "health-not-real");
    assert!(prompt.error.is_some());
}

#[test]
fn dot_repeats_semantic_insert_session() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("one"), None);
    app.set_dashboard_visibility(false);
    for key in ['i', 'X'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXone");
    assert_eq!(app.input.repeat_plan(3).unwrap().len(), 3);
}

#[test]
fn normalized_characterwise_and_linewise_changes_repeat_without_selection() {
    let mut character = app();
    character
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None);
    character.set_dashboard_visibility(false);
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
    line.set_dashboard_visibility(false);
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
    app.set_dashboard_visibility(false);
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
    app.set_dashboard_visibility(false);
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
    app.set_dashboard_visibility(false);
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
    app.set_dashboard_visibility(false);
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
    app.set_dashboard_visibility(false);
    for key in ['q', 'a', 'x', 'q', '3', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "e");
}

#[test]
fn key_hint_lifecycle_tracks_exact_prefix_delay_and_cancellation() {
    let mut app = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("hint"), None);
    app.set_dashboard_visibility(false);
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 0;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_key("space".parse().expect("leader"));
    assert_eq!(
        app.input
            .mapping_prefix()
            .map(|prefix| (prefix, app.input.mapping_deadline())),
        Some(("space", None))
    );
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space")
    );
    app.flush_keymap_timeout();
    assert!(app.input.mapping_pending());
    assert!(app.ui.key_hints.is_some());
    app.handle_configured_key(KeyEvent::character('f'));
    assert_eq!(app.input.mapping_prefix(), Some("space f"));
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space f")
    );
    app.handle_configured_key(KeyEvent::character('b'));
    app.update_key_hint_ui();
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.close_picker();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 500;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_key("space".parse().expect("leader"));
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    clock.advance(Duration::from_millis(600));
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_some());
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_paste("x".into());
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.handle_configured_key("space".parse().expect("leader"));
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
    app.close_prompt();
    app.handle_configured_key("space".parse().expect("leader"));
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
}

#[test]
fn key_hint_timeout_reload_and_pass_through_never_leave_stale_actions() {
    let mut app = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
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
            desc: None,
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
            desc: None,
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
            desc: None,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.install_bindings(keymaps, false);
    assert!(
        app.configuration
            .keymaps()
            .hints_for_prefix(
                "editor.normal",
                "space f",
                &app.configuration.settings().command_labels,
            )
            .is_some_and(|projection| { !projection.rows.iter().any(|row| row.next_key == "p") })
    );

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 0;
        app.configuration.install_fixture(fixture);
    }
    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    app.update_key_hint_ui();
    assert!(app.input.mapping_pending());
    assert!(app.ui.key_hints.is_some());
    clock.advance(Duration::from_millis(
        app.configuration.settings().input.mapping_timeout_ms + 1,
    ));
    app.flush_keymap_timeout();
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    assert!(app.input.mapping_pending());
    let mut candidate = crate::config::load_bundled_config();
    candidate.config.input.keymaps.leader = ",".into();
    app.apply_configuration_candidate(candidate, false)
        .expect("accepted configuration");
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
}

#[test]
fn an_unfinished_insert_mapping_types_its_keys() {
    let mut app = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.jk".into(),
        context: Some("editor".into()),
        mode: "insert".into(),
        keys: Some("j k".into()),
        command: Some("input.cancel".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.install_bindings(keymaps, false);
    let (_, view) = app.state.open_text(TextBuffer::from_text(""), None);
    app.set_dashboard_visibility(false);
    let text = |app: &InteractiveApplication| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };

    app.handle_configured_key(KeyEvent::character('i'));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('a'));
    assert_eq!(text(&app), "ja");

    app.handle_configured_key(KeyEvent::character('j'));
    clock.advance(Duration::from_millis(
        app.configuration.settings().input.mapping_timeout_ms + 1,
    ));
    app.flush_keymap_timeout();
    assert_eq!(text(&app), "jaj");

    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('k'));
    assert_eq!(text(&app), "jaj");
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn delayed_open_preserves_newer_unsaved_canonical_buffer_and_closed_reload_cannot_reopen_it() {
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let state_directory = app.workspace.state_path().parent().unwrap().to_path_buf();
    let path = app.state.workspace_root.join("late.txt");
    fs::write(&path, b"disk").unwrap();
    app.start_open(path.clone());
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("new unsaved"), Some(path.clone()));
    app.state.set_buffer_dirty(buffer, true);
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new unsaved");
    assert!(app.state.buffers[buffer].dirty);
    app.start_reload(true);
    app.close_active_buffer(true);
    wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(
        app.state
            .buffers
            .values()
            .all(|b| b.path.as_ref() != Some(&path))
    );
    drop(app);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(state_directory).unwrap();
}

/// Includes startup VFS admission/read/decode, editor+window publication and
/// final primary selection, but not rendering or general process startup.
#[test]
#[ignore = "explicit startup lifecycle cost probe"]
fn startup_lifecycle_cost_probe() {
    use crate::editor::session::extensions::ExtensionSession;
    let mut app = app();
    let root = app.state.workspace_root.clone();
    let state_directory = app.workspace.state_path().parent().unwrap().to_path_buf();
    let bytes = "0123456789abcdef\n".repeat(3856);
    for n in 0..4 {
        let path = app.state.workspace_root.join(format!("file-{n}.txt"));
        fs::write(&path, &bytes).unwrap();
        app.documents.enqueue_startup_fixture(path, n == 0);
    }
    let baseline = (
        crate::ui::window::WindowGraph::test_clone_count(),
        crate::editor::EditorState::test_clone_count(),
        ExtensionSession::fixture_window_clone_count(),
        TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = Instant::now();
    app.queue_startup_reads();
    wait_vfs(&mut app);
    let elapsed = start.elapsed().as_nanos();
    eprintln!("DUST_ALLOC_END");
    let materializations = TextBuffer::test_materializations();
    println!(
        "startup_files=4 bytes_per_file={} elapsed_ns={elapsed} graph_copies={} editor_copies={} host_copies={} text_decodes={} text_displays={}",
        bytes.len(),
        crate::ui::window::WindowGraph::test_clone_count() - baseline.0,
        crate::editor::EditorState::test_clone_count() - baseline.1,
        ExtensionSession::fixture_window_clone_count() - baseline.2,
        materializations.0 - baseline.3.0,
        materializations.1 - baseline.3.1
    );
    assert_eq!(app.state.buffers.len(), 4);
    assert!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .path
            .as_ref()
            .unwrap()
            .ends_with("file-0.txt")
    );
    drop(app);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(state_directory).unwrap();
}

#[test]
fn pending_open_cannot_resurrect_closed_path_or_steal_newer_navigation() {
    super::driver::TestSession::new(crate::config::load_bundled_config()).verify_open_lifetimes();
}

#[test]
fn force_save_as_does_not_overwrite_another_open_document() {
    super::driver::TestSession::new(crate::config::load_bundled_config()).verify_save_collisions();
}

#[test]
fn expired_shutdown_does_not_admit_or_report_an_exit_snapshot() {
    super::driver::TestSession::new(crate::config::load_bundled_config())
        .verify_expired_shutdown_does_not_write();
}

#[test]
fn the_line_after_a_final_newline_is_never_a_normal_mode_line() {
    let mut app = app();
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("one\n  two\n"),
        None,
    );
    app.set_dashboard_visibility(false);
    let view = app.state.active_view.expect("view");
    let line = |app: &InteractiveApplication| {
        app.state.buffers[buffer]
            .text
            .byte_to_line(app.state.views[view].cursor)
            .unwrap()
    };
    assert_eq!(app.state.buffers[buffer].text.visible_lines(), 2);
    for _ in 0..3 {
        app.handle_configured_key(KeyEvent::character('j'));
    }
    assert_eq!(line(&app), 1, "j stops at the last real line");
    app.handle_configured_key(KeyEvent::character('g'));
    app.handle_configured_key(KeyEvent::character('g'));
    app.handle_configured_key(KeyEvent::character('G'));
    assert_eq!(
        app.state.views[view].cursor.get(),
        6,
        "G lands on the last line's first non-blank"
    );
    app.handle_configured_key(KeyEvent::character('d'));
    app.handle_configured_key(KeyEvent::character('d'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "one\n");
    assert_eq!(line(&app), 0, "dd on the last line lands on the line above");
}

#[test]
fn tutor_moves_on_when_each_step_is_done() {
    fn press(app: &mut InteractiveApplication, key: &str) {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
        app.tutor_tick();
    }
    fn title(app: &InteractiveApplication) -> String {
        app.ui
            .tutor
            .as_ref()
            .map(|bar| bar.title.clone())
            .unwrap_or_default()
    }
    fn cursor_line(app: &InteractiveApplication) -> String {
        let view = &app.state.views[app.state.active_view.unwrap()];
        let text = &app.state.buffers[view.buffer].text;
        text.line(text.byte_to_line(view.cursor).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }
    fn wait_root(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.workspace.root_operation_pending() {
            app.process_trust_events();
            assert!(Instant::now() < deadline, "root switch timed out");
            thread::yield_now();
        }
        app.tutor_tick();
        wait_vfs(app);
        app.tutor_tick();
    }
    /// A done step says so for a moment before the next one comes.
    fn next_step(app: &mut InteractiveApplication) {
        assert!(
            app.ui.tutor.as_ref().is_some_and(|bar| bar.praised),
            "{}",
            title(app)
        );
        let before = title(app);
        let deadline = Instant::now() + Duration::from_secs(3);
        while title(app) == before {
            assert!(Instant::now() < deadline, "{before} never moved on");
            thread::sleep(Duration::from_millis(10));
            app.tutor_tick();
        }
        wait_vfs(app);
        app.tutor_tick();
    }
    let mut app = app();
    let home = app.state.workspace_root.canonicalize().unwrap();
    // The first start asks for a language; Rust comes first.
    app.process_effect(AppEffect::Tutor);
    assert!(app.ui.tutor.is_none());
    press(&mut app, "enter");
    wait_root(&mut app);
    assert!(title(&app).starts_with("Tutor 1/"), "{}", title(&app));
    assert!(app.ui.tutor.as_ref().unwrap().press.contains("`j`"));
    // The tutor moved the editor into its own practice repository.
    let practice = app.state.workspace_root.clone();
    assert_ne!(practice, home);
    assert!(practice.join(".git").is_dir() && practice.join("src/greet.rs").is_file());
    assert!(practice.join("practice.rs").is_file());
    press(&mut app, "j");
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 2/"), "{}", title(&app));
    assert!(
        cursor_line(&app).starts_with("h  left"),
        "{}",
        cursor_line(&app)
    );
    assert!(
        app.ui.tutor.as_ref().unwrap().target.is_some(),
        "the X is marked"
    );
    for _ in 0..2 {
        press(&mut app, "j");
    }
    assert_eq!(cursor_line(&app), "The X marks the spot.");
    // `j` kept the column of `h` through the blank line, so `w` lands on the X.
    press(&mut app, "w");
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 3/"), "{}", title(&app));
    // A step that checks text waits for the practice line to read right.
    app.process_effect(AppEffect::TutorBack);
    for _ in 0..10 {
        app.process_effect(AppEffect::TutorNext);
    }
    app.tutor_tick();
    wait_vfs(&mut app);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 12/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Thhe cow jumpped over thee moon.");
    for key in ["l", "x", "f", "p", "x", "$", "6", "h", "x"] {
        press(&mut app, key);
    }
    let view = &app.state.views[app.state.active_view.unwrap()];
    assert!(
        app.state.buffers[view.buffer]
            .text
            .to_string()
            .contains("    The cow jumped over the moon.\n")
    );
    // The next step put the cursor on its own practice line.
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Dust is a editor");
    // The card says which line, and the gutter marks it.
    let card = app.ui.tutor.as_ref().unwrap();
    let line = card.line.expect("the step's line").1;
    assert_eq!(card.place, format!("practice.rs, line {}", line + 1));
    assert!(!card.away);
    // Going back to a step already done in the text stays there.
    app.process_effect(AppEffect::TutorBack);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 12/"), "{}", title(&app));
    app.process_effect(AppEffect::TutorNext);
    app.tutor_tick();
    // A file of the user's own pauses the tutor, whose card says how to get back; :Tutor goes
    // back too.
    let own = home.join("notes.txt");
    fs::write(&own, "mine\n").unwrap();
    app.start_open(own);
    wait_vfs(&mut app);
    app.tutor_tick();
    let card = app.ui.tutor.as_ref().expect("the card stays");
    assert!(
        card.away && card.place.contains("`:e practice.rs`"),
        "{}",
        card.place
    );
    // Back in the practice file, the cursor is on the step's line again.
    app.start_open(practice.join("practice.rs"));
    wait_vfs(&mut app);
    app.tutor_tick();
    assert_eq!(cursor_line(&app), "Dust is a editor");
    app.process_effect(AppEffect::Tutor);
    wait_vfs(&mut app);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Dust is a editor");
    // Quitting moves back home and deletes the practice project.
    app.process_effect(AppEffect::TutorQuit);
    wait_root(&mut app);
    assert!(app.ui.tutor.is_none());
    assert_eq!(app.state.workspace_root, home);
    assert!(!practice.exists());
    assert!(app.state.buffers.values().all(|buffer| {
        buffer
            .path
            .as_deref()
            .is_none_or(|path| !path.starts_with(&practice))
    }));
    // The next start asks first; continuing is a fresh practice project at the step the user
    // stopped at.
    app.process_effect(AppEffect::Tutor);
    assert!(app.ui.tutor.is_none());
    press(&mut app, "enter");
    wait_root(&mut app);
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_ne!(app.state.workspace_root, practice);
}

#[test]
fn j_and_k_keep_their_column_across_shorter_lines() {
    fn press(app: &mut InteractiveApplication, key: &str) {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
    let mut app = app();
    app.set_dashboard_visibility(false);
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abcdef\n\nab\nabcdef\n"), None);
    app.state.views[view].cursor = ByteOffset(4);
    for _ in 0..3 {
        press(&mut app, "j");
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(15));
    press(&mut app, "h");
    press(&mut app, "k");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
}

fn press_keys(app: &mut InteractiveApplication, keys: &[&str]) {
    for key in keys {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
}

#[test]
fn vim_edits_that_lazyvim_users_reach_for() {
    let mut app = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("The cat sat.\n"), None);
    app.state.views[view].cursor = ByteOffset(4);
    // `cw` keeps the space after the word.
    press_keys(&mut app, &["c", "w", "d", "o", "g", "escape"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "The dog sat.\n");

    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("a\nb\n\nc\n"), None);
    app.state.views[view].cursor = ByteOffset(0);
    // Visual `>` with a count indents that many levels and leaves blank lines alone.
    press_keys(&mut app, &["V", "j", "j", "2", ">"]);
    let level = if app.state.policy.expand_tabs {
        " ".repeat(app.state.policy.indent_width)
    } else {
        "\t".into()
    };
    let indent = level.repeat(2);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        format!("{indent}a\n{indent}b\n\nc\n")
    );

    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("one two\nthree\n"), None);
    app.state.views[view].cursor = ByteOffset(0);
    // Visual `U`, `J` and `p`.
    press_keys(&mut app, &["v", "e", "U"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE two\nthree\n"
    );
    press_keys(&mut app, &["V", "j", "J"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE two three\n"
    );
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &["y", "i", "w"]);
    app.state.views[view].cursor = ByteOffset(4);
    press_keys(&mut app, &["v", "e", "p"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE ONE three\n"
    );
}

#[test]
fn paragraph_moves_are_jumps_that_ctrl_o_returns_from() {
    let mut app = app();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("jumps.txt");
    fs::write(&path, b"a\nb\n\nc\n").expect("jumps");
    app.start_open(path);
    wait_vfs(&mut app);
    let view = app.state.active_view.expect("view");
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &["}"]);
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
    press_keys(&mut app, &["G", "ctrl-o"]);
    wait_vfs(&mut app);
    let view = app.state.active_view.expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
    press_keys(&mut app, &["ctrl-o"]);
    wait_vfs(&mut app);
    let view = app.state.active_view.expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
    press_keys(&mut app, &["tab"]);
    wait_vfs(&mut app);
    let view = app.state.active_view.expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
}

#[test]
fn markdown_lines_comment_with_html_comments() {
    let mut app = app();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("notes.md");
    fs::write(&path, b"hello\n").expect("notes");
    app.start_open(path);
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("notes buffer");
    press_keys(&mut app, &["g", "c", "c"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "<!-- hello -->\n"
    );
    press_keys(&mut app, &["g", "c", "c"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "hello\n");
}

#[test]
fn several_cursors_change_every_match_and_type_on_every_line() {
    let mut app = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foo bar food foo\nx\nyy\n"), None);
    app.set_dashboard_visibility(false);
    let press = |app: &mut InteractiveApplication, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let text = |app: &InteractiveApplication| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };

    // Ctrl-N takes whole words only: `food` is not a match.
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    assert_eq!(app.state.views[view].mode, crate::editor::EditorMode::Multi);
    assert_eq!(app.state.views[view].multi_marks.len(), 2);
    press(&mut app, "c");
    for key in ["z", "z"] {
        press(&mut app, key);
    }
    press(&mut app, "escape");
    assert_eq!(text(&app), "zz bar food zz\nx\nyy\n");
    assert!(app.state.views[view].multi_marks.is_empty());

    // Cursors on the lines below, typing at each.
    app.state.views[view].cursor = ByteOffset("zz bar food zz\n".len());
    press(&mut app, "ctrl-alt-j");
    assert_eq!(app.state.views[view].multi_marks.len(), 2);
    press(&mut app, "i");
    press(&mut app, "-");
    press(&mut app, "escape");
    assert_eq!(text(&app), "zz bar food zz\n-x\n-yy\n");

    // `d` removes each match.
    app.state.views[view].cursor = ByteOffset(0);
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    press(&mut app, "d");
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    assert_eq!(text(&app), " bar food \n-x\n-yy\n");
}

#[test]
fn cgn_repeats_with_dot_substitute_confirms_each_match_and_global_deletes_lines() {
    let mut app = app();
    let (_, view) = app.state.open_text(
        TextBuffer::from_text("cat one\ncat two\n// cat three\ncat four\n"),
        None,
    );
    app.set_dashboard_visibility(false);
    let press = |app: &mut InteractiveApplication, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let typed = |app: &mut InteractiveApplication, keys: &str| {
        for character in keys.chars() {
            app.handle_configured_key(KeyEvent::character(character));
        }
    };
    let text = |app: &InteractiveApplication| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };

    // `/cat` goes to the second cat; `cgn` changes it and `.` the one after.
    typed(&mut app, "/cat");
    press(&mut app, "enter");
    typed(&mut app, "cgndog");
    press(&mut app, "escape");
    assert_eq!(text(&app), "cat one\ndog two\n// cat three\ncat four\n");
    press(&mut app, ".");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncat four\n");

    // `:s///c` asks about each match: skip the first, replace the second.
    typed(&mut app, ":%s/cat/cow/c");
    press(&mut app, "enter");
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::SubstituteConfirm)
    );
    press(&mut app, "n");
    press(&mut app, "y");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncow four\n");
    assert!(app.input.prompt().is_none());
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    press(&mut app, "u");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncat four\n");

    // `a` replaces every match left.
    typed(&mut app, ":%s/cat/cow/c");
    press(&mut app, "enter");
    press(&mut app, "a");
    assert_eq!(text(&app), "cow one\ndog two\n// dog three\ncow four\n");

    // `:g` deletes the lines that match, `:v` the ones that don't.
    typed(&mut app, ":g#^//#d");
    press(&mut app, "enter");
    assert_eq!(text(&app), "cow one\ndog two\ncow four\n");
    typed(&mut app, ":v/cow/d");
    press(&mut app, "enter");
    assert_eq!(text(&app), "cow one\ncow four\n");
}

#[test]
fn multi_cursors_take_every_match_skip_remove_and_move_together() {
    let mut app = app();
    let (_, view) = app.state.open_text(
        TextBuffer::from_text("let cat = 1;\nlet cat = 2;\nlet cat = 3;\nlet dog = cat;\n"),
        None,
    );
    app.set_dashboard_visibility(false);
    let press = |app: &mut InteractiveApplication, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let text = |app: &InteractiveApplication| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };
    let marks = |app: &InteractiveApplication| app.state.views[view].multi_marks.len();

    // Ctrl-Shift-L takes every match; Q drops the one the cursor is on.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-shift-l");
    assert_eq!(marks(&app), 4);
    press(&mut app, "N");
    assert_eq!(
        app.state.views[view].cursor,
        ByteOffset("let cat = 1;\nlet cat = 2;\nlet cat = 3;\nlet dog = ".len())
    );
    press(&mut app, "Q");
    assert_eq!(marks(&app), 3);
    press(&mut app, "c");
    for key in ["c", "o", "w"] {
        press(&mut app, key);
    }
    press(&mut app, "escape");
    assert_eq!(
        text(&app),
        "let cow = 1;\nlet cow = 2;\nlet cow = 3;\nlet dog = cat;\n"
    );

    // q skips the second match for the one after it.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    press(&mut app, "q");
    let starts: Vec<usize> = app.state.views[view]
        .multi_marks
        .iter()
        .map(|mark| mark.start.get())
        .collect();
    assert_eq!(starts, vec![4, "let cow = 1;\nlet cow = 2;\nlet ".len()]);
    press(&mut app, "escape");
    // With every match already taken, q just drops the one it is on.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-shift-l");
    assert_eq!(marks(&app), 3);
    press(&mut app, "q");
    assert_eq!(marks(&app), 2);
    press(&mut app, "escape");

    // Motions move every cursor; x deletes at each, and A types at each line's end.
    app.state.views[view].cursor = ByteOffset(0);
    press(&mut app, "ctrl-alt-j");
    press(&mut app, "ctrl-alt-j");
    press(&mut app, "w");
    press(&mut app, "x");
    assert_eq!(
        text(&app),
        "let ow = 1;\nlet ow = 2;\nlet ow = 3;\nlet dog = cat;\n"
    );
    press(&mut app, "A");
    press(&mut app, "!");
    press(&mut app, "escape");
    assert_eq!(
        text(&app),
        "let ow = 1;!\nlet ow = 2;!\nlet ow = 3;!\nlet dog = cat;\n"
    );
}

#[test]
fn selecting_every_match_of_a_selected_part_of_a_word_matches_it_as_typed() {
    let mut app = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foobar\nxfoo\n"), None);
    app.set_dashboard_visibility(false);
    for key in ["v", "l", "l"] {
        app.handle_configured_key(KeyEvent::character(key.chars().next().unwrap()));
    }
    app.handle_configured_key("ctrl-shift-l".parse().expect("key"));
    let starts: Vec<usize> = app.state.views[view]
        .multi_marks
        .iter()
        .map(|mark| mark.start.get())
        .collect();
    assert_eq!(starts, vec![0, "foobar\nx".len()]);
}

#[test]
fn visual_block_i_and_a_type_before_and_after_every_row() {
    let mut app = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("  x = 1\n  y = 2\n  z = 3\n"), None);
    app.set_dashboard_visibility(false);
    let keys = |app: &mut InteractiveApplication, keys: &str| {
        for character in keys.chars() {
            app.handle_configured_key(KeyEvent::character(character));
        }
    };
    let text = |app: &InteractiveApplication| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };
    app.state.views[view].cursor = ByteOffset(2);
    app.handle_configured_key("ctrl-v".parse().expect("key"));
    keys(&mut app, "2jIlet ");
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert_eq!(text(&app), "  let x = 1\n  let y = 2\n  let z = 3\n");
    app.state.views[view].cursor = ByteOffset("  let ".len());
    app.handle_configured_key("ctrl-v".parse().expect("key"));
    keys(&mut app, "jA:");
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert_eq!(text(&app), "  let x: = 1\n  let y: = 2\n  let z = 3\n");
}

#[test]
fn unknown_key_after_a_prefix_ends_the_sequence_without_editing() {
    let mut app = app();
    let path = app.state.workspace_root.join("prefix.txt");
    fs::write(&path, b"Header\n").expect("file");
    app.start_open(path);
    wait_vfs(&mut app);
    // Neither sequence is bound; the second key must not run as `a` or `o` would.
    for character in ['[', 'o', ']', 'a'] {
        app.handle_configured_key(KeyEvent::character(character));
    }
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().expect("buffer")]
            .text
            .to_string(),
        "Header\n"
    );
    assert_eq!(
        app.state.views[app.state.active_view.expect("view")].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn conflict_keys_move_between_markers_and_wrap() {
    let mut app = app();
    let path = app.state.workspace_root.join("merged.txt");
    fs::write(&path, b"Header\n<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\nmid\n<<<<<<< ours\nc\n=======\nd\n>>>>>>> theirs\n").expect("file");
    app.start_open(path);
    wait_vfs(&mut app);
    let line = |app: &mut InteractiveApplication| {
        wait_vfs(app);
        let view = &app.state.views[app.state.active_view.expect("view")];
        app.state.buffers[view.buffer]
            .text
            .byte_to_line(view.cursor)
            .expect("line")
    };
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 1);
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 7);
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 1, "wraps to the first");
    app.process_effect(AppEffect::GitConflictNavigate(-1));
    assert_eq!(line(&mut app), 7, "wraps to the last");
}

#[test]
fn clean_buffers_follow_the_disk_and_changed_ones_keep_their_text() {
    let mut app = app();
    let path = app.state.workspace_root.join("watched.txt");
    fs::write(&path, b"first\n").expect("file");
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.next_disk_check = None;
    app.process_disk_changes();

    // Longer content, so the length differs even if the clock did not tick.
    fs::write(&path, b"from outside\n").expect("rewrite");
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "from outside\n");

    app.state.buffers[buffer].dirty = true;
    fs::write(&path, b"outside again and longer\n").expect("rewrite");
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "from outside\n",
        "unsaved work stays"
    );
}

#[test]
fn colours_follow_typed_text_until_the_next_parse_or_token_reply() {
    let mut app = app();
    app.state.open_text(
        crate::editor::text::TextBuffer::from_text("let word = 1;"),
        None,
    );
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    let word = ByteRange::new(ByteOffset(4), ByteOffset(8)).unwrap();
    app.ui.lsp_semantic_spans = vec![crate::ui::UiSyntaxSpan {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        range: word,
        role: crate::language::syntax::Highlight::VARIABLE,
    }];
    for key in ['i', 'X'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.process_syntax();
    let span = app.ui.lsp_semantic_spans.first().expect("the colour stays");
    assert_eq!(span.revision, app.state.buffers[buffer].text.revision());
    assert_eq!(
        span.range,
        ByteRange::new(ByteOffset(5), ByteOffset(9)).unwrap()
    );
}

#[test]
fn recovery_failure_and_retired_reads_preserve_the_journal() {
    let mut app = app();
    app.recovery.skip_maintenance();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("saved"), None);
    app.state.buffers[buffer].read_only = true;
    assert!(app.replace_with_recovered(buffer, "draft".into()).is_err());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "saved");
    let path = app.state.workspace_root.join("recover.txt");
    fs::write(&path, "disk").unwrap();
    let journal = app.state.workspace_root.join("journal.toml");
    fs::write(&journal, "recoverable").unwrap();
    let id = app
        .documents
        .read_scoped(path, vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery
        .register_read(id, journal.clone(), "draft".into(), app.workspace.version());
    app.workspace.fixture_safe_mode(true);
    wait_vfs(&mut app);
    assert_eq!(fs::read_to_string(journal).unwrap(), "recoverable");
    assert!(
        app.state
            .buffers
            .values()
            .all(|buffer| buffer.text.to_string() != "draft")
    );
}

#[test]
fn flash_cancels_cached_targets_after_a_reload() {
    let mut app = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("abc abc abc"), None);
    app.set_dashboard_visibility(false);
    app.begin_flash();
    press_keys(&mut app, &["a"]);
    let label = app
        .flash
        .as_ref()
        .unwrap()
        .targets
        .iter()
        .find_map(|target| target.label)
        .unwrap();
    app.state.buffers[buffer].text = TextBuffer::from_text("é");
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &[&label.to_string()]);
    assert!(app.flash.is_none());
    assert_eq!(app.state.views[view].cursor.get(), 0);
}

#[test]
fn persisted_navigation_clamps_multibyte_offsets() {
    let mut app = app();
    let path = app.state.workspace_root.join("mark.txt");
    fs::write(&path, "éx").unwrap();
    app.navigate_persisted_location(crate::persistence::PersistedLocation {
        path,
        byte: 1,
        anchor: 2,
        scroll_line: 0,
        scroll_column: 0,
    });
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(view.cursor.get(), 0);
    assert_eq!(view.selection.unwrap().anchor.get(), 2);
    assert_eq!(view.selection.unwrap().head, view.cursor);
}

#[test]
fn cursor_only_commands_do_not_enter_the_change_list() {
    let mut app = app();
    let path = app.state.workspace_root.join("changes.txt");
    app.state
        .open_text(TextBuffer::from_text("abc def"), Some(path));
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["l", "w", "h"]);
    assert!(app.persistence.changes().is_empty());
    press_keys(&mut app, &["x"]);
    assert_eq!(app.persistence.changes().len(), 1);
}

#[test]
fn count_errors_return_operator_pending_views_to_normal() {
    let mut app = app();
    let (_, view) = app.state.open_text(TextBuffer::from_text("abc"), None);
    app.set_dashboard_visibility(false);
    press_keys(
        &mut app,
        &["1", "0", "0", "1", "d", "1", "0", "0", "0", "w"],
    );
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn count_only_and_empty_pattern_substitutions_preserve_their_meaning() {
    let mut app = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("cat cat\nkeep\n"), None);
    app.set_dashboard_visibility(false);
    app.run_substitute("%", "cat", "dog", "gn");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "cat cat\nkeep\n"
    );
    app.run_substitute("%", "cat", "dog", "z");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "cat cat\nkeep\n"
    );
    app.ui.search_query = Some("cat".into());
    app.run_substitute("%", "", "dog", "g");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "dog dog\nkeep\n"
    );
    app.state.views[view].cursor = ByteOffset(8);
    app.run_global_delete("", "keep", false);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "dog dog\n");
    assert!(app.state.views[view].cursor.get() < app.state.buffers[buffer].text.len_bytes());
}

#[test]
fn recorded_dot_and_insert_delete_and_yank_replay_the_expected_change() {
    let mut app = app();
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("abcdef"), None);
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["x", "q", "a", ".", "q", "@", "a"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "def");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("abc"), None);
    press_keys(&mut app, &["i", "X", "delete", "escape", "."]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Xbc");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("abc"), None);
    press_keys(&mut app, &["x", "y", "y", "."]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "c");
}

#[test]
fn macro_replay_stops_before_an_edit_after_a_failed_find() {
    let mut app = app();
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("az\nkeep"), None);
    app.set_dashboard_visibility(false);
    for key in "qafzxq".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.state.views[view].cursor = ByteOffset(3);
    let before = app.state.buffers[buffer].text.to_string();
    app.replay_macro('a');
    assert_eq!(app.state.buffers[buffer].text.to_string(), before);
    assert!(app.ui.notification_error);
}

#[test]
fn star_and_hash_repeat_only_whole_identifiers() {
    let mut app = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foo foobar foo"), None);
    app.set_dashboard_visibility(false);
    app.search_word_under_cursor(true);
    assert_eq!(app.state.views[view].cursor, ByteOffset(11));
    app.perform_search(true);
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
    app.search_word_under_cursor(false);
    assert_eq!(app.state.views[view].cursor, ByteOffset(11));
}

#[test]
fn operator_arrow_motions_are_linewise_like_j_and_k() {
    for (motion, cursor) in [("down", 1), ("up", 5)] {
        let mut app = app();
        let (buffer, view) = app.state.open_text(
            crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\n"),
            None,
        );
        app.set_dashboard_visibility(false);
        app.state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
        app.handle_configured_key(KeyEvent::character('d'));
        app.handle_configured_key(motion.parse().unwrap());
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            "ghi\n",
            "{motion}"
        );
    }
}
