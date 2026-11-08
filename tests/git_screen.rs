use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().into()
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Screen Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Initial commit"]);
    dir
}
fn key(screen: &mut GitScreen, code: KeyCode) -> ScreenOutcome {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    })
}
fn text(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn submit(screen: &mut GitScreen) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
}
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(screen: &mut GitScreen, ready: impl Fn(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
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

#[test]
fn unstaging_rename_restores_both_paths_and_preserves_other_staged_changes() {
    let dir = repo();
    git(dir.path(), &["mv", "file.txt", "renamed.txt"]);
    std::fs::write(dir.path().join("z-other.txt"), "unrelated staged content\n").unwrap();
    git(dir.path(), &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("renamed.txt"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(git(dir.path(), &["show", ":file.txt"]), "old");
    assert!(!dir.path().join("file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("renamed.txt")).unwrap(),
        "old\n"
    );
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "Only unrelated change");
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(
            dir.path(),
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        ),
        "z-other.txt"
    );
    assert_eq!(git(dir.path(), &["show", "HEAD:file.txt"]), "old");
}

#[test]
fn stash_preview_includes_untracked_files() {
    let dir = repo();
    std::fs::write(dir.path().join("new.txt"), "untracked stash contents\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("new.txt"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "Untracked-only stash");
    submit(&mut screen);
    wait(&mut screen, |s| {
        !s.busy() && view(s).contains("+untracked stash contents")
    });
    assert!(!dir.path().join("new.txt").exists());
    assert!(view(&mut screen).contains("new.txt"));
    key(&mut screen, KeyCode::Char('p'));
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "untracked stash contents\n"
    );
    assert_eq!(git(dir.path(), &["stash", "list"]), "");
}

#[test]
fn stages_unstages_and_commits_through_real_screen_input() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("changed"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"]),
        "file.txt"
    );
    key(&mut screen, KeyCode::Char('v'));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("changed"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "Unicode λ message");
    key(&mut screen, KeyCode::Left);
    key(&mut screen, KeyCode::Right);
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "Unicode λ message"
    );
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
}

#[test]
fn branch_preview_is_selected_branch_not_current_head() {
    let dir = repo();
    git(dir.path(), &["checkout", "-b", "feature"]);
    std::fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Feature only"]);
    git(dir.path(), &["checkout", "main"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("feature"));
    key(&mut screen, KeyCode::Char('2'));
    wait(&mut screen, |s| view(s).contains("Feature only"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| {
        view(s).contains("Author: Screen Test") && !view(s).contains("Feature only")
    });
    assert_eq!(git(dir.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn file_tree_and_real_cursor_do_not_shift_row_text() {
    let dir = repo();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/new.rs"), "fn main() {}\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("new.rs"));
    let before = screen.frame(120, 40);
    assert!(before.cursor.is_some());
    key(&mut screen, KeyCode::Char('2'));
    let after = screen.frame(120, 40);
    assert_eq!(before.grid.row_text(3), after.grid.row_text(3));
    key(&mut screen, KeyCode::Char('1'));
    key(&mut screen, KeyCode::Char('h'));
    assert!(!view(&mut screen).contains("new.rs"));
    key(&mut screen, KeyCode::Char('l'));
    assert!(view(&mut screen).contains("new.rs"));
    key(&mut screen, KeyCode::Char('j'));
    assert_eq!(
        key(&mut screen, KeyCode::Enter),
        ScreenOutcome::OpenFile(dir.path().join("src/new.rs"))
    );
}

#[test]
fn host_write_refusal_and_cancelled_form_preserve_repo() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("changed"));
    screen.write_refusal = Some("Unsaved editor buffer".into());
    key(&mut screen, KeyCode::Char(' '));
    assert!(view(&mut screen).contains("Unsaved editor buffer"));
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "cancelled");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "Initial commit"
    );
}

#[test]
fn worktrees_tags_and_stashes_are_real_repository_data() {
    let dir = repo();
    let target = dir.path().join("../screen-review");
    let owner = tempfile::tempdir().unwrap();
    let target = owner.path().join(target.file_name().unwrap());
    git(dir.path(), &["branch", "review"]);
    git(dir.path(), &["tag", "v1"]);
    git(
        dir.path(),
        &["worktree", "add", target.to_str().unwrap(), "review"],
    );
    std::fs::write(dir.path().join("file.txt"), "stashed\n").unwrap();
    git(dir.path(), &["stash", "push", "-m", "my work"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("my work"));
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |s| view(s).contains("screen-review"));
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    assert!(view(&mut screen).contains("v1"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('p'));
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "stashed\n"
    );
    assert_eq!(git(dir.path(), &["stash", "list"]), "");
}
