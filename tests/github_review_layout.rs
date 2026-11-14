#![cfg(unix)]
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, KeyInputKind, Style, UiEvent};
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
        &std::fs::read_to_string(std::env::var("DUST_GITHUB_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap()
}
fn save(value: &Value) {
    std::fs::write(
        std::env::var("DUST_GITHUB_FIXTURE").unwrap(),
        value.to_string(),
    )
    .unwrap();
}
/// The fake forge server's request log in the gh-CLI form the legacy fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(Path::new(&std::env::var("DUST_GITHUB_FIXTURE").unwrap()))
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
    panic!("no reader row shows {needle}:\n{}", text(&screen.frame(190, 48)));
}
fn child(name: &str) -> bool {
    if std::env::var_os("DUST_REVIEW_LAYOUT_CHILD").is_some() {
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
        .env("DUST_REVIEW_LAYOUT_CHILD", "1")
        .env("DUST_GITHUB_FIXTURE", &state)
        .env("DUST_GITHUB_ROOT", root)
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
    GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap()
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
    // The drilldown opens on the first failed job and its failed step.
    wait_until(&mut screen, |shown| {
        shown.contains("go_modules update") && shown.contains("First job") && shown.contains("Dependabot")
    });
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        assert!(
            !frame.grid.row_text(1).to_ascii_lowercase().contains("attempt"),
            "a sole attempt is not named:\n{}",
            text(&frame)
        );
    }
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "P00 payload");
    assert!(view(&mut screen).contains("Run first"));
    // Back to the jobs, the next job, its failed step and log.
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Run Dependabot");
    wait(&mut screen, "P00 payload");
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        let shown = text(&frame);
        assert!(frame.grid.row_text(1).contains("Dependabot") && frame.grid.row_text(1).contains("Run Dependabot"), "{shown}");
        assert!(shown.contains("Approximate step slice"), "{shown}");
        let payloads = (0..height).filter(|row| frame.grid.row_text(*row).contains("payload")).count();
        assert!(payloads >= usize::from(height) - 9, "only {payloads} payload rows:\n{shown}");
        // Long lines clip at the pane's edge with a mark, never into the border.
        assert!(shown.contains('›') || shown.contains("RIGHTEDGE"), "{shown}");
    }
    // Find in the step log counts the one emoji match.
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Char('💻'));
    key(&mut screen, KeyCode::Enter);
    for (width, height) in [(190, 48), (64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        assert!(text(&frame).contains("1/1 matches"), "{}", text(&frame));
        let (_, y, _) = frame.cursor.unwrap();
        assert!(frame.grid.row_text(y).contains("unicode"), "{}", text(&frame));
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
    assert!(sheet.contains("Failed jobs only (2)") && sheet.contains("Debug logging"), "{sheet}");
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
    // busy() covers writes, not asynchronous forge read completion. The overview names
    // each reviewer with their own verdict.
    wait_until(&mut screen, |shown| {
        shown.contains("decision ✔ Approved") && shown.contains("Latest activity")
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
    assert_eq!(restored.grid.get(x, y).unwrap().symbol, "f", "{}", text(&restored));
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
    let mut fixture = state();
    fixture["thread_resolved"] = json!({"THREAD_A":true});
    save(&fixture);
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
        !view(&mut screen).contains("Reopen review thread?"),
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
    let mut fixture = state();
    fixture["review_threads"]["example/demo#9"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    save(&fixture);
    key(&mut screen, KeyCode::Char('R'));
    // Alice is also the PR author in the list; only thread headers establish removal.
    wait_until(&mut screen, |shown| {
        shown.contains("○ Unresolved · bob · line 1") && !shown.contains("· alice · line 1")
    });
    key(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("Resolve review thread?"));
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
