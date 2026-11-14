//! Native preview transitions must never label old code as a newly selected file.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim_end().into()
}
fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-b", "main"]);
    git(root.path(), &["config", "user.name", "Preview Test"]);
    git(
        root.path(),
        &["config", "user.email", "preview@example.invalid"],
    );
    std::fs::write(root.path().join("a.rs"), "fn alpha_original() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "fn beta_original() {}\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "Initial preview files"]);
    root
}
fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
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
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn cursor(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    let (x, y, _) = frame.cursor.expect("real terminal cursor");
    let text = frame
        .grid
        .row_text(y)
        .chars()
        .skip(usize::from(x))
        .collect::<String>();
    assert!(
        !text.starts_with(['+', '-', '│']) && !text.trim().is_empty(),
        "cursor is not on code: {text:?}"
    );
    text.trim_end().into()
}
fn wait(screen: &mut GitScreen, ready: impl Fn(&mut GitScreen) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(Instant::now() < end, "preview timed out:\n{}", view(screen));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn switching_file_or_index_layer_hides_old_code_before_the_new_read_finishes() {
    let owner = repository();
    let root = owner.path();
    std::fs::write(root.join("a.rs"), "fn alpha_working() {}\n").unwrap();
    std::fs::write(root.join("b.rs"), "fn beta_staged() {}\n").unwrap();
    git(root, &["add", "b.rs"]);
    std::fs::write(root.join("b.rs"), "fn beta_working() {}\n").unwrap();
    let original_index = git(root, &["diff", "--cached"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| {
        view(screen).contains("fn alpha_working")
    });
    key(&mut screen, KeyCode::Down);
    // No poll: the child can finish, but its response has not been accepted yet.
    let pending = view(&mut screen);
    assert!(pending.contains("Loading…"), "{pending}");
    assert!(
        !pending.contains("fn alpha_working"),
        "old source is mislabeled as b.rs: {pending}"
    );
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::Char(' '));
    assert!(view(&mut screen).contains("diff is refreshing"));
    assert_eq!(git(root, &["diff", "--cached"]), original_index);
    wait(&mut screen, |screen| {
        view(screen).contains("fn beta_working") && !view(screen).contains("Loading…")
    });
    assert!(!view(&mut screen).contains("diff is refreshing"));
    key(&mut screen, KeyCode::Char('i'));
    let pending = view(&mut screen);
    assert!(pending.contains("Loading…"));
    assert!(
        !pending.contains("fn beta_working"),
        "old working diff must not appear under Index"
    );
    wait(&mut screen, |screen| {
        view(screen).contains("fn beta_staged") && !view(screen).contains("Loading…")
    });
    assert!(!view(&mut screen).contains("fn beta_working"));
    assert_eq!(git(root, &["diff", "--cached"]), original_index);
    assert_eq!(
        std::fs::read_to_string(root.join("b.rs")).unwrap(),
        "fn beta_working() {}\n"
    );
}

#[test]
fn same_file_refresh_keeps_visible_code_and_the_real_cursor_position() {
    let owner = repository();
    let root = owner.path();
    let body = (0..80)
        .map(|n| format!("fn source_{n:02}() {{}}\n"))
        .collect::<String>();
    std::fs::write(root.join("a.rs"), &body).unwrap();
    git(root, &["add", "a.rs"]);
    git(root, &["commit", "-m", "Numbered source"]);
    let changed = body.replace("source_60", "changed_60");
    std::fs::write(root.join("a.rs"), &changed).unwrap();
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| {
        view(screen).contains("changed_60") && !view(screen).contains("Loading…")
    });
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::End);
    let before = cursor(&mut screen);
    std::fs::write(
        root.join("z-marker.txt"),
        "new file observed by refresh\n",
    )
    .unwrap();
    key(&mut screen, KeyCode::Char('r'));
    assert!(view(&mut screen).contains("changed_60"));
    assert_eq!(cursor(&mut screen), before);
    wait(&mut screen, |screen| {
        view(screen).contains("z-marker.txt") && !view(screen).contains("Loading…")
    });
    assert_eq!(cursor(&mut screen), before);
    assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), changed);
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn historical_file_refresh_preserves_filter_selection_source_and_cursor() {
    let owner = repository();
    let root = owner.path();
    std::fs::write(root.join("a.rs"), "fn alpha_committed() {}\n").unwrap();
    std::fs::write(root.join("b.rs"), "fn beta_committed() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Review both source files"]);
    let mut screen = GitScreen::open(root).unwrap();
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |screen| {
        view(screen).contains("fn alpha_committed")
    });
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char('/'));
    screen.event(UiEvent::Paste("b.rs".into()));
    key(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("fn alpha_committed"));
    wait(&mut screen, |screen| {
        view(screen).contains("fn beta_committed")
    });
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::End);
    let before = cursor(&mut screen);
    assert!(before.starts_with("fn beta_committed"));
    key(&mut screen, KeyCode::Char('r'));
    let immediate = view(&mut screen);
    assert!(immediate.contains("fn beta_committed") && !immediate.contains("fn alpha_committed"));
    assert_eq!(cursor(&mut screen), before);
    // Check both while the response is pending and after it is delivered.
    let end = Instant::now() + Duration::from_millis(250);
    while Instant::now() < end {
        screen.poll();
        assert_eq!(cursor(&mut screen), before);
        std::thread::sleep(Duration::from_millis(5));
    }
    control(&mut screen, KeyCode::Char('h'));
    let frame = screen.frame(120, 40);
    let (_, y, _) = frame.cursor.unwrap();
    assert!(
        frame
            .grid
            .row_text(y)
            .chars()
            .take(36)
            .collect::<String>()
            .contains("b.rs")
    );
    assert!(view(&mut screen).contains("b.rs"));
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
}
