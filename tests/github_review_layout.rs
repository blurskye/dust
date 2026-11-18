#![cfg(unix)]
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::GitScreen;
use dusty::ui::{Frame, KeyInputKind, UiEvent};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[path = "support/forge_fake.rs"]
mod forge_fake;

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
fn text(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    text(&screen.frame(190, 65))
}
fn state() -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(std::env::var("DUSTY_GITHUB_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap()
}
fn fixture() -> std::path::PathBuf {
    std::env::var("DUSTY_GITHUB_FIXTURE").unwrap().into()
}
/// Under the server's lock: a plain write can be overwritten by the server saving its
/// request log (D9).
fn save(value: &Value) {
    forge_fake::write_state(&fixture(), value);
}
fn update(change: impl FnOnce(&mut Value)) {
    forge_fake::update(&fixture(), change);
}
/// The fake forge server's request log in the gh-CLI form the legacy fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(Path::new(&std::env::var("DUSTY_GITHUB_FIXTURE").unwrap()))
}
fn idle(screen: &mut GitScreen) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while screen.busy() {
        screen.poll();
        view(screen);
        assert!(Instant::now() < deadline, "{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[track_caller]
fn wait_until(screen: &mut GitScreen, predicate: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let shown = view(screen);
        if predicate(&shown) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "detail not ready:\n{shown}\n{:?}",
            calls()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[track_caller]
fn wait(screen: &mut GitScreen, needle: &str) {
    wait_until(screen, |shown| shown.contains(needle));
}
fn focus(screen: &mut GitScreen, needle: &str) {
    control(screen, KeyCode::Char('l'));
    for _ in 0..100 {
        let frame = screen.frame(190, 65);
        if let Some((x, y, _)) = frame.cursor {
            let tail = (x..190)
                .filter_map(|column| frame.grid.get(column, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>();
            if let Some(at) = tail.find(needle)
                && tail[..at]
                    .chars()
                    .all(|c| c.is_whitespace() || "›▾▸○✓✔×✕●│▎".contains(c))
            {
                return;
            }
        }
        key(screen, KeyCode::Tab);
    }
    panic!("unreachable {needle}:\n{}", view(screen));
}
fn activate(screen: &mut GitScreen, needle: &str) {
    focus(screen, needle);
    key(screen, KeyCode::Enter);
}
/// Moves the reader cursor with `j` from the top until its row shows `needle`; thread
/// headers are not prose, so find cannot reach them.
fn cursor_to(screen: &mut GitScreen, needle: &str) {
    key(screen, KeyCode::Home);
    for _ in 0..120 {
        let frame = screen.frame(190, 48);
        if let Some((_, y, _)) = frame.cursor
            && frame.grid.row_text(y).contains(needle)
        {
            return;
        }
        key(screen, KeyCode::Char('j'));
    }
    panic!(
        "no reader row shows {needle}:\n{}",
        text(&screen.frame(190, 48))
    );
}
fn child(name: &str) -> bool {
    if std::env::var_os("DUSTY_REVIEW_LAYOUT_CHILD").is_some() {
        return false;
    }
    let owner = tempfile::tempdir().unwrap();
    let bin = owner.path().join("bin");
    // The fake forge's legacy world honours a per-test `run` override in the state.
    let state = owner.path().join("state.json");
    let server = forge_fake::ForgeServer::start(&state, "legacy");
    server.install_gh(&bin);
    let root = owner.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Layout Test"],
        vec!["config", "user.email", "layout@example.invalid"],
        vec!["commit", "--allow-empty", "-m", "Initial"],
        // The forge target comes from the remote, as gh's did.
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/example/demo.git",
        ],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let output = server
        .configure(&mut Command::new(std::env::current_exe().unwrap()))
        .args(["--exact", name, "--nocapture"])
        .env("DUSTY_REVIEW_LAYOUT_CHILD", "1")
        .env("DUSTY_GITHUB_FIXTURE", &state)
        .env("DUSTY_GITHUB_ROOT", root)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
fn screen() -> GitScreen {
    GitScreen::open(Path::new(&std::env::var("DUSTY_GITHUB_ROOT").unwrap())).unwrap()
}

#[test]
fn actions_reader_keeps_selected_job_identity_payload_and_confirmation() {
    if child("actions_reader_keeps_selected_job_identity_payload_and_confirmation") {
        return;
    }
    let payload = (0..40).map(|n| format!("job\tUNKNOWN STEP\t2026-11-08T10:00:00.200Z P{n:02} payload with trailing sentinel RIGHTEDGE\n"))
        .collect::<String>() + "job\tUNKNOWN STEP\t2026-11-08T10:00:00.300Z unicode 👩‍💻 界\n";
    let run = json!({"databaseId":42928152565_u64,"displayTitle":"go_modules update #1694251669",
    "workflowName":"Dependabot Updates","workflowDatabaseId":31,"headBranch":"master",
    "headSha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","event":"push","attempt":1,
    "status":"completed","conclusion":"failure","jobs":[
        {"databaseId":130219809163_u64,"name":"First job","status":"completed","conclusion":"failure",
         "steps":[{"number":4,"name":"Run first","status":"completed","conclusion":"failure"}]},
        {"databaseId":130219809164_u64,"name":"Dependabot","status":"completed","conclusion":"failure",
         "steps":[{"number":4,"name":"Run Dependabot","status":"completed","conclusion":"failure"}]}
    ]});
    save(
        &json!({"calls":[],"comments":[],"run_views":2,"repository":"jesseduffield/lazygit",
        "run":run,"logs":{"130219809163":payload,"130219809164":payload}}),
    );
    let mut screen = screen();
    // Top tabs: Git → Pull requests → Issues → Actions.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('>'));
    }
    wait(&mut screen, "go_modules update");
    key(&mut screen, KeyCode::Enter);
    // The run opens on both failed jobs and their failed steps.
    wait_until(&mut screen, |shown| {
        shown.contains("go_modules update")
            && shown.contains("First job")
            && shown.contains("Dependabot")
    });
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        for row in 1..5 {
            assert!(
                !frame
                    .grid
                    .row_text(row)
                    .to_ascii_lowercase()
                    .contains("attempt"),
                "a sole attempt is not named:\n{}",
                text(&frame)
            );
        }
    }
    wait(&mut screen, "P00 payload");
    let shown = view(&mut screen);
    assert!(
        shown.contains("Run first") && shown.contains("Run Dependabot"),
        "{shown}"
    );
    // Closing the first job leaves the second job's failed step and log.
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Enter);
    // The second job's log is read after the first one's.
    wait_until(&mut screen, |shown| {
        shown.contains("P00 payload") && !shown.contains("Run first")
    });
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        let shown = text(&frame);
        assert!(
            shown.contains("Dependabot")
                && shown.contains("Run Dependabot")
                && !shown.contains("Run first"),
            "{shown}"
        );
        // A step's lines found by their time are shown without remark.
        assert!(!shown.contains("Approximate"), "{shown}");
        let payloads = (0..height)
            .filter(|row| frame.grid.row_text(*row).contains("payload"))
            .count();
        assert!(
            payloads >= usize::from(height) - 12,
            "only {payloads} payload rows:\n{shown}"
        );
        // Long lines wrap inside the pane, never into the border.
        assert!(
            shown.contains('›') || shown.contains("RIGHTEDGE"),
            "{shown}"
        );
    }
    // Find in the step log counts the one emoji match.
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Char('💻'));
    key(&mut screen, KeyCode::Enter);
    for (width, height) in [(190, 48), (64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        assert!(text(&frame).contains("1/1 matches"), "{}", text(&frame));
        let (_, y, _) = frame.cursor.unwrap();
        assert!(
            frame.grid.row_text(y).contains("unicode"),
            "{}",
            text(&frame)
        );
    }
    key(&mut screen, KeyCode::Escape);
    // A refused write opens nothing and sends nothing.
    screen.write_refusal = Some("Read-only fixture denial".into());
    key(&mut screen, KeyCode::Char('r'));
    assert!(view(&mut screen).contains("Read-only fixture denial"));
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["run".into(), "rerun".into()]))
    );
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('r'));
    let sheet = view(&mut screen);
    assert!(
        sheet.contains("Failed jobs only (2)") && sheet.contains("Debug logging"),
        "{sheet}"
    );
    // A stray key on the Jobs choice picks nothing and keeps the failed jobs chosen.
    key(&mut screen, KeyCode::Char('z'));
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["run".into(), "rerun".into()]))
    );
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    let reruns = calls()
        .into_iter()
        .filter(|args| args.starts_with(&["run".into(), "rerun".into()]))
        .collect::<Vec<_>>();
    assert_eq!(reruns.len(), 1);
    assert!(reruns[0].contains(&"42928152565".into()) && reruns[0].contains(&"--failed".into()));
}

#[test]
fn the_merge_box_says_failing_from_the_moment_the_pull_request_opens() {
    if child("the_merge_box_says_failing_from_the_moment_the_pull_request_opens") {
        return;
    }
    // One fact in three places (r15 prs-01): the list glyph, the merge box and the Checks
    // tab all say `test (linux)` failed, before and after the Checks tab reads its run.
    let run = json!({"databaseId":91,"displayTitle":"Validate native workspace","workflowName":"Native CI",
    "workflowDatabaseId":31,"headBranch":"feature/native","headSha":"a".repeat(40),"event":"pull_request",
    "attempt":1,"status":"completed","conclusion":"failure","jobs":[
        {"databaseId":42,"name":"test (linux)","status":"completed","conclusion":"failure",
         "steps":[{"number":1,"name":"Check","status":"completed","conclusion":"failure"}]}]});
    save(&json!({"calls":[],"comments":[],"run_views":2,"run":run,
        "prs":[{"number":9,"title":"Merge verdict sentinel","statusCheckRollup":[{"name":"test (linux)",
            "conclusion":"FAILURE","detailsUrl":"https://github.com/example/demo/actions/runs/91/job/42"}]}]}));
    let mut screen = screen();
    key(&mut screen, KeyCode::Char('>'));
    wait(&mut screen, "Merge verdict sentinel");
    let list = view(&mut screen);
    // The list pane's row (left of `││`), not the reader's `#9 Merge verdict sentinel` title.
    let row = list
        .lines()
        .filter_map(|row| row.split("││").next())
        .find(|pane| pane.contains("Merge verdict sentinel"))
        .unwrap_or_default()
        .to_owned();
    assert!(
        row.contains('✕') && !row.contains('✔'),
        "the list glyph says failing: {row}"
    );
    control(&mut screen, KeyCode::Char('l'));
    wait(&mut screen, "approved these changes");
    // The merge box first (the timeline's end): no run read yet, and it already says failed.
    // A frame tall enough for the whole Conversation, so the verdict is read without moving
    // the cursor (this test is about the words, not the reader's navigation).
    let whole = |screen: &mut GitScreen| text(&screen.frame(190, 160));
    let deadline = Instant::now() + Duration::from_secs(30);
    // The merge box names the failing check among what blocks it (`1. test (linux) failed`).
    let failing =
        |shown: &str| shown.contains("test (linux) failed") && !shown.contains("Checks passed");
    while !failing(&whole(&mut screen)) {
        screen.poll();
        assert!(
            Instant::now() < deadline,
            "no failing verdict:\n{}",
            whole(&mut screen)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["run".into(), "view".into()])),
        "the verdict needs no run read: {:?}",
        calls()
    );
    let overview = whole(&mut screen);
    assert!(!overview.contains("Checks passed"), "{overview}");
    // The Checks tab reads the run and agrees; back on Conversation nothing changed.
    for _ in 0..2 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "Check");
    for _ in 0..2 {
        key(&mut screen, KeyCode::Char('H'));
    }
    wait(&mut screen, "approved these changes");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let shown = whole(&mut screen);
        if failing(&shown) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the verdict changed after the run read:\n{shown}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn narrow_review_keeps_thread_drafts_and_neutral_resolve_target_on_refresh() {
    if child("narrow_review_keeps_thread_drafts_and_neutral_resolve_target_on_refresh") {
        return;
    }
    let comment = |author: &str| {
        json!({"author":{"login":author},"body":"Identical real discussion 界",
        "path":"example.rs","line":1,"originalLine":1,"commit":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}})
    };
    save(
        &json!({"calls":[],"comments":[],"prs":[{"number":9,"title":"Review layout sentinel",
        "reviews":[{"author":{"login":"empty"},"state":"COMMENTED","body":""},
            {"author":{"login":"decision"},"state":"APPROVED","body":""},
            {"author":{"login":"unknown"},"state":"","body":""}]}],
        "review_threads":{"example/demo#9":[
            {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[comment("alice")]}},
            {"id":"THREAD_B","isResolved":false,"comments":{"nodes":[comment("bob")]}}
        ]}}),
    );
    let mut screen = screen();
    key(&mut screen, KeyCode::Char('>'));
    wait(&mut screen, "Review layout sentinel");
    control(&mut screen, KeyCode::Char('l'));
    // busy() covers writes, not asynchronous forge read completion. The Conversation names
    // each reviewer's verdict on their review card and in the sidebar.
    wait_until(&mut screen, |shown| {
        shown.contains("approved these changes") && shown.contains("On merge")
    });
    let overview = view(&mut screen);
    assert!(overview.contains("Reviewers"), "{overview}");
    // Files: the threads sit under the line they discuss.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "fn after()");
    wait(&mut screen, "Mark viewed");
    wait(&mut screen, "Identical real discussion");
    for (width, height) in [(64, 20), (99, 30), (190, 48), (64, 20)] {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.unwrap();
        assert_eq!(
            frame.grid.get(x, y).unwrap().symbol,
            "f",
            "{}",
            text(&frame)
        );
    }
    control(&mut screen, KeyCode::Char('h'));
    control(&mut screen, KeyCode::Char('l'));
    let restored = screen.frame(64, 20);
    let (x, y, _) = restored.cursor.unwrap();
    assert_eq!(
        restored.grid.get(x, y).unwrap().symbol,
        "f",
        "{}",
        text(&restored)
    );
    // `c` on a thread answers it; drafts are scoped to their own thread.
    cursor_to(&mut screen, "Unresolved · alice");
    key(&mut screen, KeyCode::Char('c'));
    let reply = view(&mut screen);
    assert!(reply.contains("Reply to @alice"), "{reply}");
    screen.event(UiEvent::Paste("Unique draft A\nKeep source".into()));
    key(&mut screen, KeyCode::Escape);
    assert!(view(&mut screen).contains("Continue draft"));
    cursor_to(&mut screen, "Unresolved · bob");
    key(&mut screen, KeyCode::Char('c'));
    assert!(view(&mut screen).contains("Reply to @bob"));
    assert!(!view(&mut screen).contains("Unique draft A"));
    screen.event(UiEvent::Paste("Unique draft B".into()));
    key(&mut screen, KeyCode::Escape);
    cursor_to(&mut screen, "Unresolved · alice");
    key(&mut screen, KeyCode::Char('c'));
    assert!(
        view(&mut screen).contains("Reply to @alice")
            && view(&mut screen).contains("Unique draft A")
    );
    key(&mut screen, KeyCode::Escape);
    idle(&mut screen);
    // A refresh that resolves A keeps the cursor on A and never arms the opposite write.
    update(|fixture| fixture["thread_resolved"] = json!({"THREAD_A":true}));
    key(&mut screen, KeyCode::Char('R'));
    wait(&mut screen, "● Resolved · alice");
    let frame = screen.frame(64, 20);
    let (_, y, _) = frame.cursor.unwrap();
    assert!(
        frame.grid.row_text(y).contains("Resolved") && frame.grid.row_text(y).contains("alice"),
        "{}",
        text(&frame)
    );
    key(&mut screen, KeyCode::Enter);
    assert!(
        !view(&mut screen).contains("Unresolve conversation?"),
        "refresh armed an opposite mutation"
    );
    // Nothing opened, so the reader keeps focus (Esc here would go back to the list).
    // The blank after a thread is a real cursor position, not a fallback.
    for _ in 0..4 {
        key(&mut screen, KeyCode::Down);
    }
    let blank = screen.frame(190, 48);
    let (_, y, _) = blank.cursor.unwrap();
    assert!(y > 5, "hidden separator selection: {}", text(&blank));
    cursor_to(&mut screen, "● Resolved · alice");
    key(&mut screen, KeyCode::Char('c'));
    assert!(view(&mut screen).contains("Unique draft A"));
    key(&mut screen, KeyCode::Escape);
    idle(&mut screen);
    update(|fixture| {
        fixture["review_threads"]["example/demo#9"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });
    key(&mut screen, KeyCode::Char('R'));
    // Alice is also the PR author in the list; only thread headers establish removal.
    wait_until(&mut screen, |shown| {
        shown.contains("◔ Unresolved · bob · line 1") && !shown.contains("· alice · line 1")
    });
    key(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("Resolve conversation?"));
    assert!(
        !calls()
            .iter()
            .flatten()
            .any(|arg| arg.contains("mutation("))
    );
}

#[test]
fn narrow_native_jk_crosses_wrapped_replies_and_same_author_thread_separators() {
    if child("narrow_native_jk_crosses_wrapped_replies_and_same_author_thread_separators") {
        return;
    }
    let root_body = "Wrapped root 界 e\u{301} 👩‍💻 keeps native navigation moving through every real reply author and source fragment. ".repeat(2);
    let comment = |body: &str| {
        json!({
            "author":{"login":"same-author"},"body":body,"path":"example.rs",
            "line":1,"originalLine":1,"commit":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        })
    };
    save(
        &json!({"calls":[],"comments":[],"prs":[{"number":9,"title":"Native reply traversal"}],
        "review_threads":{"example/demo#9":[
            {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[
                comment(&root_body), comment("Native reply body 界")
            ]}},
            {"id":"THREAD_B","isResolved":false,"comments":{"nodes":[
                comment("Second thread boundary 界"), comment("Native reply body 界")
            ]}}
        ]}}),
    );
    let mut screen = screen();
    key(&mut screen, KeyCode::Char('>'));
    wait(&mut screen, "Native reply traversal");
    control(&mut screen, KeyCode::Char('l'));
    // Review threads read in Files, beside the line they discuss.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "fn after()");
    wait(&mut screen, "Mark viewed");
    wait_until(&mut screen, |shown| {
        shown.contains("Wrapped root")
            && shown.contains("Native reply body")
            && shown.contains("Second thread boundary")
    });
    activate(&mut screen, "Reply");
    screen.event(UiEvent::Paste("Native draft A 界".into()));
    key(&mut screen, KeyCode::Escape);
    activate(&mut screen, "Reply"); // A now has Continue draft, so this is B.
    screen.event(UiEvent::Paste("Native draft B 界".into()));
    key(&mut screen, KeyCode::Escape);
    let before = state();
    let before_calls = calls();

    let selected = |frame: &Frame| {
        let (x, y, _) = frame.cursor.expect("native detail cursor");
        (x, y, frame.grid.row_text(y))
    };
    key(&mut screen, KeyCode::Home);
    let mut frame = screen.frame(64, 20);
    for _ in 0..40 {
        if selected(&frame).2.contains("Wrapped root") {
            break;
        }
        key(&mut screen, KeyCode::Char('j'));
        frame = screen.frame(64, 20);
    }
    assert!(
        selected(&frame).2.contains("Wrapped root"),
        "{}",
        text(&frame)
    );
    let mut steps = 0;
    let mut authors = 0;
    let mut saw_reply = false;
    let mut saw_next = false;
    while !selected(&frame).2.contains("Review changes") && steps < 80 {
        key(&mut screen, KeyCode::Char('j'));
        frame = screen.frame(64, 20); // Never batch native movement without its frame.
        steps += 1;
        let (x, y, row) = selected(&frame);
        // Body rows run from below the Files heading to the pane's last row (height - 3).
        assert!(x < 24 && (7..18).contains(&y), "{}", text(&frame));
        if row.contains("↳ same-author") {
            authors += 1;
        }
        if row.contains("Native reply body") {
            assert!(
                authors > 0,
                "reply body skipped its stable native author row"
            );
            saw_reply = true;
        }
        if row.contains("Second thread boundary") {
            assert!(saw_reply, "next thread skipped the first reply body");
            saw_next = true;
        }
    }
    assert!(
        selected(&frame).2.contains("Review changes"),
        "{}",
        text(&frame)
    );
    assert!(saw_next && saw_reply);
    assert_eq!(
        authors, 2,
        "same-author rows must each be stable cursor targets"
    );
    // Review changes is beyond B's blank separator; reverse the entire path too.
    let mut reverse_authors = 0;
    for _ in 0..steps {
        key(&mut screen, KeyCode::Char('k'));
        frame = screen.frame(64, 20);
        if selected(&frame).2.contains("↳ same-author") {
            reverse_authors += 1;
        }
    }
    assert_eq!(reverse_authors, 2);
    assert!(
        selected(&frame).2.contains("Wrapped root"),
        "{}",
        text(&frame)
    );
    assert_eq!(
        calls(),
        before_calls,
        "native reading must not request or mutate data"
    );
    let after = state();
    for field in ["prs", "review_threads", "thread_resolved", "comments"] {
        assert_eq!(after[field], before[field], "{field}");
    }
    key(&mut screen, KeyCode::Home);
    activate(&mut screen, "Continue draft");
    assert!(view(&mut screen).contains("Native draft A 界"));
    assert!(!view(&mut screen).contains("Native draft B 界"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Tab);
    screen.frame(64, 20);
    key(&mut screen, KeyCode::Tab);
    screen.frame(64, 20);
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Native draft B 界"));
    assert!(!view(&mut screen).contains("Native draft A 界"));
    key(&mut screen, KeyCode::Escape);
    assert!(
        !calls()
            .iter()
            .flatten()
            .any(|arg| arg.contains("mutation("))
    );
}
