#![cfg(unix)]
//! Issue reading regressions: display reflow must not become source editing or
//! metadata readiness, and native focus must not change the selected scope.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, KeyInputKind, UiEvent};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

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
fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn text(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    text(&screen.frame(190, 65))
}
fn state_path() -> PathBuf {
    std::env::var_os("DUST_GITHUB_FIXTURE").unwrap().into()
}
/// The fake forge server's request log in the gh-CLI form the legacy fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(&state_path())
}
fn wait(screen: &mut GitScreen, predicate: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let shown = view(screen);
        if predicate(&shown) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timeout:\n{shown}\n{:?}",
            calls()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn wait_item(screen: &mut GitScreen, id: u64) {
    let prefix = vec!["issue".to_owned(), "view".to_owned(), id.to_string()];
    wait(screen, |_| {
        calls().iter().any(|args| args.starts_with(&prefix))
    });
}
fn wait_timeline(screen: &mut GitScreen, id: u64) {
    let path = format!("issues/{id}/timeline");
    wait(screen, |_| {
        calls().iter().flatten().any(|arg| arg.contains(&path))
    });
}
fn release(name: &str) {
    std::fs::write(format!("{}.{}", state_path().display(), name), "release").unwrap();
}
fn fixture_child(name: &str) -> bool {
    if std::env::var_os("DUST_GITHUB_READING_CHILD").is_some() {
        return false;
    }
    let owner = tempfile::tempdir().unwrap();
    let bin = owner.path().join("bin");
    let state = owner.path().join("github.json");
    let server = forge_fake::ForgeServer::start(&state, "legacy");
    server.install_gh(&bin);
    let root = owner.path().join("reading-repository");
    // An unborn disposable repository is sufficient: no commit/index write is
    // required for the reading route under test. The forge target comes from its remote.
    git2::Repository::init(&root)
        .unwrap()
        .remote("origin", "https://github.com/example/demo.git")
        .unwrap();
    std::fs::write(root.join("untouched.txt"), "local content\n").unwrap();
    let output = server
        .configure(&mut Command::new(std::env::current_exe().unwrap()))
        .args(["--exact", name, "--nocapture"])
        .env("DUST_GITHUB_READING_CHILD", "1")
        // `wait_timeline` takes the timeline's request as the sign that the item read before
        // it finished: true of the screen's one-lane mode, which these tests pin.
        .env("DUSTY_FORGE_LANES", "1")
        .env("DUST_GITHUB_FIXTURE", &state)
        .env("DUST_GITHUB_ROOT", &root)
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
fn open(state: Value) -> GitScreen {
    std::fs::write(state_path(), state.to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    // Issues is the third top tab: Git → Pull requests → Issues.
    key(&mut screen, KeyCode::Char('>'));
    key(&mut screen, KeyCode::Char('>'));
    screen
}
fn find(screen: &mut GitScreen, query: &str) {
    control(screen, KeyCode::Char('f'));
    control(screen, KeyCode::Char('e'));
    control(screen, KeyCode::Char('u'));
    paste(screen, query);
    key(screen, KeyCode::Enter);
}
fn cursor_tail(frame: &Frame) -> String {
    let (x, y, _) = frame.cursor.expect("native cursor");
    (x..frame.grid.width())
        .filter_map(|x| frame.grid.get(x, y))
        .filter(|cell| !cell.continuation)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
        .trim_end()
        .into()
}
fn assert_read_only() {
    for args in calls() {
        assert!(
            args.starts_with(&["repo".into(), "view".into()])
                || args.starts_with(&["issue".into(), "list".into()])
                || args.starts_with(&["issue".into(), "view".into()])
                || args.first().is_some_and(|arg| arg == "api")
                    && args.iter().any(|arg| arg.contains("/timeline")),
            "unexpected GitHub operation: {args:?}"
        );
        assert!(!args.iter().any(|arg| arg == "--method" || arg == "-X"));
    }
}

#[test]
fn logical_markdown_distinguishes_prose_from_independently_wide_literal_rows() {
    use dust::ui::markdown::{Styled, render_logical};
    let code = format!("\tlet value = \"**{}**\";", "x".repeat(160));
    let diagram = format!("    left ----{}---- right `literal`", "-".repeat(130));
    let table = format!("| cell | {} |", "wide".repeat(35));
    let mut out = Styled::default();
    render_logical(
        &mut out,
        &format!(
            "# Heading\n**ordinary prose**\n- parent\n  - nested\n```rust\n{code}\n```\n\n{diagram}\n    # **literal heading**\n\n| Name | Value |\n|---|---|\n{table}"
        ),
    );
    for literal in [&code, &diagram] {
        let start = out
            .text
            .find(literal.as_str())
            .expect("literal text must not be rewritten");
        assert!(
            out.preformatted
                .iter()
                .any(|range| range.contains(&start) && range.end >= start + literal.len())
        );
        assert!(literal.width() > 80);
    }
    let table_start = out.text.find("cell │").unwrap();
    assert!(
        out.preformatted
            .iter()
            .any(|range| range.contains(&table_start))
    );
    assert!(
        !out.preformatted
            .iter()
            .any(|range| range.contains(&out.text.find("ordinary prose").unwrap()))
    );
    assert!(out.text.contains("  ◦ nested"));
    assert!(!out.text.contains("```"));
    assert!(out.text.contains("    # **literal heading**"));
}

#[test]
fn issue_prose_measure_is_bounded_code_and_diagram_pan_and_tables_wrap() {
    if fixture_child("issue_prose_measure_is_bounded_code_and_diagram_pan_and_tables_wrap") {
        return;
    }
    let prose = (0..70)
        .map(|n| format!("word{n:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!(
        "{prose}\n\n```text\nCODE_START{}CODE_END\n```\n\n| Name | Value |\n|---|---|\n| table | {}TABLE_END |\n\n    DIAGRAM_START{}DIAGRAM_END",
        "x".repeat(180),
        "t".repeat(180),
        "-".repeat(180)
    );
    let mut screen = open(
        json!({"calls":[],"comments":[],"issues":[{"number":7,"title":"Independent widths","body":body}]}),
    );
    wait_timeline(&mut screen, 7);
    control(&mut screen, KeyCode::Char('l'));
    let frame = screen.frame(190, 65);
    // The reader pane starts after the list (44% of a wide terminal).
    let left = 190u16 * 44 / 100;
    let prose_rows = (5..65)
        .filter_map(|y| {
            let row = (left + 2..189)
                .filter_map(|x| frame.grid.get(x, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>();
            // The Conversation's sidebar sits beside the body past a wide gap; the prose is
            // what comes before it.
            let prose = row.trim().split("  ").next().unwrap_or_default();
            prose.contains("word").then(|| prose.to_owned())
        })
        .collect::<Vec<_>>();
    assert!(
        prose_rows.len() > 4,
        "wide prose must reflow, not fill 139 columns"
    );
    // Bodies wrap at a readable measure (prose::MEASURE), not the pane's full width.
    assert!(prose_rows.iter().all(|row| row.width() <= 100));
    assert_eq!(
        prose_rows.join(" ").split_whitespace().collect::<Vec<_>>(),
        prose.split_whitespace().collect::<Vec<_>>(),
        "display reflow must retain every word in order"
    );
    // A table wider than the pane wraps its cells inside their columns, boxed in as the editor
    // draws it, keeping every character, rather than panning.
    for (width, height) in [(190, 65), (64, 60)] {
        let frame = screen.frame(width, height);
        let rows = text(&frame);
        let cells = rows
            .lines()
            // The cell between its column's borders, apart from a sidebar or the pane's border.
            .flat_map(|row| row.split('│').map(str::trim).collect::<Vec<_>>())
            .filter(|cell| !cell.is_empty() && cell.chars().all(|c| c == 't' || "TABLE_END".contains(c)))
            .collect::<Vec<_>>();
        assert!(cells.len() > 1, "the long cell wraps:\n{rows}");
        assert_eq!(cells.concat(), format!("{}TABLE_END", "t".repeat(180)), "{rows}");
        assert!(
            rows.lines().filter(|row| row.contains(" │ t")).all(|row| !row.contains('›')),
            "a wrapped table does not pan:\n{rows}"
        );
    }
    for end in ["CODE_END", "DIAGRAM_END"] {
        find(&mut screen, end);
        let frame = screen.frame(64, 30);
        assert!(cursor_tail(&frame).starts_with(end), "{}", text(&frame));
        let row = frame.cursor.unwrap().1;
        key(&mut screen, KeyCode::Char('h'));
        let frame = screen.frame(64, 30);
        assert_eq!(
            frame.cursor.unwrap().1,
            row,
            "horizontal navigation must stay on the same literal row"
        );
        assert!(
            frame.grid.row_text(row).contains('›'),
            "literal overflow remains horizontally reachable"
        );
    }
    // Panning literal rows must not horizontally displace ordinary paragraphs.
    key(&mut screen, KeyCode::Home);
    let panned = screen.frame(190, 65);
    for prose in prose_rows {
        assert!(text(&panned).contains(prose.trim()));
    }
    assert_read_only();
}

#[test]
fn pending_list_prose_is_quiet_without_enabling_loaded_controls_or_hiding_errors() {
    if fixture_child(
        "pending_list_prose_is_quiet_without_enabling_loaded_controls_or_hiding_errors",
    ) {
        return;
    }
    for (name, body, missing, failed) in [
        ("reading-ready", "Known cached body.", false, false),
        ("reading-empty", "", false, false),
        (
            "reading-missing",
            "Full body after missing list field.",
            true,
            false,
        ),
        ("reading-error", "Body retained after error.", false, true),
    ] {
        let mut screen = open(json!({"calls":[],"comments":[],
            "issues":[{"number":7,"title":"Truthful detail state","body":body}],
            "list_omit_fields":if missing { vec!["body"] } else { vec![] },
            "fail_item_views":u8::from(failed),
            "read_gates":[{"prefix":["issue","view","7"],"name":name}]
        }));
        wait_item(&mut screen, 7);
        let pending = view(&mut screen);
        assert!(pending.contains("Loading details…"));
        assert!(
            !pending.contains("showing list description")
                && !pending.contains("Actions & metadata")
        );
        assert_eq!(
            pending.contains("No description provided."),
            body.is_empty()
        );
        if !body.is_empty() {
            assert_eq!(pending.contains(body), !missing);
        }
        key(&mut screen, KeyCode::Char('e'));
        assert!(
            view(&mut screen).contains("Item is loading"),
            "list prose is not editable item metadata"
        );
        release(name);
        if failed {
            wait(&mut screen, |shown| {
                shown.contains("Fixture refresh refused")
            });
            let failed = view(&mut screen);
            assert!(failed.contains(body) && failed.contains("Details unavailable"));
            assert!(!failed.contains("Loading details") && !failed.contains("Actions & metadata"));
        } else {
            // Loaded details show the Conversation's sidebar and its comment box.
            wait(&mut screen, |shown| shown.contains("Add a comment"));
            let loaded = view(&mut screen);
            assert!(
                !loaded.contains("Loading details") && !loaded.contains("showing list description")
            );
            assert!(
                loaded.contains("Open")
                    && loaded.contains("alice")
                    && loaded.contains("Labels")
                    && loaded.contains("example/demo")
            );
            if !body.is_empty() {
                assert!(loaded.contains(body));
            }
            key(&mut screen, KeyCode::Char('e'));
            assert!(view(&mut screen).contains("Edit title and description"));
            key(&mut screen, KeyCode::Escape);
        }
        wait_timeline(&mut screen, 7);
        assert_read_only();
    }
}

#[test]
fn issue_reading_focus_round_trips_scope_native_cursor_scroll_and_drafts() {
    if fixture_child("issue_reading_focus_round_trips_scope_native_cursor_scroll_and_drafts") {
        return;
    }
    let body = (0..18)
        .map(|n| {
            format!(
                "Paragraph {n:02}: {}",
                (0..24)
                    .map(|word| format!("token{word:02}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let title = "A focused issue title whose final identifying words remain accessible at sixty four columns";
    let mut screen = open(
        json!({"calls":[],"comments":[],"host":"git.example.test","repository":"other/demo",
        "issues":[{"number":8,"title":"Other issue","body":"Wrong identity."}, {"number":7,"title":title,"body":body}]}),
    );
    wait_timeline(&mut screen, 8);
    // Exact query scope is also retained when the detail takes over the surface.
    key(&mut screen, KeyCode::Char('/'));
    paste(&mut screen, "author:alice");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |shown| {
        shown.contains("#8 Other issue") && shown.contains("author:alice")
    });
    key(&mut screen, KeyCode::Down);
    wait_timeline(&mut screen, 7);
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "Unsent scoped reading draft.");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Enter);
    let narrow = screen.frame(64, 30);
    let narrow_text = text(&narrow);
    assert!(
        narrow_text.contains("final identifying words")
            && narrow_text.contains("sixty four columns")
    );
    assert!(!narrow_text.contains("query:") && !narrow_text.contains("Custom · Issues"));
    assert!(narrow_text.contains("Esc list") && !narrow_text.contains("S save"));
    // Move to a wrapped continuation, not just a paragraph boundary; returning
    // through Enter must not reset the native detail cursor to its title.
    for _ in 0..24 {
        key(&mut screen, KeyCode::Down);
        screen.frame(64, 30);
    }
    let before = screen.frame(64, 30);
    let before_text = text(&before);
    let before_tail = cursor_tail(&before);
    let before_cursor = before.cursor;
    let reads = calls();
    for _ in 0..3 {
        key(&mut screen, KeyCode::Escape);
        let list = screen.frame(64, 30);
        assert!(
            cursor_tail(&list).contains("#7"),
            "list selection identity changed: {}",
            text(&list)
        );
        key(&mut screen, KeyCode::Enter);
        let restored = screen.frame(64, 30);
        assert_eq!(cursor_tail(&restored), before_tail);
        assert_eq!(restored.cursor, before_cursor);
        assert_eq!(
            text(&restored),
            before_text,
            "focus round trip must preserve viewport, not only cursor"
        );
    }
    // Wider reflow and return must keep the same source byte/continuation.
    screen.frame(190, 65);
    screen.frame(100, 30);
    assert_eq!(cursor_tail(&screen.frame(64, 30)), before_tail);
    control(&mut screen, KeyCode::Char('h'));
    let list = view(&mut screen);
    assert!(list.contains("author:alice") && list.contains("other/demo"));
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Unsent scoped reading draft."));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        calls(),
        reads,
        "focus/render/draft restoration cannot refetch or mutate"
    );
    let root = PathBuf::from(std::env::var_os("DUST_GITHUB_ROOT").unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join("untouched.txt")).unwrap(),
        "local content\n"
    );
    assert!(!root.join(".git/index").exists());
    assert_read_only();
}

#[test]
fn refreshing_loaded_issue_keeps_real_body_and_readiness_after_a_failed_refresh() {
    if fixture_child("refreshing_loaded_issue_keeps_real_body_and_readiness_after_a_failed_refresh")
    {
        return;
    }
    let mut screen = open(json!({"calls":[],"comments":[],"issues":[{
        "number":7,"title":"Loaded readiness","body":"The loaded body remains authoritative."
    }]}));
    wait(&mut screen, |shown| shown.contains("Add a comment"));
    wait_timeline(&mut screen, 7);
    let mut state: Value = serde_json::from_slice(&std::fs::read(state_path()).unwrap()).unwrap();
    state["fail_item_views"] = json!(1);
    state["read_gates"] = json!([{"prefix":["issue","view","7"],"name":"loaded-refresh"}]);
    std::fs::write(state_path(), state.to_string()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |_| {
        calls()
            .iter()
            .filter(|args| args.starts_with(&["issue".into(), "view".into()]))
            .count()
            >= 2
    });
    let pending = view(&mut screen);
    // The reader header says the details are refreshing (the footer may be busy with the
    // list's own refresh).
    assert!(
        (pending.contains("⟳ refreshing") || pending.contains("Refreshing details…"))
            && pending.contains("The loaded body remains authoritative."),
        "{pending}"
    );
    assert!(
        pending.contains("Add a comment") && !pending.contains("showing the list description")
    );
    key(&mut screen, KeyCode::Char('e'));
    assert!(
        view(&mut screen).contains("Edit title and description"),
        "a refresh does not revoke real loaded readiness"
    );
    key(&mut screen, KeyCode::Escape);
    release("loaded-refresh");
    wait(&mut screen, |shown| {
        shown.contains("Fixture refresh refused · showing ")
    });
    let failed = view(&mut screen);
    assert!(
        failed.contains("Fixture refresh refused")
            && failed.contains("The loaded body remains authoritative.")
    );
    assert!(!failed.contains("Refreshing details") && !failed.contains("Details unavailable"));
    key(&mut screen, KeyCode::Char('e'));
    assert!(view(&mut screen).contains("Edit title and description"));
    key(&mut screen, KeyCode::Escape);
    assert_read_only();
}
