#![cfg(unix)]
//! Multi-repository inbox acceptance tests. Each test re-execs against a private fake forge
//! server and fake gh, with GH_HOST, Git config and an unborn local repository; no network or
//! checkout.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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
fn paste(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn shown(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 65);
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn state_path() -> PathBuf {
    std::env::var_os("DUST_INBOX_STATE").unwrap().into()
}
fn state() -> Value {
    serde_json::from_slice(&std::fs::read(state_path()).unwrap()).unwrap()
}
/// The fake forge server's request log in the gh-CLI form the old inbox fixture logged.
fn calls() -> Vec<Vec<String>> {
    forge_fake::calls(&state_path())
}
/// Each logged request with its gh-CLI form and the forge host it named.
fn hosted_calls() -> Vec<(Vec<String>, Value)> {
    forge_fake::requests(&state_path())
        .into_iter()
        .filter_map(|request| {
            Some((
                serde_json::from_value(request["call"].clone()).ok()?,
                request["host"].clone(),
            ))
        })
        .collect()
}
fn wait(screen: &mut GitScreen, predicate: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let text = shown(screen);
        if predicate(&text) {
            return;
        }
        assert!(Instant::now() < deadline, "timeout:\n{text}\n{:?}", calls());
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn fixture_child(name: &str, host: &str, gh_host: &str) -> bool {
    fixture_child_with_root(name, host, gh_host, false)
}
fn fixture_child_with_root(name: &str, host: &str, gh_host: &str, committed_dirty: bool) -> bool {
    if std::env::var_os("DUST_INBOX_CHILD").is_some() {
        return false;
    }
    let owner = tempfile::tempdir().unwrap();
    let bin = owner.path().join("bin");
    let state = owner.path().join("state.json");
    let server = forge_fake::ForgeServer::start(&state, "inbox");
    server.install_gh(&bin);
    let root = owner.path().join("repository");
    let repository = git2::Repository::init(&root).unwrap();
    // The local repository's forge target comes from its remote, as gh's did.
    repository
        .remote("origin", &format!("https://{host}/local/root.git"))
        .unwrap();
    if committed_dirty {
        std::fs::write(root.join("tracked.txt"), "committed source\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Inbox Test", "inbox@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        std::fs::write(root.join("tracked.txt"), "staged source\n").unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        std::fs::write(root.join("tracked.txt"), "dirty working source\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "untracked source\n").unwrap();
    }
    std::fs::write(root.join("untouched.txt"), "exact local source\n").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    server.configure(&mut command);
    command
        .args(["--exact", name, "--nocapture"])
        .env("DUST_INBOX_CHILD", "1")
        .env("DUST_INBOX_ROOT", &root)
        .env("DUST_INBOX_HOST", host)
        .env("DUST_INBOX_STATE", &state)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
    if gh_host.is_empty() {
        command.env_remove("GH_HOST");
    } else {
        command.env("GH_HOST", gh_host);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
fn open(initial: Value) -> GitScreen {
    std::fs::write(state_path(), initial.to_string()).unwrap();
    let mut screen =
        GitScreen::open(Path::new(&std::env::var("DUST_INBOX_ROOT").unwrap())).unwrap();
    // Top tabs run Git → Pull requests → Issues → Actions; visit Issues, then back to PRs.
    key(&mut screen, KeyCode::Char('>'));
    key(&mut screen, KeyCode::Char('>'));
    wait(&mut screen, |text| {
        text.contains("Canonical source for local/root issue 7")
    });
    key(&mut screen, KeyCode::Char('<'));
    wait(&mut screen, |text| {
        text.contains("Canonical source for local/root pr 7")
    });
    screen
}
fn assert_local_source_unchanged() {
    let root = PathBuf::from(std::env::var_os("DUST_INBOX_ROOT").unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join("untouched.txt")).unwrap(),
        "exact local source\n"
    );
    let repo = git2::Repository::open(root).unwrap();
    assert!(
        repo.head().is_err(),
        "selecting a remote item must not create/switch a branch"
    );
    assert_eq!(repo.index().unwrap().len(), 0);
}

#[test]
fn inbox_same_number_stale_read_drafts_scoped_write_files_and_saved_sections() {
    let name = "inbox_same_number_stale_read_drafts_scoped_write_files_and_saved_sections";
    if fixture_child(name, "github.com", "github.com") {
        return;
    }
    let mut screen = open(json!({"calls":[],"host":"github.com","gates":[
        {"prefix":["pr","view","7"],"repo":"alpha/one","name":"alpha"}
    ]}));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| {
        text.contains("Inbox")
            && calls().iter().any(|args| {
                args.starts_with(&["pr".into(), "view".into(), "7".into()])
                    && args.iter().any(|arg| arg == "github.com/alpha/one")
            })
    });
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "alpha draft untouched 界");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('C'));
    assert!(!shown(&mut screen).contains("alpha draft untouched"));
    paste(&mut screen, "only beta receives this comment");
    control(&mut screen, KeyCode::Enter);
    std::fs::write(format!("{}.alpha", state_path().display()), "release").unwrap();
    wait(&mut screen, |text| {
        assert!(
            !text.contains("Canonical source for alpha/one pr 7"),
            "stale alpha read replaced beta source: {text}"
        );
        text.contains("Saved on GitHub")
            && state()["writes"]
                .as_array()
                .is_some_and(|writes| writes.len() == 1)
    });
    assert_eq!(
        state()["writes"][0],
        json!({"host":"github.com","repo":"beta/two","number":7,"kind":"pr","body":"only beta receives this comment"})
    );
    wait(&mut screen, |text| {
        text.contains("Canonical source for beta/two pr 7")
    });
    key(&mut screen, KeyCode::Char('k'));
    key(&mut screen, KeyCode::Char('C'));
    assert!(shown(&mut screen).contains("alpha draft untouched"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('j'));
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('L'));
    }
    wait(&mut screen, |text| text.contains("source_for_beta"));
    key(&mut screen, KeyCode::Char('f'));
    let files = shown(&mut screen);
    assert!(files.contains("beta.rs"), "{files}");
    assert!(!files.contains("alpha.rs"), "{files}");
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Char('o'));
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |_| {
        state()["browsers"]
            .as_array()
            .is_some_and(|browsers| browsers.len() == 1)
    });
    assert_eq!(
        state()["browsers"][0],
        json!({"host":"github.com","repo":"beta/two","number":7})
    );
    wait(&mut screen, |text| text.contains("Opened in browser"));
    key(&mut screen, KeyCode::Char('S'));
    paste(&mut screen, "Review queue");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |text| {
        text.contains("View saved locally: Review queue")
    });
    let root = PathBuf::from(std::env::var_os("DUST_INBOX_ROOT").unwrap());
    let config = git2::Repository::open(&root)
        .unwrap()
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    let views: Value =
        serde_json::from_str(&config.get_string("dusty-git.githubViews").unwrap()).unwrap();
    assert_eq!(views[0]["repo"], "*");
    assert_eq!(views[0]["host"], "github.com");
    key(&mut screen, KeyCode::Char('i'));
    assert!(!shown(&mut screen).contains("Inbox ·"));
    key(&mut screen, KeyCode::Char('i'));
    assert!(shown(&mut screen).contains("Review queue"));
    assert_local_source_unchanged();
    drop(screen);
    let mut reopened = GitScreen::open(&root).unwrap();
    // Top tabs run Git → Pull requests → Issues → Actions; visit Issues, then back to PRs.
    key(&mut reopened, KeyCode::Char('>'));
    key(&mut reopened, KeyCode::Char('>'));
    wait(&mut reopened, |text| {
        text.contains("Canonical source for local/root issue 7")
    });
    key(&mut reopened, KeyCode::Char('<'));
    wait(&mut reopened, |text| {
        text.contains("Canonical source for local/root pr 7")
    });
    key(&mut reopened, KeyCode::Char('i'));
    wait(&mut reopened, |text| text.contains("Inbox"));
    key(&mut reopened, KeyCode::Char('v'));
    key(&mut reopened, KeyCode::Down); // Review requested → the persisted user section.
    let picker = shown(&mut reopened);
    assert!(picker.contains("Saved: Review queue"), "{picker}");
    control(&mut reopened, KeyCode::Enter);
    assert!(shown(&mut reopened).contains("Review queue"));
    key(&mut reopened, KeyCode::Char('>'));
    wait(&mut reopened, |text| {
        text.contains("Canonical source for alpha/one issue 7")
    });
    key(&mut reopened, KeyCode::Char('j'));
    key(&mut reopened, KeyCode::Char('C'));
    paste(&mut reopened, "beta issue comment");
    control(&mut reopened, KeyCode::Enter);
    wait(&mut reopened, |_| {
        state()["writes"]
            .as_array()
            .is_some_and(|writes| writes.len() == 2)
    });
    assert_eq!(
        state()["writes"][1],
        json!({"host":"github.com","repo":"beta/two","number":7,"kind":"issue","body":"beta issue comment"})
    );
    assert_local_source_unchanged();
    assert!(
        calls()
            .iter()
            .filter(|args| args.first().is_some_and(|arg| arg == "search"))
            .all(|args| args
                .windows(2)
                .any(|pair| pair[0] == "--limit" && pair[1] == "100"))
    );
}

#[test]
fn inbox_pagination_failed_index_reload_keeps_full_identity_and_successful_depth() {
    let name = "inbox_pagination_failed_index_reload_keeps_full_identity_and_successful_depth";
    if fixture_child(name, "git.example.test", "wrong.example.test") {
        return;
    }
    let mut screen =
        open(json!({"calls":[],"host":"git.example.test","count":105,"fail_limits":["200"]}));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| text.contains("100 indexed"));
    key(&mut screen, KeyCode::End);
    wait(&mut screen, |text| {
        text.contains("Canonical source for beta/two pr 56")
    });
    control(&mut screen, KeyCode::Char('n'));
    wait(&mut screen, |text| {
        text.contains("Search index/rate-limit unavailable") && text.contains("100 indexed")
    });
    control(&mut screen, KeyCode::Char('n'));
    wait(&mut screen, |text| text.contains("105 indexed"));
    key(&mut screen, KeyCode::Char('R'));
    wait(&mut screen, |text| {
        text.contains("105 indexed")
            && calls()
                .iter()
                .filter(|args| {
                    args.windows(2)
                        .any(|pair| pair[0] == "--limit" && pair[1] == "200")
                })
                .count()
                >= 3
    });
    assert!(shown(&mut screen).contains("Canonical source for beta/two pr 56"));
    assert!(
        hosted_calls()
            .iter()
            .filter(|(args, _)| args[0] == "search")
            .all(|(_, host)| host == "git.example.test")
    );
    assert_local_source_unchanged();
}

#[test]
fn inbox_binds_search_host_without_changing_other_requests() {
    let name = "inbox_binds_search_host_without_changing_other_requests";
    if fixture_child(name, "github.com", "wrong.example.test") {
        return;
    }
    let mut screen = open(json!({"calls":[],"host":"github.com"}));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| text.contains("2 indexed"));
    // Every request names its host (API URL or `gh api --hostname`), so neither inbox search
    // nor any other read depends on GH_HOST, and GH_HOST itself is left alone.
    let calls = hosted_calls();
    assert!(
        calls
            .iter()
            .any(|(args, host)| args[0] == "search" && host == "github.com")
    );
    assert!(
        calls.iter().all(|(_, host)| host == "github.com"),
        "{calls:?}"
    );
    assert!(
        forge_fake::requests(&state_path())
            .iter()
            .filter_map(|request| request["gh_argv"].as_array())
            .all(|argv| argv
                .windows(2)
                .any(|pair| pair[0] == "--hostname" && pair[1] == "github.com"))
    );
    assert_eq!(std::env::var("GH_HOST").unwrap(), "wrong.example.test");
    assert_local_source_unchanged();
}

#[test]
fn inbox_binds_search_host_when_no_default_is_configured() {
    let name = "inbox_binds_search_host_when_no_default_is_configured";
    if fixture_child(name, "git.example.test", "") {
        return;
    }
    let mut screen = open(json!({"calls":[],"host":"git.example.test"}));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| text.contains("2 indexed"));
    let calls = hosted_calls();
    assert!(
        calls
            .iter()
            .any(|(args, host)| args[0] == "search" && host == "git.example.test")
    );
    assert!(
        calls.iter().all(|(_, host)| host == "git.example.test"),
        "{calls:?}"
    );
    assert!(std::env::var_os("GH_HOST").is_none());
    assert_local_source_unchanged();
}

#[derive(Debug, Eq, PartialEq)]
struct DirtyRootSnapshot {
    root: PathBuf,
    head: git2::Oid,
    head_name: String,
    head_file: Vec<u8>,
    index_file: Vec<u8>,
    staged_blob: git2::Oid,
    files: Vec<(String, Vec<u8>)>,
    worktrees: Vec<String>,
}
impl DirtyRootSnapshot {
    fn capture(screen: &GitScreen) -> Self {
        let root = screen.root().to_path_buf();
        let repository = git2::Repository::open(&root).unwrap();
        let head = repository.head().unwrap();
        let staged_blob = repository
            .index()
            .unwrap()
            .get_path(Path::new("tracked.txt"), 0)
            .unwrap()
            .id;
        let mut files = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_file())
            .map(|entry| {
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            root,
            head: head.target().unwrap(),
            head_name: head.name().unwrap().into(),
            head_file: std::fs::read(repository.path().join("HEAD")).unwrap(),
            index_file: std::fs::read(repository.path().join("index")).unwrap(),
            staged_blob,
            files,
            worktrees: repository
                .worktrees()
                .unwrap()
                .iter()
                .flatten()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[test]
fn inbox_fixture_rejects_wrong_host_cli_targets_before_any_write_or_browser() {
    let name = "inbox_fixture_rejects_wrong_host_cli_targets_before_any_write_or_browser";
    if fixture_child(name, "github.com", "wrong.example.test") {
        return;
    }
    std::fs::write(
        state_path(),
        json!({"host":"github.com","calls":[],"writes":[],"browsers":[]}).to_string(),
    )
    .unwrap();
    for arguments in [
        vec!["pr", "view", "7", "--repo", "other.test/beta/two"],
        vec!["pr", "view", "7", "--web", "--repo", "other.test/beta/two"],
        vec![
            "issue",
            "comment",
            "7",
            "--body",
            "must never be written",
            "--repo",
            "other.test/beta/two",
        ],
        vec!["pr", "diff", "7", "--repo", "other.test/beta/two"],
        vec!["pr", "checkout", "7", "--repo", "other.test/beta/two"],
        vec!["pr", "view", "7", "--repo", "beta/two"],
        vec!["pr", "view", "7"],
        vec![
            "api",
            "--hostname",
            "other.test",
            "repos/beta/two/issues/7/timeline",
        ],
    ] {
        let output = Command::new("gh").args(arguments).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("host") || error.contains("--repo"),
            "{error}"
        );
    }
    assert_eq!(state()["writes"], json!([]));
    assert_eq!(state()["browsers"], json!([]));
    assert_local_source_unchanged();
}

#[test]
fn inbox_explicit_merged_section_preserves_closed_vs_merged_list_truth() {
    let name = "inbox_explicit_merged_section_preserves_closed_vs_merged_list_truth";
    if fixture_child(name, "github.com", "wrong.example.test") {
        return;
    }
    let mut screen = open(json!({"host":"github.com","calls":[]}));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| text.contains("2 indexed"));
    key(&mut screen, KeyCode::Char('/'));
    control(&mut screen, KeyCode::Char('e'));
    control(&mut screen, KeyCode::Char('u'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Down); // open -> closed
    key(&mut screen, KeyCode::Down); // closed -> merged
    control(&mut screen, KeyCode::Enter);
    // By the search itself, not the fake's last-search state: a section's count search may
    // run after it on a loaded machine.
    let searched = |flag: &'static str, value: Option<&'static str>| {
        move || {
            calls().iter().any(|args| {
                args.first().is_some_and(|arg| arg == "search")
                    && args
                        .windows(2)
                        .any(|pair| pair[0] == flag && value.is_none_or(|value| pair[1] == value))
                    && !args.iter().any(|arg| arg.contains("@me"))
            })
        }
    };
    let merged = searched("--merged", None);
    let marked = |text: &str, word: &str| {
        text.lines()
            .any(|line| line.contains("#7 by alice") && line.contains(word))
    };
    wait(&mut screen, |text| {
        text.contains("2 indexed") && marked(text, "Merged") && merged()
    });
    let frame = screen.frame(190, 65);
    // The record's state chip names the lifecycle (the glyph differs only in colour); it
    // ends the record's meta row, so every title clips at one column (r12).
    assert!(
        (0..frame.grid.height()).any(|y| {
            frame.grid.row_text(y).contains("alpha/one item 7")
                && (frame.grid.row_text(y).contains("Merged")
                    || frame.grid.row_text(y + 1).contains("Merged"))
        }),
        "explicit merged search must mark the records merged"
    );
    assert!(
        calls()
            .iter()
            .any(|args| args.first().is_some_and(|arg| arg == "search")
                && args.iter().any(|arg| arg == "--merged"))
    );
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Up); // merged -> closed (not merged-only)
    control(&mut screen, KeyCode::Enter);
    let closed = searched("--state", Some("closed"));
    wait(&mut screen, |text| {
        text.contains("2 indexed") && marked(text, "Closed") && closed()
    });
    let frame = screen.frame(190, 65);
    assert!(
        (0..frame.grid.height()).any(|y| {
            let row = frame.grid.row_text(y);
            let meta = frame.grid.row_text(y + 1);
            row.contains("alpha/one item 7")
                && (row.contains("Closed") || meta.contains("Closed"))
                && !row.contains("Merged")
                && !meta.contains("Merged")
        }),
        "ordinary closed results must not all be relabeled merged"
    );
    assert_local_source_unchanged();
}

#[test]
fn inbox_failed_confirmed_write_after_a_b_a_preserves_dirty_root_and_scoped_drafts() {
    let name = "inbox_failed_confirmed_write_after_a_b_a_preserves_dirty_root_and_scoped_drafts";
    if fixture_child_with_root(name, "github.com", "wrong.example.test", true) {
        return;
    }
    let mut screen = open(json!({"host":"github.com","calls":[],"writes":[],
        "gates":[{"prefix":["pr","comment","7"],"repo":"alpha/one","name":"alpha-write"}],
        "fail_comment_repos":["alpha/one"]
    }));
    let before = DirtyRootSnapshot::capture(&screen);
    let repository = git2::Repository::open(screen.root()).unwrap();
    assert_eq!(
        repository.find_blob(before.staged_blob).unwrap().content(),
        b"staged source\n"
    );
    assert_eq!(
        std::fs::read(screen.root().join("tracked.txt"))
            .unwrap()
            .as_slice(),
        b"dirty working source\n"
    );
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |text| {
        text.contains("Canonical source for alpha/one pr 7")
    });
    key(&mut screen, KeyCode::Char('C'));
    paste(&mut screen, "alpha failed draft must survive");
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |_| {
        calls().iter().any(|args| {
            args.starts_with(&["pr".into(), "comment".into(), "7".into()])
                && args.iter().any(|arg| arg == "github.com/alpha/one")
        })
    });
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('C'));
    assert!(!shown(&mut screen).contains("alpha failed draft"));
    paste(&mut screen, "beta independent draft");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('k'));
    assert_eq!(DirtyRootSnapshot::capture(&screen), before);
    std::fs::write(format!("{}.alpha-write", state_path().display()), "release").unwrap();
    wait(&mut screen, |text| {
        text.contains("Fixture comment write refused · your draft is kept")
    });
    assert_eq!(state()["writes"], json!([]));
    assert_eq!(
        state()["write_attempts"][0],
        json!({
            "host":"github.com","repo":"alpha/one","kind":"pr","number":7,"body":"alpha failed draft must survive"
        })
    );
    key(&mut screen, KeyCode::Char('C'));
    assert!(shown(&mut screen).contains("alpha failed draft must survive"));
    assert!(!shown(&mut screen).contains("beta independent draft"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Char('C'));
    assert!(shown(&mut screen).contains("beta independent draft"));
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |_| {
        state()["writes"]
            .as_array()
            .is_some_and(|writes| writes.len() == 1)
    });
    assert_eq!(
        state()["writes"][0],
        json!({
            "host":"github.com","repo":"beta/two","kind":"pr","number":7,"body":"beta independent draft"
        })
    );
    wait(&mut screen, |text| text.contains("Saved on GitHub"));
    key(&mut screen, KeyCode::Char('k'));
    key(&mut screen, KeyCode::Char('C'));
    assert!(shown(&mut screen).contains("alpha failed draft must survive"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(DirtyRootSnapshot::capture(&screen), before);
    key(&mut screen, KeyCode::Char('i'));
    assert_eq!(DirtyRootSnapshot::capture(&screen), before);
}
