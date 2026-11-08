#![cfg(unix)]
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

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
    let frame = screen.frame(190, 65);
    (0..65)
        .map(|y| frame.grid.row_text(y))
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
fn activate(screen: &mut GitScreen, label: &str) {
    control(screen, KeyCode::Char('l'));
    for _ in 0..80 {
        let frame = screen.frame(190, 65);
        if let Some((x, y, _)) = frame.cursor {
            let tail = (x..190)
                .filter_map(|column| frame.grid.get(column, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>();
            if let Some(at) = tail.find(label)
                && tail[..at]
                    .chars()
                    .all(|c| c.is_whitespace() || c.is_ascii_digit() || "[]+-│▾▸✓○●×⑂".contains(c))
            {
                key(screen, KeyCode::Enter);
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
fn calls() -> Vec<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("DUST_GITHUB_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap();
    serde_json::from_value(value["calls"].clone()).unwrap()
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

#[test]
fn native_github_controls_forms_reviews_and_workflow_logs() {
    if std::env::var_os("DUST_GITHUB_CHILD").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let owner = tempfile::tempdir().unwrap();
        let bin = owner.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, include_bytes!("fixtures/github_cli.py")).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Screen Test"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-m", "Initial"]);
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_github_controls_forms_reviews_and_workflow_logs",
                "--nocapture",
            ])
            .env("DUST_GITHUB_CHILD", "1")
            .env("DUST_GITHUB_FIXTURE", owner.path().join("github.json"))
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
        return;
    }
    let state_path = std::env::var("DUST_GITHUB_FIXTURE").unwrap();
    std::fs::write(&state_path, r#"{"calls":[],"comments":[],"empty":true}"#).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_GITHUB_ROOT").unwrap())).unwrap();
    key(&mut screen, KeyCode::Char(']'));
    wait(&mut screen, "No issues match this view");
    for (width, height) in [(100, 30), (180, 45)] {
        let frame = screen.frame(width, height);
        let (_, y, _) = frame
            .cursor
            .expect("empty list must keep a terminal cursor");
        assert!(frame.grid.row_text(y).contains("No entries"));
    }
    let mut fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    fixture["empty"] = false.into();
    std::fs::write(&state_path, serde_json::to_string(&fixture).unwrap()).unwrap();
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, "A real native issue layout.");
    assert!(view(&mut screen).contains("github.com/example/demo"));
    assert!(view(&mut screen).contains("Edit Assignees"));
    key(&mut screen, KeyCode::Char('c'));
    paste(&mut screen, "First line\nq[] keep this draft");
    key(&mut screen, KeyCode::Up);
    key(&mut screen, KeyCode::Home);
    paste(&mut screen, "界e\u{0301} ");
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::End);
    key(&mut screen, KeyCode::Char('X'));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('c'));
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
    wait(&mut screen, "tester · commented");
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
    activate(&mut screen, "Edit Labels");
    key(&mut screen, KeyCode::End);
    paste(&mut screen, ",native");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.contains(&"--add-label".into()) && args.contains(&"native".into()))
    );
    wait(&mut screen, "Create linked branch");
    activate(&mut screen, "Create linked branch");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["issue".into(), "develop".into()])
                && args.contains(&"issue-7".into()))
    );
    key(&mut screen, KeyCode::Char(']'));
    wait(&mut screen, "feature/native → main");
    key(&mut screen, KeyCode::Char('n'));
    paste(&mut screen, "New draft PR");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(
        calls()
            .iter()
            .any(|args| args.starts_with(&["pr".into(), "create".into()])
                && args.contains(&"--head".into())
                && args.contains(&"--draft".into()))
    );
    wait(&mut screen, "feature/native → main");
    key(&mut screen, KeyCode::Char('L'));
    activate(&mut screen, "aaaaaaaa Keep drafts");
    wait(&mut screen, "+fn after()");
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, "+fn after()");
    activate(&mut screen, "+fn after()");
    paste(&mut screen, "Inline comment");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    assert!(calls().iter().any(
        |args| args.contains(&"repos/example/demo/pulls/9/comments".into())
            && args.contains(&"line=1".into())
            && args.contains(&"side=RIGHT".into())
    ));
    wait(&mut screen, "Review changes");
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
    wait(&mut screen, "Resolve conversation");
    assert!(!view(&mut screen).contains("Merge pull request"));
    activate(&mut screen, "Resolve conversation");
    control(&mut screen, KeyCode::Enter);
    idle(&mut screen);
    wait(&mut screen, "Merge pull request");
    activate(&mut screen, "Merge pull request");
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
    key(&mut screen, KeyCode::Char(']'));
    wait(&mut screen, "Native CI");
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, "Declared job dependencies");
    assert!(view(&mut screen).contains("test ← build"));
    activate(&mut screen, "Run workflow");
    assert!(view(&mut screen).contains("dry_run"));
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
    wait(&mut screen, "Validate native");
    control(&mut screen, KeyCode::Char('h'));
    control(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, "test (windows)");
    activate(&mut screen, "test (linux)");
    activate(&mut screen, "Check · success");
    wait(&mut screen, "Job running");
    assert!(!calls().iter().any(|args| args.contains(&"--log".into())));
    wait(&mut screen, "all native checks passed");
    assert!(view(&mut screen).contains("Logs matched by step timestamps"));
    assert!(!view(&mut screen).contains("[32m"));
    let frame = screen.frame(190, 65);
    let (_, row, _) = frame.cursor.unwrap();
    assert!(
        frame.grid.row_text(row).contains("Check · success"),
        "loading logs must preserve the selected step"
    );
    activate(&mut screen, "Full job log");
    wait(&mut screen, "Current runner version");
    assert!(!view(&mut screen).contains("UNKNOWN STEP"));
    activate(&mut screen, "Attempt 1");
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
