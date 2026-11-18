#![cfg(unix)]
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::GitScreen;
use dusty::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[path = "support/forge_fake.rs"]
mod forge_fake;

fn key(screen: &mut GitScreen, code: KeyCode, control: bool) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code,
            modifiers: Modifiers {
                control,
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
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}
/// The fake forge server's request log in the gh-CLI form the legacy fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(Path::new(&std::env::var("DUSTY_GITHUB_FIXTURE").unwrap()))
}
fn creates() -> usize {
    calls()
        .iter()
        .filter(|args| args.contains(&"repos/team/demo/pulls".into()))
        .count()
}
fn open(screen: &mut GitScreen) {
    key(screen, KeyCode::Char('n'), false);
    wait(screen, "Create pull request");
}
fn review(screen: &mut GitScreen) {
    key(screen, KeyCode::Enter, true);
    wait(screen, "Review draft pull request");
}

#[test]
fn fork_submission_reviews_exact_targets_and_recovers_after_push() {
    if std::env::var_os("DUSTY_FORK_CHILD").is_none() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let bare = owner.path().join("fork.git");
        std::fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare"]);
        git(&root, &["init", "-b", "feature/fork"]);
        git(&root, &["config", "user.name", "Fork Test"]);
        git(&root, &["config", "user.email", "fork@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-m", "Fork change"]);
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/team/demo-fork.git",
            ],
        );
        git(
            &root,
            &["remote", "add", "upstream", "git@github.com:team/demo.git"],
        );
        let bin = owner.path().join("bin");
        let state = owner.path().join("github.json");
        let server = forge_fake::ForgeServer::start(&state, "legacy");
        server.install_gh(&bin);
        let git_executable = dusty::tools::discover_executable("git").unwrap();
        let wrapper = bin.join("git");
        // The real push writes only to our disposable bare repository; all other Git calls are real.
        forge_fake::write_executable(&wrapper, b"#!/usr/bin/env python3\nimport os,sys\na=sys.argv[1:]\nif a[:3]==['push','--','https://github.com/team/demo-fork.git']:\n a[2]=os.environ['DUSTY_FORK_BARE']\nos.execv(os.environ['DUSTY_REAL_GIT'],['git']+a)\n");
        std::fs::write(&state, serde_json::json!({"calls":[],"comments":[],"fail_create":1,"repo_info":{
            "team/demo-fork":{"full_name":"team/demo-fork","default_branch":"fork-default","fork":true,"parent":{"full_name":"team/demo","default_branch":"trunk"}}
        }}).to_string()).unwrap();
        let output = server
            .configure(&mut Command::new(std::env::current_exe().unwrap()))
            .args([
                "--exact",
                "fork_submission_reviews_exact_targets_and_recovers_after_push",
                "--nocapture",
            ])
            .env("DUSTY_FORK_CHILD", "1")
            .env("DUSTY_GITHUB_FIXTURE", &state)
            .env("DUSTY_GITHUB_ROOT", &root)
            .env("DUSTY_FORK_BARE", &bare)
            .env("DUSTY_REAL_GIT", git_executable)
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
    let root = std::env::var("DUSTY_GITHUB_ROOT").unwrap();
    let root = Path::new(&root);
    let bare = std::env::var("DUSTY_FORK_BARE").unwrap();
    let bare = Path::new(&bare);
    let mut screen = GitScreen::open(root).unwrap();
    // Pull requests is the tab right after Git.
    key(&mut screen, KeyCode::Char('>'), false);
    wait(&mut screen, "Native review");
    open(&mut screen);
    let form = view(&mut screen);
    assert!(
        form.contains("team/demo-fork") && form.contains("trunk") && form.contains("feature/fork"),
        "{form}"
    );
    // A fork: both repositories show; the unpushed branch is pushed first unless unticked.
    assert!(
        form.contains("Head repository") && form.contains("☑ Push to origin first"),
        "{form}"
    );
    // Each side of a fork names its owner.
    assert!(
        form.contains("team/demo-fork:feature/fork → team/demo:trunk"),
        "{form}"
    );
    paste(&mut screen, "A fork contribution");
    key(&mut screen, KeyCode::Tab, false);
    paste(&mut screen, "Preserve this résumé.\nAnd this second line.");
    // Description → base repository, base branch, head repository, push, draft.
    for _ in 0..5 {
        key(&mut screen, KeyCode::Tab, false);
    }
    key(&mut screen, KeyCode::Char(' '), false);
    assert!(
        view(&mut screen).contains("☑ Open as draft"),
        "{}",
        view(&mut screen)
    );
    review(&mut screen);
    assert!(view(&mut screen).contains("github.com/team/demo : trunk"));
    assert!(view(&mut screen).contains("github.com/team/demo-fork : feature/fork"));
    paste(&mut screen, "do not replace the reviewed source");
    assert!(!view(&mut screen).contains("do not replace"));
    assert_eq!(creates(), 0, "review is not submission");
    key(&mut screen, KeyCode::Escape, false);
    assert!(view(&mut screen).contains("Preserve this résumé."));
    review(&mut screen);
    assert!(view(&mut screen).contains("no force push"));
    // Reject a branch that moved after the explicit review.
    git(root, &["commit", "--allow-empty", "-m", "Newer local work"]);
    key(&mut screen, KeyCode::Enter, true);
    wait(&mut screen, "local branch changed");
    assert_eq!(creates(), 0);
    assert!(git(bare, &["for-each-ref", "refs/heads/"]).is_empty());
    open(&mut screen);
    review(&mut screen);
    // Read-only summary remains navigable and keeps its cursor at compact sizes.
    for (width, height) in [(100, 30), (180, 45)] {
        let frame = screen.frame(width, height);
        assert!(frame.cursor.is_some());
    }
    key(&mut screen, KeyCode::Enter, true);
    wait(
        &mut screen,
        "Branch pushed, but the pull request wasn't created · your draft is kept",
    );
    assert!(
        view(&mut screen)
            .contains("Branch pushed, but the pull request wasn't created · your draft is kept")
    );
    assert_eq!(
        git(bare, &["rev-parse", "refs/heads/feature/fork"]),
        git(root, &["rev-parse", "HEAD"])
    );
    assert_eq!(creates(), 1);
    open(&mut screen);
    assert!(view(&mut screen).contains("A fork contribution"));
    assert!(view(&mut screen).contains("And this second line."));
    // Already pushed: untick the push and only retry the PR submission.
    assert!(
        view(&mut screen).contains("☑ Push to origin first"),
        "{}",
        view(&mut screen)
    );
    for _ in 0..5 {
        key(&mut screen, KeyCode::Tab, false);
    }
    key(&mut screen, KeyCode::Char(' '), false);
    assert!(
        view(&mut screen).contains("☐ Push to origin first"),
        "{}",
        view(&mut screen)
    );
    review(&mut screen);
    key(&mut screen, KeyCode::Enter, true);
    wait(&mut screen, "https://github.com/team/demo/pull/9");
    assert!(view(&mut screen).contains("github.com/team/demo"));
    assert_eq!(creates(), 2);
    let calls = calls();
    let request = calls
        .iter()
        .rev()
        .find(|args| args.contains(&"repos/team/demo/pulls".into()))
        .unwrap();
    for field in [
        "head=team:feature/fork",
        "head_repo=demo-fork",
        "base=trunk",
        "draft=true",
        "title=A fork contribution",
        "body=Preserve this résumé.\nAnd this second line.",
    ] {
        assert!(
            request.contains(&field.to_owned()),
            "missing {field}: {request:?}"
        );
    }
}
