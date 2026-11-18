//! A repository with no commits (`git init`) is a normal state: the Git tab and every forge
//! tab open on it without showing Git's stderr (`fatal: bad revision 'HEAD'`).
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::GitScreen;
use dusty::ui::{KeyInputKind, UiEvent};
use std::process::Command;
use std::time::{Duration, Instant};

const WIDTH: u16 = 160;
const HEIGHT: u16 = 44;

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code,
            modifiers: Modifiers::default(),
        },
        kind: KeyInputKind::Press,
    });
}
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(WIDTH, HEIGHT);
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
/// Polls until `ready`, then a little longer, so late job errors would show too.
fn settle(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready(screen) {
        assert!(Instant::now() < deadline, "timed out:\n{}", view(screen));
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..30 {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn no_stderr(screen: &mut GitScreen, page: &str) {
    let shown = view(screen);
    for raw in [
        "fatal:",
        "bad revision",
        "unknown revision",
        "ambiguous argument",
    ] {
        assert!(
            !shown.contains(raw),
            "{page} shows Git's stderr ({raw}):\n{shown}"
        );
    }
}

#[test]
fn a_repository_with_no_commits_opens_every_tab_without_git_errors() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .arg(dir.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(dir.path().join("first.txt"), "The first file\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    let s = &mut screen;
    settle(s, |s| !s.busy() && view(s).contains("first.txt"));
    no_stderr(s, "Git");

    // History is empty and says how to start.
    key(s, KeyCode::Char('4'));
    settle(s, |s| {
        !s.busy() && view(s).contains("No commits yet · c to make the first commit")
    });
    no_stderr(s, "History");
    key(s, KeyCode::Char('2'));

    // Every forge tab, then round to Git again.
    for page in ["Pull requests", "Issues", "Actions", "Notifications"] {
        key(s, KeyCode::Char('>'));
        settle(s, |s| !s.busy());
        no_stderr(s, page);
    }
    key(s, KeyCode::Char('>'));
    settle(s, |s| !s.busy() && view(s).contains("first.txt"));
    no_stderr(s, "Git, again");
}
