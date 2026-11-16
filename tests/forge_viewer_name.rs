#![cfg(unix)]
//! The signed-in account in the fake forges is a person of the world (`rowan` on GitHub and
//! GitLab, `maya` on Gitea and Forgejo), never a placeholder: no forge screen shows `tester`,
//! and the review sheet names the real viewer ("Review by rowan").
//!
//! Each world runs in its own child process, as in `forge_features`.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_world::{ALL, Forge, World};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_VIEWER_CHILD";
const QUIET: Duration = Duration::from_millis(600);
const DEADLINE: Duration = Duration::from_secs(60);

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
}

/// The frame once nothing loads and it stays unchanged for `QUIET`.
fn settle(screen: &mut GitScreen, what: &str) -> String {
    let start = Instant::now();
    let mut last = String::new();
    let mut since = Instant::now();
    loop {
        screen.poll();
        let frame = view(screen);
        if frame != last || frame.contains("Loading") || frame.contains("loading details") {
            last = frame;
            since = Instant::now();
        } else if since.elapsed() >= QUIET {
            return last;
        }
        assert!(start.elapsed() < DEADLINE, "{what} never settled:\n{last}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_forge_screen_shows_a_placeholder_viewer() {
    let Ok(name) = std::env::var(CHILD) else {
        for forge in ALL {
            let world = World::new(forge);
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["no_forge_screen_shows_a_placeholder_viewer", "--exact", "--nocapture", "--test-threads", "1"])
                .env(CHILD, forge.name())
                .env("DUSTY_VIEWER_REPO", &world.repo);
            world.configure(&mut command);
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{} child:\n{stdout}\n{}",
                forge.name(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let forge = ALL.into_iter().find(|forge| forge.name() == name).unwrap();
    let repo = PathBuf::from(std::env::var_os("DUSTY_VIEWER_REPO").unwrap());
    let mut screen = GitScreen::open(&repo).unwrap();
    let mut seen = Vec::new();
    for page in ["prs", "issues", "actions", "notifications"] {
        screen.open_target(page, None);
        seen.push(settle(&mut screen, page));
    }
    screen.open_target("issues", Some(forge.issue()));
    seen.push(settle(&mut screen, "the issue"));
    screen.open_target("prs", Some(forge.pull()));
    seen.push(settle(&mut screen, "the pull request"));
    for tab in ["Commits", "Checks", "Files changed"] {
        key(&mut screen, KeyCode::Char('L'));
        seen.push(settle(&mut screen, tab));
    }
    key(&mut screen, KeyCode::Char('L'));
    settle(&mut screen, "Conversation");
    // The review sheet names who submits it.
    key(&mut screen, KeyCode::Char('r'));
    let sheet = settle(&mut screen, "the review sheet");
    let viewer = if matches!(forge, Forge::Gitea | Forge::Forgejo) { "maya" } else { "rowan" };
    assert!(sheet.contains(&format!("Review by {viewer}")), "{}:\n{sheet}", forge.name());
    seen.push(sheet);
    key(&mut screen, KeyCode::Escape);
    for frame in seen {
        assert!(!frame.contains("tester"), "{} shows a placeholder viewer:\n{frame}", forge.name());
    }
}
