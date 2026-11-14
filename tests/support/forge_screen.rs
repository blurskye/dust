//! A GitHub screen against the fake forge with the disk cache on (FORGE.md §7, F3). Each test
//! re-runs itself as a child with `DUSTY_FORGE_CACHE_DIR` pointing at a private directory, so
//! sessions in one test share a cache and tests never share one. Include after `forge_fake`:
//! `#[path = "support/forge_screen.rs"] mod forge_screen;`.
#![allow(dead_code)]

use super::forge_fake;
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

pub fn control(screen: &mut GitScreen, code: KeyCode) {
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

pub fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}

/// Top tabs cycle Git → Pull requests → Issues → Actions → Notifications (pages 0, 2, 1, 3, 4).
pub fn goto(screen: &mut GitScreen, from: usize, to: usize) {
    const ORDER: [usize; 5] = [0, 2, 1, 3, 4];
    let at = |page| ORDER.iter().position(|&p| p == page).unwrap();
    for _ in 0..(at(to) + 5 - at(from)) % 5 {
        key(screen, KeyCode::Char('>'));
    }
}

pub fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 65);
    (0..65)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of the row under the list cursor.
pub fn cursor_row(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 65);
    let (_, y, _) = frame.cursor.expect("the list keeps a cursor");
    frame.grid.row_text(y)
}

pub fn wait(screen: &mut GitScreen, text: &str) {
    wait_for(screen, text, |frame| frame.contains(text));
}

pub fn wait_for(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = view(screen);
        if done(&frame) {
            return;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Poll until the fake server has seen no request for `quiet` (warm-up included).
pub fn settle(screen: &mut GitScreen, quiet: Duration) {
    let end = Instant::now() + Duration::from_secs(30);
    let mut seen = requests().len();
    let mut since = Instant::now();
    while since.elapsed() < quiet {
        screen.poll();
        let now = requests().len();
        if now != seen {
            seen = now;
            since = Instant::now();
        }
        assert!(Instant::now() < end, "the forge never went quiet");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn fixture() -> PathBuf {
    PathBuf::from(std::env::var("DUST_GITHUB_FIXTURE").unwrap())
}

pub fn cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("DUSTY_FORGE_CACHE_DIR").unwrap())
}

pub fn requests() -> Vec<Value> {
    forge_fake::requests(&fixture())
}

pub fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(&fixture())
}

pub fn state() -> Value {
    forge_fake::state(&fixture())
}

/// Change the fake forge's world in place, keeping its request log.
pub fn update_state(change: impl FnOnce(&mut Value)) {
    let mut value = state();
    change(&mut value);
    std::fs::write(fixture(), serde_json::to_string(&value).unwrap()).unwrap();
}

pub fn release(name: &str) {
    std::fs::write(format!("{}.{name}", fixture().display()), "release").unwrap();
}

pub fn open() -> GitScreen {
    GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap()
}

/// Every cache entry file (not indexes or locks).
pub fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path.file_name().is_some_and(|name| name != "index.json")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Make every cached entry `seconds` older, as if the last session were that long ago.
pub fn age_cache(seconds: u64) {
    for path in entries(&cache_dir()) {
        let mut envelope: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let at = envelope["fetched_at"].as_u64().unwrap();
        envelope["fetched_at"] = (at - seconds * 1000).into();
        std::fs::write(&path, serde_json::to_string(&envelope).unwrap()).unwrap();
    }
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}

/// Run `name` as a child against a fresh fake forge, repository and cache. Returns `true` in
/// the parent (the test is done), `false` in the child (run the body).
pub fn fixture_child(name: &str, world: Value) -> bool {
    if std::env::var_os("DUST_GITHUB_CHILD").is_some() {
        return false;
    }
    let owner = tempfile::tempdir().unwrap();
    let bin = owner.path().join("bin");
    let state = owner.path().join("github.json");
    std::fs::write(&state, world.to_string()).unwrap();
    let server = forge_fake::ForgeServer::start(&state, "legacy");
    server.install_gh(&bin);
    let root = owner.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Screen Test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["commit", "--allow-empty", "-m", "Initial"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/demo.git",
        ],
    );
    let output = server
        .configure(&mut Command::new(std::env::current_exe().unwrap()))
        .args(["--exact", name, "--nocapture"])
        .env("DUST_GITHUB_CHILD", "1")
        .env("DUST_GITHUB_FIXTURE", &state)
        .env("DUST_GITHUB_ROOT", &root)
        .env("DUSTY_FORGE_CACHE_DIR", owner.path().join("cache"))
        .env_remove("DUSTY_FORGE_WARM")
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Measurements the child prints are worth seeing in the parent's output too.
    for line in stdout.lines().filter(|line| line.starts_with("measured:")) {
        println!("{line}");
    }
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

pub fn navigation_issues() -> Vec<Value> {
    [8, 7, 6, 5, 4, 2, 1]
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "number": id,
                "title": format!("Navigation issue {id}"),
                "body": format!("Unique selected body {id}."),
            })
        })
        .collect()
}
