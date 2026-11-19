#![cfg(unix)]
//! Create pull request, review drafts and deep links on every forge's fake world.
//!
//! - The Create sheet starts from the branch's commits and the base branch's template, and
//!   what it sends carries them (`n` on the Pull requests tab).
//! - Signed out, the same sheet opens the forge's own form, prefilled, in the browser.
//! - Inline comments wait as drafts that survive a restart (under the forge state directory,
//!   never in the repository) and go out as one review; on GitHub a draft covers a range.
//! - `dusty-git open <link>`: a pull request, issue or run link, or `#N`, opens that item.
//!
//! Each world runs in its own child process, as in `forge_labels`.

#[path = "../../../tests/support/forge_fake.rs"]
mod forge_fake;
#[path = "../../../tests/support/forge_world.rs"]
mod forge_world;

use dusty_core::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty_git::screen::GitScreen;
use dusty_ui::{KeyInputKind, UiEvent};
use forge_world::{Forge, World, git};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_FEATURES_CHILD";
const TEMPLATE: &str = "## Why this change\n\n## How it was tested\n";

fn rerun_in_child(test: &str, world: &World, adjust: impl FnOnce(&mut Command)) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("DUSTY_FEATURES_REPO", &world.repo)
        .env("DUSTY_FEATURES_STATE", world.state_path());
    world.configure(&mut command);
    adjust(&mut command);
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
    Some(PathBuf::from(
        std::env::var_os("DUSTY_FEATURES_REPO").unwrap(),
    ))
}

fn requests() -> Vec<Value> {
    forge_fake::requests(Path::new(
        &std::env::var_os("DUSTY_FEATURES_STATE").unwrap(),
    ))
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn control(screen: &mut GitScreen, code: KeyCode) {
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

fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait(screen: &mut GitScreen, text: &str) -> String {
    wait_for(screen, text, |frame| frame.contains(text))
}

/// How long a condition may take: generous, since a full suite runs many fake forges at once
/// and a passing wait returns as soon as its condition holds.
const DEADLINE: Duration = Duration::from_secs(120);

fn wait_for(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + DEADLINE;
    loop {
        screen.poll();
        let frame = view(screen);
        if done(&frame) {
            return frame;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Polls the screen until the fake forge logged a request `matches` accepts.
fn wait_request(screen: &mut GitScreen, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + DEADLINE;
    loop {
        screen.poll();
        if let Some(found) = requests().into_iter().find(|request| matches(request)) {
            return found;
        }
        assert!(Instant::now() < end, "no {what} request:\n{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn settle(screen: &mut GitScreen, millis: u64) {
    let quiet = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < quiet {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The row the native cursor is on.
fn cursor_row(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    frame
        .cursor
        .map(|(_, y, _)| frame.grid.row_text(y))
        .unwrap_or_default()
}

/// Moves the Files reader's cursor down until it sits on `text`.
fn cursor_to(screen: &mut GitScreen, text: &str) {
    for _ in 0..60 {
        if cursor_row(screen).contains(text) {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("the cursor never reached {text}:\n{}", view(screen));
}

fn body_of(request: &Value) -> Value {
    match &request["body"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::String(text.clone())),
        other => other.clone(),
    }
}

fn is_post(request: &Value, suffix: &str) -> bool {
    request["method"] == "POST"
        && request["path"]
            .as_str()
            .is_some_and(|path| path.ends_with(suffix))
}

/// Where a pull request's drafts live: the forge state directory, never the repository. The
/// host is the forge's own name for itself (the GitLab world answers as gitlab.com), so any
/// host directory holding `<repo>/<number>.json` counts.
fn draft_file(repo: &str, number: u64) -> Option<PathBuf> {
    let state = std::env::var_os("DUSTY_FORGE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap()).join("dusty/forge")
        });
    std::fs::read_dir(state.join("drafts"))
        .ok()?
        .flatten()
        .map(|host| {
            host.path()
                .join(repo.replace('/', "%2F"))
                .join(format!("{number}.json"))
        })
        .find(|path| path.is_file())
}

// ----------------------------------------------------------------------------- Create

fn template_path(forge: Forge) -> &'static str {
    match forge {
        Forge::GitHub => ".github/pull_request_template.md",
        Forge::GitLab => ".gitlab/merge_request_templates/Default.md",
        Forge::Gitea => ".gitea/pull_request_template.md",
        Forge::Forgejo => ".forgejo/pull_request_template.md",
    }
}

/// `main` with a template (also as `origin/main`), then `feature/retry-backoff` with two
/// commits, already pushed.
fn feature_branch(world: &World, template: bool) {
    let repo = &world.repo;
    std::fs::write(repo.join("README.md"), "Orbit\n").unwrap();
    if template {
        let path = repo.join(template_path(world.forge));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, TEMPLATE).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "Initial"]);
    git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(repo, &["switch", "-q", "-c", "feature/retry-backoff"]);
    std::fs::write(repo.join("retry.rs"), "fn retry() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "Add retry"]);
    std::fs::write(repo.join("retry_test.rs"), "fn retry_test() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "Test retry"]);
    git(
        repo,
        &[
            "update-ref",
            "refs/remotes/origin/feature/retry-backoff",
            "HEAD",
        ],
    );
}

fn create_world(forge: Forge) -> World {
    let world = match forge {
        Forge::Gitea | Forge::Forgejo => World::with_state(
            forge,
            json!({ "answers": { "POST repos/orbit/orbit/pulls": { "status": 201, "body": {
                "number": 4242, "title": "Retry backoff",
                "html_url": format!("https://{}/orbit/orbit/pulls/4242", forge.host()) } } } }),
        ),
        _ => World::new(forge),
    };
    feature_branch(&world, true);
    world
}

fn create_sheet(forge: Forge, test: &str) {
    let Some(repo) = child_repo() else {
        let world = create_world(forge);
        rerun_in_child(test, &world, |_| {});
        let created = world
            .requests()
            .into_iter()
            .filter(|request| is_post(request, "/pulls") || is_post(request, "/merge_requests"))
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1, "{created:?}");
        let body = body_of(&created[0]);
        let title = body["title"].as_str().unwrap_or_default();
        assert!(
            title.ends_with("Retry backoff"),
            "the branch names the request: {body}"
        );
        let description = body["body"]
            .as_str()
            .or(body["description"].as_str())
            .unwrap_or_default();
        let (list, template) = (
            description.find("- "),
            description.find("## Why this change"),
        );
        assert!(
            list.is_some() && template.is_some() && list < template,
            "the commits, then the template: {body}"
        );
        if forge == Forge::GitHub {
            assert_eq!(body["draft"], true, "Open as draft was ticked: {body}");
        }
        let (head, base) = match forge {
            Forge::GitLab => ("source_branch", "target_branch"),
            _ => ("head", "base"),
        };
        assert!(
            body[head]
                .as_str()
                .unwrap_or_default()
                .ends_with("feature/retry-backoff"),
            "{body}"
        );
        assert_eq!(body[base], "main", "{body}");
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    // The list names its forge repository once the repository read resolved it. A Create
    // sheet asked for before then is dropped when the repository changes under it.
    wait(&mut screen, forge.repo());
    key(&mut screen, KeyCode::Char('n'));
    let noun = if forge == Forge::GitLab {
        "merge request"
    } else {
        "pull request"
    };
    let sheet = wait_for(&mut screen, "the Create sheet", |frame| {
        frame.contains(&format!("Create {noun}")) && frame.contains("Retry backoff")
    });
    assert!(
        sheet.contains("feature/retry-backoff → main · 2 commits"),
        "{sheet}"
    );
    assert!(
        sheet.contains("## Why this change"),
        "the template follows the commits:\n{sheet}"
    );
    assert!(
        !sheet.contains("is not on"),
        "the branch is pushed:\n{sheet}"
    );
    // A same-repository branch that is already pushed: title, description, base, draft.
    for clutter in [
        "Base repository",
        "Head repository",
        "Head branch",
        "Publish branch",
        "Push ",
    ] {
        assert!(!sheet.contains(clutter), "{clutter}:\n{sheet}");
    }
    assert!(sheet.contains("☐ Open as draft"), "{sheet}");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Tab);
    }
    key(&mut screen, KeyCode::Char(' '));
    assert!(
        view(&mut screen).contains("☑ Open as draft"),
        "{}",
        view(&mut screen)
    );
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Review draft pull request");
    control(&mut screen, KeyCode::Enter);
    wait_request(&mut screen, "create", |request| {
        is_post(request, "/pulls") || is_post(request, "/merge_requests")
    });
    settle(&mut screen, 300);
}

#[test]
fn create_sheet_starts_from_the_branch_on_github() {
    create_sheet(
        Forge::GitHub,
        "create_sheet_starts_from_the_branch_on_github",
    );
}

#[test]
fn create_sheet_starts_from_the_branch_on_gitlab() {
    create_sheet(
        Forge::GitLab,
        "create_sheet_starts_from_the_branch_on_gitlab",
    );
}

#[test]
fn create_sheet_starts_from_the_branch_on_gitea() {
    create_sheet(Forge::Gitea, "create_sheet_starts_from_the_branch_on_gitea");
}

#[test]
fn create_sheet_starts_from_the_branch_on_forgejo() {
    create_sheet(
        Forge::Forgejo,
        "create_sheet_starts_from_the_branch_on_forgejo",
    );
}

/// `o` on a branch in the Git tab: one commit gives the subject and body; with no template the
/// body is the description, and an unpushed branch says so.
#[test]
fn branch_key_opens_the_create_sheet_for_that_branch() {
    let Some(repo) = child_repo() else {
        let world = World::new(Forge::GitHub);
        let repo = &world.repo;
        std::fs::write(repo.join("README.md"), "Orbit\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "Initial"]);
        git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(repo, &["switch", "-q", "-c", "fix/typo"]);
        std::fs::write(repo.join("README.md"), "Orbit.\n").unwrap();
        git(
            repo,
            &[
                "commit",
                "-q",
                "-am",
                "Fix the readme typo\n\nThe period was missing.",
            ],
        );
        rerun_in_child(
            "branch_key_opens_the_create_sheet_for_that_branch",
            &world,
            |_| {},
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    settle(&mut screen, 300);
    // `3` focuses the Branches pane; `fix/typo` sorts first.
    key(&mut screen, KeyCode::Char('3'));
    // The top bar names the current branch from the start; wait for its row in the list.
    wait_for(&mut screen, "the fix/typo row", |frame| {
        frame.matches("fix/typo").count() >= 2
    });
    key(&mut screen, KeyCode::Char('o'));
    let sheet = wait_for(&mut screen, "the Create sheet", |frame| {
        frame.contains("Create pull request")
    });
    assert!(sheet.contains("Fix the readme typo"), "{sheet}");
    assert!(sheet.contains("The period was missing."), "{sheet}");
    assert!(sheet.contains("fix/typo → main · 1 commit"), "{sheet}");
    // Not on the forge yet: pushing first is the default, and unticking it says why it matters.
    assert!(sheet.contains("☑ Push to origin first"), "{sheet}");
    assert!(
        !sheet.contains("Base repository") && !sheet.contains("Head branch"),
        "{sheet}"
    );
    for _ in 0..3 {
        key(&mut screen, KeyCode::Tab);
    }
    key(&mut screen, KeyCode::Char(' '));
    let unticked = view(&mut screen);
    assert!(unticked.contains("☐ Push to origin first"), "{unticked}");
    assert!(
        unticked.contains("fix/typo is not on example/demo yet"),
        "{unticked}"
    );
}

/// Signed out: the same fields open the forge's own form, prefilled, in the browser.
#[test]
fn signed_out_create_opens_the_forge_form_in_the_browser() {
    let Some(repo) = child_repo() else {
        let world = World::with_state(Forge::Gitea, json!({ "anonymous": { "limit": 60 } }));
        feature_branch(&world, true);
        let opened = world.directory.path().join("opened.txt");
        let browser = world.directory.path().join("browser.sh");
        std::fs::write(
            &browser,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" > '{}'\n",
                opened.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bin = world.directory.path().join("git-only");
        std::fs::create_dir_all(&bin).unwrap();
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        let _ = std::os::unix::fs::symlink(real, bin.join("git"));
        rerun_in_child(
            "signed_out_create_opens_the_forge_form_in_the_browser",
            &world,
            |command| {
                command
                    .env_remove("DUSTY_FORGE_TOKEN")
                    .env_remove("DUSTY_FORGE_ANONYMOUS")
                    .env("PATH", &bin)
                    .env("BROWSER", &browser);
            },
        );
        let end = Instant::now() + Duration::from_secs(10);
        while !opened.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let url = std::fs::read_to_string(&opened).unwrap();
        // The commits' list, then the template under it.
        assert!(
            url.starts_with("https://gitea.com/orbit/orbit/compare/main...feature/retry-backoff?expand=1&title=Retry%20backoff&body=-%20"),
            "{url}"
        );
        assert!(url.contains("%0A%0A%23%23%20Why%20this%20change"), "{url}");
        assert!(
            world
                .requests()
                .iter()
                .all(|request| request["method"] != "POST"),
            "nothing is written signed out"
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", None);
    wait(&mut screen, "Anonymous");
    key(&mut screen, KeyCode::Char('n'));
    let sheet = wait(&mut screen, "New pull request in the browser");
    assert!(sheet.contains("Retry backoff"), "{sheet}");
    assert!(
        sheet.contains("the forge's own form opens with these filled in"),
        "{sheet}"
    );
    assert!(sheet.contains("Open in browser"), "{sheet}");
    control(&mut screen, KeyCode::Enter);
    wait_for(&mut screen, "the browser", |frame| {
        frame.contains("Opening the browser") || frame.contains("Opened in browser")
    });
    settle(&mut screen, 500);
}

// ----------------------------------------------------------------------------- Drafts

const RANGE_DIFF: &str = "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1,4 @@\n-fn before() {}\n+fn alpha() {}\n+fn beta() {}\n+fn gamma() {}\n+fn delta() {}\n";

/// Opens a pull request's Files tab with the reader focused.
fn open_files(screen: &mut GitScreen, number: u64, first: &str) {
    screen.open_target("prs", Some(number));
    wait(screen, &format!("{number}"));
    settle(screen, 300);
    key(screen, KeyCode::Char('d'));
    wait(screen, first);
    settle(screen, 300);
    if screen.frame(190, 48).cursor.is_none() {
        control(screen, KeyCode::Char('l'));
    }
}

/// The Conversation's events in GitHub's words, through the provider, the
/// cache and the page: a link names its target, branch events name the branch.
#[test]
fn github_timeline_events_read_in_githubs_words() {
    let Some(repo) = child_repo() else {
        let event = |kind: &str, minute: u32, extra: Value| {
            let mut value = json!({"event": kind, "actor": {"login": "maya"},
                                   "created_at": format!("2026-11-08T10:{minute:02}:00Z")});
            for (key, field) in extra.as_object().cloned().unwrap_or_default() {
                value[key] = field;
            }
            value
        };
        let pull = json!([
            event(
                "renamed",
                1,
                json!({"rename": {"from": "Native drafts", "to": "Native review"}})
            ),
            event("connected", 2, json!({})),
            event(
                "labeled",
                3,
                json!({"label": {"name": "native", "color": "0e8a16"}})
            ),
            event(
                "base_ref_changed",
                4,
                json!({"previous_ref_name": "release", "current_ref_name": "main"})
            ),
            event("convert_to_draft", 5, json!({})),
            event("ready_for_review", 6, json!({})),
            event(
                "review_request_removed",
                7,
                json!({"requested_reviewer": {"login": "alex"}})
            ),
            event(
                "head_ref_force_pushed",
                8,
                json!({"before": "1a2b3c4d5e", "after": "5d6e7f8a9b"})
            ),
            event("referenced", 9, json!({"commit_id": "9f8e7d6c5b"})),
            event("locked", 10, json!({"lock_reason": "off-topic"})),
            event(
                "deployed",
                11,
                json!({"deployment": {"environment": "production"}})
            ),
            event("merged", 12, json!({"commit_id": "1a2b3c4d5e"})),
            event("head_ref_deleted", 13, json!({})),
            event("subscribed", 14, json!({})),
        ]);
        let issue = json!([
            event("connected", 1, json!({})),
            event("pinned", 2, json!({})),
            event("closed", 3, json!({"state_reason": "not_planned"})),
        ]);
        let world = World::with_state(
            Forge::GitHub,
            json!({ "timelines": { "9": pull, "7": issue } }),
        );
        git(
            &world.repo,
            &["commit", "-q", "--allow-empty", "-m", "Initial"],
        );
        rerun_in_child(
            "github_timeline_events_read_in_githubs_words",
            &world,
            |_| {},
        );
        return;
    };
    let tall = |screen: &mut GitScreen| {
        let frame = screen.frame(190, 140);
        (0..140)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let line = |page: &str, words: &str| -> String {
        page.lines()
            .find(|line| line.contains(words))
            .unwrap_or_else(|| panic!("no {words:?}:\n{page}"))
            .to_owned()
    };
    // An event's whole sentence: its row from the actor on, and the rows that continue it
    // under the actor (a long sentence wraps), up to the sidebar's column.
    let sentence = |page: &str, start: &str| -> String {
        let lines = page
            .lines()
            .map(|line| line.chars().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let at = page
            .lines()
            .position(|line| line.contains(start))
            .unwrap_or_else(|| panic!("no {start:?}:\n{page}"));
        let text = page.lines().nth(at).unwrap();
        let column = text[..text.find(start).unwrap()].chars().count();
        let piece = |chars: &[char]| -> String {
            let rest = chars
                .get(column..)
                .unwrap_or_default()
                .iter()
                .collect::<String>();
            rest.split("   ")
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let mut whole = piece(&lines[at]);
        for chars in &lines[at + 1..] {
            let continues = column >= 2
                && chars.get(column - 2) == Some(&' ')
                && chars
                    .get(column)
                    .is_some_and(|ch| !ch.is_whitespace() && *ch != '│');
            if !continues {
                break;
            }
            whole = format!("{whole} {}", piece(chars));
        }
        whole
    };
    // A link sentence and its target, which reads on the row below when it does not fit.
    let with_next = |page: &str, words: &str| -> String {
        let lines = page.lines().collect::<Vec<_>>();
        let at = lines
            .iter()
            .position(|line| line.contains(words))
            .unwrap_or_else(|| panic!("no {words:?}:\n{page}"));
        format!(
            "{}\n{}",
            lines[at],
            lines.get(at + 1).copied().unwrap_or_default()
        )
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.open_target("prs", Some(9));
    wait(&mut screen, "changed the title");
    settle(&mut screen, 300);
    let page = tall(&mut screen);
    let linked = with_next(
        &page,
        "maya linked an issue that may be closed by this pull request:",
    );
    assert!(linked.contains("#7 Preserve keyboard drafts"), "{linked}");
    assert!(
        sentence(&page, "maya changed the title").contains("Native drafts → Native review"),
        "{page}"
    );
    let label = sentence(&page, "maya added the");
    assert!(
        label.contains("native") && label.trim_end().contains("label"),
        "{label}"
    );
    for words in [
        "maya changed the base branch from release to main",
        "maya marked this pull request as draft",
        "maya marked this pull request as ready for review",
        "maya removed the request for review from alex",
        "maya force-pushed the feature/native branch from 1a2b3c4 to 5d6e7f8",
        "maya added a commit that referenced this pull request 9f8e7d6",
        "maya locked as off-topic and limited conversation to collaborators",
        "maya deployed to production",
        "maya merged commit 1a2b3c4 into main",
        "maya deleted the feature/native branch",
    ] {
        // The longest start of the sentence on one row tells it from its neighbours.
        let parts = words.split(' ').collect::<Vec<_>>();
        let first = (3..=parts.len())
            .rev()
            .map(|n| parts[..n].join(" "))
            .find(|start| page.contains(start.as_str()))
            .unwrap_or_else(|| panic!("no {words:?}:\n{page}"));
        let whole = sentence(&page, &first);
        assert!(
            whole.starts_with(words),
            "{words:?} reads {whole:?}:\n{page}"
        );
    }
    assert!(
        !page.contains("subscribed") && !page.contains("an issue or pull request"),
        "{page}"
    );
    screen.open_target("issues", Some(7));
    wait(&mut screen, "pinned this issue");
    settle(&mut screen, 300);
    let page = tall(&mut screen);
    let linked = with_next(
        &page,
        "maya linked a pull request that will close this issue:",
    );
    assert!(linked.contains("#9 Native review"), "{linked}");
    line(&page, "maya closed this as not planned");
}

/// GitHub: a range (`V`, move, `c`) and a single line wait as drafts, survive a restart and go
/// out as one review with both comments.
#[test]
fn github_range_drafts_survive_a_restart_and_send_as_one_review() {
    let Some(repo) = child_repo() else {
        let world = World::with_state(Forge::GitHub, json!({ "diff": RANGE_DIFF }));
        git(
            &world.repo,
            &["commit", "-q", "--allow-empty", "-m", "Initial"],
        );
        rerun_in_child(
            "github_range_drafts_survive_a_restart_and_send_as_one_review",
            &world,
            |_| {},
        );
        let reviews = world
            .requests()
            .into_iter()
            .filter(|request| is_post(request, "pulls/9/reviews"))
            .collect::<Vec<_>>();
        assert_eq!(reviews.len(), 1, "{reviews:?}");
        let body = body_of(&reviews[0]);
        let comments = body["comments"]
            .as_array()
            .unwrap_or_else(|| panic!("{body}"));
        assert_eq!(comments.len(), 2, "{body}");
        let range = comments
            .iter()
            .find(|comment| comment["body"] == "Name these four")
            .unwrap();
        assert_eq!(range["start_line"], 2, "{body}");
        assert_eq!(range["line"], 4, "{body}");
        let single = comments
            .iter()
            .find(|comment| comment["body"] == "First line")
            .unwrap();
        assert_eq!(single["line"], 1, "{body}");
        assert!(
            single.get("start_line").is_none_or(Value::is_null),
            "{body}"
        );
        assert!(!world.repo.join(".git/dusty").exists());
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    open_files(&mut screen, 9, "fn alpha()");
    cursor_to(&mut screen, "fn beta()");
    key(&mut screen, KeyCode::Char('V'));
    wait(&mut screen, "Range from line 2");
    // `c` on a review thread while a range is open neither replies nor drops the range.
    key(&mut screen, KeyCode::Home);
    cursor_to(&mut screen, "Please keep this draft.");
    key(&mut screen, KeyCode::Char('c'));
    let refused = wait(&mut screen, "Move to a line of the diff to end the range");
    assert!(!refused.contains("Reply to @"), "{refused}");
    cursor_to(&mut screen, "fn delta()");
    // While the range is open its keys lead the footer; `C` comments on it.
    let open = view(&mut screen);
    assert!(
        open.contains("extend") && open.contains("cancel range"),
        "{open}"
    );
    key(&mut screen, KeyCode::Char('C'));
    wait(&mut screen, "Comment on example.rs:2–4");
    paste(&mut screen, "Name these four");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Added to your review · 1 pending comment");
    // A second comment joins the review: once one is pending the sheet starts on GitHub's
    // Add review comment.
    key(&mut screen, KeyCode::Home);
    cursor_to(&mut screen, "fn alpha()");
    key(&mut screen, KeyCode::Char('c'));
    let sheet = wait(&mut screen, "Comment on example.rs:1");
    assert!(sheet.contains("Add review comment"), "{sheet}");
    paste(&mut screen, "First line");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "2 pending comments");
    settle(&mut screen, 200);
    let file = draft_file("example/demo", 9).expect("the drafts are saved");
    assert!(
        file.starts_with(std::env::var_os("DUSTY_FORGE_STATE_DIR").unwrap()),
        "{}",
        file.display()
    );
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["drafts"].as_array().unwrap().len(), 2, "{saved}");

    // A new session finds them where they were.
    drop(screen);
    let mut screen = GitScreen::open(&repo).unwrap();
    open_files(&mut screen, 9, "fn alpha()");
    let files = wait(&mut screen, "Name these four");
    // GitHub's Pending label on each held comment, and the count on Review changes.
    let card = files
        .lines()
        .find(|row| row.contains("lines 2–4"))
        .unwrap_or_default();
    assert!(card.contains("Pending"), "{files}");
    assert!(files.contains("Review changes · 2 pending"), "{files}");
    key(&mut screen, KeyCode::Char('r'));
    let sheet = wait(&mut screen, "Review #9");
    assert!(sheet.contains("Pending comments (2)"), "{sheet}");
    assert!(
        sheet.contains("Submits to #9 · 2 pending comments"),
        "{sheet}"
    );
    assert!(
        sheet.contains("Comment") && sheet.contains("Approve") && sheet.contains("Request changes"),
        "{sheet}"
    );
    // Drafts make a review on their own: no summary needed.
    control(&mut screen, KeyCode::Enter);
    wait_request(&mut screen, "review", |request| {
        is_post(request, "pulls/9/reviews")
    });
    let end = Instant::now() + Duration::from_secs(10);
    while file.exists() && Instant::now() < end {
        screen.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !file.exists(),
        "sent drafts are gone:\n{}",
        view(&mut screen)
    );
}

const SUGGESTED_FILE: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\n";

/// GitHub: a ```suggestion block reads as the change it suggests, in a review thread and in a
/// pending comment, and `Commit suggestion` writes it to the head branch. Fixture
/// only: the contents API of the fake forge.
#[test]
fn github_suggestions_read_as_diffs_and_commit_to_the_head_branch() {
    let Some(repo) = child_repo() else {
        let thread = json!({
            "id": "THREAD_S",
            "isResolved": false,
            "startLine": 2,
            "comments": { "nodes": [{
                "author": { "login": "maya" },
                "body": "Fold these two.\r\n```suggestion\r\nfn beta_and_gamma() {}\r\n```",
                "path": "example.rs",
                "line": 3,
                "originalLine": 3,
                "diffHunk": "@@ -1 +1,4 @@\n-fn before() {}\n+fn alpha() {}\n+fn beta() {}\n+fn gamma() {}",
            }] },
        });
        let world = World::with_state(
            Forge::GitHub,
            json!({
                "diff": RANGE_DIFF,
                "contents": { "example.rs": SUGGESTED_FILE },
                "review_threads": { "example/demo#9": [thread] },
            }),
        );
        git(
            &world.repo,
            &["commit", "-q", "--allow-empty", "-m", "Initial"],
        );
        rerun_in_child(
            "github_suggestions_read_as_diffs_and_commit_to_the_head_branch",
            &world,
            |_| {},
        );
        let requests = world.requests();
        let put = requests
            .iter()
            .find(|request| {
                request["method"] == "PUT"
                    && request["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("contents/example.rs"))
            })
            .unwrap_or_else(|| panic!("no commit: {requests:?}"));
        let body = body_of(put);
        assert_eq!(body["branch"], "feature/native", "{body}");
        assert_eq!(
            body["message"], "Apply suggestion from code review",
            "{body}"
        );
        assert_eq!(
            world.state()["contents"]["example.rs"],
            "fn alpha() {}\nfn beta_and_gamma() {}\nfn delta() {}\n"
        );
        // Nothing reached the forge for the pending comment.
        assert!(
            !requests
                .iter()
                .any(|request| is_post(request, "pulls/9/reviews")),
            "{requests:?}"
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    // The Conversation reads the suggestion as a diff too, with Commit suggestion under it.
    screen.open_target("prs", Some(9));
    let conversation = wait_for(&mut screen, "the suggestion in the Conversation", |frame| {
        frame.contains("› Commit suggestion")
    });
    for row in [
        "Suggested change",
        "− fn beta() {}",
        "− fn gamma() {}",
        "+ fn beta_and_gamma() {}",
        "› Commit suggestion",
    ] {
        assert!(conversation.contains(row), "{row}:\n{conversation}");
    }
    open_files(&mut screen, 9, "fn alpha()");
    let files = wait(&mut screen, "Suggested change");
    for row in [
        "− fn beta() {}",
        "− fn gamma() {}",
        "+ fn beta_and_gamma() {}",
    ] {
        assert!(files.contains(row), "{row}:\n{files}");
    }
    // A pending comment's suggestion reads against the lines on screen.
    key(&mut screen, KeyCode::Home);
    cursor_to(&mut screen, "fn gamma()");
    key(&mut screen, KeyCode::Char('V'));
    wait(&mut screen, "Range from line 3");
    cursor_to(&mut screen, "fn delta()");
    key(&mut screen, KeyCode::Char('C'));
    wait(&mut screen, "Comment on example.rs:3–4");
    paste(&mut screen, "```suggestion\nfn gamma_delta() {}\n```");
    control(&mut screen, KeyCode::Enter);
    let files = wait(&mut screen, "+ fn gamma_delta() {}");
    assert!(files.contains("− fn delta() {}"), "{files}");
    // Commit the thread's suggestion: the sheet says where it goes and what it changes.
    // In the diff the thread's actions are rows under it, Commit suggestion with them.
    key(&mut screen, KeyCode::Home);
    cursor_to(&mut screen, "Fold these two.");
    let thread = view(&mut screen);
    assert!(
        thread.contains("› Resolve conversation") && thread.contains("› Commit suggestion"),
        "{thread}"
    );
    cursor_to(&mut screen, "› Commit suggestion");
    key(&mut screen, KeyCode::Enter);
    let sheet = wait(&mut screen, "Commit suggestion to example.rs:2–3");
    assert!(
        sheet.contains("Commits to feature/native · replaces 2 lines with 1 line"),
        "{sheet}"
    );
    control(&mut screen, KeyCode::Enter);
    wait_request(&mut screen, "commit", |request| {
        request["method"] == "PUT"
            && request["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("contents/example.rs"))
    });
    settle(&mut screen, 300);
}

fn single_draft(forge: Forge, test: &str) {
    let number = forge.pull();
    let Some(repo) = child_repo() else {
        let world = match forge {
            Forge::Gitea | Forge::Forgejo => World::with_state(
                forge,
                json!({ "answers": { format!("POST repos/orbit/orbit/pulls/{number}/reviews"): {
                    "status": 200, "body": { "id": 1, "state": "COMMENT" } } } }),
            ),
            _ => World::new(forge),
        };
        git(
            &world.repo,
            &["commit", "-q", "--allow-empty", "-m", "Initial"],
        );
        rerun_in_child(test, &world, |_| {});
        let requests = world.requests();
        match forge {
            Forge::GitLab => {
                let note = requests
                    .iter()
                    .find(|request| is_post(request, "/draft_notes"))
                    .expect("a draft note");
                assert_eq!(body_of(note)["note"], "Check this line", "{note}");
                assert!(
                    requests
                        .iter()
                        .any(|request| is_post(request, "/draft_notes/bulk_publish"))
                );
            }
            _ => {
                let review = requests
                    .iter()
                    .find(|request| is_post(request, &format!("pulls/{number}/reviews")))
                    .expect("one review");
                let body = body_of(review);
                let comments = body["comments"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{body}"));
                assert_eq!(comments.len(), 1, "{body}");
                assert_eq!(comments[0]["body"], "Check this line", "{body}");
            }
        }
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    open_files(&mut screen, number, "Files");
    // The reader starts on the first changed file's first line of code.
    let mut opened = false;
    for _ in 0..40 {
        key(&mut screen, KeyCode::Char('c'));
        if view(&mut screen).contains("Comment on ") {
            opened = true;
            break;
        }
        key(&mut screen, KeyCode::Escape);
        key(&mut screen, KeyCode::Down);
    }
    assert!(opened, "no line takes a comment:\n{}", view(&mut screen));
    paste(&mut screen, "Check this line");
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    assert!(
        view(&mut screen).contains("Start a review"),
        "{}",
        view(&mut screen)
    );
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Added to your review · 1 pending comment");
    settle(&mut screen, 200);
    let file = draft_file(forge.repo(), number).expect("the draft is saved");

    drop(screen);
    let mut screen = GitScreen::open(&repo).unwrap();
    open_files(&mut screen, number, "Files");
    wait(&mut screen, "Check this line");
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, "1 pending comment");
    control(&mut screen, KeyCode::Enter);
    match forge {
        Forge::GitLab => wait_request(&mut screen, "publish", |request| {
            is_post(request, "/draft_notes/bulk_publish")
        }),
        _ => wait_request(&mut screen, "review", |request| {
            is_post(request, &format!("pulls/{number}/reviews"))
        }),
    };
    let end = Instant::now() + Duration::from_secs(10);
    while file.exists() && Instant::now() < end {
        screen.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !file.exists(),
        "sent drafts are gone:\n{}",
        view(&mut screen)
    );
}

#[test]
fn gitlab_drafts_survive_a_restart_and_publish_together() {
    single_draft(
        Forge::GitLab,
        "gitlab_drafts_survive_a_restart_and_publish_together",
    );
}

#[test]
fn gitea_drafts_survive_a_restart_and_send_as_one_review() {
    single_draft(
        Forge::Gitea,
        "gitea_drafts_survive_a_restart_and_send_as_one_review",
    );
}

#[test]
fn forgejo_drafts_survive_a_restart_and_send_as_one_review() {
    single_draft(
        Forge::Forgejo,
        "forgejo_drafts_survive_a_restart_and_send_as_one_review",
    );
}

// ----------------------------------------------------------------------------- Deep links

/// Each link opens its item; `#N` that is not a pull request falls back to the issue.
fn deep_links(forge: Forge, test: &str, state: Value, links: &[(String, &str)]) {
    let Some(repo) = child_repo() else {
        let world = World::with_state(forge, state);
        git(
            &world.repo,
            &["commit", "-q", "--allow-empty", "-m", "Initial"],
        );
        rerun_in_child(test, &world, |_| {});
        return;
    };
    for (link, expected) in links {
        let mut screen = GitScreen::open(&repo).unwrap();
        screen
            .open_link(link)
            .unwrap_or_else(|error| panic!("{link}: {error}"));
        let frame = wait(&mut screen, expected);
        settle(&mut screen, 300);
        let frame = format!("{frame}\n{}", view(&mut screen));
        assert!(
            !frame.contains("Could not resolve") && !frame.contains("That link is on"),
            "{link}:\n{frame}"
        );
    }
    // Another host is refused once the repository resolves; a repository link at once.
    let mut screen = GitScreen::open(&repo).unwrap();
    screen
        .open_link("https://example.org/someone/else/pull/3")
        .unwrap();
    wait(&mut screen, "That link is on example.org");
    let error = screen
        .open_link(&format!("https://{}/{}", forge.host(), forge.repo()))
        .unwrap_err();
    assert!(!error.is_empty());
}

#[test]
fn github_links_open_pull_requests_issues_and_runs() {
    let state = json!({
        "strict_numbers": true,
        "prs": [{ "number": 9, "title": "Linked pull needle" }],
        "issues": [{ "number": 7, "title": "Linked issue needle" }],
    });
    deep_links(
        Forge::GitHub,
        "github_links_open_pull_requests_issues_and_runs",
        state,
        &[
            (
                "https://github.com/example/demo/pull/9/files".into(),
                "Linked pull needle",
            ),
            (
                "https://github.com/example/demo/issues/7".into(),
                "Linked issue needle",
            ),
            ("#9".into(), "Linked pull needle"),
            ("#7".into(), "Linked issue needle"),
            ("example/demo#7".into(), "Linked issue needle"),
            (
                "https://github.com/example/demo/actions/runs/91".into(),
                "#91",
            ),
        ],
    );
}

#[test]
fn gitlab_links_open_merge_requests_and_issues() {
    deep_links(
        Forge::GitLab,
        "gitlab_links_open_merge_requests_and_issues",
        json!({}),
        &[
            (
                "https://gitlab.ci.test/orbit/team/orbit/-/merge_requests/13".into(),
                "Retry flaky artifact uploads",
            ),
            (
                "https://gitlab.ci.test/orbit/team/orbit/-/issues/14".into(),
                "Artifact uploads fail on nested groups",
            ),
            ("!13".into(), "Retry flaky artifact uploads"),
            ("#14".into(), "Artifact uploads fail on nested groups"),
        ],
    );
}

#[test]
fn gitea_links_open_pull_requests_and_issues() {
    deep_links(
        Forge::Gitea,
        "gitea_links_open_pull_requests_and_issues",
        json!({}),
        &[
            (
                "https://gitea.com/orbit/orbit/pulls/1402".into(),
                "add host.stable_workdir",
            ),
            (
                "https://gitea.com/orbit/orbit/issues/1393".into(),
                "actions/checkout fails in container",
            ),
            ("#1393".into(), "actions/checkout fails in container"),
        ],
    );
}

#[test]
fn forgejo_links_open_pull_requests_and_issues() {
    deep_links(
        Forge::Forgejo,
        "forgejo_links_open_pull_requests_and_issues",
        json!({}),
        &[
            (
                "https://code.forgejo.org/orbit/orbit/pulls/1784".into(),
                "set container daemon socket",
            ),
            (
                "https://code.forgejo.org/orbit/orbit/issues/1792".into(),
                "leaves shallow.lock",
            ),
            ("#1784".into(), "set container daemon socket"),
        ],
    );
}

/// `dusty-git open <link>` is the same as `--link`.
#[test]
fn the_binary_takes_open_with_a_link() {
    let binary = env!("CARGO_BIN_EXE_dusty-git");
    let output = Command::new(binary).args(["open"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage: dusty-git open <url|#number>")
    );
}
