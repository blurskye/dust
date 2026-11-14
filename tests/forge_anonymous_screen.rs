#![cfg(unix)]
//! The forge tabs signed out (FORGE.md anonymous reads, DESIGN.md §3.45): a public host shows
//! the `Anonymous · read-only · N/60 requests left` chip over the list, the footer drops its
//! write keys, a write key says why instead of opening a form, and an instance that hides even
//! reads gets a full-pane sign-in state with its commands, not an error dump. Each case re-runs
//! itself in a child pointed at its own world, with no token and no forge CLI.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_world::{Forge, World};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_ANONYMOUS_SCREEN_CHILD";

fn rerun_in_child(test: &str, world: &World) {
    let bin = world.directory.path().join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let _ = std::os::unix::fs::symlink(git, bin.join("git"));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("DUSTY_ANONYMOUS_REPO", &world.repo);
    world.configure(&mut command);
    command
        .env_remove("DUSTY_FORGE_TOKEN")
        .env_remove("DUSTY_FORGE_ANONYMOUS")
        .env("PATH", &bin);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child_repo() -> Option<PathBuf> {
    std::env::var_os(CHILD)?;
    Some(PathBuf::from(std::env::var_os("DUSTY_ANONYMOUS_REPO").unwrap()))
}

fn world(forge: Forge, anonymous: Value) -> World {
    World::with_state(forge, json!({ "anonymous": anonymous }))
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn text(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    (0..height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = text(screen, 160, 40);
        if done(&frame) {
            return frame;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn footer(frame: &str) -> &str {
    frame.lines().last().unwrap_or_default()
}

#[test]
fn a_public_host_reads_signed_out_and_says_why_writes_are_refused() {
    let Some(repo) = child_repo() else {
        rerun_in_child(
            "a_public_host_reads_signed_out_and_says_why_writes_are_refused",
            &world(Forge::Gitea, json!({ "limit": 60 })),
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    let pull = format!("#{}", Forge::Gitea.pull());
    screen.open_target("prs", Some(Forge::Gitea.pull()));
    let frame = wait_for(&mut screen, "the anonymous chip", |frame| {
        frame.contains(&pull)
            && frame.contains("Anonymous · read-only")
            && frame.contains("Showing")
            && !frame.contains("Loading")
    });
    assert!(frame.contains("requests left"), "the chip counts the quota:\n{frame}");
    assert!(frame.contains("sign in: tea"), "the chip says how to sign in:\n{frame}");
    let footer_text = footer(&frame);
    for write in ["review", "comment"] {
        assert!(!footer_text.contains(write), "a write key stayed in the footer: {footer_text}");
    }
    // A write key is not ignored: it says why, and how to sign in.
    key(&mut screen, KeyCode::Char('C'));
    let frame = wait_for(&mut screen, "the refusal", |frame| frame.contains("Sign in to comment"));
    assert!(!frame.contains("Leave a comment"), "no form opens:\n{frame}");
    assert!(frame.contains("tea login add"), "the refusal names the sign-in:\n{frame}");
}

#[test]
fn an_instance_that_hides_reads_shows_its_sign_in_commands() {
    let Some(repo) = child_repo() else {
        rerun_in_child(
            "an_instance_that_hides_reads_shows_its_sign_in_commands",
            &world(Forge::Gitea, json!({ "sign_in_required": true })),
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    let frame = wait_for(&mut screen, "the sign-in state", |frame| frame.contains("requires sign-in") && frame.contains("$ "));
    assert!(frame.contains("tea login add"), "{frame}");
    assert!(frame.contains("Sign-in required"), "the list says it too:\n{frame}");
    assert!(!frame.contains("This view is unavailable"), "no error dump:\n{frame}");
}
