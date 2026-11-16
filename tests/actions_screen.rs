#![cfg(unix)]
//! The Actions tab against the fake forge's Actions world
//! (`server.py --world actions`, octo/orbit on github.com over direct HTTP): the run history
//! beside the selected run's tree (jobs, steps, inline logs opened at the first error), its
//! identity across attempts, the full log's find counts and pan, artifacts with a download, and
//! the approve/cancel sheets. Each case re-runs itself in a child pointed at its own world.

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
    rerun_in(test, "actions", "{}", "https://github.com/octo/orbit.git");
}

/// The four provider shapes, each against the designed Actions world.
#[derive(Clone, Copy, Debug)]
enum Shape {
    GitHub,
    Gitea,
    Forgejo,
    GitLab,
}

impl Shape {
    fn world(self) -> &'static str {
        match self {
            Shape::GitHub => "actions",
            Shape::Gitea => "gitea",
            Shape::Forgejo => "forgejo",
            Shape::GitLab => "gitlab",
        }
    }
    fn state(self) -> Value {
        let mut state = serde_json::json!({ "actions_seed": "design", "viewer": "maya", "glab_auth": { "*": "token" } });
        let probe = |host: &str, version: &str| serde_json::json!({ host: { "api/v1/version": { "body": { "version": version } } } });
        match self {
            Shape::Gitea => state["probes"] = probe("gitea.com", "29.0.0+dev-160-g55186ca39b"),
            Shape::Forgejo => {
                state["probes"] = probe("code.forgejo.org", "17.0.0-26-363f3dc30b+gitea-1.22.0")
            }
            Shape::GitHub | Shape::GitLab => {}
        }
        state
    }
    fn remote(self) -> &'static str {
        match self {
            Shape::GitHub => "https://github.com/octo/orbit.git",
            Shape::Gitea => "https://gitea.com/orbit/orbit.git",
            Shape::Forgejo => "https://code.forgejo.org/orbit/orbit.git",
            Shape::GitLab => "https://gitlab.com/orbit/team/orbit.git",
        }
    }
}

fn rerun_in_shape(test: &str, shape: Shape) {
    rerun_in(
        test,
        shape.world(),
        &shape.state().to_string(),
        shape.remote(),
    );
}

fn rerun_in(test: &str, world: &str, seed: &str, remote: &str) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state.json");
    std::fs::write(&state, seed).unwrap();
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
        .env("DUSTY_ACTIONS_STATE", &state)
        .env("DUSTY_FORGE_TOKEN", FAKE_TOKEN)
        .env("FORGEJO_TOKEN", FAKE_TOKEN)
        .env("GITLAB_TOKEN", FAKE_TOKEN)
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
            && request["path"]
                .as_str()
                .is_some_and(|path| path.contains(fragment))
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

fn wait_for(
    screen: &mut GitScreen,
    size: (u16, u16),
    what: &str,
    done: impl Fn(&str) -> bool,
) -> String {
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

/// The failed run (the third row) selected, its tree loaded, and the run pane focused.
fn failed_run(screen: &mut GitScreen) {
    key(screen, KeyCode::Down);
    key(screen, KeyCode::Down);
    wait(screen, "exit code 101");
    key(screen, KeyCode::Enter);
}

#[test]
fn run_history_beside_the_run_tree_keeps_identity_across_attempts() {
    let Some(repo) = child() else {
        rerun_in_child("run_history_beside_the_run_tree_keeps_identity_across_attempts");
        return;
    };
    let mut screen = open(&repo);
    // The state filters only, each counted; `All` is said once, by the workflows pane.
    let wide = wait(&mut screen, "Failing 1");
    assert!(!wide.contains(" All 3 │"), "{wide}");
    for needle in [
        "Workflow runs",
        "All workflows",
        "Running ",
        "Retry flaky uploads",
        "CI · #9 · ",
        "Fix typo",
        "Release v1.4",
    ] {
        assert!(wide.contains(needle), "{needle}:\n{wide}");
    }
    for raw in [
        "failure",
        "success",
        "in_progress",
        "Select a workflow",
        "view rows",
    ] {
        assert!(!wide.contains(raw), "{raw}:\n{wide}");
    }
    // Moving down the history shows each run beside it, without Enter: the failed run opens
    // at its failing job, its failing step and the step's first error line.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    assert!(cursor_row(&mut screen, 190, 48).contains("Retry flaky uploads"));
    let run = wait(&mut screen, "exit code 101");
    for needle in [
        "✕ Retry flaky uploads",
        "Run #10 · attempt 2 of 2 · #9 · retry-uploads",
        "Run cargo test",
        "$ cargo test",
        "test retries_uploads ... FAILED",
    ] {
        assert!(run.contains(needle), "{needle}:\n{run}");
    }
    assert!(
        !run.contains("[31m") && !run.contains("##[error]"),
        "SGR and markers are rendered:\n{run}"
    );
    // A passing job stays closed: its steps are not listed.
    assert!(
        run.contains("build") && !run.contains("Run cargo build --release"),
        "{run}"
    );
    // At 100×30 both panels stay; below 90 columns the list and the run take turns.
    let compact = text(&mut screen, 100, 30);
    assert!(
        compact.contains("Retry flaky uploads") && compact.contains("Run cargo test"),
        "{compact}"
    );
    let narrow = text(&mut screen, 80, 24);
    assert!(
        narrow.contains("Workflow runs") && !narrow.contains("Run cargo test"),
        "{narrow}"
    );
    // Enter focuses the run: the cursor rests on the first error.
    key(&mut screen, KeyCode::Enter);
    assert!(cursor_row(&mut screen, 190, 48).contains("test retries_uploads"));
    assert!(text(&mut screen, 80, 24).contains("Run cargo test"));
    // `a` lists the attempts; choosing the first reads that attempt, not the latest again.
    key(&mut screen, KeyCode::Char('a'));
    let attempts = wait(&mut screen, "Attempt 1");
    assert!(attempts.contains("Attempt 2"), "{attempts}");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    let earlier = wait(&mut screen, "attempt 1 of 2");
    assert!(requested("GET", "actions/runs/501/attempts/1"), "{earlier}");
    // Esc gives the history its focus back.
    key(&mut screen, KeyCode::Escape);
    assert!(cursor_row(&mut screen, 190, 48).contains("Retry flaky uploads"));
}

#[test]
fn full_log_find_counts_pan_and_wrap() {
    let Some(repo) = child() else {
        rerun_in_child("full_log_find_counts_pan_and_wrap");
        return;
    };
    let mut screen = open(&repo);
    failed_run(&mut screen);
    key(&mut screen, KeyCode::Char('f'));
    let full = wait(&mut screen, "Runner version");
    assert!(full.contains("running 3 tests"), "{full}");
    // `f` lands on the job's first error.
    assert!(cursor_row(&mut screen, 190, 48).contains("test retries_uploads"));
    // Find: smart case, counts from the cursor, n/N wrap around, Esc clears.
    key(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "test");
    key(&mut screen, KeyCode::Enter);
    let found = text(&mut screen, 190, 48);
    assert!(found.contains("4/4 matches"), "{found}");
    key(&mut screen, KeyCode::Char('n'));
    assert!(text(&mut screen, 190, 48).contains("1/4 matches"));
    key(&mut screen, KeyCode::Char('N'));
    key(&mut screen, KeyCode::Char('N'));
    assert!(text(&mut screen, 190, 48).contains("3/4 matches"));
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
    // Without a find, n and N jump between error lines.
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('n'));
    assert!(cursor_row(&mut screen, 190, 48).contains("test retries_uploads"));
    key(&mut screen, KeyCode::Char('n'));
    assert!(cursor_row(&mut screen, 190, 48).contains("exit code 101"));
    key(&mut screen, KeyCode::Char('N'));
    assert!(cursor_row(&mut screen, 190, 48).contains("test retries_uploads"));
    // Long lines wrap by default; `w` lets them run past the edge, where L and H pan them:
    // L moves 8 cells and marks the clipped start, H comes back.
    key(&mut screen, KeyCode::Char('w'));
    let before = cursor_row(&mut screen, 100, 30);
    key(&mut screen, KeyCode::Char('L'));
    let panned = cursor_row(&mut screen, 100, 30);
    assert!(
        panned.contains('‹') && panned != before,
        "{before}\n{panned}"
    );
    key(&mut screen, KeyCode::Char('H'));
    assert_eq!(cursor_row(&mut screen, 100, 30), before);
    // `w` again wraps, keeping every character on screen at the narrowest size.
    key(&mut screen, KeyCode::Char('w'));
    let wrapped = text(&mut screen, 64, 20);
    assert!(
        !wrapped.contains('›') || !wrapped.contains('‹'),
        "{wrapped}"
    );
    // `z` gives the run the whole width; Esc returns.
    key(&mut screen, KeyCode::Char('z'));
    assert!(!text(&mut screen, 190, 48).contains("Workflow runs"));
    key(&mut screen, KeyCode::Escape);
    assert!(text(&mut screen, 190, 48).contains("Workflow runs"));
    // Ctrl+s writes the log beside the repository.
    control(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, "Save job log");
    control(&mut screen, KeyCode::Enter);
    let saved = std::fs::read_to_string(repo.join("test.log")).unwrap();
    assert!(
        saved.contains("running 3 tests") && !saved.contains('\u{1b}'),
        "{saved}"
    );
}

#[test]
fn artifacts_node_lists_browses_and_downloads() {
    let Some(repo) = child() else {
        rerun_in_child("artifacts_node_lists_browses_and_downloads");
        return;
    };
    let mut screen = open(&repo);
    failed_run(&mut screen);
    // Artifacts are the tree's last node.
    wait(&mut screen, "Artifacts (3)");
    key(&mut screen, KeyCode::End);
    assert!(cursor_row(&mut screen, 190, 48).contains("Artifacts (3)"));
    key(&mut screen, KeyCode::Enter);
    let listed = wait(&mut screen, "orbit-linux");
    for needle in ["coverage", "old-logs", "expired"] {
        assert!(listed.contains(needle), "{needle}:\n{listed}");
    }
    // Enter opens an archive's files (Space leads chords now); `s` downloads it.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    let files = wait(&mut screen, "lcov.info");
    assert!(files.contains("coverage/index.html"), "{files}");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Down);
    }
    assert!(cursor_row(&mut screen, 190, 48).contains("old-logs"));
    key(&mut screen, KeyCode::Enter);
    assert!(text(&mut screen, 190, 48).contains("This artifact has expired"));
    for _ in 0..3 {
        key(&mut screen, KeyCode::Up);
    }
    key(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, "Destination folder");
    control(&mut screen, KeyCode::Enter);
    let receipt = wait(&mut screen, "Downloaded");
    assert!(receipt.contains("artifacts/coverage.zip"), "{receipt}");
    assert!(repo.join("artifacts/coverage.zip").is_file());
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
    let waiting = wait(&mut screen, "Waiting · no jobs have started");
    assert!(waiting.contains("Waiting for approval"), "{waiting}");
    key(&mut screen, KeyCode::Char('A'));
    wait(&mut screen, "Approve run #11 · Fix typo?");
    assert!(!requested("POST", "approve"));
    control(&mut screen, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(30);
    while !requested("POST", "actions/runs/502/approve") {
        screen.poll();
        assert!(
            Instant::now() < end,
            "{}\n{:?}",
            text(&mut screen, 190, 48),
            requests()
                .iter()
                .map(|r| format!("{} {}", r["method"], r["path"]))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    wait(&mut screen, "Saved on GitHub");
    // The running release can be cancelled.
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('x'));
    // The sheet names the run as its header does, says what cancelling does, and starts on
    // `Keep running`: a stray Enter closes it and cancels nothing.
    let sheet = wait(&mut screen, "Cancel run #12 · Release v1.4?");
    for needle in [
        "Running jobs stop. Finished jobs keep their logs.",
        " Cancel run ",
        " Keep running ",
        "Enter keep running",
    ] {
        assert!(sheet.contains(needle), "{needle}:\n{sheet}");
    }
    key(&mut screen, KeyCode::Enter);
    assert!(!text(&mut screen, 190, 48).contains("Cancel run #12"));
    assert!(!requested("POST", "actions/runs/503/cancel"));
    key(&mut screen, KeyCode::Char('x'));
    wait(&mut screen, "Cancel run #12 · Release v1.4?");
    control(&mut screen, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(30);
    while !requested("POST", "actions/runs/503/cancel") {
        screen.poll();
        assert!(
            Instant::now() < end,
            "{}\n{:?}",
            text(&mut screen, 190, 48),
            requests()
                .iter()
                .map(|r| format!("{} {}", r["method"], r["path"]))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// R9-05: Enter on a pull request's check opens its run in Actions; the run's title says where
/// it came from and Esc goes back to the same pull request, tab and line.
#[test]
fn a_check_opens_its_run_and_esc_goes_back_to_the_pull_request() {
    let Some(repo) = child() else {
        rerun_in_child("a_check_opens_its_run_and_esc_goes_back_to_the_pull_request");
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    key(&mut screen, KeyCode::Char('>'));
    wait(&mut screen, "Retry flaky uploads");
    // The row under the terminal cursor, if a pane has one.
    let marked = |screen: &mut GitScreen| {
        let frame = screen.frame(190, 48);
        frame
            .cursor
            .map(|(_, y, _)| frame.grid.row_text(y))
            .unwrap_or_default()
    };
    // Pull request #9 (the one with checks; the oldest of four), its reader, then Conversation →
    // Commits → Checks.
    for _ in 0..6 {
        if marked(&mut screen).contains("Retry flaky uploads") {
            break;
        }
        key(&mut screen, KeyCode::Char('j'));
    }
    let row = marked(&mut screen);
    assert!(
        row.contains("Retry flaky uploads"),
        "{row:?}\n{}",
        text(&mut screen, 190, 48)
    );
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char(']'));
    key(&mut screen, KeyCode::Char(']'));
    // A check with a run opens it on Enter; the footer names that (r12: no `↗ logs` column).
    wait(&mut screen, "passed");
    for _ in 0..20 {
        if text(&mut screen, 190, 48).contains("Enter open run") {
            break;
        }
        key(&mut screen, KeyCode::Char('j'));
    }
    // Beside the job list the selected job's steps and log (F-PRCHECKS): the line is taken
    // once its log is in, so it reads the same when Esc brings the pull request back.
    wait(&mut screen, "test retries_uploads ... FAILED");
    let line = marked(&mut screen);
    assert!(
        text(&mut screen, 190, 48).contains("Enter open run"),
        "{line:?}\n{}",
        text(&mut screen, 190, 48)
    );
    key(&mut screen, KeyCode::Enter);
    let run = wait(&mut screen, "Esc back to #9");
    assert!(run.contains("from #9 · Checks"), "{run}");
    key(&mut screen, KeyCode::Escape);
    let back = wait_for(
        &mut screen,
        (190, 48),
        "the pull request's checks",
        |frame| !frame.contains("back to #9"),
    );
    assert!(back.contains("Retry flaky uploads"), "{back}");
    // The same check line under the reader's cursor once the reader has settled.
    let reader = |row: &str| row.split("││").nth(1).unwrap_or_default().to_owned();
    let end = Instant::now() + Duration::from_secs(10);
    while reader(&marked(&mut screen)) != reader(&line) && Instant::now() < end {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        reader(&marked(&mut screen)),
        reader(&line),
        "the same check line:\n{line:?}\n{}",
        text(&mut screen, 190, 48)
    );
}

/// The designed world's Actions tab, its newest run listed.
fn open_designed(repo: &Path) -> GitScreen {
    let mut screen = GitScreen::open(repo).unwrap();
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('>'));
    }
    wait(&mut screen, "Document the quickstart");
    screen
}

/// What one provider's Workflows panel lists, and how its rows are named.
struct Expected {
    title: &'static str,
    all: &'static str,
    rows: &'static [&'static str],
    /// A workflow to choose, its latest run's glyph, the runs panel's title once it is
    /// chosen and a run it lists (the newest, preview run is not one).
    first: &'static str,
    glyph: &'static [char],
    chosen: &'static str,
    shows: &'static str,
    /// A row turned off on the forge (`(off)`), if this forge says so.
    off: Option<&'static str>,
    /// Requests the panel needs, and one it must not make.
    read: &'static [&'static str],
    unread: &'static str,
}

fn expected(shape: Shape) -> Expected {
    match shape {
        Shape::GitHub | Shape::Gitea => Expected {
            title: "Workflows",
            all: "All workflows",
            rows: &[
                "Quality checks",
                "Preview deployment",
                "Publish package",
                "Nightly build",
            ],
            first: "Quality checks",
            glyph: &['✕'],
            chosen: "Runs · Quality checks",
            shows: "Exercise the retry-limit",
            off: Some("Nightly build (off)"),
            read: &["actions/workflows"],
            // A workflow's definition is read for `d`, not for choosing it.
            unread: "quality.yml",
        },
        // No workflow list on Forgejo: its workflow files are the rows, with no on/off state.
        Shape::Forgejo => Expected {
            title: "Workflows",
            all: "All workflows",
            rows: &["quality", "preview", "publish", "nightly"],
            first: "quality",
            glyph: &['✕'],
            chosen: "Runs · quality",
            shows: "Exercise the retry-limit",
            off: None,
            read: &["contents/.forgejo/workflows"],
            unread: "actions/workflows",
        },
        // What starts a pipeline, then the schedules.
        Shape::GitLab => Expected {
            title: "Sources",
            all: "All pipelines",
            rows: &[
                "Merge request",
                "Push",
                "Run pipeline",
                "Weekly dependency audit",
                "Nightly build",
            ],
            first: "Push",
            glyph: &['✓', '✔'],
            chosen: "Pipelines · Push",
            shows: "Release 0.4.0",
            off: Some("Nightly build (off)"),
            read: &["/pipelines", "pipeline_schedules"],
            unread: ".gitlab-ci.yml",
        },
    }
}

/// Workflows over the chosen workflow's runs, the run on the right; 1 2 3
/// and Tab move between them; `d` dispatches the chosen workflow or says why not; `Space e`
/// hides the left column and the next session remembers it.
fn workflows_panel_filters_runs_and_panels_take_turns(shape: Shape) {
    let repo = child().unwrap();
    let want = expected(shape);
    let mut screen = open_designed(&repo);
    let wide = wait_for(&mut screen, (190, 48), "the Workflows panel", |frame| {
        want.rows.iter().all(|row| frame.contains(row)) && frame.contains(want.all)
    });
    assert!(wide.contains(want.title), "{wide}");
    if let Some(off) = want.off {
        assert!(wide.contains(off), "{off}:\n{wide}");
    }
    // A workflow's row carries its newest run's state.
    let first = wide
        .lines()
        .find(|line| line.contains(&format!(" {}", want.first)))
        .unwrap_or_default();
    assert!(first.contains(want.glyph), "{first}\n{wide}");
    // 1 focuses Workflows; choosing a row filters the runs below at once.
    key(&mut screen, KeyCode::Char('1'));
    assert!(
        cursor_row(&mut screen, 190, 48).contains(want.all),
        "{}",
        text(&mut screen, 190, 48)
    );
    for _ in 0..6 {
        if cursor_row(&mut screen, 190, 48).contains(want.first) {
            break;
        }
        key(&mut screen, KeyCode::Char('j'));
    }
    let chosen = wait_for(
        &mut screen,
        (190, 48),
        "the chosen workflow's runs",
        |frame| {
            frame.contains(want.chosen)
                && frame.contains(want.shows)
                && !frame.contains("Document the quickstart")
        },
    );
    assert!(
        !chosen.contains("Add a queue summary") || !matches!(shape, Shape::GitLab),
        "{chosen}"
    );
    for path in want.read {
        assert!(
            requested("GET", path),
            "{path}: {:?}",
            requests()
                .iter()
                .map(|r| r["path"].clone())
                .collect::<Vec<_>>()
        );
    }
    assert!(
        !requested("GET", want.unread),
        "{}: {:?}",
        want.unread,
        requests()
            .iter()
            .map(|r| r["path"].clone())
            .collect::<Vec<_>>()
    );
    // `d` on a workflow that is turned off says so, and on All asks for a workflow (GitLab
    // runs its one pipeline from any row).
    if let Some(off) = want.off {
        key(&mut screen, KeyCode::End);
        assert!(cursor_row(&mut screen, 190, 48).contains(off));
        key(&mut screen, KeyCode::Char('d'));
        if matches!(shape, Shape::GitLab) {
            wait(&mut screen, "Starts a pipeline on main");
            key(&mut screen, KeyCode::Escape);
        } else {
            // The reason also stays in the empty run pane, not only in a toast that fades.
            wait_for(
                &mut screen,
                (190, 48),
                "the reason in the run pane",
                |frame| {
                    frame
                        .lines()
                        .any(|line| line.contains(" to show · ") && line.contains("is turned off"))
                },
            );
        }
    }
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('d'));
    if !matches!(shape, Shape::GitLab) {
        wait(&mut screen, "press 1 for Workflows");
    } else {
        key(&mut screen, KeyCode::Escape);
    }
    // Tab: Workflows → runs → the run, and round again.
    key(&mut screen, KeyCode::Tab);
    assert!(cursor_row(&mut screen, 190, 48).contains("Document the quickstart"));
    key(&mut screen, KeyCode::Tab);
    wait(&mut screen, "f full log");
    key(&mut screen, KeyCode::Tab);
    assert!(cursor_row(&mut screen, 190, 48).contains(want.all));
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, "f full log");
    key(&mut screen, KeyCode::Char('2'));
    assert!(cursor_row(&mut screen, 190, 48).contains("Document the quickstart"));
    // Lines wrap by default; `w` toggles it from any panel.
    key(&mut screen, KeyCode::Char('w'));
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, "w wrap");
    key(&mut screen, KeyCode::Char('w'));
    wait(&mut screen, "w no wrap");
    // Space waits for its chord; `e` hides the left column. An unknown key cancels quietly.
    // (The run's own footer offers `Space e sidebar` too; the pending leader adds `Esc cancel`.)
    key(&mut screen, KeyCode::Char(' '));
    let leader = text(&mut screen, 190, 48);
    assert!(
        leader.contains("Space e sidebar") && leader.contains("Esc cancel"),
        "{leader}"
    );
    // The footer lane says each chord whole, with no lone `Space …` at its edge; no which-key
    // box over the panes repeats it.
    assert!(
        !leader.contains("Space …") && !leader.contains("Keys:"),
        "{leader}"
    );
    key(&mut screen, KeyCode::Char('q'));
    let cancelled = text(&mut screen, 190, 48);
    assert!(
        !cancelled.contains("Esc cancel") && cancelled.contains(want.all),
        "{cancelled}"
    );
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    let hidden = text(&mut screen, 190, 48);
    assert!(
        !hidden.contains(want.all) && hidden.contains("Document the quickstart"),
        "{hidden}"
    );
    // The next session opens with it hidden too.
    drop(screen);
    let mut again = open_designed(&repo);
    let remembered = wait(&mut again, "f full log");
    assert!(!remembered.contains(want.all), "{remembered}");
    // 1 brings it back.
    key(&mut again, KeyCode::Char('1'));
    assert!(text(&mut again, 190, 48).contains(want.all));
    // Below 90 columns the left column stacks Workflows over the runs; the run takes turns.
    let narrow = text(&mut again, 80, 24);
    assert!(
        narrow.contains(want.title) && narrow.contains(want.all) && narrow.contains("Document the"),
        "{narrow}"
    );
    // At 100 columns both columns stay.
    let compact = text(&mut again, 100, 30);
    assert!(
        compact.contains(want.all) && compact.contains("Document the"),
        "{compact}"
    );
}

#[test]
fn explorer_on_github() {
    if child().is_none() {
        return rerun_in_shape("explorer_on_github", Shape::GitHub);
    }
    workflows_panel_filters_runs_and_panels_take_turns(Shape::GitHub);
}

#[test]
fn explorer_on_gitea() {
    if child().is_none() {
        return rerun_in_shape("explorer_on_gitea", Shape::Gitea);
    }
    workflows_panel_filters_runs_and_panels_take_turns(Shape::Gitea);
}

#[test]
fn explorer_on_forgejo() {
    if child().is_none() {
        return rerun_in_shape("explorer_on_forgejo", Shape::Forgejo);
    }
    workflows_panel_filters_runs_and_panels_take_turns(Shape::Forgejo);
}

#[test]
fn explorer_on_gitlab() {
    if child().is_none() {
        return rerun_in_shape("explorer_on_gitlab", Shape::GitLab);
    }
    workflows_panel_filters_runs_and_panels_take_turns(Shape::GitLab);
}
