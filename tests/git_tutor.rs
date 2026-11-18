//! The Git tutor, driven through real screen input: every lesson advances only after
//! the keys it teaches changed the practice repository or the screen, and quitting
//! restores the user's own workspace untouched.
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::GitScreen;
use dusty::ui::{Frame, KeyInputKind, UiEvent};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
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
    let deadline = Instant::now() + Duration::from_secs(120);
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
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        action(screen);
        if started(screen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "never started:\n{}",
            view(screen)
        );
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
    wait(screen, |screen| {
        !screen.busy() && view(screen).contains(needle)
    });
}
fn banner(screen: &mut GitScreen) -> String {
    let frame = screen.frame(WIDTH, HEIGHT);
    (0..HEIGHT)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("Git tutor ·"))
        .unwrap_or_default()
}
/// The Git track's lessons without the vim ones, as their banners name them.
fn outline() -> Vec<String> {
    GitScreen::tutor_outline(false, false)
}
fn lessons() -> usize {
    outline().len()
}
/// The one-based number of the lesson `Chapter · Title`.
fn number(name: &str) -> usize {
    outline()
        .iter()
        .position(|lesson| lesson == name)
        .unwrap_or_else(|| panic!("no lesson {name}"))
        + 1
}
/// Waits for lesson `number` (one-based); `lessons() + 1` is the finished tutor.
fn lesson(screen: &mut GitScreen, number: usize) {
    let total = lessons();
    let expected = if number > total {
        format!("all {total} lessons done")
    } else {
        format!("· {number}/{total} ·")
    };
    wait(screen, |screen| banner(screen).contains(&expected));
    idle(screen);
}
/// Waits for the lesson `Chapter · Title`.
fn at(screen: &mut GitScreen, name: &str) {
    let number = number(name);
    lesson(screen, number);
    assert!(
        banner(screen).contains(name),
        "lesson {number} is not {name}: {}",
        banner(screen)
    );
}
fn finished(screen: &mut GitScreen) {
    lesson(screen, lessons() + 1);
}
/// `:tutor next` until the lesson `name`: the lessons between have walkthroughs of their own.
fn skip_to(screen: &mut GitScreen, name: &str) {
    let number = number(name);
    for _ in 0..40 {
        if banner(screen).contains(&format!("· {number}/")) {
            break;
        }
        command(screen, "tutor next");
        idle(screen);
    }
    at(screen, name);
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
    at(s, "Panes · Focus panes");
    let base = practice_base(s);
    assert!(!base.starts_with(dir.path()) && !dir.path().starts_with(&base));
    assert!(banner(s).contains(&format!(
        "Git tutor · 1/{} · Panes · Focus panes",
        lessons()
    )));
    assert!(view(s).contains(":tutor next skips"));

    // Basics: panes, the rail, the keys sheet.
    ready(s, "greet.rs");
    keys(s, "345");
    assert!(
        banner(s).contains("Panes · Focus panes"),
        "advanced before returning to Changes"
    );
    char(s, '2');
    at(s, "Panes · Reader and Tab");
    ready(s, "greet.rs");
    char(s, '0');
    key(s, KeyCode::Tab);
    at(s, "Panes · Sub-tabs");
    ready(s, "greet.rs");
    keys(s, "3]]4]");
    at(s, "Panes · Hide the rail");
    idle(s);
    char(s, ' ');
    char(s, 'e');
    wait(s, |s| !view(s).contains("Stashes"));
    char(s, ' ');
    char(s, 'e');
    at(s, "Help · Keys");
    char(s, '?');
    key(s, KeyCode::Escape);

    // Changes.
    at(s, "Changes · Flat and tree");
    ready(s, "main.rs");
    char(s, 't');
    char(s, 'h');
    at(s, "Changes · All files");
    ready(s, "main.rs");
    char(s, 'F');
    ready(s, "README.md");
    char(s, 'F');
    at(s, "Changes · Filter");
    ready(s, "guide.md");
    char(s, '/');
    paste(s, "guide");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Escape);
    at(s, "Changes · Find");
    ready(s, "echo.txt");
    select(s, '2', "echo.txt");
    reader(s);
    seek(s, "start");
    until(s, |s| char(s, '/'), |s| view(s).contains("Find source"));
    paste(s, "hello");
    key(s, KeyCode::Enter);
    idle(s);
    char(s, 'n');
    at(s, "Changes · Split diff");
    ready(s, "Try the tree view.");
    reader(s);
    keys(s, "||");
    at(s, "Changes · Open in the editor");
    ready(s, "greet.rs");
    select(s, '2', "greet.rs");
    reader(s);
    seek(s, "Hello, editor!");
    char(s, 'e');

    // Staging.
    at(s, "Staging · Stage a file");
    ready(s, "notes.md");
    select(s, '2', "notes.md");
    char(s, 's');
    at(s, "Staging · Stage all");
    ready(s, "c.txt");
    char(s, 'a');
    idle(s);
    char(s, 'a');
    at(s, "Staging · Mark files");
    ready(s, "c.txt");
    select(s, '2', "a.txt");
    char(s, 'x');
    key(s, KeyCode::Down);
    char(s, 'x');
    char(s, 's');
    at(s, "Staging · Stage a hunk");
    ready(s, "Line 3 was rewritten");
    reader(s);
    seek(s, "Line 3 was rewritten");
    until(s, |s| char(s, 's'), |s| s.busy());
    at(s, "Staging · Stage a line");
    ready(s, "gamma");
    reader(s);
    seek(s, "beta");
    until(s, |s| char(s, 'a'), |s| s.busy());
    at(s, "Staging · Stage a range");
    ready(s, "epsilon");
    reader(s);
    seek(s, "beta");
    until(s, |s| char(s, 'V'), |s| view(s).contains("Range started"));
    key(s, KeyCode::Down);
    until(s, |s| char(s, 'a'), |s| s.busy());

    // Discard, each with its confirmation.
    at(s, "Discard · Discard a hunk");
    ready(s, "debug: remove me");
    select(s, '2', "greet.rs");
    reader(s);
    seek(s, "debug: remove me");
    until(
        s,
        |s| char(s, 'd'),
        |s| view(s).contains("Discard unstaged change?"),
    );
    key(s, KeyCode::Escape);
    idle(s);
    assert!(
        banner(s).contains("Discard · Discard a hunk"),
        "Esc must cancel the discard"
    );
    until(
        s,
        |s| char(s, 'd'),
        |s| view(s).contains("Discard unstaged change?"),
    );
    key(s, KeyCode::Enter);
    at(s, "Discard · Discard a line");
    ready(s, "gamma");
    select(s, '2', "list.txt");
    reader(s);
    seek(s, "gamma");
    until(
        s,
        |s| char(s, 'D'),
        |s| view(s).contains("Discard unstaged change?"),
    );
    key(s, KeyCode::Enter);
    at(s, "Discard · Discard files");
    ready(s, "c.txt");
    select(s, '2', "a.txt");
    char(s, 'x');
    key(s, KeyCode::Down);
    char(s, 'x');
    until(
        s,
        |s| char(s, 'D'),
        |s| view(s).contains("Discard changes in 2 files?"),
    );
    key(s, KeyCode::Enter);

    // Commits.
    at(s, "Commit · Subject and body");
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Practice commit");
    key(s, KeyCode::Enter);
    paste(s, "With a body line.");
    ctrl(s, KeyCode::Enter);
    at(s, "Commit · Sign-off");
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Signed practice commit");
    alt(s, 's');
    ctrl(s, KeyCode::Enter);
    at(s, "Commit · Skip hooks");
    ready(s, "greet.rs");
    char(s, 'c');
    paste(s, "Commit past the hook");
    ctrl(s, KeyCode::Enter);
    idle(s);
    assert!(
        banner(s).contains("Commit · Skip hooks"),
        "the practice hook must block a plain commit"
    );
    char(s, 'c');
    paste(s, "Commit past the hook");
    alt(s, 'n');
    ctrl(s, KeyCode::Enter);
    at(s, "Commit · Amend");
    ready(s, "farewell.md");
    char(s, 'c');
    alt(s, 'a');
    ctrl(s, KeyCode::Enter);

    // History.
    at(s, "History · Commit files");
    ready(s, "Add a guide");
    select(s, '4', "Add a guide");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Escape);
    at(s, "History · File diffs");
    ready(s, "Add the app");
    select(s, '4', "Add the app");
    key(s, KeyCode::Enter);
    ready(s, "◇ docs/guide.md");
    until(
        s,
        |s| char(s, 'J'),
        |s| view(s).contains("◇ src/app/main.rs"),
    );
    until(
        s,
        |s| key(s, KeyCode::Tab),
        |s| cursor(s).is_some_and(|(x, _)| x > RAIL),
    );
    idle(s);
    key(s, KeyCode::Escape);
    at(s, "History · Open a commit's line");
    ready(s, "Add the app");
    select(s, '4', "Add the app");
    key(s, KeyCode::Enter);
    ready(s, "◇ docs/guide.md");
    until(
        s,
        |s| key(s, KeyCode::Tab),
        |s| cursor(s).is_some_and(|(x, _)| x > RAIL),
    );
    until(
        s,
        |s| {
            char(s, 'j');
            char(s, 'e')
        },
        |s| banner(s).contains("History · Full message"),
    );
    at(s, "History · Full message");
    ready(s, "Explain the design");
    select(s, '4', "Explain the");
    // One `m`: a second one closes the message (it toggles), which completes the lesson
    // before its text arrived and starts Copy on Changes.
    char(s, 'm');
    ready(s, "so the keys you learn");
    idle(s);
    key(s, KeyCode::Escape);
    at(s, "History · Copy");
    ready(s, "Add a guide");
    // Selected again on each try: a status or History reload landing between the
    // selection and `y` must not leave the key on another pane.
    until(
        s,
        |s| {
            select(s, '4', "Add a guide");
            char(s, 'y');
        },
        |s| view(s).contains("Copy from"),
    );
    key(s, KeyCode::Enter);
    at(s, "History · Amend HEAD");
    ready(s, "farewell.md");
    char(s, 'A');
    ctrl(s, KeyCode::Enter);
    at(s, "History · Reword");
    ready(s, "Add greting");
    char(s, 'W');
    clear_field(s);
    paste(s, "Add greeting");
    ctrl(s, KeyCode::Enter);
    at(s, "History · Fixup");
    ready(s, "Add docs");
    select(s, '4', "Add greeting");
    char(s, 'F');
    key(s, KeyCode::Enter);

    // Stash.
    at(s, "Stash · Save");
    ready(s, "draft.md");
    char(s, '5');
    char(s, 'n');
    paste(s, "Practice stash");
    ctrl(s, KeyCode::Enter);
    at(s, "Stash · Pop");
    char(s, '5');
    ready(s, "stash@{0}");
    char(s, 'p');
    key(s, KeyCode::Enter);
    // Apply the older stash, a tracked edit; the reader titles the selected stash.
    at(s, "Stash · Apply");
    char(s, '5');
    ready(s, "stash@{0}");
    key(s, KeyCode::Down);
    ready(s, "stash@{1}");
    char(s, 'a');
    key(s, KeyCode::Enter);
    at(s, "Stash · Drop");
    char(s, '5');
    char(s, 'd');
    key(s, KeyCode::Enter);

    // Branches.
    at(s, "Branches · New branch");
    ready(s, "practice");
    char(s, '3');
    char(s, 'n');
    paste(s, "my-branch");
    ctrl(s, KeyCode::Enter);
    at(s, "Branches · Checkout");
    select(s, '3', "feature");
    key(s, KeyCode::Enter);
    key(s, KeyCode::Enter);
    at(s, "Branches · Rename");
    select(s, '3', "old-name");
    char(s, 'e');
    clear_field(s);
    paste(s, "new-name");
    ctrl(s, KeyCode::Enter);
    at(s, "Branches · Upstream");
    select(s, '3', "feature");
    until(
        s,
        |s| char(s, 'u'),
        |s| view(s).contains("Set selected branch upstream"),
    );
    ctrl(s, KeyCode::Enter);
    at(s, "Branches · Delete");
    select(s, '3', "done-topic");
    char(s, 'd');
    key(s, KeyCode::Enter);

    // Merge with a conflict.
    at(s, "Merge · Merge");
    select(s, '3', "topic");
    char(s, 'm');
    key(s, KeyCode::Enter);
    at(s, "Merge · Resolve");
    select(s, '2', "greet.rs");
    char(s, 't');
    ctrl(s, KeyCode::Enter);
    idle(s);
    char(s, 's');
    at(s, "Merge · Continue");
    char(s, 'C');
    key(s, KeyCode::Enter);

    // Rebase: continue, skip, abort.
    at(s, "Rebase · Rebase");
    select(s, '3', "main");
    char(s, 'R');
    key(s, KeyCode::Enter);
    at(s, "Rebase · Continue");
    select(s, '2', "greet.txt");
    char(s, 't');
    ctrl(s, KeyCode::Enter);
    idle(s);
    char(s, 's');
    idle(s);
    char(s, 'C');
    key(s, KeyCode::Enter);
    at(s, "Rebase · Skip");
    char(s, 'S');
    key(s, KeyCode::Enter);
    at(s, "Rebase · Abort");
    ready(s, "greet.txt");
    char(s, 'X');
    key(s, KeyCode::Enter);
    // The conflict chapters have their own walkthrough (`tests/git_conflicts.rs`).
    lesson(s, number("Rebase · Abort") + 1);
    skip_to(s, "Worktrees · New worktree");

    // Worktrees.
    ready(s, "practice");
    keys(s, "2]n");
    paste(s, "review");
    ctrl(s, KeyCode::Enter);
    at(s, "Worktrees · Open");
    select(s, '2', "review");
    key(s, KeyCode::Enter);
    wait(s, |s| s.root().ends_with("review"));
    at(s, "Worktrees · Remove");
    keys(s, "2]");
    select(s, '2', "practice");
    key(s, KeyCode::Enter);
    wait(s, |s| s.root().ends_with("practice"));
    idle(s);
    keys(s, "2]");
    select(s, '2', "review");
    char(s, 'd');
    key(s, KeyCode::Enter);

    // Rewrite history.
    at(s, "Rewrite history · Squash");
    select(s, '4', "Fix farewell");
    until(
        s,
        |s| char(s, 's'),
        |s| view(s).contains("Squash into the commit below"),
    );
    key(s, KeyCode::Enter);
    at(s, "Rewrite history · Fixup");
    select(s, '4', "Fix farewell");
    until(
        s,
        |s| char(s, 'f'),
        |s| view(s).contains("Fixup into the commit below"),
    );
    key(s, KeyCode::Enter);
    at(s, "Rewrite history · Drop");
    select(s, '4', "Add debug");
    until(s, |s| char(s, 'd'), |s| view(s).contains("Drop ·"));
    key(s, KeyCode::Enter);
    at(s, "Rewrite history · Move");
    select(s, '4', "First step");
    until(s, |s| char(s, 'J'), |s| view(s).contains("Move down ·"));
    key(s, KeyCode::Enter);
    at(s, "Rewrite history · Edit");
    select(s, '4', "Second step");
    until(
        s,
        |s| char(s, 'e'),
        |s| view(s).contains("Edit (stop the rebase here)"),
    );
    key(s, KeyCode::Enter);
    wait(s, |s| {
        s.root().join(".git/rebase-merge").is_dir() && !s.busy()
    });
    idle(s);
    char(s, 'C');
    key(s, KeyCode::Enter);
    at(s, "Rewrite history · Reword an older commit");
    select(s, '4', "Add greting");
    until(s, |s| char(s, 'W'), |s| view(s).contains("Reword ·"));
    clear_field(s);
    keys(s, "Add greeting");
    ctrl(s, KeyCode::Enter);
    at(s, "Rewrite history · Amend an older commit");
    select(s, '4', "Add farewell");
    until(
        s,
        |s| char(s, 'A'),
        |s| view(s).contains("Amend with the staged changes"),
    );
    key(s, KeyCode::Enter);

    // Undo and redo: each sheet shows the move, Enter confirms it.
    at(s, "Undo · Undo");
    until(
        s,
        |s| char(s, 'z'),
        |s| view(s).contains("Undo the last action?"),
    );
    assert!(view(s).contains("Commit too early"), "{}", view(s));
    key(s, KeyCode::Enter);
    at(s, "Undo · Redo");
    until(
        s,
        |s| char(s, 'Z'),
        |s| view(s).contains("Redo the undone action?"),
    );
    key(s, KeyCode::Enter);

    // Reflog: rescue, browse, detach, a worktree.
    at(s, "Reflog · Rescue");
    char(s, '4');
    char(s, ']');
    ready(s, "Precious work");
    select(s, '4', "Precious work");
    char(s, 'n');
    paste(s, "rescued");
    ctrl(s, KeyCode::Enter);
    for (name, action) in [
        ("Reflog · Browse", 0),
        ("Reflog · Detach", 1),
        ("Reflog · Worktree", 2),
    ] {
        at(s, name);
        char(s, '4');
        char(s, ']');
        ready(s, "Precious work");
        select(s, '4', "Precious work");
        match action {
            0 => {
                key(s, KeyCode::Enter);
                idle(s);
                key(s, KeyCode::Escape);
            }
            1 => {
                until(
                    s,
                    |s| char(s, 'o'),
                    |s| view(s).contains("Check out commit with detached HEAD?"),
                );
                key(s, KeyCode::Enter);
            }
            _ => {
                until(
                    s,
                    |s| char(s, 'w'),
                    |s| view(s).contains("Create detached worktree at commit"),
                );
                ctrl(s, KeyCode::Enter);
            }
        }
    }

    // Remotes and tags.
    at(s, "Remotes and tags · Fetch");
    keys(s, "3]");
    // The `origin · 1 branch` row appears once `git remote -v` has been read; `busy()` does
    // not cover reads, and `f` on `origin/practice` before then has no remote to name.
    select(s, '3', "origin ·");
    char(s, 'f');
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Track");
    keys(s, "3]");
    select(s, '3', "topic");
    until(
        s,
        |s| key(s, KeyCode::Enter),
        |s| view(s).contains("Create a local branch tracking"),
    );
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Pull");
    until(
        s,
        |s| char(s, 'p'),
        |s| view(s).contains("Pull origin/practice into practice?"),
    );
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Push");
    ready(s, "Ready to share");
    char(s, 'P');
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Inspect a tag");
    keys(s, "3]]");
    select(s, '3', "v1.0");
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Delete a tag");
    keys(s, "3]]");
    select(s, '3', "v0.1");
    char(s, 'd');
    key(s, KeyCode::Enter);
    at(s, "Remotes and tags · Push a tag");
    keys(s, "3]]");
    select(s, '3', "v1.0");
    until(s, |s| char(s, 'P'), |s| view(s).contains("Push tag v1.0?"));
    key(s, KeyCode::Enter);

    // Submodules.
    at(s, "Submodules · Init");
    keys(s, "2]]");
    select(s, '2', "lib");
    char(s, 'u');
    key(s, KeyCode::Enter);
    at(s, "Submodules · Stage pointer");
    keys(s, "2]]");
    select(s, '2', "lib");
    char(s, 's');
    finished(s);

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

/// The tracks run basics → staging → commits → branches → history rewriting → remotes, and
/// the forge track pull requests → reviews → issues → Actions → notifications.
#[test]
fn the_tracks_teach_in_order() {
    let git = outline();
    let chapter = |name: &str| {
        git.iter()
            .position(|lesson| lesson.starts_with(&format!("{name} · ")))
            .unwrap_or_else(|| panic!("no chapter {name}: {git:?}"))
    };
    let order = [
        "Panes",
        "Staging",
        "Commit",
        "Branches",
        "Rewrite history",
        "Remotes and tags",
    ];
    for pair in order.windows(2) {
        assert!(
            chapter(pair[0]) < chapter(pair[1]),
            "{} before {}",
            pair[0],
            pair[1]
        );
    }
    let forge = GitScreen::tutor_outline(true, false);
    let chapter = |name: &str| {
        forge
            .iter()
            .position(|lesson| lesson.starts_with(&format!("{name} · ")))
            .unwrap_or_else(|| panic!("no chapter {name}: {forge:?}"))
    };
    let order = [
        "Pull requests",
        "Pull request",
        "Issues",
        "Actions",
        "Notifications",
    ];
    for pair in order.windows(2) {
        assert!(
            chapter(pair[0]) < chapter(pair[1]),
            "{} before {}",
            pair[0],
            pair[1]
        );
    }
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

    let split = number("Changes · Split diff");
    command(s, &format!("tutor git {split}"));
    at(s, "Changes · Split diff");
    assert!(banner(s).contains(&format!(
        "Git tutor · {split}/{} · Changes · Split diff",
        lessons()
    )));
    let base = practice_base(s);
    let seventh = s.root().to_owned();
    command(s, "tutor next");
    // The lesson after Split diff, whichever it is.
    at(s, &outline()[split]);
    assert!(!seventh.exists(), "a left lesson's fixture is removed");
    command(s, "tutor prev");
    at(s, "Changes · Split diff");
    let again = s.root().to_owned();
    command(s, "tutor reset");
    at(s, "Changes · Split diff");
    assert_ne!(s.root(), again, "reset rebuilds the lesson");

    // Jumping into a lesson that continues another builds that state on its own.
    command(s, &format!("tutor {}", number("Merge · Resolve")));
    at(s, "Merge · Resolve");
    assert!(s.root().join(".git/MERGE_HEAD").is_file());
    command(s, &format!("tutor {}", number("Rebase · Skip")));
    at(s, "Rebase · Skip");
    assert!(s.root().join(".git/rebase-merge").is_dir());
    assert_eq!(
        git(s.root(), &["diff", "--name-only", "--diff-filter=U"]),
        "farewell.txt"
    );
    command(s, &format!("tutor {}", number("Worktrees · Open")));
    at(s, "Worktrees · Open");
    assert!(s.root().parent().unwrap().join("review/.git").is_file());

    let last = lessons();
    command(s, &format!("tutor {last}"));
    lesson(s, last);
    command(s, "tutor next");
    finished(s);
    command(s, "tutor prev");
    lesson(s, last);
    command(s, "tutor 999");
    assert!(view(s).contains("tutor git [N]"), "{}", view(s));
    assert!(banner(s).contains(&format!("· {last}/{last} ·")));
    command(s, "tutor git");
    assert!(view(s).contains("already running"), "{}", view(s));

    command(s, "tutor quit");
    assert_eq!(screen.root(), dir.path());
    assert!(banner(&mut screen).is_empty());
    assert!(!base.exists());
}

#[test]
fn the_vim_lessons_teach_the_modes_and_appear_only_when_vim_is_on() {
    let vim = GitScreen::tutor_outline(false, true);
    let total = vim.len();
    assert_eq!(total, lessons() + 3, "{vim:?}");
    let first = lessons() + 1;
    let at = |screen: &mut GitScreen, number: usize| {
        let expected = if number > total {
            format!("all {total} lessons done")
        } else {
            format!("· {number}/{total} · {}", vim[number - 1])
        };
        wait(screen, |screen| banner(screen).contains(&expected));
        idle(screen);
    };
    let dir = repo();
    // Not modal: the track ends before the vim lessons.
    let mut plain = GitScreen::open(dir.path()).unwrap();
    idle(&mut plain);
    command(&mut plain, &format!("tutor git {first}"));
    assert!(
        view(&mut plain).contains("tutor git [N]"),
        "{}",
        view(&mut plain)
    );

    let mut screen = GitScreen::open(dir.path()).unwrap();
    screen.configure_input(&dusty::config::load_bundled_config().config);
    let s = &mut screen;
    idle(s);
    command(s, &format!("tutor git {first}"));
    at(s, first);
    assert!(
        banner(s).contains(&format!(
            "Git tutor · {first}/{total} · Vim mode · Mode chip"
        )),
        "{}",
        banner(s)
    );

    // The chip: V in the reader is VISUAL, Esc is NORMAL again.
    select(s, '2', "list.txt");
    reader(s);
    seek(s, "gamma");
    char(s, 'V');
    assert_eq!(s.input_mode(), Some(dusty::git::screen::InputMode::Visual));
    assert!(
        banner(s).contains("Vim mode · Mode chip"),
        "advanced before Esc"
    );
    key(s, KeyCode::Escape);
    at(s, first + 1);

    // The commit box: INSERT, Esc to NORMAL, i to type again, Ctrl+Enter commits.
    ready(s, "greet.rs");
    until(
        s,
        |s| char(s, 'c'),
        |s| s.input_mode() == Some(dusty::git::screen::InputMode::Insert),
    );
    keys(s, "Learn the modes");
    key(s, KeyCode::Escape);
    assert_eq!(s.input_mode(), Some(dusty::git::screen::InputMode::Normal));
    keys(s, "i!");
    assert_eq!(s.input_mode(), Some(dusty::git::screen::InputMode::Insert));
    ctrl(s, KeyCode::Enter);
    at(s, first + 2);

    // The keys sheet's Vim group; the leader's which-key hint names its chords.
    char(s, ' ');
    assert!(view(s).contains("toggle sidebar"), "{}", view(s));
    key(s, KeyCode::Escape);
    char(s, '?');
    assert!(view(s).contains("Leader · which-key"), "{}", view(s));
    key(s, KeyCode::Escape);
    at(s, total + 1);
    command(s, "tutor quit");
    assert_eq!(screen.root(), dir.path());
}

#[test]
fn the_lessons_spell_the_keys_of_the_preset_in_force() {
    let dir = repo();
    let mut config = dusty::config::load_bundled_config().config;
    config.ui.git.keys = dusty::config::GitKeys::Lazygit;
    let mut screen = GitScreen::open(dir.path()).unwrap();
    screen.configure_input(&config);
    let s = &mut screen;
    idle(s);
    for (name, expected) in [
        (
            "Staging · Stage a file",
            "press Space to stage the whole file",
        ),
        (
            "Branches · Checkout",
            "select feature, Space, then Enter again",
        ),
        ("Merge · Merge", "select topic, M, then Enter merges it"),
        ("Rebase · Rebase", "select main, r, then Enter replays"),
        ("Stash · Pop", "select the stash, g, then Enter"),
        ("Undo · Redo", "C-z shows the undone commit"),
        ("Rewrite history · Move", "C-j moves it below"),
        (
            "Rewrite history · Reword an older commit",
            "'Add greting'; r, fix the typo",
        ),
    ] {
        let number = number(name);
        command(s, &format!("tutor git {number}"));
        wait(s, |s| banner(s).contains(&format!("· {number}/")));
        let shown = view(s);
        assert!(shown.contains(expected), "{name}: {expected}:\n{shown}");
        assert!(!shown.contains("{Files:"), "{shown}");
        assert!(!shown.contains("{Commits:"), "{shown}");
    }
    command(s, "tutor quit");

    // The default keys read as written.
    let mut plain = GitScreen::open(dir.path()).unwrap();
    idle(&mut plain);
    let merge = number("Merge · Merge");
    command(&mut plain, &format!("tutor git {merge}"));
    wait(&mut plain, |s| banner(s).contains(&format!("· {merge}/")));
    assert!(
        view(&mut plain).contains("select topic, m, then Enter merges it"),
        "{}",
        view(&mut plain)
    );
    command(&mut plain, "tutor quit");
}
