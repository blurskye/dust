//! Git behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::documents::tests::history_fixture;
use crate::app::documents::tests::index_bytes;
use crate::app::documents::tests::near_changes_fixture;
use crate::app::explorer::tests::wait_explorer;
use crate::app::startup::StartupTarget;
use crate::app::tests::*;
use crate::app::workspace::tests::wait_workspace_search;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::ui::ExplorerSegmentRole;
use crate::ui::UiEvent;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn explorer_auto_git_status_is_silent_outside_a_repository() {
    let (_app_dirs, mut app) = app();
    // An invalid gitfile stops discovery before any repository above the fixture.
    fs::write(
        app.state.workspace_root.join(".git"),
        "gitdir: missing-repository\n",
    )
    .unwrap();
    assert!(git2::Repository::discover(&app.state.workspace_root).is_err());
    let path = app.state.workspace_root.join("plain.txt");
    fs::write(&path, "plain\n").unwrap();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("plain\n"),
            Some(path),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().is_none());
    assert!(!app.ui.notification_error);
    let request_id = app.git.status_request_id();
    for _ in 0..256 {
        app.reconcile_git_signs();
        app.process_git_status_events();
        assert_eq!(app.git.status_request_id(), request_id);
    }
}

#[test]
fn active_tracked_buffer_starts_git_status_and_updates_unsaved_signs_without_explorer() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("live.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("live.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\ntwo\nthree\n"),
            Some(path),
        )
        .unwrap();
    app.set_dashboard_visibility(false);

    app.reconcile_git_signs();
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().is_some());
    assert!(app.ui.explorer.is_none());
    app.handle_configured_key(KeyEvent::character('d'));
    app.handle_configured_key(KeyEvent::character('d'));
    assert!(app.state.buffers[buffer].dirty);
    let revision = app.state.buffers[buffer].text.revision().0;
    app.reconcile_git_signs();
    assert!(app.git.signs_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.ui.git_signs.iter().any(|sign| {
        sign.buffer == buffer
            && sign.buffer_revision == revision
            && sign.kind == crate::ui::UiGitSignKind::Deleted
    }));

    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() || app.git.signs_pending() {
        app.process_git_status_events();
        app.reconcile_git_signs();
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.ui.explorer.is_none());
    assert!(!app.state.buffers[buffer].dirty);
    assert!(app.ui.git_signs.iter().any(|sign| sign.buffer == buffer));

    let path = app.state.buffers[buffer].path.clone().unwrap();
    fs::write(path, "one\ntwo\nthree\n").unwrap();
    app.start_reload(false);
    wait_vfs(&mut app);
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().unwrap().entries.is_empty());
}

#[test]
fn the_first_hunk_key_reads_the_hunks_and_moves_to_the_next_one() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("hunks.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("hunks.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "one\ntwo changed\nthree\nfour\nfive\n").unwrap();
    app.start_open(path);
    wait_vfs(&mut app);
    app.start_git_status_refresh_silent();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }

    app.process_effect(AppEffect::GitHunkNavigate(1));
    assert!(app.git.hunks_pending(), "{:?}", app.ui.notification);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(&mut app);
    assert!(app.git.has_selection(), "{:?}", app.ui.notification);
    let view = &app.state.views[app.state.active_view().expect("view")];
    assert_eq!(
        app.state.buffers[view.buffer]
            .text
            .byte_to_line(view.cursor)
            .expect("line"),
        1,
        "{:?}",
        app.ui.notification
    );

    app.process_effect(AppEffect::GitHunkPreview);
    assert_eq!(
        app.ui.documentation_popup.as_ref().map(|popup| popup.kind),
        Some(crate::ui::DocumentationPopupKind::Git)
    );
}

pub(in crate::app) fn settle_git_work(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_status_events();
        app.process_git_hunk_events();
        app.process_git_mutation_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wait_vfs(app);
}

#[test]
fn hunk_preview_and_stage_take_the_change_under_the_cursor() {
    let (_app_dirs, mut app, repo, _) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);

    app.process_effect(AppEffect::GitHunkPreview);
    let popup = app.ui.documentation_popup.as_ref().expect("preview");
    let shown = popup.sections[0].lines.join("|");
    assert!(shown.contains("FOUR") && !shown.contains("TWO"), "{shown}");

    app.process_effect(AppEffect::GitHunkStage);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(blob.content()),
        "one\ntwo\nthree\nFOUR\nfive\n",
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn hunk_reset_takes_the_change_under_the_cursor() {
    let (_app_dirs, mut app, _repo, path) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);

    app.process_effect(AppEffect::GitHunkReset);
    settle_git_work(&mut app);
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    settle_git_work(&mut app);

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "one\ntwo\nthree\nFOUR\nfive\n",
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn leaving_a_worktree_closes_shared_buffers_without_alternating_fallbacks() {
    let (_app_dirs, mut app) = app();
    let old = app.state.workspace_root.clone();
    let new = old.join(".worktree/topic");
    fs::create_dir_all(&new).unwrap();
    let (a, _) = app
        .state
        .open_text(TextBuffer::from_text("a"), Some(old.join("a.txt")))
        .unwrap();
    let (b, first) = app
        .state
        .open_text(TextBuffer::from_text("b"), Some(old.join("b.txt")))
        .unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    app.state.replace_active_view(Some(a)).unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    app.state.replace_active_view(Some(b)).unwrap();
    assert_eq!(app.state.views[first].buffer, b);
    app.finish_root_switch(new, &old);
    assert!(!app.state.buffers.contains_key(a));
    assert!(!app.state.buffers.contains_key(b));
    assert!(
        !app.state
            .views
            .values()
            .any(|view| view.buffer == a || view.buffer == b)
    );
}

#[test]
fn leaving_a_worktree_closes_every_clean_buffer_from_it() {
    let (_app_dirs, mut app) = app();
    let old = app.state.workspace_root.clone();
    let new = old.join(".worktree/topic");
    fs::create_dir_all(&new).unwrap();
    for name in ["first.txt", "second.txt"] {
        fs::write(old.join(name), "text\n").unwrap();
        app.start_open(old.join(name));
        wait_vfs(&mut app);
    }
    assert_eq!(app.state.buffers.len(), 2);

    app.finish_root_switch(new, &old);
    wait_vfs(&mut app);

    assert!(!app.state.buffers.values().any(|buffer| {
        buffer
            .path
            .as_deref()
            .is_some_and(|path| path.starts_with(&old) && !path.starts_with(old.join(".worktree")))
    }),);
}

#[test]
fn explorer_auto_git_status_colors_names_opens_picker_and_idles() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("tracked.txt");
    fs::write(&path, "normal\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "normal\nadded\n").unwrap();
    app.open_explorer();
    assert!(app.git.status_pending());
    assert!(app.git.silent());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let revision = app.git.status().unwrap().revision;
    assert_eq!(
        app.git.decorations().get(&path),
        Some(&crate::git::GitFileStatus::Modified)
    );
    assert_eq!(
        app.git.decorations().get(&app.state.workspace_root),
        Some(&crate::git::GitFileStatus::Modified)
    );
    wait_explorer(&mut app);
    let tracked_row = app
        .ui
        .explorer
        .as_ref()
        .and_then(|explorer| {
            explorer.rows.iter().find(|row| {
                row.segments
                    .iter()
                    .any(|segment| segment.text == "tracked.txt")
            })
        })
        .expect("tracked Git row");
    assert!(
        tracked_row
            .segments
            .iter()
            .any(|segment| segment.role == ExplorerSegmentRole::Git)
    );
    assert!(
        tracked_row
            .right_segments
            .iter()
            .any(|segment| segment.text == "M")
    );
    assert!(!app.git.silent());
    let request_id = app.git.status_request_id();
    for _ in 0..256 {
        app.process_git_status_events();
        assert_eq!(app.git.status_request_id(), request_id);
        assert_eq!(app.git.status().unwrap().revision, revision)
    }
    app.open_git_status_picker();
    let title = app
        .ui
        .picker
        .as_ref()
        .map(|p| p.title.clone())
        .expect("title");
    let branch = crate::git::git_branch_name(&app.state.workspace_root)
        .expect("a branch read")
        .expect("the fixture is on a branch");
    assert_eq!(
        title,
        format!("Changes · {branch} ±1"),
        "the surface titles itself with the branch and the change count the document draws"
    );
    // Accepting the only entry opens it; a section header is not an entry.
    assert!(
        app.ui
            .picker
            .as_ref()
            .is_some_and(|picker| picker.selected.is_some_and(|selected| {
                picker.rows[selected].kind == crate::ui::PickerRowKind::Entry
            })),
        "the initial selection is an entry, never a section header"
    );

    app.accept_picker();
    wait_vfs(&mut app);
    assert_eq!(
        app.state
            .active_buffer_id()
            .and_then(|id| app.state.buffers[id].path.as_ref()),
        Some(&path)
    );
    app.reconcile_git_signs();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(!app.ui.git_signs.is_empty());
    app.window_session.observe_size((100, 30));
    app.reconcile_editor_windows();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    let added_cells = (0..frame.grid.height())
        .flat_map(|y| (0..frame.grid.width()).map(move |x| (x, y)))
        .filter(|(x, y)| {
            frame
                .grid
                .get(*x, *y)
                .is_some_and(|cell| cell.style == crate::ui::Style::GitSignAdded)
        })
        .collect::<Vec<_>>();
    assert_eq!(added_cells.len(), 1);
    let (sign_x, sign_y) = added_cells[0];
    assert_eq!(frame.grid.get(sign_x, sign_y).unwrap().symbol, "┃");
    assert!(sign_y > 0);
    let normal_sign_cell = frame.grid.get(sign_x, sign_y - 1).unwrap();
    assert_eq!(normal_sign_cell.symbol, "│");
    assert_eq!(normal_sign_cell.style, crate::ui::Style::Muted);
    let sign_request_id = app.git.sign_request_id();
    let signs = app.ui.git_signs.clone();
    for _ in 0..256 {
        app.reconcile_git_signs();
        app.process_git_sign_events();
        let _ = crate::ui::render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            100,
            30,
        );
        assert_eq!(app.git.sign_request_id(), sign_request_id);
        assert_eq!(app.ui.git_signs, signs);
    }
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(
        app.ui.picker.as_ref().map(|p| p.title.as_str()),
        Some("Git hunks")
    );
    app.accept_picker();
    assert!(app.git.has_selection());
    app.preview_selected_git_hunk();
    assert_eq!(
        app.ui.documentation_popup.as_ref().map(|popup| popup.kind),
        Some(crate::ui::DocumentationPopupKind::Git)
    );
    app.navigate_git_hunk(1);
    wait_vfs(&mut app);
    assert!(app.git.has_selection());
    app.stage_selected_git_hunk();
    assert!(!app.git.mutation_pending());
    assert!(app.ui.notification_error);

    app.set_explorer_focus(false);
    // The hunk key landed on the added line. Deleting all of it would match the index again.
    app.handle_configured_key(KeyEvent::character('x'));
    let active = app.state.active_buffer_id().unwrap();
    assert!(app.state.buffers[active].dirty);
    let dirty_revision = app.state.buffers[active].text.revision().0;
    app.reconcile_git_signs();
    // The anchored signs stay at their lines while the new diff is being read.
    assert_eq!(
        app.ui.git_signs,
        signs
            .iter()
            .map(|sign| crate::ui::UiGitSign {
                buffer_revision: dirty_revision,
                ..*sign
            })
            .collect::<Vec<_>>()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.signs_pending() {
        app.process_git_sign_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(!app.ui.git_signs.is_empty());
    assert!(
        app.ui
            .git_signs
            .iter()
            .all(|sign| { sign.buffer == active && sign.buffer_revision == dirty_revision })
    );
    let dirty_frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        30,
    );
    assert!((0..dirty_frame.grid.height()).any(|y| {
        (0..dirty_frame.grid.width()).any(|x| {
            dirty_frame.grid.get(x, y).is_some_and(|cell| {
                matches!(
                    cell.style,
                    crate::ui::Style::GitSignAdded
                        | crate::ui::Style::GitSignChanged
                        | crate::ui::Style::GitSignDeleted
                )
            })
        })
    }));
}

#[test]
fn trusted_stage_hunk_flow_changes_only_index_and_refreshes_status() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("two-hunks.txt");
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("two-hunks.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.hunks().unwrap().clone();
    assert_eq!(snapshot.hunks.len(), 2);
    assert!(!app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision,
        &path.with_extension("foreign"),
        snapshot.hunks[0].id
    ));
    assert!(!app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision + 1,
        &path,
        snapshot.hunks[0].id
    ));
    assert!(!app.git.has_selection());
    app.accept_picker();
    assert!(app.git.has_selection());
    assert!(
        app.git
            .mutate(&app.state, false, true, snapshot.root_revision + 1)
            .is_err()
    );
    assert!(!app.git.mutation_pending());
    let before = crate::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    app.stage_selected_git_hunk();
    assert!(app.git.mutation_pending());
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let after = crate::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    assert_ne!(before, after);
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    assert!(!app.git.has_selection());
    assert!(app.git.hunks().is_none());
    let status = app.git.status().cloned().unwrap();
    let mut policy = crate::git::GitHunkPolicy {
        scope_root: status.scope_root,
        repository_id: status.repository_id,
        status_revision: status.revision,
        root_revision: status.root_revision,
        file: "two-hunks.txt".into(),
        layer: crate::git::GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(2),
    };
    assert_eq!(crate::git::read_hunks(&policy).unwrap().hunks.len(), 1);
    policy.layer = crate::git::GitDiffLayer::Staged;
    assert_eq!(crate::git::read_hunks(&policy).unwrap().hunks.len(), 1);
    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.accept_picker();
    let index_before_reset = crate::git::git_index_fingerprint(&app.state.workspace_root).unwrap();
    let worktree_before_reset = fs::read_to_string(&path).unwrap();
    app.request_reset_selected_git_hunk();
    assert_eq!(
        app.ui.prompt.as_ref().map(|p| p.kind),
        Some(crate::ui::PromptKind::GitReset)
    );
    let prompt = app.ui.prompt.as_ref().expect("Git reset prompt");
    assert!(prompt.error.is_none());
    assert!(
        prompt
            .help
            .as_deref()
            .is_some_and(|help| help.contains("Discards worktree changes"))
    );
    app.close_prompt();
    assert_eq!(
        crate::git::git_index_fingerprint(&app.state.workspace_root).unwrap(),
        index_before_reset
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), worktree_before_reset);
    app.request_reset_selected_git_hunk();
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        crate::git::git_index_fingerprint(&app.state.workspace_root).unwrap(),
        index_before_reset
    );
    let reset_text = fs::read_to_string(&path).unwrap();
    assert!(reset_text.contains("changed 2\n") && !reset_text.contains("changed 18\n"));
    policy.layer = crate::git::GitDiffLayer::Unstaged;
    assert!(crate::git::read_hunks(&policy).unwrap().hunks.is_empty());
}

#[test]
fn workspace_replacement_previews_without_disk_mutation_and_checked_save_rejects_conflict() {
    let (_app_dirs, mut app) = app();
    let first = app.state.workspace_root.join("a.txt");
    let second = app.state.workspace_root.join("b.txt");
    fs::write(&first, "needle one\n").expect("first");
    fs::write(&second, "two needle\n").expect("second");
    app.start_workspace_search("needle".into(), Some("changed".into()));
    wait_workspace_search(&mut app);
    let proposal = app.workspace_edits.proposal().expect("preview");
    assert_eq!(proposal.files.len(), 2);
    assert_eq!(
        fs::read_to_string(&first).expect("unchanged"),
        "needle one\n"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("unchanged"),
        "two needle\n"
    );

    fs::write(&first, "external\n").expect("external change");
    app.accept_workspace_proposal();
    wait_workspace_approval(&mut app);
    assert_eq!(
        fs::read_to_string(&first).expect("still external"),
        "external\n"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("still source"),
        "two needle\n"
    );
    let (first_buffer, _) = app
        .state
        .buffers
        .iter()
        .find(|(_, buffer)| buffer.path.as_ref() == Some(&first))
        .expect("hidden first buffer");
    app.state.show_buffer(first_buffer).expect("show first");
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert_eq!(
        fs::read_to_string(&first).expect("conflict preserved"),
        "external\n"
    );
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("externally modified"))
    );
}

#[test]
fn git_change_source_labels_cover_every_combination() {
    use crate::git::GitChangeSource;
    use crate::git::GitChangeSources;
    let label = |staged, unstaged, unsaved: Option<u64>| {
        GitChangeSources {
            staged,
            unstaged,
            unsaved_document_revision: unsaved,
        }
        .source_label()
    };
    assert_eq!(label(true, false, None), "staged");
    assert_eq!(label(false, true, None), "unstaged");
    assert_eq!(label(false, false, Some(4)), "unsaved");
    assert_eq!(label(true, true, None), "staged+unstaged");
    assert_eq!(label(true, false, Some(4)), "staged+unsaved");
    assert_eq!(label(false, true, Some(4)), "unstaged+unsaved");
    assert_eq!(label(true, true, Some(4)), "staged+unstaged+unsaved");
    assert_eq!(label(false, false, None), "unchanged");
    assert_eq!(GitChangeSource::Staged.source_label(), "staged");
    assert_eq!(GitChangeSource::Unstaged.source_label(), "unstaged");
    assert_eq!(
        GitChangeSource::Unsaved {
            document_revision: 9
        }
        .source_label(),
        "unsaved"
    );
}

#[test]
fn git_status_rows_label_each_source_without_conflating_them() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let original = "one\n";
    for name in ["staged.txt", "unstaged.txt", "unsaved.txt", "both.txt"] {
        fs::write(app.state.workspace_root.join(name), original).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    {
        let mut index = repo.index().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
    }
    // staged: index differs from HEAD, worktree matches the index.
    fs::write(app.state.workspace_root.join("staged.txt"), "two\n").unwrap();
    // both: staged and then modified again.
    fs::write(app.state.workspace_root.join("both.txt"), "two\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("staged.txt")).unwrap();
        index.add_path(std::path::Path::new("both.txt")).unwrap();
        index.write().unwrap();
    }
    fs::write(app.state.workspace_root.join("both.txt"), "three\n").unwrap();
    // unstaged: index matches HEAD, worktree differs.
    fs::write(app.state.workspace_root.join("unstaged.txt"), "two\n").unwrap();
    // unsaved: nothing differs on disk; only the buffer holds a change.
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let projection = app.search.picker_projection().expect("status picker rows");
    let section = |title: &str| {
        let start = projection
            .rows
            .iter()
            .position(|row| {
                // A header carries its count, as the document draws it: `── Staged 2 ──`.
                row.kind == crate::ui::PickerRowKind::Section
                    && row
                        .label
                        .strip_prefix(title)
                        .is_some_and(|rest| rest.starts_with(' '))
            })
            .unwrap_or_else(|| panic!("no {title} section"));
        projection.rows[start + 1..]
            .iter()
            .take_while(|row| row.kind == crate::ui::PickerRowKind::Entry)
            .collect::<Vec<_>>()
    };
    let provenance_of = |title: &str, name: &str| {
        section(title)
            .into_iter()
            .find(|row| row.label == name)
            .unwrap_or_else(|| panic!("no {name} row in {title}"))
            .detail
            .clone()
    };
    // Every row names the comparison it belongs to, so an unstaged row is never read as
    // unsaved and a staged row is never read as unstaged.
    assert_eq!(provenance_of("Staged", "staged.txt"), "HEAD -> index");
    assert_eq!(provenance_of("Unstaged", "unstaged.txt"), "index -> disk");
    // A path that is both staged and further modified appears in BOTH sections, each row
    // naming its own comparison, rather than one row conflating them.
    assert_eq!(provenance_of("Staged", "both.txt"), "HEAD -> index");
    assert_eq!(provenance_of("Unstaged", "both.txt"), "index -> disk");
    // A staged path is not listed as unstaged.
    assert!(
        !section("Unstaged")
            .iter()
            .any(|row| row.label == "staged.txt")
    );
    assert!(
        !section("Staged")
            .iter()
            .any(|row| row.label == "unsaved.txt")
    );
    // A path with only unsaved edits is still listed, under its OWN provenance rather than
    // as an unstaged change, and carries the badge as its own drawn column.
    let unsaved = section("Unstaged")
        .into_iter()
        .find(|row| row.label == "unsaved.txt")
        .expect("a path with unsaved edits is still listed");
    assert_eq!(
        unsaved.detail, "buffer -> disk",
        "unsaved is not labelled unstaged"
    );
    assert!(
        unsaved
            .cells
            .iter()
            .any(|cell| cell.text.trim() == "unsaved"),
        "an unsaved path carries its badge column: {:?}",
        unsaved
            .cells
            .iter()
            .map(|cell| &cell.text)
            .collect::<Vec<_>>()
    );
    // Sections are drawn in the order the document names and are absent when empty.
    let titles: Vec<&str> = projection
        .rows
        .iter()
        .filter(|row| row.kind == crate::ui::PickerRowKind::Section)
        .map(|row| row.label.split(' ').next().unwrap_or_default())
        .collect();
    // Conflicts, Unstaged and Staged are drawn in this order and are absent when empty.
    let expected: Vec<&str> = ["Conflicts", "Unstaged", "Staged"]
        .into_iter()
        .filter(|title| titles.contains(title))
        .collect();
    assert_eq!(
        titles, expected,
        "sections appear in the drawn order, absent when empty"
    );
}

#[test]
fn staged_hunk_rows_are_labelled_staged_and_unstaged_rows_are_not() {
    fn last_segment(detail: &str) -> String {
        detail.rsplit(" · ").next().unwrap_or_default().to_owned()
    }
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("layers.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("layers.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let open_hunks = |app: &mut App, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let row_labels = |app: &App| {
        app.search
            .picker_projection()
            .expect("hunk picker rows")
            .rows
            .iter()
            .map(|row| last_segment(&row.detail))
            .collect::<Vec<_>>()
    };

    // Nothing is staged yet, so every row describes an unstaged change.
    open_hunks(&mut app, "workspace.git.hunks.open");
    let unstaged = row_labels(&app);
    assert_eq!(unstaged.len(), 2);
    assert!(
        unstaged.iter().all(|label| label == "unstaged"),
        "unstaged rows must not claim another source: {unstaged:?}"
    );

    // Stage the whole file, then the same rows describe the staged layer.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("command id"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    open_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = row_labels(&app);
    assert_eq!(staged.len(), 2);
    assert!(
        staged.iter().all(|label| label == "staged"),
        "staged rows must be labelled staged: {staged:?}"
    );
}

#[test]
fn hunk_unstage_returns_one_staged_hunk_to_head_and_refuses_the_rest() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("unstage.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("unstage.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let invoke = |app: &mut App, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let drain = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let load_hunks = |app: &mut App, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        invoke(app, id);
        while app.git.hunks_pending() {
            app.process_git_hunk_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    invoke(&mut app, "workspace.git.file.stage");
    drain(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        changed
    );

    // Unstage exactly the first staged hunk.
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = app.git.hunks().expect("staged hunks").clone();
    assert_eq!(staged.hunks.len(), 2);
    assert!(app.git.select(
        staged.status_revision,
        staged.repository_id,
        staged.root_revision,
        &path,
        staged.hunks[0].id
    ));
    invoke(&mut app, "workspace.git.hunk.unstage");
    drain(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        original.replace("line 38\n", "changed 38\n"),
        "only the selected staged hunk returned to HEAD"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        changed,
        "unstaging never writes the worktree"
    );
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    assert_eq!(
        app.git.hunks().expect("staged hunks").hunks.len(),
        1,
        "the other staged hunk stays staged"
    );

    // Unstaging where only an unstaged hunk is selected finds no staged hunk on the line,
    // refuses and writes nothing.
    let before = index_bytes(&app.state.workspace_root, "unstage.txt");
    load_hunks(&mut app, "workspace.git.hunks.open");
    let unstaged = app.git.hunks().expect("unstaged hunks").clone();
    assert_eq!(unstaged.hunks.len(), 1);
    assert!(app.git.select(
        unstaged.status_revision,
        unstaged.repository_id,
        unstaged.root_revision,
        &path,
        unstaged.hunks[0].id
    ));
    load_hunks(&mut app, "workspace.git.hunk.unstage");
    assert!(!app.git.mutation_pending(), "no plan may start");
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("no staged Git hunk on this line")
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        before
    );

    // Staging a hunk that is already staged is refused, not reapplied.
    load_hunks(&mut app, "workspace.git.hunks.open_staged");
    let staged = app.git.hunks().expect("staged hunks").clone();
    assert!(app.git.select(
        staged.status_revision,
        staged.repository_id,
        staged.root_revision,
        &path,
        staged.hunks[0].id
    ));
    invoke(&mut app, "workspace.git.hunk.stage");
    assert!(!app.git.mutation_pending(), "no plan may start");
    assert_eq!(
        index_bytes(&app.state.workspace_root, "unstage.txt"),
        before
    );
}

#[test]
fn the_unstage_key_undoes_the_hunk_just_staged_under_the_cursor() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("undo.txt");
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("undo.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let settle = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_hunk_events();
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    let indexed = |app: &App| {
        let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new("undo.txt"), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    };
    let invoke = |app: &mut App, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    app.start_git_status_refresh();
    settle(&mut app);

    invoke(&mut app, "workspace.git.hunk.stage");
    settle(&mut app);
    assert_eq!(indexed(&app), original.replace("line 2\n", "changed 2\n"));

    invoke(&mut app, "workspace.git.hunk.unstage");
    settle(&mut app);
    assert_eq!(indexed(&app), original, "{:?}", app.ui.notification);
}

#[test]
fn a_buffer_stays_bound_to_the_worktree_it_was_opened_in() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    // Two linked worktrees, both OUTSIDE the main worktree so the main worktree stays clean.
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let mut roots = Vec::new();
    for name in ["alpha", "beta"] {
        let branch = repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        let path = outside.join(name);
        repo.worktree(name, &path, Some(&options)).unwrap();
        fs::write(path.join("note.txt"), name).unwrap();
        roots.push(path);
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    for (name, root) in ["alpha", "beta"].into_iter().zip(&roots) {
        app.workspace
            .fixture_add_root(
                name.into(),
                crate::files::RootIdentity::capture(root).unwrap(),
            )
            .unwrap();
    }
    let open_note = |app: &mut App, root: &std::path::Path| {
        app.open_explorer();
        app.explorer.select(
            app.explorer
                .tree()
                .id_for_path(root)
                .expect("worktree root"),
        );
        app.explorer_toggle();
        wait_explorer(app);
        let path = root.join("note.txt");
        app.explorer.select(
            app.explorer
                .tree()
                .id_for_path(&path)
                .expect("note in explorer"),
        );
        app.explorer_open_file(false);
        wait_vfs(app);
        let buffer = app.state.active_buffer_id().expect("opened note buffer");
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
        buffer
    };

    let alpha_file = roots[0].join("note.txt");
    let beta_file = roots[1].join("note.txt");
    let alpha_buffer = open_note(&mut app, &roots[0]);
    assert_eq!(app.state.buffers[alpha_buffer].text.to_string(), "alpha");
    // Documents are bound by their stored paths, not a separate Git identity cache.
    let alpha = app.state.buffers[alpha_buffer].path.clone().unwrap();
    assert_eq!(
        alpha.parent().unwrap(),
        std::fs::canonicalize(&roots[0]).unwrap(),
        "the buffer path names the worktree the document was opened in"
    );

    let beta_buffer = open_note(&mut app, &roots[1]);
    assert_ne!(alpha_buffer, beta_buffer);
    assert_eq!(app.state.buffers[beta_buffer].text.to_string(), "beta");
    assert_eq!(app.state.buffers[alpha_buffer].text.to_string(), "alpha");
    assert_eq!(
        app.state.buffers[alpha_buffer].path.as_ref(),
        Some(&alpha_file)
    );
    let beta = app.state.buffers[beta_buffer].path.clone().unwrap();
    assert_eq!(beta, beta_file);
    assert_eq!(
        beta.parent().unwrap(),
        std::fs::canonicalize(&roots[1]).unwrap()
    );
    assert_ne!(
        alpha, beta,
        "two linked worktrees carry distinct buffer paths"
    );
    assert_eq!(
        app.state.buffers[alpha_buffer].path.as_ref(),
        Some(&alpha),
        "opening another worktree never retargets an existing buffer"
    );

    assert!(
        app.state.buffers.values().all(|buffer| {
            buffer.path.as_deref() != Some(std::path::Path::new("/nonexistent-dust/orphan.txt"))
        }),
        "a nonexistent, unopened document has no buffer binding"
    );
    // A file under a repository has no buffer binding until this session opens it.
    let unopened = app.state.workspace_root.join("never-opened.txt");
    fs::write(&unopened, "x").unwrap();
    assert!(
        app.state
            .buffers
            .values()
            .all(|buffer| buffer.path.as_ref() != Some(&unopened)),
        "an unopened document has no buffer binding"
    );
}

#[test]
fn switching_to_a_worktree_moves_the_editor_there() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let worktree = outside.join("scoped");
    {
        let branch = repo.branch("scoped", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("scoped", &worktree, Some(&options)).unwrap();
    }
    fs::write(worktree.join("note.txt"), "scoped").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let main_root = std::fs::canonicalize(&app.state.workspace_root).unwrap();
    let main_file = main_root.join("h0.txt");
    let (main_buffer, main_view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_bytes(b"x\n").expect("text"),
            Some(main_file.clone()),
        )
        .unwrap();
    app.state.focus_view(main_view).unwrap();

    let invoke = |app: &mut App, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let open_worktree_row = |app: &mut App| {
        // A trust reconciliation changes the workspace version and invalidates the previous status
        // snapshot, so the picker's own refresh has to be drained on every open.
        let deadline = Instant::now() + Duration::from_secs(10);
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() {
            app.process_git_branch_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        let projection = app
            .search
            .picker_projection()
            .unwrap_or_else(|| panic!("worktrees picker missing: {:?}", app.ui.notification));
        let index = projection
            .rows
            .iter()
            .position(|row| row.label.starts_with("scoped"))
            .unwrap_or_else(|| panic!("no scoped row in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };

    let switch = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(10);
        invoke(app, "workspace.git.worktree.open");
        while app.workspace.root_operation_pending() {
            app.process_trust_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Unsaved changes in the current root block the switch.
    app.state.buffers[main_buffer].dirty = true;
    open_worktree_row(&mut app);
    switch(&mut app);
    assert!(
        app.ui.notification_error,
        "unsaved changes block the switch"
    );
    assert_eq!(
        app.workspace.roots()[0].identity.canonical_path,
        main_root,
        "a blocked switch leaves the root alone"
    );
    app.state.buffers[main_buffer].dirty = false;

    // The worktree replaces the root, and clean buffers from the old root close.
    open_worktree_row(&mut app);
    switch(&mut app);
    let canonical = std::fs::canonicalize(&worktree).unwrap();
    assert_eq!(app.workspace.roots().len(), 1, "a switch replaces the root");
    assert_eq!(app.workspace.roots()[0].identity.canonical_path, canonical);
    assert_eq!(app.state.workspace_root, canonical);
    assert!(
        !app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_deref() == Some(main_file.as_path())),
        "clean buffers from the old root close"
    );
    wait_vfs(&mut app);
    let reopened = canonical.join("h0.txt");
    assert!(
        app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_deref() == Some(reopened.as_path())),
        "the file in front reopens in the new worktree"
    );
    assert!(
        worktree.join("note.txt").is_file() && main_file.is_file(),
        "switching never touches either checkout"
    );
}

#[test]
fn primary_worktree_and_linked_main_have_distinct_picker_entries() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let mut branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    if branch == "main" {
        repo.find_branch("main", git2::BranchType::Local)
            .unwrap()
            .rename("trunk", false)
            .unwrap();
        branch = "trunk".into();
        repo.set_head("refs/heads/trunk").unwrap();
    }
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let linked = outside.path().join("main");
    let reference = repo.branch("side", &head, false).unwrap().into_reference();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repo.worktree("main", &linked, Some(&options)).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.start_git_worktrees();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let rows = app.search.picker_projection().unwrap().rows;
    assert_eq!(rows.iter().filter(|row| row.label == "main").count(), 2);
    let primary = rows
        .iter()
        .position(|row| row.id == "git-worktree:primary")
        .unwrap();
    assert!(rows.iter().any(|row| row.id == "git-worktree:linked:main"));
    for _ in 0..primary {
        app.invoke(
            crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test).unwrap(),
        );
    }
    app.request_git_worktree_remove();
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .unwrap_or_default()
            .contains("primary")
    );
    assert!(app.git.take_pending_destructive().is_none());
    assert!(!app.git.branch_pending());
    assert!(linked.join("h0.txt").is_file());
    assert!(repo.find_worktree("main").is_ok());
    assert_eq!(repo.head().unwrap().shorthand(), Some(branch.as_str()));
}

#[test]
fn removing_the_main_checkout_cannot_remove_a_linked_worktree_named_main() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let outside = tempdir().unwrap();
    let linked = outside.path().join("linked");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = repo
        .branch("linked-main", &head, false)
        .unwrap()
        .into_reference();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&branch));
    repo.worktree("main", &linked, Some(&options)).unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.start_git_worktrees();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let rows = app.search.picker_projection().unwrap().rows;
    assert_eq!(rows.iter().filter(|row| row.label == "main").count(), 2);
    let index = rows
        .iter()
        .position(|row| row.detail.contains(" · main"))
        .unwrap();
    for _ in 0..index {
        app.invoke(
            crate::editor::CommandInvocation::new("picker.next", InvocationSource::Test).unwrap(),
        );
    }
    app.request_git_worktree_remove();
    assert!(app.input.prompt().is_none());
    assert!(app.git.take_pending_destructive().is_none());
    assert!(
        app.ui
            .notification
            .as_ref()
            .unwrap()
            .contains("main checkout")
    );
    assert!(linked.is_dir());
    assert!(repo.find_worktree("main").is_ok());
}

#[test]
fn a_locked_worktree_refuses_removal() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let outside_guard = tempfile::tempdir().unwrap();
    let outside = outside_guard.path();
    let worktree = outside.join("locked");
    {
        let branch = repo.branch("locked", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("locked", &worktree, Some(&options)).unwrap();
    }
    let locked = repo.find_worktree("locked").unwrap();
    locked.lock(Some("held by another tool")).unwrap();
    drop(locked);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let invoke = |app: &mut App, id: &str| {
        app.invoke(
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id"),
        );
    };
    let select_worktree_row = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(10);
        app.start_git_status_refresh();
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        invoke(app, "workspace.git.worktrees.open");
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() {
            app.process_git_branch_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        let projection = app.search.picker_projection().expect("worktrees picker");
        let index = projection
            .rows
            .iter()
            .position(|row| row.label.starts_with("locked"))
            .unwrap_or_else(|| panic!("no locked row in {:?}", projection.rows));
        for _ in 0..index {
            invoke(app, "picker.next");
        }
    };
    let drain = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git.branch_pending() || app.git.status_pending() {
            app.process_git_branch_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Removal is refused through the command and names the lock.
    select_worktree_row(&mut app);
    invoke(&mut app, "workspace.git.worktree.remove");
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    drain(&mut app);
    assert!(
        app.ui.notification_error,
        "a locked worktree refuses removal through the command"
    );
    let notice = app.ui.notification.clone().unwrap_or_default();
    assert!(
        notice.contains("locked"),
        "the error names the lock: {notice}"
    );
    assert!(
        worktree.is_dir() && repo.find_worktree("locked").is_ok(),
        "a refused removal leaves the worktree and its registration alone"
    );
    assert!(repo.find_branch("locked", git2::BranchType::Local).is_ok());
}

#[test]
fn the_conflict_surface_shows_the_paused_operation_and_each_side() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let base_tip = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base_tip).unwrap();
    // A branch from the base that changes the same path differently, then a real merge.
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo
            .treebuilder(Some(&base_commit.tree().unwrap()))
            .unwrap();
        let blob = repo.blob(b"their version\n").unwrap();
        builder.insert("h0.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    // Ours changes the same file, so the merge conflicts.
    fs::write(root.join("h0.txt"), "our version\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("h0.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();
    let their_commit = repo.find_commit(their_tip).unwrap();
    let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
    repo.merge(&[&annotated], None, None).unwrap();
    drop(annotated);
    drop(their_commit);
    drop(base_commit);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflicts.open",
            InvocationSource::Test,
        )
        .expect("command id"),
    );
    let page = app
        .search
        .picker_projection()
        .expect("the conflict surface opened");
    // The operation bar names the operation and its phase.
    assert!(
        page.title.contains("merge") && page.title.contains("1 conflicted path"),
        "the surface names the paused operation and its phase: {:?}",
        page.title
    );
    let row = page
        .rows
        .iter()
        .find(|row| row.label == "h0.txt")
        .unwrap_or_else(|| panic!("the conflicted path is listed: {:?}", page.rows));
    // Each side is present with a real OID, and the labels say which side is which.
    for label in ["base ", "ours ", "theirs "] {
        assert!(
            row.detail.contains(label),
            "the row names the {label:?} side: {:?}",
            row.detail
        );
    }
    let index = repo.index().unwrap();
    let ours = index.get_path(std::path::Path::new("h0.txt"), 2).unwrap();
    let theirs = index.get_path(std::path::Path::new("h0.txt"), 3).unwrap();
    let base = index.get_path(std::path::Path::new("h0.txt"), 1).unwrap();
    for side in [ours, theirs, base] {
        let short = &side.id.to_string()[..8];
        assert!(
            row.detail.contains(short),
            "the row shows the real OID {short} read from the index: {:?}",
            row.detail
        );
    }
}

#[test]
fn choosing_a_side_for_an_unconflicted_path_is_refused() {
    // Its own app: opening a second buffer INVALIDATES the earlier view key, so this case must
    // not share a session with the conflict fixture.
    let (_app_dirs, mut app) = app();
    // A real repository, so the git status the command consults exists.
    let _repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    fs::write(root.join("clean.txt"), "clean\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let (buffer, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("clean\n"),
            Some(root.join("clean.txt")),
        )
        .unwrap();
    app.state.focus_view(view).unwrap();
    app.state.views[view].buffer = buffer;

    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.choose",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("ours".into())),
    );
    assert!(
        app.ui.notification_error,
        "choosing a side for an unconflicted path is refused: {:?}",
        app.ui.notification
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("not a conflicted path")),
        "and the refusal names the reason rather than failing for another one: {:?}",
        app.ui.notification
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "clean\n",
        "the unconflicted buffer is untouched"
    );
    assert!(!app.state.buffers[buffer].dirty, "and is not marked dirty");
}

#[test]
fn changes_rows_display_index_and_worktree_status_independently() {
    let (_app_dirs, mut app, repo, _) = near_changes_fixture();
    let path = app.state.workspace_root.join("added.txt");
    fs::write(&path, "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("added.txt")).unwrap();
    index.write().unwrap();
    fs::write(&path, "modified after staging\n").unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.open_git_status_picker();
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    let added = rows
        .iter()
        .filter(|row| row.label == "added.txt")
        .collect::<Vec<_>>();
    assert_eq!(added.len(), 2);
    assert!(added.iter().all(|row| row.source == "AM"));
}

/// A commit made outside Dust shows after the explorer's refresh key.
#[test]
fn refreshing_the_explorer_also_refreshes_git_status() {
    let (_app_dirs, mut app) = app();
    git2::Repository::init(&app.state.workspace_root).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.process_effect(AppEffect::ExplorerRefresh);
    assert!(app.git.status_pending(), "the refresh asks Git again");
}

#[test]
fn switching_up_from_a_nested_worktree_is_blocked_by_its_unsaved_file() {
    let main = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let main = main.path().canonicalize().unwrap();
    let nested = main.join(".worktree/feature");
    fs::create_dir_all(&nested).unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: nested.clone(),
        },
        crate::config::load_bundled_config(),
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("unsaved\n"),
            Some(nested.join("notes.txt")),
        )
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);

    app.switch_workspace_root(main.clone());
    assert!(app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|text| text.contains("unsaved changes")),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn cached_hunk_reset_never_targets_a_previous_buffer() {
    let (_app_dirs, mut app, _repo, path) = near_changes_fixture();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    let before = fs::read(&path).unwrap();
    let other = app.state.workspace_root.join("other.txt");
    fs::write(&other, "other\n").unwrap();
    app.state
        .open_text(TextBuffer::from_text("other\n"), Some(other))
        .unwrap();
    app.request_reset_selected_git_hunk();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    settle_git_work(&mut app);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn conflict_navigation_works_in_unnamed_buffers() {
    let (_app_dirs, mut app) = app();
    let text = "intro\n<<<<<<< ours\nleft\n=======\nright\n>>>>>>> theirs\n";
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(text), None)
        .unwrap();
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));
    app.state.views[view].cursor = ByteOffset(text.len());
    app.process_effect(AppEffect::GitConflictNavigate(-1));
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));
}

#[test]
fn save_and_stage_writes_the_refused_file_and_refreshes_its_hunks() {
    let (_app_dirs, mut app, repo, path) = near_changes_fixture();
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "one\nNEW\nthree\nFOUR\nfive\n".into())
        .unwrap();
    let other = app.state.workspace_root.join("other.txt");
    fs::write(&other, "other\n").unwrap();
    let (other_buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("other\n"), Some(other.clone()))
        .unwrap();
    app.replace_with_recovered(other_buffer, "other unsaved\n".into())
        .unwrap();
    app.open_git_status_picker();
    for _ in 0..8 {
        if app
            .ui
            .picker
            .as_ref()
            .and_then(|picker| picker.selected.map(|row| picker.rows[row].label.as_str()))
            == Some("near.txt")
        {
            break;
        }
        app.picker_move(1);
    }
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_none(),
        "confirmation was not visible: {:?}",
        app.input.prompt()
    );
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    assert_eq!(fs::read_to_string(&other).unwrap(), "other\n");
    assert_eq!(app.state.active_buffer_id(), Some(other_buffer));
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    let expected = b"one\nNEW\nthree\nFOUR\nfive\n";
    assert_eq!(fs::read(path).unwrap(), expected);
    assert_eq!(blob.content(), expected, "{:?}", app.ui.notification);
}

#[test]
fn failed_conflict_staging_cannot_arm_the_next_save() {
    let (_app_dirs, mut app) = app();
    app.stage_git_conflict();
    assert!(app.input.take_pending_stage().is_none());
}

#[test]
fn save_and_stage_refreshes_the_pre_save_hunk_snapshot() {
    let (_app_dirs, mut app, repo, _) = near_changes_fixture();
    app.open_explorer();
    app.process_effect(AppEffect::GitHunkNavigate(1));
    settle_git_work(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "one\nNEW\nthree\nFOUR\nfive\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitHunkStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(
        app.input.prompt().is_none(),
        "confirmation was not visible: {:?}",
        app.input.prompt()
    );
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert!(
        String::from_utf8_lossy(blob.content()).contains("NEW"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn conflict_picker_resolves_paths_from_the_repository_root() {
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    let path = root.path().join("sub/file.rs");
    let signature = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let commit = |text: &str| {
        fs::write(&path, text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("sub/file.rs")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let base = commit("base\n");
    repo.branch("side", &repo.find_commit(base).unwrap(), false)
        .unwrap();
    commit("ours\n");
    let main = repo.head().unwrap().name().unwrap().to_owned();
    repo.set_head("refs/heads/side").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    let theirs = commit("theirs\n");
    repo.set_head(&main).unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    repo.merge(&[&repo.find_annotated_commit(theirs).unwrap()], None, None)
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().join("sub"),
        },
        crate::config::load_bundled_config(),
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.open_git_conflicts();
    let Some(crate::app::picker::PickerPayload::MatcherSource(source)) =
        app.selected_picker_payload()
    else {
        panic!("conflict row missing")
    };
    assert_eq!(source.path, path);
}

#[test]
fn document_paths_identify_their_worktrees_independently_of_open_requests() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base", "tip"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    // Two linked worktrees, both OUTSIDE the main worktree so the main worktree stays clean.
    let outside = tempfile::tempdir().unwrap().keep();
    let mut roots = Vec::new();
    for name in ["alpha", "beta"] {
        let branch = repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        let path = outside.join(name);
        repo.worktree(name, &path, Some(&options)).unwrap();
        fs::write(path.join("note.txt"), name).unwrap();
        roots.push(path);
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let identities = app
        .workspace
        .roots()
        .iter()
        .map(|root| root.identity.clone())
        .collect::<Vec<_>>();

    let alpha_file = roots[0].join("note.txt");
    let beta_file = roots[1].join("note.txt");
    app.documents
        .open_explorer(alpha_file.clone(), identities.clone())
        .expect("open the alpha document");
    wait_vfs(&mut app);
    let alpha =
        git2::Repository::discover(&alpha_file).expect("the alpha path belongs to a worktree");
    assert_eq!(
        alpha.workdir().unwrap(),
        std::fs::canonicalize(&roots[0]).unwrap(),
        "the identity names the worktree the document was opened in"
    );

    app.documents
        .open_explorer(beta_file.clone(), identities)
        .expect("open the beta document");
    wait_vfs(&mut app);
    let beta = git2::Repository::discover(&beta_file).expect("the beta path belongs to a worktree");
    assert_eq!(
        beta.workdir().unwrap(),
        std::fs::canonicalize(&roots[1]).unwrap()
    );
    assert_ne!(
        alpha.path(),
        beta.path(),
        "two linked worktrees carry distinct identities"
    );
    assert_eq!(
        git2::Repository::discover(&alpha_file)
            .expect("the alpha identity is unchanged")
            .path(),
        alpha.path(),
        "opening another worktree never retargets an existing buffer"
    );

    // A path that cannot be canonicalized has no identity rather than a fabricated one.
    assert!(
        git2::Repository::discover(std::path::Path::new("/nonexistent-dust/orphan.txt")).is_err(),
        "a document that does not exist has no worktree"
    );
    let unopened = roots[0].join("never-opened.txt");
    fs::write(&unopened, "x").unwrap();
    let identity = git2::Repository::discover(&unopened).unwrap();
    assert_eq!(identity.workdir(), alpha.workdir());
    assert_eq!(identity.path(), alpha.path());
    assert!(unopened.starts_with(identity.workdir().unwrap()));
}

#[test]
fn a_name_no_worktree_matches_is_the_name_of_a_new_one() {
    let (_app_dirs, mut app) = app();
    let new_row = crate::workspace::PickerItem {
        badge: String::new(),
        kind: crate::workspace::PickerItemKind::Entry,
        id: crate::workspace::PickerItemId("git-worktree-new".into()),
        label: "New worktree…".into(),
        detail: String::new(),
        source: String::new(),
        action: crate::workspace::PickerAction::None,
    };
    let payload =
        crate::app::picker::PickerPayload::Command("workspace.git.worktree.create".into());
    app.install_picker("Git worktrees".into(), vec![(new_row, payload)]);
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "the row itself is selected"
    );
    for key in ["x", "y", "z"] {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
    assert_eq!(app.search.new_worktree_name().as_deref(), Some("xyz"));
    app.install_picker("Files".into(), Vec::new());
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "only the worktree list creates"
    );
}
