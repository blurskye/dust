#![cfg(unix)]
//! Notifications laid out like github.com/notifications (DESIGN.md §3.35.1): the sidebar's
//! views, filters and repositories with their counts, the list grouped by repository with a
//! reason chip per row, `e` done and `s` save kept in this repository's Git config (the only
//! forge write is the existing mark-read), `Enter` opening the pull request reader whose
//! `Esc` comes back, and `<leader> e` hiding the sidebar. Everything runs against the
//! loopback fake forge.

#[path = "support/forge_fake.rs"]
mod forge_fake;

use dust::config::{Config, GitKeys, load_bundled_config};
use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_NOTIFICATIONS_CHILD";

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A local Git config value, empty when unset.
fn setting(root: &Path, name: &str) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(["config", "--local", "--get", name]).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Every GraphQL operation the forge code sends as `mutation Name`.
fn mutation_names() -> Vec<String> {
    fn walk(dir: &Path, names: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, names);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for (at, _) in text.match_indices("mutation ") {
                    let name: String = text[at + 9..].chars().take_while(char::is_ascii_alphanumeric).collect();
                    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                        names.push(name);
                    }
                }
            }
        }
    }
    let mut names = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/forge"), &mut names);
    assert!(!names.is_empty(), "no GraphQL mutations found under src/forge");
    names
}

fn config() -> Config {
    let mut config = load_bundled_config().config;
    config.ui.git.keys = GitKeys::Dusty;
    config
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key { key: KeyEvent::plain(code), kind: KeyInputKind::Press });
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 36);
    (0..36).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
}

/// Polls until `done` holds for the 120×36 frame and nothing is in flight.
fn wait(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let text = view(screen);
        if done(&text) && !screen.busy() {
            return text;
        }
        assert!(Instant::now() < end, "missing {what}:\n{text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The sidebar's half of each row (left of the facing borders `││`).
fn side(text: &str) -> String {
    text.lines().map(|row| row.split("││").next().unwrap_or(row)).collect::<Vec<_>>().join("\n")
}

fn side_count(text: &str, name: &str) -> Option<String> {
    side(text).lines().find_map(|row| {
        let row = row.trim_matches(['│', '▎', ' ']);
        let rest = row.strip_prefix(name)?;
        Some(rest.trim().to_owned())
    })
}

#[test]
fn notifications_read_like_githubs_inbox() {
    let Some(repo) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state.json");
        // The default seed: its run 501 (CI on retry-uploads, failed) is the check suite thread 9002's.
        std::fs::write(&state, r#"{"viewer":"maya"}"#).unwrap();
        let server = forge_fake::ForgeServer::start(&state, "actions");
        let repo = directory.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", "https://github.com/octo/orbit.git"]);
        let home = directory.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let missing = directory.path().join("no-cli");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["notifications_read_like_githubs_inbox", "--exact", "--nocapture", "--test-threads", "1"]);
        server.configure(&mut command);
        command
            .env(CHILD, &repo)
            .env("DUSTY_FORGE_TOKEN", forge_fake::FAKE_TOKEN)
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
            "child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The forge saw reads and one mark-read (the thread marked done), nothing else. The
        // reader's GraphQL queries are POSTs that read; a mutation is any operation the forge
        // code names `mutation …` (or one with no name at all).
        let mutations = mutation_names();
        let writes = forge_fake::requests(&state)
            .into_iter()
            .filter(|request| {
                let read_query = request["path"] == "graphql"
                    && request["operation"].as_str().is_some_and(|operation| !mutations.iter().any(|name| name == operation));
                request["method"] != "GET" && !read_query
            })
            .map(|request| {
                let operation = request["operation"].as_str().unwrap_or("");
                format!("{} {} {operation}", request["method"].as_str().unwrap_or(""), request["path"].as_str().unwrap_or(""))
            })
            .collect::<Vec<_>>();
        assert!(
            !writes.is_empty() && writes.iter().all(|write| write.starts_with("PATCH") && write.contains("notifications/threads/9001")),
            "writes: {writes:?}"
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.configure_input(&config());
    screen.open_target("notifications", None);
    let text = wait(&mut screen, "the inbox", |text| text.contains("Retry flaky uploads") && text.contains("Repositories"));

    // N1–N3: the sidebar's views, filters and repositories, unread counted.
    let left = side(&text);
    for word in ["Inbox", "Saved", "Done", "Filters", "Assigned", "Participating", "Mentioned", "Team mentioned", "Review requested", "Repositories", "octo/orbit"] {
        assert!(left.contains(word), "sidebar lacks {word}:\n{text}");
    }
    assert_eq!(side_count(&text, "Inbox").as_deref(), Some("2"), "{text}");
    assert_eq!(side_count(&text, "Review requested").as_deref(), Some("1"), "{text}");
    // N4, N5, N7: grouped under the repository, read threads too, one row each with its
    // number and reason chip; `All │ Unread` above.
    assert!(text.contains("All 3") && text.contains("Unread 2"), "{text}");
    let row = text.lines().find(|row| row.contains("Retry flaky uploads")).unwrap();
    assert!(row.contains("#9") && row.contains("review requested"), "{row}");
    let mention = text.lines().find(|row| row.contains("Pipelines tab shows stale status")).unwrap();
    assert!(mention.contains(" mention "), "{mention}");
    let heading = text.lines().position(|row| row.contains("││") && row.split("││").nth(1).is_some_and(|list| list.contains("octo/orbit"))).unwrap();
    let first = text.lines().position(|row| row.contains("Retry flaky uploads")).unwrap();
    assert!(heading < first, "the repository heads its threads:\n{text}");
    assert!(text.lines().last().unwrap().contains("Enter open"), "{text}");

    // `[ ]`: only the unread.
    key(&mut screen, KeyCode::Char(']'));
    let text = view(&mut screen);
    assert!(!text.contains("Pipelines tab shows stale status") && text.contains("Retry flaky uploads"), "{text}");
    key(&mut screen, KeyCode::Char('['));

    // N6: `e` marks the first thread done: it leaves the inbox, Done counts it, and the mark
    // is this repository's.
    key(&mut screen, KeyCode::Char('e'));
    // (The receipt names it: `✓ Done · Retry flaky uploads #9`; no list row does.)
    let text = wait(&mut screen, "done", |text| {
        text.contains("✓ Done · Retry flaky uploads #9") && !text.lines().any(|row| row.contains("Retry flaky uploads") && !row.contains('✓'))
    });
    assert_eq!(side_count(&text, "Done").as_deref(), Some("1"), "{text}");
    assert_eq!(side_count(&text, "Inbox").as_deref(), Some("1"), "{text}");
    assert!(setting(&repo, "dusty-git.notifyDone").ends_with("/9001"), "{}", setting(&repo, "dusty-git.notifyDone"));

    // N9: Tab to the sidebar, down to Done, Enter shows it.
    key(&mut screen, KeyCode::Tab);
    for _ in 0..2 {
        key(&mut screen, KeyCode::Char('j'));
    }
    key(&mut screen, KeyCode::Enter);
    let text = view(&mut screen);
    assert!(text.contains("Retry flaky uploads") && !text.contains("Pipelines tab shows stale status"), "{text}");
    assert!(text.lines().last().unwrap().contains("e inbox"), "{text}");
    // `e` in Done moves it back.
    key(&mut screen, KeyCode::Char('e'));
    let text = view(&mut screen);
    assert!(text.contains("Nothing done yet"), "{text}");
    assert!(setting(&repo, "dusty-git.notifyDone").is_empty());

    // Back to the inbox; `s` saves the first thread (kept in Git config, starred).
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Char('g'));
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char('s'));
    let text = view(&mut screen);
    assert_eq!(side_count(&text, "Saved").as_deref(), Some("1"), "{text}");
    let row = text.lines().find(|row| row.contains("Retry flaky uploads")).unwrap();
    assert!(row.contains('★'), "{row}");
    assert!(setting(&repo, "dusty-git.notifySaved").ends_with("/9001"));

    // N8: Enter opens the pull request's reader; its Esc comes back here.
    key(&mut screen, KeyCode::Char('g'));
    key(&mut screen, KeyCode::Enter);
    let text = wait(&mut screen, "the pull request", |text| {
        text.contains("#9 Retry flaky uploads") && text.contains("Conversation") && !text.contains("Repositories")
    });
    // The reader's way back names where it goes (its footer, at a width that holds it).
    let frame = screen.frame(190, 48);
    let footer = frame.grid.row_text(47);
    assert!(footer.contains("Esc back"), "{footer}\n{text}");
    key(&mut screen, KeyCode::Escape);
    let text = wait(&mut screen, "back to notifications", |text| text.contains("Repositories"));
    assert!(text.contains("Retry flaky uploads"), "{text}");

    // `<leader> e` hides the sidebar and brings it back.
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    let text = view(&mut screen);
    assert!(!text.contains("Repositories") && text.contains("Retry flaky uploads"), "{text}");
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    let text = view(&mut screen);
    assert!(text.contains("Repositories"), "{text}");

    // N10: the check suite's row carries its run's outcome (run 501 failed).
    let suite = text.lines().find(|row| row.contains("CI workflow run failed")).unwrap();
    assert!(suite.contains('✕'), "{suite}");
    // N11: Enter opens that run (found by its workflow and branch: the subject names no
    // run), its footer says the way back, and Esc lands on the same thread again.
    key(&mut screen, KeyCode::Char('g'));
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Enter);
    let text = wait(&mut screen, "the run", |text| !text.contains("Repositories") && text.contains("deploy"));
    let footer = text.lines().last().unwrap_or_default().to_owned();
    assert!(footer.contains("Esc back"), "footer: {footer}\n{text}");
    key(&mut screen, KeyCode::Escape);
    let text = wait(&mut screen, "back from the run", |text| text.contains("Repositories"));
    let current = text.lines().find(|row| row.split("││").nth(1).is_some_and(|list| list.starts_with('▎'))).unwrap_or_default();
    assert!(current.contains("CI workflow run failed"), "the cursor is on the thread that was opened:\n{text}");

    // `/` filters the list by subject (r15 notif-actions-12): typing takes every key, `Enter`
    // keeps the filter, `Esc` clears it.
    key(&mut screen, KeyCode::Char('/'));
    for c in "flaky".chars() {
        key(&mut screen, KeyCode::Char(c));
    }
    key(&mut screen, KeyCode::Enter);
    let text = view(&mut screen);
    assert!(text.contains("Retry flaky uploads") && !text.contains("CI workflow run failed"), "{text}");
    assert!(text.contains("flaky") && text.contains("Esc clears"), "{text}");
    key(&mut screen, KeyCode::Escape);
    let text = view(&mut screen);
    assert!(text.contains("CI workflow run failed") && !text.contains("Esc clears"), "{text}");
}
