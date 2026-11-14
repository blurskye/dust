#![cfg(unix)]
//! The forge tabs speak each forge's language (DESIGN.md §4.3): GitLab, Gitea and Forgejo
//! screens never show GitHub-only words in their chrome. Each provider's world is opened by
//! deep link (`GitScreen::open_target`, as `dusty-git --page … --open …` does), every reader
//! tab of a pull (merge) request and an issue is rendered at a wide and a narrow size, and the
//! text is searched for the words that belong to GitHub alone. Each case re-runs itself in a
//! child pointed at its own world.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_world::{Forge, World};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_WORDING_CHILD";

fn rerun_in_child(test: &str, world: &World) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("DUSTY_WORDING_REPO", &world.repo);
    world.configure(&mut command);
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
    Some(PathBuf::from(std::env::var_os("DUSTY_WORDING_REPO").unwrap()))
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

/// Poll until `done` holds for the wide frame, then a little longer so follow-up loads land.
fn wait_for(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = text(screen, 190, 48);
        if done(&frame) {
            break;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let quiet = Instant::now() + Duration::from_millis(600);
    while Instant::now() < quiet {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Every reader tab of the open item, at 190×48 and 100×30.
fn every_tab(screen: &mut GitScreen, tabs: usize) -> Vec<String> {
    let mut shots = Vec::new();
    for _ in 0..tabs {
        let quiet = Instant::now() + Duration::from_millis(400);
        while Instant::now() < quiet {
            screen.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        shots.push(text(screen, 190, 48));
        shots.push(text(screen, 100, 30));
        key(screen, KeyCode::Char(']'));
    }
    shots
}

/// Words that belong to GitHub (or to another forge) and so must not reach this forge's screen.
fn foreign(forge: Forge) -> Vec<&'static str> {
    let mut words = vec!["gh auth", "GH_TOKEN"];
    match forge {
        Forge::GitLab => words.extend([
            "Pull request",
            "pull request",
            "Review requested",
            "Checks",
            "│ CI",
            "CI │",
        ]),
        Forge::Gitea | Forge::Forgejo => words.extend(["Merge request", "merge request", "Pipelines"]),
        Forge::GitHub => {}
    }
    words
}

fn wording(forge: Forge, test: &str) {
    let Some(repo) = child_repo() else {
        let world = World::new(forge);
        rerun_in_child(test, &world);
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    let sigil = if forge == Forge::GitLab { "!" } else { "#" };
    let pull = format!("{sigil}{}", forge.pull());
    let pages = if forge == Forge::GitLab { "mrs" } else { "prs" };
    screen.open_target(pages, Some(forge.pull()));
    wait_for(&mut screen, &pull, |frame| frame.contains(&pull) && !frame.contains("Loading details"));
    let mut shots = every_tab(&mut screen, 5);
    let issue = format!("#{}", forge.issue());
    screen.open_target("issues", Some(forge.issue()));
    wait_for(&mut screen, &issue, |frame| frame.contains(&issue) && !frame.contains("Loading details"));
    shots.extend(every_tab(&mut screen, 3));
    let mut found = Vec::new();
    for shot in &shots {
        // The forge's own name may only be GitHub's in chrome: recorded bodies talk about
        // GitHub freely, so the word is looked for in the top bar, tab rows and footer only.
        let lines = shot.lines().collect::<Vec<_>>();
        let chrome = lines.iter().take(3).chain(lines.iter().rev().take(2));
        for line in chrome {
            if line.contains("GitHub") {
                found.push(format!("\"GitHub\" in: {}", line.trim()));
            }
        }
        for word in foreign(forge) {
            if let Some(line) = shot.lines().find(|line| line.contains(word)) {
                found.push(format!("{word:?} in: {}", line.trim()));
            }
        }
    }
    found.sort();
    found.dedup();
    assert!(
        found.is_empty(),
        "{} screens use another forge's words:\n{}\n\nfirst screen:\n{}",
        forge.name(),
        found.join("\n"),
        shots[0]
    );
}

#[test]
fn gitlab_screens_use_gitlab_words() {
    wording(Forge::GitLab, "gitlab_screens_use_gitlab_words");
}

#[test]
fn gitea_screens_use_gitea_words() {
    wording(Forge::Gitea, "gitea_screens_use_gitea_words");
}

#[test]
fn forgejo_screens_use_forgejo_words() {
    wording(Forge::Forgejo, "forgejo_screens_use_forgejo_words");
}
