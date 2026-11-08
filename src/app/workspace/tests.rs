//! Workspace behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::tests::*;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn workspace_trust_is_one_key_cancels_on_focus_loss_and_persists() {
    let key = |name: &str| name.parse::<KeyEvent>().expect("key");
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.notify_message("stale notification".into(), true);
    app.request_workspace_trust();
    assert!(app.ui.notification.is_none());
    assert!(!app.ui.notification_error);
    let prompt = app.ui.prompt.as_ref().expect("trust prompt");
    assert_eq!(prompt.kind, crate::ui::PromptKind::WorkspaceTrust);
    let confirmation = prompt.confirmation.as_ref().expect("yes/no trust prompt");
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::Yes);
    assert!(confirmation.message.contains("LSP servers"));

    // Escape declines and says what stays off.
    app.handle_configured_key(key("escape"));
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("not trusted"))
    );

    app.request_workspace_trust();
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );

    // Enter accepts the preselected Trust.
    app.request_workspace_trust();
    app.handle_configured_key(key("enter"));
    assert!(app.ui.prompt.is_none());
    wait_trust(&mut app);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    assert!(app.input.search_history().is_empty() && app.input.command_history().is_empty());
    let source = fs::read_to_string(app.workspace.state_path()).expect("persisted trust");
    assert!(source.contains("filesystem_identity"));

    app.revoke_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    wait_trust(&mut app);
    let source = fs::read_to_string(app.workspace.state_path()).expect("persisted revocation");
    assert!(source.contains("identities = []"));
    assert!(source.contains("[[seen]]"));
}

#[test]
fn dashboard_trust_accepts_the_documented_yes_key_without_opening_a_document() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.window_session.observe_size((160, 45));
    app.reconcile_editor_windows();
    assert!(app.ui.dashboard);
    assert!(app.state.active_view().is_none());
    app.request_workspace_trust();
    app.handle_configured_key("y".parse().unwrap());
    assert!(app.ui.prompt.is_none());
    wait_trust(&mut app);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    assert!(app.ui.dashboard);
    assert!(app.state.active_view().is_none());
}

#[test]
fn shutdown_waits_for_a_queued_workspace_revocation() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    // Another instance grants trust after this instance loaded its untrusted snapshot.
    let identity = app.workspace.roots()[0].identity.clone();
    let mut other = crate::workspace::WorkspaceTrustWorker::start(1);
    other
        .persist(path.clone(), vec![identity.clone()], vec![identity], vec![])
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(crate::workspace::TrustEvent::Persisted { result, .. }) =
            other.try_event().unwrap()
        {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let lock = crate::files::lock_file(&path.with_extension("lock")).unwrap();
    app.revoke_workspace_trust();
    assert!(app.workspace.persist_pending());
    thread::scope(|scope| {
        scope.spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(lock);
        });
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(2))
            .unwrap();
    });
    assert!(!app.workspace.persist_pending());
    assert!(
        fs::read_to_string(path)
            .unwrap()
            .contains("identities = []")
    );
}

#[test]
fn shutdown_reports_failed_workspace_trust_writes() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    fs::create_dir_all(&path).unwrap();
    app.revoke_workspace_trust();
    assert!(matches!(
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(2)),
        Err(crate::app::InteractiveError::Vfs(
            crate::files::VfsError::Io {
                operation: "finish workspace trust shutdown",
                ..
            }
        ))
    ));
}

#[test]
fn shutdown_reports_workspace_trust_writes_that_exceed_its_deadline() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let path = app.workspace.state_path().to_path_buf();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = crate::files::lock_file(&path.with_extension("lock")).unwrap();
    app.revoke_workspace_trust();
    let error = app.finish_shutdown_before(Instant::now()).unwrap_err();
    // The shared shutdown result may wrap trust failure with other unfinished work.
    assert!(
        error
            .to_string()
            .contains("finish workspace trust shutdown"),
        "{error}"
    );
    assert!(matches!(
        error,
        crate::app::InteractiveError::Vfs(crate::files::VfsError::Io { .. })
    ));
    assert!(app.workspace.persist_pending());
    drop(lock);
    wait_trust(&mut app);
}

#[test]
fn unseen_workspace_prompts_once_after_identity_and_database_are_ready() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let identity = app.workspace.roots()[0].identity.clone();
    assert!(!app.workspace.seen(&identity));

    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(
        app.ui.prompt.as_ref().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::WorkspaceTrust)
    );
    assert!(app.workspace.seen(&identity));
    app.close_prompt();
    wait_trust(&mut app);
    assert!(
        fs::read_to_string(app.workspace.state_path())
            .expect("seen workspace state")
            .contains("[[seen]]")
    );

    app.maybe_prompt_for_unseen_workspace();
    assert!(app.ui.prompt.is_none());
}

#[test]
fn workspace_history_records_visits_without_trust_prompts() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let first = app.state.workspace_root.clone();
    // Recording the startup visit must not suppress its first trust prompt.
    app.process_trust_events();
    assert!(app.workspace.seen_roots().contains(&first));
    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(
        app.ui.prompt.as_ref().unwrap().kind,
        crate::ui::PromptKind::WorkspaceTrust
    );
    app.close_prompt();
    wait_trust(&mut app);

    let second = tempdir().unwrap();
    let second_path = crate::files::canonicalize(second.path()).unwrap();
    app.switch_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    app.switch_workspace_root(first.clone());
    wait_root_operation(&mut app);
    wait_trust(&mut app);
    assert_eq!(app.state.workspace_root, first);
    assert!(app.workspace.seen_roots().contains(&second_path));
    let database: crate::workspace::TrustDatabase =
        toml::from_str(&fs::read_to_string(app.workspace.state_path()).unwrap()).unwrap();
    assert!(
        database
            .seen
            .iter()
            .any(|identity| identity.canonical_path == second_path)
    );
    assert!(
        database.identities.is_empty(),
        "visiting does not grant trust"
    );
}

#[test]
fn automatic_workspace_trust_prompt_never_steals_an_active_prompt_or_safe_mode() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.maybe_prompt_for_unseen_workspace();
    assert_eq!(
        app.ui.prompt.as_ref().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::Command)
    );
    app.close_prompt();
    app.workspace.fixture_safe_mode(true);
    app.maybe_prompt_for_unseen_workspace();
    assert!(app.ui.prompt.is_none());
}

#[test]
fn workspace_trust_never_activates_before_identity_and_database_are_ready() {
    let (_app_dirs, mut app) = app();
    app.confirm_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    assert!(app.ui.notification_error);
}

pub(in crate::app) fn wait_root_operation(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.workspace.root_operation_pending() {
        app.process_trust_events();
        assert!(Instant::now() < deadline, "root operation timed out");
        thread::yield_now();
    }
}

#[test]
fn async_root_add_activation_and_dirty_removal_preflight_are_real() {
    let (_app_dirs, mut app) = app();
    let first = app.workspace.active_root().expect("first root");
    let second_directory = tempdir().expect("second root");
    let second_path = crate::files::canonicalize(second_directory.path()).expect("canonical");
    app.add_workspace_root(second_path.clone());
    wait_root_operation(&mut app);
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Untrusted
    );
    let second = app.workspace.active_root().expect("added root active");
    assert_eq!(app.state.workspace_root, second_path);
    let first_path = app.workspace.roots()[0].identity.canonical_path.clone();
    // Picking an existing root from :roots activates it instead of replacing the active one.
    app.switch_workspace_root(first_path.clone());
    assert_eq!(app.workspace.active_root(), Some(first));
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(app.state.workspace_root, first_path);
    assert!(!app.ui.notification_error);
    app.switch_workspace_root(second_path.clone());
    assert_eq!(app.workspace.active_root(), Some(second));
    assert_eq!(app.state.workspace_root, second_path);

    let dirty_path = second_path.join("dirty.txt");
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("dirty"),
            Some(dirty_path),
        )
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 2);
    assert!(app.ui.notification_error);
    app.state.buffers[buffer].dirty = false;
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 1);
}

#[test]
fn workspace_switch_rechecks_dirty_buffers_before_publishing() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let original = app.state.workspace_root.clone();
    let roots = app.workspace.roots().to_vec();
    let version = app.workspace.version();
    let target = tempdir().unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("clean"),
            Some(original.join("notes.txt")),
        )
        .unwrap();
    app.switch_workspace_root(target.path().to_path_buf());
    assert!(app.workspace.root_operation_pending());
    // The inspection may have finished on its worker, but its result is not published yet.
    app.state.set_buffer_dirty(buffer, true);
    wait_root_operation(&mut app);
    assert_eq!(app.workspace.roots(), roots);
    assert_eq!(app.workspace.version(), version);
    assert_eq!(app.state.workspace_root, original);
    assert!(app.state.buffers[buffer].dirty);
    assert!(app.ui.notification_error);

    app.state.set_buffer_dirty(buffer, false);
    app.switch_workspace_root(target.path().to_path_buf());
    wait_root_operation(&mut app);
    assert_eq!(
        app.state.workspace_root,
        crate::files::canonicalize(target.path()).unwrap()
    );
}

#[test]
fn stale_async_root_add_and_pending_service_removal_are_rejected() {
    let (_app_dirs, mut app) = app();
    let second = tempdir().expect("second");
    let third = tempdir().expect("third");
    app.add_workspace_root(second.path().to_path_buf());
    app.workspace
        .fixture_add_root(
            "third".into(),
            crate::files::RootIdentity::capture(third.path()).expect("identity"),
        )
        .expect("concurrent model mutation");
    wait_root_operation(&mut app);
    assert_eq!(
        app.workspace.roots().len(),
        2,
        "stale async add did not commit"
    );
    let third_id = app.workspace.active_root().expect("third active");
    app.start_workspace_search("needle".into(), None);
    app.remove_workspace_root(third_id);
    assert_eq!(app.workspace.roots().len(), 2);
    assert!(app.ui.notification_error);
}

pub(in crate::app) fn wait_workspace_search(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.search.pending() || app.workspace_edits.preparing() {
        app.process_workspace_search_events();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "workspace search timed out");
        thread::yield_now();
    }
}

#[test]
fn workspace_file_picker_publishes_the_full_discovery_byte_budget() {
    let (_app_dirs, mut app) = app();
    let files = tempfile::tempdir_in(&app.state.workspace_root).unwrap();
    let mut directory = files.path().to_path_buf();
    // Deep enough to make the budget matter, yet inside macOS's 1024-byte path limit.
    for _ in 0..4 {
        directory.push("d".repeat(200));
    }
    fs::create_dir_all(&directory).unwrap();
    let count = 8000;
    for index in 0..count {
        fs::write(directory.join(format!("file-{index:04}")), "").unwrap();
    }
    // Two path copies fit discovery's budget; a third label copy exceeds it.
    let relative_length = directory
        .strip_prefix(files.path())
        .unwrap()
        .as_os_str()
        .len()
        + 10;
    let absolute_length = directory.as_os_str().len() + 10;
    assert!(count * (relative_length + absolute_length) < 16 * 1024 * 1024);
    assert!(count * (2 * relative_length + absolute_length) > 16 * 1024 * 1024);
    app.search
        .start_files(
            crate::files::WorkspaceFilePolicy {
                roots: vec![files.path().into()],
                root_revision: app.workspace.root_revision(),
                include_hidden: false,
                include_ignored: false,
                maximum_files: 100_000,
                maximum_path_bytes: 16 * 1024 * 1024,
                batch_size: 128,
                event_queue_capacity: 8,
            },
            app.workspace.version(),
            app.configuration.binding_revision(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let picker = app.search.picker().unwrap().presentation();
    assert!(!picker.loading);
    assert_eq!(picker.rows.len(), count);
}

#[test]
fn workspace_file_command_uses_bounded_picker_and_opens_selection() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("find-me.txt");
    let mut contents = String::from("found\n");
    contents.push_str(&"x".repeat(crate::ui::render::picker::PICKER_MAXIMUM_PREVIEW_BYTES));
    fs::write(&path, contents).unwrap();
    assert!(
        fs::metadata(&path).unwrap().len()
            > crate::ui::render::picker::PICKER_MAXIMUM_PREVIEW_BYTES as u64
    );
    app.start_workspace_file_picker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(&mut app);
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.title == "Files"
            && picker.preview.as_ref().is_some_and(|preview| {
                preview.title == "find-me.txt"
                    && preview.as_text().is_some_and(|text| {
                        text.lines().len() == 1
                            && text.lines()[0].contains("preview unavailable:")
                            && text.lines()[0].contains("exceeds")
                    })
            })
    }));
    app.picker_insert("find-me");
    for _ in 0..256 {
        app.sync_picker_ui();
        assert_eq!(app.search.file_requests(), 1)
    }
    app.reconcile_editor_windows();
    app.accept_picker();
    app.reconcile_editor_windows();
    wait_vfs(&mut app);
    assert_eq!(
        app.state
            .active_buffer_id()
            .and_then(|id| app.state.buffers[id].path.as_ref()),
        Some(&path)
    );
}

#[test]
fn workspace_search_uses_generic_picker_and_opens_unopened_match() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("match.txt");
    let text = (0..1_200)
        .map(|line| {
            if line == 1_087 {
                "before needle after".into()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let expected_cursor = text.find("needle").expect("needle offset");
    fs::write(&path, text).expect("fixture");
    let departure = app.state.workspace_root.join("departure.txt");
    fs::write(&departure, "origin\n").unwrap();
    let (original, view) = app
        .state
        .open_text(TextBuffer::from_text("origin\n"), Some(departure))
        .unwrap();
    app.state.views[view].cursor = ByteOffset(1);
    app.invoke_mapped_command("workspace.search.prompt");
    assert!(app.ui.prompt.is_none());
    assert_eq!(
        app.ui.picker.as_ref().map(|picker| picker.title.as_str()),
        Some("Grep")
    );
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    assert_eq!(app.search.matches().len(), 1);
    assert_eq!(
        app.ui.picker.as_ref().map(|picker| picker.title.as_str()),
        Some("Grep")
    );
    assert!(app.ui.picker.as_ref().is_some_and(|picker| {
        picker.preview.as_ref().is_some_and(|preview| {
            let text = preview.as_text().expect("a text preview");
            preview.title == "match.txt"
                && text.highlight_line == Some(1_087)
                && text.highlight_line.is_some_and(|line| {
                    text.lines()
                        .get(line.saturating_sub(text.first_line))
                        .is_some_and(|line| line == "before needle after")
                })
        })
    }));
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened result");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    let view = app.state.active_view().expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(expected_cursor));
    app.walk_jumps(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(original));
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(1)
    );
}

#[test]
fn workspace_search_accepts_normalized_open_buffers() {
    for open in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("crlf.txt");
        let bytes = b"\xef\xbb\xbffirst\r\nneedle\r\n";
        fs::write(&path, bytes).unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        let found = app.search.matches()[0].clone();
        app.close_picker();
        if open {
            app.state
                .open_text(TextBuffer::from_bytes(bytes).unwrap(), Some(path.clone()))
                .unwrap();
        }
        app.navigate_workspace_search_match(found);
        wait_vfs(&mut app);
        let view = &app.state.views[app.state.active_view().unwrap()];
        assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
        assert_eq!(view.cursor, ByteOffset(6));
        assert!(!app.ui.notification_error);
    }
}

#[test]
fn workspace_search_rejects_stale_open_and_closed_file_offsets() {
    for open in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("match.txt");
        fs::write(&path, "before needle\n").unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        let found = app.search.matches()[0].clone();
        app.close_picker();
        let (_, view) = if open {
            app.state
                .open_text(
                    TextBuffer::from_text("new prefix before needle\n"),
                    Some(path.clone()),
                )
                .unwrap()
        } else {
            fs::write(&path, "new prefix before needle\n").unwrap();
            app.state
                .open_text(TextBuffer::from_text("unchanged active view"), None)
                .unwrap()
        };
        let buffer = app.state.views[view].buffer;
        app.navigate_workspace_search_match(found);
        wait_vfs(&mut app);
        assert_eq!(app.state.active_buffer_id(), Some(buffer));
        assert_eq!(app.state.views[view].cursor, ByteOffset(0));
        assert!(app.ui.notification_error);
    }
}

#[test]
fn workspace_search_streams_exact_batches_and_root_changes_cancel() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("one.txt"), "needle\n").unwrap();
    fs::write(app.state.workspace_root.join("two.txt"), "needle\n").unwrap();
    app.start_workspace_search("needle".into(), None);
    wait_workspace_search(&mut app);
    // Each file publishes its remaining matches before the next file is read.
    assert_eq!(app.search.sequence(), 2);
    let broker = app.search.workflow().unwrap();
    let snapshot = broker.snapshot_if_changed(0).unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.items.len(), 2);
    for _ in 0..256 {
        assert!(broker.snapshot_if_changed(snapshot.revision).is_none())
    }
    app.close_picker();
    app.start_workspace_search("needle".into(), None);
    let extra = tempdir().unwrap();
    app.workspace
        .fixture_add_root(
            "extra".into(),
            crate::files::RootIdentity::capture(extra.path()).unwrap(),
        )
        .unwrap();
    app.process_workspace_search_events();
    assert!(!app.search.pending());
    assert!(app.search.workflow().is_none());
    assert!(app.search.matches().is_empty());
}

#[test]
fn workspace_replacement_groups_many_matches_from_shared_snapshots() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("many.txt");
    let before = "needle\n".repeat(3000);
    fs::write(&path, &before).unwrap();
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    assert_eq!(app.search.matches().len(), 3000);
    assert_eq!(app.workspace_edits.proposal().unwrap().files.len(), 1);
    app.accept_workspace_proposal();
    wait_workspace_approval(&mut app);
    let buffer = app
        .state
        .buffers
        .values()
        .find(|buffer| buffer.path.as_ref() == Some(&path))
        .unwrap();
    assert_eq!(buffer.text.to_string(), "changed\n".repeat(3000));
    assert_eq!(fs::read_to_string(path).unwrap(), before);
}

#[test]
fn workspace_replacement_rejects_open_buffer_that_differs_from_search_snapshot() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("stale.txt");
    fs::write(&path, "needle\n").expect("disk");
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("prefix needle\n"),
            Some(path),
        )
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    assert!(app.workspace_edits.proposal().is_none());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "prefix needle\n"
    );
    assert!(app.ui.notification_error);
}

#[test]
fn truncated_workspace_search_does_not_offer_partial_replacement() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("many.txt");
    fs::write(&path, "needle\n").unwrap();
    app.process_effect(AppEffect::WorkspaceReplace {
        query: "needle".into(),
        replacement: "changed".into(),
    });
    app.search.fixture_search_completion(
        vec![crate::files::WorkspaceSearchMatch {
            id: "match".into(),
            root: app.state.workspace_root.clone(),
            canonical_path: path,
            relative_path: "many.txt".into(),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 6,
            range: 0..6,
            line_text: "needle".into(),
            snapshot: std::sync::Arc::from(b"needle\n".as_slice()),
        }],
        true,
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.search.pending() {
        app.process_workspace_search_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.workspace_edits.proposal().is_none());
    assert!(app.ui.notification_error);
    assert!(
        app.ui.notification.as_ref().unwrap().contains("truncated"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn workspace_search_rejects_changed_disk_and_open_buffer_snapshots() {
    for open in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("match.txt");
        fs::write(&path, "needle\n").unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        assert_eq!(app.search.matches().len(), 1);
        if open {
            app.state
                .open_text(TextBuffer::from_text("prefix needle\n"), Some(path.clone()))
                .unwrap();
        } else {
            fs::write(&path, "prefix needle\n").unwrap();
        }
        let before = app.state.active_buffer_id();
        app.accept_picker();
        wait_vfs(&mut app);
        assert_eq!(app.state.active_buffer_id(), before);
        assert!(app.ui.notification_error);
        assert_eq!(
            app.ui.notification.as_deref(),
            Some(app.message("workspace_search_stale_result", &[]).as_str())
        );
        assert_eq!(app.search.matches().len(), 1);
        assert!(app.search.workflow().is_some());
        app.invoke_mapped_command("workspace.search.resume");
        app.accept_picker();
        wait_vfs(&mut app);
        assert_eq!(app.state.active_buffer_id(), before);
        assert_eq!(
            app.ui.notification.as_deref(),
            Some(app.message("workspace_search_stale_result", &[]).as_str())
        );
    }
}

#[test]
fn workspace_search_revalidates_buffers_opened_while_navigation_is_pending() {
    for changed in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("match.txt");
        fs::write(&path, "head needle\n").unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        let before = app.state.active_buffer_id();
        app.accept_picker();
        let buffer = app
            .state
            .load_hidden_text(
                TextBuffer::from_text(if changed {
                    "prefix head needle\n"
                } else {
                    "head needle\n"
                }),
                path,
            )
            .unwrap();
        wait_vfs(&mut app);
        if changed {
            assert_eq!(app.state.active_buffer_id(), before);
            assert!(app.ui.notification_error);
            assert_eq!(
                app.ui.notification.as_deref(),
                Some(app.message("workspace_search_stale_result", &[]).as_str())
            );
        } else {
            assert_eq!(app.state.active_buffer_id(), Some(buffer));
            assert_eq!(
                app.state.views[app.state.active_view().unwrap()].cursor,
                ByteOffset(5)
            );
        }
    }
}

#[test]
fn workspace_search_accepts_unchanged_encoded_snapshots() {
    for open in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("match.txt");
        let bytes = b"\xef\xbb\xbfhead\r\nneedle\r\n";
        fs::write(&path, bytes).unwrap();
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        if open {
            app.state
                .open_text(TextBuffer::from_bytes(bytes).unwrap(), Some(path.clone()))
                .unwrap();
        }
        app.accept_picker();
        wait_vfs(&mut app);
        let buffer = app.state.active_buffer_id().unwrap();
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
        assert_eq!(
            app.state.views[app.state.active_view().unwrap()].cursor,
            ByteOffset(5)
        );
    }
}

#[test]
fn workspace_search_presentation_limit_keeps_matches_and_blocks_replacement() {
    for replacement in [None, Some("changed".into())] {
        let replacing = replacement.is_some();
        let (_app_dirs, mut app) = app();
        fs::write(
            app.state.workspace_root.join("many.txt"),
            "needle".repeat(2500),
        )
        .unwrap();
        app.start_workspace_search("needle".into(), replacement);
        wait_workspace_search(&mut app);
        assert!(!app.search.matches().is_empty());
        assert!(app.search.matches().len() < 2500);
        let snapshot = app
            .search
            .workflow()
            .unwrap()
            .snapshot_if_changed(0)
            .unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.items.len(), app.search.matches().len());
        assert!(app.workspace_edits.proposal().is_none());
        assert!(app.ui.notification.as_ref().unwrap().contains("truncated"));
        if !replacing {
            assert!(app.ui.picker.is_some());
            app.accept_picker();
            wait_vfs(&mut app);
            assert!(
                app.state.buffers[app.state.active_buffer_id().unwrap()]
                    .path
                    .as_ref()
                    .unwrap()
                    .ends_with("many.txt")
            );
        }
    }
}

#[test]
fn remote_files_outside_the_workspace_open_writable_at_the_requested_line() {
    let (_app_dirs, mut app) = app();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let server = crate::remote::RemoteServer::start(directory.path()).unwrap();
    crate::remote::send(server.path(), std::slice::from_ref(&path), Some(2)).unwrap();
    app.remote = Some(server);
    for _ in 0..200 {
        app.process_remote_opens();
        app.process_vfs_results();
        if app
            .state
            .active_buffer_id()
            .is_some_and(|buffer| app.state.buffers[buffer].path.as_ref() == Some(&path))
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let view = &app.state.views[app.state.active_view().unwrap()];
    assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
    assert!(!app.state.buffers[view.buffer].read_only);
    assert_eq!(view.cursor.get(), 4);
}

use crate::app::workspace::*;
pub(in crate::app) fn fixture() -> (Workspace, tempfile::TempDir) {
    let state = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [
            (
                "one".into(),
                RootIdentity {
                    canonical_path: crate::files::absolute_test_path("/one"),
                    filesystem_identity: Some((1, 1)),
                },
            ),
            (
                "two".into(),
                RootIdentity {
                    canonical_path: crate::files::absolute_test_path("/two"),
                    filesystem_identity: Some((1, 2)),
                },
            ),
        ],
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut owner = Workspace::new(
        state,
        directory.path().join("trust.toml"),
        4,
        false,
        false,
        4,
    );
    owner.database_ready = true;
    owner.identity_ready = true;
    for root in owner.state.roots() {
        owner.store.trust(root.identity.clone());
        owner.store.mark_seen(root.identity.clone());
    }
    owner.persisted_identities = owner.store.identities();
    owner.reconcile_trust();
    (owner, directory)
}
#[test]
fn removing_a_root_preserves_trust_of_remaining_roots() {
    let (mut owner, _directory) = fixture();
    owner.remove_root(RootId(2)).unwrap();
    assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
    assert!(owner.execution_allowed().is_ok());
}

#[test]
fn workspace_root_removal_reconciles_remaining_trust() {
    let (mut owner, _directory) = fixture();
    owner.remove_root(RootId(2)).unwrap();
    assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
    assert!(owner.execution_allowed().is_ok());
}

#[test]
fn workspace_root_addition_reconciles_previously_saved_trust() {
    let (mut owner, _directory) = fixture();
    let identity = RootIdentity {
        canonical_path: crate::files::absolute_test_path("/three"),
        filesystem_identity: Some((1, 3)),
    };
    owner.store.trust(identity.clone());
    owner.persisted_identities.push(identity.clone());
    owner
        .add_root_request(identity.canonical_path.clone(), None)
        .unwrap_or_else(|_| panic!("root request"));
    let request_id = owner.root_operation.as_ref().unwrap().request_id;
    let mut updates = Vec::new();
    owner.accept_event(
        TrustEvent::RootInspected {
            request_id,
            result: Ok(identity),
        },
        &mut updates,
        &EditorState::new("/".into()),
    );
    assert!(matches!(
        updates.as_slice(),
        [WorkspaceUpdate::RootAdded(_)]
    ));
    assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
    assert!(owner.execution_allowed().is_ok());
}

#[test]
fn workspace_root_replacement_at_capacity_is_atomic() {
    let (mut owner, _directory) = fixture();
    for index in 3..=64 {
        owner
            .state
            .add_root(
                format!("root-{index}"),
                RootIdentity {
                    canonical_path: format!("/root-{index}").into(),
                    filesystem_identity: Some((1, index)),
                },
            )
            .unwrap();
    }
    let identity = RootIdentity {
        canonical_path: crate::files::absolute_test_path("/replacement"),
        filesystem_identity: Some((1, 100)),
    };
    owner
        .add_root_request(identity.canonical_path.clone(), Some(RootId(1)))
        .unwrap_or_else(|_| panic!("root request"));
    let request_id = owner.root_operation.as_ref().unwrap().request_id;
    let mut updates = Vec::new();
    owner.accept_event(
        TrustEvent::RootInspected {
            request_id,
            result: Ok(identity.clone()),
        },
        &mut updates,
        &EditorState::new("/".into()),
    );
    assert!(
        matches!(updates.as_slice(), [WorkspaceUpdate::RootSwitched { path, old }]
            if path == &identity.canonical_path && *old == crate::files::absolute_test_path("/one"))
    );
    assert_eq!(owner.roots().len(), 64);
    assert!(owner.roots().iter().all(|root| root.id != RootId(1)));
    let new = owner.active_root().unwrap();
    assert_eq!(
        owner
            .roots()
            .iter()
            .find(|root| root.id == new)
            .unwrap()
            .identity,
        identity
    );

    let roots = owner.roots().to_vec();
    let version = owner.version();
    let revision = owner.root_revision();
    assert!(
        owner
            .switch_root(new, "duplicate".into(), roots[0].identity.clone())
            .is_err()
    );
    assert_eq!(owner.roots(), roots);
    assert_eq!(owner.active_root(), Some(new));
    assert_eq!(owner.version(), version);
    assert_eq!(owner.root_revision(), revision);
}

#[test]
fn workspace_trust_persistence_survives_active_root_changes() {
    for fail in [false, true] {
        let (mut owner, directory) = fixture();
        owner.store.replace(Vec::new());
        owner.persisted_identities.clear();
        owner.reconcile_trust();
        if fail {
            owner.state_path = directory.path().to_path_buf();
        }
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.confirm().unwrap_or_else(|_| panic!("save trust"));
        owner.set_active_root(RootId(2)).unwrap();
        assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
        assert!(owner.execution_allowed().is_err());

        let mut updates = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while owner.persist_pending() {
            updates.extend(owner.poll(&EditorState::new("/".into())));
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        if fail {
            assert!(matches!(
                updates.as_slice(),
                [WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                    "workspace_trust_state_failed",
                    _
                ))]
            ));
            assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
            assert!(owner.store.identities().is_empty());
            owner.reconcile_trust();
            assert!(owner.execution_allowed().is_err());
        } else {
            assert!(updates.is_empty(), "no stale trust confirmation message");
            assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
            assert!(owner.execution_allowed().is_ok());
        }
    }
}

#[test]
fn revoking_trust_cancels_queued_probes_permanently() {
    let (mut owner, _directory) = fixture();
    let cancelled = owner.probe_cancellation().expect("trusted");
    assert!(!cancelled.load(Ordering::Acquire));
    assert!(owner.revoke().is_ok());
    assert!(cancelled.load(Ordering::Acquire));
    assert!(owner.probe_cancellation().is_none());
    for root in owner.state.roots() {
        owner.store.trust(root.identity.clone());
    }
    owner.persisted_identities = owner.store.identities();
    owner.reconcile_trust();
    assert!(!owner.probe_cancellation().unwrap().load(Ordering::Acquire));
    assert!(cancelled.load(Ordering::Acquire));
    owner.safe_mode = true;
    owner.reconcile_trust();
    assert!(owner.probe_cancellation().is_none());
}
#[test]
fn failed_trust_persistence_cancels_queued_probes() {
    let (mut owner, _directory) = fixture();
    let cancelled = owner.probe_cancellation().expect("trusted");
    owner.persist = Some(Persist {
        request_id: 7,
        source: owner.version(),
        target: None,
        identities: owner.store.identities(),
    });
    owner.accept_event(
        TrustEvent::Persisted {
            request_id: 7,
            result: Err(crate::workspace::TrustStateError::UnsupportedVersion(99)),
        },
        &mut Vec::new(),
        &EditorState::new("/".into()),
    );
    assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(owner.probe_cancellation().is_none());
}
#[test]
fn cancelled_root_request_ignores_its_late_inspection() {
    let (mut owner, _directory) = fixture();
    let path = PathBuf::from("/practice");
    owner.root_operation = Some(RootOperation {
        request_id: 99,
        source: owner.version(),
        requested_path: path.clone(),
        replaces: owner.active_root(),
    });
    let active = owner.active_root();
    owner.cancel_root_request(Path::new("/other"));
    assert!(owner.root_operation_pending());
    owner.cancel_root_request(&path);
    assert!(!owner.root_operation_pending());
    let mut updates = Vec::new();
    owner.accept_event(
        TrustEvent::RootInspected {
            request_id: 99,
            result: Ok(RootIdentity {
                canonical_path: path,
                filesystem_identity: Some((1, 3)),
            }),
        },
        &mut updates,
        &EditorState::new("/".into()),
    );
    assert!(updates.is_empty());
    assert_eq!(owner.active_root(), active);
    assert_eq!(owner.roots().len(), 2);
}

#[test]
fn failed_trust_load_cannot_replace_the_original_database() {
    let (mut owner, _directory) = fixture();
    std::fs::write(&owner.state_path, "broken database").unwrap();
    owner.load_request_id = Some(7);
    let mut updates = Vec::new();
    owner.accept_event(
        TrustEvent::Loaded {
            request_id: 7,
            result: Err(crate::workspace::TrustStateError::UnsupportedVersion(99)),
        },
        &mut updates,
        &EditorState::new("/".into()),
    );
    assert!(!owner.database_ready);
    assert!(owner.persist_state(None).is_err());
    assert_eq!(
        std::fs::read_to_string(&owner.state_path).unwrap(),
        "broken database"
    );
}
#[test]
fn revoking_trust_does_not_cancel_initial_root_inspection() {
    let (mut owner, _directory) = fixture();
    let root = owner.state.roots()[0].clone();
    owner.identity_ready = false;
    owner.identity_request_id = Some((7, root.id, owner.version()));
    assert!(owner.revoke().is_ok());
    owner.accept_event(
        TrustEvent::RootInspected {
            request_id: 7,
            result: Ok(root.identity),
        },
        &mut Vec::new(),
        &EditorState::new("/".into()),
    );
    assert!(owner.identity_ready);
}
#[test]
fn confirmation_is_bound_to_identity_root_selection_cancellation_and_safe_mode() {
    let (mut owner, _directory) = fixture();
    owner
        .request_confirmation()
        .unwrap_or_else(|_| panic!("prompt"));
    owner.cancel_confirmation();
    assert!(owner.confirm().is_err());
    owner
        .request_confirmation()
        .unwrap_or_else(|_| panic!("prompt"));
    owner.set_active_root(RootId(2)).unwrap();
    assert!(owner.confirm().is_err());
    owner
        .request_confirmation()
        .unwrap_or_else(|_| panic!("prompt"));
    let mut replacement = owner.roots()[0].identity.clone();
    replacement.filesystem_identity = Some((1, 99));
    owner.fixture_identity(RootId(1), replacement).unwrap();
    assert!(owner.confirm().is_err());
    owner
        .request_confirmation()
        .unwrap_or_else(|_| panic!("prompt"));
    owner.fixture_safe_mode(true);
    assert!(owner.confirm().is_err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while owner.persist.is_some() {
        owner.poll(&EditorState::new("/".into()));
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
