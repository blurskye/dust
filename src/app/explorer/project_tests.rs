use crate::app::tests::*;




#[test]
pub(in crate::app) fn explorer_command_prompt_creates_a_file_from_the_selected_directory() {
    let (_app_dirs, mut app) = app();
    app.open_explorer();
    app.open_prompt(crate::ui::PromptKind::Command, "exploreradd ".into());
    app.prompt_insert("new.txt");
    app.submit_prompt();
    assert!(app.ui.prompt.is_none(), "{:?}", app.ui.prompt);
    assert!(app.explorer.mutation_pending());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !app.state.workspace_root.join("new.txt").exists() {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.state.workspace_root.join("new.txt").is_file());
}


#[test]
pub(in crate::app) fn explorer_prefilled_rename_keeps_original_target_and_dirty_split_views() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let source = root.join("source.rs");
    let other = root.join("other.rs");
    fs::write(&source, "disk text").unwrap();
    fs::write(&other, "other").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let (buffer, first) = app
        .state
        .open_text(TextBuffer::from_text("dirty text"), Some(source.clone()))
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    let second = app
        .state
        .split_active(crate::editor::Axis::Vertical, 500)
        .unwrap();
    app.state.views[first].cursor = ByteOffset(1);
    app.state.views[second].cursor = ByteOffset(4);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    app.handle_configured_key(KeyEvent::character('e'));
    let prompt = app.input.prompt().expect("e opens shared prompt");
    assert_eq!(prompt.kind, crate::ui::PromptKind::ExplorerRename);
    assert_eq!(prompt.input, "source.rs");
    assert_eq!(prompt.cursor, "source.rs".len());
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    let screen = (0..24)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Rename path"));
    assert!(screen.contains("source.rs"));
    app.handle_event(UiEvent::Paste("invalid\nname".into()));
    assert_eq!(app.input.prompt().unwrap().input, "source.rs");
    wait_vfs(&mut app);
    // Selection changes must never retarget the original rename request.
    app.explorer
        .select(app.explorer.tree().id_for_path(&other).unwrap());
    for _ in 0.."source.rs".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("../escape.rs");
    app.submit_prompt();
    assert!(app.input.prompt().unwrap().error.is_some());
    assert!(!app.explorer.mutation_pending());
    for _ in 0.."../escape.rs".len() {
        app.prompt_backspace();
    }
    let value = "nested/new 'é'.rs";
    app.handle_event(UiEvent::Paste(value.into()));
    app.submit_prompt();
    app.submit_prompt(); // a second Enter cannot submit a second mutation
    wait_vfs(&mut app);
    let target = root.join(value);
    assert!(app.input.prompt().is_none());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "disk text");
    assert_eq!(fs::read_to_string(&other).unwrap(), "other");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "dirty text");
    assert_eq!(app.state.views[first].buffer, buffer);
    assert_eq!(app.state.views[second].buffer, buffer);
    assert_eq!(app.state.views[first].cursor, ByteOffset(1));
    assert_eq!(app.state.views[second].cursor, ByteOffset(4));
    assert!(app.input.command_history().is_empty());
    assert!(app.input.search_history().is_empty());
}


#[test]
pub(in crate::app) fn explorer_rename_prompt_refuses_replaced_files_and_retired_or_cancelled_requests() {
    let (_app_dirs, mut app) = app();
    let source = app.state.workspace_root.join("source.txt");
    let target = app.state.workspace_root.join("target.txt");
    fs::write(&source, "original").unwrap();
    fs::write(&target, "collision").unwrap();
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    app.handle_configured_key(KeyEvent::character('r'));
    wait_vfs(&mut app);
    for _ in 0.."source.txt".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("target.txt");
    app.submit_prompt();
    wait_vfs(&mut app);
    assert_eq!(app.input.prompt().unwrap().input, "target.txt");
    assert!(app.input.prompt().unwrap().error.is_some());
    assert_eq!(fs::read_to_string(&target).unwrap(), "collision");
    fs::remove_file(&target).unwrap();
    let parked = source.with_extension("parked");
    fs::rename(&source, &parked).unwrap();
    fs::write(&source, "replacement").unwrap();
    app.submit_prompt();
    wait_vfs(&mut app);
    assert!(app.input.prompt().unwrap().error.is_some());
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    assert_eq!(fs::read_to_string(&parked).unwrap(), "original");
    assert!(!target.exists());
    app.close_prompt();
    // Closing before inspection delivery cannot resurrect a prompt or enable a rename.
    app.open_explorer_rename_prompt();
    app.handle_configured_key("escape".parse().unwrap());
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    assert!(
        app.explorer
            .rename_value(
                "target.txt",
                app.workspace.version(),
                app.configuration.binding_revision(),
                &app.state.window_version()
            )
            .is_err()
    );
    // Cancelling after preparation admission still performs no mutation.
    app.open_explorer_rename_prompt();
    wait_vfs(&mut app);
    for _ in 0.."source.txt".len() {
        app.prompt_backspace();
    }
    app.prompt_insert("target.txt");
    app.submit_prompt();
    app.close_prompt();
    wait_vfs(&mut app);
    assert!(source.exists());
    assert!(!target.exists());
    app.open_explorer_rename_prompt();
    app.handle_event(UiEvent::FocusLost);
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    app.open_explorer_rename_prompt();
    app.process_effect(AppEffect::ExplorerClose);
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    // A root epoch change retires the original prompt, even for equal path strings.
    app.open_explorer_rename_prompt();
    app.explorer
        .reset_roots(app.workspace.roots(), app.workspace.root_revision());
    app.retire_stale_explorer_intents();
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    assert!(!target.exists());
}


#[test]
pub(in crate::app) fn explorer_rename_rejects_destinations_owned_by_unsaved_buffers() {
    for directory in [false, true] {
        let (_app_dirs, mut app) = app();
        let root = app.state.workspace_root.clone();
        let source = root.join("source");
        let target = root.join("target");
        let (source_file, target_file) = if directory {
            fs::create_dir(&source).unwrap();
            (source.join("file.txt"), target.join("file.txt"))
        } else {
            (source.clone(), target.clone())
        };
        fs::write(&source_file, "source text").unwrap();
        let (buffer, _) = app
            .state
            .open_text(
                TextBuffer::from_text("source text"),
                Some(source_file.clone()),
            )
            .unwrap();
        let (destination, _) = app
            .state
            .open_text(
                TextBuffer::from_text("unsaved destination"),
                Some(target_file.clone()),
            )
            .unwrap();
        app.state.set_buffer_dirty(destination, true);
        app.open_explorer();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.explorer.tree().id_for_path(&source).is_none() {
            app.process_explorer_result();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        app.explorer
            .select(app.explorer.tree().id_for_path(&source).unwrap());
        app.rename_explorer_path("target".into());
        wait_vfs(&mut app);
        assert!(!app.explorer.mutation_pending());
        assert_eq!(fs::read_to_string(&source_file).unwrap(), "source text");
        assert!(!target.exists());
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&source_file));
        assert_eq!(
            app.state.buffers[destination].path.as_ref(),
            Some(&target_file)
        );
        assert_eq!(
            app.state.buffers[destination].text.to_string(),
            "unsaved destination"
        );
        assert!(app.state.buffers[destination].dirty);
    }
}


#[test]
pub(in crate::app) fn explorer_close_reconciles_an_admitted_rename_even_after_disk_completion() {
    use crate::app::documents::DocumentEvent;
    let (_app_dirs, mut app) = app();
    let source = app.state.workspace_root.join("before.txt");
    let target = app.state.workspace_root.join("after.txt");
    fs::write(&source, "kept").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("kept"), Some(source.clone()))
        .unwrap();
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&source).unwrap());
    app.rename_explorer_path("after.txt".into());
    // Admit the prepared write, but do not consume its completion yet.
    loop {
        if let Some(DocumentEvent::Explorer(reply)) = app
            .documents
            .poll(&app.state.window_version(), app.workspace.root_revision())
            .unwrap()
        {
            app.process_explorer_io(reply);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    while !target.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&source));
    app.process_effect(AppEffect::ExplorerClose);
    app.retire_stale_explorer_intents();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    assert!(!app.explorer.mutation_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "kept");
}


#[test]
pub(in crate::app) fn explorer_rename_refuses_destinations_owned_by_unsaved_buffers_before_writing() {
    for directory in [false, true] {
        let (_app_dirs, mut app) = app();
        let source = app.state.workspace_root.join("source");
        let target = app.state.workspace_root.join("renamed");
        let (source_file, target_file) = if directory {
            fs::create_dir(&source).unwrap();
            (source.join("inside.txt"), target.join("inside.txt"))
        } else {
            (source.clone(), target.clone())
        };
        fs::write(&source_file, "original").unwrap();
        let (original, _) = app
            .state
            .open_text(TextBuffer::from_text("original"), Some(source_file.clone()))
            .unwrap();
        let (unsaved, _) = app
            .state
            .open_text(TextBuffer::from_text("unsaved"), Some(target_file.clone()))
            .unwrap();
        app.state.set_buffer_dirty(unsaved, true);
        app.open_explorer();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.explorer.tree().id_for_path(&source).is_none() {
            app.process_explorer_result();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        app.explorer
            .select(app.explorer.tree().id_for_path(&source).unwrap());
        app.rename_explorer_path("renamed".into());
        while app.explorer.mutation_pending() {
            app.process_vfs_results();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fs::read_to_string(&source_file).unwrap(), "original");
        assert!(!target.exists());
        assert_eq!(
            app.state.buffers[original].path.as_ref(),
            Some(&source_file)
        );
        assert_eq!(app.state.buffers[unsaved].path.as_ref(), Some(&target_file));
        assert_eq!(app.state.buffers[unsaved].text.to_string(), "unsaved");
        assert!(app.ui.notification_error);
    }
}


#[test]
pub(in crate::app) fn explorer_rename_and_delete_reconcile_open_buffers_without_losing_text() {
    let (_app_dirs, mut app) = app();
    let source = app.state.workspace_root.join("source.txt");
    fs::write(&source, "kept").unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("kept plus unsaved"),
            Some(source.clone()),
        )
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.explorer.tree().id_for_path(&source).is_none() {
        app.process_explorer_result();
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer.select(
        app.explorer
            .tree()
            .id_for_path(&source)
            .expect("source node"),
    );
    app.rename_explorer_path("renamed.txt".into());
    let target = app.state.workspace_root.join("renamed.txt");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.state.buffers[buffer].path.as_ref() != Some(&target) {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.state.buffers[buffer].path.as_ref(),
        Some(&target),
        "notice={:?}; roots={:?}; pending={}; disk target={}",
        app.notices.current(),
        app.workspace.roots(),
        app.explorer.mutation_pending(),
        target.exists()
    );
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&target).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&target).unwrap());
    let ready_delete = |app: &mut App| {
        app.request_explorer_delete();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app
            .input
            .prompt()
            .is_none_or(|p| p.kind != crate::ui::PromptKind::ExplorerDelete)
        {
            app.process_vfs_results();
            assert!(Instant::now() < deadline, "delete preflight timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    ready_delete(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No
    );
    app.submit_prompt();
    assert!(!app.explorer.mutation_pending());
    assert!(target.exists(), "default Enter/No must not delete");
    app.confirm_explorer_delete();
    assert!(target.exists());
    ready_delete(&mut app);
    let original = target.with_extension("original");
    fs::rename(&target, &original).unwrap();
    fs::write(&target, "replacement must survive").unwrap();
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.mutation_pending() {
        app.process_vfs_results();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "replacement must survive"
    );
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    fs::remove_file(&target).unwrap();
    fs::rename(&original, &target).unwrap();
    ready_delete(&mut app);
    let root_node = app
        .explorer
        .tree()
        .id_for_path(&app.state.workspace_root)
        .expect("workspace root node");
    app.explorer.select(root_node); // the retained operation still targets the original file
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && app.state.buffers[buffer].path.is_some() {
        app.process_vfs_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!target.exists());
    assert!(app.state.buffers[buffer].path.is_none());
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );
    let opened = app.state.workspace_root.join("opened.txt");
    fs::write(&opened, "open through owner").unwrap();
    app.refresh_explorer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.tree().id_for_path(&opened).is_none() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.explorer
        .select(app.explorer.tree().id_for_path(&opened).unwrap());
    app.explorer_open_file(false);
    app.ui.explorer = None;
    app.reconcile_explorer_watch();
    wait_vfs(&mut app);
    assert!(
        !app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&opened)),
        "closed explorer read resurrected a document"
    );
    app.open_explorer();
    app.explorer
        .select(app.explorer.tree().id_for_path(&opened).unwrap());
    let panes = app.state.panes.len();
    app.explorer_open_file(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.panes.len(), panes + 1);
    let active = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[active].path.as_ref(), Some(&opened));
    assert_eq!(
        app.state.buffers[active].text.to_string(),
        "open through owner"
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "kept plus unsaved"
    );
}


pub(in crate::app) fn wait_explorer(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.explorer.pending() {
        app.process_explorer_result();
        assert!(Instant::now() < deadline, "explorer timed out");
        thread::yield_now();
    }
}


#[test]
pub(in crate::app) fn explorer_lazily_loads_and_opens_scoped_file() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("explorer.txt");
    fs::write(&path, "explorer content\n").expect("fixture");
    app.open_explorer();
    app.explorer_toggle();
    wait_explorer(&mut app);
    assert_eq!(app.explorer.tree().visible().len(), 2);
    let explorer_ui = app.ui.explorer.as_ref().expect("dedicated explorer ui");
    assert_eq!(explorer_ui.rows.len(), 2, "the root and its file");
    assert!(explorer_ui.rows[1].node_id > 0);
    assert!(explorer_ui.rows[1].selected);
    assert!(
        explorer_ui.rows[1]
            .segments
            .iter()
            .any(|segment| segment.role == crate::ui::ExplorerSegmentRole::File)
    );
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
        app.configuration.install_fixture(fixture);
    }
    app.sync_explorer_ui();
    assert!(app.ui.explorer.as_ref().is_some_and(|ui| {
        ui.rows[1].segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "."
        })
    }));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Unicode;
        app.configuration.install_fixture(fixture);
    }
    app.sync_explorer_ui();
    assert!(app.ui.explorer.as_ref().is_some_and(|ui| {
        ui.rows[1].segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "·"
        })
    }));
    app.explorer.navigate(1);
    app.reconcile_editor_windows();
    app.explorer_open_file(false);
    assert!(app.ui.explorer.is_some());
    assert!(!app.ui.explorer_focused);
    // A normal redraw can reconcile the graph before asynchronous I/O finishes.
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    // Without Nerd Font icons the file's tab uses the same plain glyph as the explorer.
    app.reconcile_editor_windows();
    assert!(!app.ui.content_tabs.is_empty());
    assert!(app.ui.content_tabs.iter().all(|tab| tab.icon == "·"));
    assert_eq!(app.active_input_context(), "editor.normal");
    let tab = app.state.active_tab;
    let view = app.state.active_view;

    // Reopening an existing file switches back to its tab/view, without
    // duplicating the buffer or replacing its in-memory contents from disk.
    app.state.new_tab().unwrap();
    app.process_effect(AppEffect::NewBuffer);
    fs::write(&path, "different disk contents").unwrap();
    let buffers = app.state.buffers.len();
    app.open_explorer();
    app.explorer
        .select(app.explorer.tree().id_for_path(&path).unwrap());
    app.reconcile_editor_windows();
    app.explorer_open_file(false);
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(buffer));
    assert_eq!(app.state.active_tab, tab);
    assert_eq!(app.state.active_view, view);
    assert_eq!(app.state.buffers.len(), buffers);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "explorer content\n"
    );
    assert_eq!(app.active_input_context(), "editor.normal");

    app.open_explorer();
    app.explorer_open_file(true);
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_tab_views().len(), 2);

    // Genuine navigation while the read is outstanding still wins.
    app.open_explorer();
    app.explorer_open_file(false);
    app.process_effect(AppEffect::NewBuffer);
    let newer = app.state.active_buffer_id();
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), newer);
}


#[test]
pub(in crate::app) fn explorer_uses_filename_and_extension_specific_nerd_font_icons() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("README.md"), "# Dust\n").unwrap();
    fs::write(app.state.workspace_root.join("notes.md"), "notes\n").unwrap();
    fs::write(app.state.workspace_root.join("main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(app.state.workspace_root.join("src")).unwrap();

    app.open_explorer();
    app.explorer_toggle();
    wait_explorer(&mut app);
    let rows = &app.ui.explorer.as_ref().expect("explorer ui").rows;
    for (name, glyph, role) in [
        ("README.md", "", crate::ui::ExplorerSegmentRole::IconYellow),
        ("notes.md", "󰍔", crate::ui::ExplorerSegmentRole::IconGrey),
        ("main.rs", "󱘗", crate::ui::ExplorerSegmentRole::IconOrange),
        ("src", "󰴉", crate::ui::ExplorerSegmentRole::IconPurple),
    ] {
        let row = rows
            .iter()
            .find(|row| row.segments.iter().any(|segment| segment.text == name))
            .unwrap_or_else(|| panic!("missing explorer row for {name}"));
        assert!(
            row.segments
                .iter()
                .any(|segment| segment.text == glyph && segment.role == role),
            "wrong explorer icon for {name}"
        );
    }
}


#[test]
pub(in crate::app) fn explorer_follow_lazily_expands_to_active_file() {
    let (_app_dirs, mut app) = app();
    let directory = app.state.workspace_root.join("a/b");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("follow.txt");
    fs::write(&path, "follow").unwrap();
    app.state
        .open_text(TextBuffer::from_text("follow"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.follow_active_file_in_explorer();
    wait_explorer(&mut app);
    assert_eq!(
        app.explorer.tree().selected_node().map(|node| &node.path),
        Some(&path)
    );
    assert!(!app.explorer.following());
    assert!(
        app.ui
            .explorer
            .as_ref()
            .is_some_and(|ui| ui.rows.iter().any(|row| row.selected))
    );
}


#[test]
pub(in crate::app) fn explorer_watch_batches_reload_through_directory_service_and_preserve_identity() {
    let (_app_dirs, mut app) = app();
    let directory = app.state.workspace_root.join("dir");
    fs::create_dir_all(&directory).unwrap();
    let kept = directory.join("kept.txt");
    fs::write(&kept, "x").unwrap();
    fs::write(app.state.workspace_root.join("root.txt"), "x").unwrap();
    app.open_explorer();
    wait_explorer(&mut app);
    let directory_id = app.explorer.tree().id_for_path(&directory).unwrap();
    app.explorer.select(directory_id);
    app.explorer_toggle();
    wait_explorer(&mut app);
    let kept_id = app.explorer.tree().id_for_path(&kept).unwrap();
    app.explorer.select(kept_id);
    app.sync_explorer_ui();
    let generation = app.explorer.watch_generation().unwrap();
    let root = app.state.workspace_root.clone();
    let stale = ExplorerWatchBatch {
        generation: ExplorerWatchGeneration(generation.0.saturating_sub(1)),
        root_revision: app.workspace.root_revision(),
        directories: vec![root.clone()],
    };
    app.accept_explorer_watch_batch(stale);
    assert!(app.explorer.watch_queue_empty());
    let added = root.join("added.txt");
    fs::write(&added, "x").unwrap();
    let hidden = root.join(".hidden");
    let ignored = root.join("ignored.log");
    fs::write(&hidden, "x").unwrap();
    fs::write(&ignored, "x").unwrap();
    fs::write(root.join(".ignore"), "*.log\n").unwrap();
    app.accept_explorer_watch_batch(ExplorerWatchBatch {
        generation,
        root_revision: app.workspace.root_revision(),
        directories: vec![root.clone()],
    });
    app.start_next_explorer_watch_reload();
    wait_explorer(&mut app);
    assert!(app.explorer.tree().id_for_path(&added).is_some());
    assert!(app.explorer.tree().id_for_path(&hidden).is_none());
    assert!(app.explorer.tree().id_for_path(&ignored).is_none());
    assert_eq!(
        app.explorer.tree().id_for_path(&directory),
        Some(directory_id)
    );
    assert!(app.explorer.tree().is_expanded(directory_id));
    assert_eq!(app.explorer.tree().id_for_path(&kept), Some(kept_id));
    fs::remove_file(&kept).unwrap();
    let generation = app.explorer.watch_generation().unwrap();
    app.accept_explorer_watch_batch(ExplorerWatchBatch {
        generation,
        root_revision: app.workspace.root_revision(),
        directories: vec![directory.clone()],
    });
    app.start_next_explorer_watch_reload();
    wait_explorer(&mut app);
    assert!(app.explorer.tree().id_for_path(&kept).is_none());
    assert_eq!(app.explorer.tree().selected_id(), Some(directory_id));
    let reconciles = app.explorer.watch_reconciles();
    let pending = app.documents.legacy_count();
    for _ in 0..256 {
        app.reconcile_editor_windows();
        let _ = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
    }
    assert_eq!(app.explorer.watch_reconciles(), reconciles);
    assert_eq!(app.documents.legacy_count(), pending);
    app.explorer.select(directory_id);
    app.explorer.toggle_loaded(directory_id);
    app.sync_explorer_ui();
    assert!(
        !app.explorer
            .tree()
            .watched_directories()
            .contains(&directory)
    );
    app.process_effect(AppEffect::ExplorerClose);
    assert!(app.explorer.watch_generation().is_none());
}


#[test]
pub(in crate::app) fn explorer_projection_formats_only_the_terminal_viewport() {
    let (_app_dirs, mut app) = app();
    app.window_session
        .observe_size((app.window_session.width(), 10));
    for index in 0..40 {
        fs::write(
            app.state.workspace_root.join(format!("file{index}.txt")),
            "fixture",
        )
        .expect("fixture");
    }
    app.open_explorer();
    wait_explorer(&mut app);
    let explorer = app.ui.explorer.as_ref().expect("explorer projection");
    assert_eq!(explorer.total, 41);
    assert_eq!(explorer.rows.len(), 7);
    assert!(explorer.rows.len() < explorer.total);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    let first_child = app.explorer.tree().selected_id().expect("first child");
    let projection_generation = app.explorer.tree().projection_generation();

    app.move_explorer_selection(-1);
    let root = app.explorer.tree().selected_id();
    assert_ne!(root, Some(first_child));
    app.move_explorer_selection(1);
    assert_eq!(app.explorer.tree().selected_id(), Some(first_child));
    app.explorer_collapse_or_parent();
    assert_eq!(app.explorer.tree().selected_id(), root);
    app.move_explorer_selection(1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.handle_event(crate::ui::UiEvent::Resize(80, 20));
    let explorer = app.ui.explorer.as_ref().expect("grown projection");
    assert_eq!(explorer.rows.len(), 17);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );
    app.handle_event(crate::ui::UiEvent::Resize(80, 8));
    let explorer = app.ui.explorer.as_ref().expect("shrunk projection");
    assert_eq!(explorer.rows.len(), 5);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    app.move_explorer_selection(30);
    let explorer = app.ui.explorer.as_ref().expect("scrolled projection");
    assert_eq!(explorer.rows.len(), 5);
    assert!(explorer.scroll > 0);
    assert!(explorer.rows.iter().any(|row| row.selected));
    let before_page = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    app.page_explorer_selection(-1);
    let after_page = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    assert!(after_page < before_page);
    app.page_explorer_selection(1);
    assert!(
        app.explorer
            .tree()
            .visible_index(app.explorer.tree().selected_id().unwrap())
            .unwrap()
            > after_page
    );
    let rect = app.explorer_window_rect().unwrap();
    let wheel = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseScrollUp,
        crate::editor::input::Modifiers::default(),
    );
    let before_wheel = app
        .explorer
        .tree()
        .visible_index(app.explorer.tree().selected_id().unwrap())
        .unwrap();
    app.handle_event(UiEvent::Mouse {
        key: wheel,
        column: rect.x,
        row: rect.y.saturating_add(2),
    });
    assert!(
        app.explorer
            .tree()
            .visible_index(app.explorer.tree().selected_id().unwrap())
            .unwrap()
            < before_wheel
    );
    app.move_explorer_selection(isize::MIN);
    let top = app.explorer.tree().selected_id();
    app.page_explorer_selection(-1);
    assert_eq!(app.explorer.tree().selected_id(), top);
    app.move_explorer_selection(isize::MAX);
    let bottom = app.explorer.tree().selected_id();
    app.page_explorer_selection(1);
    assert_eq!(app.explorer.tree().selected_id(), bottom);
    let first_viewport_node = app.ui.explorer.as_ref().unwrap().rows[0].node_id;
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            crate::editor::input::Modifiers::default(),
        ),
        column: rect.x.saturating_add(1),
        row: rect.y.saturating_add(3),
    });
    assert_eq!(
        app.explorer.tree().selected_id(),
        Some(crate::app::explorer::tree::ExplorerNodeId(first_viewport_node))
    );
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );

    let projected_before_rejection = app.ui.explorer.clone();
    let mut invalid = crate::config::load_bundled_config();
    invalid.config.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
    invalid
        .config
        .input
        .text_commands
        .insert("editor.insert".into(), "unknown.command".into());
    assert!(app.apply_configuration_candidate(invalid, false).is_err());
    assert_eq!(app.ui.explorer, projected_before_rejection);
    assert_eq!(
        app.configuration.settings().ui.explorer.icon_mode,
        crate::config::ExplorerIconMode::NerdFont
    );

    let mut valid = crate::config::load_bundled_config();
    valid.config.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
    app.apply_configuration_candidate(valid, false)
        .expect("valid explorer reload");
    let explorer = app.ui.explorer.as_ref().expect("reconfigured projection");
    assert_eq!(explorer.rows.len(), 5);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert!(explorer.rows.iter().all(|row| {
        row.segments.iter().any(|segment| {
            segment.role == crate::ui::ExplorerSegmentRole::Icon && segment.text == "."
        })
    }));
    assert_eq!(
        app.explorer.tree().projection_generation(),
        projection_generation
    );
}


#[test]
pub(in crate::app) fn directional_focus_moves_spatially_then_crosses_the_explorer_boundary() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("focus"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.apply_layout_action(crate::app::windows::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    ));
    let tab = app.state.active_tab.expect("active tab");
    let right = app.state.tabs[tab].focused;
    let left = app
        .state
        .directional_pane(crate::editor::FocusDirection::Left)
        .expect("directional query")
        .expect("left pane");

    app.open_explorer();
    assert!(app.ui.explorer_focused);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert_eq!(app.state.tabs[tab].focused, right);

    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tabs[tab].focused, left);
}
use crate::app::documents::DocumentEvent;
use std::time::Duration;
use std::time::Instant;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::editor::FocusDirection;
use crate::ui::PromptKind;
use crate::ui::UiEvent;
use std::fs;
use std::thread;
use crate::files::ExplorerWatchBatch;
use crate::files::ExplorerWatchGeneration;
use crate::ui::ExplorerSegmentRole;
use crate::ui::render;
use crate::app::App;
use crate::app::AppEffect;

