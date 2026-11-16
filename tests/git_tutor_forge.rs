//! The forge tutor (`:tutor forge`), driven through real screen input against its own
//! practice forge: every lesson advances only after the keys it teaches changed the forge
//! tabs or reached the practice forge, and quitting leaves the forge sandbox.
//!
//! One test only: the forge sandbox is process-wide. `scripts/.tutor-run.sh` runs it with
//! decoy credentials and a decoy API base in the environment; the lessons still pass because
//! the sandbox never reads them.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const WIDTH: u16 = 190;
const HEIGHT: u16 = 48;

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Tutor Test"]);
    git(dir.path(), &["config", "user.email", "test@example.invalid"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("file.txt"), "my own work\n").unwrap();
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
fn ctrl_enter(screen: &mut GitScreen) {
    press(
        screen,
        KeyCode::Enter,
        Modifiers {
            control: true,
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
fn wait(screen: &mut GitScreen, what: &str, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}:\n{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn shows(screen: &mut GitScreen, needle: &str) {
    wait(screen, needle, |screen| view(screen).contains(needle));
}
fn banner(screen: &mut GitScreen) -> String {
    let frame = screen.frame(WIDTH, HEIGHT);
    (0..HEIGHT)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("Forge tutor ·"))
        .unwrap_or_default()
}
/// The forge track's lessons, as "Chapter · Title".
fn outline() -> Vec<String> {
    GitScreen::tutor_outline(true, false)
}
fn total() -> usize {
    outline().len()
}
/// The one-based number of the lesson named "Chapter · Title"; `None` is past the last one.
fn number(name: Option<&str>) -> usize {
    match name {
        None => total() + 1,
        Some(name) => {
            outline().iter().position(|lesson| lesson == name).unwrap_or_else(|| panic!("no lesson {name}")) + 1
        }
    }
}
fn on_number(screen: &mut GitScreen, number: usize) -> bool {
    let total = total();
    let expected = if number > total {
        format!("all {total} lessons done")
    } else {
        format!("· {number}/{total} ·")
    };
    banner(screen).contains(&expected)
}
fn on_lesson(screen: &mut GitScreen, name: &str) -> bool {
    on_number(screen, number(Some(name)))
}
/// Waits for the lesson named "Chapter · Title" and checks its banner names its title.
fn lesson(screen: &mut GitScreen, name: &str) {
    wait(screen, name, |screen| on_lesson(screen, name));
    let title = name.split(" · ").last().unwrap();
    assert!(banner(screen).contains(title), "{}", view(screen));
    idle(screen);
}
fn idle(screen: &mut GitScreen) {
    wait(screen, "idle", |screen| !screen.busy());
}
/// Repeats `action` (the screen may refuse it while still loading) until lesson `next` starts.
fn until(screen: &mut GitScreen, next: &str, action: impl Fn(&mut GitScreen)) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        action(screen);
        for _ in 0..20 {
            screen.poll();
            if on_lesson(screen, next) {
                idle(screen);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Instant::now() < deadline, "{next} never started:\n{}", view(screen));
    }
}
fn ctrl(screen: &mut GitScreen, letter: char) {
    press(
        screen,
        KeyCode::Char(letter),
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
}
/// Types `text` one key at a time, as into a search field that takes keys, not pastes.
fn typed(screen: &mut GitScreen, text: &str) {
    for letter in text.chars() {
        char(screen, letter);
    }
}
/// Tabs the reader's cursor onto the control labelled `label`.
fn focus_control(screen: &mut GitScreen, label: &str) -> usize {
    for tabs in 0..60 {
        let frame = screen.frame(WIDTH, HEIGHT);
        if let Some((_, y, _)) = frame.cursor
            && frame.grid.row_text(y).contains(label)
        {
            return tabs;
        }
        key(screen, KeyCode::Tab);
    }
    panic!("{label} is not keyboard-reachable:\n{}", view(screen));
}

#[test]
fn every_forge_lesson_advances_on_real_input_against_the_practice_forge() {
    let dir = repo();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    let s = &mut screen;
    shows(s, "Initial commit");
    assert!(!dust::forge::sandbox::active());
    char(s, '?');
    assert!(view(s).contains(":tutor forge"), "{}", view(s));
    key(s, KeyCode::Escape);

    command(s, "tutor forge 999");
    assert!(view(s).contains("tutor forge [N]"), "{}", view(s));
    command(s, "tutor forge");
    lesson(s, "Pull requests · Open the tab");
    assert!(dust::forge::sandbox::active());
    assert!(banner(s).contains(&format!("1/{}", total())));
    assert!(view(s).contains("Practice forge on this computer"), "{}", view(s));
    command(s, "tutor forge");
    assert!(view(s).contains("already running"), "{}", view(s));

    // Pull requests: the tab, its sections, the filter and its menus, saved views.
    char(s, '>');
    lesson(s, "Pull requests · Sections");
    // The finished lesson is one check mark on the banner, never a footer notice.
    assert!(banner(s).contains("✓ Open the tab"), "{}", view(s));
    assert!(!view(s).contains("Open the tab done"), "{}", view(s));
    // The banner stays on the forge tabs, above their footer.
    assert!(view(s).contains("] cycles the list's sections"), "{}", view(s));
    shows(s, "Clamp exponential backoff");
    until(s, "Pull requests · Filter", |s| char(s, ']'));
    char(s, '/');
    shows(s, "Search");
    paste(s, "retry");
    ctrl_enter(s);
    lesson(s, "Pull requests · Open · Closed");
    char(s, 's');
    idle(s);
    char(s, 's');
    lesson(s, "Pull requests · Menus");
    char(s, 'F');
    shows(s, "Filter pull requests");
    key(s, KeyCode::Down);
    ctrl_enter(s);
    lesson(s, "Pull requests · Save a section");
    char(s, 'S');
    shows(s, "Save view");
    paste(s, "Mine");
    ctrl_enter(s);
    idle(s);
    char(s, 'D');
    shows(s, "Delete saved view");
    key(s, KeyCode::Tab);
    key(s, KeyCode::Down);
    ctrl_enter(s);
    lesson(s, "Pull requests · Views");
    char(s, 'v');
    shows(s, "Repository views");
    key(s, KeyCode::Down);
    ctrl_enter(s);
    lesson(s, "Pull requests · Inbox");
    char(s, 'i');
    idle(s);
    char(s, 'i');
    lesson(s, "Pull requests · Refresh");
    shows(s, "Clamp exponential backoff");
    until(s, "Pull requests · Browser", |s| char(s, 'R'));
    char(s, 'o');
    shows(s, "in your browser?");
    ctrl_enter(s);
    lesson(s, "Pull request · Open one");

    // A pull request: the reader, its tabs and links, find, writes, Files, a review.
    shows(s, "Export completed task history");
    char(s, 'j');
    key(s, KeyCode::Enter);
    lesson(s, "Pull request · List and reader");
    char(s, '2');
    char(s, '3');
    lesson(s, "Pull request · Tabs");
    until(s, "Pull request · Links", |s| char(s, ']'));
    key(s, KeyCode::Tab);
    lesson(s, "Pull request · Find");
    ctrl(s, 'f');
    shows(s, "Find detail");
    paste(s, "Python");
    key(s, KeyCode::Enter);
    lesson(s, "Pull request · Hide the list");
    char(s, ' ');
    char(s, 'e');
    char(s, ' ');
    char(s, 'e');
    lesson(s, "Pull request · Comment");
    char(s, 'C');
    shows(s, "Comment as");
    paste(s, "Thanks, this reads well.");
    ctrl_enter(s);
    lesson(s, "Pull request · Edit");
    char(s, 'e');
    shows(s, "Edit title and description");
    paste(s, " (v2)");
    ctrl_enter(s);
    lesson(s, "Pull request · Labels");
    char(s, 'l');
    shows(s, "Edit metadata");
    paste(s, "bug");
    ctrl_enter(s);
    idle(s);
    char(s, 'a');
    shows(s, "Edit metadata");
    paste(s, "rowan");
    ctrl_enter(s);
    lesson(s, "Pull request · Close");
    char(s, 'x');
    shows(s, "Close pull request");
    ctrl_enter(s);
    idle(s);
    char(s, 'X');
    shows(s, "Reopen pull request");
    ctrl_enter(s);
    lesson(s, "Pull request · Files");
    char(s, 'd');
    lesson(s, "Pull request · Choose a file");
    char(s, 'f');
    shows(s, "Choose changed file");
    key(s, KeyCode::Down);
    ctrl_enter(s);
    lesson(s, "Pull request · Wrap");
    char(s, 'w');
    char(s, 'w');
    lesson(s, "Pull request · Viewed");
    char(s, 'v');
    lesson(s, "Pull request · Open in the editor");
    focus_control(s, "def test_delay_is_capped");
    char(s, 'e');
    lesson(s, "Pull request · Line comment");
    focus_control(s, "def test_delay_is_capped");
    key(s, KeyCode::Enter);
    shows(s, "Comment on");
    paste(s, "Name it for what it checks.");
    ctrl_enter(s);
    lesson(s, "Pull request · Range comment");
    focus_control(s, "retry_delay(3) == 8");
    char(s, 'V');
    char(s, 'j');
    char(s, 'C');
    shows(s, "Comment on");
    paste(s, "Both asserts, together.");
    ctrl_enter(s);
    lesson(s, "Pull request · Review");
    until(s, "Pull request · Check out", |s| {
        if !view(s).contains("Review #") {
            char(s, 'r');
            return;
        }
        key(s, KeyCode::Down);
        key(s, KeyCode::Tab);
        paste(s, "Reads well; thanks.");
        ctrl_enter(s);
    });
    shows(s, "Clamp exponential backoff");
    char(s, 'c');
    shows(s, "Check out pull request in this repository");
    ctrl_enter(s);
    lesson(s, "Pull request · Worktree");
    char(s, 'w');
    shows(s, "separate worktree");
    ctrl_enter(s);
    lesson(s, "Pull request · From a branch");
    shows(s, "⎇ fix/backoff");
    char(s, '3');
    until(s, "Pull request · Create", |s| char(s, 'o'));
    shows(s, "Clamp exponential backoff");
    char(s, 'n');
    shows(s, "Create pull request");
    paste(s, "Add a quiet mode");
    until(s, "Merge · Merge", |s| ctrl_enter(s));

    // Merge an approved pull request.
    shows(s, "Introduce a quiet status indicator");
    char(s, 'm');
    shows(s, "Merge pull request");
    let tabs = focus_control(s, "Merge pull request");
    eprintln!("merge control reached after {tabs} Tab presses");
    key(s, KeyCode::Enter);
    shows(s, "Method");
    ctrl_enter(s);
    lesson(s, "Issues · Open one");

    // Issues: open, comment, edit, labels, assignees, milestone, close, a new one.
    char(s, '>');
    char(s, '>');
    shows(s, "Preserve scroll position in long logs");
    key(s, KeyCode::Enter);
    lesson(s, "Issues · Comment");
    char(s, 'C');
    shows(s, "Comment as");
    paste(s, "I can reproduce this.");
    ctrl_enter(s);
    lesson(s, "Issues · Edit");
    char(s, 'e');
    shows(s, "Edit title and description");
    paste(s, " on refresh");
    ctrl_enter(s);
    lesson(s, "Issues · Labels");
    char(s, 'l');
    shows(s, "Edit metadata");
    key(s, KeyCode::End);
    paste(s, ",native");
    ctrl_enter(s);
    lesson(s, "Issues · Assignees");
    char(s, 'a');
    shows(s, "Edit metadata");
    paste(s, "rowan");
    ctrl_enter(s);
    lesson(s, "Issues · Milestone");
    char(s, 'm');
    shows(s, "Edit metadata");
    paste(s, "v1");
    ctrl_enter(s);
    lesson(s, "Issues · Close");
    char(s, 'x');
    shows(s, "Close issue #");
    ctrl_enter(s);
    idle(s);
    char(s, 'X');
    shows(s, "Reopen issue #");
    ctrl_enter(s);
    lesson(s, "Issues · New issue");
    key(s, KeyCode::Escape);
    char(s, 'n');
    shows(s, "Create issue");
    paste(s, "Practice issue");
    ctrl_enter(s);
    lesson(s, "Actions · Run history");

    // Actions: the run history, a run, its log, the run list's filters, the run writes.
    shows(s, "Exercise the retry-limit regression check");
    until(s, "Actions · Open a run", |s| char(s, ']'));
    key(s, KeyCode::Enter);
    lesson(s, "Actions · Panels");
    char(s, '1');
    char(s, '2');
    char(s, '3');
    lesson(s, "Actions · Errors");
    until(s, "Actions · Full log", |s| char(s, 'n'));
    char(s, 'f');
    char(s, 'f');
    lesson(s, "Actions · Wrap");
    char(s, 'w');
    char(s, 'w');
    lesson(s, "Actions · Pan");
    char(s, 'w');
    char(s, 'L');
    char(s, 'H');
    lesson(s, "Actions · Find in the log");
    char(s, '/');
    typed(s, "test_regression.py");
    key(s, KeyCode::Enter);
    lesson(s, "Actions · Open file:line");
    key(s, KeyCode::Enter);
    lesson(s, "Actions · Save the log");
    ctrl(s, 's');
    shows(s, "Save job log");
    ctrl_enter(s);
    lesson(s, "Actions · Zoom");
    char(s, 'z');
    until(s, "Actions · Left column", |s| key(s, KeyCode::Escape));
    char(s, ' ');
    char(s, 'e');
    char(s, ' ');
    char(s, 'e');
    lesson(s, "Actions · Filter runs");
    char(s, '2');
    char(s, '[');
    char(s, '/');
    typed(s, "Release");
    key(s, KeyCode::Enter);
    lesson(s, "Actions · Attempts");
    key(s, KeyCode::Enter);
    until(s, "Actions · Narrow", |s| {
        // The list's own heading: the lesson text says "Attempt 1" too.
        if view(s).contains("Attempts · Enter shows one · Esc closes") {
            key(s, KeyCode::Down);
            key(s, KeyCode::Enter);
        } else {
            char(s, 'a');
        }
    });
    char(s, '2');
    char(s, 'F');
    key(s, KeyCode::Down);
    key(s, KeyCode::Enter);
    lesson(s, "Actions · Run workflow");
    char(s, 'd');
    shows(s, "Run ");
    until(s, "Actions · Refresh", |s| ctrl_enter(s));
    shows(s, "Exercise the retry-limit regression check");
    until(s, "Actions · Cancel", |s| char(s, 'R'));
    until(s, "Actions · Approve", |s| {
        char(s, 'x');
        ctrl_enter(s);
    });
    char(s, 'j');
    until(s, "Actions · Rerun", |s| {
        char(s, 'A');
        ctrl_enter(s);
    });
    char(s, ']');
    until(s, "Actions · Artifacts", |s| {
        if view(s).contains("Failed jobs only") {
            ctrl_enter(s);
        } else {
            char(s, 'r');
        }
    });
    key(s, KeyCode::Enter);
    until(s, "Notifications · Inbox", |s| {
        let shown = view(s);
        if shown.contains("Download artifact") {
            ctrl_enter(s);
        } else if shown.contains("junit.xml") || shown.contains("coverage.json") {
            char(s, 's');
        } else {
            char(s, 'G');
            key(s, KeyCode::Enter);
        }
    });

    // Notifications: the inbox, read, unread, filter, sidebar, save, done, unsubscribe.
    char(s, '<');
    lesson(s, "Notifications · Mark read");
    shows(s, "Clamp exponential backoff");
    until(s, "Notifications · Unread", |s| char(s, 'm'));
    char(s, ']');
    idle(s);
    char(s, '[');
    lesson(s, "Notifications · Filter");
    char(s, '/');
    typed(s, "queue");
    key(s, KeyCode::Enter);
    lesson(s, "Notifications · Sidebar");
    key(s, KeyCode::Escape);
    key(s, KeyCode::Tab);
    key(s, KeyCode::Tab);
    lesson(s, "Notifications · Save");
    char(s, 's');
    shows(s, "✓ Saved");
    char(s, 's');
    lesson(s, "Notifications · Done");
    char(s, 'e');
    lesson(s, "Notifications · Unsubscribe");
    char(s, 'u');
    lesson(s, "Notifications · Browser");
    char(s, 'o');
    shows(s, "in your browser?");
    ctrl_enter(s);
    lesson(s, "Notifications · Open");
    key(s, KeyCode::Enter);
    lesson(s, "Sign in · Accounts · how to sign in [@]");

    // Signing in: anonymous and read-only, then a practice token.
    shows(s, "Clamp exponential backoff");
    char(s, '@');
    shows(s, "Accounts");
    key(s, KeyCode::Escape);
    lesson(s, "Sign in · Commenting while signed out");
    until(s, "Sign in · Sign in", |s| char(s, 'C'));
    char(s, '@');
    shows(s, "Accounts");
    char(s, 'r');
    wait(s, "the finished tutor", |s| on_number(s, number(None)));
    assert!(view(s).contains("You practiced every forge lesson"), "{}", view(s));

    // `q` on a forge tab leaves the tutor, the practice forge and the sandbox.
    char(s, 'q');
    wait(s, "the user's workspace", |s| !s.in_tutor());
    assert!(!dust::forge::sandbox::active());
    assert_eq!(s.root().canonicalize().unwrap(), dir.path().canonicalize().unwrap());
    shows(s, "Initial commit");

    // Jumping into a lesson builds its own start; quitting from it leaves the sandbox too.
    command(s, &format!("tutor forge {}", number(Some("Actions · Zoom"))));
    lesson(s, "Actions · Zoom");
    shows(s, "Exercise the retry-limit regression check");
    // `:tutor forge N` while it runs jumps, as `:tutor N` does.
    command(s, &format!("tutor forge {}", number(Some("Sign in · Accounts · how to sign in [@]"))));
    lesson(s, "Sign in · Accounts · how to sign in [@]");
    // A jump finishes nothing.
    assert!(!banner(s).contains('✓'), "{}", view(s));
    command(s, "tutor quit");
    wait(s, "the user's workspace", |s| !s.in_tutor());
    assert!(!dust::forge::sandbox::active());
}
