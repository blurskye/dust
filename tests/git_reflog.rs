//! Native reflog recovery against generated, real Git repositories.
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::{GitScreen, ScreenOutcome};
use dusty::ui::{KeyInputKind, UiEvent};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn git_output(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap()
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim_end().into()
}
fn write(root: &Path, path: &str, value: &str) {
    std::fs::write(root.join(path), value).unwrap();
}
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    head: String,
    lost: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Reflog Test"]);
        git(&root, &["config", "user.email", "reflog@example.invalid"]);
        write(&root, "tracked.txt", "baseline\n");
        write(&root, "staged.txt", "baseline index\n");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "Baseline"]);
        let head = git(&root, &["rev-parse", "HEAD"]);
        // Lose a detached commit by returning to main, without resetting any branch.
        git(&root, &["checkout", "--detach"]);
        write(&root, "tracked.txt", "lost tracked content\n");
        write(&root, "recovered.txt", "recovered only from the reflog\n");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "Lost useful changes"]);
        let lost = git(&root, &["rev-parse", "HEAD"]);
        // Same OID, distinct events: selection must use the event key, not the commit ID.
        for message in ["lost event first", "lost event second"] {
            git(&root, &["update-ref", "HEAD", &head]);
            git(&root, &["update-ref", "-m", message, "HEAD", &lost]);
        }
        git(&root, &["checkout", "main"]);
        assert_eq!(git(&root, &["branch", "--contains", &lost]), "");
        Self {
            _directory: directory,
            root,
            head,
            lost,
        }
    }
    fn destination(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join(name)
    }
    fn dirty(&self) {
        write(&self.root, "tracked.txt", "keep working edits\n");
        write(&self.root, "staged.txt", "keep staged bytes\n");
        git(&self.root, &["add", "staged.txt"]);
        write(&self.root, "staged.txt", "keep later unstaged bytes\n");
        write(&self.root, "untracked.txt", "keep untracked bytes\n");
    }
}

fn key(screen: &mut GitScreen, code: KeyCode) -> ScreenOutcome {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    })
}
fn control(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
}
fn text(screen: &mut GitScreen, value: &str) {
    screen.event(UiEvent::Paste(value.into()));
}
fn view_at(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    (0..height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    view_at(screen, 190, 48)
}
fn wait(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "screen timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn selected(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    // A pane still loading shows a note and no cursor: nothing is selected yet.
    let Some((x, y, _)) = frame.cursor else {
        return String::new();
    };
    assert!(x < 52, "expected sidebar focus, got cursor {x},{y}");
    frame.grid.row_text(y).chars().take(52).collect()
}
fn filter(screen: &mut GitScreen, query: &str) {
    key(screen, KeyCode::Char('/'));
    text(screen, query);
    key(screen, KeyCode::Enter);
}
fn open_reflog(fixture: &Fixture) -> GitScreen {
    let mut screen = GitScreen::open(&fixture.root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("Baseline"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| selected(screen).contains("HEAD@{0}"));
    filter(&mut screen, "lost event second");
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("lost event second")
            && displayed.contains("recovered only from the reflog")
    });
    screen
}
fn replace_input(screen: &mut GitScreen, value: &str) {
    control(screen, KeyCode::Char('u'));
    text(screen, value);
}
fn submit(screen: &mut GitScreen) {
    control(screen, KeyCode::Enter);
}
/// A sheet is open while its footer offers `Esc cancel`.
fn sheet_open(screen: &mut GitScreen) -> bool {
    view(screen).contains("Esc cancel")
}
fn finished(screen: &mut GitScreen) {
    wait(screen, |screen| !screen.busy());
}
fn refs(root: &Path) -> String {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
}
#[derive(Debug, PartialEq)]
struct ParentState {
    head: Vec<u8>,
    oid: String,
    index: Vec<u8>,
    status: String,
    files: Vec<Option<Vec<u8>>>,
}
fn parent_state(root: &Path) -> ParentState {
    let repo = git2::Repository::open(root).unwrap();
    ParentState {
        head: std::fs::read(repo.path().join("HEAD")).unwrap(),
        oid: git(root, &["rev-parse", "HEAD"]),
        index: std::fs::read(repo.path().join("index")).unwrap(),
        status: git(root, &["status", "--porcelain=v1", "--untracked-files=all"]),
        files: [
            "tracked.txt",
            "staged.txt",
            "untracked.txt",
            "recovered.txt",
        ]
        .iter()
        .map(|name| std::fs::read(root.join(name)).ok())
        .collect(),
    }
}

#[test]
fn rescue_branch_recovers_the_lost_commit_without_checkout_or_dirty_parent_index_changes() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_reflog(&fixture);
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "rescue/lost-work");
    let form = view(&mut screen);
    assert!(form.contains("Create rescue branch at commit"));
    assert!(form.contains(&fixture.lost));
    assert!(form.contains("lost event second"));
    assert!(form.contains("Lost useful changes"));
    assert!(form.contains("do not check out"));
    assert!(
        !git_output(
            &fixture.root,
            &["show-ref", "--verify", "refs/heads/rescue/lost-work"]
        )
        .status
        .success()
    );
    submit(&mut screen);
    assert!(screen.busy());
    // While the effect runs, repeat Enter, paste and Escape cannot create a second action
    // or lose the original input before a possible Git refusal.
    submit(&mut screen);
    text(&mut screen, "ignored while submitting");
    key(&mut screen, KeyCode::Escape);
    finished(&mut screen);
    assert_eq!(
        git(&fixture.root, &["rev-parse", "refs/heads/rescue/lost-work"]),
        fixture.lost
    );
    assert_eq!(parent_state(&fixture.root), before);
    let after = refs(&fixture.root)
        .lines()
        .filter(|line| !line.starts_with("refs/heads/rescue/lost-work "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(after, refs_before);
    assert!(!view(&mut screen).contains("Create rescue branch at commit"));
}

#[test]
fn equal_oid_events_keep_their_identity_and_an_open_form_keeps_its_immutable_commit() {
    let fixture = Fixture::new();
    let events = git(&fixture.root, &["log", "-g", "--format=%H %gs"]);
    for message in ["lost event first", "lost event second"] {
        assert!(events.contains(&format!("{} {message}", fixture.lost)));
    }
    let mut screen = open_reflog(&fixture);
    let original = selected(&mut screen);
    assert!(original.contains("HEAD@{1}"));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "rescue/pinned");
    // The immutable display field cannot be pasted over or cleared like an editable input.
    key(&mut screen, KeyCode::Tab);
    control(&mut screen, KeyCode::Char('u'));
    text(&mut screen, "HEAD@{0}");
    assert!(view(&mut screen).contains(&fixture.lost));
    git(
        &fixture.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Inserted while recovery form is open",
        ],
    );
    let moved = git(&fixture.root, &["rev-parse", "HEAD"]);
    assert_ne!(moved, fixture.head);
    assert_ne!(moved, fixture.lost);
    let before_submit = parent_state(&fixture.root);
    submit(&mut screen);
    finished(&mut screen);
    assert_eq!(
        git(&fixture.root, &["rev-parse", "rescue/pinned"]),
        fixture.lost
    );
    assert_eq!(parent_state(&fixture.root), before_submit);
    wait(&mut screen, |screen| selected(screen).contains("HEAD@{2}"));
    assert!(selected(&mut screen).contains("lost event second"));
    // Clearing the event filter restores its original identity; returning to the
    // duplicate event must not collapse it onto the first event with the same OID.
    key(&mut screen, KeyCode::Escape);
    filter(&mut screen, "lost event first");
    assert!(selected(&mut screen).contains("HEAD@{4}"));
    git(
        &fixture.root,
        &["commit", "--allow-empty", "-m", "Another inserted event"],
    );
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| selected(screen).contains("HEAD@{5}"));
    assert!(selected(&mut screen).contains("lost event first"));
}

#[test]
fn duplicate_and_invalid_branch_refusals_preserve_the_form_input_and_allow_retry() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_reflog(&fixture);
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "main");
    submit(&mut screen);
    finished(&mut screen);
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let displayed = view_at(&mut screen, width, height);
        assert!(displayed.contains("Create rescue branch at commit"));
        assert!(
            displayed.contains("already exists"),
            "{width}x{height}: {displayed}"
        );
        assert!(displayed.contains("main"));
        assert!(displayed.contains(&fixture.lost));
    }
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(parent_state(&fixture.root), before);
    for invalid in ["bad..name", "-force", "@{-1}", "has space"] {
        replace_input(&mut screen, invalid);
        submit(&mut screen);
        assert!(!screen.busy());
        let displayed = view(&mut screen);
        assert!(displayed.contains("Choose a valid branch name"));
        assert!(displayed.contains(invalid));
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(parent_state(&fixture.root), before);
    }
    replace_input(&mut screen, "rescue/retry");
    submit(&mut screen);
    finished(&mut screen);
    assert_eq!(
        git(&fixture.root, &["rev-parse", "rescue/retry"]),
        fixture.lost
    );
    assert_eq!(parent_state(&fixture.root), before);
}

#[test]
fn all_reflog_workflows_authorize_at_submission_and_cancel_without_effects() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let destination = fixture.destination("cancelled worktree");
    for action in ['n', 'o', 'w'] {
        let mut screen = open_reflog(&fixture);
        key(&mut screen, KeyCode::Char(action));
        match action {
            'n' => text(&mut screen, "rescue/cancelled"),
            'w' => replace_input(&mut screen, destination.to_str().unwrap()),
            _ => {}
        }
        // The host can revoke permission after the target and input were captured.
        screen.write_refusal = Some("Host refuses recovery writes".into());
        submit(&mut screen);
        assert!(!screen.busy());
        let displayed = view(&mut screen);
        assert!(displayed.contains("Host refuses recovery writes"));
        assert!(displayed.contains(&fixture.lost));
        assert_eq!(parent_state(&fixture.root), before);
        assert_eq!(refs(&fixture.root), refs_before);
        screen.write_refusal = None;
        if action == 'n' {
            // Name is still present after refusal. Use the actual Cancel button.
            assert!(view(&mut screen).contains("rescue/cancelled"));
            for _ in 0..5 {
                key(&mut screen, KeyCode::Tab);
            }
            key(&mut screen, KeyCode::Enter);
        } else {
            key(&mut screen, KeyCode::Escape);
        }
        assert!(selected(&mut screen).contains("lost event second"));
        assert!(!sheet_open(&mut screen));
        assert_eq!(parent_state(&fixture.root), before);
        assert_eq!(refs(&fixture.root), refs_before);
        assert!(!destination.exists());
        assert_eq!(
            git(&fixture.root, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }
}

#[test]
fn an_empty_reflog_filter_cannot_recover_or_check_out_a_hidden_old_selection() {
    let fixture = Fixture::new();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_reflog(&fixture);
    key(&mut screen, KeyCode::Escape);
    filter(&mut screen, "no-such-reflog-event");
    assert!(view(&mut screen).contains("No matches"));
    for action in ['n', 'o', 'w'] {
        key(&mut screen, KeyCode::Char(action));
        assert!(!screen.busy());
        assert!(view(&mut screen).contains("Select a reflog entry"));
        assert!(!sheet_open(&mut screen));
        assert_eq!(parent_state(&fixture.root), before);
        assert_eq!(refs(&fixture.root), refs_before);
    }
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("HEAD@{0}"));
}

#[test]
fn o_confirms_detached_checkout_of_the_selected_commit_and_keeps_main_ref() {
    let fixture = Fixture::new();
    let refs_before = refs(&fixture.root);
    let before = parent_state(&fixture.root);
    let mut screen = open_reflog(&fixture);
    key(&mut screen, KeyCode::Char('o'));
    let displayed = view(&mut screen);
    assert!(displayed.contains("Check out commit with detached HEAD?"));
    assert!(displayed.contains("Detach HEAD and update the index and working files"));
    assert!(displayed.contains("Git refuses conflicting edits; no auto-stash"));
    assert!(displayed.contains(&fixture.lost));
    assert_eq!(
        parent_state(&fixture.root),
        before,
        "opening confirmation must not check out"
    );
    // The native Submit control is initially selected for a confirmation-only form.
    key(&mut screen, KeyCode::Enter);
    assert!(screen.busy());
    finished(&mut screen);
    let repository = git2::Repository::open(&fixture.root).unwrap();
    assert!(repository.head_detached().unwrap());
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.lost);
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(
        git(&fixture.root, &["show", ":tracked.txt"]),
        "lost tracked content"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("recovered.txt")).unwrap(),
        "recovered only from the reflog\n"
    );
    assert_eq!(git(&fixture.root, &["stash", "list"]), "");
}

#[test]
fn detached_checkout_lets_git_refuse_worktree_index_and_untracked_conflicts_without_autostash() {
    for conflict in ["working", "index", "untracked"] {
        let fixture = Fixture::new();
        match conflict {
            "working" => write(&fixture.root, "tracked.txt", "conflicting working edits\n"),
            "index" => {
                write(&fixture.root, "tracked.txt", "conflicting staged edits\n");
                git(&fixture.root, &["add", "tracked.txt"]);
            }
            _ => write(
                &fixture.root,
                "recovered.txt",
                "conflicting untracked file\n",
            ),
        }
        let before = parent_state(&fixture.root);
        let refs_before = refs(&fixture.root);
        let mut screen = open_reflog(&fixture);
        key(&mut screen, KeyCode::Char('o'));
        key(&mut screen, KeyCode::Enter);
        finished(&mut screen);
        let displayed = view_at(&mut screen, 64, 20);
        assert!(
            displayed.contains("Check out commit with detached HEAD?"),
            "{conflict}: {displayed}"
        );
        assert!(
            displayed.contains("would be overwritten"),
            "{conflict}: {displayed}"
        );
        assert!(view(&mut screen).contains(&fixture.lost));
        assert_eq!(parent_state(&fixture.root), before, "{conflict}");
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(git(&fixture.root, &["stash", "list"]), "");
        key(&mut screen, KeyCode::Escape);
        assert!(selected(&mut screen).contains("lost event second"));
    }
}

#[test]
fn detached_worktree_uses_selected_commit_and_source_identity_without_touching_dirty_parent() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let destination = fixture.destination("recovered worktree");
    let mut screen = open_reflog(&fixture);
    key(&mut screen, KeyCode::Char('w'));
    replace_input(&mut screen, destination.to_str().unwrap());
    let displayed = view(&mut screen);
    assert!(displayed.contains("Create detached worktree at commit"));
    assert!(displayed.contains(&fixture.lost));
    assert!(displayed.contains("keep this worktree unchanged"));
    assert!(!destination.exists());
    submit(&mut screen);
    assert!(screen.busy());
    finished(&mut screen);
    assert!(destination.join(".git").is_file(), "{}", view(&mut screen));
    let source = git2::Repository::open(&fixture.root).unwrap();
    let linked = git2::Repository::open(&destination).unwrap();
    assert_eq!(
        std::fs::canonicalize(linked.commondir()).unwrap(),
        std::fs::canonicalize(source.commondir()).unwrap()
    );
    assert!(linked.head_detached().unwrap());
    assert_eq!(
        linked.head().unwrap().target().unwrap().to_string(),
        fixture.lost
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("tracked.txt")).unwrap(),
        "lost tracked content\n"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("recovered.txt")).unwrap(),
        "recovered only from the reflog\n"
    );
    assert_eq!(parent_state(&fixture.root), before);
    assert_eq!(
        refs(&fixture.root),
        refs_before,
        "no persistent or moved branch from detached recovery"
    );
    // Git lists worktrees with `/` on Windows too.
    assert!(
        git(&fixture.root, &["worktree", "list", "--porcelain"])
            .contains(&destination.to_str().unwrap().replace('\\', "/"))
    );
    assert_eq!(screen.root(), fixture.root.as_path());
}

#[test]
fn worktree_destination_refusal_keeps_path_input_and_never_overwrites_existing_data_or_refs() {
    let fixture = Fixture::new();
    fixture.dirty();
    let occupied = fixture.destination("occupied");
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("keep.txt"), "existing destination data\n").unwrap();
    let protected = fixture.destination("protected-name");
    git(&fixture.root, &["branch", "dusty-worktree/protected-name"]);
    let registered = fixture.destination("registered");
    git(
        &fixture.root,
        &["worktree", "add", "--detach", registered.to_str().unwrap()],
    );
    let elsewhere = fixture.destination("other-parent");
    std::fs::create_dir(&elsewhere).unwrap();
    let duplicate_name = elsewhere.join("registered");
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_reflog(&fixture);
    for (destination, error) in [
        (&occupied, "not empty"),
        (&protected, "A branch uses this worktree directory name"),
        (&duplicate_name, "already exists"),
    ] {
        key(&mut screen, KeyCode::Char('w'));
        replace_input(&mut screen, destination.to_str().unwrap());
        submit(&mut screen);
        finished(&mut screen);
        let displayed = view(&mut screen);
        assert!(displayed.contains("Create detached worktree at commit"));
        assert!(displayed.contains(destination.to_str().unwrap()));
        assert!(displayed.contains(error), "{displayed}");
        assert_eq!(parent_state(&fixture.root), before);
        assert_eq!(refs(&fixture.root), refs_before);
        key(&mut screen, KeyCode::Escape);
    }
    assert_eq!(
        std::fs::read_to_string(occupied.join("keep.txt")).unwrap(),
        "existing destination data\n"
    );
    assert!(!protected.exists());
    assert!(!duplicate_name.exists());
}

#[cfg(unix)]
#[test]
fn detached_worktree_refuses_leaf_and_parent_symlink_aliases_without_touching_targets() {
    let fixture = Fixture::new();
    let outside = fixture.destination("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "outside data\n").unwrap();
    let alias = fixture.destination("alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    let before = parent_state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_reflog(&fixture);
    for destination in [&alias, &alias.join("child")] {
        key(&mut screen, KeyCode::Char('w'));
        replace_input(&mut screen, destination.to_str().unwrap());
        submit(&mut screen);
        assert!(!screen.busy());
        assert!(view(&mut screen).contains("Choose a worktree path without symlinks"));
        assert!(view(&mut screen).contains("Create detached worktree at commit"));
        assert_eq!(parent_state(&fixture.root), before);
        assert_eq!(refs(&fixture.root), refs_before);
        key(&mut screen, KeyCode::Escape);
    }
    assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
    assert!(!outside.join("child").exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "outside data\n"
    );
}

/// Top border rows of the rail panes (the rail starts at column 0). Medium and wide
/// layouts stack Changes, Branches, History and Stashes; narrow shows one surface.
fn pane_tops(screen: &mut GitScreen, width: u16, height: u16) -> Vec<u16> {
    let frame = screen.frame(width, height);
    (1..height - 1)
        .filter(|&y| frame.grid.row_text(y).starts_with('╭'))
        .collect()
}
#[test]
fn reflog_keys_help_history_files_and_native_cursor_preserve_64_100_190_pane_geometry() {
    let fixture = Fixture::new();
    let mut screen = GitScreen::open(&fixture.root).unwrap();
    // History can paint before the panes take their sizes for the session.
    wait(&mut screen, |screen| {
        let view = view(screen);
        view.contains("Baseline") && view.contains("Local 1") && screen.panes_sized()
    });
    let geometries = [(64, 20), (100, 30), (190, 48)];
    let positions = geometries.map(|(width, height)| pane_tops(&mut screen, width, height));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| selected(screen).contains("HEAD@{0}"));
    filter(&mut screen, "lost event second");
    wait(&mut screen, |screen| {
        view(screen).contains("recovered only from the reflog")
    });
    for ((width, height), positions) in geometries.into_iter().zip(positions) {
        let stacked = width >= 100;
        if stacked {
            assert_eq!(pane_tops(&mut screen, width, height), positions);
        }
        let before = screen.frame(width, height);
        let (x, y, _) = before.cursor.expect("native reflog cursor");
        assert_eq!(x, 1);
        if stacked {
            // The cursor sits inside the History pane (third), above Stashes.
            assert!(y > positions[2] && y < positions[3]);
        }
        let footer = before.grid.row_text(height - 1);
        for hint in ["n", "rescue branch", "o", "detach", "keys"] {
            assert!(
                footer.contains(hint),
                "{width}x{height}: missing {hint}: {footer}"
            );
        }
        key(&mut screen, KeyCode::Char('?'));
        let help = view_at(&mut screen, width, height);
        for hint in [
            "Keys · Reflog",
            "Rescue branch",
            "Checkout detached",
            "Detached worktree",
            "Browse files",
            "Filter",
        ] {
            assert!(
                help.contains(hint),
                "{width}x{height}: missing {hint}: {help}"
            );
        }
        key(&mut screen, KeyCode::Escape);
        assert_eq!(screen.frame(width, height).cursor, before.cursor);
        key(&mut screen, KeyCode::Char('n'));
        key(&mut screen, KeyCode::Escape);
        if stacked {
            assert_eq!(pane_tops(&mut screen, width, height), positions);
        }
        assert_eq!(screen.frame(width, height).cursor, before.cursor);
        assert_eq!(key(&mut screen, KeyCode::Enter), ScreenOutcome::Continue);
        let historical = view_at(&mut screen, width, height);
        assert!(historical.contains("Commit files"), "{historical}");
        if stacked {
            assert_eq!(pane_tops(&mut screen, width, height), positions);
        }
        // Historical file `-` remains read-only; it must not detach or stage.
        key(&mut screen, KeyCode::Char('-'));
        assert!(!screen.busy());
        assert_eq!(git(&fixture.root, &["branch", "--show-current"]), "main");
        key(&mut screen, KeyCode::Escape);
        assert_eq!(screen.frame(width, height).cursor, before.cursor);
    }
    // Commit is still c; recovery has not stolen it. With nothing staged the commit
    // sheet refuses to open and says why.
    key(&mut screen, KeyCode::Char('c'));
    let refused = view(&mut screen);
    assert!(
        refused.contains("Nothing staged · s stages the selected change"),
        "{refused}"
    );
    assert!(!refused.contains("Create rescue branch at commit"));
    assert!(!sheet_open(&mut screen));
}
