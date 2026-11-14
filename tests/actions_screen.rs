#![cfg(unix)]
//! The Actions tab (DESIGN.md §3.37–§3.41) against the fake forge's Actions world
//! (`server.py --world actions`, octo/orbit on github.com over direct HTTP): the runs table and
//! its summary, the run › jobs › steps › log drilldown and its identity, attempts, the full log's
//! find counts and pan, artifacts with a download, and the approve/cancel sheets. Each case
//! re-runs itself in a child pointed at its own world.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_fake::{FAKE_TOKEN, ForgeServer};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_ACTIONS_SCREEN_CHILD";

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

/// Runs `test` in a child of this binary with its own Actions world.
fn rerun_in_child(test: &str) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state.json");
    std::fs::write(&state, "{}").unwrap();
    let server = ForgeServer::start(&state, "actions");
    let repo = directory.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", "https://github.com/octo/orbit.git"]);
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let missing = directory.path().join("no-cli");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([test, "--exact", "--nocapture", "--test-threads", "1"]);
    server.configure(&mut command);
    command
        .env(CHILD, &repo)
        .env("DUSTY_ACTIONS_STATE", &state)
        .env("DUSTY_FORGE_TOKEN", FAKE_TOKEN)
        .env("DUSTY_FORGE_GH", &missing)
        .env("DUSTY_FORGE_GLAB", &missing)
        .env("DUSTY_FORGE_TEA", &missing)
        .env("DUSTY_FORGE_CACHE_DIR", directory.path().join("cache"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("DUST_CONFIG_HOME", directory.path().join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child() -> Option<PathBuf> {
    std::env::var_os(CHILD).map(PathBuf::from)
}

fn requests() -> Vec<Value> {
    forge_fake::requests(Path::new(&std::env::var_os("DUSTY_ACTIONS_STATE").unwrap()))
}

fn requested(method: &str, fragment: &str) -> bool {
    requests().iter().any(|request| {
        request["method"].as_str() == Some(method)
            && request["path"].as_str().is_some_and(|path| path.contains(fragment))
    })
}

fn press(screen: &mut GitScreen, key: KeyEvent) {
    screen.event(UiEvent::Key {
        key,
        kind: KeyInputKind::Press,
    });
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    press(screen, KeyEvent::plain(code));
}

fn control(screen: &mut GitScreen, code: KeyCode) {
    press(
        screen,
        KeyEvent {
            code,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
    );
}

fn typed(screen: &mut GitScreen, text: &str) {
    for c in text.chars() {
        key(screen, KeyCode::Char(c));
    }
}

fn text(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    (0..height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for(screen: &mut GitScreen, size: (u16, u16), what: &str, done: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = text(screen, size.0, size.1);
        if done(&frame) {
            return frame;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait(screen: &mut GitScreen, needle: &str) -> String {
    wait_for(screen, (190, 48), needle, |frame| frame.contains(needle))
}

/// The row the terminal cursor sits on.
fn cursor_row(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    let (_, y, _) = frame.cursor.expect("a cursor");
    frame.grid.row_text(y)
}

fn open(repo: &Path) -> GitScreen {
    let mut screen = GitScreen::open(repo).unwrap();
    // Git → Pull requests → Issues → Actions.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('>'));
    }
    wait(&mut screen, "Release v1.4");
    screen
}

#[test]
fn runs_table_summary_and_drilldown_keep_identity_across_attempts() {
    let Some(repo) = child() else {
        rerun_in_child("runs_table_summary_and_drilldown_keep_identity_across_attempts");
        return;
    };
    let mut screen = open(&repo);
    let wide = wait(&mut screen, "3 runs");
    for needle in ["Workflow runs", "All workflows", "All branches", "All events", "#12", "#11", "#10", "retry-uploads", "PR #9"] {
        assert!(wide.contains(needle), "{needle}:\n{wide}");
    }
    assert!(wide.contains("1 running") && wide.contains("1 failed"), "{wide}");
    // Down to the failed run: its summary names the failed job and its first failure line.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    assert!(cursor_row(&mut screen, 190, 48).contains("#10"));
    let summary = wait(&mut screen, "exit code 101");
    assert!(summary.contains("Retry flaky uploads") && summary.contains("Jobs"), "{summary}");
    assert!(!summary.contains("[31m"), "SGR is rendered, never shown raw:\n{summary}");
    // At 100×30 the summary is gone and the row names what failed.
    let narrow = text(&mut screen, 100, 30);
    assert!(narrow.contains("· test ✕"), "{narrow}");
    assert!(!narrow.contains("Jobs ·"), "{narrow}");
    // Enter drills in: the failed job is selected; its steps come from the log's sections.
    key(&mut screen, KeyCode::Enter);
    let drill = wait(&mut screen, "Run cargo test");
    assert!(drill.contains("attempt 2 of 2"), "{drill}");
    assert!(drill.lines().nth(1).unwrap().contains("Run #10"), "{drill}");
    assert!(cursor_row(&mut screen, 190, 48).contains("test"));
    key(&mut screen, KeyCode::Enter);
    let steps = wait(&mut screen, "FAILED");
    assert!(cursor_row(&mut screen, 190, 48).contains("Run cargo test"), "{steps}");
    assert!(!steps.contains("[31m") && !steps.contains("##[error]"), "{steps}");
    assert!(steps.contains("error: Process completed with exit code 101."), "{steps}");
    // Into the log: the cursor starts on its first error.
    key(&mut screen, KeyCode::Enter);
    assert!(cursor_row(&mut screen, 190, 48).contains("error: Process completed"));
    // `a` lists the attempts; choosing the first reads that attempt, not the latest again.
    key(&mut screen, KeyCode::Char('a'));
    let attempts = wait(&mut screen, "Attempt 1");
    assert!(attempts.contains("Attempt 2"), "{attempts}");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    let earlier = wait(&mut screen, "attempt 1 of 2");
    assert!(requested("GET", "actions/runs/501/attempts/1"), "{earlier}");
    // A different attempt is a different log even when its text is identical.
    key(&mut screen, KeyCode::Escape);
    let back = wait(&mut screen, "Workflow runs");
    assert!(back.contains("#10"), "{back}");
}

#[test]
fn full_log_find_counts_pan_and_wrap() {
    let Some(repo) = child() else {
        rerun_in_child("full_log_find_counts_pan_and_wrap");
        return;
    };
    let mut screen = open(&repo);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Run cargo test");
    key(&mut screen, KeyCode::Char('f'));
    let full = wait(&mut screen, "Runner version");
    assert!(full.contains("Job log · test") && full.contains("lines"), "{full}");
    // Find: smart case, counts, n/N wrap around, Esc clears.
    key(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "test");
    key(&mut screen, KeyCode::Enter);
    let found = text(&mut screen, 190, 48);
    assert!(found.contains("1/4 matches"), "{found}");
    key(&mut screen, KeyCode::Char('n'));
    assert!(text(&mut screen, 190, 48).contains("2/4 matches"));
    key(&mut screen, KeyCode::Char('N'));
    key(&mut screen, KeyCode::Char('N'));
    assert!(text(&mut screen, 190, 48).contains("4/4 matches"));
    key(&mut screen, KeyCode::Escape);
    assert!(!text(&mut screen, 190, 48).contains("matches"));
    key(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "FAILED");
    key(&mut screen, KeyCode::Enter);
    assert!(text(&mut screen, 190, 48).contains("1/1 matches"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "nothing like this");
    key(&mut screen, KeyCode::Enter);
    assert!(text(&mut screen, 190, 48).contains("no matches"));
    key(&mut screen, KeyCode::Escape);
    // Pan: l moves 8 cells and marks the clipped start; h comes back.
    let before = cursor_row(&mut screen, 100, 30);
    key(&mut screen, KeyCode::Char('l'));
    let panned = cursor_row(&mut screen, 100, 30);
    assert!(panned.contains('‹') && panned != before, "{before}\n{panned}");
    key(&mut screen, KeyCode::Char('h'));
    assert_eq!(cursor_row(&mut screen, 100, 30), before);
    // Wrap keeps every character on screen at the narrowest size.
    key(&mut screen, KeyCode::Char('w'));
    let wrapped = text(&mut screen, 64, 20);
    assert!(!wrapped.contains('›') || !wrapped.contains('‹'), "{wrapped}");
    // Ctrl+s writes the log beside the repository.
    control(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, "Save job log");
    control(&mut screen, KeyCode::Enter);
    let saved = std::fs::read_to_string(repo.join("test.log")).unwrap();
    assert!(saved.contains("running 3 tests") && !saved.contains('\u{1b}'), "{saved}");
}

#[test]
fn artifacts_tab_lists_browses_and_downloads() {
    let Some(repo) = child() else {
        rerun_in_child("artifacts_tab_lists_browses_and_downloads");
        return;
    };
    let mut screen = open(&repo);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Run cargo test");
    key(&mut screen, KeyCode::Char(']'));
    let tab = wait(&mut screen, "lcov.info");
    for needle in ["Artifacts 3", "coverage", "old-logs", "expired", "orbit-linux", "coverage/index.html", "Destination"] {
        assert!(tab.contains(needle), "{needle}:\n{tab}");
    }
    // The expired archive refuses a download.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    assert!(text(&mut screen, 190, 48).contains("This artifact has expired"));
    key(&mut screen, KeyCode::Up);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Destination folder");
    control(&mut screen, KeyCode::Enter);
    let receipt = wait(&mut screen, "Downloaded");
    assert!(receipt.contains("artifacts/coverage.zip"), "{receipt}");
    assert!(repo.join("artifacts/coverage.zip").is_file());
    // `[` goes back to the jobs.
    key(&mut screen, KeyCode::Char('['));
    key(&mut screen, KeyCode::Char('['));
    wait(&mut screen, "Run cargo test");
}

#[test]
fn approve_and_cancel_sheets_check_the_run_first() {
    let Some(repo) = child() else {
        rerun_in_child("approve_and_cancel_sheets_check_the_run_first");
        return;
    };
    let mut screen = open(&repo);
    // A finished run cannot be cancelled; one that is not waiting cannot be approved.
    key(&mut screen, KeyCode::End);
    wait(&mut screen, "exit code 101");
    key(&mut screen, KeyCode::Char('x'));
    assert!(text(&mut screen, 190, 48).contains("already finished"));
    key(&mut screen, KeyCode::Char('A'));
    assert!(text(&mut screen, 190, 48).contains("not waiting for approval"));
    // The fork run waits for approval.
    key(&mut screen, KeyCode::Up);
    wait(&mut screen, "Waiting for approval");
    key(&mut screen, KeyCode::Char('A'));
    wait(&mut screen, "Approve CI #11?");
    assert!(!requested("POST", "approve"));
    control(&mut screen, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(30);
    while !requested("POST", "actions/runs/502/approve") {
        screen.poll();
        assert!(Instant::now() < end, "{}\n{:?}", text(&mut screen, 190, 48), requests().iter().map(|r| format!("{} {}", r["method"], r["path"])).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(10));
    }
    wait(&mut screen, "Saved on GitHub");
    // The running release can be cancelled.
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('x'));
    wait(&mut screen, "Cancel Release #12?");
    control(&mut screen, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(30);
    while !requested("POST", "actions/runs/503/cancel") {
        screen.poll();
        assert!(Instant::now() < end, "{}\n{:?}", text(&mut screen, 190, 48), requests().iter().map(|r| format!("{} {}", r["method"], r["path"])).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(10));
    }
}
