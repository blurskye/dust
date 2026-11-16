#![cfg(unix)]
//! The forge tabs on their unhappy paths (DESIGN.md §3.46): no forge remote, signed out, a
//! refused or under-scoped token, a rate limit, no network, a server error, and the empty
//! states. Every screen says what happened in plain words with one next key, keeps what it
//! already showed, and never shows a raw error, Debug text or a token. Each case re-runs itself
//! in a child pointed at its own fake forge world.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_fake::FAKE_TOKEN;
use forge_world::{Forge, World};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_STATES_CHILD";

/// How the child reaches the forge.
#[derive(Clone, Copy, PartialEq)]
enum Access {
    Token,
    Anonymous,
}

fn rerun_in_child(test: &str, world: &World, access: Access) {
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
        .env("DUSTY_STATES_REPO", &world.repo)
        .env("DUSTY_STATES_FIXTURE", world.state_path());
    world.configure(&mut command);
    if access == Access::Anonymous {
        command.env_remove("DUSTY_FORGE_TOKEN").env_remove("DUSTY_FORGE_ANONYMOUS");
    }
    command.env("PATH", &bin);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if std::env::var_os("DUSTY_STATES_PRINT").is_some() {
        println!("{stdout}");
    }
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child_repo() -> Option<PathBuf> {
    std::env::var_os(CHILD)?;
    Some(PathBuf::from(std::env::var_os("DUSTY_STATES_REPO").unwrap()))
}

fn update(change: impl FnOnce(&mut Value)) {
    forge_fake::update(
        &PathBuf::from(std::env::var_os("DUSTY_STATES_FIXTURE").unwrap()),
        change,
    );
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

/// Poll until nothing has changed for a while and no read says it is still loading, or 20 s.
fn settle(screen: &mut GitScreen) {
    let end = Instant::now() + Duration::from_secs(20);
    let mut quiet = Instant::now();
    loop {
        if screen.poll() {
            quiet = Instant::now();
        }
        let frame = text(screen, 120, 36);
        let loading = frame.contains("Loading") || frame.contains("loading");
        // Ready when no read is queued or in flight and the screen has stopped changing: a
        // loaded machine finishes reads later, never in a different shape.
        if !screen.forge_quiet() {
            quiet = Instant::now();
        }
        if (!loading && quiet.elapsed() > Duration::from_millis(300)) || Instant::now() > end {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Text that only a raw error, a Debug value, a panic, a server's own JSON or a leaked secret
/// would put on screen. (Fixture code such as `Ok(())` or `src/lib.rs:12` is real content.)
const RAW: [&str; 21] = [
    "panicked",
    "RUST_BACKTRACE",
    "Error {",
    "{\"",
    "\":\"",
    "at src/",
    "fixture server error",
    "Traceback",
    "Bearer ",
    "ForgeError",
    "TransportError",
    "NotAuthenticated",
    "BadResponse",
    "Forbidden(",
    "Network error",
    "Bad credentials",
    "Bad Gateway",
    "error sending request",
    "retry in ",
    "authenticate with gh",
    "signed out?",
];

fn assert_plain(label: &str, frame: &str) {
    if std::env::var_os("DUSTY_STATES_PRINT").is_some() {
        for raw in RAW.iter().copied().chain([FAKE_TOKEN, "ghp_"]) {
            if frame.contains(raw) {
                println!("RAW {label}: {raw:?}");
            }
        }
        return;
    }
    for raw in RAW {
        assert!(!frame.contains(raw), "{label} shows raw text {raw:?}:\n{frame}");
    }
    assert!(!frame.contains(FAKE_TOKEN), "{label} shows the token:\n{frame}");
    assert!(!frame.contains("ghp_"), "{label} shows a token:\n{frame}");
}

/// Every forge screen once: the PR list, the reader's four tabs, Issues and an issue, Actions
/// and a run, Notifications. Each frame at 120×36 and 190×48.
fn tour(screen: &mut GitScreen, forge: Forge) -> Vec<(String, String, String)> {
    let mut frames = Vec::new();
    let mut shoot = |screen: &mut GitScreen, label: &str| {
        settle(screen);
        let small = text(screen, 120, 36);
        let large = text(screen, 190, 48);
        assert_plain(&format!("{label} 120×36"), &small);
        assert_plain(&format!("{label} 190×48"), &large);
        frames.push((label.to_owned(), small, large));
    };
    screen.open_target("prs", None);
    shoot(screen, "pr-list");
    screen.open_target("prs", Some(forge.pull()));
    shoot(screen, "pr-conversation");
    for tab in ["pr-commits", "pr-checks", "pr-files"] {
        key(screen, KeyCode::Char(']'));
        shoot(screen, tab);
    }
    screen.open_target("issues", None);
    shoot(screen, "issue-list");
    screen.open_target("issues", Some(forge.issue()));
    shoot(screen, "issue");
    screen.open_target("actions", None);
    shoot(screen, "actions");
    key(screen, KeyCode::Enter);
    shoot(screen, "run");
    screen.open_target("notifications", None);
    shoot(screen, "notifications");
    frames
}

fn print(state: &str, forge: Forge, frames: &[(String, String, String)]) {
    if std::env::var_os("DUSTY_STATES_PRINT").is_none() {
        return;
    }
    for (label, small, _) in frames {
        println!("===== {state} {} {label}\n{small}", forge.name());
    }
}

type Frames = Vec<(String, String, String)>;

/// Some screen of the tour shows `text`, at either size.
fn shows(frames: &Frames, text: &str) -> bool {
    frames.iter().any(|(_, small, large)| small.contains(text) || large.contains(text))
}

fn assert_shows(frames: &Frames, text: &str) {
    assert!(
        shows(frames, text),
        "no screen shows {text:?}:\n{}",
        frames.iter().map(|(label, small, _)| format!("== {label}\n{small}")).collect::<Vec<_>>().join("\n")
    );
}

fn assert_never(frames: &Frames, text: &str) {
    for (label, small, large) in frames {
        assert!(!small.contains(text) && !large.contains(text), "{label} shows {text:?}:\n{small}");
    }
}

/// The way in this forge names: its CLI, or its token settings page.
fn sign_in_words(forge: Forge) -> &'static str {
    match forge {
        Forge::GitHub => "gh auth login",
        Forge::GitLab => "glab auth login",
        Forge::Gitea | Forge::Forgejo => "/user/settings/applications",
    }
}

/// Another forge's sign-in never shows (no stock `gh auth login` on GitLab).
fn assert_own_sign_in(frames: &Frames, forge: Forge) {
    for other in ["gh auth login", "glab auth login", "tea login add"] {
        if other != sign_in_words(forge) {
            assert_never(frames, other);
        }
    }
}

/// The child's repository has a commit (a fresh `git init` would put Git's own
/// "bad revision 'HEAD'" in the status line, which is the Git tab's concern, not this one).
fn committed(repo: &std::path::Path) -> &std::path::Path {
    let head = std::process::Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .unwrap();
    if !head.status.success() {
        forge_world::git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
    }
    repo
}

/// A reply every API request gets, as `fail` rules in the fake (`server.py`).
fn always(status: u16, body: Value, headers: Value) -> Value {
    json!([{ "status": status, "body": body, "headers": headers, "times": 1_000_000 }])
}

fn unauthorized(forge: Forge) -> Value {
    let message = match forge {
        Forge::GitHub => "Bad credentials",
        Forge::GitLab => "401 Unauthorized",
        Forge::Gitea | Forge::Forgejo => "invalid username, password or token",
    };
    always(401, json!({ "message": message }), json!({}))
}

fn missing_scope(forge: Forge) -> Value {
    match forge {
        Forge::GitHub => always(
            403,
            json!({ "message": "Resource not accessible by personal access token" }),
            json!({ "X-OAuth-Scopes": "read:org", "X-Accepted-OAuth-Scopes": "repo, workflow" }),
        ),
        Forge::GitLab => always(
            403,
            json!({ "error": "insufficient_scope", "scope": "api",
                "error_description": "The request requires higher privileges than provided by the access token." }),
            json!({ "WWW-Authenticate": "Bearer realm=\"\", error=\"insufficient_scope\", scope=\"api\"" }),
        ),
        Forge::Gitea | Forge::Forgejo => always(
            403,
            json!({ "message": "token does not have at least one of required scope(s), required=[read:repository], token scope=read:user" }),
            json!({}),
        ),
    }
}

fn rate_limited(forge: Forge, reset: u64) -> Value {
    match forge {
        Forge::GitHub => always(
            403,
            json!({ "message": "API rate limit exceeded for user ID 1." }),
            json!({ "x-ratelimit-limit": "5000", "x-ratelimit-remaining": "0",
                "x-ratelimit-reset": reset.to_string(), "x-ratelimit-resource": "core" }),
        ),
        _ => always(
            429,
            json!({ "message": "Too Many Requests" }),
            json!({ "Retry-After": "720", "RateLimit-Reset": reset.to_string() }),
        ),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

macro_rules! per_forge {
    ($state:ident, $access:expr, $world:expr, $body:expr) => {
        mod $state {
            use super::*;
            fn run(test: &str, forge: Forge) {
                let Some(repo) = child_repo() else {
                    let world: fn(Forge) -> World = $world;
                    rerun_in_child(test, &world(forge), $access);
                    return;
                };
                let body: fn(PathBuf, Forge) = $body;
                body(repo, forge);
            }
            #[test]
            fn github() {
                run(concat!(stringify!($state), "::github"), Forge::GitHub);
            }
            #[test]
            fn gitlab() {
                run(concat!(stringify!($state), "::gitlab"), Forge::GitLab);
            }
            #[test]
            fn gitea() {
                run(concat!(stringify!($state), "::gitea"), Forge::Gitea);
            }
            #[test]
            fn forgejo() {
                run(concat!(stringify!($state), "::forgejo"), Forge::Forgejo);
            }
        }
    };
}

per_forge!(
    token_refused,
    Access::Token,
    |forge| World::with_state(forge, json!({ "fail": unauthorized(forge) })),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("401", forge, &frames);
        // A refused token says so (not "not signed in"), with the key and this forge's way in.
        assert_shows(&frames, "refused your token");
        assert_shows(&frames, "@ sign in");
        assert_shows(&frames, sign_in_words(forge));
        assert_own_sign_in(&frames, forge);
    }
);

per_forge!(
    scope_missing,
    Access::Token,
    |forge| World::with_state(forge, json!({ "fail": missing_scope(forge) })),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("403-scope", forge, &frames);
        // The token is known: the screens name the scope it lacks and where to add it.
        let (scopes, fix) = match forge {
            Forge::GitHub => ("needs the repo, workflow scopes", "gh auth refresh -s repo,workflow"),
            Forge::GitLab => ("needs the api scope", "/-/user_settings/personal_access_tokens"),
            Forge::Gitea | Forge::Forgejo => ("needs the read:repository scope", "/user/settings/applications"),
        };
        assert_shows(&frames, scopes);
        assert_shows(&frames, fix);
        assert_never(&frames, "Not signed in");
        assert_own_sign_in(&frames, forge);
    }
);

per_forge!(
    signed_out,
    Access::Anonymous,
    |forge| World::with_state(forge, json!({ "anonymous": { "limit": 60 } })),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("signed-out", forge, &frames);
        // What works anonymously stays readable; the sign-in key is on screen.
        assert_shows(&frames, "Anonymous · read-only");
        assert_shows(&frames, "@ sign in");
        assert_shows(&frames, &pull_ref(forge));
        assert_own_sign_in(&frames, forge);
        // `@` opens the accounts popover with this forge's own way in.
        key(&mut screen, KeyCode::Char('@'));
        let popover = text(&mut screen, 190, 48);
        assert!(popover.contains("Accounts") && popover.contains(sign_in_words(forge)), "{popover}");
        key(&mut screen, KeyCode::Escape);
        assert!(!text(&mut screen, 190, 48).contains("r re-check"), "Esc closes the popover");
    }
);

#[test]
fn signed_out_github_empty_list_shows_sign_in_once() {
    let Some(repo) = child_repo() else {
        let world = World::with_state(
            Forge::GitHub,
            json!({ "fail": always(403, json!({ "message": "Only signed in user is allowed to call APIs." }), json!({})) }),
        );
        rerun_in_child("signed_out_github_empty_list_shows_sign_in_once", &world, Access::Anonymous);
        return;
    };
    let mut screen = GitScreen::open(committed(&repo)).unwrap();
    screen.open_target("issues", None);
    settle(&mut screen);
    for (width, height, list_width) in [(120, 36, 48), (190, 48, 83)] {
        let frame = screen.frame(width, height);
        let list = (1..height - 1)
            .map(|y| (0..list_width).map(|x| frame.grid.get(x, y).unwrap().symbol.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // Said once, in the reader: the list beside it stays blank.
        for text in ["Sign-in required", "Not signed in", "needs a sign-in", "@ sign in"] {
            assert!(!list.contains(text), "the list repeats {text:?}:\n{list}");
        }
        let reader = (1..height - 1)
            .map(|y| (list_width..width).map(|x| frame.grid.get(x, y).unwrap().symbol.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(reader.contains("gh auth login"), "the reader keeps the sign-in commands:\n{reader}");
        assert_eq!(reader.matches("Not signed in").count(), 1, "the reader says it once:\n{reader}");
        // The footer offers what can fix it, no keys that need a row.
        let footer = frame.grid.row_text(height - 1);
        assert!(footer.contains("R retry") && footer.contains("@ sign in"), "{footer}");
        for text in ["Enter open", "C comment", "x close", "l labels"] {
            assert!(!footer.contains(text), "{text:?} with nothing to act on:\n{footer}");
        }
    }
}

per_forge!(
    rate_limited_keeps_cache,
    Access::Token,
    |forge| World::new(forge),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        tour(&mut screen, forge);
        drop(screen);
        let reset = now() + 720;
        update(|state| state["fail"] = rate_limited(forge, reset));
        let mut screen = reopen_and_refresh(&repo);
        let frames = tour(&mut screen, forge);
        print("rate", forge, &frames);
        // When it resets, and the cached view stays.
        assert_shows(&frames, "Rate limited");
        assert_shows(&frames, "resets in 1");
        assert_shows(&frames, &pull_ref(forge));
        assert_never(&frames, "· offline");
    }
);

per_forge!(
    offline_keeps_cache,
    Access::Token,
    |forge| World::new(forge),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        tour(&mut screen, forge);
        drop(screen);
        update(|state| state["offline"] = json!(true));
        let mut screen = reopen_and_refresh(&repo);
        let frames = tour(&mut screen, forge);
        print("offline", forge, &frames);
        // The cached view with an "Offline · showing …" marker; nothing loads forever.
        assert_shows(&frames, "Offline");
        assert_shows(&frames, "· showing ");
        assert_shows(&frames, &pull_ref(forge));
        assert_never(&frames, "Loading…");
        // The network comes back: the next refresh clears the marker.
        update(|state| state["offline"] = json!(false));
        screen.open_target("prs", None);
        key(&mut screen, KeyCode::Char('R'));
        settle(&mut screen);
        let back = text(&mut screen, 190, 48);
        assert!(!back.contains("Offline"), "{back}");
    }
);

per_forge!(
    server_error,
    Access::Token,
    |forge| World::new(forge),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        screen.open_target("prs", None);
        settle(&mut screen);
        update(|state| {
            state["fail"] = always(502, json!({ "message": "Bad Gateway" }), json!({}))
        });
        let frames = tour(&mut screen, forge);
        print("5xx", forge, &frames);
        // A server error mid-session: the forge's fault in words, what was read stays.
        assert_shows(&frames, "is having problems");
        assert_shows(&frames, "is having problems · R retry");
        assert_never(&frames, "· r retry");
        assert_shows(&frames, &pull_ref(forge));
        assert_never(&frames, "sign in ·");
        assert_never(&frames, "Loading…");
    }
);

per_forge!(
    no_forge_remote,
    Access::Token,
    |forge| {
        let world = World::new(forge);
        forge_world::git(&world.repo, &["remote", "remove", "origin"]);
        world
    },
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("no-remote", forge, &frames);
        assert_shows(&frames, "No GitHub, GitLab, Gitea or Forgejo remote in this repository");
        assert_shows(&frames, "git remote add origin");
        assert_never(&frames, "Loading…");
    }
);

per_forge!(
    unknown_host,
    Access::Token,
    |forge| {
        let world = World::new(forge);
        forge_world::git(
            &world.repo,
            &["remote", "set-url", "origin", "https://git.unknown.invalid/team/app.git"],
        );
        world
    },
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("unknown-host", forge, &frames);
        assert_shows(&frames, "Unknown site git.unknown.invalid");
        assert_never(&frames, "Loading…");
    }
);

fn control(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code,
            modifiers: dust::editor::input::Modifiers {
                control: true,
                ..Default::default()
            },
        },
        kind: KeyInputKind::Press,
    });
}

fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}

/// Shoot the current screen at both sizes, checked for raw text.
fn shot(screen: &mut GitScreen, label: &str) -> (String, String, String) {
    settle(screen);
    let small = text(screen, 120, 36);
    let large = text(screen, 190, 48);
    assert_plain(&format!("{label} 120×36"), &small);
    assert_plain(&format!("{label} 190×48"), &large);
    (label.to_owned(), small, large)
}

/// A canned 200 for every request whose path holds `path` (the fake's `fail` rule).
fn answer(path: &str, body: Value) -> Value {
    json!({ "status": 200, "body": body, "headers": {}, "times": 1_000_000, "path": path, "method": "GET" })
}

/// A new session on a cache the first one filled, asking the forge again (`R`): the cache is
/// seconds old, so nothing would check it otherwise. The first session reads everything
/// before it ends, so the second never depends on which reads the first left unfinished.
fn reopen_and_refresh(repo: &std::path::Path) -> GitScreen {
    let mut screen = GitScreen::open(repo).unwrap();
    screen.open_target("prs", None);
    settle(&mut screen);
    key(&mut screen, KeyCode::Char('R'));
    settle(&mut screen);
    screen
}

/// `#13`, or GitLab's `!13`.
fn pull_ref(forge: Forge) -> String {
    format!("{}{}", if matches!(forge, Forge::GitLab) { "!" } else { "#" }, forge.pull())
}

fn pull_noun(forge: Forge) -> &'static str {
    if forge == Forge::GitLab { "merge requests" } else { "pull requests" }
}

per_forge!(
    empty_states,
    Access::Token,
    |forge| {
        let none = json!({ "total_count": 0, "workflow_runs": [], "workflows": [] });
        World::with_state(
            forge,
            json!({ "filter_terms": true, "fail": [
                // Gitea and Forgejo filter by author on the forge (`created_by=`, or the issue search).
                { "status": 200, "body": [], "headers": { "X-Total-Count": "0" }, "times": 1_000_000,
                  "query": "nobody-states" },
                answer("notifications", json!([])),
                answer("todos", json!([])),
                answer("actions/runs", none.clone()),
                answer("actions/tasks", none.clone()),
                answer("actions/workflows", none),
                answer("pipelines", json!([])),
            ] }),
        )
    },
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let mut frames = Vec::new();
        // Zero results for a filter: say so, and offer the way back (GitHub's "Clear current
        // search query, filters, and sorts").
        screen.open_target("prs", None);
        settle(&mut screen);
        key(&mut screen, KeyCode::Char('/'));
        paste(&mut screen, "author:nobody-states");
        control(&mut screen, KeyCode::Enter);
        frames.push(shot(&mut screen, "pr-filter-empty"));
        // An empty inbox, and a repository without workflows.
        screen.open_target("notifications", None);
        frames.push(shot(&mut screen, "inbox-empty"));
        screen.open_target("actions", None);
        frames.push(shot(&mut screen, "actions-empty"));
        print("empty", forge, &frames);
        assert_shows(&frames, &format!("No {} match this filter", pull_noun(forge)));
        assert_shows(&frames, "Clear the filter");
        assert_shows(&frames, "Inbox zero");
        assert_never(&frames, "Loading…");
    }
);

per_forge!(
    expired_log,
    Access::Token,
    |forge| World::with_state(
        forge,
        json!({ "fail": [{ "status": 410, "body": { "message": "Gone" }, "times": 1_000_000, "path": "logs" },
                         { "status": 410, "body": { "message": "Gone" }, "times": 1_000_000, "path": "trace" }] }),
    ),
    |repo, forge| {
        let mut screen = GitScreen::open(committed(&repo)).unwrap();
        let frames = tour(&mut screen, forge);
        print("expired-log", forge, &frames);
        assert_never(&frames, "Loading…");
    }
);

/// Long content on GitHub's world: a 300-file pull request, a 10,000-line log and a
/// 200-comment thread. Nothing pins the UI: every frame paints in well under a second.
#[test]
fn long_content_never_pins_the_ui() {
    let forge = Forge::GitHub;
    let Some(repo) = child_repo() else {
        let mut diff = String::new();
        for file in 0..300 {
            diff.push_str(&format!(
                "diff --git a/src/f{file}.rs b/src/f{file}.rs\n--- a/src/f{file}.rs\n+++ b/src/f{file}.rs\n@@ -1 +1 @@\n-fn a{file}() {{}}\n+fn b{file}() {{}}\n"
            ));
        }
        let log: String = (0..10_000)
            .map(|line| format!("test (linux)\tUNKNOWN STEP\t2026-11-08T10:00:00.200Z line {line} of the build\n"))
            .collect();
        let comments: Vec<String> = (0..200).map(|index| format!("Comment number {index}")).collect();
        let world = World::with_state(forge, json!({ "diff": diff, "log": log, "comments": comments }));
        rerun_in_child("long_content_never_pins_the_ui", &world, Access::Token);
        return;
    };
    let mut screen = GitScreen::open(committed(&repo)).unwrap();
    let frames = tour(&mut screen, forge);
    print("long", forge, &frames);
    let started = Instant::now();
    for _ in 0..5 {
        text(&mut screen, 190, 48);
    }
    let each = started.elapsed() / 5;
    assert!(each < Duration::from_millis(500), "a frame took {each:?}");
    // Scrolling to the end of the long reader stays quick too.
    let started = Instant::now();
    key(&mut screen, KeyCode::Char('G'));
    text(&mut screen, 190, 48);
    assert!(started.elapsed() < Duration::from_secs(1), "G took {:?}", started.elapsed());
    // A 200-comment thread folds its middle the way GitHub does.
    screen.open_target("prs", Some(forge.pull()));
    settle(&mut screen);
    // Card by card from the top: the fold is the card after the first twenty items.
    key(&mut screen, KeyCode::Char('g'));
    let mut seen = String::new();
    let mut last = String::new();
    for _ in 0..80 {
        let frame = text(&mut screen, 190, 48);
        last.clone_from(&frame);
        assert!(!frame.contains("Comment number 100\n") && !frame.contains("Comment number 100 "), "the fold should hide the middle:\n{frame}");
        if frame.contains("hidden items") {
            seen = frame;
            break;
        }
        key(&mut screen, KeyCode::Char('j'));
    }
    if std::env::var_os("DUSTY_STATES_PRINT").is_some() {
        println!("===== long fold\n{seen}");
    }
    assert!(seen.contains("hidden items · Load more…"), "no fold in a 200-comment thread:\n{last}");
}
