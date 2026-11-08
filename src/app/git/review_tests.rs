//! Changed-files review through configured keys and real repository mutations.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::documents::tests::index_bytes;
use crate::app::tests::{app, prepare_trust_identity, wait_vfs};
use crate::ui::{KeyInputKind, PickerPreviewBody, PickerSurfaceKind, PromptKind, UiEvent};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

fn key(app: &mut App, keys: &str) {
    for key in keys.split_whitespace() {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: KeyInputKind::Press,
        });
    }
}

fn wait_git(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.git.status_pending() || app.git.mutation_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline, "Git operation timed out");
        std::thread::yield_now();
    }
}

fn wait_diff(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.search.diff_snapshot.is_none() {
        app.process_git_hunk_events();
        assert!(
            Instant::now() < deadline,
            "diff did not load: {:?}",
            app.ui.picker
        );
        std::thread::yield_now();
    }
}

fn review() -> ((tempfile::TempDir, tempfile::TempDir), App, String) {
    let (dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let original = (1..=60)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    for name in ["a.txt", "b.txt"] {
        fs::write(app.state.workspace_root.join(name), &original).unwrap();
    }
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.add_path(Path::new("b.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Review", "review@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
        .unwrap();
    fs::write(
        app.state.workspace_root.join("a.txt"),
        original
            .replace("line 2\n", "changed 2\n")
            .replace("line 50\n", "changed 50\n"),
    )
    .unwrap();
    fs::write(
        app.state.workspace_root.join("b.txt"),
        original.replace("line 10\n", "changed 10\n"),
    )
    .unwrap();
    fs::write(app.state.workspace_root.join("new.txt"), "new file\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    key(&mut app, "space g s");
    wait_git(&mut app);
    wait_diff(&mut app);
    assert_eq!(app.search.picker_surface(), PickerSurfaceKind::Changes);
    assert!(app.input.picker_normal());
    assert_eq!(
        app.search.diff_snapshot.as_ref().unwrap().file,
        Path::new("a.txt")
    );
    (dirs, app, original)
}

#[test]
fn review_keys_move_files_and_hunks_and_open_at_the_selected_change() {
    let (_dirs, mut app, _) = review();
    assert_eq!(app.search.diff_snapshot.as_ref().unwrap().hunks.len(), 2);
    key(&mut app, "] h");
    assert_eq!(app.search.diff_hunk, 1);
    let PickerPreviewBody::Diff(diff) = &app.search.preview.as_ref().unwrap().body else {
        panic!("diff")
    };
    assert!(diff.provenance.contains("Hunk 2/2"));
    let scroll = app.search.picker_preview_scroll(12).unwrap();
    assert!(scroll > 0);
    key(&mut app, "[ h");
    assert_eq!(app.search.diff_hunk, 0);
    key(&mut app, "[ h");
    assert_eq!(app.search.diff_hunk, 1, "navigation wraps");
    key(&mut app, "j");
    assert!(
        app.search.diff_snapshot.is_none(),
        "old selection is invalid immediately"
    );
    wait_diff(&mut app);
    assert_eq!(
        app.search.diff_snapshot.as_ref().unwrap().file,
        Path::new("b.txt")
    );
    assert_eq!(app.search.diff_hunk, 0);
    key(&mut app, "k");
    wait_diff(&mut app);
    key(&mut app, "] h enter");
    wait_vfs(&mut app);
    assert!(app.ui.picker.is_none());
    let view = &app.state.views[app.state.active_view().unwrap()];
    let buffer = &app.state.buffers[view.buffer];
    assert_eq!(
        buffer.path.as_deref(),
        Some(app.state.workspace_root.join("a.txt").as_path())
    );
    assert_eq!(buffer.text.byte_to_line(view.cursor).unwrap(), 49);
}

#[test]
fn review_stages_and_unstages_only_the_displayed_hunk_then_the_whole_file() {
    let (_dirs, mut app, original) = review();
    key(&mut app, "] h S");
    wait_git(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "a.txt"),
        original.replace("line 50\n", "changed 50\n")
    );
    assert_eq!(index_bytes(&app.state.workspace_root, "b.txt"), original);
    wait_diff(&mut app);
    key(&mut app, "tab");
    wait_diff(&mut app);
    key(&mut app, "U");
    wait_git(&mut app);
    assert_eq!(index_bytes(&app.state.workspace_root, "a.txt"), original);
    wait_diff(&mut app);
    key(&mut app, "s");
    wait_git(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "a.txt"),
        fs::read_to_string(app.state.workspace_root.join("a.txt")).unwrap()
    );
    // Refresh retains the query so file actions still name the reviewed path.
    app.picker_insert("a.txt");
    wait_diff(&mut app);
    key(&mut app, "u");
    wait_git(&mut app);
    assert_eq!(index_bytes(&app.state.workspace_root, "a.txt"), original);
}

#[test]
fn review_discard_requires_confirmation_and_preserves_other_hunks_and_the_index() {
    let (_dirs, mut app, original) = review();
    let path = app.state.workspace_root.join("a.txt");
    let changed = fs::read_to_string(&path).unwrap();
    key(&mut app, "] h r");
    assert_eq!(app.ui.prompt.as_ref().unwrap().kind, PromptKind::GitReset);
    assert!(
        app.ui
            .prompt
            .as_ref()
            .unwrap()
            .help
            .as_ref()
            .unwrap()
            .contains("a.txt")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    key(&mut app, "escape");
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    key(&mut app, "r");
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("wrong".into());
    assert!(!app.git.mutation_pending());
    key(&mut app, "r");
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    wait_git(&mut app);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        original.replace("line 2\n", "changed 2\n")
    );
    assert_eq!(index_bytes(&app.state.workspace_root, "a.txt"), original);
    assert_eq!(app.search.picker_surface(), PickerSurfaceKind::Changes);
}

#[test]
fn review_refuses_a_stale_disk_hunk_and_an_unsaved_buffer() {
    let (_dirs, mut app, original) = review();
    let path = app.state.workspace_root.join("a.txt");
    fs::write(&path, "external change\n").unwrap();
    key(&mut app, "S");
    wait_git(&mut app);
    assert_eq!(index_bytes(&app.state.workspace_root, "a.txt"), original);
    key(&mut app, "r");
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    wait_git(&mut app);
    assert_eq!(fs::read_to_string(&path).unwrap(), "external change\n");
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("unsaved\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    key(&mut app, "S");
    assert!(!app.git.mutation_pending());
    key(&mut app, "r");
    app.close_prompt();
    app.confirm_reset_selected_git_hunk("reset this hunk".into());
    assert!(!app.git.mutation_pending());
    assert_eq!(fs::read_to_string(path).unwrap(), "external change\n");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "unsaved\n");
}

#[test]
fn review_only_keys_do_not_mutate_from_other_pickers() {
    let (_dirs, mut app, original) = review();
    app.install_picker("Other picker".into(), Vec::new());
    app.input.set_picker_normal(true);
    for keys in ["S", "U", "r", "] h", "[ h"] {
        key(&mut app, keys);
        assert!(!app.git.mutation_pending());
        assert!(app.ui.prompt.is_none());
    }
    assert_eq!(index_bytes(&app.state.workspace_root, "a.txt"), original);
    app.process_effect(AppEffect::GitStatusOpen);
    wait_diff(&mut app);
    key(&mut app, "i");
    key(&mut app, "j");
    assert_eq!(
        app.search.picker().unwrap().query(),
        "j",
        "Insert still filters"
    );
}
