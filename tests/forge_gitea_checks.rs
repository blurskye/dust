#![cfg(unix)]
//! Gitea's and Forgejo's Checks tab reads the job's steps through the run's API id, not the
//! number in its web link (`…/actions/runs/6/jobs/0` is run 4407): at every width the
//! selected job shows its steps, never "Steps and log unavailable".
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
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_GITEA_CHECKS_CHILD";
const QUIET: Duration = Duration::from_millis(600);
const DEADLINE: Duration = Duration::from_secs(60);

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn view(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    (0..height).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
}

/// The frame at this size once nothing loads and it stays unchanged for `QUIET`.
fn settle(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let start = Instant::now();
    let mut last = String::new();
    let mut since = Instant::now();
    loop {
        screen.poll();
        let frame = view(screen, width, height);
        if frame != last || frame.contains("Loading") || frame.contains("loading details") {
            last = frame;
            since = Instant::now();
        } else if since.elapsed() >= QUIET {
            return last;
        }
        assert!(start.elapsed() < DEADLINE, "Checks never settled at {width}x{height}:\n{last}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs `test` once per world in a child process; true in the parent (which then returns).
fn in_parent(test: &str) -> bool {
    if std::env::var(CHILD).is_ok() {
        return false;
    }
    for forge in [Forge::Gitea, Forge::Forgejo] {
        let world = World::with_state(forge, json!({ "conversation": true }));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([test, "--exact", "--nocapture", "--test-threads", "1"])
            .env(CHILD, forge.name())
            .env("DUSTY_GITEA_CHECKS_REPO", &world.repo);
        world.configure(&mut command);
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        // What the screen asked the forge, to tell a read never sent from one that failed.
        let asked = world
            .api_requests()
            .iter()
            .map(|request| format!("{} {}", request["method"].as_str().unwrap_or(""), request["path"].as_str().unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{} child:\n{stdout}\n{}\nrequests:\n{asked}",
            forge.name(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    true
}

/// The child's forge and repository.
fn child() -> (Forge, PathBuf) {
    let name = std::env::var(CHILD).unwrap();
    let forge = ALL.into_iter().find(|forge| forge.name() == name).unwrap();
    (forge, PathBuf::from(std::env::var_os("DUSTY_GITEA_CHECKS_REPO").unwrap()))
}

#[test]
fn gitea_checks_show_the_jobs_steps_at_every_width() {
    if in_parent("gitea_checks_show_the_jobs_steps_at_every_width") {
        return;
    }
    let (forge, repo) = child();
    let name = forge.name();
    let pull = if forge == Forge::Gitea { 1412 } else { 1794 };
    for (width, height) in [(190, 48), (120, 36)] {
        let mut screen = GitScreen::open(&repo).unwrap();
        screen.open_target("prs", Some(pull));
        settle(&mut screen, width, height);
        for _ in 0..2 {
            key(&mut screen, KeyCode::Char('L'));
            settle(&mut screen, width, height);
        }
        let checks = settle(&mut screen, width, height);
        assert!(checks.contains("lint"), "{name} {width}x{height} is not on Checks:\n{checks}");
        assert!(
            !checks.contains("unavailable"),
            "{name} {width}x{height} says the steps are unavailable:\n{checks}"
        );
        assert!(
            checks.contains("ruff check orbit tests") || checks.contains("Set up job"),
            "{name} {width}x{height} shows no steps of the selected job:\n{checks}"
        );
    }
}

/// Gitea's list API has no CI state: the rows on screen are asked for their combined status
/// once the list is painted, and the conversation's passing pull request gets its mark.
#[test]
fn gitea_pull_request_rows_show_their_checks() {
    if in_parent("gitea_pull_request_rows_show_their_checks") {
        return;
    }
    let (_, repo) = child();
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    let start = Instant::now();
    loop {
        screen.poll();
        let frame = view(&mut screen, 190, 48);
        // The list's half of each row: the reader beside it repeats the title.
        let list = frame
            .lines()
            .map(|line| line.split("││").next().unwrap_or_default())
            .find(|half| half.contains("Clamp exponential backoff"))
            .unwrap_or_default();
        if list.contains('✔') {
            return;
        }
        assert!(start.elapsed() < DEADLINE, "the passing row never got its check mark:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
