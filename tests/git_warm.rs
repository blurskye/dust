#![cfg(unix)]
//! The Git tab paints before git's slow reads answer. Each child below runs with a `git` shim
//! that sleeps 5 s before the status reads (`diff --raw`, `ls-files --others`, `status`), and
//! for the warm children before `log` too.
//!
//! - Cold (no cache): History paints from its own `git log` while Changes reads as loading.
//! - Warm, same HEAD and index: the cached Changes (1,200 modified files), History and
//!   branches paint at once from the cache (`src/git/screen/warm.rs`), marked `updating…`.
//! - Warm, another HEAD: History and the branches still paint from the cache, but Changes
//!   reads as loading rather than listing another HEAD's changes.

use dust::git::screen::GitScreen;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_GIT_WARM_CHILD";
const FILES: usize = 1_200;
/// git sleeps this long first in the slow children.
const SLOW: Duration = Duration::from_secs(5);
/// The status worker's reads (`src/git/status.rs`) and the branch list's dirty check.
const STATUS_READS: &str = "--raw|--others|status";

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
}

/// Polls and paints until `ready`; the frame and the time from `start`.
fn until(screen: &mut GitScreen, start: Instant, ready: impl Fn(&str) -> bool) -> (String, Duration) {
    loop {
        screen.poll();
        let frame = view(screen);
        if ready(&frame) {
            return (frame, start.elapsed());
        }
        assert!(start.elapsed() < Duration::from_secs(60), "never painted:\n{frame}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A `git` first on PATH that sleeps before any command with one of `words` among its
/// arguments, then runs the real one.
fn slow_git(directory: &Path, name: &str, words: &str) -> PathBuf {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap();
    let bin = directory.join(name);
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor argument in \"$@\"; do case \"$argument\" in {words}) sleep {}; break;; esac; done\nexec {} \"$@\"\n",
            SLOW.as_secs(),
            real.trim()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn child(mode: &str, repo: &Path, cache: &Path, path: Option<&Path>) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["warm_child", "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, format!("{mode}:{}", repo.display()))
        .env("DUSTY_GIT_CACHE_DIR", cache)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(path) = path {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        command.env("PATH", std::env::join_paths(std::iter::once(path.to_owned()).chain(std::env::split_paths(&inherited))).unwrap());
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // libtest writes `test warm_child ... ` before the child's own output.
    for line in stdout.lines().filter_map(|line| line.find("GITWARM").map(|at| &line[at..])) {
        println!("{line}");
    }
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{mode} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_reopened_git_tab_paints_from_its_cache_without_waiting_for_git() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Warm"]);
    git(&repo, &["config", "user.email", "warm@example.com"]);
    for index in 0..FILES {
        std::fs::write(repo.join(format!("src/file{index:04}.txt")), "one\n").unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "Warm tip"]);
    for index in 0..FILES {
        std::fs::write(repo.join(format!("src/file{index:04}.txt")), "two\n").unwrap();
    }
    let cache = directory.path().join("cache");
    // 1. Cold: History paints while the status reads sleep.
    let slow_status = slow_git(directory.path(), "slow-status", STATUS_READS);
    child("cold", &repo, &directory.path().join("cold-cache"), Some(&slow_status));
    // 2. A first session fills the cache.
    child("prime", &repo, &cache, None);
    let slow = slow_git(directory.path(), "slow-all", &format!("{STATUS_READS}|log"));
    // 3. Same HEAD and index: everything paints from the cache while git sleeps.
    child("warm", &repo, &cache, Some(&slow));
    // 4. Another HEAD: History and branches from the cache, Changes loading.
    std::fs::write(repo.join("moved.txt"), "moved\n").unwrap();
    git(&repo, &["add", "moved.txt"]);
    git(&repo, &["commit", "-q", "-m", "Moved tip"]);
    child("moved", &repo, &cache, Some(&slow));
}

/// The children of the test above (`DUSTY_GIT_WARM_CHILD=mode:repo`); passes alone.
#[test]
fn warm_child() {
    let Some(value) = std::env::var_os(CHILD) else { return };
    let value = value.to_string_lossy().into_owned();
    let (mode, repo) = value.split_once(':').unwrap();
    let repo = Path::new(repo);
    let cache = PathBuf::from(std::env::var_os("DUSTY_GIT_CACHE_DIR").unwrap());
    let count = format!("{FILES} · tree");
    let start = Instant::now();
    let mut screen = GitScreen::open(repo).unwrap();
    match mode {
        "cold" => {
            let (frame, time) = until(&mut screen, start, |frame| frame.contains("Warm tip"));
            println!("GITWARM\tcold History paint\t{}", time.as_micros());
            assert!(time < SLOW / 5, "History waited for the status ({time:?}):\n{frame}");
            assert!(!frame.contains(&count), "the status has not answered yet:\n{frame}");
            assert!(frame.contains("Loading…"), "Changes reads as loading:\n{frame}");
            // Then the status fills Changes.
            let (_, time) = until(&mut screen, start, |frame| frame.contains(&count));
            println!("GITWARM\tcold Changes paint\t{}", time.as_micros());
            assert!(time >= SLOW, "the shim did not hold the status back ({time:?})");
        }
        "prime" => {
            until(&mut screen, start, |frame| frame.contains(&count) && frame.contains("Warm tip"));
            // The cache is written on a thread once status, History and branches are in.
            let written = || std::fs::read_dir(&cache).is_ok_and(|mut entries| {
                entries.any(|entry| entry.is_ok_and(|entry| entry.path().extension().is_some_and(|e| e == "json")))
            });
            until(&mut screen, start, |_| written());
        }
        "warm" => {
            let (frame, time) = until(&mut screen, start, |frame| frame.contains(&count) && frame.contains("Warm tip"));
            println!("GITWARM\twarm first paint\t{}", time.as_micros());
            assert!(time < SLOW / 5, "the warm paint waited for git ({time:?}):\n{frame}");
            assert!(frame.contains("updating…"), "a cached status says it is updating:\n{frame}");
        }
        "moved" => {
            let (frame, time) = until(&mut screen, start, |frame| frame.contains("Warm tip"));
            println!("GITWARM\tmoved first paint\t{}", time.as_micros());
            assert!(time < SLOW / 5, "History waited for git ({time:?}):\n{frame}");
            assert!(!frame.contains(&count), "another HEAD's changes are never listed:\n{frame}");
            assert!(frame.contains("Loading…"), "Changes reads as loading:\n{frame}");
        }
        _ => unreachable!("{mode}"),
    }
}
