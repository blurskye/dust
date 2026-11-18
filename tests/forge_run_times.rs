#![cfg(unix)]
//! A run's `running X` counts from when it started, not from when it was created: a re-run of
//! a two-day-old run that started three minutes ago reads `running 3m`, not `running 48h`.
//! GitHub sends the start as `run_started_at`, Gitea `started_at` and Forgejo `started`, each
//! beside a `created_at` / `created` that a re-run keeps.
//!
//! The designed Actions world (`world_actions.seed_design`), seeded here with its newest run
//! (812, in progress) created two days ago and started three minutes ago.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dusty::editor::input::{KeyCode, KeyEvent};
use dusty::git::screen::GitScreen;
use dusty::ui::{KeyInputKind, UiEvent};
use forge_fake::{FAKE_TOKEN, ForgeServer};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_RUN_TIMES_CHILD";

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

/// The designed world with run 812 created two days ago and started three minutes ago.
fn designed_world() -> Value {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forge");
    let script = r#"
import json, sys, time
sys.path.insert(0, sys.argv[1])
import world_actions
world = world_actions.seed_design()
now = time.time()
run = next(item for item in world["runs"] if item["id"] == 812)
assert run["status"] == "in_progress"
run["created_at"] = world_actions.iso(now - 2 * 86400)
run["run_started_at"] = world_actions.iso(now - 185)
print(json.dumps(world))
"#;
    let output = Command::new("python3")
        .args(["-c", script])
        .arg(&fixtures)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn rerun_in(test: &str, world: &str, remote: &str, probe: Option<(&str, &str)>) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state.json");
    let mut seed = serde_json::json!({ "actions_seed": "design", "viewer": "maya", "actions": designed_world() });
    if let Some((host, version)) = probe {
        seed["probes"] =
            serde_json::json!({ host: { "api/v1/version": { "body": { "version": version } } } });
    }
    std::fs::write(&state, seed.to_string()).unwrap();
    let server = ForgeServer::start(&state, world);
    let repo = directory.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", remote]);
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let missing = directory.path().join("no-cli");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([test, "--exact", "--nocapture", "--test-threads", "1"]);
    server.configure(&mut command);
    command
        .env(CHILD, &repo)
        .env("DUSTY_FORGE_TOKEN", FAKE_TOKEN)
        .env("FORGEJO_TOKEN", FAKE_TOKEN)
        .env("DUSTY_FORGE_GH", &missing)
        .env("DUSTY_FORGE_GLAB", &missing)
        .env("DUSTY_FORGE_TEA", &missing)
        .env("DUSTY_FORGE_CACHE_DIR", directory.path().join("cache"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("DUSTY_CONFIG_HOME", directory.path().join("config"))
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

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn text(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Actions tab lists run 812 as `running 3m`.
fn reads_running_from_the_start(repo: &Path) {
    let mut screen = GitScreen::open(repo).unwrap();
    // Git → Pull requests → Issues → Actions.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('>'));
    }
    let end = Instant::now() + Duration::from_secs(30);
    let frame = loop {
        screen.poll();
        let frame = text(&mut screen);
        if frame.contains("Document the quickstart") && frame.contains("running ") {
            break frame;
        }
        assert!(Instant::now() < end, "no running run:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        frame.contains("running 3m"),
        "the re-run counts from its start:\n{frame}"
    );
    assert!(
        !frame.contains("running 48h"),
        "not from its creation:\n{frame}"
    );
}

#[test]
fn a_github_rerun_reads_running_from_run_started_at() {
    let Some(repo) = child() else {
        return rerun_in(
            "a_github_rerun_reads_running_from_run_started_at",
            "actions",
            "https://github.com/octo/orbit.git",
            None,
        );
    };
    reads_running_from_the_start(&repo);
}

#[test]
fn a_gitea_rerun_reads_running_from_started_at() {
    let Some(repo) = child() else {
        return rerun_in(
            "a_gitea_rerun_reads_running_from_started_at",
            "gitea",
            "https://gitea.com/orbit/orbit.git",
            Some(("gitea.com", "29.0.0+dev-160-g55186ca39b")),
        );
    };
    reads_running_from_the_start(&repo);
}

#[test]
fn a_forgejo_rerun_reads_running_from_started() {
    let Some(repo) = child() else {
        return rerun_in(
            "a_forgejo_rerun_reads_running_from_started",
            "forgejo",
            "https://code.forgejo.org/orbit/orbit.git",
            Some(("code.forgejo.org", "17.0.0-26-363f3dc30b+gitea-1.22.0")),
        );
    };
    reads_running_from_the_start(&repo);
}
