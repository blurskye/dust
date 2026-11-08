use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(140, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(screen: &mut GitScreen, ready: impl Fn(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(Instant::now() < deadline, "{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn creates_worktree_from_entered_branch_and_recovers_after_refusal() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Worktree Test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["commit", "--allow-empty", "-m", "Root commit"]);
    git(&root, &["branch", "review"]);
    let target = owner.path().join("review");
    let mut screen = GitScreen::open(&root).unwrap();
    wait(&mut screen, |s| view(s).contains("Root commit"));
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char('n'));
    screen.event(UiEvent::Paste("review".into()));
    key(&mut screen, KeyCode::Tab);
    // The default destination is the sibling review directory.
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| !s.busy());
    assert!(target.join(".git").is_file());
    let expected = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "review"])
        .output()
        .unwrap()
        .stdout;
    let actual = Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(actual, expected);
    // Creating it again must report a refusal and release the UI's busy state.
    key(&mut screen, KeyCode::Char('n'));
    screen.event(UiEvent::Paste("review".into()));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| !s.busy());
    let refusal = view(&mut screen);
    assert!(refusal.contains("is not empty"), "{refusal}");
}
