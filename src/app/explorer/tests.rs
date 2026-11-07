//! Explorer behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::documents::DocumentEvent;
use crate::app::tests::*;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::files::ExplorerWatchBatch;
use crate::files::ExplorerWatchId;
use crate::process::PtyId;
use crate::ui::PtyScreen;
use crate::ui::UiEvent;
use crate::ui::render;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn explorer_command_prompt_creates_a_file_from_the_selected_directory() {
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
fn explorer_prefilled_rename_keeps_original_target_and_dirty_split_views() {
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
        .split_active(crate::windows::TileAxis::Vertical, 500)
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
fn explorer_rename_prompt_refuses_replaced_files_and_stale_or_cancelled_requests() {
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
    // Cancelling after preparation submission still performs no mutation.
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
    // A root epoch change invalidates the original prompt, even for equal path strings.
    app.open_explorer_rename_prompt();
    app.explorer
        .reset_roots(app.workspace.roots(), app.workspace.root_revision());
    app.cancel_stale_explorer_intents();
    wait_vfs(&mut app);
    assert!(app.input.prompt().is_none());
    assert!(!target.exists());
}

#[test]
fn explorer_rename_rejects_destinations_owned_by_unsaved_buffers() {
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
fn explorer_close_reconciles_an_accepted_rename_even_after_disk_completion() {
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
    // Submit the prepared write, but do not consume its completion yet.
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
    app.cancel_stale_explorer_intents();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&target));
    assert!(!app.explorer.mutation_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "kept");
}

#[test]
fn explorer_rename_refuses_destinations_owned_by_unsaved_buffers_before_writing() {
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
fn explorer_rename_and_delete_reconcile_open_buffers_without_losing_text() {
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
    let panes = app.state.editor_windows().count();
    app.explorer_open_file(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.editor_windows().count(), panes + 1);
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
fn explorer_lazily_loads_and_opens_scoped_file() {
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
    let tab = app.state.active_tab();
    let view = app.state.active_view();

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
    assert_eq!(app.state.active_tab(), tab);
    assert_eq!(app.state.active_view(), view);
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
fn explorer_uses_filename_and_extension_specific_nerd_font_icons() {
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
fn explorer_follow_lazily_expands_to_active_file() {
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
fn explorer_watch_batches_reload_through_directory_service_and_preserve_identity() {
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
    let stream_id = app.explorer.watch_stream_id().unwrap();
    let root = app.state.workspace_root.clone();
    let stale = ExplorerWatchBatch {
        stream_id: ExplorerWatchId(stream_id.0.saturating_sub(1)),
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
        stream_id,
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
    let stream_id = app.explorer.watch_stream_id().unwrap();
    app.accept_explorer_watch_batch(ExplorerWatchBatch {
        stream_id,
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
    assert!(app.explorer.watch_stream_id().is_none());
}

#[test]
fn explorer_projection_formats_only_the_terminal_viewport() {
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
    let projection_revision = app.explorer.tree().projection_revision();

    app.move_explorer_selection(-1);
    let root = app.explorer.tree().selected_id();
    assert_ne!(root, Some(first_child));
    app.move_explorer_selection(1);
    assert_eq!(app.explorer.tree().selected_id(), Some(first_child));
    app.explorer_collapse_or_parent();
    assert_eq!(app.explorer.tree().selected_id(), root);
    app.move_explorer_selection(1);
    assert_eq!(
        app.explorer.tree().projection_revision(),
        projection_revision
    );

    app.handle_event(crate::ui::UiEvent::Resize(80, 20));
    let explorer = app.ui.explorer.as_ref().expect("grown projection");
    assert_eq!(explorer.rows.len(), 17);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_revision(),
        projection_revision
    );
    app.handle_event(crate::ui::UiEvent::Resize(80, 8));
    let explorer = app.ui.explorer.as_ref().expect("shrunk projection");
    assert_eq!(explorer.rows.len(), 5);
    assert_eq!(explorer.rows.iter().filter(|row| row.selected).count(), 1);
    assert_eq!(
        app.explorer.tree().projection_revision(),
        projection_revision
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
        Some(crate::app::explorer::tree::ExplorerNodeId(
            first_viewport_node
        ))
    );
    assert_eq!(
        app.explorer.tree().projection_revision(),
        projection_revision
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
        app.explorer.tree().projection_revision(),
        projection_revision
    );
}

#[test]
fn directional_focus_moves_spatially_then_crosses_the_explorer_boundary() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("focus"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.apply_layout_action(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    ));
    let tab = app.state.active_tab().expect("active tab");
    let right = app.state.tab_focused_window(tab).unwrap();
    let left = app
        .state
        .directional_pane(crate::windows::SpatialDirection::Left)
        .expect("directional query")
        .expect("left pane");

    app.open_explorer();
    assert!(app.ui.explorer_focused);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), right);

    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), left);
    app.handle_configured_key("ctrl-h".parse::<KeyEvent>().expect("Ctrl-h"));
    assert!(app.ui.explorer_focused);
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), left);
    app.handle_configured_key("ctrl-l".parse::<KeyEvent>().expect("Ctrl-l"));
    assert!(!app.ui.explorer_focused);
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), left);
}

#[test]
fn directional_resize_uses_the_focused_explorer_clone() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let primary = app.window_session.native_ids().explorer().unwrap().window;
    app.clone_graph_window("explorer", crate::windows::TileAxis::Horizontal)
        .unwrap();
    let clone = app.window_session.graph().focused().unwrap();
    assert_ne!(primary, clone);
    app.place_graph_window(
        &format!("window:{}", clone.0),
        WindowPlacement::Floating(WindowRect {
            x: 3,
            y: 3,
            width: 20,
            height: 8,
        }),
    )
    .unwrap();
    let primary_placement = app
        .window_session
        .graph()
        .window(primary)
        .unwrap()
        .placement;
    app.process_effect(AppEffect::WindowResizeDirectional(
        crate::windows::SpatialDirection::Right,
        1,
    ));
    assert_eq!(
        app.window_session
            .graph()
            .window(primary)
            .unwrap()
            .placement,
        primary_placement
    );
    assert!(
        matches!(app.window_session.graph().window(clone).unwrap().placement, WindowPlacement::Floating(rect) if rect.width > 20)
    );
    app.process_effect(AppEffect::WindowResizeDirectional(
        crate::windows::SpatialDirection::Left,
        1_000_000,
    ));
    assert!(
        matches!(app.window_session.graph().window(clone).unwrap().placement, WindowPlacement::Floating(rect) if rect.width == 1)
    );
}

#[test]
fn dashboard_exposes_and_dispatches_the_file_explorer_without_picker_fallback() {
    let (_app_dirs, mut app) = app();
    app.project_configuration();
    assert!(app.ui.dashboard);
    assert!(
        app.ui
            .dashboard_actions
            .iter()
            .any(|action| { action.label == "Explorer" && action.key.as_deref() == Some("e") }),
        "resolved dashboard actions: {:?}",
        app.ui.dashboard_actions
    );
    for key in ["space", "f", "e"] {
        app.handle_configured_key(key.parse::<KeyEvent>().expect("dashboard explorer key"));
    }
    assert!(app.ui.explorer.is_some());
    assert!(app.ui.explorer_focused);
    assert!(app.ui.picker.is_none());
}

#[test]
fn clicking_explorer_cancels_flash_without_swallowing_the_next_key() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc abc abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    app.set_explorer_focus(false);
    app.reconcile_editor_windows();
    app.begin_flash();
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('a'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert!(app.flash.is_some());
    let rect = app.explorer_window_rect().unwrap();
    app.handle_event(UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseLeftDown,
            Default::default(),
        ),
        column: rect.x + 1,
        row: rect.y + 1,
    });
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character('j'),
        kind: crate::ui::KeyInputKind::Press,
    });
    assert!(app.flash.is_none());
    assert!(app.ui.flash.is_none());
    assert!(app.window_session.explorer_focused());
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
}

#[test]
fn explorer_is_a_graph_dock_and_bundled_disable_is_transactional() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let ids = app
        .window_session
        .native_ids()
        .explorer()
        .expect("explorer resources");
    assert!(matches!(
        app.window_session
            .graph()
            .window(ids.window)
            .expect("explorer window")
            .placement,
        WindowPlacement::Docked {
            edge: crate::windows::DockEdge::Left,
            ..
        }
    ));
    let rect = app.explorer_window_rect().expect("projected explorer");
    // The status line keeps the last row of the screen.
    assert!(rect.width > 1 && rect.height == app.window_session.height() - 1);
}

#[test]
fn the_explorer_moves_to_the_side_its_setting_names_while_open() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let left = app.explorer_window_rect().expect("projected explorer");
    assert_eq!(left.x, 0);
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.ui.explorer.side = crate::config::PanelSide::Right;
    app.configuration.install_fixture(fixture);
    app.reconcile_editor_windows();
    let right = app.explorer_window_rect().expect("projected explorer");
    assert_eq!(
        right.x.saturating_add(right.width),
        app.window_session.width()
    );
    assert_eq!(right.width, left.width);
}

#[test]
fn swapping_a_configured_explorer_into_hidden_retains_its_actual_placement() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let placement = WindowPlacement::Docked {
        edge: DockEdge::Right,
        size: 31,
    };
    let candidate = app
        .prepare_configured_window_rules(&crate::config::WindowsConfig {
            rules: vec![crate::config::WindowRuleConfig {
                content_type: "builtin.explorer".into(),
                placement: crate::config::WindowPlacementConfig::Docked {
                    edge: "right".into(),
                    size: 31,
                },
            }],
        })
        .unwrap();
    app.window_session
        .accept_current(candidate, &app.state.window_version(), &mut app.state)
        .unwrap();
    app.open_explorer();
    let explorer = app.window_session.native_ids().explorer().unwrap();
    app.terminal_session.fixture(
        PtyId(7),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(7));
    app.sync_terminal_panel();
    app.place_graph_window("terminal", WindowPlacement::Hidden)
        .unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        placement
    );
    app.swap_graph_windows("explorer", "terminal").unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    app.show_graph_window("explorer").unwrap();
    assert_eq!(
        app.window_session
            .graph()
            .window(explorer.window)
            .unwrap()
            .placement,
        placement
    );
}

#[test]
fn hiding_the_terminal_goes_back_to_the_file_not_the_explorer() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_event(crate::ui::UiEvent::Resize(100, 30));
    app.process_effect(AppEffect::ExplorerOpen);
    app.set_explorer_focus(false);
    app.reconcile_editor_windows();
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    app.process_effect(AppEffect::TerminalToggle);
    app.reconcile_editor_windows();
    for _ in 0..2 {
        assert_eq!(app.active_input_context(), "editor.normal");
        app.process_effect(AppEffect::TerminalToggle);
        assert!(
            app.window_session.terminal_focused(),
            "{:?}",
            app.ui.notification
        );
        app.process_effect(AppEffect::TerminalToggle);
        app.reconcile_editor_windows();
        assert!(!app.window_session.terminal_focused());
        assert!(
            !app.window_session.explorer_focused(),
            "the file had the keyboard before"
        );
    }
    // From the explorer, it goes back to the explorer.
    app.set_explorer_focus(true);
    app.process_effect(AppEffect::TerminalToggle);
    app.process_effect(AppEffect::TerminalToggle);
    app.reconcile_editor_windows();
    assert!(app.window_session.explorer_focused());
}

#[test]
fn picker_mouse_input_does_not_scroll_the_background_explorer() {
    let (_app_dirs, mut app) = app();
    for name in ["a", "b", "c", "d", "e"] {
        fs::write(app.state.workspace_root.join(name), name).unwrap();
    }
    app.process_effect(AppEffect::ExplorerOpen);
    wait_vfs(&mut app);
    app.reconcile_editor_windows();
    let explorer = app.explorer_window_rect().unwrap();
    let before = app.ui.explorer.clone();
    app.process_effect(AppEffect::OpenPicker(
        crate::app::commands::PickerKind::Buffers,
    ));
    app.handle_event(UiEvent::Mouse {
        key: "mouse-scroll-down".parse().unwrap(),
        column: explorer.x,
        row: explorer.y + 1,
    });
    assert_eq!(app.ui.explorer, before);
}

use crate::app::explorer::*;
pub(in crate::app) fn fixture(root: &std::path::Path) -> (Explorer, WorkspaceVersion) {
    let identity = crate::files::RootIdentity::capture(root).unwrap();
    let state = crate::workspace::WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [("root".into(), identity)],
    )
    .unwrap();
    let workspace =
        crate::app::workspace::Workspace::new(state, root.join("trust.json"), 1, false, false, 1);
    let mut owner = Explorer::new(
        ExplorerWatchWorker::start(crate::files::ExplorerWatchPolicy::default()).unwrap(),
    );
    owner.reset_roots(workspace.roots(), workspace.root_revision());
    (owner, workspace.version())
}
pub(in crate::app) fn drain(owner: &mut Explorer, scope: WorkspaceVersion) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while owner.pending() {
        owner.poll(scope.clone(), 1);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn reopening_a_collapsed_directory_reloads_missed_changes() {
    let root = tempfile::tempdir().unwrap();
    let (mut owner, source) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source.clone());
    owner.toggle_loaded(node);
    std::fs::write(root.path().join("new.txt"), "new").unwrap();
    owner.toggle_loaded(node);
    let request = owner.next_reload().unwrap();
    assert_eq!(request.node, node);
    owner.load_request(request, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source);
    assert!(
        owner
            .tree()
            .id_for_path(&root.path().join("new.txt"))
            .is_some()
    );
}

#[test]
fn queued_directory_loads_are_deduplicated_and_survive_continuations() {
    let root = tempfile::tempdir().unwrap();
    for name in ["a", "b", "c"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    for name in ["one", "two", "three"] {
        std::fs::write(root.path().join("b").join(name), "text").unwrap();
    }
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let [a, b, c] =
        ["a", "b", "c"].map(|name| owner.tree().id_for_path(&root.path().join(name)).unwrap());
    owner.load(a, scope.clone(), 1, 2).unwrap();
    owner.load(b, scope.clone(), 1, 2).unwrap();
    owner.load(b, scope.clone(), 1, 2).unwrap();
    owner.load(c, scope.clone(), 1, 2).unwrap();
    drain(&mut owner, scope.clone());
    for expected in [b, c, b] {
        let request = owner.next_reload().unwrap();
        assert_eq!(request.node, expected);
        owner.load_request(request, scope.clone(), 1, 2).unwrap();
        drain(&mut owner, scope.clone());
    }
    assert!(owner.next_reload().is_none());
    for node in [a, b, c] {
        assert_eq!(
            owner.tree().node(node).unwrap().load,
            ExplorerLoadState::Loaded
        );
        assert!(owner.tree().is_expanded(node));
    }
    assert_eq!(owner.tree().node(b).unwrap().children.len(), 3);
}

#[test]
fn queued_search_load_does_not_expand_after_the_filter_is_cleared() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("dir")).unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let dir = owner.tree().id_for_path(&root.path().join("dir")).unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    owner.set_search("needle".into());
    owner.load(dir, scope.clone(), 1, 100).unwrap();
    owner.set_search(String::new());
    drain(&mut owner, scope.clone());
    let request = owner.next_reload().unwrap();
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope);
    assert_eq!(
        owner.tree().node(dir).unwrap().load,
        ExplorerLoadState::Loaded
    );
    assert!(!owner.tree().is_expanded(dir));
}

#[test]
fn queued_expansions_stay_with_each_requesting_view() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("dir")).unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let dir = owner.tree().id_for_path(&root.path().join("dir")).unwrap();
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = owner.accept_clone(candidate).unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    owner.load(dir, scope.clone(), 1, 100).unwrap();
    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.load(dir, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let request = owner.next_reload().unwrap();
    assert_eq!(request.view, clone);
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    assert!(owner.tree_for(clone).unwrap().is_expanded(dir));
    assert!(!owner.tree().is_expanded(dir));
    let request = owner.next_reload().unwrap();
    assert_eq!(request.view, PRIMARY_EXPLORER_VIEW);
    owner.activate_view(clone).unwrap();
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope);
    assert!(
        owner
            .tree_for(PRIMARY_EXPLORER_VIEW)
            .unwrap()
            .is_expanded(dir)
    );
    assert!(owner.next_reload().is_none());
}

#[test]
fn child_navigation_reloads_a_collapsed_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    let old = directory.join("old.txt");
    let new = directory.join("new.txt");
    std::fs::write(&old, "old").unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let node = owner.tree().id_for_path(&directory).unwrap();
    owner.load(node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    owner.select(node);
    owner.toggle_loaded(node);
    std::fs::remove_file(&old).unwrap();
    std::fs::write(&new, "new").unwrap();

    owner.select_first_child();
    assert!(owner.tree().is_expanded(node));
    assert!(
        owner
            .tree()
            .visible_index(owner.tree().selected_id().unwrap())
            .is_some()
    );
    let request = owner.next_reload().unwrap();
    assert_eq!(request.node, node);
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope);
    assert!(owner.tree().id_for_path(&old).is_none());
    assert!(owner.tree().id_for_path(&new).is_some());
}

#[test]
fn root_load_completion_preserves_a_later_collapse() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.txt"), "text").unwrap();
    let (mut owner, scope) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, scope.clone(), 1, 100).unwrap();
    owner.parent();
    assert!(!owner.tree().is_expanded(node));
    drain(&mut owner, scope);
    assert!(!owner.tree().is_expanded(node));
    assert_eq!(owner.tree().selected_id(), Some(node));
    assert_eq!(owner.tree().visible_ids(), &[node]);
}

#[test]
fn truncated_directories_continue_with_a_larger_existing_limit() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..5 {
        std::fs::write(root.path().join(format!("file{index}")), "text").unwrap();
    }
    let (mut owner, scope) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, scope.clone(), 1, 2).unwrap();
    drain(&mut owner, scope.clone());
    while let Some(request) = owner.next_reload() {
        owner.load_request(request, scope.clone(), 1, 2).unwrap();
        drain(&mut owner, scope.clone());
    }
    assert!(
        owner
            .tree()
            .id_for_path(&root.path().join("file4"))
            .is_some()
    );
    assert_eq!(owner.tree().node(node).unwrap().children.len(), 5);
}

#[test]
fn filtered_collapsed_directories_are_watched_and_reloaded_across_views() {
    for cloned in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        let old = directory.join("match-old.txt");
        let new = directory.join("match-new.txt");
        std::fs::write(&old, "old").unwrap();
        let (mut owner, scope) = fixture(root.path());
        let root_node = owner.tree().selected_id().unwrap();
        owner.load(root_node, scope.clone(), 1, 100).unwrap();
        drain(&mut owner, scope.clone());
        if cloned {
            let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
            let view = owner.accept_clone(candidate).unwrap();
            owner.activate_view(view).unwrap();
        }
        owner.tree_mut().set_filter("match");
        let request = owner.next_reload().expect("search loads directory");
        let directory_node = request.node;
        owner.load_request(request, scope.clone(), 1, 100).unwrap();
        drain(&mut owner, scope.clone());
        assert!(!owner.tree().is_expanded(directory_node));
        assert!(owner.tree().visible_index(directory_node).is_some());
        let filtered_view = owner.active_view;
        if cloned {
            owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
            assert!(owner.tree().filter().is_empty());
        }
        assert!(owner.watched_directories().contains(&directory));
        owner.reconcile_watch(true).unwrap();

        std::fs::remove_file(&old).unwrap();
        std::fs::write(&new, "new").unwrap();
        owner.accept_watch(
            ExplorerWatchBatch {
                stream_id: owner.watch_stream_id.unwrap(),
                root_revision: owner.tree().root_revision(),
                directories: vec![directory.clone()],
            },
            true,
        );
        let request = owner.next_reload().unwrap();
        assert_eq!(request.node, directory_node);
        owner.load_request(request, scope.clone(), 1, 100).unwrap();
        drain(&mut owner, scope);
        assert!(owner.tree().id_for_path(&old).is_none());
        assert!(owner.tree().id_for_path(&new).is_some());
        let filtered = owner.tree_for_mut(filtered_view).unwrap();
        assert!(!filtered.is_expanded(directory_node));
        let new_node = filtered.id_for_path(&new).unwrap();
        assert!(filtered.visible_index(new_node).is_some());
        filtered.set_filter("");
        assert!(!owner.watched_directories().contains(&directory));
        owner.watch_reload.push_back(directory);
        assert_eq!(owner.next_reload(), None);
    }
}

#[test]
fn failed_watch_reconciliation_is_retried_without_tree_changes() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let (mut owner, scope) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let node = owner.tree().id_for_path(&dir).unwrap();
    owner.load(node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope);
    std::fs::remove_dir(&dir).unwrap();
    owner.reconcile_watch(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while owner.poll_watch(true).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(owner.watch_observation.is_none());
    std::fs::create_dir(&dir).unwrap();
    let attempts = owner.watch.reconcile_count();
    owner.reconcile_watch(true).unwrap();
    assert!(owner.watch.reconcile_count() > attempts);
}

#[test]
fn follow_stays_in_its_view_across_directory_completions() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("a/b");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("file");
    std::fs::write(&file, "text").unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    owner.set_search("primary".into());
    let primary_selection = owner.tree().selected_id();
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = owner.accept_clone(candidate).unwrap();
    owner.set_search("file".into());
    owner.follow(file.clone());
    let request = owner.continue_follow().unwrap().unwrap();
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    drain(&mut owner, scope.clone());
    let request = owner.continue_follow().unwrap().unwrap();
    assert_eq!(request.view, clone);
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    assert!(owner.continue_follow().unwrap().is_none());
    assert!(!owner.following());
    assert_eq!(owner.tree().filter(), "primary");
    assert_eq!(owner.tree().selected_id(), primary_selection);
    let tree = owner.tree_for(clone).unwrap();
    assert_eq!(tree.filter(), "");
    assert_eq!(tree.selected_id(), tree.id_for_path(&file));
    for ancestor in [root.path().join("a"), directory] {
        let id = tree.id_for_path(&ancestor).unwrap();
        assert!(tree.is_expanded(id));
        assert!(!owner.tree().is_expanded(id));
    }

    owner.activate_view(clone).unwrap();
    owner.follow(root.path().join("a/missing/file"));
    owner.remove_view(clone).unwrap();
    assert!(!owner.following());
    assert!(owner.continue_follow().unwrap().is_none());
}

#[test]
fn follow_stops_after_an_ancestor_directory_load_fails() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("gone");
    std::fs::create_dir(&directory).unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    std::fs::remove_dir(&directory).unwrap();
    owner.follow(directory.join("file"));
    let request = owner.continue_follow().unwrap().unwrap();
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope);
    assert_eq!(
        owner.tree().node(request.node).unwrap().load,
        ExplorerLoadState::Error
    );
    assert!(owner.continue_follow().is_err());
    assert!(!owner.following());
    assert!(owner.continue_follow().unwrap().is_none());
    assert!(owner.next_reload().is_none());
}

#[test]
fn retained_search_keeps_loading_when_an_unfiltered_clone_is_active() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("a/b");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("needle");
    std::fs::write(&file, "text").unwrap();
    let (mut owner, scope) = fixture(root.path());
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = owner.accept_clone(candidate).unwrap();
    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.set_search("needle".into());
    let request = owner.next_reload().unwrap();
    owner.load_request(request, scope.clone(), 1, 100).unwrap();
    owner.activate_view(clone).unwrap();
    drain(&mut owner, scope.clone());
    let mut loads = 0;
    while let Some(request) = owner.next_reload() {
        assert_eq!(request.view, PRIMARY_EXPLORER_VIEW);
        owner.load_request(request, scope.clone(), 1, 100).unwrap();
        drain(&mut owner, scope.clone());
        loads += 1;
        assert!(loads <= 2);
    }
    assert_eq!(loads, 2);
    assert_eq!(owner.tree().filter(), "");
    let tree = owner.tree_for(PRIMARY_EXPLORER_VIEW).unwrap();
    assert_eq!(tree.filter(), "needle");
    assert!(
        tree.visible_ids()
            .contains(&tree.id_for_path(&file).unwrap())
    );
}

#[test]
fn follow_reveals_cached_children_and_closed_clone_loads_finish() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("dir");
    std::fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    std::fs::write(&file, "text").unwrap();
    let (mut owner, scope) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    let dir = owner.tree().id_for_path(&directory).unwrap();
    owner.load(dir, scope.clone(), 1, 100).unwrap();
    drain(&mut owner, scope.clone());
    if owner.tree().is_expanded(dir) {
        owner.tree_mut().toggle_loaded(dir);
    }
    owner.follow(file.clone());
    owner.continue_follow().unwrap();
    assert!(owner.tree().is_expanded(dir));
    assert_eq!(owner.tree().selected_id(), owner.tree().id_for_path(&file));
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = candidate.id();
    owner.accept_clone(candidate).unwrap();
    owner.active_view = clone;
    owner.load(dir, scope.clone(), 1, 100).unwrap();
    owner.cloned_views.remove(&clone);
    owner.active_view = PRIMARY_EXPLORER_VIEW;
    drain(&mut owner, scope);
    assert_eq!(
        owner.tree().node(dir).unwrap().load,
        ExplorerLoadState::Loaded
    );
}

#[test]
fn closing_and_reopening_invalidates_old_directory_results_before_tree_projection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one.txt");
    std::fs::write(&path, "one").unwrap();
    let (mut owner, source) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.reconcile_watch(true).unwrap();
    owner.load(node, source.clone(), 1, 100).unwrap();
    owner.reconcile_watch(false).unwrap();
    drain(&mut owner, source.clone());
    assert!(owner.tree().id_for_path(&path).is_none());
    assert!(owner.next_reload().is_none());
    owner.reconcile_watch(true).unwrap();
    owner.load(node, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source);
    assert!(owner.tree().id_for_path(&path).is_some());
}
#[test]
fn stale_directory_policy_completes_requests() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a.txt");
    let b = root.path().join("b.txt");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let (mut owner, source) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, source.clone(), 1, 100).unwrap();
    let wait = |owner: &mut Explorer| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while owner.pending() {
            owner.poll(source.clone(), 2);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    wait(&mut owner);
    assert_eq!(
        owner.tree().node(node).unwrap().load,
        ExplorerLoadState::Error
    );
    owner.load(node, source.clone(), 2, 100).unwrap();
    wait(&mut owner);
    assert_eq!(
        owner.tree().node(node).unwrap().load,
        ExplorerLoadState::Loaded
    );
}
#[test]
fn changing_explorer_views_stops_search_input_without_changing_queries() {
    let root = tempfile::tempdir().unwrap();
    let (mut owner, _) = fixture(root.path());
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = owner.accept_clone(candidate).unwrap();
    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.set_search("primary".into());
    owner.start_search();
    owner.activate_view(clone).unwrap();
    assert!(!owner.searching());
    assert_eq!(owner.tree().filter(), "");
    assert_eq!(
        owner.tree_for(PRIMARY_EXPLORER_VIEW).unwrap().filter(),
        "primary"
    );
    owner.start_search();
    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    assert!(!owner.searching());
    assert_eq!(owner.tree().filter(), "primary");
}

#[test]
fn cloned_explorer_views_share_tree_data_but_own_projection_state() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(root.path().join("file.txt"), "file").unwrap();
    std::fs::write(directory.join("nested.txt"), "nested").unwrap();
    let (mut owner, source) = fixture(root.path());
    let root_node = owner.tree().selected_id().unwrap();
    owner.load(root_node, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source.clone());

    let asynchronous_stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    std::fs::write(root.path().join("arrived-later.txt"), "late").unwrap();
    owner.load(root_node, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source);
    assert_eq!(
        owner.accept_clone(asynchronous_stale),
        Err("explorer clone source is stale or foreign")
    );

    let stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.tree_mut().move_selection(1);
    owner.mark_view_changed();
    assert_eq!(
        owner.accept_clone(stale),
        Err("explorer clone source is stale or foreign")
    );

    let primary_selection = owner.tree().selected_id();
    let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    let clone = candidate.id();
    assert_eq!(owner.accept_clone(candidate), Ok(clone));
    assert!(owner.views_share_data(PRIMARY_EXPLORER_VIEW, clone));
    assert_eq!(
        owner.tree_for(clone).unwrap().selected_id(),
        primary_selection
    );

    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    let focus_stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.activate_view(clone).unwrap();
    assert_eq!(
        owner.accept_clone(focus_stale),
        Err("explorer clone source is stale or foreign")
    );

    owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
    owner.tree_mut().move_selection(-1);
    owner.mark_view_changed();
    let moved_primary = owner.tree().selected_id();
    assert_ne!(moved_primary, primary_selection);
    assert_eq!(
        owner.tree_for(clone).unwrap().selected_id(),
        primary_selection
    );

    owner.activate_view(clone).unwrap();
    owner.tree_mut().set_filter("nested");
    owner.mark_view_changed();
    assert_eq!(owner.tree().filter(), "nested");
    assert_eq!(owner.tree_for(PRIMARY_EXPLORER_VIEW).unwrap().filter(), "");
    assert!(owner.views_share_data(PRIMARY_EXPLORER_VIEW, clone));

    owner.select(root_node);
    owner.toggle_loaded(root_node);
    assert!(!owner.tree().is_expanded(root_node));
    assert!(
        owner
            .tree_for(PRIMARY_EXPLORER_VIEW)
            .unwrap()
            .is_expanded(root_node)
    );
}

#[test]
fn explorer_clone_rejects_foreign_exhausted_and_over_limit_sources_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let (mut owner, _) = fixture(root.path());
    let (other, _) = fixture(other_root.path());
    let foreign = other.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
    assert_eq!(
        owner.accept_clone(foreign),
        Err("explorer clone source is stale or foreign")
    );
    assert_eq!(owner.cloned_views.len(), 0);

    for expected in 2..=crate::windows::MAX_WINDOWS as u64 {
        let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        assert_eq!(candidate.id(), ExplorerViewId(expected));
        owner.accept_clone(candidate).unwrap();
    }
    assert_eq!(
        owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap_err(),
        "explorer view limit reached"
    );
    assert_eq!(
        owner.cloned_views.len().saturating_add(1),
        crate::windows::MAX_WINDOWS
    );

    let (mut exhausted, _) = fixture(root.path());
    exhausted.next_view = u64::MAX;
    assert_eq!(
        exhausted.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap_err(),
        "explorer view identity exhausted"
    );
    assert!(exhausted.cloned_views.is_empty());
}

#[test]
#[ignore = "explicit explorer ownership cost probe"]
fn explorer_owner_cost_probe() {
    let root = tempfile::tempdir().unwrap();
    for n in 0..40 {
        std::fs::write(root.path().join(format!("file-{n:02}.txt")), "file").unwrap();
    }
    let (mut owner, source) = fixture(root.path());
    let node = owner.tree().selected_id().unwrap();
    owner.load(node, source.clone(), 1, 100).unwrap();
    drain(&mut owner, source);
    let config = crate::config::load_bundled_config();
    let git = BTreeMap::new();
    let body = crate::windows::WindowRect {
        x: 0,
        y: 3,
        width: 30,
        height: 20,
    };
    let before = (
        crate::windows::WindowGraph::test_clone_count(),
        crate::editor::EditorState::test_clone_count(),
        crate::editor::text::TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = Instant::now();
    let mut rows = 0;
    for _ in 0..100 {
        rows += std::hint::black_box(owner.projection(
            &config.config.ui.explorer,
            body,
            &git,
            "Files".into(),
            "root",
        ))
        .rows
        .len();
    }
    let elapsed = start.elapsed();
    eprintln!("DUST_ALLOC_END");
    let after = crate::editor::text::TextBuffer::test_materializations();
    println!(
        "case=explorer_projection iterations=100 rows_materialized={rows} elapsed_ns={} graph_copies={} editor_copies={} text_decodes={} text_displays={}",
        elapsed.as_nanos(),
        crate::windows::WindowGraph::test_clone_count() - before.0,
        crate::editor::EditorState::test_clone_count() - before.1,
        after.0 - before.2.0,
        after.1 - before.2.1
    );
    assert_eq!(rows, 2000);
}
