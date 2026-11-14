//! The Git tutor, driven through real screen input: every lesson advances only after
//! the keys it teaches changed the practice repository or the screen, and quitting
//! restores the user's own workspace untouched.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, KeyInputKind, UiEvent};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LESSONS: usize = 48;
const WIDTH: u16 = 190;
const HEIGHT: u16 = 48;
/// The wide layout's rail: `(25 + width / 7).clamp(44, 60)` columns.
const RAIL: usize = 52;

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
    git(dir.path(), &["config", "user.name", "Tutor Test"]);
    git(dir.path(), &["config", "user.email", "test@example.invalid"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Initial commit"]);
    dir
}

fn press(screen: &mut GitScreen, code: KeyCode, modifiers: Modifiers) {
    screen.event(UiEvent::Key {
        key: KeyEvent { code, modifiers },
        kind: KeyInputKind::Press,
    });
}
fn key(screen: &mut GitScreen, code: KeyCode) {
    press(screen, code, Modifiers::default());
}
fn char(screen: &mut GitScreen, letter: char) {
    key(screen, KeyCode::Char(letter));
}
fn keys(screen: &mut GitScreen, letters: &str) {
    for letter in letters.chars() {
        char(screen, letter);
    }
}
fn ctrl(screen: &mut GitScreen, code: KeyCode) {
    press(
        screen,
        code,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
}
fn alt(screen: &mut GitScreen, letter: char) {
    press(
        screen,
        KeyCode::Char(letter),
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
}
fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn command(screen: &mut GitScreen, text: &str) {
    char(screen, ':');
    paste(screen, text);
    key(screen, KeyCode::Enter);
}
fn rendered(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    rendered(&screen.frame(WIDTH, HEIGHT))
}
fn wait(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "tutor timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// Repeats an action that the screen may refuse while it is still loading.
fn until(
    screen: &mut GitScreen,
    action: impl Fn(&mut GitScreen),
    started: impl Fn(&mut GitScreen) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        action(screen);
        if started(screen) {
            return;
        }
        assert!(Instant::now() < deadline, "never started:\n{}", view(screen));
        for _ in 0..5 {
            screen.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn idle(screen: &mut GitScreen) {
    wait(screen, |screen| !screen.busy());
}
fn ready(screen: &mut GitScreen, needle: &str) {
    wait(screen, |screen| !screen.busy() && view(screen).contains(needle));
}
fn banner(screen: &mut GitScreen) -> String {
    let frame = screen.frame(WIDTH, HEIGHT);
    (0..HEIGHT)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("Git tutor ·"))
        .unwrap_or_default()
}
/// Waits for lesson `number` (one-based); `LESSONS + 1` is the finished tutor.
fn lesson(screen: &mut GitScreen, number: usize) {
    let expected = if number > LESSONS {
        format!("all {LESSONS} lessons done")
    } else {
        format!("· {number}/{LESSONS} ·")
    };
    wait(screen, |screen| banner(screen).contains(&expected));
    idle(screen);
}
fn cursor(screen: &mut GitScreen) -> Option<(usize, String)> {
    let frame = screen.frame(WIDTH, HEIGHT);
    let (x, y, _) = frame.cursor?;
    Some((usize::from(x), frame.grid.row_text(y)))
}
/// Focuses a pane by its digit and moves its native cursor onto the row naming `name`.
fn select(screen: &mut GitScreen, pane: char, name: &str) {
    char(screen, pane);
    wait(screen, |screen| {
        key(screen, KeyCode::Home);
        for _ in 0..40 {
            if cursor(screen).is_some_and(|(x, row)| {
                x < RAIL && row.chars().take(RAIL).collect::<String>().contains(name)
            }) {
                return true;
            }
            key(screen, KeyCode::Down);
        }
        false
    });
}
/// `v` opens the selected change in the reader; Home puts its cursor on the first line.
fn reader(screen: &mut GitScreen) {
    until(
        screen,
        |screen| {
            char(screen, 'v');
            key(screen, KeyCode::Home);
        },
        |screen| cursor(screen).is_some_and(|(x, _)| x > RAIL),
    );
}
/// Moves the reader's cursor onto the diff line containing `needle`.
fn seek(screen: &mut GitScreen, needle: &str) {
    wait(screen, |screen| {
        key(screen, KeyCode::Home);
        for _ in 0..80 {
            if cursor(screen).is_some_and(|(x, row)| {
                x > RAIL && row.chars().skip(RAIL).collect::<String>().contains(needle)
            }) {
                return true;
            }
            key(screen, KeyCode::Down);
        }
        false
    });
}
fn clear_field(screen: &mut GitScreen) {
    for _ in 0..40 {
        key(screen, KeyCode::Backspace);
    }
}
fn practice_base(screen: &GitScreen) -> PathBuf {
    // <TempDir>/<NN-attempt>/practice
    screen.root().parent().unwrap().parent().unwrap().to_owned()
}

#[test]
fn every_git_lesson_advances_on_real_input_and_quit_restores_the_workspace() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "untouched original work\n").unwrap();
    let original_head = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    screen.write_refusal = Some("Original workspace is untrusted".into());
    wait(&mut screen, |s| view(s).contains("untouched original work"));
    let before = view(&mut screen);
    let s = &mut screen;
    command(s, "tutor git");
    lesson(s, 1);
    let base = practice_base(s);
    assert!(!base.starts_with(dir.path()) && !dir.path().starts_with(&base));
    assert!(banner(s).contains("Git tutor · 1/48 · Panes · Focus panes"));
    assert!(view(s).contains(":tutor next skips"));

    // Panes.
    ready(s, "greet.rs");
    keys(s, "345");
    assert!(banner(s).contains("· 1/48 ·"), "advanced before returning to Changes");
    char(s, '2');
    lesson(s, 2);
    ready(s, "greet.rs");
    char(s, '0');
    key(s, KeyCode::Tab);
    lesson(s, 3);
    ready(s, "greet.rs");
    keys(s, "3]]4]");
    lesson(s, 4);

    // Changes.
    ready(s, "main.rs");
    char(s, 't');
    char(s, 'h');
    lesson(s, 5);
    ready(s, "main.rs");
    char(s, 'F');
    ready(s, "README.md");
    char(s, 'F');
    lesson(s, 6);
    ready(s, "guide.md");
    char(s, '/');
    paste(s, "guide");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Escape);
    lesson(s, 7);
    ready(s, "Try the tree view.");
    reader(s);
    keys(s, "ss");
    lesson(s, 8);

    // Staging.
    ready(s, "notes.md");
    select(s, '2', "notes.md");
    char(s, ' ');
    lesson(s, 9);
    ready(s, "c.txt");
    char(s, 'a');
    idle(s);
    char(s, 'a');
    lesson(s, 10);
    ready(s, "c.txt");
    select(s, '2', "a.txt");
    char(s, 'x');
    key(s, KeyCode::Down);
    char(s, 'x');
    char(s, ' ');
    lesson(s, 11);
    ready(s, "Line 3 was rewritten");
    reader(s);
    seek(s, "Line 3 was rewritten");
    until(s, |s| char(s, ' '), |s| s.busy());
    lesson(s, 12);
    ready(s, "gamma");
    reader(s);
    seek(s, "beta");
    until(s, |s| char(s, 'a'), |s| s.busy());
    lesson(s, 13);
    ready(s, "epsilon");
    reader(s);
    seek(s, "beta");
    until(s, |s| char(s, 'V'), |s| view(s).contains("Range started"));
    key(s, KeyCode::Down);
    until(s, |s| char(s, 'a'), |s| s.busy());
    lesson(s, 14);

    // Commit sheet.
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Practice commit");
    key(s, KeyCode::Enter);
    paste(s, "With a body line.");
    ctrl(s, KeyCode::Enter);
    lesson(s, 15);
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Signed practice commit");
    alt(s, 's');
    ctrl(s, KeyCode::Enter);
    lesson(s, 16);
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Commit past the hook");
    ctrl(s, KeyCode::Enter);
    idle(s);
    assert!(banner(s).contains("· 16/48 ·"), "the practice hook must block a plain commit");
    char(s, 'c');
    paste(s, "Commit past the hook");
    alt(s, 'n');
    ctrl(s, KeyCode::Enter);
    lesson(s, 17);
    ready(s, "farewell.md");
    char(s, 'c');
    alt(s, 'a');
    ctrl(s, KeyCode::Enter);
    lesson(s, 18);
    assert!(banner(s).contains("History · Commit files"));

    // History.
    ready(s, "Add a guide");
    select(s, '4', "Add a guide");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Escape);
    lesson(s, 19);
    ready(s, "farewell.md");
    char(s, 'A');
    ctrl(s, KeyCode::Enter);
    lesson(s, 20);
    ready(s, "Add greting");
    char(s, 'W');
    clear_field(s);
    paste(s, "Add greeting");
    ctrl(s, KeyCode::Enter);
    lesson(s, 21);
    ready(s, "Add docs");
    select(s, '4', "Add greeting");
    char(s, 'F');
    key(s, KeyCode::Enter);
    lesson(s, 22);

    // Branches.
    ready(s, "practice");
    char(s, '3');
    char(s, 'n');
    paste(s, "my-branch");
    ctrl(s, KeyCode::Enter);
    lesson(s, 23);
    select(s, '3', "feature");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Enter);
    lesson(s, 24);
    select(s, '3', "old-name");
    char(s, 'e');
    clear_field(s);
    paste(s, "new-name");
    ctrl(s, KeyCode::Enter);
    lesson(s, 25);
    select(s, '3', "feature");
    until(s, |s| char(s, 'u'), |s| view(s).contains("Set selected branch upstream"));
    ctrl(s, KeyCode::Enter);
    lesson(s, 26);
    select(s, '3', "done-topic");
    char(s, 'd');
    key(s, KeyCode::Enter);
    lesson(s, 27);

    // Merge with a conflict.
    select(s, '3', "topic");
    char(s, 'm');
    key(s, KeyCode::Enter);
    lesson(s, 28);
    select(s, '2', "greet.rs");
    char(s, 't');
    ctrl(s, KeyCode::Enter);
    idle(s);
    char(s, ' ');
    lesson(s, 29);
    char(s, 'C');
    key(s, KeyCode::Enter);
    lesson(s, 30);

    // Rebase: continue, skip, abort.
    select(s, '3', "main");
    char(s, 'R');
    key(s, KeyCode::Enter);
    lesson(s, 31);
    select(s, '2', "greet.txt");
    char(s, 't');
    ctrl(s, KeyCode::Enter);
    idle(s);
    char(s, ' ');
    idle(s);
    char(s, 'C');
    key(s, KeyCode::Enter);
    lesson(s, 32);
    char(s, 'S');
    key(s, KeyCode::Enter);
    lesson(s, 33);
    assert!(banner(s).contains("Rebase · Abort"));
    ready(s, "greet.txt");
    char(s, 'X');
    key(s, KeyCode::Enter);
    lesson(s, 34);

    // Reflog rescue.
    char(s, '4');
    char(s, ']');
    ready(s, "Precious work");
    select(s, '4', "Precious work");
    char(s, 'n');
    paste(s, "rescued");
    ctrl(s, KeyCode::Enter);
    lesson(s, 35);

    // Stash.
    ready(s, "draft.md");
    char(s, '5');
    char(s, 'n');
    paste(s, "Practice stash");
    ctrl(s, KeyCode::Enter);
    lesson(s, 36);
    char(s, '5');
    ready(s, "stash@{0}");
    char(s, 'p');
    key(s, KeyCode::Enter);
    lesson(s, 37);
    // Apply the older stash, a tracked edit; the reader titles the selected stash.
    char(s, '5');
    ready(s, "stash@{0}");
    key(s, KeyCode::Down);
    ready(s, "stash@{1}");
    char(s, 'a');
    key(s, KeyCode::Enter);
    lesson(s, 38);
    char(s, '5');
    char(s, 'd');
    key(s, KeyCode::Enter);
    lesson(s, 39);

    // Worktrees.
    ready(s, "practice");
    keys(s, "2]n");
    paste(s, "review");
    ctrl(s, KeyCode::Enter);
    lesson(s, 40);
    select(s, '2', "review");
    key(s, KeyCode::Enter);
    lesson(s, 41);
    assert!(s.root().ends_with("review"));
    keys(s, "2]");
    select(s, '2', "practice");
    key(s, KeyCode::Enter);
    wait(s, |s| s.root().ends_with("practice"));
    idle(s);
    keys(s, "2]");
    select(s, '2', "review");
    char(s, 'd');
    key(s, KeyCode::Enter);
    lesson(s, 42);

    // Submodules.
    keys(s, "2]]");
    select(s, '2', "lib");
    char(s, 'u');
    key(s, KeyCode::Enter);
    lesson(s, 43);
    keys(s, "2]]");
    select(s, '2', "lib");
    char(s, ' ');
    lesson(s, 44);

    // Tags and remotes.
    keys(s, "3]]");
    select(s, '3', "v0.1");
    char(s, 'd');
    key(s, KeyCode::Enter);
    lesson(s, 45);
    keys(s, "3]");
    // The `origin · 1 branch` row appears once `git remote -v` has been read; `busy()` does
    // not cover reads, and `f` on `origin/practice` before then has no remote to name.
    select(s, '3', "origin ·");
    char(s, 'f');
    key(s, KeyCode::Enter);
    lesson(s, 46);
    ready(s, "Ready to share");
    char(s, 'P');
    key(s, KeyCode::Enter);
    lesson(s, 47);

    // Discard, with its confirmation.
    ready(s, "debug: remove me");
    select(s, '2', "greet.rs");
    reader(s);
    seek(s, "debug: remove me");
    until(s, |s| char(s, 'd'), |s| view(s).contains("Discard unstaged change?"));
    key(s, KeyCode::Escape);
    idle(s);
    assert!(banner(s).contains("· 47/48 ·"), "Esc must cancel the discard");
    until(s, |s| char(s, 'd'), |s| view(s).contains("Discard unstaged change?"));
    key(s, KeyCode::Enter);
    lesson(s, 48);

    // Help.
    char(s, '?');
    key(s, KeyCode::Escape);
    lesson(s, LESSONS + 1);

    // Leaving restores the user's own workspace exactly; the practice tree is gone.
    char(s, 'q');
    assert_eq!(screen.root(), dir.path());
    assert_eq!(view(&mut screen), before);
    assert_eq!(
        screen.write_refusal.as_deref(),
        Some("Original workspace is untrusted")
    );
    assert!(!base.exists());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "untouched original work\n"
    );
}

#[test]
fn tutor_commands_skip_go_back_jump_and_reset_with_fresh_fixtures() {
    let dir = repo();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    let s = &mut screen;
    idle(s);
    command(s, "tutor next");
    assert!(view(s).contains("not running"), "{}", view(s));
    assert_eq!(s.root(), dir.path());

    command(s, "tutor git 7");
    lesson(s, 7);
    assert!(banner(s).contains("Git tutor · 7/48 · Changes · Split diff"));
    let base = practice_base(s);
    let seventh = s.root().to_owned();
    command(s, "tutor next");
    lesson(s, 8);
    assert!(banner(s).contains("Staging · Stage a file"));
    assert!(!seventh.exists(), "a left lesson's fixture is removed");
    command(s, "tutor prev");
    lesson(s, 7);
    let again = s.root().to_owned();
    command(s, "tutor reset");
    lesson(s, 7);
    assert_ne!(s.root(), again, "reset rebuilds the lesson");

    // Jumping into a lesson that continues another builds that state on its own.
    command(s, "tutor 28");
    lesson(s, 28);
    assert!(banner(s).contains("Merge · Resolve"));
    assert!(s.root().join(".git/MERGE_HEAD").is_file());
    command(s, "tutor 32");
    lesson(s, 32);
    assert!(s.root().join(".git/rebase-merge").is_dir());
    assert_eq!(
        git(s.root(), &["diff", "--name-only", "--diff-filter=U"]),
        "farewell.txt"
    );
    command(s, "tutor 41");
    lesson(s, 41);
    assert!(s.root().parent().unwrap().join("review/.git").is_file());

    command(s, "tutor 48");
    lesson(s, 48);
    command(s, "tutor next");
    lesson(s, LESSONS + 1);
    command(s, "tutor prev");
    lesson(s, 48);
    command(s, "tutor 99");
    assert!(view(s).contains("tutor git [N]"), "{}", view(s));
    assert!(banner(s).contains("· 48/48 ·"));
    command(s, "tutor git");
    assert!(view(s).contains("already running"), "{}", view(s));

    command(s, "tutor quit");
    assert_eq!(screen.root(), dir.path());
    assert!(banner(&mut screen).is_empty());
    assert!(!base.exists());
}
