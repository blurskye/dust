#![cfg(unix)]
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, KeyInputKind, Style, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/records.rs"]
mod records;
use records::{lists, names};

/// The record under the list cursor: its title row and the `#N opened …` row beneath it.
fn cursor_record(frame: &Frame) -> String {
    let (_, y, _) = frame.cursor.expect("the list keeps a cursor");
    format!("{}\n{}", frame.grid.row_text(y), frame.grid.row_text(y + 1))
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}
/// Top tabs cycle in display order Git → Pull requests → Issues → Actions → Notifications
/// (pages 0, 2, 1, 3, 4); `>` moves to the next one.
fn goto(screen: &mut GitScreen, from: usize, to: usize) {
    const ORDER: [usize; 5] = [0, 2, 1, 3, 4];
    let at = |page| ORDER.iter().position(|&p| p == page).unwrap();
    for _ in 0..(at(to) + 5 - at(from)) % 5 {
        key(screen, KeyCode::Char('>'));
    }
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
    let frame = screen.frame(190, 65);
    (0..65)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
/// The list pane at 100×30, where it is 40 columns wide beside the reader.
fn compact_list(screen: &mut GitScreen) -> String {
    let frame = screen.frame(100, 30);
    (0..30)
        .map(|y| {
            (0..40)
                .filter_map(|x| frame.grid.get(x, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(screen: &mut GitScreen, text: &str) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = view(screen);
        if frame.contains(text) {
            return;
        }
        assert!(Instant::now() < end, "missing {text}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn idle(screen: &mut GitScreen) {
    let end = Instant::now() + Duration::from_secs(30);
    while screen.busy() {
        screen.poll();
        assert!(Instant::now() < end, "{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn focus_control(screen: &mut GitScreen, label: &str) {
    control(screen, KeyCode::Char('l'));
    for _ in 0..80 {
        let frame = screen.frame(190, 65);
        if let Some((x, y, _)) = frame.cursor {
            let tail = (x..190)
                .filter_map(|column| frame.grid.get(column, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>();
            let shown_label = if label == "Reply / continue reply" {
                if tail.contains("Continue draft") {
                    "Continue draft"
                } else {
                    "Reply"
                }
            } else {
                label
            };
            if let Some(at) = tail.find(shown_label)
                && tail[..at].chars().all(|c| {
                    c.is_whitespace()
                        || c.is_ascii_digit()
                        || "[]+-│▾▸✓✔○◔●×✕⑂›▎⎇#☐☑".contains(c)
                        || (label.ends_with('/') || label.ends_with(".rs"))
                            && (c.is_alphanumeric() || "/._-".contains(c))
                })
            {
                return;
            }
        }
        key(screen, KeyCode::Tab);
    }
    panic!(
        "control {label} is not keyboard-reachable:\n{}",
        view(screen)
    );
}
fn activate(screen: &mut GitScreen, label: &str) {
    focus_control(screen, label);
    key(screen, KeyCode::Enter);
}
/// The fake forge server's request log in the gh-CLI form the legacy fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(Path::new(&std::env::var("DUST_GITHUB_FIXTURE").unwrap()))
}
fn is_call(args: &[String], prefix: &[&str]) -> bool {
    args.iter()
        .take(prefix.len())
        .map(String::as_str)
        .eq(prefix.iter().copied())
}
fn wait_call(screen: &mut GitScreen, predicate: impl Fn(&[String]) -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if calls().iter().any(|args| predicate(args)) {
            return;
        }
        assert!(
            Instant::now() < end,
            "missing fixture call: {:?}\n{}",
            calls(),
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn release_read(name: &str) {
    std::fs::write(
        format!("{}.{}", std::env::var("DUST_GITHUB_FIXTURE").unwrap(), name),
        "release",
    )
    .unwrap();
}
fn navigation_issues() -> Vec<serde_json::Value> {
    [8, 7, 6, 5, 4, 2, 1].into_iter().map(|id| serde_json::json!({
        "number":id,"title":format!("Navigation issue {id}"),"body":format!("Unique selected body {id}.")
    })).collect()
}
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn fixture_child(name: &str) -> bool {
    if std::env::var_os("DUST_GITHUB_CHILD").is_none() {
        let owner = tempfile::tempdir().unwrap();
        let bin = owner.path().join("bin");
        let state = owner.path().join("github.json");
        let server = forge_fake::ForgeServer::start(&state, "legacy");
        server.install_gh(&bin);
        let root = owner.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Screen Test"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-m", "Initial"]);
        // The forge target comes from the remote, as gh's did.
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/demo.git",
            ],
        );
        let output = server
            .configure(&mut Command::new(std::env::current_exe().unwrap()))
            .args(["--exact", name, "--nocapture"])
            .env("DUST_GITHUB_CHILD", "1")
            // These tests gate single reads and read the request log's order as the order
            // reads finished (one read at a time): the screen's one-lane mode. Reads side by
            // side are covered by `lanes.rs` and `forge_speed`.
            .env("DUSTY_FORGE_LANES", "1")
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
        return true;
    }
    false
}

#[test]
fn native_github_controls_forms_reviews_and_workflow_logs() {
    if fixture_child("native_github_controls_forms_reviews_and_workflow_logs") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, r#"{"calls":[],"comments":[],"empty":true}"#).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    // One empty state, in the reader; the list keeps its count and a cursor.
    wait(&mut screen, "No issues match this view");
    wait_list_count(&mut screen, "open", 0);
    for (width, height) in [(100, 30), (180, 45)] {
        let frame = screen.frame(width, height);
        // No lone cursor in a blank list: the focused border shows where keys go.
        assert!(frame.cursor.is_none(), "an empty list parks no cursor");
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            text.matches("No issues match this view").count(),
            1,
            "{text}"
        );
        assert!(
            text.contains("Showing 0") && !text.contains("Your repository is connected"),
            "{text}"
        );
        assert!(
            text.contains("n New issue") && text.contains("/ filter"),
            "{text}"
        );
    }
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["empty"] = false.into();
    std::fs::write(&state_path, serde_json::to_string(&fixture).unwrap()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, "A real native issue layout.");
    wait(&mut screen, "Add a comment");
    for (width, height) in [(64, 20), (100, 30), (190, 65)] {
        let frame = screen.frame(width, height);
        let header = frame.grid.row_text(0);
        // The shared top bar names the repository and HEAD; key help lives in the footer.
        assert!(
            (width < 100 || header.contains("repository")) && header.contains("main"),
            "{header}"
        );
        assert!(
            !header.contains("github.com") && !header.contains("? keys"),
            "{header}"
        );
        let footer = frame.grid.row_text(height - 1);
        assert!(footer.contains("? keys"), "{footer}");
    }
    assert!(!view(&mut screen).contains("Edit Assignees"));
    for (width, height) in [(100, 30), (190, 65)] {
        let frame = screen.frame(width, height);
        let body = (0..height)
            .find(|&y| {
                frame
                    .grid
                    .row_text(y)
                    .contains("A real native issue layout.")
            })
            .unwrap();
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        // Header, tabs and the metadata rows, then the description: not a wall of controls.
        assert!(body <= 14, "description starts at row {body}:\n{text}");
        assert!(
            text.contains("Labels") && text.contains("Development"),
            "{text}"
        );
        assert!(text.contains("/ filter"), "{text}");
        assert!(!text.contains("Edit title and description"));
    }
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "First line\nq[] keep this draft");
    key(&mut screen, KeyCode::Up);
    key(&mut screen, KeyCode::Home);
    paste(&mut screen, "界e\u{0301} ");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::End);
    key(&mut screen, KeyCode::Char('X'));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("q[] keep this draft"));
    screen.write_refusal = Some("Untrusted workspace".into());
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Untrusted workspace"));
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["issue".into(), "comment".into()]))
    );
    screen.write_refusal = None;
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    wait(&mut screen, "keep this draftX");
    let comments = calls();
    let comment = comments
        .iter()
        .find(|args| args.starts_with(&["issue".into(), "comment".into()]))
        .unwrap();
    let at = comment.iter().position(|arg| arg == "--body").unwrap();
    assert_eq!(
        comment[at + 1],
        "界e\u{0301} First line\nq[] keep this draftX"
    );
    assert!(comment.contains(&"github.com/example/demo".into()));
    activate(&mut screen, "Labels");
    key(&mut screen, KeyCode::End);
    paste(&mut screen, ",native");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.contains(&"--add-label".into()) && args.contains(&"native".into()))
    );
    wait(&mut screen, "Create a branch");
    activate(&mut screen, "Create a branch");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["issue".into(), "develop".into()])
                && args.contains(&"issue-7".into()))
    );
    activate(&mut screen, "#9 Native review");
    wait(&mut screen, "into main from feature/native");
    assert!(!view(&mut screen).contains("New worktree"));
    let compact = screen.frame(100, 30);
    // Title, state and branches, then the tabs: a compact pinned header.
    let tabs = (0..30)
        .find(|&y| compact.grid.row_text(y).contains("Commits"))
        .unwrap();
    assert!(tabs <= 6, "PR tabs must not become a bulky pinned header");
    git(
        Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap()),
        &["switch", "-c", "feature/native"],
    );
    // `n` walks the unresolved threads in the Conversation; from the list it opens a new
    // pull request.
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('n'));
    wait(&mut screen, "Create pull request");
    paste(&mut screen, "New draft PR");
    // Title, Description, Base branch, then the toggles: the branch is not pushed here, so
    // untick the push (nothing leaves this machine) and tick Open as draft.
    wait(&mut screen, "☑ Push to origin first");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Tab);
    }
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, "☑ Open as draft");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Review draft pull request");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.contains(&"repos/example/demo/pulls".into())
                && args.contains(&"head=example:feature/native".into())
                && args.contains(&"draft=true".into()))
    );
    wait(&mut screen, "into main from feature/native");
    key(&mut screen, KeyCode::Char('L'));
    activate(&mut screen, "Keep drafts");
    wait(&mut screen, "fn after()");
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, "fn after()");
    assert!(!view(&mut screen).contains("fn second()"));
    activate(&mut screen, "other.rs");
    wait(&mut screen, "fn second()");
    assert!(!view(&mut screen).contains("fn after()"));
    activate(&mut screen, "nested/");
    let folded = view(&mut screen);
    assert!(
        (folded.contains("▸ nested/") || folded.contains("▸ src/nested/"))
            && folded.contains("fn second()")
    );
    activate(&mut screen, "example.rs");
    wait(&mut screen, "fn after()");
    wait(&mut screen, "Mark viewed");
    screen.write_refusal = Some("Untrusted workspace".into());
    activate(&mut screen, "Mark viewed");
    assert!(view(&mut screen).contains("Untrusted workspace"));
    assert!(
        !calls()
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("markFileAsViewed")))
    );
    screen.write_refusal = None;
    activate(&mut screen, "Mark viewed");
    idle(&mut screen);
    wait(&mut screen, "☑ Viewed");
    assert!(
        calls()
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("markFileAsViewed")))
    );
    screen = GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "into main from feature/native");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "☑ Viewed");
    let wide = screen.frame(190, 65);
    let diff_row = (0..65)
        .find(|&y| wide.grid.row_text(y).contains("fn after()"))
        .unwrap();
    // The code sits right of the file tree, whose files carry their Viewed box.
    assert!(wide.grid.row_text(diff_row).find("fn after()").unwrap() > 40);
    // The list stays beside Files, as beside every reader tab; `Space e` hides it, so the
    // reader and its diff take the width, and brings it back.
    let code_column = |frame: &Frame| {
        let row = (0..65)
            .map(|y| frame.grid.row_text(y))
            .find(|row| row.contains("fn after()"))
            .unwrap();
        row[..row.find("fn after()").unwrap()].chars().count()
    };
    let tree = (0..65)
        .map(|y| wide.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(drawn_reader_left(&wide) > 0, "{tree}");
    assert!(tree.contains("☑ example.rs"), "{tree}");
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    let full = screen.frame(190, 65);
    assert_eq!(drawn_reader_left(&full), 0);
    assert!(code_column(&full) < code_column(&wide));
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    let back = screen.frame(190, 65);
    assert_eq!(drawn_reader_left(&back), drawn_reader_left(&wide));
    assert_eq!(code_column(&back), code_column(&wide));
    focus_control(&mut screen, "fn after()");
    let compact = screen.frame(100, 30);
    let (x, y, _) = compact.cursor.unwrap();
    assert_eq!(compact.grid.get(x, y).unwrap().symbol, "f");
    assert!(compact.grid.row_text(y).contains("fn after()"));
    key(&mut screen, KeyCode::Char('c'));
    assert!(view(&mut screen).contains("Comment on example.rs:1"));
    paste(&mut screen, "Inline comment");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(calls().iter().any(
        |args| args.contains(&"repos/example/demo/pulls/9/comments".into())
            && args.contains(&"line=1".into())
            && args.contains(&"side=RIGHT".into())
    ));
    wait(&mut screen, "Inline comment");
    assert!(view(&mut screen).contains("Saved on GitHub"));
    assert!(!view(&mut screen).contains("COMMENT_NODE_1"));
    assert!(view(&mut screen).contains("Please keep this draft."));
    activate(&mut screen, "Review changes");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Tab);
    paste(&mut screen, "Reviewed in the TUI");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["pr".into(), "review".into()])
                && args.contains(&"--approve".into()))
    );
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, "Please keep this draft.");
    focus_control(&mut screen, "Resolve conversation");
    assert!(!view(&mut screen).contains("Merge pull request"));
    activate(&mut screen, "Resolve conversation");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    // A resolved thread folds to its header.
    wait(&mut screen, "● Resolved");
    assert!(
        !view(&mut screen).contains("Merge pull request"),
        "resolving one thread must not resolve the separate inline comment thread"
    );
    activate(&mut screen, "Resolve conversation");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    // The merge box ends the Conversation and arms only once every thread is resolved.
    key(&mut screen, KeyCode::Char('m'));
    wait(&mut screen, "Merge pull request");
    let merge_box = view(&mut screen);
    assert!(!merge_box.contains("Pinned to") && !merge_box.contains("a newer push cancels"));
    activate(&mut screen, "Merge pull request");
    assert!(
        view(&mut screen).contains("Merges aaaaaaa · new commits pushed before then cancel it")
    );
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["pr".into(), "merge".into()]))
    );
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["pr".into(), "merge".into()])
                && args.contains(&"--match-head-commit".into())
                && args.contains(&"a".repeat(40)))
    );
    goto(&mut screen, 2, 3);
    wait(&mut screen, "Native CI");
    // `F` chooses the workflow; `d` reads its definition and opens the dispatch sheet.
    key(&mut screen, KeyCode::Char('F'));
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char('d'));
    wait(&mut screen, "dry_run");
    assert!(view(&mut screen).contains("Run Native CI"));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    assert!(
        !calls()
            .iter()
            .any(|args| args.starts_with(&["workflow".into(), "run".into()]))
    );
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["workflow".into(), "run".into()])
                && args.contains(&"target=production".into())
                && args.contains(&"dry_run=false".into()))
    );
    // The run shows beside the history: its jobs, passing ones closed, no log read yet.
    wait(&mut screen, "Validate native");
    wait(&mut screen, "test (windows)");
    let end = Instant::now() + Duration::from_secs(20);
    while view(&mut screen).contains("running") {
        assert!(
            Instant::now() < end,
            "the run never finished:\n{}",
            view(&mut screen)
        );
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!calls().iter().any(|args| args.contains(&"--log".into())));
    // Enter focuses the run; the job opens to its steps and the step to its log.
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "all native checks passed");
    assert!(!view(&mut screen).contains("Approximate"));
    assert!(!view(&mut screen).contains("[32m"));
    let compact = screen.frame(100, 30);
    let compact_text = (0..30)
        .map(|y| compact.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compact_text.contains("all native checks passed"),
        "the step log must show beside the run history at 100×30:\n{compact_text}"
    );
    key(&mut screen, KeyCode::Char('?'));
    let help = screen.frame(100, 30);
    let help_text = (0..30)
        .map(|y| help.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        help_text.contains("Pan 8 columns") && help_text.contains("Refresh"),
        "{help_text}"
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('f'));
    wait(&mut screen, "Current runner version");
    assert!(!view(&mut screen).contains("UNKNOWN STEP"));
    // `a` lists the attempts; the earlier one is read with `--attempt 1`.
    key(&mut screen, KeyCode::Char('a'));
    wait(&mut screen, "Attempt 1");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        screen.poll();
        view(&mut screen);
        if calls()
            .iter()
            .any(|args| args.contains(&"--attempt".into()) && args.contains(&"1".into()))
        {
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The Files reader pins the file's heading under the tabs, above the scrolling diff.
fn pinned_file(frame: &Frame, path: &str) -> bool {
    (4..9).any(|y| {
        let row = frame.grid.row_text(y);
        row.contains(path) && row.contains('◇')
    })
}
#[test]
fn native_thread_replies_and_narrow_files_preserve_context_and_scoped_drafts() {
    if fixture_child("native_thread_replies_and_narrow_files_preserve_context_and_scoped_drafts") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let comment = |author: &str, body: &str| {
        serde_json::json!({
            "author":{"login":author},"body":body,"path":"example.rs","line":1,"originalLine":1
        })
    };
    let mut diff = "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1,16 +1,16 @@\n".to_owned();
    for line in 1..=15 {
        diff.push_str(&format!(" context_{line:02}\n"));
    }
    diff.push_str("-before\n+after\ndiff --git a/src/nested/other.rs b/src/nested/other.rs\n--- /dev/null\n+++ b/src/nested/other.rs\n@@ -0,0 +1 @@\n+fn second() {}\n");
    let fixture = serde_json::json!({
        "calls":[],"comments":[],"diff":diff,"fail_reply":1,
        "prs":[{"number":9,"title":"Native review"},{"number":10,"title":"Other pull request"}],
        "review_threads":{
            "example/demo#9":[
                {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[comment("alice","First original conversation")]}},
                {"id":"THREAD_B","isResolved":true,"comments":{"nodes":[comment("bob","Second original conversation")]}}
            ],
            "example/demo#10":[
                {"id":"THREAD_C","isResolved":false,"comments":{"nodes":[comment("carol","Other PR conversation")]}}
            ]
        }
    });
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "into main from feature/native");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "context_06");
    // Diff, viewed state, and review threads are independent requests. Source is
    // usable before metadata arrives; wait for the control we assert below, not
    // GitScreen::busy(), which tracks writes rather than GitHub read readiness.
    wait(&mut screen, "Mark viewed");
    control(&mut screen, KeyCode::Char('l'));
    for (width, height) in [(64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        // Every tab keeps its name at every width, never a bare icon; below 110
        // columns no reader shows counts (r12: one terminal width decides them, so Files
        // reads like the other tabs, with the list beside it or hidden).
        let files = "Files";
        for expected in [
            "#9 Native review",
            files,
            "Conversation",
            "example.rs",
            "Mark viewed",
            "context_01",
            "context_06",
        ] {
            assert!(
                text.contains(expected),
                "missing {expected} on first Files use at {width}x{height}:\n{text}"
            );
        }
        let (x, y, _) = frame
            .cursor
            .expect("Files review must have a native code cursor");
        assert_eq!(
            frame.grid.get(x, y).unwrap().symbol,
            "c",
            "cursor must be on source, not line numbers"
        );
        assert!(frame.grid.row_text(y).contains("context_01"));
    }
    key(&mut screen, KeyCode::End);
    let narrow = screen.frame(64, 20);
    assert!(
        pinned_file(&narrow, "example.rs"),
        "file identity must remain pinned while scrolling discussions"
    );
    key(&mut screen, KeyCode::Char('f'));
    assert!(view(&mut screen).contains("Choose changed file"));
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "fn second()");
    assert!(!view(&mut screen).contains("context_01"));
    for (width, height) in [(64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame
            .cursor
            .expect("file chooser must reveal the selected source");
        assert_eq!(frame.grid.get(x, y).unwrap().symbol, "f");
        assert!(frame.grid.row_text(y).contains("fn second()"));
        assert!(pinned_file(&frame, "other.rs"));
    }
    key(&mut screen, KeyCode::Char('f'));
    key(&mut screen, KeyCode::Up);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "First original conversation");
    for (width, height) in [(64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame
            .cursor
            .expect("returning to a file must reveal its source");
        assert_eq!(frame.grid.get(x, y).unwrap().symbol, "c");
        assert!(frame.grid.row_text(y).contains("context_01"));
        assert!(pinned_file(&frame, "example.rs"));
    }
    activate(&mut screen, "Reply / continue reply");
    let reply = view(&mut screen);
    assert!(reply.contains("Reply to @alice"), "{reply}");
    paste(&mut screen, "Unique draft A\nKeep the original thread");
    screen.write_refusal = Some("Untrusted workspace".into());
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Untrusted workspace"));
    assert!(
        !calls()
            .iter()
            .flatten()
            .any(|arg| arg.contains("addPullRequestReviewThreadReply"))
    );
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Tab); // Resolve first thread.
    key(&mut screen, KeyCode::Tab); // Reply to the second, paginated thread.
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Reply to @bob"));
    assert!(!view(&mut screen).contains("Unique draft A"));
    paste(&mut screen, "Unique draft B");
    key(&mut screen, KeyCode::Escape);
    // Both threads have the same button label. Start from the top to target
    // Alice's first thread, rather than reopening Bob's currently selected one.
    key(&mut screen, KeyCode::Home);
    activate(&mut screen, "Reply / continue reply");
    assert!(view(&mut screen).contains("Reply to @alice"));
    assert!(view(&mut screen).contains("Unique draft A"));
    assert!(!view(&mut screen).contains("Unique draft B"));
    control(&mut screen, KeyCode::Enter);
    wait(
        &mut screen,
        "GitHub rejected the thread reply · your draft is kept",
    );
    activate(&mut screen, "Reply / continue reply");
    assert!(
        view(&mut screen).contains("Unique draft A"),
        "a rejected write must preserve its exact reply draft"
    );
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Unique draft A");
    wait(&mut screen, "Saved on GitHub");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(saved["reply_targets"].as_array().unwrap().len(), 1);
    assert_eq!(saved["reply_targets"][0]["thread"], "THREAD_A");
    assert_eq!(saved["reply_targets"][0]["repository"], "example/demo");
    assert_eq!(saved["reply_targets"][0]["host"], "github.com");
    assert_eq!(
        saved["reply_targets"][0]["body"],
        "Unique draft A\nKeep the original thread"
    );
    assert!(
        saved["inline_comments"].is_null(),
        "reply must not post a new inline thread"
    );
    activate(&mut screen, "Reply / continue reply");
    assert!(view(&mut screen).contains("Reply to @alice"));
    // A successful submission clears only that thread's draft; the second is intact.
    let posted = calls()
        .iter()
        .flatten()
        .filter(|arg| arg.contains("addPullRequestReviewThreadReply"))
        .count();
    control(&mut screen, KeyCode::Enter);
    assert!(
        view(&mut screen).contains("Reply is required"),
        "a successful submission must clear its composer draft"
    );
    assert_eq!(
        calls()
            .iter()
            .flatten()
            .filter(|arg| arg.contains("addPullRequestReviewThreadReply"))
            .count(),
        posted,
        "an empty reply must not create a duplicate comment"
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Unique draft B"));
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Other PR conversation");
    activate(&mut screen, "Reply / continue reply");
    let frame = view(&mut screen);
    assert!(frame.contains("Reply to @carol"));
    assert!(!frame.contains("Unique draft A") && !frame.contains("Unique draft B"));
}

#[test]
fn github_closed_count_arrives_with_the_open_list() {
    if fixture_child("github_closed_count_arrives_with_the_open_list") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[], "comments":["count fixture ready"],
            "issues":[
                {"number":7,"title":"First issue"},
                {"number":8,"title":"Second issue"},
                {"number":5,"title":"Old closed issue","state":"CLOSED"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "count fixture ready");
    // GitHub counts the closed side in the open list's own request: `1 Closed` before the
    // closed list was ever read, and no read of it.
    let end = Instant::now() + Duration::from_secs(30);
    while !compact_list(&mut screen).contains("1 Closed") {
        assert!(Instant::now() < end, "{}", compact_list(&mut screen));
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    let list = compact_list(&mut screen);
    assert!(
        list.contains("2 Open") && !list.contains("Old closed issue"),
        "{list}"
    );
    assert!(
        !calls()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "closed")),
        "{:?}",
        calls()
    );
}

#[test]
fn github_triage_queries_refresh_and_selection_are_native_and_scoped() {
    if fixture_child("github_triage_queries_refresh_and_selection_are_native_and_scoped") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let first =
        serde_json::json!({"number":7,"title":"First issue","body":"First issue description."});
    let second =
        serde_json::json!({"number":8,"title":"Second issue","body":"Second issue description."});
    let mut fixture = serde_json::json!({
        "calls":[], "comments":["triage fixture ready"], "issues":[first.clone(), second.clone()],
        "issue_queries":{"label:bug":[first.clone()], "author:@me":[second.clone()], "assignee:@me":[second.clone()]},
        "pr_queries":{"review-requested:@me":[{"number":9,"title":"Ready for review"}]},
        "prs":[
            {"number":9,"title":"Ready for review"},
            {"number":10,"title":"Draft work","isDraft":true,"reviewDecision":"REVIEW_REQUIRED","changedFiles":5,
             "statusCheckRollup":[{"status":"IN_PROGRESS","conclusion":""},{"state":"FAILURE"}]},
            {"number":11,"title":"Already merged","state":"MERGED","isDraft":true,"reviewDecision":"",
             "statusCheckRollup":null,"changedFiles":1,"comments":[]}
        ]
    });
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "triage fixture ready");
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Second issue description.");
    wait(&mut screen, "triage fixture ready");
    // An insertion above the selection and a failed detail refresh must not switch items
    // or destroy the body which was already read successfully.
    fixture = serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["issues"] = serde_json::json!([
        {"number":6,"title":"Newly inserted issue","body":"New item."}, first, second
    ]);
    fixture["fail_item_views"] = 1.into();
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    assert!(view(&mut screen).contains("Second issue description."));
    // "· showing " alone also matches the footer's "Refreshing details… · showing what was
    // loaded" while the read is still out; the failure names itself.
    wait(&mut screen, "Fixture refresh refused · showing ");
    let frame = screen.frame(100, 30);
    assert!(lists(&cursor_record(&frame), 8, "Second issue"));
    let text = (0..30)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Second issue description."));
    assert!(text.contains("Fixture refresh refused"));

    key(&mut screen, KeyCode::Char('/'));
    paste(&mut screen, "label:bug");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "First issue description.");
    let list = compact_list(&mut screen);
    // `label:bug` is the Bugs section; the footer carries the query.
    assert!(
        view(&mut screen)
            .lines()
            .last()
            .unwrap()
            .contains("label:bug")
    );
    assert!(
        list.contains("First issue") && !list.contains("Second issue"),
        "{list}"
    );
    assert!(calls().iter().any(|args| {
        args.windows(2)
            .any(|pair| pair[0] == "--search" && pair[1] == "label:bug")
    }));

    key(&mut screen, KeyCode::Char('/'));
    control(&mut screen, KeyCode::Char('e'));
    control(&mut screen, KeyCode::Char('u'));
    paste(&mut screen, "author:@me");
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Second issue description.");
    let list = compact_list(&mut screen);
    assert!(
        list.contains("is:issue author:@me") && list.contains("Showing 1"),
        "{list}"
    );
    assert!(!list.contains("First issue"));

    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, "Repository views");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Second issue description.");
    assert!(calls().iter().any(|args| {
        args.windows(2)
            .any(|pair| pair[0] == "--search" && pair[1] == "assignee:@me")
    }));

    goto(&mut screen, 1, 2);
    wait(&mut screen, "into main from feature/native");
    key(&mut screen, KeyCode::Char('/'));
    assert!(
        !view(&mut screen).contains("author:@me"),
        "issue filter drafts must not leak into PR queries"
    );
    key(&mut screen, KeyCode::Tab);
    for _ in 0..3 {
        key(&mut screen, KeyCode::Down);
    }
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Already merged");
    let list = compact_list(&mut screen);
    // One row per record at this height: state, number, title, chips and marks.
    for number in ["#9 ", "#10 ", "#11 "] {
        assert!(list.contains(number), "{list}");
    }
    assert!(list.contains("Draft") && list.contains("Merged"), "{list}");
    // The search box names every state by naming none: `is:pr` alone.
    assert!(
        list.contains("is:pr") && !list.contains("is:open") && list.contains("Showing 3"),
        "{list}"
    );
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Up);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Already merged");
    let list = compact_list(&mut screen);
    assert!(
        list.contains("is:pr is:merged")
            && list.contains("Showing 1")
            && !list.contains("#9 ")
            && !list.contains("#10 "),
        "{list}"
    );
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, "Repository views");
    for _ in 0..4 {
        key(&mut screen, KeyCode::Down);
    }
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "Ready for review");
    let list = compact_list(&mut screen);
    assert!(
        list.contains("Showing 1") && !list.contains("Draft work") && !list.contains("#11 "),
        "{list}"
    );
    assert!(calls().iter().any(|args| {
        args.windows(2)
            .any(|pair| pair[0] == "--search" && pair[1] == "review-requested:@me")
            && args
                .windows(2)
                .any(|pair| pair[0] == "--state" && pair[1] == "open")
    }));
    assert!(!calls().iter().any(|args| {
        args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "POST" | "comment" | "edit" | "review" | "merge"
            )
        })
    }));
}

#[test]
fn rapid_github_navigation_reads_the_latest_target_before_skipped_items() {
    if fixture_child("rapid_github_navigation_reads_the_latest_target_before_skipped_items") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":["Navigation activity ready"],"issues":navigation_issues(),
            "read_gates":[
                {"prefix":["issue","view","7"],"name":"old-body"},
                {"prefix":["issue","view","1"],"name":"latest-body"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Navigation activity ready");
    key(&mut screen, KeyCode::Char('j'));
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
    let blocked_calls = calls().len();
    for selected in [6, 5, 4, 2, 1] {
        key(&mut screen, KeyCode::Char('j'));
        let warm = detail_text(&screen.frame(190, 48));
        assert!(
            warm.contains(&format!("#{selected} Navigation issue {selected}")),
            "{warm}"
        );
        assert!(
            warm.contains(&format!("Unique selected body {selected}.")),
            "{warm}"
        );
        assert!(warm.contains("Loading details"), "{warm}");
        assert!(!warm.contains("Development") && !warm.contains("Add a comment"));
        for other in [8, 7, 6, 5, 4, 2, 1]
            .into_iter()
            .filter(|id| *id != selected)
        {
            assert!(
                !warm.contains(&format!("Unique selected body {other}.")),
                "stale prose: {warm}"
            );
        }
    }
    assert_eq!(
        calls().len(),
        blocked_calls,
        "warm rendering may not start extra requests"
    );
    let loading = view(&mut screen);
    assert!(loading.contains("Unique selected body 1.") && loading.contains("Loading details"));
    assert!(
        !loading.contains("showing the list description")
            && !loading.contains("Development")
            && !loading.contains("Add a comment")
    );
    release_read("old-body");
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "1"]));
    let log = calls();
    let old = log
        .iter()
        .position(|args| is_call(args, &["issue", "view", "7"]))
        .unwrap();
    assert!(is_call(&log[old + 1], &["issue", "view", "1"]), "{log:?}");
    let pending = view(&mut screen);
    assert!(
        pending.contains("Unique selected body 1.") && !pending.contains("Unique selected body 7."),
        "a late old reply must not replace the selected list description: {pending}"
    );
    assert!(!pending.contains("Development"));
    release_read("latest-body");
    wait(&mut screen, "Add a comment");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/1/timeline"))
    });
    for id in [6, 5, 4, 2] {
        assert!(
            !calls()
                .iter()
                .any(|args| is_call(args, &["issue", "view", &id.to_string()])),
            "unused queued item {id} was read"
        );
        assert!(
            !calls()
                .iter()
                .flatten()
                .any(|arg| arg.contains(&format!("issues/{id}/timeline")))
        );
    }
    assert!(
        !calls()
            .iter()
            .flatten()
            .any(|arg| arg.contains("issues/7/timeline"))
    );
    for _ in 0..6 {
        key(&mut screen, KeyCode::Char('k'));
    }
    assert!(
        view(&mut screen).contains("Unique selected body 8."),
        "loaded cache must remain reusable"
    );
    assert_eq!(
        calls()
            .iter()
            .filter(|args| is_call(args, &["issue", "view", "8"]))
            .count(),
        1
    );
    let lists = calls()
        .into_iter()
        .filter(|args| is_call(args, &["issue", "list"]))
        .collect::<Vec<_>>();
    assert_eq!(
        lists.len(),
        1,
        "the body must ride the original list response"
    );
    assert!(
        lists[0]
            .windows(2)
            .any(|pair| pair[0] == "--json" && pair[1].split(',').any(|field| field == "body"))
    );
    assert_read_only_github_calls();
}

fn assert_find_query_input(screen: &mut GitScreen, query: &str) {
    for (width, height) in [(190, 48), (100, 30), (64, 20)] {
        let frame = screen.frame(width, height);
        assert!(detail_text(&frame).contains("Find detail"));
        let (x, y, _) = frame.cursor.expect("find query keeps its input cursor");
        let input = (0..x)
            .filter_map(|column| frame.grid.get(column, y))
            .filter(|cell| !cell.continuation)
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(
            input.ends_with(query),
            "expected query input {query:?}, got {input:?}:\n{}",
            detail_text(&frame)
        );
    }
}

#[test]
fn github_find_prompt_keeps_keyboard_input_when_metadata_and_discussion_arrive() {
    if fixture_child("github_find_prompt_keeps_keyboard_input_when_metadata_and_discussion_arrive")
    {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":["Discussion arrived while finding."],
        "issues":[{"number":7,"title":"Loading find target","body":"## Background\n\n**Rebase onto base branch** stays in this description."}],
        "read_gates":[{"prefix":["issue","view","7"],"name":"input-details"},
                      {"contains":"issues/7/timeline","name":"input-discussion"}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
    control(&mut screen, KeyCode::Char('f'));
    for character in "temporary e query".chars() {
        key(&mut screen, KeyCode::Char(character));
    }
    assert_find_query_input(&mut screen, "temporary e query");
    release_read("input-details");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/7/timeline"))
    });
    assert_find_query_input(&mut screen, "temporary e query");
    release_read("input-discussion");
    wait(&mut screen, "Discussion arrived while finding.");
    assert_find_query_input(&mut screen, "temporary e query");
    control(&mut screen, KeyCode::Char('e'));
    control(&mut screen, KeyCode::Char('u'));
    for character in "Rebase onto base branch".chars() {
        key(&mut screen, KeyCode::Char(character));
    }
    assert_find_query_input(&mut screen, "Rebase onto base branch");
    control(&mut screen, KeyCode::Enter);
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "Rebase onto base branch", "1/1");
    }
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["comments"]
        .as_array_mut()
        .unwrap()
        .push("**Rebase onto base branch** also appears in newly loaded discussion.".into());
    fixture["read_gates"] = serde_json::json!([
        {"contains":"issues/7/timeline","name":"active-find-discussion"}
    ]);
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    release_read("active-find-discussion");
    wait(&mut screen, "also appears in newly loaded discussion.");
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "Rebase onto base branch", "1/2");
    }
    key(&mut screen, KeyCode::Char('n'));
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "Rebase onto base branch", "2/2");
    }
    assert_find_read_only_calls();
}

#[test]
fn github_find_replaced_source_keeps_the_query_and_finds_in_the_fresh_text() {
    if fixture_child("github_find_replaced_source_keeps_the_query_and_finds_in_the_fresh_text") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":["Initial discussion is fully loaded."],
            "issues":[{"number":7,"title":"Original target","body":"Original source paragraph."}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Initial discussion is fully loaded.");
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["issues"][0]["body"] = "A replacement paragraph is now visible.".into();
    fixture["read_gates"] = serde_json::json!([
        {"prefix":["issue","view","7"],"name":"replaced-source"}
    ]);
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    control(&mut screen, KeyCode::Char('f'));
    for character in "retained query".chars() {
        key(&mut screen, KeyCode::Char(character));
    }
    assert_find_query_input(&mut screen, "retained query");
    release_read("replaced-source");
    wait(&mut screen, "A replacement paragraph is now visible.");
    assert_find_query_input(&mut screen, "retained query");
    // Edit and comment shortcuts stay inside the box; the query then runs on the fresh text.
    control(&mut screen, KeyCode::Char('e'));
    control(&mut screen, KeyCode::Char('u'));
    for character in "replacement".chars() {
        key(&mut screen, KeyCode::Char(character));
    }
    assert_find_query_input(&mut screen, "replacement");
    control(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("detail changed while finding"));
    assert!(!screen.busy(), "a find must not start a write");
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "replacement", "1/1");
    }
    assert_find_read_only_calls();
}

#[test]
fn github_warm_list_find_and_cancel_survive_unchanged_detail_completion() {
    if fixture_child("github_warm_list_find_and_cancel_survive_unchanged_detail_completion") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":[],
        "issues":[{"number":7,"title":"Warm identity","body":"### warm **needle** first `needle` second.\n_Unchanged description._"}],
        "read_gates":[{"prefix":["issue","view","7"],"name":"find-details"}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
    let pending = view(&mut screen);
    assert!(pending.contains("#7 Warm identity") && pending.contains("Loading details"));
    assert!(pending.contains("warm needle first needle second."));
    assert!(!pending.contains("Development"));
    let reads = calls().len();
    find_detail(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('n'));
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "needle second", "2/2");
    }
    key(&mut screen, KeyCode::Char('N'));
    issue_detail_matched(&mut screen, (100, 30), "needle first", "1/2");
    key(&mut screen, KeyCode::Char('N'));
    issue_detail_matched(&mut screen, (100, 30), "needle second", "2/2");
    let cursor = detail_cursor(&screen.frame(100, 30));
    control(&mut screen, KeyCode::Char('f'));
    replace_field(&mut screen, "cancelled warm query");
    key(&mut screen, KeyCode::Escape);
    issue_detail_matched(&mut screen, (100, 30), "needle second", "2/2");
    assert_eq!(detail_cursor(&screen.frame(100, 30)), cursor);
    assert_eq!(
        calls().len(),
        reads,
        "find/cancel/render must use only the cached list"
    );
    // Metadata may arrive behind an open find form; cancelling it must preserve
    // the old query and the exact logical match, not jump to a newly added control.
    control(&mut screen, KeyCode::Char('f'));
    replace_field(&mut screen, "another cancelled query");
    release_read("find-details");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/7/timeline"))
    });
    key(&mut screen, KeyCode::Escape);
    issue_detail_matched(&mut screen, (100, 30), "needle second", "2/2");
    // Metadata rows arrive above the description, so the match may move down the pane;
    // the cursor stays on the same match and column.
    let after = detail_cursor(&screen.frame(100, 30));
    assert_eq!((after.0, &after.2), (cursor.0, &cursor.2));
    assert!(view(&mut screen).contains("Development"));
    assert!(!view(&mut screen).contains("showing the list description"));
    key(&mut screen, KeyCode::Char('e'));
    let form = view(&mut screen);
    assert!(form.contains("Warm identity") && form.contains("Unchanged description."));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        calls()
            .iter()
            .filter(|args| is_call(args, &["issue", "view", "7"]))
            .count(),
        1
    );
    assert_read_only_github_calls();
}

#[test]
fn github_pr_warm_description_keeps_tabs_find_and_metadata_separate() {
    if fixture_child("github_pr_warm_description_keeps_tabs_find_and_metadata_separate") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":[],
        "prs":[{"number":9,"title":"Warm PR identity","body":"PR warm needle before metadata.","reviews":[]}],
        "read_gates":[{"prefix":["pr","view","9"],"name":"pr-details"},
                      {"contains":"issues/9/timeline","name":"pr-activity"}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait_call(&mut screen, |args| is_call(args, &["pr", "view", "9"]));
    let pending = view(&mut screen);
    assert!(
        pending.contains("#9 Warm PR identity")
            && pending.contains("PR warm needle before metadata.")
    );
    assert!(pending.contains("Loading details") && pending.contains("Conversation"));
    assert!(!pending.contains("On merge"));
    let reads = calls().len();
    key(&mut screen, KeyCode::Char('L'));
    assert!(!view(&mut screen).contains("PR warm needle before metadata."));
    key(&mut screen, KeyCode::Char('H'));
    find_detail(&mut screen, "needle");
    for size in DETAIL_SIZES {
        detail_matched(&mut screen, size, "needle before", "1/1");
    }
    let cursor = detail_cursor(&screen.frame(100, 30));
    assert_eq!(
        calls().len(),
        reads,
        "tabs/find must not add eager requests behind the active read"
    );
    release_read("pr-details");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/9/timeline"))
    });
    detail_matched(&mut screen, (100, 30), "needle before", "1/1");
    // Metadata rows arrive above the description: same match and column, lower row.
    let after = detail_cursor(&screen.frame(100, 30));
    assert_eq!((after.0, &after.2), (cursor.0, &cursor.2));
    let cursor = after;
    let loaded = view(&mut screen);
    assert!(loaded.contains("On merge"));
    assert!(
        !loaded.contains("showing the list description") && !loaded.contains("Merge pull request")
    );
    release_read("pr-activity");
    // The timeline joins the Conversation below the description.
    let end = Instant::now() + Duration::from_secs(30);
    while view(&mut screen).contains("Loading the conversation") {
        assert!(Instant::now() < end, "the timeline never arrived");
        screen.poll();
    }
    // Appending discussion cannot invalidate a match in the unchanged description.
    for size in DETAIL_SIZES {
        detail_matched(&mut screen, size, "needle before", "1/1");
    }
    assert_eq!(detail_cursor(&screen.frame(100, 30)), cursor);
    key(&mut screen, KeyCode::Char('n'));
    detail_matched(&mut screen, (100, 30), "needle before", "1/1");
    let log = calls();
    assert_eq!(
        log.iter()
            .filter(|args| is_call(args, &["pr", "view", "9"]))
            .count(),
        1
    );
    let lists = log
        .iter()
        .filter(|args| is_call(args, &["pr", "list"]))
        .collect::<Vec<_>>();
    assert_eq!(lists.len(), 1);
    assert!(
        lists[0]
            .windows(2)
            .any(|pair| pair[0] == "--json" && pair[1].split(',').any(|field| field == "body"))
    );
    assert_read_only_github_calls();
}

#[test]
fn github_warm_list_query_state_and_page_changes_never_reuse_unrelated_prose() {
    if fixture_child("github_warm_list_query_state_and_page_changes_never_reuse_unrelated_prose") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":[],
        "issues":[{"number":7,"title":"Inbox identity","body":"Inbox list prose."},
                  {"number":8,"title":"Assigned identity","body":"Assigned list prose."}],
        "issue_queries":{"assignee:@me":[{"number":8,"title":"Assigned identity","body":"Assigned list prose."}]},
        "read_gates":[{"prefix":["issue","view","7"],"name":"inbox-details"},
                      {"prefix":["issue","view","8"],"name":"assigned-details"}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
    assert!(view(&mut screen).contains("Inbox list prose."));
    choose_view(&mut screen, "Assigned to me");
    assert!(!view(&mut screen).contains("Inbox list prose."));
    choose_view(&mut screen, "All open");
    assert!(
        view(&mut screen).contains("Inbox list prose."),
        "cached list should be usable before detail"
    );
    goto(&mut screen, 1, 2);
    assert!(
        !view(&mut screen).contains("Inbox list prose."),
        "issue prose must not appear on the PR page"
    );
    goto(&mut screen, 2, 1);
    choose_view(&mut screen, "Assigned to me");
    release_read("inbox-details");
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "8"]));
    let assigned = view(&mut screen);
    assert!(assigned.contains("#8 Assigned identity") && assigned.contains("Assigned list prose."));
    assert!(assigned.contains("Loading details") && !assigned.contains("Inbox list prose."));
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down); // closed, not the cached open-state list
    control(&mut screen, KeyCode::Enter);
    let changed = view(&mut screen);
    assert!(!changed.contains("Inbox list prose.") && !changed.contains("Assigned list prose."));
    choose_view(&mut screen, "Assigned to me");
    assert!(view(&mut screen).contains("Assigned list prose."));
    release_read("assigned-details");
    wait(&mut screen, "Development");
    assert!(!view(&mut screen).contains("Inbox list prose."));
    assert_read_only_github_calls();
}

#[test]
fn github_warm_list_empty_missing_and_failed_details_are_truthful_and_scoped() {
    if fixture_child("github_warm_list_empty_missing_and_failed_details_are_truthful_and_scoped") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    // Reopening with the same item number under another repository/host must
    // never borrow a previous workspace's summary or full metadata.
    for (name, host, repo, body, missing, fail) in [
        ("empty", "github.com", "example/demo", "", false, false),
        (
            "missing",
            "github.com",
            "other/demo",
            "Full description after an older list response.",
            true,
            false,
        ),
        (
            "failed",
            "git.example.test",
            "other/demo",
            "Available list description after detail failure.",
            false,
            true,
        ),
    ] {
        std::fs::write(
            &state_path,
            serde_json::json!({
                "calls":[],"comments":["Warm fixture settled"],"host":host,"repository":repo,
                "issues":[{"number":7,"title":format!("Scoped {name}"),"body":body}],
                "list_omit_fields":if missing { vec!["body"] } else { vec![] },
                "fail_item_views":u8::from(fail),
                "read_gates":[{"prefix":["issue","view","7"],"name":name}]
            })
            .to_string(),
        )
        .unwrap();
        let mut screen =
            GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
        goto(&mut screen, 0, 1);
        wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
        let pending = view(&mut screen);
        assert!(
            pending.contains(&format!("#7 Scoped {name}")) && pending.contains("Loading details")
        );
        assert_eq!(
            pending.contains("No description provided."),
            body.is_empty()
        );
        if !body.is_empty() {
            assert_eq!(pending.contains(body), !missing);
        }
        assert!(!pending.contains("Development") && !pending.contains("Add a comment"));
        key(&mut screen, KeyCode::Char('e'));
        assert!(
            view(&mut screen).contains("Item is loading"),
            "a list summary is not editable metadata"
        );
        release_read(name);
        if fail {
            wait(&mut screen, "Details unavailable");
            let failed = view(&mut screen);
            assert!(failed.contains(body) && failed.contains("Details unavailable"));
            assert!(failed.contains("Fixture refresh refused") && failed.contains("R retry"));
            assert!(!failed.contains("Development") && !failed.contains("Add a comment"));
        } else {
            wait(&mut screen, "Development");
            wait(&mut screen, "Warm fixture settled");
            let loaded = view(&mut screen);
            assert!(!loaded.contains("showing the list description"));
            if !body.is_empty() {
                assert!(loaded.contains(body));
            }
        }
        // The last serialized read saves the log before replying. No previous
        // scope's fixture process may overwrite the next iteration's state.
        wait_call(&mut screen, |args| {
            args.iter().any(|arg| arg.contains("issues/7/timeline"))
        });
        assert_eq!(
            calls()
                .iter()
                .filter(|args| is_call(args, &["issue", "view", "7"]))
                .count(),
            1
        );
        for args in calls()
            .into_iter()
            .filter(|args| is_call(args, &["issue", "list"]) || is_call(args, &["issue", "view"]))
        {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--repo" && pair[1] == format!("{host}/{repo}"))
            );
        }
        assert_read_only_github_calls();
    }
}

#[test]
fn github_commit_tab_requeues_pruned_source_and_preserves_it_on_refresh_error() {
    if fixture_child("github_commit_tab_requeues_pruned_source_and_preserves_it_on_refresh_error") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":[],
            "read_gates":[{"contains":"issues/9/timeline","name":"commit-prerequisite"}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/9/timeline"))
    });
    key(&mut screen, KeyCode::Char('L'));
    activate(&mut screen, "Keep drafts");
    key(&mut screen, KeyCode::Char('L')); // Checks prunes the queued commit read.
    key(&mut screen, KeyCode::Char('H')); // Returning must recover the missing source read.
    release_read("commit-prerequisite");
    wait(&mut screen, "fn after()");
    let log = calls();
    let timeline = log
        .iter()
        .position(|args| args.iter().any(|arg| arg.contains("issues/9/timeline")))
        .unwrap();
    assert!(
        log[timeline + 1]
            .iter()
            .any(|arg| arg.ends_with(&format!("commits/{}", "a".repeat(40)))),
        "selected commit source must precede optional metadata: {log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|args| args.iter().any(|arg| arg.contains("/commits/")))
            .count(),
        1
    );
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["fail_commit_views"] = 1.into();
    let temporary = Path::new(&state_path).with_extension("input");
    std::fs::write(&temporary, fixture.to_string()).unwrap();
    std::fs::rename(&temporary, &state_path).unwrap();
    key(&mut screen, KeyCode::Char('R'));
    assert!(view(&mut screen).contains("fn after()"));
    wait(&mut screen, "is having problems");
    let cached = view(&mut screen);
    let notice = cached
        .lines()
        .find(|line| line.contains("is having problems"))
        .unwrap();
    assert!(
        notice.contains("· showing ") && notice.contains("· R retry"),
        "{notice}"
    );
    assert_eq!(notice.matches("R retry").count(), 1, "{notice}");
    assert!(!cached.contains("· r retry"), "{cached}");
    assert!(
        cached.contains("fn after()") && cached.contains("· showing "),
        "{cached}"
    );
    assert_read_only_github_calls();
}

#[test]
fn queued_github_write_survives_navigation_and_clears_only_its_own_draft() {
    if fixture_child("queued_github_write_survives_navigation_and_clears_only_its_own_draft") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":["Write fixture ready"],"issues":navigation_issues(),
            "host":"git.example.test","fail_comments":1,
            "read_gates":[{"prefix":["issue","view","7"],"name":"writing-source"}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Write fixture ready");
    key(&mut screen, KeyCode::Char('j'));
    wait_call(&mut screen, |args| is_call(args, &["issue", "view", "7"]));
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "Scoped queued draft seven");
    control(&mut screen, KeyCode::Enter);
    assert!(
        screen.busy(),
        "the confirmed write must be queued behind the active read"
    );
    assert!(
        !calls()
            .iter()
            .any(|args| is_call(args, &["issue", "comment"]))
    );
    for _ in 0..5 {
        key(&mut screen, KeyCode::Char('j'));
    }
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "Independent draft one");
    key(&mut screen, KeyCode::Escape);
    release_read("writing-source");
    wait(
        &mut screen,
        "GitHub rejected the comment · your draft is kept",
    );
    wait(&mut screen, "Unique selected body 1.");
    let log = calls();
    let old = log
        .iter()
        .position(|args| is_call(args, &["issue", "view", "7"]))
        .unwrap();
    assert!(
        is_call(&log[old + 1], &["issue", "comment", "7"]),
        "{log:?}"
    );
    let write = &log[old + 1];
    assert!(
        write
            .windows(2)
            .any(|pair| pair[0] == "--repo" && pair[1] == "git.example.test/example/demo")
    );
    assert!(
        write
            .windows(2)
            .any(|pair| pair[0] == "--body" && pair[1] == "Scoped queued draft seven")
    );
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Independent draft one"));
    assert!(!view(&mut screen).contains("Scoped queued draft seven"));
    key(&mut screen, KeyCode::Escape);
    for _ in 0..5 {
        key(&mut screen, KeyCode::Char('k'));
    }
    key(&mut screen, KeyCode::Char('C'));
    assert!(
        view(&mut screen).contains("Scoped queued draft seven"),
        "a rejected queued write must retain its source draft"
    );
    control(&mut screen, KeyCode::Enter);
    for _ in 0..5 {
        key(&mut screen, KeyCode::Char('j'));
    }
    wait(&mut screen, "Saved on GitHub");
    key(&mut screen, KeyCode::Char('C'));
    assert!(
        view(&mut screen).contains("Independent draft one"),
        "success must not clear the currently selected item's draft"
    );
    key(&mut screen, KeyCode::Escape);
    for _ in 0..5 {
        key(&mut screen, KeyCode::Char('k'));
    }
    key(&mut screen, KeyCode::Char('C'));
    let form = view(&mut screen);
    assert!(!form.contains("Scoped queued draft seven") && !form.contains("Independent draft one"));
    assert_eq!(
        calls()
            .iter()
            .filter(|args| is_call(args, &["issue", "comment", "7"]))
            .count(),
        2
    );
}

#[test]
fn github_files_tabs_queries_and_views_reload_pruned_metadata_without_hiding_source() {
    if fixture_child(
        "github_files_tabs_queries_and_views_reload_pruned_metadata_without_hiding_source",
    ) {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let diff = |body: &str| {
        format!(
            "diff --git a/example.rs b/example.rs\n--- /dev/null\n+++ b/example.rs\n@@ -0,0 +1 @@\n+fn {body}() {{}}\ndiff --git a/src/nested/other.rs b/src/nested/other.rs\n--- /dev/null\n+++ b/src/nested/other.rs\n@@ -0,0 +1 @@\n+fn selected_other_source() {{}}\n"
        )
    };
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":["Selected PR activity"],
        "prs":[
            {"number":9,"title":"Old PR","body":"Old PR description."},
            {"number":10,"title":"Skipped PR","body":"Skipped PR description."},
            {"number":11,"title":"Selected PR","body":"Selected PR description."}
        ],
        "pr_queries":{"assignee:@me":[{"number":10}],"author:@me":[{"number":9}]},
        "diffs":{"9":diff("old_pr_source"),"10":diff("skipped_pr_source"),"11":diff("selected_pr_source")},
        "review_threads":{"example/demo#11":[{"id":"SELECTED_THREAD","isResolved":false,"comments":{"nodes":[
            {"author":{"login":"reviewer"},"body":"Selected PR conversation","path":"src/nested/other.rs","line":1,"originalLine":1}
        ]}}]},
        "read_gates":[
            {"contains":"issues/9/timeline","name":"old-activity"},
            {"prefix":["pr","diff","11"],"name":"selected-diff"},
            {"contains":"viewerViewedState","name":"viewed-metadata"},
            {"contains":"reviewThreads","name":"thread-metadata"}
        ]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/9/timeline"))
    });
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    control(&mut screen, KeyCode::Char('h')); // Return from tab reader to the PR list.
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('j'));
    release_read("old-activity");
    wait_call(&mut screen, |args| is_call(args, &["pr", "diff", "11"]));
    let log = calls();
    let selected = log
        .iter()
        .position(|args| is_call(args, &["pr", "view", "11"]))
        .unwrap();
    assert!(
        is_call(&log[selected + 1], &["pr", "diff", "11"]),
        "visible diff must precede optional metadata: {log:?}"
    );
    assert!(!log.iter().any(|args| is_call(args, &["pr", "view", "10"]) || is_call(args, &["pr", "diff", "9"])));
    release_read("selected-diff");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("viewerViewedState"))
    });
    wait(&mut screen, "fn selected_pr_source()");
    assert!(
        view(&mut screen).contains("Loading"),
        "metadata loading must remain truthful without blocking source"
    );
    key(&mut screen, KeyCode::Char('f'));
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "fn selected_other_source()");
    assert!(!view(&mut screen).contains("fn selected_pr_source()"));
    key(&mut screen, KeyCode::Char('H')); // Checks prunes pending Files metadata.
    key(&mut screen, KeyCode::Char('L')); // Files must enqueue it again.
    // Back to the list: in the Files reader `v` marks the file viewed.
    control(&mut screen, KeyCode::Char('h'));
    choose_view(&mut screen, "Assigned to me");
    key(&mut screen, KeyCode::Char('/'));
    replace_field(&mut screen, "author:@me");
    control(&mut screen, KeyCode::Enter);
    choose_view(&mut screen, "All open");
    assert!(
        view(&mut screen).contains("fn selected_other_source()"),
        "restoring a cached view must restore selected file identity"
    );
    release_read("viewed-metadata");
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("reviewThreads"))
    });
    let source = view(&mut screen);
    assert!(
        source.contains("fn selected_other_source()") && source.contains("other.rs"),
        "{source}"
    );
    assert!(!source.contains("old_pr_source") && !source.contains("skipped_pr_source"));
    release_read("thread-metadata");
    wait(&mut screen, "Selected PR conversation");
    wait(&mut screen, "Mark viewed");
    let log = calls();
    assert!(
        log.iter()
            .filter(|args| is_call(args, &["pr", "diff", "11"]))
            .count()
            >= 2,
        "view refresh must not strand a pruned source refresh: {log:?}"
    );
    assert!(!log.iter().any(|args| is_call(args, &["pr", "list"])
        && args.windows(2).any(|pair| pair[0] == "--search"
            && matches!(pair[1].as_str(), "assignee:@me" | "author:@me"))));
    for args in log.iter().filter(|args| {
        args.iter()
            .any(|arg| arg.contains("reviewThreads") || arg.contains("viewerViewedState"))
    }) {
        assert!(
            args.contains(&"number=11".into()),
            "metadata must belong to the selected PR: {args:?}"
        );
        assert!(args.contains(&"owner=example".into()) && args.contains(&"name=demo".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--hostname" && pair[1] == "github.com")
        );
    }
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('H')); // The Conversation's never-loaded timeline is needed now.
    }
    wait_call(&mut screen, |args| {
        args.iter().any(|arg| arg.contains("issues/11/timeline"))
    });
    wait(&mut screen, "Selected PR activity");
    let conversation = view(&mut screen);
    assert!(
        conversation.contains("Selected PR description.")
            && !conversation.contains("selected_other_source")
    );
    assert_read_only_github_calls();
}

fn replace_field(screen: &mut GitScreen, text: &str) {
    control(screen, KeyCode::Char('e'));
    control(screen, KeyCode::Char('u'));
    paste(screen, text);
}
fn local_views(root: &Path) -> serde_json::Value {
    let repo = git2::Repository::open(root).unwrap();
    let config = repo
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    serde_json::from_str(&config.get_string("dusty-git.githubViews").unwrap()).unwrap()
}
fn choose_view(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('v'));
    assert!(view(screen).contains("Repository views"));
    replace_field(screen, name);
    control(screen, KeyCode::Enter);
}
fn assert_read_only_github_calls() {
    for args in calls() {
        assert!(
            matches!(
                args.first().map(String::as_str),
                Some("repo" | "issue" | "pr")
            ) && matches!(args.get(1).map(String::as_str), Some("view" | "list"))
                || args.first().map(String::as_str) == Some("pr")
                    && args.get(1).map(String::as_str) == Some("diff")
                || args.first().map(String::as_str) == Some("api")
                    && !args
                        .iter()
                        .any(|arg| arg == "--method" || arg.contains("mutation(")),
            "saved views must not mutate GitHub: {args:?}"
        );
    }
}
// Issue list hints prioritize reading; the tests below still exercise native
// save/delete/cycle forms directly instead of requiring their footer duplication.
fn assert_native_issue_view_controls(screen: &mut GitScreen) {
    for (width, height) in [(64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        // A fresh notice ("View saved locally: …") owns the footer until the next key.
        let notice = !frame.grid.row_text(height - 1).contains(" · ");
        for label in ["Enter", "open", "keys"] {
            assert!(
                notice || text.contains(label),
                "missing {label} at {width}x{height}:\n{text}"
            );
        }
        let (x, y, _) = frame
            .cursor
            .expect("saved views must retain a native list cursor");
        assert!(x < width && y < height - 2);
        assert!(cursor_record(&frame).contains('#'));
        let left = reader_left(width, height);
        if left > 0 {
            assert_eq!(
                frame.grid.get(left, 2).unwrap().symbol,
                "│",
                "list pane must stay fixed"
            );
        }
    }
}

#[test]
fn saved_github_views_round_trip_update_delete_and_restore_cached_identity() {
    if fixture_child("saved_github_views_round_trip_update_delete_and_restore_cached_identity") {
        return;
    }
    let root_string = std::env::var("DUST_GITHUB_ROOT").unwrap();
    let root = Path::new(&root_string);
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let query = "label:\"needs triage\" assignee:@me";
    let updated_query = "label:bug sort:updated-desc";
    let first = serde_json::json!({"number":7,"title":"First closed issue","body":"First saved-view body.","state":"CLOSED"});
    let second = serde_json::json!({"number":8,"title":"Second closed issue","body":"Second saved-view body.","state":"CLOSED"});
    let fixture = serde_json::json!({
        "calls":[], "comments":["saved view fixture ready"],
        "issues":[{"number":6,"title":"Open inbox","body":"Open inbox body."}, first, second],
        "issue_queries":{(query):[first,second],(updated_query):[first,second]}
    });
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    let mut screen = GitScreen::open(root).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "saved view fixture ready");
    assert_native_issue_view_controls(&mut screen);
    key(&mut screen, KeyCode::Char('S'));
    paste(&mut screen, "Triage 界");
    for (width, height) in [(64, 20), (100, 30)] {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.expect("save form needs a native input cursor");
        assert!(x < width && y < height);
        assert!(frame.grid.row_text(y).contains("Triage 界"));
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("View name")
                && text.contains("Search (GitHub query)")
                && text.contains("State")
        );
        assert!(
            text.contains("Save view") && text.contains("Cancel"),
            "{text}"
        );
    }
    key(&mut screen, KeyCode::Tab);
    paste(&mut screen, query);
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down); // closed
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("View saved locally"));
    wait(&mut screen, "First saved-view body.");
    assert_native_issue_view_controls(&mut screen);
    assert!(screen.frame(100, 30).grid.row_text(2).contains("Triage 界"));
    let saved = local_views(root);
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!(saved[0]["name"], "Triage 界");
    assert_eq!(saved[0]["query"], query);
    assert_eq!(saved[0]["state"], "closed");

    // Reusing the name updates, rather than appending a second preference.
    key(&mut screen, KeyCode::Char('S'));
    key(&mut screen, KeyCode::Tab);
    replace_field(&mut screen, updated_query);
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down); // all
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "First saved-view body.");
    let saved = local_views(root);
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!(saved[0]["query"], updated_query);
    assert_eq!(saved[0]["state"], "all");

    // Cycling is saved-only, wraps in both directions, and never steals PR tab keys.
    key(&mut screen, KeyCode::Char('S'));
    replace_field(&mut screen, "Inbox 🦀");
    key(&mut screen, KeyCode::Tab);
    replace_field(&mut screen, "");
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down); // all -> open
    control(&mut screen, KeyCode::Enter);
    assert_eq!(local_views(root).as_array().unwrap().len(), 2);
    key(&mut screen, KeyCode::Char(','));
    assert!(screen.frame(100, 30).grid.row_text(2).contains("Triage 界"));
    key(&mut screen, KeyCode::Char('.'));
    assert!(screen.frame(100, 30).grid.row_text(2).contains("Inbox 🦀"));
    key(&mut screen, KeyCode::Char('.'));
    assert!(screen.frame(100, 30).grid.row_text(2).contains("Triage 界"));
    key(&mut screen, KeyCode::Char(','));
    key(&mut screen, KeyCode::Char('D'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    assert_eq!(local_views(root).as_array().unwrap().len(), 1);

    // A new controller in a linked worktree must discover the same local preference.
    drop(screen);
    let linked = root.parent().unwrap().join("saved-view-worktree");
    git(
        root,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    let mut screen = GitScreen::open(&linked).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Open inbox body.");
    choose_view(&mut screen, "Saved: Triage 界");
    wait(&mut screen, "First saved-view body.");
    assert!(screen.frame(100, 30).grid.row_text(2).contains("Triage 界"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Second saved-view body.");
    let open_timelines_before = calls()
        .iter()
        .filter(|args| args.iter().any(|arg| arg.contains("issues/6/timeline")))
        .count();
    choose_view(&mut screen, "All open");
    wait(&mut screen, "Open inbox body.");
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if calls()
            .iter()
            .filter(|args| args.iter().any(|arg| arg.contains("issues/6/timeline")))
            .count()
            > open_timelines_before
        {
            break;
        }
        assert!(Instant::now() < end, "{}", view(&mut screen));
        std::thread::sleep(Duration::from_millis(10));
    }
    let timelines_before = calls()
        .iter()
        .filter(|args| args.iter().any(|arg| arg.contains("issues/8/timeline")))
        .count();
    choose_view(&mut screen, "Saved: Triage 界");
    let cached = screen.frame(100, 30);
    assert!(lists(&cursor_record(&cached), 8, "Second"));
    assert!(
        (0..30).any(|y| cached.grid.row_text(y).contains("Refreshing…")),
        "cached results must not imply refresh completed"
    );
    assert!((0..30).any(|y| cached.grid.row_text(y).contains("Second saved-view body.")));

    // Finish this refresh's last serialized request before changing fixture data.
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if calls()
            .iter()
            .filter(|args| args.iter().any(|arg| arg.contains("issues/8/timeline")))
            .count()
            > timelines_before
        {
            break;
        }
        assert!(Instant::now() < end, "{}", view(&mut screen));
        std::thread::sleep(Duration::from_millis(10));
    }
    // A refreshed insertion above that item must preserve its identity, too.
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["issue_queries"][updated_query] = serde_json::json!([
        {"number":5,"title":"Inserted above cache","body":"Inserted body.","state":"CLOSED"},
        first, second
    ]);
    let temporary = Path::new(&state_path).with_extension("input");
    std::fs::write(&temporary, fixture.to_string()).unwrap();
    std::fs::rename(&temporary, &state_path).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, "Inserted above cache");
    let refreshed = screen.frame(100, 30);
    assert!(lists(&cursor_record(&refreshed), 8, "Second"));

    key(&mut screen, KeyCode::Char('D'));
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Choose Delete locally to confirm"));
    assert_eq!(local_views(root).as_array().unwrap().len(), 1);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('D'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Escape); // Even a chosen confirmation is not a deletion.
    assert_eq!(local_views(root).as_array().unwrap().len(), 1);
    key(&mut screen, KeyCode::Char('D'));
    assert!(
        view(&mut screen).contains("Keep view"),
        "cancel must not restore a destructive confirmation"
    );
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("View deleted locally"));
    assert!(local_views(root).as_array().unwrap().is_empty());
    assert!(view(&mut screen).contains("Second saved-view body."));
    assert_read_only_github_calls();
}

#[test]
fn saved_github_views_isolate_host_repository_page_and_refuse_unsafe_local_writes() {
    if fixture_child(
        "saved_github_views_isolate_host_repository_page_and_refuse_unsafe_local_writes",
    ) {
        return;
    }
    let root_string = std::env::var("DUST_GITHUB_ROOT").unwrap();
    let root = Path::new(&root_string);
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    for (host, repository, page, query) in [
        ("github.com", "example/demo", 1, "label:issue"),
        ("github.com", "example/demo", 2, "review-requested:@me"),
        ("github.com", "other/demo", 1, "author:@me"),
        ("git.example.test", "example/demo", 1, "assignee:@me"),
    ] {
        let fixture = serde_json::json!({"calls":[],"comments":["scope fixture ready"],"host":host,"repository":repository});
        std::fs::write(&state_path, fixture.to_string()).unwrap();
        let mut screen = GitScreen::open(root).unwrap();
        goto(&mut screen, 0, page);
        // Both overviews end with the latest comment.
        wait(&mut screen, "scope fixture ready");
        // A similarly named view in another scope must not appear in this picker.
        key(&mut screen, KeyCode::Char('v'));
        // Pasting its name picks nothing here: the picker only offers this scope's views.
        replace_field(&mut screen, "Saved: Shared name 🦀");
        assert!(
            !view(&mut screen).contains("Saved: Shared name"),
            "{}",
            view(&mut screen)
        );
        key(&mut screen, KeyCode::Escape);
        key(&mut screen, KeyCode::Char('S'));
        paste(&mut screen, "Shared name 🦀");
        key(&mut screen, KeyCode::Tab);
        paste(&mut screen, query);
        control(&mut screen, KeyCode::Enter);
        assert!(view(&mut screen).contains("View saved locally"));
        assert!(
            !screen.busy(),
            "saving a view must not enqueue a GitHub write"
        );
        assert_read_only_github_calls();
    }
    let saved = local_views(root);
    assert_eq!(saved.as_array().unwrap().len(), 4);
    for query in [
        "label:issue",
        "review-requested:@me",
        "author:@me",
        "assignee:@me",
    ] {
        assert!(
            saved
                .as_array()
                .unwrap()
                .iter()
                .any(|view| view["query"] == query)
        );
    }

    // Canonical host/repository casing must reopen the original preference.
    std::fs::write(&state_path, serde_json::json!({"calls":[],"comments":[],"host":"GitHub.COM","repository":"Example/Demo"}).to_string()).unwrap();
    let mut screen = GitScreen::open(root).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "A real native issue layout.");
    choose_view(&mut screen, "Saved: Shared name 🦀");
    goto(&mut screen, 1, 2);
    wait(&mut screen, "On merge");
    choose_view(&mut screen, "Saved: Shared name 🦀");
    key(&mut screen, KeyCode::Char('S'));
    let pr_form = view(&mut screen);
    assert!(pr_form.contains("review-requested:@me") && !pr_form.contains("label:issue"));
    key(&mut screen, KeyCode::Escape);
    goto(&mut screen, 2, 1);
    key(&mut screen, KeyCode::Char('S'));
    assert!(view(&mut screen).contains("label:issue"));
    screen.write_refusal = Some("Untrusted workspace: local writes refused".into());
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("local writes refused"));
    assert_eq!(local_views(root), saved);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('D'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("local writes refused"));
    assert_eq!(local_views(root), saved);
    key(&mut screen, KeyCode::Escape);
    // Applying views is read-only even when the host refuses writes.
    choose_view(&mut screen, "All open");
    choose_view(&mut screen, "Saved: Shared name 🦀");
    assert!(
        screen
            .frame(100, 30)
            .grid
            .row_text(2)
            .contains("Shared name")
    );
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('S'));
    let repo = git2::Repository::open(root).unwrap();
    let mut config = repo
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    config
        .set_str("dusty-git.githubViews", "{malformed saved views}")
        .unwrap();
    control(&mut screen, KeyCode::Enter);
    assert!(
        view(&mut screen).contains("Couldn't read saved views · your saved views are unchanged")
    );
    assert_eq!(
        config.get_string("dusty-git.githubViews").unwrap(),
        "{malformed saved views}"
    );
    key(&mut screen, KeyCode::Escape);
    for code in ['v', 'S', 'D', '.', ','] {
        key(&mut screen, KeyCode::Char(code));
        assert!(
            view(&mut screen)
                .contains("Couldn't read saved views · your saved views are unchanged")
        );
        assert_eq!(
            config.get_string("dusty-git.githubViews").unwrap(),
            "{malformed saved views}"
        );
    }
    drop(screen);
    let mut screen = GitScreen::open(root).unwrap();
    goto(&mut screen, 0, 1);
    wait(
        &mut screen,
        "Couldn't read saved views · your saved views are unchanged",
    );
    assert_read_only_github_calls();
}

const DETAIL_SIZES: [(u16, u16); 3] = [(64, 20), (100, 30), (190, 48)];
fn detail_text(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn detail_pane_text(frame: &Frame) -> String {
    let left = drawn_reader_left(frame);
    (2..frame.grid.height() - 2)
        .map(|y| {
            (left + 1..frame.grid.width() - 1)
                .filter_map(|x| frame.grid.get(x, y))
                .filter(|cell| !cell.continuation)
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// The reader pane's left border beside the issue list: 44% in wide mode, 40% in medium
/// mode, at least 43 columns (`All 10 │ Bugs 4 │ Features 2 │ Other 4` keeps its counts at
/// 100, r10 R9-01); none below 100 columns, where the reader takes the screen.
fn reader_left(width: u16, height: u16) -> u16 {
    if width < 100 {
        0
    } else {
        let percent = if width >= 160 && height >= 40 { 44 } else { 40 };
        (width * percent / 100).max(43)
    }
}
/// The reader pane's left border as drawn: where the list's top-right corner meets the
/// reader's top-left one (each page sizes its own list, r10 R9-01); 0 with no list beside it.
fn drawn_reader_left(frame: &Frame) -> u16 {
    let symbol = |x: u16| {
        frame
            .grid
            .get(x, 1)
            .map_or(String::new(), |cell| cell.symbol.to_string())
    };
    (1..frame.grid.width())
        .find(|&x| symbol(x - 1) == "╮" && symbol(x) == "╭")
        .unwrap_or(0)
}
fn assert_detail_style(frame: &Frame, text: &str, expected: dust::config::Highlight) {
    let left = drawn_reader_left(frame);
    for y in 2..frame.grid.height() - 2 {
        for x in left + 1..frame.grid.width() - 1 {
            if text.chars().enumerate().all(|(offset, character)| {
                frame.grid.get(x + offset as u16, y).is_some_and(|cell| {
                    cell.symbol == character.to_string() && prose_style(cell.style, expected)
                })
            }) {
                return;
            }
        }
    }
    panic!(
        "missing styled prose {text:?} ({expected:?}):\n{}",
        detail_pane_text(frame)
    );
}
/// Whether a reader cell carries the semantic style of a Markdown highlight: headings and
/// list marks in the accent, links in the link colour, code on the code surface's hue,
/// quotes muted; fenced code keeps its syntax group. Any surface (the cursor row moves it).
fn prose_style(style: Style, expected: dust::config::Highlight) -> bool {
    use dust::config::Highlight;
    use dust::ui::{Emphasis, GitRole};
    let want = if expected == Highlight::HEADING {
        Some((GitRole::Focus, Emphasis::Bold))
    } else if expected == Highlight::KEYWORD {
        Some((GitRole::Focus, Emphasis::None))
    } else if expected == Highlight::QUOTE {
        Some((GitRole::Muted, Emphasis::None))
    } else if expected == Highlight::LINK {
        Some((GitRole::Link, Emphasis::None))
    } else if expected == Highlight::STRONG {
        Some((GitRole::Text, Emphasis::Bold))
    } else if expected == Highlight::ITALIC {
        Some((GitRole::Text, Emphasis::None))
    } else if expected == Highlight::RAW {
        Some((GitRole::Modified, Emphasis::None))
    } else {
        None
    };
    match (style, want) {
        (Style::Git { role, emphasis, .. }, Some(want)) => (role, emphasis) == want,
        (Style::GitSyntax { group, .. }, None) => group == expected,
        _ => false,
    }
}
fn find_detail(screen: &mut GitScreen, query: &str) {
    control(screen, KeyCode::Char('f'));
    assert!(view(screen).contains("Find detail"));
    replace_field(screen, query);
    key(screen, KeyCode::Enter);
    assert!(
        !view(screen).contains("Find detail"),
        "Enter applies the native find form"
    );
    assert!(!screen.busy(), "find must never authorize a write");
}
fn detail_cursor(frame: &Frame) -> (u16, u16, String) {
    let (x, y, _) = frame.cursor.expect("native detail cursor");
    assert!(!frame.grid.get(x, y).unwrap().continuation);
    let tail = (x..frame.grid.width())
        .filter_map(|at| frame.grid.get(at, y))
        .filter(|cell| !cell.continuation)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    (x, y, tail.trim_end().into())
}
// Issues and PRs use native full-width detail focus below 100 columns.
fn issue_detail_left(width: u16) -> u16 {
    reader_left(width, if width >= 160 { 48 } else { 30 })
}
fn issue_detail_matched(screen: &mut GitScreen, size: (u16, u16), expected: &str, count: &str) {
    detail_matched_at(screen, size, expected, count, issue_detail_left(size.0));
}
fn detail_matched(screen: &mut GitScreen, size: (u16, u16), expected: &str, count: &str) {
    // PR Files lays out its own file tree beside the diff, so only the match itself and
    // its count identify the reader cursor here.
    detail_matched_at(screen, size, expected, count, 0);
}
fn detail_matched_at(
    screen: &mut GitScreen,
    size: (u16, u16),
    expected: &str,
    count: &str,
    left: u16,
) {
    let frame = screen.frame(size.0, size.1);
    let (x, y, tail) = detail_cursor(&frame);
    assert!(
        x > left && y < size.1 - 2,
        "not a source/prose cursor:\n{}",
        detail_text(&frame)
    );
    assert_eq!(frame.grid.get(x, y).unwrap().style, Style::Search);
    assert!(
        tail.starts_with(expected),
        "expected {expected:?}, got {tail:?}:\n{}",
        detail_text(&frame)
    );
    // Issues and PRs report the position in the footer; the Actions reader in its own
    // status row.
    assert!(
        frame.grid.row_text(size.1 - 1).contains(count)
            || frame.grid.row_text(size.1 - 3).contains(count),
        "missing {count}:\n{}",
        detail_text(&frame)
    );
}
fn issue_detail_geometry(frame: &Frame) -> Vec<(u16, u16, String)> {
    detail_geometry_at(frame, issue_detail_left(frame.grid.width()))
}
fn detail_geometry(frame: &Frame) -> Vec<(u16, u16, String)> {
    detail_geometry_at(frame, issue_detail_left(frame.grid.width()))
}
fn detail_geometry_at(frame: &Frame, left: u16) -> Vec<(u16, u16, String)> {
    // In focused narrow issues left is the actual outer detail edge (0), not
    // the old divider now occupied by prose. Split modes still sample all three
    // original edges, preserving exact before/after frame geometry comparisons.
    let edges = if left == 0 {
        vec![0, frame.grid.width() - 1]
    } else {
        vec![0, left, frame.grid.width() - 1]
    };
    (1..frame.grid.height() - 2)
        .flat_map(|y| {
            edges
                .iter()
                .copied()
                .map(move |x| (x, y, frame.grid.get(x, y).unwrap().symbol.clone()))
        })
        .collect()
}
fn assert_find_read_only_calls() {
    for args in calls() {
        let read = matches!(
            args.first().map(String::as_str),
            Some("repo" | "issue" | "pr" | "run" | "workflow")
        ) && matches!(args.get(1).map(String::as_str), Some("view" | "list"))
            || is_call(&args, &["pr", "diff"])
            || args.first().map(String::as_str) == Some("api")
                && !args
                    .iter()
                    .any(|arg| arg == "--method" || arg.contains("mutation("));
        assert!(read, "find must not write to GitHub: {args:?}");
    }
}

#[test]
fn github_markdown_prose_styles_visible_find_and_raw_drafts_preserve_local_git() {
    if fixture_child("github_markdown_prose_styles_visible_find_and_raw_drafts_preserve_local_git")
    {
        return;
    }
    use dust::config::Highlight;
    let root = std::path::PathBuf::from(std::env::var("DUST_GITHUB_ROOT").unwrap());
    let local = root.join("local.txt");
    std::fs::write(&local, "staged content\n").unwrap();
    git(&root, &["add", "local.txt"]);
    std::fs::write(&local, "unstaged content\n").unwrap();
    let index = std::fs::read(root.join(".git/index")).unwrap();
    let raw = "### Readable heading\nSome **bold text**, _italic text_, `inline code` and [docs label](https://hidden.test/target).\n> quoted text\n- list item\n- [x] done item\n\n| Name | Value |\n|---|---|\n| row | `cell` |\n\n```rust\n\tlet literal = \"**code stays**\";\n```";
    std::fs::write(
        std::env::var("DUST_GITHUB_FIXTURE").unwrap(),
        serde_json::json!({
            "calls":[],"comments":["### Comment heading\nA **comment needle** with `styled runs`."],
            "issues":[{"number":7,"title":"Only identity","body":raw}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen = GitScreen::open(&root).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Comment heading");
    let frame = screen.frame(190, 48);
    let prose = detail_pane_text(&frame);
    // Headings render without their `#` marks, in the accent; find never matches the marks.
    for literal in [
        "### ",
        "**bold text**",
        "_italic text_",
        "`inline code`",
        "hidden.test",
        "```rust",
        "|---|",
    ] {
        assert!(
            !prose.contains(literal),
            "raw Markdown leaked: {literal}:\n{prose}"
        );
    }
    assert!(
        prose.contains("**code stays**"),
        "fenced text is literal: {prose}"
    );
    for (text, role) in [
        ("Readable heading", Highlight::HEADING),
        ("bold text", Highlight::STRONG),
        ("italic text", Highlight::ITALIC),
        ("inline code", Highlight::RAW),
        ("docs label", Highlight::LINK),
        ("quoted text", Highlight::QUOTE),
        ("• ", Highlight::KEYWORD),
        ("Name", Highlight::STRONG),
        ("cell", Highlight::RAW),
        ("Comment heading", Highlight::HEADING),
        ("comment needle", Highlight::STRONG),
        ("let", Highlight::named("@keyword.rust").unwrap()),
    ] {
        assert_detail_style(&frame, text, role);
    }
    // A done checklist item's box is green, like any finished state.
    let done = (2..frame.grid.height() - 2)
        .flat_map(|y| (0..frame.grid.width()).map(move |x| (x, y)))
        .filter_map(|(x, y)| frame.grid.get(x, y))
        .find(|cell| cell.symbol == "☑")
        .expect("done checklist box");
    assert!(
        matches!(
            done.style,
            Style::Git {
                role: dust::ui::GitRole::Added,
                ..
            }
        ),
        "{:?}",
        done.style
    );
    screen.write_refusal = Some("Read-only Markdown inspection".into());
    let reads = calls().len();
    for query in [
        "### Readable",
        "**bold text**",
        "`inline code`",
        "hidden.test/target",
        "Only identity",
        "Write a comment",
    ] {
        find_detail(&mut screen, query);
        let footer = screen.frame(100, 30).grid.row_text(29);
        assert!(footer.contains("no matches"), "{query}: {footer}");
    }
    for (query, expected) in [
        ("bold text, italic text, inline code", "bold text,"),
        ("**code stays**", "**code stays**"),
        (
            "comment needle with styled runs",
            "comment needle with styled runs",
        ),
    ] {
        key(&mut screen, KeyCode::Home);
        find_detail(&mut screen, query);
        for size in DETAIL_SIZES {
            issue_detail_matched(&mut screen, size, expected, "1/1");
        }
    }
    assert_eq!(
        calls().len(),
        reads,
        "Markdown and find use cached data only"
    );
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "**Unsent draft** `raw`");
    key(&mut screen, KeyCode::Escape);
    find_detail(&mut screen, "comment needle");
    key(&mut screen, KeyCode::Char('C'));
    key(&mut screen, KeyCode::Home);
    assert!(
        detail_cursor(&screen.frame(100, 30))
            .2
            .starts_with("**Unsent draft** `raw`")
    );
    key(&mut screen, KeyCode::Escape);
    assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
    assert_eq!(
        std::fs::read_to_string(local).unwrap(),
        "unstaged content\n"
    );
    assert_find_read_only_calls();
}

#[test]
fn github_markdown_reviews_and_threads_share_visible_find_but_patches_stay_literal() {
    if fixture_child(
        "github_markdown_reviews_and_threads_share_visible_find_but_patches_stay_literal",
    ) {
        return;
    }
    use dust::config::Highlight;
    std::fs::write(std::env::var("DUST_GITHUB_FIXTURE").unwrap(), serde_json::json!({
        "calls":[],"comments":["### Comment heading\nA **comment needle**."],
        "prs":[{"number":9,"title":"PR identity","body":"### Description heading\nA **description needle**.",
            "reviews":[{"author":{"login":"reviewer"},"state":"APPROVED","body":"### Review heading\nA **review needle**."}]}],
        "review_threads":{"example/demo#9":[{"id":"THREAD_1","isResolved":false,"comments":{"nodes":[
            {"author":{"login":"reviewer"},"body":"### Thread heading\nA **thread needle**.","path":"example.rs","line":1}
        ]}}]},
        "diff":"diff --git a/example.rs b/example.rs\n--- /dev/null\n+++ b/example.rs\n@@ -0,0 +1 @@\n+let raw = \"**patch literal**\";\n"
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    // The overview reads the description and the latest conversation.
    wait(&mut screen, "Review heading");
    wait(&mut screen, "Comment heading");
    let frame = screen.frame(190, 48);
    for kind in ["Description", "Comment", "Review"] {
        assert_detail_style(&frame, &format!("{kind} heading"), Highlight::HEADING);
        assert_detail_style(
            &frame,
            &format!("{} needle", kind.to_lowercase()),
            Highlight::STRONG,
        );
    }
    // The Conversation holds the review's threads too; they render Markdown the same way.
    control(&mut screen, KeyCode::Char('l'));
    wait(&mut screen, "Thread heading");
    let frame = screen.frame(190, 48);
    for kind in ["Comment", "Review", "Thread"] {
        assert_detail_style(&frame, &format!("{kind} heading"), Highlight::HEADING);
        assert_detail_style(
            &frame,
            &format!("{} needle", kind.to_lowercase()),
            Highlight::STRONG,
        );
    }
    screen.write_refusal = Some("Read-only reviews".into());
    for size in DETAIL_SIZES {
        key(&mut screen, KeyCode::Home);
        find_detail(&mut screen, "needle");
        // The description, the comment, the review and its thread: one Conversation.
        for index in 0..4 {
            detail_matched(&mut screen, size, "needle.", &format!("{}/4", index + 1));
            if index < 3 {
                key(&mut screen, KeyCode::Char('n'));
            }
        }
        key(&mut screen, KeyCode::Char('N'));
        detail_matched(&mut screen, size, "needle.", "3/4");
    }
    // Files changed (H wraps from Conversation): a find never leaks across tabs, and
    // patches stay literal.
    key(&mut screen, KeyCode::Char('H'));
    assert!(
        !screen
            .frame(100, 30)
            .grid
            .row_text(29)
            .contains("next match")
    );
    wait(&mut screen, "**patch literal**");
    find_detail(&mut screen, "**patch literal**");
    for size in DETAIL_SIZES {
        detail_matched(&mut screen, size, "**patch literal**", "1/1");
    }
    assert_find_read_only_calls();
}

/// Every word of the list pane at 100×30 is whole: the same word is somewhere in the wide
/// frame, or the row ends it with `…`. A status phrase is dropped or shortened whole
/// (`◔ 1 pending` never reads `1 pend`).
fn assert_list_words_whole(screen: &mut GitScreen) {
    let wide = view(screen);
    let words = wide
        .split_whitespace()
        .collect::<std::collections::HashSet<_>>();
    // The list pane's box at 100×30: from the left edge to its first `╮`, under the top bar
    // and above the footer.
    let frame = screen.frame(100, 30);
    let symbol = |x: u16, y: u16| {
        frame
            .grid
            .get(x, y)
            .map_or(" ", |cell| cell.symbol.as_str())
    };
    let right = (0..100)
        .find(|&x| symbol(x, 1) == "╮")
        .expect("the list pane's box");
    let narrow = (1..29)
        .map(|y| (0..=right).map(|x| symbol(x, y)).collect::<String>())
        .collect::<Vec<_>>();
    let rule = |word: &&str| word.chars().all(|c| ('\u{2500}'..='\u{259f}').contains(&c));
    for line in &narrow {
        for word in line
            .split(|c: char| c.is_whitespace() || c == '│')
            .filter(|word| !word.is_empty() && !rule(word))
        {
            assert!(
                word.ends_with('…') || words.contains(word),
                "{word:?} is cut in the 100-column list row {line:?}:\n{}\n---\n{wide}",
                narrow.join("\n")
            );
        }
    }
}

#[test]
fn list_rows_at_100_columns_never_cut_a_word() {
    if fixture_child("list_rows_at_100_columns_never_cut_a_word") {
        return;
    }
    // At 100×30 the subtitle's shortest rung, `#9 mayalee · ✓ 1 approval · 1 pending`, is three
    // cells wider than the list: unfitted, it read `· 1 pend`.
    std::fs::write(std::env::var("DUST_GITHUB_FIXTURE").unwrap(), serde_json::json!({
        "calls":[],
        "prs":[{"number":9,"title":"Resume queued tasks after a worker reconnects","author":{"login":"mayalee"},
            "reviewDecision":"REVIEW_REQUIRED","reviewRequests":["sam"],
            "reviews":[{"author":{"login":"lee"},"state":"APPROVED","body":"Looks good."}]}],
        "issues":[{"number":4,"title":"Queued tasks stall after a worker reconnects","author":{"login":"mayalee"}}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "Resume queued tasks");
    wait(&mut screen, "approval");
    assert_list_words_whole(&mut screen);
    goto(&mut screen, 2, 1);
    wait(&mut screen, "Queued tasks stall");
    assert_list_words_whole(&mut screen);
}

#[test]
fn files_changed_e_opens_the_line_under_the_cursor_in_the_editor() {
    if fixture_child("files_changed_e_opens_the_line_under_the_cursor_in_the_editor") {
        return;
    }
    use dust::git::screen::ScreenOutcome;
    std::fs::write(std::env::var("DUST_GITHUB_FIXTURE").unwrap(), serde_json::json!({
        "calls":[],"prs":[{"number":9,"title":"PR identity","body":"Body"}],
        "diff":"diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1,3 +1,3 @@\n fn before() {}\n-fn old() {}\n+fn after() {}\n fn tail() {}\n"
    }).to_string()).unwrap();
    let root = std::path::PathBuf::from(std::env::var("DUST_GITHUB_ROOT").unwrap());
    let mut screen = GitScreen::open(&root).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "PR identity");
    control(&mut screen, KeyCode::Char('l'));
    // Files changed: `H` wraps back from Conversation.
    key(&mut screen, KeyCode::Char('H'));
    wait(&mut screen, "fn after()");
    let e = |screen: &mut GitScreen| {
        screen.event(UiEvent::Key {
            key: KeyEvent::plain(KeyCode::Char('e')),
            kind: KeyInputKind::Press,
        })
    };
    let at = |line| ScreenOutcome::OpenFileAt(root.join("example.rs"), line);
    // An added line opens at its own line; a deleted one at the line that took its place;
    // a context line at its new-side line.
    focus_control(&mut screen, "fn after()");
    assert_eq!(e(&mut screen), at(2));
    focus_control(&mut screen, "fn old()");
    assert_eq!(e(&mut screen), at(2));
    focus_control(&mut screen, "fn tail()");
    assert_eq!(e(&mut screen), at(3));
    // `e` reads only: no write reached the forge.
    assert_find_read_only_calls();
}

#[test]
fn github_detail_find_prose_wrap_unicode_cancel_and_scoped_drafts() {
    if fixture_child("github_detail_find_prose_wrap_unicode_cancel_and_scoped_drafts") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let wrapped = format!("{}界", "abcdefghij".repeat(20));
    let filler = (0..55)
        .map(|n| format!("Paragraph line {n}.\n"))
        .collect::<String>();
    let body = format!(
        "**needle** one `needle` two.\n{filler}**needle** final.\nbefore **{wrapped}**\nunicode: **界 e\u{301} 👩‍💻**\n```rust\n\tlet literal = \"**fenced**\";\n```"
    );
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":["### Discussion **needle** reply."],
            "issues":[{"number":7,"title":"Identity only","body":body},
                      {"number":8,"title":"Other item","body":"Other item body."}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "needle one needle two.");
    control(&mut screen, KeyCode::Char('l'));
    // The overview ends with the latest comment once the timeline arrives; End follows it.
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        key(&mut screen, KeyCode::End);
        let shown = view(&mut screen);
        if shown.contains("Discussion needle reply.") {
            break;
        }
        assert!(Instant::now() < end, "missing the latest comment:\n{shown}");
        std::thread::sleep(Duration::from_millis(10));
    }
    screen.write_refusal = Some("Find is allowed in a read-only workspace".into());
    let reads = calls().len();
    for size in DETAIL_SIZES {
        key(&mut screen, KeyCode::Home);
        let geometry = issue_detail_geometry(&screen.frame(size.0, size.1));
        find_detail(&mut screen, "needle");
        issue_detail_matched(&mut screen, size, "needle one", "1/4");
        let first = detail_cursor(&screen.frame(size.0, size.1));
        key(&mut screen, KeyCode::Char('n'));
        issue_detail_matched(&mut screen, size, "needle two", "2/4");
        let second = detail_cursor(&screen.frame(size.0, size.1));
        assert_eq!(first.1, second.1);
        assert!(second.0 > first.0);
        key(&mut screen, KeyCode::Char('n'));
        issue_detail_matched(&mut screen, size, "needle final", "3/4");
        key(&mut screen, KeyCode::Char('n'));
        issue_detail_matched(&mut screen, size, "needle reply", "4/4");
        key(&mut screen, KeyCode::Char('n'));
        issue_detail_matched(&mut screen, size, "needle one", "1/4");
        key(&mut screen, KeyCode::Char('N'));
        issue_detail_matched(&mut screen, size, "needle reply", "4/4");
        assert_eq!(
            issue_detail_geometry(&screen.frame(size.0, size.1)),
            geometry
        );
    }
    let selected = detail_cursor(&screen.frame(100, 30));
    key(&mut screen, KeyCode::Char('/'));
    assert!(
        view(&mut screen).contains("Filter issues"),
        "/ remains the GitHub list query"
    );
    key(&mut screen, KeyCode::Escape);
    issue_detail_matched(&mut screen, (100, 30), "needle reply", "4/4");
    control(&mut screen, KeyCode::Char('f'));
    replace_field(&mut screen, "cancelled query must not become a draft");
    key(&mut screen, KeyCode::Escape);
    issue_detail_matched(&mut screen, (100, 30), "needle reply", "4/4");
    assert_eq!(detail_cursor(&screen.frame(100, 30)), selected);
    find_detail(&mut screen, "NEEDLE");
    let failed = screen.frame(100, 30);
    assert!(failed.grid.row_text(29).contains("no matches"));
    assert_eq!(
        detail_cursor(&failed),
        selected,
        "no-match must not move the cursor"
    );
    key(&mut screen, KeyCode::Char('N')); // No results still consumes n/N, never Create.
    assert!(!view(&mut screen).contains("Create issue"));
    key(&mut screen, KeyCode::Escape);
    assert!(!detail_text(&screen.frame(100, 30)).contains("Find /"));
    assert_eq!(detail_cursor(&screen.frame(100, 30)), selected);
    key(&mut screen, KeyCode::Escape);
    assert!(
        detail_cursor(&screen.frame(100, 30)).0 < 25,
        "second Escape returns to the list"
    );
    key(&mut screen, KeyCode::Char('n'));
    assert!(
        view(&mut screen).contains("Create issue"),
        "list n is not find"
    );
    key(&mut screen, KeyCode::Escape);
    for query in [
        "Identity only",
        "Write a comment",
        "[a-z].*",
        "**needle**",
        "`needle`",
        "```rust",
    ] {
        find_detail(&mut screen, query);
        assert!(detail_text(&screen.frame(100, 30)).contains("no matches"));
    }
    for (query, expected) in [
        ("e\u{301}", "e\u{301}"),
        ("\u{301}", "e\u{301}"),
        ("💻", "👩‍💻"),
    ] {
        key(&mut screen, KeyCode::Home);
        find_detail(&mut screen, query);
        for size in DETAIL_SIZES {
            issue_detail_matched(&mut screen, size, expected, "1/1");
        }
    }
    key(&mut screen, KeyCode::Home);
    find_detail(&mut screen, &wrapped);
    for size in DETAIL_SIZES {
        issue_detail_matched(&mut screen, size, "abcdefghij", "1/1");
        let frame = screen.frame(size.0, size.1);
        let (_, y, _) = detail_cursor(&frame);
        assert!(
            (y + 1..size.1 - 2).any(|row| (issue_detail_left(size.0) + 1..size.0 - 1)
                .any(|x| { frame.grid.get(x, row).unwrap().style == Style::Search })),
            "a match crossing a soft wrap highlights its continuation"
        );
    }
    for (query, expected) in [("let literal", "let literal"), ("**fenced**", "**fenced**")] {
        key(&mut screen, KeyCode::Home);
        find_detail(&mut screen, query);
        for size in DETAIL_SIZES {
            issue_detail_matched(&mut screen, size, expected, "1/1");
        }
    }
    let selected = detail_cursor(&screen.frame(100, 30));
    find_detail(&mut screen, "");
    assert!(!view(&mut screen).contains("Find /"));
    assert_eq!(
        detail_cursor(&screen.frame(100, 30)),
        selected,
        "empty query clears without leaving detail"
    );
    assert_eq!(
        calls().len(),
        reads,
        "find must use loaded data, not new API calls"
    );
    key(&mut screen, KeyCode::Home);
    find_detail(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('n'));
    let selected = detail_cursor(&screen.frame(100, 30));
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["issues"][0]["title"] = "Metadata only refresh".into();
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if detail_pane_text(&screen.frame(190, 48)).contains("#7 Metadata only refresh") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "metadata refresh did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    issue_detail_matched(&mut screen, (100, 30), "needle two", "2/4");
    assert_eq!(detail_cursor(&screen.frame(100, 30)), selected);
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "Scoped comment draft");
    key(&mut screen, KeyCode::Escape);
    find_detail(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Scoped comment draft"));
    assert!(!view(&mut screen).contains("cancelled query must not become a draft"));
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Other item body.");
    assert!(!view(&mut screen).contains("Find /"));
    control(&mut screen, KeyCode::Char('f'));
    let fresh = screen.frame(100, 30);
    let (_, y, _) = fresh.cursor.unwrap();
    assert!(
        !fresh.grid.row_text(y).contains("needle"),
        "new item has an empty find field:\n{}",
        fresh.grid.row_text(y)
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('C'));
    assert!(!view(&mut screen).contains("Scoped comment draft"));
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Up);
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Scoped comment draft"));
    key(&mut screen, KeyCode::Escape);
    assert_find_read_only_calls();
}

#[test]
fn github_detail_find_files_refresh_tabs_items_and_narrow_chooser() {
    if fixture_child("github_detail_find_files_refresh_tabs_items_and_narrow_chooser") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let filler = (0..55)
        .map(|n| format!(" context_{n:02}\n"))
        .collect::<String>();
    let diff = format!(
        "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1,58 +1,59 @@\n \tlet needle = \"needle\";\n{filler}-before\n+let emoji = \"👩‍💻 e\u{301} 界\";\n+{}needle end\ndiff --git a/src/nested/other.rs b/src/nested/other.rs\n--- /dev/null\n+++ b/src/nested/other.rs\n@@ -0,0 +1 @@\n+needle other_file\n",
        "界".repeat(160)
    );
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":[],"diff":diff,
            "prs":[{"number":9},{"number":10}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "into main from feature/native");
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, "let needle");
    control(&mut screen, KeyCode::Char('l'));
    // The thread sits under the line it discusses, at the top of this diff.
    key(&mut screen, KeyCode::Home);
    wait(&mut screen, "Please keep this draft.");
    let reads = calls().len();
    for size in DETAIL_SIZES {
        key(&mut screen, KeyCode::Home);
        let geometry = detail_geometry(&screen.frame(size.0, size.1));
        find_detail(&mut screen, "needle");
        detail_matched(&mut screen, size, "needle =", "1/3");
        key(&mut screen, KeyCode::Char('n'));
        detail_matched(&mut screen, size, "needle\"", "2/3");
        key(&mut screen, KeyCode::Char('n'));
        detail_matched(&mut screen, size, "needle end", "3/3");
        key(&mut screen, KeyCode::Char('n'));
        detail_matched(&mut screen, size, "needle =", "1/3");
        key(&mut screen, KeyCode::Char('N'));
        detail_matched(&mut screen, size, "needle end", "3/3");
        assert_eq!(detail_geometry(&screen.frame(size.0, size.1)), geometry);
    }
    // Wrapped (the default), the long line continues on rows with a blank gutter: the match
    // sits on a part with no line number. `w` pans the line instead, the number beside it.
    let code_before = |screen: &mut GitScreen| {
        let frame = screen.frame(100, 30);
        let (_, y, _) = detail_cursor(&frame);
        let row = frame.grid.row_text(y);
        let at = row.find("needle end").unwrap();
        let start = row[..at]
            .rfind('│')
            .map_or(0, |border| border + '│'.len_utf8());
        row[start..at].to_owned()
    };
    let wrapped = code_before(&mut screen);
    assert!(
        wrapped.contains('界') && !wrapped.chars().any(|c| c.is_ascii_digit()),
        "{wrapped:?}"
    );
    key(&mut screen, KeyCode::Char('w'));
    detail_matched(&mut screen, (100, 30), "needle end", "3/3");
    let panned = code_before(&mut screen);
    assert!(panned.contains("58"), "{panned:?}");
    assert!(view(&mut screen).contains("w wrap"));
    key(&mut screen, KeyCode::Char('w'));
    detail_matched(&mut screen, (100, 30), "needle end", "3/3");
    assert!(view(&mut screen).contains("w no wrap"));
    find_detail(&mut screen, "💻");
    for size in DETAIL_SIZES {
        detail_matched(&mut screen, size, "👩‍💻", "1/1");
    }
    for query in ["example.rs", "@@", "Mark viewed", "RIGHT"] {
        find_detail(&mut screen, query);
        assert!(
            view(&mut screen).contains("no matches"),
            "chrome/gutters are not source"
        );
    }
    assert_eq!(calls().len(), reads);
    key(&mut screen, KeyCode::Home);
    find_detail(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('N'));
    let selected = detail_cursor(&screen.frame(100, 30));
    let before = calls()
        .iter()
        .filter(|args| args.iter().any(|arg| arg.contains("reviewThreads")))
        .count();
    key(&mut screen, KeyCode::Char('R'));
    detail_matched(&mut screen, (100, 30), "needle end", "3/3");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        if calls()
            .iter()
            .filter(|args| args.iter().any(|arg| arg.contains("reviewThreads")))
            .count()
            > before
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unchanged source refresh did not reach metadata"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    detail_matched(&mut screen, (100, 30), "needle end", "3/3");
    assert_eq!(detail_cursor(&screen.frame(100, 30)), selected);
    key(&mut screen, KeyCode::Char('f'));
    key(&mut screen, KeyCode::Escape);
    detail_matched(&mut screen, (64, 20), "needle end", "3/3");
    key(&mut screen, KeyCode::Char('f'));
    key(&mut screen, KeyCode::Down);
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, "needle other_file");
    assert!(!view(&mut screen).contains("Find /"));
    find_detail(&mut screen, "needle");
    for size in DETAIL_SIZES {
        detail_matched(&mut screen, size, "needle other_file", "1/1");
    }
    key(&mut screen, KeyCode::Char('H')); // Checks cannot inherit Files matches.
    assert!(!view(&mut screen).contains("Find /"));
    key(&mut screen, KeyCode::Char('L'));
    assert!(!view(&mut screen).contains("Find /"));
    find_detail(&mut screen, "needle");
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down); // Identical diff text in another PR still resets.
    assert!(!view(&mut screen).contains("Find /"));
    control(&mut screen, KeyCode::Char('f'));
    let fresh = screen.frame(100, 30);
    let (_, y, _) = fresh.cursor.unwrap();
    assert!(
        !fresh.grid.row_text(y).contains("needle"),
        "another PR must not inherit the query:\n{}",
        fresh.grid.row_text(y)
    );
    key(&mut screen, KeyCode::Escape);
    assert_find_read_only_calls();
}

#[test]
fn github_detail_find_actions_logs_and_attempt_identity_are_native_and_read_only() {
    if fixture_child(
        "github_detail_find_actions_logs_and_attempt_identity_are_native_and_read_only",
    ) {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let prefix = "test (linux)\tUNKNOWN STEP\t2026-11-08T10:00:00.200Z ";
    let filler = (0..55)
        .map(|n| format!("{prefix}log line {n}\n"))
        .collect::<String>();
    let log = format!(
        "{prefix}needle one needle two\n{filler}{prefix}{}needle end\n{prefix}unicode: 👩‍💻 e\u{301} 界\n{prefix}**log literal** `raw`\n",
        "x".repeat(240)
    );
    std::fs::write(
        &state_path,
        serde_json::json!({"calls":[],"comments":[],"run_views":2,"log":log}).to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 3);
    wait(&mut screen, "Validate native workspace");
    wait(&mut screen, "attempt 2 of 2");
    // The header comes from the history row; the tree waits for the run's jobs.
    wait(&mut screen, "test (linux)");
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Char('f'));
    wait(&mut screen, "needle one needle two");
    screen.write_refusal = Some("Read-only logs".into());
    let reads = calls().len();
    let typed = |screen: &mut GitScreen, query: &str| {
        key(screen, KeyCode::Char('/'));
        for c in query.chars() {
            key(screen, KeyCode::Char(c));
        }
        key(screen, KeyCode::Enter);
    };
    // The cursor row (▸) and the find row's count, at every size.
    let matched = |screen: &mut GitScreen, expected: &str, count: &str| {
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let frame = screen.frame(width, height);
            let text = (0..height)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            let (_, y, _) = frame.cursor.expect("log cursor");
            assert!(
                frame.grid.row_text(y).contains(expected),
                "{expected} at {width}×{height}:\n{text}"
            );
            assert!(
                text.contains(&format!("{count} matches")),
                "{count} at {width}×{height}:\n{text}"
            );
        }
    };
    key(&mut screen, KeyCode::Home);
    typed(&mut screen, "needle");
    matched(&mut screen, "needle one", "1/3");
    key(&mut screen, KeyCode::Char('n'));
    matched(&mut screen, "needle two", "2/3");
    key(&mut screen, KeyCode::Char('N'));
    matched(&mut screen, "needle one", "1/3");
    key(&mut screen, KeyCode::Char('N'));
    matched(&mut screen, "needle end", "3/3");
    // The match sits 240 cells in. Wrapped (the default) the cursor rests on the part that
    // holds it; unwrapped (`w`) the log pans to it and marks the clipped start.
    key(&mut screen, KeyCode::Char('w'));
    let frame = screen.frame(100, 30);
    let (_, y, _) = frame.cursor.unwrap();
    assert!(
        frame.grid.row_text(y).contains('‹'),
        "{}",
        frame.grid.row_text(y)
    );
    key(&mut screen, KeyCode::Char('w'));
    key(&mut screen, KeyCode::Escape);
    typed(&mut screen, "💻");
    matched(&mut screen, "👩‍💻", "1/1");
    key(&mut screen, KeyCode::Escape);
    typed(&mut screen, "**log literal**");
    matched(&mut screen, "**log literal**", "1/1");
    for query in [
        "UNKNOWN STEP",
        "2026-11-08T10:00",
        "Re-run jobs",
        "test (linux)",
    ] {
        key(&mut screen, KeyCode::Escape);
        typed(&mut screen, query);
        assert!(view(&mut screen).contains("no matches"), "{query}");
    }
    assert_eq!(calls().len(), reads, "find reads nothing from the forge");
    // The step log is another projection: its find starts empty.
    key(&mut screen, KeyCode::Char('f'));
    assert!(
        !view(&mut screen).contains("no matches"),
        "changing log projection clears find"
    );
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "needle one needle two");
    typed(&mut screen, "needle");
    matched(&mut screen, "needle one", "1/3");
    // Identical text in another attempt is another log.
    key(&mut screen, KeyCode::Char('a'));
    wait(&mut screen, "Attempt 1");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "attempt 1 of 2");
    assert!(
        !view(&mut screen).contains("/3 matches"),
        "attempt identity changes even with identical log text"
    );
    wait(&mut screen, "test (linux)");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "needle one needle two");
    typed(&mut screen, "needle");
    matched(&mut screen, "needle one", "1/3");
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("/3 matches"));
    assert_find_read_only_calls();
}

fn batch_items(start: u64, count: u64, state: &str) -> Vec<serde_json::Value> {
    (start..start + count)
        .map(|id| {
            serde_json::json!({
                "number":id,"title":format!("Batch item {id}"),"state":state,
                "body":format!("Batch body {id}.\nneedle first needle second.")
            })
        })
        .collect()
}
/// Logical list loads: a GraphQL search's `--after` pages fold into the load they continue.
fn requested_list_limits(kind: &str, query: &str, state: &str) -> Vec<usize> {
    forge_fake::list_load_limits(&calls(), |args| {
        is_call(args, &[kind, "list"])
            && args
                .windows(2)
                .any(|pair| pair[0] == "--search" && pair[1] == query)
            && args
                .windows(2)
                .any(|pair| pair[0] == "--state" && pair[1] == state)
    })
}
/// A refresh keeps the rows on screen, so the count alone cannot tell it finished; wait
/// for the forge to have been asked.
fn wait_limits(screen: &mut GitScreen, kind: &str, query: &str, state: &str, expected: &[usize]) {
    let end = Instant::now() + Duration::from_secs(30);
    while requested_list_limits(kind, query, state) != expected {
        screen.poll();
        assert!(
            Instant::now() < end,
            "{:?} != {expected:?}",
            requested_list_limits(kind, query, state)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn wait_list_count(screen: &mut GitScreen, state: &str, count: usize) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let text = view(screen);
        // The list footer counts what is loaded; a fresh view shows "Loading…" until then,
        // and an empty one says so.
        let list = compact_list(screen);
        let shown = list
            .match_indices(&format!("Showing {count}"))
            .any(|(at, found)| !list[at + found.len()..].starts_with(|c: char| c.is_ascii_digit()));
        let _ = state;
        if shown
            && !text.contains("Refreshing…")
            && !text.contains("Loading more")
            && !text.contains("loading more")
        {
            return;
        }
        assert!(Instant::now() < end, "list count {count}:\n{text}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn github_load_more_preserves_latest_body_find_identity_and_drafts_on_failure_and_retry() {
    if fixture_child(
        "github_load_more_preserves_latest_body_find_identity_and_drafts_on_failure_and_retry",
    ) {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":["Batch activity ready"],"issues":batch_items(1, 205, "OPEN"),
        "fail_list_limits":{"200":1},
        "read_gates":[{"prefix":["issue","list","--state","open","--search","","--limit","200"],"name":"more-issues"}]
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Batch activity ready");
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Batch activity ready");
    assert!(view(&mut screen).contains("Batch body 2."));
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "Only item two owns this unsent draft");
    key(&mut screen, KeyCode::Escape);
    screen.write_refusal = Some("Read-only browsing is allowed".into());
    find_detail(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('n'));
    issue_detail_matched(&mut screen, (100, 30), "needle second", "2/2");
    let cursor = detail_cursor(&screen.frame(100, 30));
    let geometry = DETAIL_SIZES.map(|(w, h)| issue_detail_geometry(&screen.frame(w, h)));

    // The next list inserts above the selection. It must follow the immutable
    // item number rather than the old index, without replacing loaded metadata.
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["issues"]
        .as_array_mut()
        .unwrap()
        .insert(0, batch_items(4001, 1, "OPEN").remove(0));
    std::fs::write(&state_path, fixture.to_string()).unwrap();
    control(&mut screen, KeyCode::Char('n'));
    wait_call(&mut screen, |args| {
        is_call(args, &["issue", "list"])
            && args
                .windows(2)
                .any(|pair| pair[0] == "--limit" && pair[1] == "200")
    });
    for _ in 0..20 {
        control(&mut screen, KeyCode::Char('n'));
        screen.poll();
    }
    assert_eq!(requested_list_limits("issue", "", "open"), [100, 200]);
    assert!(detail_text(&screen.frame(190, 48)).contains("Batch body 2."));
    for (index, size) in DETAIL_SIZES.into_iter().enumerate() {
        issue_detail_matched(&mut screen, size, "needle second", "2/2");
        assert_eq!(
            issue_detail_geometry(&screen.frame(size.0, size.1)),
            geometry[index]
        );
    }
    control(&mut screen, KeyCode::Char('h'));
    assert!(view(&mut screen).contains("Loading more…"));
    assert_native_issue_view_controls(&mut screen);
    control(&mut screen, KeyCode::Char('l'));
    release_read("more-issues");
    wait(&mut screen, "Fixture list read refused");
    assert!(view(&mut screen).contains("· showing "));
    assert!(detail_text(&screen.frame(190, 48)).contains("Batch body 2."));
    assert_eq!(detail_cursor(&screen.frame(100, 30)), cursor);

    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 200);
    issue_detail_matched(&mut screen, (100, 30), "needle second", "2/2");
    assert_eq!(detail_cursor(&screen.frame(100, 30)), cursor);
    control(&mut screen, KeyCode::Char('h'));
    let selected = screen.frame(100, 30);
    assert!(lists(&cursor_record(&selected), 2, "Batch"));
    assert_eq!(requested_list_limits("issue", "", "open"), [100, 200, 200]);
    assert_eq!(
        calls()
            .iter()
            .filter(|args| is_call(args, &["issue", "view", "2"]))
            .count(),
        1
    );
    for _ in 0..99 {
        key(&mut screen, KeyCode::Down);
    }
    assert!(detail_text(&screen.frame(190, 48)).contains("Batch body 101."));
    wait(&mut screen, "Batch activity ready");
    key(&mut screen, KeyCode::Char('C'));
    assert!(!view(&mut screen).contains("Only item two owns"));
    paste(&mut screen, "Only item 101 owns this draft");
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 206);
    assert!(!view(&mut screen).contains("Ctrl+n Load more"));
    assert!(!compact_list(&mut screen).contains("^N more"));
    assert!(detail_text(&screen.frame(190, 48)).contains("Batch body 101."));
    for _ in 0..99 {
        key(&mut screen, KeyCode::Up);
    }
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Only item two owns this unsent draft"));
    assert!(!view(&mut screen).contains("Only item 101 owns"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        requested_list_limits("issue", "", "open"),
        [100, 200, 200, 300]
    );
    assert_read_only_github_calls();
}

#[test]
fn github_load_more_depth_is_scoped_to_queries_states_saved_views_and_pages_and_refresh() {
    if fixture_child(
        "github_load_more_depth_is_scoped_to_queries_states_saved_views_and_pages_and_refresh",
    ) {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    let mut assigned = batch_items(501, 155, "OPEN");
    assigned.extend(batch_items(801, 165, "CLOSED"));
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":[],"issues":batch_items(1, 205, "OPEN"),
            "prs":batch_items(2001, 125, "OPEN"),"issue_queries":{"author:@me":assigned}
        })
        .to_string(),
    )
    .unwrap();
    let root_string = std::env::var("DUST_GITHUB_ROOT").unwrap();
    let root = Path::new(&root_string);
    git2::Repository::open(root).unwrap().config().unwrap()
        .open_level(git2::ConfigLevel::Local).unwrap()
        .set_str("dusty-git.githubViews", &serde_json::json!([
            {"host":"github.com","repo":"example/demo","page":1,"name":"Closed assigned","query":"author:@me","state":"closed"}
        ]).to_string()).unwrap();
    let mut screen = GitScreen::open(root).unwrap();
    goto(&mut screen, 0, 1);
    wait_list_count(&mut screen, "open", 100);
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 200);
    key(&mut screen, KeyCode::Char('r'));
    wait_limits(&mut screen, "issue", "", "open", &[100, 200, 200]);
    wait_list_count(&mut screen, "open", 200);

    choose_view(&mut screen, "Created by me");
    wait_list_count(&mut screen, "open", 100);
    assert_eq!(requested_list_limits("issue", "author:@me", "open"), [100]);
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 155);
    choose_view(&mut screen, "Saved: Closed assigned");
    wait_list_count(&mut screen, "closed", 100);
    assert_eq!(
        requested_list_limits("issue", "author:@me", "closed"),
        [100]
    );
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "closed", 165);
    choose_view(&mut screen, "Created by me");
    wait_list_count(&mut screen, "open", 155);
    key(&mut screen, KeyCode::Char('r'));
    wait_limits(
        &mut screen,
        "issue",
        "author:@me",
        "open",
        &[100, 200, 200, 200],
    );
    wait_list_count(&mut screen, "open", 155);
    choose_view(&mut screen, "Saved: Closed assigned");
    wait_list_count(&mut screen, "closed", 165);
    // Choosing a view shows its cached depth at once and revalidates it in the background.
    wait_limits(
        &mut screen,
        "issue",
        "author:@me",
        "closed",
        &[100, 200, 200],
    );
    choose_view(&mut screen, "All open");
    wait_list_count(&mut screen, "open", 200);
    wait_limits(&mut screen, "issue", "", "open", &[100, 200, 200, 200]);
    assert_native_issue_view_controls(&mut screen);

    goto(&mut screen, 1, 2);
    wait_list_count(&mut screen, "open", 100);
    assert_eq!(requested_list_limits("pr", "", "open"), [100]);
    screen.write_refusal = Some("No remote writes are authorized".into());
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 125);
    assert!(!compact_list(&mut screen).contains("^N more"));
    // On pull requests `r` reviews; `R` refreshes.
    key(&mut screen, KeyCode::Char('R'));
    wait_limits(&mut screen, "pr", "", "open", &[100, 200, 200]);
    wait_list_count(&mut screen, "open", 125);
    goto(&mut screen, 2, 1);
    wait_list_count(&mut screen, "open", 200);
    assert_eq!(
        requested_list_limits("issue", "", "open"),
        [100, 200, 200, 200]
    );
    assert_read_only_github_calls();
}

#[test]
fn github_load_more_empty_short_and_exact_batches_are_truthful() {
    if fixture_child("github_load_more_empty_short_and_exact_batches_are_truthful") {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":[],"issues":[],
        "issue_queries":{"author:@me":batch_items(1, 99, "OPEN"),"assignee:@me":batch_items(101, 100, "OPEN")}
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_list_count(&mut screen, "open", 0);
    assert!(view(&mut screen).contains("No issues match this view"));
    control(&mut screen, KeyCode::Char('n'));
    assert_eq!(requested_list_limits("issue", "", "open"), [100]);
    assert!(!view(&mut screen).contains("^N more"));
    choose_view(&mut screen, "Created by me");
    wait_list_count(&mut screen, "open", 99);
    control(&mut screen, KeyCode::Char('n'));
    assert!(!compact_list(&mut screen).contains("^N more"));
    assert_eq!(requested_list_limits("issue", "author:@me", "open"), [100]);
    choose_view(&mut screen, "Assigned to me");
    wait_list_count(&mut screen, "open", 100);
    for (width, height) in DETAIL_SIZES {
        let frame = screen.frame(width, height);
        // The list's last row says more can be loaded.
        let footer = frame.grid.row_text(height - 3);
        assert!(footer.contains("^N more"), "{footer}");
        assert!(!footer.contains("100+"));
    }
    assert_native_issue_view_controls(&mut screen);
    control(&mut screen, KeyCode::Char('n'));
    wait_list_count(&mut screen, "open", 100);
    // The 200-deep load stops after its first GraphQL page: GitHub's pageInfo already says
    // nothing follows an exact batch, where `gh issue list --limit 200` had to ask.
    assert_eq!(
        requested_list_limits("issue", "assignee:@me", "open"),
        [100, 100]
    );
    assert!(!compact_list(&mut screen).contains("^N more"));
    assert!(!view(&mut screen).contains("Ctrl+n Load more"));
    for _ in 0..10 {
        control(&mut screen, KeyCode::Char('n'));
        screen.poll();
    }
    assert_eq!(
        requested_list_limits("issue", "assignee:@me", "open"),
        [100, 100]
    );
    assert_read_only_github_calls();
}

#[test]
fn github_load_more_ceiling_keeps_refresh_depth_and_invites_refining_the_filter() {
    if fixture_child("github_load_more_ceiling_keeps_refresh_depth_and_invites_refining_the_filter")
    {
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        serde_json::json!({
            "calls":[],"comments":[],"issues":batch_items(1, 1001, "OPEN")
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_list_count(&mut screen, "open", 100);
    for limit in (200..=1000).step_by(100) {
        control(&mut screen, KeyCode::Char('n'));
        for _ in 0..10 {
            control(&mut screen, KeyCode::Char('n'));
        }
        wait_list_count(&mut screen, "open", limit);
        assert!(detail_text(&screen.frame(190, 48)).contains("Batch body 1."));
    }
    assert_eq!(
        requested_list_limits("issue", "", "open"),
        (100..=1000).step_by(100).collect::<Vec<_>>()
    );
    for (width, height) in DETAIL_SIZES {
        let frame = screen.frame(width, height);
        let status = frame.grid.row_text(height - 3);
        assert!(
            status.contains("limit") && status.contains("/ refine"),
            "{status}"
        );
        assert!(!status.contains("^N"));
    }
    for _ in 0..10 {
        control(&mut screen, KeyCode::Char('n'));
        screen.poll();
    }
    assert_eq!(requested_list_limits("issue", "", "open").len(), 10);
    key(&mut screen, KeyCode::Char('r'));
    wait_list_count(&mut screen, "open", 1000);
    assert_eq!(
        requested_list_limits("issue", "", "open").last(),
        Some(&1000)
    );
    assert_eq!(requested_list_limits("issue", "", "open").len(), 11);
    assert_read_only_github_calls();
}

#[test]
fn github_list_edges_and_pages_are_native_and_read_only() {
    if fixture_child("github_list_edges_and_pages_are_native_and_read_only") {
        return;
    }
    std::fs::write(
        std::env::var("DUST_GITHUB_FIXTURE").unwrap(),
        serde_json::json!({
            "calls":[],"comments":[],"issues":batch_items(1, 99, "OPEN"),
            "issue_queries":{"":[],"author:@me":batch_items(1, 99, "OPEN")}
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 1);
    wait_list_count(&mut screen, "open", 0);
    for code in [
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageDown,
        KeyCode::PageUp,
    ] {
        key(&mut screen, code);
        assert!(view(&mut screen).contains("No issues match this view"));
    }
    choose_view(&mut screen, "Created by me");
    wait_list_count(&mut screen, "open", 99);
    for (width, height) in DETAIL_SIZES {
        screen.frame(width, height);
        for (code, number) in [
            (KeyCode::End, 99),
            (KeyCode::Home, 1),
            (KeyCode::PageDown, 21),
            (KeyCode::PageUp, 1),
        ] {
            key(&mut screen, code);
            wait(&mut screen, &format!("Batch body {number}."));
            let frame = screen.frame(width, height);
            let (x, y, _) = frame.cursor.expect("native selected item cursor");
            assert!(x < 24, "expected list focus");
            // The record's two rows: the title, then `#N opened …`.
            let field = [y, y + 1]
                .map(|row| {
                    (x..24)
                        .map(|column| frame.grid.get(column, row).unwrap().symbol.as_str())
                        .collect::<String>()
                })
                .join("\n");
            assert!(names(&field, number), "{width}x{height}: {field}");
        }
    }
    assert_read_only_github_calls();
}

/// WCAG relative luminance of an RGB theme colour.
fn theme_luminance(colour: dust::config::ThemeColor) -> Option<f64> {
    let dust::config::ThemeColor::Rgb(red, green, blue) = colour else {
        return None;
    };
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue))
}

#[test]
fn forge_screens_take_every_colour_from_the_theme() {
    if fixture_child("forge_screens_take_every_colour_from_the_theme") {
        return;
    }
    use dust::config::{ThemeColor, ThemeConfig};
    use dust::ui::{Emphasis, GitRole, GitSurface, Style};
    // Dust's designed dark palette: no other theme may show any of it (it once coloured every
    // dark theme's forge screens, whatever the theme).
    const DESIGNED: [(u8, u8, u8); 16] = [
        (0x15, 0x19, 0x23),
        (0x1b, 0x21, 0x30),
        (0x1f, 0x26, 0x36),
        (0xdc, 0xe4, 0xf2),
        (0x9a, 0xa8, 0xc1),
        (0x6b, 0xda, 0xd5),
        (0x7a, 0xa2, 0xf7),
        (0xa6, 0xda, 0x95),
        (0xf3, 0x8b, 0xa8),
        (0xee, 0xd4, 0x9f),
        (0xf5, 0xa9, 0x7f),
        (0xc6, 0xa0, 0xf6),
        (0xf0, 0xc6, 0x74),
        (0xb8, 0xc0, 0xe0),
        (0xed, 0x87, 0x96),
        (0x3b, 0x45, 0x5c),
    ];
    let themes: [(&str, &str); 5] = [
        (
            "catppuccin-latte",
            include_str!("../assets/themes/catppuccin-latte.toml"),
        ),
        (
            "tokyo-night-day",
            include_str!("../assets/themes/tokyo-night-day.toml"),
        ),
        (
            "rose-pine-dawn",
            include_str!("../assets/themes/rose-pine-dawn.toml"),
        ),
        (
            "gruvbox-dark",
            include_str!("../assets/themes/gruvbox-dark.toml"),
        ),
        ("nord", include_str!("../assets/themes/nord.toml")),
    ];
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(
        &state_path,
        r#"{"calls":[],"comments":["Theme audit discussion."]}"#,
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    // The issues list and reader (label chips, the composer), the close dialog over its dimmed
    // backdrop, and a pull request's Conversation with its sidebar and merge card.
    goto(&mut screen, 0, 1);
    wait(&mut screen, "A real native issue layout.");
    wait(&mut screen, "Add a comment");
    let mut frames = vec![("issue", screen.frame(120, 36))];
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::Char('x'));
    assert!(
        view(&mut screen).contains("Close issue"),
        "{}",
        view(&mut screen)
    );
    frames.push(("close dialog", screen.frame(120, 36)));
    key(&mut screen, KeyCode::Escape);
    goto(&mut screen, 1, 2);
    wait(&mut screen, "On merge");
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::Char('m'));
    frames.push(("pull request", screen.frame(160, 48)));
    for (name, source) in themes {
        let theme: ThemeConfig = toml::from_str(source).unwrap();
        let base = theme_luminance(theme.styles.default.background).unwrap();
        let light = base > 0.4;
        for (screen_name, frame) in &frames {
            for y in 0..frame.grid.height() {
                for x in 0..frame.grid.width() {
                    let cell = frame.grid.get(x, y).unwrap();
                    let Style::Git {
                        role,
                        surface,
                        emphasis,
                    } = cell.style
                    else {
                        continue;
                    };
                    let style = theme.git(role, surface, emphasis);
                    let at = format!(
                        "{name} {screen_name} ({x}, {y}) {:?} {role:?} on {surface:?}",
                        cell.symbol
                    );
                    for colour in [style.foreground, style.background] {
                        if let ThemeColor::Rgb(red, green, blue) = colour {
                            assert!(
                                !DESIGNED.contains(&(red, green, blue)),
                                "Dust's dark palette in {at}"
                            );
                        }
                    }
                    // The screen's ground matches the theme's own: no dark pane on a pale theme.
                    if matches!(
                        surface,
                        GitSurface::Canvas
                            | GitSurface::Pane
                            | GitSurface::TopBar
                            | GitSurface::Sheet
                    ) {
                        let ground = theme_luminance(style.background).unwrap();
                        assert_eq!(ground > 0.4, light, "ground {ground:.2} in {at}");
                    }
                    // Words stay readable: 3:1 everywhere, 4.5:1 on a forge label's chip.
                    if cell.symbol.trim().is_empty()
                        || matches!(role, GitRole::Border | GitRole::Lane(_))
                        || emphasis == Emphasis::Dim
                    {
                        continue;
                    }
                    let (fore, back) = (
                        theme_luminance(style.foreground).unwrap(),
                        theme_luminance(style.background).unwrap(),
                    );
                    let ratio = (fore.max(back) + 0.05) / (fore.min(back) + 0.05);
                    let minimum = if matches!(role, GitRole::Label(..)) {
                        4.5
                    } else {
                        3.0
                    };
                    assert!(ratio >= minimum, "contrast {ratio:.2} in {at}");
                }
            }
        }
    }
}

#[test]
fn enter_on_a_blocked_merge_box_moves_to_the_first_reason_without_a_message() {
    if fixture_child("enter_on_a_blocked_merge_box_moves_to_the_first_reason_without_a_message") {
        return;
    }
    std::fs::write(
        std::env::var("DUST_GITHUB_FIXTURE").unwrap(),
        serde_json::json!({
            "calls":[], "prs":[{"number":9,"reviewDecision":"REVIEW_REQUIRED"}]
        })
        .to_string(),
    )
    .unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "On merge");
    wait(&mut screen, "Merging is blocked");
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::Char('G'));
    key(&mut screen, KeyCode::Enter);
    let frame = screen.frame(190, 65);
    let (x, y, _) = frame.cursor.expect("the reader keeps a cursor");
    let text = (x..190)
        .filter_map(|column| frame.grid.get(column, y))
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(
        text.contains("1."),
        "cursor did not move to the first reason: {text}"
    );
    assert!(
        !view(&mut screen).contains("Merging is blocked ·"),
        "{}",
        view(&mut screen)
    );
}

#[test]
fn j_reaches_the_merge_box_past_a_row_that_shares_the_sidebars_link() {
    if fixture_child("j_reaches_the_merge_box_past_a_row_that_shares_the_sidebars_link") {
        return;
    }
    // The timeline mentions #7, which the sidebar's Development also links: two rows with one
    // action. The cursor must keep the row it is on, not snap back to the first (build 81).
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, serde_json::json!({
        "calls":[],"comments":["Discussion after the mention."],
        "timelines":{"9":[
            {"event":"labeled","actor":{"login":"alice"},"label":{"name":"bug"},"created_at":"2026-11-08T10:00:00Z"},
            {"event":"cross-referenced","actor":{"login":"alice"},"created_at":"2026-11-08T10:05:00Z",
             "source":{"type":"issue","issue":{"number":7,"title":"Preserve keyboard drafts","state":"open",
                "html_url":"https://github.com/example/demo/issues/7","user":{"login":"alice"},
                "repository":{"full_name":"example/demo","html_url":"https://github.com/example/demo"}}}},
            {"event":"commented","user":{"login":"rowan"},"body":"A comment below the mention.",
             "created_at":"2026-11-08T10:10:00Z","node_id":"C_BELOW"}
        ]}
    }).to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    goto(&mut screen, 0, 2);
    wait(&mut screen, "On merge");
    wait(&mut screen, "A comment below the mention.");
    control(&mut screen, KeyCode::Char('l'));
    // The size view() renders at, so every frame lays the page out the same way.
    let (width, height) = (190, 65);
    let cursor_row = |screen: &mut GitScreen| {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.expect("the reader keeps a cursor");
        let text = (x..width)
            .filter_map(|column| frame.grid.get(column, y))
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        (y, text)
    };
    let mut seen = Vec::new();
    let mut reached = false;
    for _ in 0..120 {
        let (_, text) = cursor_row(&mut screen);
        if text.contains("Merge") {
            reached = true;
            break;
        }
        seen.push(text.trim().to_owned());
        key(&mut screen, KeyCode::Char('j'));
    }
    assert!(
        reached,
        "j never reached the merge box; stops: {seen:#?}\n{}",
        view(&mut screen)
    );
    // The mention of #7 in the timeline was passed once, and Development's #7 once: j never
    // went back up the page.
    let development = seen
        .iter()
        .position(|stop| stop.contains("#7"))
        .expect("Development links #7");
    assert!(
        seen[development + 1..]
            .iter()
            .filter(|stop| stop.contains("#7"))
            .count()
            <= 1,
        "j looped back to #7: {seen:#?}"
    );
    // G lands on the very last row: j there moves nothing.
    key(&mut screen, KeyCode::Char('G'));
    let at_end = cursor_row(&mut screen);
    let before = view(&mut screen);
    key(&mut screen, KeyCode::Char('j'));
    assert_eq!(
        cursor_row(&mut screen),
        at_end,
        "G stops short of the last row:\n{before}"
    );
    assert_eq!(
        view(&mut screen),
        before,
        "j after G still moved:\n{before}"
    );
}
