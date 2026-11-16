//! Branch merges, rebases and paused-operation actions through native input and real repositories.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{Frame, GitSurface, KeyInputKind, Style, UiEvent};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const TOPIC: &str = "topic/merge";
const SIZES: [(u16, u16); 3] = [(190, 48), (100, 30), (64, 20)];

fn output(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap()
}
fn git(root: &Path, args: &[&str]) -> String {
    let result = output(root, args);
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim_end().into()
}
#[derive(Clone, Copy)]
enum History {
    FastForward,
    Diverged,
    Conflict,
}
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    before: String,
    topic: String,
}
impl Fixture {
    fn new(history: History) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Merge Test"]);
        git(&root, &["config", "user.email", "merge@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        git(&root, &["config", "merge.ff", "true"]);
        for path in ["conflict.txt", "working.txt", "staged.txt"] {
            std::fs::write(root.join(path), "baseline\n").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "Generated baseline"]);
        git(&root, &["checkout", "-b", TOPIC]);
        let conflict = matches!(history, History::Conflict);
        std::fs::write(
            root.join(if conflict {
                "conflict.txt"
            } else {
                "topic.txt"
            }),
            "selected topic bytes\n",
        )
        .unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "Selected topic commit"]);
        let topic = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["checkout", "main"]);
        if !matches!(history, History::FastForward) {
            std::fs::write(
                root.join(if conflict { "conflict.txt" } else { "main.txt" }),
                "current branch bytes\n",
            )
            .unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-m", "Current branch commit"]);
        }
        let before = git(&root, &["rev-parse", "HEAD"]);
        Self {
            _directory: directory,
            root,
            before,
            topic,
        }
    }
    fn index(&self) -> Vec<u8> {
        let result = output(&self.root, &["ls-files", "--stage", "-z"]);
        assert!(result.status.success());
        result.stdout
    }
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        std::fs::read_dir(&self.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().is_file())
            .map(|entry| {
                (
                    PathBuf::from(entry.file_name()),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }
    fn pause(&self) {
        let result = output(&self.root, &["merge", "--no-edit", "--no-autostash", TOPIC]);
        assert!(!result.status.success());
        assert!(self.root.join(".git/MERGE_HEAD").exists());
    }
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
fn text(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    text(&screen.frame(190, 48))
}
fn wait(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(Instant::now() < deadline, "{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// Whether the repository itself is paused in a merge, rebase, cherry-pick or revert.
fn paused(root: &Path) -> bool {
    [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|name| root.join(".git").join(name).exists())
}
/// Idle, and the operation banner (row 1) agrees with the repository: the banner is
/// filled from the asynchronous status refresh, so it can lag a finished job.
fn finished(screen: &mut GitScreen) {
    wait(screen, |screen| {
        let paused = paused(screen.root());
        !screen.busy() && screen.frame(190, 48).grid.row_text(1).contains('⟳') == paused
    });
}
fn filter(screen: &mut GitScreen, query: &str) {
    key(screen, KeyCode::Char('/'));
    control(screen, KeyCode::Char('e'));
    control(screen, KeyCode::Char('u'));
    screen.event(UiEvent::Paste(query.into()));
    key(screen, KeyCode::Enter);
}
fn open(fixture: &Fixture) -> GitScreen {
    let mut screen = GitScreen::open(&fixture.root).unwrap();
    finished(&mut screen);
    key(&mut screen, KeyCode::Char('3'));
    filter(&mut screen, TOPIC);
    wait(&mut screen, |screen| {
        let frame = screen.frame(100, 30);
        frame.cursor.is_some_and(|(x, y, _)| {
            x < 25
                && (0..25)
                    .filter_map(|column| frame.grid.get(column, y))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
                    .contains(TOPIC)
        })
    });
    screen
}
fn merge_form(screen: &mut GitScreen) {
    key(screen, KeyCode::Char('m'));
    assert!(view(screen).contains("Merge selected branch?"));
}
fn when_ready(screen: &mut GitScreen, action: impl Fn(&mut GitScreen)) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        action(screen);
        screen.poll();
        if !view(screen).contains("Wait for the current Git operation") {
            return;
        }
        assert!(Instant::now() < deadline, "{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn submit(screen: &mut GitScreen) {
    when_ready(screen, |screen| control(screen, KeyCode::Enter));
}
fn start_merge(screen: &mut GitScreen) {
    merge_form(screen);
    submit(screen);
    finished(screen);
}
fn assert_parents(fixture: &Fixture) {
    let parents = git(&fixture.root, &["show", "-s", "--format=%P", "HEAD"]);
    assert_eq!(parents, format!("{} {}", fixture.before, fixture.topic));
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(git(&fixture.root, &["rev-parse", TOPIC]), fixture.topic);
}

#[test]
fn confirmed_fast_forward_keeps_unrelated_index_working_files_and_drafts() {
    let fixture = Fixture::new(History::FastForward);
    std::fs::write(fixture.root.join("working.txt"), "keep working bytes\n").unwrap();
    std::fs::write(fixture.root.join("staged.txt"), "keep staged bytes\n").unwrap();
    git(&fixture.root, &["add", "staged.txt"]);
    std::fs::write(fixture.root.join("staged.txt"), "keep later bytes\n").unwrap();
    std::fs::write(fixture.root.join("draft.txt"), "keep untracked draft\n").unwrap();
    let index = fixture.index();
    let mut files = fixture.files();
    let mut screen = open(&fixture);
    merge_form(&mut screen);
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let rendered = text(&frame);
        assert!(rendered.contains("Merge into") && rendered.contains("Selected branch or tag"));
        assert!(rendered.contains(TOPIC) && rendered.contains(" Merge "));
        let (x, y, _) = frame.cursor.unwrap();
        assert!(x < width && y < height - 1);
        let row = frame.grid.row_text(y);
        assert!(row.contains(" Merge ") && row.contains(" Cancel "), "{row}");
    }
    key(&mut screen, KeyCode::Escape);
    assert_eq!(fixture.index(), index);
    assert_eq!(fixture.files(), files);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    start_merge(&mut screen);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.topic);
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        git(&fixture.root, &["show", ":staged.txt"]),
        "keep staged bytes"
    );
    files.insert("topic.txt".into(), b"selected topic bytes\n".to_vec());
    assert_eq!(fixture.files(), files);
    let current = fixture.index();
    let remaining = current
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty() && !entry.ends_with(b"\ttopic.txt"))
        .collect::<Vec<_>>();
    let original = index
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        remaining, original,
        "every unrelated index entry is unchanged"
    );
    assert_eq!(
        git(&fixture.root, &["show", ":topic.txt"]),
        "selected topic bytes"
    );
    assert!(git(&fixture.root, &["stash", "list"]).is_empty());
}

#[test]
fn diverged_branch_merge_records_both_parents_without_switching_the_selected_branch() {
    let fixture = Fixture::new(History::Diverged);
    std::fs::write(fixture.root.join("draft.txt"), "keep this draft\n").unwrap();
    let mut screen = open(&fixture);
    start_merge(&mut screen);
    assert_parents(&fixture);
    assert_eq!(
        std::fs::read(fixture.root.join("topic.txt")).unwrap(),
        b"selected topic bytes\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("main.txt")).unwrap(),
        b"current branch bytes\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("draft.txt")).unwrap(),
        b"keep this draft\n"
    );
    assert!(git(&fixture.root, &["diff", "HEAD", "--", "."]).is_empty());
    assert!(!fixture.root.join(".git/MERGE_HEAD").exists());
}

#[test]
fn stopped_merge_refreshes_conflicts_then_native_side_stage_and_continue_finish_it() {
    let fixture = Fixture::new(History::Conflict);
    std::fs::write(
        fixture.root.join("draft.txt"),
        "untracked draft survives continue\n",
    )
    .unwrap();
    let mut screen = open(&fixture);
    start_merge(&mut screen);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    assert!(fixture.root.join(".git/MERGE_HEAD").exists());
    assert!(!git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    wait(&mut screen, |screen| view(screen).contains("abort"));
    let stopped = view(&mut screen);
    assert!(stopped.contains("CONFLICT"), "{stopped}");
    assert!(stopped.contains("abort"), "{stopped}");
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Resolve and stage"));
    assert!(!view(&mut screen).contains("Continue resolved merge?"));
    key(&mut screen, KeyCode::Char('2'));
    filter(&mut screen, "conflict.txt");
    wait(&mut screen, |screen| {
        let rendered = view(screen);
        rendered.contains("selected topic bytes") && rendered.contains("current branch bytes")
    });
    let before_inspection = replay_inspection_state(&fixture);
    let merge_head = std::fs::read(fixture.root.join(".git/MERGE_HEAD")).unwrap();
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        assert_conflict_row_and_reader(&frame, width, height);
    }
    assert_eq!(replay_inspection_state(&fixture), before_inspection);
    assert_eq!(
        std::fs::read(fixture.root.join(".git/MERGE_HEAD")).unwrap(),
        merge_head
    );
    key(&mut screen, KeyCode::Char('t'));
    assert!(view(&mut screen).contains("stage separately"));
    submit(&mut screen);
    finished(&mut screen);
    assert_eq!(
        std::fs::read(fixture.root.join("conflict.txt")).unwrap(),
        b"selected topic bytes\n"
    );
    assert!(!git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    key(&mut screen, KeyCode::Char('-'));
    finished(&mut screen);
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    wait(&mut screen, |screen| view(screen).contains("continue"));
    for (width, height) in SIZES {
        let rendered = text(&screen.frame(width, height));
        assert!(
            rendered.contains("continue") && rendered.contains("abort"),
            "{rendered}"
        );
    }
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Continue resolved merge?"));
    submit(&mut screen);
    finished(&mut screen);
    assert_parents(&fixture);
    assert!(!fixture.root.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        std::fs::read(fixture.root.join("draft.txt")).unwrap(),
        b"untracked draft survives continue\n"
    );
}

/// The conflicted file is the native cursor row of the flat Changes list (`UU` status),
/// and the reader beside it (row 2, below the operation banner) is titled with that
/// file. At 64 columns only the focused Changes pane is shown.
fn assert_conflict_row_and_reader(frame: &Frame, width: u16, height: u16) {
    let (x, y, _) = frame.cursor.expect("native conflict file cursor");
    assert_eq!(x, 1);
    let row = frame.grid.row_text(y);
    assert!(row.contains("UU") && row.contains("conflict.txt"), "{row}");
    let border = frame.grid.row_text(2);
    if width >= 100 {
        let reader = border
            .chars()
            .skip(usize::from(width / 4))
            .collect::<String>();
        assert!(
            reader.contains("conflict.txt"),
            "{width}x{height}: {border}"
        );
    } else {
        assert!(border.contains("Changes"), "{width}x{height}: {border}");
    }
}

#[test]
fn native_abort_cancel_preserves_paused_merge_then_abort_restores_its_start() {
    let fixture = Fixture::new(History::Conflict);
    let original = fixture.files();
    let mut screen = open(&fixture);
    start_merge(&mut screen);
    let paused_files = fixture.files();
    let paused_index = fixture.index();
    key(&mut screen, KeyCode::Char('X'));
    assert!(view(&mut screen).contains("Abort merge?"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(fixture.files(), paused_files);
    assert_eq!(fixture.index(), paused_index);
    assert!(fixture.root.join(".git/MERGE_HEAD").exists());
    key(&mut screen, KeyCode::Char('X'));
    submit(&mut screen);
    finished(&mut screen);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    assert_eq!(fixture.files(), original);
    assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
    assert!(!fixture.root.join(".git/MERGE_HEAD").exists());
}

#[test]
fn merge_confirmation_rejects_a_changed_target_current_tip_or_current_branch() {
    for changed in ["target", "tip", "branch"] {
        let fixture = Fixture::new(History::FastForward);
        git(&fixture.root, &["branch", "other-current"]);
        let mut screen = open(&fixture);
        merge_form(&mut screen);
        if changed == "branch" {
            git(&fixture.root, &["checkout", "other-current"]);
        } else {
            let tree = git(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
            let commit = git(
                &fixture.root,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    "HEAD",
                    "-m",
                    "External ref update",
                ],
            );
            let reference = if changed == "target" {
                "refs/heads/topic/merge"
            } else {
                "refs/heads/main"
            };
            git(&fixture.root, &["update-ref", reference, &commit]);
        }
        let head = git(&fixture.root, &["rev-parse", "HEAD"]);
        let index = fixture.index();
        let files = fixture.files();
        submit(&mut screen);
        let rendered = view(&mut screen);
        assert!(
            rendered.contains("changed") && rendered.contains("Merge selected branch?"),
            "{rendered}"
        );
        assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), head);
        assert_eq!(fixture.index(), index);
        assert_eq!(fixture.files(), files);
        assert!(!fixture.root.join(".git/MERGE_HEAD").exists());
    }
}

#[test]
fn merge_continue_abort_recheck_host_authorization_and_show_refusals_inside_forms() {
    let fixture = Fixture::new(History::FastForward);
    let mut screen = open(&fixture);
    merge_form(&mut screen);
    screen.write_refusal = Some("Host changed to read-only".into());
    submit(&mut screen);
    for (width, height) in SIZES {
        let rendered = text(&screen.frame(width, height));
        assert!(
            rendered.contains("read-only") && rendered.contains("Merge selected branch?"),
            "{rendered}"
        );
    }
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    let fixture = Fixture::new(History::Conflict);
    fixture.pause();
    std::fs::write(fixture.root.join("conflict.txt"), "resolved bytes\n").unwrap();
    git(&fixture.root, &["add", "conflict.txt"]);
    let files = fixture.files();
    let index = fixture.index();
    let mut screen = open(&fixture);
    for (code, title) in [('C', "Continue resolved merge?"), ('X', "Abort merge?")] {
        key(&mut screen, KeyCode::Char(code));
        assert!(view(&mut screen).contains(title));
        screen.write_refusal = Some("Host changed to read-only".into());
        submit(&mut screen);
        for (width, height) in SIZES {
            let rendered = text(&screen.frame(width, height));
            assert!(
                rendered.contains("read-only") && rendered.contains(title),
                "{rendered}"
            );
        }
        assert_eq!(fixture.files(), files);
        assert_eq!(fixture.index(), index);
        assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
        key(&mut screen, KeyCode::Escape);
        screen.write_refusal = None;
    }
}

#[test]
fn abort_protects_unrelated_staging_then_keeps_all_working_drafts_after_native_unstage() {
    let fixture = Fixture::new(History::Conflict);
    fixture.pause();
    std::fs::write(fixture.root.join("draft.txt"), "keep unrelated draft\n").unwrap();
    std::fs::write(
        fixture.root.join("working.txt"),
        "keep unrelated working bytes\n",
    )
    .unwrap();
    std::fs::write(fixture.root.join("later.txt"), "keep newly staged bytes\n").unwrap();
    git(&fixture.root, &["add", "later.txt"]);
    let mut files = fixture.files();
    let index = fixture.index();
    let mut screen = open(&fixture);
    key(&mut screen, KeyCode::Char('X'));
    submit(&mut screen);
    let rendered = view(&mut screen);
    assert!(
        rendered.contains("Abort merge?")
            && rendered.contains("would discard")
            && rendered.contains("later.txt"),
        "{rendered}"
    );
    assert_eq!(fixture.files(), files);
    assert_eq!(fixture.index(), index);
    assert!(fixture.root.join(".git/MERGE_HEAD").exists());
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('2'));
    filter(&mut screen, "later.txt");
    wait(&mut screen, |screen| {
        let frame = screen.frame(100, 30);
        frame.cursor.is_some_and(|(x, y, _)| {
            x < 25
                && (0..25)
                    .filter_map(|column| frame.grid.get(column, y))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
                    .contains("later.txt")
        })
    });
    when_ready(&mut screen, |screen| key(screen, KeyCode::Char('-')));
    finished(&mut screen);
    assert!(
        git(&fixture.root, &["ls-files", "--stage", "later.txt"]).is_empty(),
        "native unstage left later.txt staged; busy={}; status={}\n{}",
        screen.busy(),
        git(&fixture.root, &["status", "--porcelain=v1"]),
        view(&mut screen)
    );
    assert_eq!(fixture.files(), files);
    key(&mut screen, KeyCode::Char('X'));
    submit(&mut screen);
    finished(&mut screen);
    assert!(!fixture.root.join(".git/MERGE_HEAD").exists());
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    files.insert("conflict.txt".into(), b"current branch bytes\n".to_vec());
    assert_eq!(fixture.files(), files);
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    assert_eq!(git(&fixture.root, &["show", ":working.txt"]), "baseline");
}

#[test]
fn abort_confirmation_cannot_switch_from_merge_to_an_external_cherry_pick() {
    let fixture = Fixture::new(History::Conflict);
    fixture.pause();
    let mut screen = open(&fixture);
    key(&mut screen, KeyCode::Char('X'));
    assert!(view(&mut screen).contains("Abort merge?"));
    git(&fixture.root, &["merge", "--abort"]);
    assert!(
        !output(&fixture.root, &["cherry-pick", TOPIC])
            .status
            .success()
    );
    assert!(fixture.root.join(".git/CHERRY_PICK_HEAD").exists());
    let files = fixture.files();
    let index = fixture.index();
    submit(&mut screen);
    let rendered = view(&mut screen);
    assert!(
        rendered.contains("Abort merge?") && rendered.contains("operation changed"),
        "{rendered}"
    );
    assert!(fixture.root.join(".git/CHERRY_PICK_HEAD").exists());
    assert_eq!(fixture.files(), files);
    assert_eq!(fixture.index(), index);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
}

fn rebase_form(screen: &mut GitScreen) {
    key(screen, KeyCode::Char('R'));
    assert!(view(screen).contains("Rebase current branch?"));
}
fn start_rebase(screen: &mut GitScreen) {
    rebase_form(screen);
    submit(screen);
    finished(screen);
}
fn resolve_rebase_file(screen: &mut GitScreen, fixture: &Fixture) {
    key(screen, KeyCode::Char('2'));
    filter(screen, "conflict.txt");
    wait(screen, |screen| {
        let frame = screen.frame(100, 30);
        frame.cursor.is_some_and(|(x, y, _)| {
            x < 25
                && (0..25)
                    .filter_map(|column| frame.grid.get(column, y))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
                    .contains("conflict.txt")
        })
    });
    key(screen, KeyCode::Char('t'));
    assert!(view(screen).contains("stage separately"));
    submit(screen);
    finished(screen);
    assert_eq!(
        std::fs::read(fixture.root.join("conflict.txt")).unwrap(),
        b"current branch bytes\n"
    );
    assert!(!git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    when_ready(screen, |screen| key(screen, KeyCode::Char('-')));
    finished(screen);
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
}

#[test]
fn confirmed_rebase_rewrites_only_current_branch_and_preserves_selected_and_other_refs() {
    let fixture = Fixture::new(History::Diverged);
    git(&fixture.root, &["branch", "keep-original"]);
    git(&fixture.root, &["config", "rebase.updateRefs", "true"]);
    let author = git(
        &fixture.root,
        &["show", "-s", "--format=%an%n%ae%n%aI", "HEAD"],
    );
    let mut expected = fixture.files();
    let index = fixture.index();
    let mut screen = open(&fixture);
    rebase_form(&mut screen);
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let rendered = text(&frame);
        assert!(
            rendered.contains("Rewrite current branch") && rendered.contains(" Rebase "),
            "{rendered}"
        );
        let (x, y, _) = frame.cursor.unwrap();
        let row = frame.grid.row_text(y);
        assert!(
            x < width && y < height - 1 && row.contains(" Rebase ") && row.contains(" Cancel "),
            "{row}"
        );
    }
    key(&mut screen, KeyCode::Escape);
    assert_eq!(fixture.files(), expected);
    assert_eq!(fixture.index(), index);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    start_rebase(&mut screen);
    assert_ne!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%P", "HEAD"]),
        fixture.topic
    );
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(git(&fixture.root, &["rev-parse", TOPIC]), fixture.topic);
    assert_eq!(
        git(&fixture.root, &["rev-parse", "keep-original"]),
        fixture.before
    );
    assert_eq!(
        git(
            &fixture.root,
            &["show", "-s", "--format=%an%n%ae%n%aI", "HEAD"]
        ),
        author
    );
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "Current branch commit"
    );
    expected.insert("topic.txt".into(), b"selected topic bytes\n".to_vec());
    assert_eq!(fixture.files(), expected);
    assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn rebase_dirty_and_host_refusals_preserve_all_refs_index_and_working_bytes_without_stash() {
    for dirty in [true, false] {
        let fixture = Fixture::new(History::Diverged);
        if dirty {
            std::fs::write(fixture.root.join("working.txt"), "keep working bytes\n").unwrap();
            std::fs::write(fixture.root.join("staged.txt"), "keep staged bytes\n").unwrap();
            git(&fixture.root, &["add", "staged.txt"]);
            std::fs::write(fixture.root.join("staged.txt"), "keep later bytes\n").unwrap();
        }
        std::fs::write(fixture.root.join("draft.txt"), "keep draft bytes\n").unwrap();
        let refs = git(
            &fixture.root,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        let index = fixture.index();
        let files = fixture.files();
        let mut screen = open(&fixture);
        rebase_form(&mut screen);
        if !dirty {
            screen.write_refusal = Some("Host changed to read-only".into());
        }
        submit(&mut screen);
        finished(&mut screen);
        let rendered = view(&mut screen);
        assert!(
            if dirty {
                rendered.contains("cannot rebase")
            } else {
                rendered.contains("read-only")
            },
            "{rendered}"
        );
        assert_eq!(
            git(
                &fixture.root,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            refs
        );
        assert_eq!(fixture.index(), index);
        assert_eq!(fixture.files(), files);
        assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
        assert!(git(&fixture.root, &["stash", "list"]).is_empty());
        assert!(!fixture.root.join(".git/rebase-merge").exists());
    }
}

#[test]
fn rebase_confirmation_rejects_moved_source_current_tip_and_current_branch() {
    for changed in ["source", "tip", "branch"] {
        let fixture = Fixture::new(History::Diverged);
        git(&fixture.root, &["branch", "other-current"]);
        let mut screen = open(&fixture);
        rebase_form(&mut screen);
        if changed == "branch" {
            git(&fixture.root, &["checkout", "other-current"]);
        } else {
            let tree = git(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
            let commit = git(
                &fixture.root,
                &["commit-tree", &tree, "-p", "HEAD", "-m", "External update"],
            );
            let reference = if changed == "source" {
                "refs/heads/topic/merge"
            } else {
                "refs/heads/main"
            };
            git(&fixture.root, &["update-ref", reference, &commit]);
        }
        let refs = git(
            &fixture.root,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        let files = fixture.files();
        let index = fixture.index();
        submit(&mut screen);
        assert!(
            view(&mut screen).contains("changed")
                && view(&mut screen).contains("Rebase current branch?")
        );
        assert_eq!(
            git(
                &fixture.root,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            refs
        );
        assert_eq!(fixture.files(), files);
        assert_eq!(fixture.index(), index);
    }
}

#[test]
fn real_cli_rebase_conflict_native_side_stage_and_continue_preserve_author_message_and_draft() {
    let fixture = Fixture::new(History::Conflict);
    let author = git(
        &fixture.root,
        &["show", "-s", "--format=%an%n%ae%n%aI", "HEAD"],
    );
    std::fs::write(fixture.root.join("draft.txt"), "keep untracked draft\n").unwrap();
    let mut screen = open(&fixture);
    start_rebase(&mut screen);
    wait(&mut screen, |screen| view(screen).contains("S skip"));
    assert!(fixture.root.join(".git/rebase-merge").exists());
    assert!(!git(&fixture.root, &["ls-files", "--unmerged"]).is_empty());
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Resolve and stage"));
    resolve_rebase_file(&mut screen, &fixture);
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Continue resolved rebase?"));
    submit(&mut screen);
    finished(&mut screen);
    assert!(!fixture.root.join(".git/rebase-merge").exists());
    for (width, height) in SIZES {
        let summary = operation_summary_at(&mut screen, width, height);
        assert!(
            !summary.contains("Current ") && !summary.contains("Current branch commit"),
            "{summary}"
        );
    }
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%P", "HEAD"]),
        fixture.topic
    );
    assert_eq!(
        git(
            &fixture.root,
            &["show", "-s", "--format=%an%n%ae%n%aI", "HEAD"]
        ),
        author
    );
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "Current branch commit"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("draft.txt")).unwrap(),
        b"keep untracked draft\n"
    );
    assert!(git(&fixture.root, &["diff", "HEAD", "--", "."]).is_empty());
}

/// The receipt band at the top of the reader: its text and surface.
fn receipt(screen: &mut GitScreen, needle: &str) -> (String, Option<GitSurface>) {
    let frame = screen.frame(190, 48);
    for y in 0..frame.grid.height() {
        let row = frame.grid.row_text(y);
        if let Some(at) = row.find(needle) {
            let column = row[..at].chars().count() as u16;
            let surface = match frame.grid.get(column, y).map(|cell| cell.style) {
                Some(Style::Git { surface, .. }) => Some(surface),
                _ => None,
            };
            return (row, surface);
        }
    }
    panic!("no receipt {needle:?}:\n{}", text(&frame));
}

#[test]
fn rebase_continue_and_skip_that_stop_on_the_next_conflict_close_the_sheet_with_a_receipt() {
    let fixture = Fixture::new(History::Conflict);
    // Two more commits on main that each conflict with the topic: add/add on new files.
    git(&fixture.root, &["checkout", TOPIC]);
    for path in ["second.txt", "third.txt"] {
        std::fs::write(fixture.root.join(path), "topic bytes\n").unwrap();
    }
    git(&fixture.root, &["add", "."]);
    git(
        &fixture.root,
        &["commit", "-m", "Topic adds second and third"],
    );
    git(&fixture.root, &["checkout", "main"]);
    for (path, subject) in [
        ("second.txt", "Second main commit"),
        ("third.txt", "Third main commit"),
    ] {
        std::fs::write(fixture.root.join(path), format!("{subject} bytes\n")).unwrap();
        git(&fixture.root, &["add", path]);
        git(&fixture.root, &["commit", "-m", subject]);
    }
    let mut screen = open(&fixture);
    start_rebase(&mut screen);
    wait(&mut screen, |screen| view(screen).contains("S skip"));
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).contains("conflict.txt"));
    resolve_rebase_file(&mut screen, &fixture);

    // Continue records the first commit, then Git stops on the second conflict.
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Continue resolved rebase?"));
    submit(&mut screen);
    finished(&mut screen);
    let shown = view(&mut screen);
    assert!(
        !shown.contains("Continue resolved rebase?"),
        "the sheet closes: {shown}"
    );
    assert!(!shown.contains("operation changed"), "{shown}");
    assert!(!shown.contains("failed"), "{shown}");
    let (row, surface) = receipt(
        &mut screen,
        "Rebase continued · stopped on conflict in second.txt (2/3)",
    );
    assert!(row.contains('⚠') && !row.contains('✕'), "{row}");
    assert_eq!(
        surface,
        Some(GitSurface::Warning),
        "a neutral receipt, not an error: {row}"
    );
    assert!(fixture.root.join(".git/rebase-merge").exists());
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).contains("second.txt"));
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "Current branch commit"
    );
    // The bar was refreshed: continue is refused again until second.txt is resolved.
    let banner = operation_summary_at(&mut screen, 190, 48);
    assert!(
        banner.contains("S skip") && !banner.contains("C continue"),
        "{banner}"
    );
    key(&mut screen, KeyCode::Char('C'));
    assert!(view(&mut screen).contains("Resolve and stage"));

    // Skip drops the second commit, and Git stops on the third conflict.
    key(&mut screen, KeyCode::Char('S'));
    assert!(view(&mut screen).contains("Skip current rebase commit?"));
    submit(&mut screen);
    finished(&mut screen);
    let shown = view(&mut screen);
    assert!(
        !shown.contains("Skip current rebase commit?"),
        "the sheet closes: {shown}"
    );
    let (row, surface) = receipt(
        &mut screen,
        "Rebase commit skipped · stopped on conflict in third.txt (3/3)",
    );
    assert_eq!(surface, Some(GitSurface::Warning), "{row}");
    assert!(git(&fixture.root, &["ls-files", "--unmerged"]).contains("third.txt"));
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "Current branch commit"
    );

    // Abort still works from the new stop and restores main.
    key(&mut screen, KeyCode::Char('X'));
    submit(&mut screen);
    finished(&mut screen);
    assert!(!fixture.root.join(".git/rebase-merge").exists());
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "Third main commit"
    );
}

#[test]
fn real_cli_rebase_abort_and_skip_cancel_then_restore_or_drop_only_the_replayed_commit() {
    for code in ['X', 'S'] {
        let fixture = Fixture::new(History::Conflict);
        let mut expected = fixture.files();
        let mut screen = open(&fixture);
        start_rebase(&mut screen);
        wait(&mut screen, |screen| view(screen).contains("S skip"));
        let paused_files = fixture.files();
        let paused_index = fixture.index();
        let title = if code == 'X' {
            "Abort rebase?"
        } else {
            "Skip current rebase commit?"
        };
        key(&mut screen, KeyCode::Char(code));
        for (width, height) in SIZES {
            let frame = screen.frame(width, height);
            assert!(text(&frame).contains(title));
            if code == 'X' {
                assert!(
                    text(&frame).contains("Return to branch")
                        && text(&frame).contains("Restore commit"),
                    "{}",
                    text(&frame)
                );
                assert!(
                    text(&frame).contains(&fixture.before[..8]),
                    "{}",
                    text(&frame)
                );
                assert!(
                    !text(&frame).contains("read only")
                        && !text(&frame).contains("3/3")
                        && !text(&frame).contains("refuse unrelated"),
                    "{}",
                    text(&frame)
                );
            }
            let (x, y, _) = frame.cursor.unwrap();
            assert!(x < width && y < height - 2);
        }
        key(&mut screen, KeyCode::Escape);
        assert_eq!(fixture.files(), paused_files);
        assert_eq!(fixture.index(), paused_index);
        key(&mut screen, KeyCode::Char(code));
        submit(&mut screen);
        finished(&mut screen);
        assert!(!fixture.root.join(".git/rebase-merge").exists());
        for (width, height) in SIZES {
            let summary = operation_summary_at(&mut screen, width, height);
            assert!(
                !summary.contains("Current ") && !summary.contains("Current branch commit"),
                "{summary}"
            );
        }
        assert_eq!(
            git(&fixture.root, &["symbolic-ref", "HEAD"]),
            "refs/heads/main"
        );
        assert_eq!(
            git(&fixture.root, &["rev-parse", "HEAD"]),
            if code == 'X' {
                fixture.before.as_str()
            } else {
                fixture.topic.as_str()
            }
        );
        if code == 'S' {
            expected.insert("conflict.txt".into(), b"selected topic bytes\n".to_vec());
        }
        assert_eq!(fixture.files(), expected);
        assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
    }
}

#[test]
fn rebase_lifecycle_checks_authorization_unrelated_data_and_captured_replay_before_writing() {
    for code in ['C', 'X', 'S'] {
        let fixture = Fixture::new(History::Conflict);
        let mut screen = open(&fixture);
        start_rebase(&mut screen);
        wait(&mut screen, |screen| view(screen).contains("S skip"));
        if code == 'C' {
            resolve_rebase_file(&mut screen, &fixture);
        }
        key(&mut screen, KeyCode::Char(code));
        let files = fixture.files();
        let index = fixture.index();
        screen.write_refusal = Some("Host changed to read-only".into());
        submit(&mut screen);
        assert!(view(&mut screen).contains("read-only"));
        assert_eq!(fixture.files(), files);
        assert_eq!(fixture.index(), index);
        screen.write_refusal = None;
        git(
            &fixture.root,
            &["update-ref", "REBASE_HEAD", &fixture.topic],
        );
        submit(&mut screen);
        assert!(view(&mut screen).contains("operation changed"));
        assert_eq!(fixture.files(), files);
        assert_eq!(fixture.index(), index);
        assert!(fixture.root.join(".git/rebase-merge").exists());
    }
    for code in ['C', 'X', 'S'] {
        let fixture = Fixture::new(History::Conflict);
        let mut screen = open(&fixture);
        start_rebase(&mut screen);
        if code == 'C' {
            resolve_rebase_file(&mut screen, &fixture);
        }
        std::fs::write(
            fixture.root.join("later.txt"),
            "keep unrelated staged bytes\n",
        )
        .unwrap();
        git(&fixture.root, &["add", "later.txt"]);
        let files = fixture.files();
        let index = fixture.index();
        key(&mut screen, KeyCode::Char(code));
        for (width, height) in SIZES {
            let rendered = text(&screen.frame(width, height));
            assert!(
                rendered.contains("Unrelated changes") && rendered.contains("later.txt"),
                "{rendered}"
            );
        }
        submit(&mut screen);
        assert!(
            view(&mut screen).contains("would discard"),
            "{}",
            view(&mut screen)
        );
        assert_eq!(fixture.files(), files);
        assert_eq!(fixture.index(), index);
        assert!(fixture.root.join(".git/rebase-merge").exists());
    }
}

#[test]
fn external_detached_rebase_confirmation_does_not_invent_a_branch() {
    let fixture = Fixture::new(History::Conflict);
    git(&fixture.root, &["checkout", "--detach", &fixture.before]);
    assert!(!output(&fixture.root, &["rebase", TOPIC]).status.success());
    let mut screen = open(&fixture);
    key(&mut screen, KeyCode::Char('X'));
    for (width, height) in SIZES {
        let rendered = text(&screen.frame(width, height));
        assert!(
            rendered.contains("Return to:") && rendered.contains("detached HEAD"),
            "{rendered}"
        );
        assert!(!rendered.contains("Return to branch"), "{rendered}");
    }
    submit(&mut screen);
    finished(&mut screen);
    assert!(!fixture.root.join(".git/rebase-merge").exists());
    assert!(
        !output(&fixture.root, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success()
    );
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
    assert_eq!(git(&fixture.root, &["rev-parse", "main"]), fixture.before);
    assert_eq!(git(&fixture.root, &["rev-parse", TOPIC]), fixture.topic);
}

#[test]
fn operation_confirmation_shows_destination_and_current_blockers_without_form_scaffolding() {
    let fixture = Fixture::new(History::Conflict);
    let mut screen = open(&fixture);
    start_rebase(&mut screen);
    std::fs::write(fixture.root.join("draft.txt"), "untracked draft\n").unwrap();
    key(&mut screen, KeyCode::Char('X'));
    let files = fixture.files();
    let index = fixture.index();
    for (width, height) in SIZES {
        let rendered = text(&screen.frame(width, height));
        for expected in [
            "Return to branch",
            "main",
            "Restore commit",
            &fixture.before[..8],
            "Unrelated changes",
            "draft.txt",
            "Retry abort",
            "Enter cancel",
        ] {
            assert!(rendered.contains(expected), "{expected}: {rendered}");
        }
        for clutter in [
            "read only",
            "Paused operation",
            "Effect",
            "refuse unrelated",
            "3/3",
        ] {
            assert!(!rendered.contains(clutter), "{clutter}: {rendered}");
        }
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.expect("modal action cursor");
        // The focused action is the shared filled button chip (R16 #22).
        assert_eq!(
            frame.grid.get(x, y).unwrap().style,
            dust::ui::Style::Git {
                role: dust::ui::GitRole::OnFocus,
                surface: dust::ui::GitSurface::ChipActive,
                emphasis: dust::ui::Emphasis::Bold,
            }
        );
        key(&mut screen, KeyCode::Tab);
        assert!(text(&screen.frame(width, height)).contains("Enter retry abort"));
        key(&mut screen, KeyCode::Tab);
    }
    // Known blockers focus Cancel rather than inviting a refused action.
    key(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("Abort rebase?"));
    assert_eq!(fixture.files(), files);
    assert_eq!(fixture.index(), index);
    assert!(fixture.root.join(".git/rebase-merge").exists());
}

#[test]
fn rebase_confirmation_refuses_changed_start_onto_or_branch_metadata() {
    for code in ['C', 'X', 'S'] {
        for (name, replacement) in [
            ("orig-head", "0000000000000000000000000000000000000001\n"),
            ("onto", "0000000000000000000000000000000000000002\n"),
            ("head-name", "refs/heads/unexpected\n"),
        ] {
            let fixture = Fixture::new(History::Conflict);
            let mut screen = open(&fixture);
            start_rebase(&mut screen);
            wait(&mut screen, |screen| view(screen).contains("S skip"));
            if code == 'C' {
                resolve_rebase_file(&mut screen, &fixture);
            }
            key(&mut screen, KeyCode::Char(code));
            let path = fixture.root.join(".git/rebase-merge").join(name);
            std::fs::write(&path, replacement).unwrap();
            let files = fixture.files();
            let index = fixture.index();
            let head = git(&fixture.root, &["rev-parse", "HEAD"]);
            let refs = git(&fixture.root, &["show-ref"]);
            submit(&mut screen);
            assert!(
                view(&mut screen).contains("operation changed"),
                "{}",
                view(&mut screen)
            );
            assert_eq!(fixture.files(), files);
            assert_eq!(fixture.index(), index);
            assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), head);
            assert_eq!(git(&fixture.root, &["show-ref"]), refs);
            assert_eq!(std::fs::read(&path).unwrap(), replacement.as_bytes());
            assert!(fixture.root.join(".git/rebase-merge").exists());
        }
    }
}

#[test]
fn git_jobs_preserve_private_user_configuration_for_rebase_and_commit() {
    const CHILD: &str = "DUST_GIT_ENV_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("gitconfig");
        std::fs::write(
            &config,
            "[user]\n name = Private Config User\n email = private@example.invalid\n",
        )
        .unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git_jobs_preserve_private_user_configuration_for_rebase_and_commit",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        return;
    }
    let fixture = Fixture::new(History::Diverged);
    git(&fixture.root, &["config", "--unset", "user.name"]);
    git(&fixture.root, &["config", "--unset", "user.email"]);
    let mut screen = open(&fixture);
    start_rebase(&mut screen);
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%P", "HEAD"]),
        fixture.topic
    );
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%cn%n%ce", "HEAD"]),
        "Private Config User\nprivate@example.invalid"
    );
    std::fs::write(fixture.root.join("working.txt"), "new staged bytes\n").unwrap();
    git(&fixture.root, &["add", "working.txt"]);
    key(&mut screen, KeyCode::Char('r'));
    finished(&mut screen);
    key(&mut screen, KeyCode::Char('2'));
    wait(&mut screen, |screen| view(screen).contains("working.txt"));
    key(&mut screen, KeyCode::Char('c'));
    assert!(
        view(&mut screen).contains("Commit to main"),
        "{}",
        view(&mut screen)
    );
    screen.event(UiEvent::Paste("User configured commit".into()));
    submit(&mut screen);
    finished(&mut screen);
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%s", "HEAD"]),
        "User configured commit"
    );
    assert_eq!(
        git(&fixture.root, &["show", "-s", "--format=%an%n%ae", "HEAD"]),
        "Private Config User\nprivate@example.invalid"
    );
    assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
}

/// The head label lives in the top bar (row 0); while an operation is paused the
/// banner on row 1 repeats it in full even where the top bar ellipsizes it.
fn persistent_head(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    let banner = frame.grid.row_text(1);
    let banner = if banner.contains('⟳') {
        banner
    } else {
        String::new()
    };
    format!("{}\n{banner}", frame.grid.row_text(0))
}

#[test]
fn paused_rebase_header_uses_recorded_branch_but_does_not_change_actual_detached_head() {
    for apply in [false, true] {
        let fixture = Fixture::new(History::Conflict);
        let args = if apply {
            vec!["rebase", "--apply", TOPIC]
        } else {
            vec!["rebase", TOPIC]
        };
        assert!(!output(&fixture.root, &args).status.success());
        let backend = if apply {
            "rebase-apply"
        } else {
            "rebase-merge"
        };
        let metadata = fixture.root.join(".git").join(backend).join("head-name");
        assert_eq!(
            std::fs::read_to_string(&metadata).unwrap(),
            "refs/heads/main\n"
        );
        let head = git(&fixture.root, &["rev-parse", "HEAD"]);
        let index = std::fs::read(fixture.root.join(".git/index")).unwrap();
        let files = fixture.files();
        let refs_before = git(&fixture.root, &["show-ref"]);
        let mut screen = open(&fixture);
        for (width, height) in SIZES {
            let header = persistent_head(&mut screen, width, height);
            assert!(
                header.contains("rebase · main"),
                "{width}x{height}: {header}"
            );
            assert!(!header.contains("detached"), "{header}");
        }
        assert!(
            !output(&fixture.root, &["symbolic-ref", "-q", "HEAD"])
                .status
                .success()
        );
        assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&fixture.root, &["show-ref"]), refs_before);
        assert_eq!(
            std::fs::read(fixture.root.join(".git/index")).unwrap(),
            index
        );
        assert_eq!(fixture.files(), files);
        assert_eq!(
            std::fs::read_to_string(metadata).unwrap(),
            "refs/heads/main\n"
        );
        key(&mut screen, KeyCode::Char('X'));
        submit(&mut screen);
        finished(&mut screen);
        for (width, height) in SIZES {
            let header = persistent_head(&mut screen, width, height);
            assert!(
                header.contains("main") && !header.contains("rebase"),
                "{header}"
            );
        }
        assert_eq!(
            git(&fixture.root, &["symbolic-ref", "HEAD"]),
            "refs/heads/main"
        );
    }
}

#[test]
fn externally_detached_rebase_header_stays_distinct_from_recorded_branch_and_normal_detach() {
    let fixture = Fixture::new(History::Conflict);
    git(&fixture.root, &["checkout", "--detach", &fixture.before]);
    let mut plain = open(&fixture);
    for (width, height) in SIZES {
        let header = persistent_head(&mut plain, width, height);
        assert!(header.contains("detached HEAD"), "{header}");
        assert!(
            !header.contains("rebase") && !header.contains("main"),
            "{header}"
        );
    }
    drop(plain);
    assert!(!output(&fixture.root, &["rebase", TOPIC]).status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(".git/rebase-merge/head-name")).unwrap(),
        "detached HEAD\n"
    );
    let index = std::fs::read(fixture.root.join(".git/index")).unwrap();
    let refs_before = git(&fixture.root, &["show-ref"]);
    let mut screen = open(&fixture);
    for (width, height) in SIZES {
        let header = persistent_head(&mut screen, width, height);
        // The full detached label may ellipsize at 64, but never becomes main.
        assert!(
            header.contains("rebase · detach"),
            "{width}x{height}: {header}"
        );
        if width >= 100 {
            assert!(header.contains("rebase · detached HEAD"), "{header}");
        }
        assert!(!header.contains("main"), "{header}");
    }
    assert_eq!(
        std::fs::read(fixture.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(git(&fixture.root, &["show-ref"]), refs_before);
    key(&mut screen, KeyCode::Char('X'));
    submit(&mut screen);
    finished(&mut screen);
    for (width, height) in SIZES {
        let header = persistent_head(&mut screen, width, height);
        assert!(
            header.contains("detached HEAD") && !header.contains("rebase"),
            "{header}"
        );
    }
    assert!(
        !output(&fixture.root, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success()
    );
}

#[test]
fn malformed_rebase_branch_metadata_has_truthful_head_fallback_and_is_not_read_in_rendering() {
    let cases = [
        None,
        Some(Vec::new()),
        Some(b"main\n".to_vec()),
        Some(b"refs/remotes/origin/main\n".to_vec()),
        Some(b"refs/heads/main\nrefs/heads/other\n".to_vec()),
        Some(b"refs/heads/main\n\n".to_vec()),
        Some(b"refs/heads/bad..name\n".to_vec()),
        Some(vec![0xff, 0xfe, b'\n']),
        Some(vec![b'x'; 4097]),
    ];
    for bytes in cases {
        let fixture = Fixture::new(History::Conflict);
        assert!(!output(&fixture.root, &["rebase", TOPIC]).status.success());
        let metadata = fixture.root.join(".git/rebase-merge/head-name");
        if let Some(bytes) = &bytes {
            std::fs::write(&metadata, bytes).unwrap();
        } else {
            std::fs::remove_file(&metadata).unwrap();
        }
        let index = std::fs::read(fixture.root.join(".git/index")).unwrap();
        let refs_before = git(&fixture.root, &["show-ref"]);
        let head = std::fs::read(fixture.root.join(".git/HEAD")).unwrap();
        let files = fixture.files();
        let mut screen = open(&fixture);
        for (width, height) in SIZES {
            let header = persistent_head(&mut screen, width, height);
            assert!(header.contains("rebase · HEAD"), "{bytes:?}: {header}");
            assert!(
                !header.contains("main") && !header.contains("detached"),
                "{header}"
            );
        }
        assert_eq!(std::fs::read(&metadata).ok(), bytes);
        assert_eq!(
            std::fs::read(fixture.root.join(".git/index")).unwrap(),
            index
        );
        assert_eq!(std::fs::read(fixture.root.join(".git/HEAD")).unwrap(), head);
        assert_eq!(git(&fixture.root, &["show-ref"]), refs_before);
        assert_eq!(fixture.files(), files);
        // A draw uses the captured label even if metadata subsequently changes.
        std::fs::write(&metadata, "refs/heads/do-not-read-during-draw\n").unwrap();
        assert!(persistent_head(&mut screen, 190, 48).contains("rebase · HEAD"));
    }
}

// Replay-context acceptance concerns the actual Files/conflict overview, not a
// focused branch filter whose query deliberately owns the shared status row.
fn open_conflict_overview(fixture: &Fixture) -> GitScreen {
    let mut screen = open(fixture);
    key(&mut screen, KeyCode::Char('2'));
    wait(&mut screen, |screen| {
        let frame = screen.frame(190, 48);
        let conflict_selected = frame.cursor.is_some_and(|(x, y, _)| {
            x < 52
                && frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(52)
                    .collect::<String>()
                    .contains("conflict.txt")
        });
        let contents = text(&frame);
        conflict_selected
            && contents.contains("selected topic bytes")
            && contents.contains("current branch bytes")
    });
    screen
}

/// The operation banner (row 1); empty when no operation is paused.
fn operation_summary_at(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let _ = height;
    let banner = screen.frame(width, height).grid.row_text(1);
    if banner.contains('⟳') {
        banner
    } else {
        String::new()
    }
}

#[derive(Debug, PartialEq)]
struct ReplayInspectionState {
    head: Vec<u8>,
    index: Vec<u8>,
    refs: String,
    files: BTreeMap<PathBuf, Vec<u8>>,
    metadata: BTreeMap<PathBuf, Vec<u8>>,
}
fn replay_inspection_state(fixture: &Fixture) -> ReplayInspectionState {
    fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                collect(root, &entry.unwrap().path(), files);
            }
        } else if path.is_file() {
            files.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                std::fs::read(path).unwrap(),
            );
        }
    }
    let git_dir = fixture.root.join(".git");
    let mut metadata = BTreeMap::new();
    for name in ["rebase-merge", "rebase-apply", "REBASE_HEAD", "ORIG_HEAD"] {
        collect(&git_dir, &git_dir.join(name), &mut metadata);
    }
    ReplayInspectionState {
        head: std::fs::read(git_dir.join("HEAD")).unwrap(),
        index: std::fs::read(git_dir.join("index")).unwrap(),
        refs: git(&fixture.root, &["show-ref"]),
        files: fixture.files(),
        metadata,
    }
}

#[test]
fn paused_replay_summary_retains_actual_commit_for_normal_detached_and_apply_rebases_then_clears() {
    for apply in [false, true] {
        for detached in [false, true] {
            let fixture = Fixture::new(History::Conflict);
            if detached {
                git(&fixture.root, &["checkout", "--detach", &fixture.before]);
            }
            let args = if apply {
                vec!["rebase", "--apply", TOPIC]
            } else {
                vec!["rebase", TOPIC]
            };
            assert!(!output(&fixture.root, &args).status.success());
            let replay_subject = git(
                &fixture.root,
                &["show", "-s", "--format=%s", &fixture.before],
            );
            assert_eq!(replay_subject, "Current branch commit");
            let before = replay_inspection_state(&fixture);
            let mut screen = open_conflict_overview(&fixture);
            wait(&mut screen, |screen| {
                operation_summary_at(screen, 190, 48).contains(&fixture.before[..7])
            });
            for (width, height) in SIZES {
                let summary = operation_summary_at(&mut screen, width, height);
                assert!(summary.contains("Rebase"), "{width}x{height}: {summary}");
                // The banner names the replayed commit; narrower banners clip that
                // segment before the right-aligned actions.
                if width >= 190 {
                    assert!(
                        summary.contains(&fixture.before[..7]),
                        "{width}x{height}: {summary}"
                    );
                    assert!(summary.contains("Current branch commit"), "{summary}");
                    assert!(
                        summary.contains(&replay_subject),
                        "{width}x{height}: {summary}"
                    );
                    assert!(
                        summary.contains(&format!("Current {}", &fixture.before[..7])),
                        "{summary}"
                    );
                }
                let frame = screen.frame(width, height);
                assert_conflict_row_and_reader(&frame, width, height);
                assert!(
                    !summary.contains("Selected topic commit"),
                    "must not substitute normal history: {summary}"
                );
                assert!(
                    !summary.contains("pick "),
                    "do not invent a sequencer instruction: {summary}"
                );
            }
            let header = persistent_head(&mut screen, 64, 20);
            assert!(
                header.contains(if detached {
                    "rebase · detached HEAD"
                } else {
                    "rebase · main"
                }),
                "{header}"
            );
            assert_eq!(replay_inspection_state(&fixture), before);
            key(&mut screen, KeyCode::Char('X'));
            submit(&mut screen);
            finished(&mut screen);
            for (width, height) in SIZES {
                let summary = operation_summary_at(&mut screen, width, height);
                assert!(
                    !summary.contains("Current ") && !summary.contains("Current branch commit"),
                    "{summary}"
                );
            }
            assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), fixture.before);
            assert_eq!(git(&fixture.root, &["rev-parse", TOPIC]), fixture.topic);
        }
    }
}

#[test]
fn paused_replay_optional_context_rejects_missing_malformed_or_stale_metadata_without_writes() {
    for apply in [false, true] {
        for case in [
            "missing-marker",
            "malformed-marker",
            "oversized-marker",
            "invalid-utf8-marker",
            "wrong-marker",
            "malformed-ref",
            "wrong-ref",
            "bad-original",
            "bad-onto",
            "stale-pair",
            "stale-head",
        ] {
            let fixture = Fixture::new(History::Conflict);
            let args = if apply {
                vec!["rebase", "--apply", TOPIC]
            } else {
                vec!["rebase", TOPIC]
            };
            assert!(!output(&fixture.root, &args).status.success());
            let directory = fixture.root.join(".git").join(if apply {
                "rebase-apply"
            } else {
                "rebase-merge"
            });
            let marker = directory.join(if apply {
                "original-commit"
            } else {
                "stopped-sha"
            });
            match case {
                "missing-marker" => std::fs::remove_file(&marker).unwrap(),
                "malformed-marker" => std::fs::write(&marker, "not-an-object-id\n").unwrap(),
                "oversized-marker" => std::fs::write(&marker, vec![b'a'; 4097]).unwrap(),
                "invalid-utf8-marker" => std::fs::write(&marker, [0xff, 0xfe]).unwrap(),
                "wrong-marker" => std::fs::write(&marker, format!("{}\n", fixture.topic)).unwrap(),
                "malformed-ref" => {
                    std::fs::write(fixture.root.join(".git/REBASE_HEAD"), "not-an-object-id\n")
                        .unwrap()
                }
                "wrong-ref" => {
                    git(
                        &fixture.root,
                        &["update-ref", "REBASE_HEAD", &fixture.topic],
                    );
                }
                "bad-original" => std::fs::write(directory.join("orig-head"), "bad\n").unwrap(),
                "bad-onto" => std::fs::write(directory.join("onto"), "bad\n").unwrap(),
                "stale-pair" => {
                    std::fs::write(&marker, format!("{}\n", fixture.topic)).unwrap();
                    git(
                        &fixture.root,
                        &["update-ref", "REBASE_HEAD", &fixture.topic],
                    );
                }
                "stale-head" => {
                    git(
                        &fixture.root,
                        &["update-ref", "--no-deref", "HEAD", &fixture.before],
                    );
                }
                _ => unreachable!(),
            }
            let before = replay_inspection_state(&fixture);
            let mut screen = open_conflict_overview(&fixture);
            wait(&mut screen, |screen| {
                operation_summary_at(screen, 190, 48).contains("rebase:")
            });
            for (width, height) in SIZES {
                let summary = operation_summary_at(&mut screen, width, height);
                assert!(
                    !summary.contains("Current ") && !summary.contains("Current branch commit"),
                    "{apply}/{case}: {summary}"
                );
                assert!(persistent_head(&mut screen, width, height).contains("rebase · main"));
            }
            assert_eq!(replay_inspection_state(&fixture), before, "{apply}/{case}");
        }
    }
}

#[test]
fn apply_replay_uses_actual_recorded_commit_without_rebase_head_and_refresh_clears_missing_context()
{
    let fixture = Fixture::new(History::Conflict);
    assert!(
        !output(&fixture.root, &["rebase", "--apply", TOPIC])
            .status
            .success()
    );
    let reference = fixture.root.join(".git/REBASE_HEAD");
    if reference.exists() {
        std::fs::remove_file(reference).unwrap();
    }
    let before = replay_inspection_state(&fixture);
    let mut screen = open_conflict_overview(&fixture);
    wait(&mut screen, |screen| {
        operation_summary_at(screen, 190, 48).contains(&fixture.before[..7])
    });
    assert!(operation_summary_at(&mut screen, 190, 48).contains("Current branch commit"));
    assert_eq!(replay_inspection_state(&fixture), before);
    // Rendering uses the captured context, not filesystem state at a new geometry.
    std::fs::remove_file(fixture.root.join(".git/rebase-apply/original-commit")).unwrap();
    let after_external_change = replay_inspection_state(&fixture);
    assert!(operation_summary_at(&mut screen, 190, 48).contains(&fixture.before[..7]));
    assert_eq!(replay_inspection_state(&fixture), after_external_change);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| {
        !operation_summary_at(screen, 190, 48).contains("Current branch commit")
    });
    assert!(!operation_summary_at(&mut screen, 100, 30).contains("Current "));
    assert_eq!(replay_inspection_state(&fixture), after_external_change);
}
