//! Local reference actions exercised only through native screen input and real Git repositories.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn git_output(root: &Path, args: &[&str]) -> Output {
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
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim_end().into()
}
struct Fixture {
    directory: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Reference Test"]);
        git(&root, &["config", "user.email", "refs@example.invalid"]);
        for name in ["tracked.txt", "staged.txt"] {
            std::fs::write(root.join(name), "baseline\n").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "Baseline"]);
        for name in ["aaa/unrelated", "topic/selected", "topic/sibling"] {
            git(&root, &["branch", name]);
        }
        // The same short name in both namespaces must never retarget a delete.
        git(
            &root,
            &["tag", "-a", "topic/selected", "-m", "Annotated local tag"],
        );
        git(&root, &["tag", "release/keep"]);
        Self { directory, root }
    }
    fn dirty(&self) {
        std::fs::write(self.root.join("tracked.txt"), "keep worktree bytes\n").unwrap();
        std::fs::write(self.root.join("staged.txt"), "keep index bytes\n").unwrap();
        git(&self.root, &["add", "staged.txt"]);
        std::fs::write(self.root.join("staged.txt"), "keep later working bytes\n").unwrap();
        std::fs::write(self.root.join("untracked.txt"), "keep untracked bytes\n").unwrap();
    }
    fn remotes(&self) {
        // A real local fetch, with both ordinary and slash-bearing remote names.
        for name in ["origin", "team/origin"] {
            git(
                &self.root,
                &["remote", "add", name, self.root.to_str().unwrap()],
            );
            git(&self.root, &["fetch", name]);
        }
        git(
            &self.root,
            &[
                "symbolic-ref",
                "refs/remotes/team/origin/HEAD",
                "refs/remotes/team/origin/main",
            ],
        );
    }
    fn unmerged_commit(&self) -> String {
        let tree = git(&self.root, &["rev-parse", "HEAD^{tree}"]);
        git(
            &self.root,
            &["commit-tree", &tree, "-p", "HEAD", "-m", "Unmerged tip"],
        )
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
fn text(screen: &mut GitScreen, value: &str) {
    screen.event(UiEvent::Paste(value.into()));
}
fn replace(screen: &mut GitScreen, value: &str) {
    control(screen, KeyCode::Char('u'));
    text(screen, value);
}
fn submit(screen: &mut GitScreen) {
    control(screen, KeyCode::Enter);
}
fn view_at(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let frame = screen.frame(width, height);
    (0..height)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}
fn view(screen: &mut GitScreen) -> String {
    view_at(screen, 190, 48)
}
fn selected(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    frame.cursor.map_or_else(String::new, |(x, y, _)| {
        assert!(x < 52, "expected sidebar cursor, got {x},{y}");
        frame.grid.row_text(y).chars().take(52).collect()
    })
}
fn wait(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "screen timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// A sheet is open while its footer offers `Esc cancel`.
fn sheet_open(screen: &mut GitScreen) -> bool {
    view(screen).contains("Esc cancel")
}
fn finished(screen: &mut GitScreen) {
    wait(screen, |screen| !screen.busy());
}
fn filter(screen: &mut GitScreen, query: &str) {
    key(screen, KeyCode::Char('/'));
    replace(screen, query);
    key(screen, KeyCode::Enter);
}
fn open(fixture: &Fixture, tag: bool, name: &str) -> GitScreen {
    let mut screen = GitScreen::open(&fixture.root).unwrap();
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, |screen| {
        selected(screen).contains("aaa/unrelated")
    });
    if tag {
        key(&mut screen, KeyCode::Char('L'));
        key(&mut screen, KeyCode::Char('L'));
        wait(&mut screen, |screen| view(screen).contains("release/keep"));
    }
    filter(&mut screen, name);
    assert!(
        selected(&mut screen).contains(name),
        "{}",
        view(&mut screen)
    );
    screen
}
fn refs(root: &Path) -> BTreeMap<String, String> {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .map(|line| {
            let (name, id) = line.split_once(' ').unwrap();
            (name.into(), id.into())
        })
        .collect()
}
#[derive(Debug, PartialEq)]
struct WorktreeState {
    head: Vec<u8>,
    id: String,
    index: Vec<u8>,
    status: String,
    files: Vec<Option<Vec<u8>>>,
}
fn state(root: &Path) -> WorktreeState {
    let repository = git2::Repository::open(root).unwrap();
    WorktreeState {
        head: std::fs::read(repository.path().join("HEAD")).unwrap(),
        id: git(root, &["rev-parse", "HEAD"]),
        index: std::fs::read(repository.path().join("index")).unwrap(),
        status: git(root, &["status", "--porcelain=v1", "--untracked-files=all"]),
        files: ["tracked.txt", "staged.txt", "untracked.txt"]
            .iter()
            .map(|name| std::fs::read(root.join(name)).ok())
            .collect(),
    }
}

#[test]
fn filtered_rename_keeps_the_selected_identity_and_all_dirty_worktree_bytes() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = state(&fixture.root);
    let mut expected = refs(&fixture.root);
    let id = expected.remove("refs/heads/topic/selected").unwrap();
    expected.insert("refs/heads/topic/renamed".into(), id);
    let mut screen = open(&fixture, false, "topic/selected");
    key(&mut screen, KeyCode::Char('e'));
    replace(&mut screen, "topic/renamed");
    // The identity shown in the form is immutable, even to paste and Ctrl+U.
    key(&mut screen, KeyCode::Tab);
    replace(&mut screen, "main");
    assert!(view(&mut screen).contains("refs/heads/topic/selected"));
    submit(&mut screen);
    assert!(screen.busy());
    submit(&mut screen);
    text(&mut screen, "ignored while running");
    key(&mut screen, KeyCode::Escape);
    finished(&mut screen);
    assert_eq!(refs(&fixture.root), expected);
    assert_eq!(state(&fixture.root), before);
    assert!(!view(&mut screen).contains("Rename local branch"));
    filter(&mut screen, "topic/renamed");
    assert!(selected(&mut screen).contains("topic/renamed"));
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("aaa/unrelated"));
}

#[test]
fn rename_refuses_duplicates_and_invalid_names_without_losing_input_then_retries() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open(&fixture, false, "topic/selected");
    key(&mut screen, KeyCode::Char('e'));
    replace(&mut screen, "topic/sibling");
    submit(&mut screen);
    assert!(screen.busy());
    finished(&mut screen);
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let displayed = view_at(&mut screen, width, height);
        // The sheet's one-row status line ellipsizes Git's refusal at 64 columns.
        let refusal = if width < 100 {
            "a branch named 'topic/sibling' already exis"
        } else {
            "already exists"
        };
        for evidence in [
            "Rename local branch",
            "topic/sibling",
            "refs/heads/topic/selected",
            refusal,
        ] {
            assert!(
                displayed.contains(evidence),
                "{width}x{height}: {evidence}: {displayed}"
            );
        }
    }
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
    for invalid in ["bad..name", "-force", "@{-1}", "has space"] {
        replace(&mut screen, invalid);
        submit(&mut screen);
        assert!(!screen.busy());
        let displayed = view(&mut screen);
        assert!(displayed.contains("Choose a valid branch name"));
        assert!(displayed.contains(invalid));
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(state(&fixture.root), before);
    }
    replace(&mut screen, "topic/retried");
    submit(&mut screen);
    finished(&mut screen);
    let mut expected = refs_before;
    let id = expected.remove("refs/heads/topic/selected").unwrap();
    expected.insert("refs/heads/topic/retried".into(), id);
    assert_eq!(refs(&fixture.root), expected);
    assert_eq!(state(&fixture.root), before);
}

#[test]
fn local_branch_and_annotated_tag_deletion_require_confirmation_and_stay_in_their_namespace() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = state(&fixture.root);
    let mut expected = refs(&fixture.root);
    for tag in [false, true] {
        let mut screen = open(&fixture, tag, "topic/selected");
        let namespace = if tag { "tags" } else { "heads" };
        let reference = format!("refs/{namespace}/topic/selected");
        for cancel_button in [false, true] {
            key(&mut screen, KeyCode::Char('d'));
            assert!(view(&mut screen).contains(&reference));
            assert_eq!(
                refs(&fixture.root),
                expected,
                "opening confirmation wrote a ref"
            );
            if cancel_button {
                key(&mut screen, KeyCode::Tab);
                key(&mut screen, KeyCode::Enter);
            } else {
                key(&mut screen, KeyCode::Escape);
            }
            assert!(!sheet_open(&mut screen));
            assert_eq!(refs(&fixture.root), expected);
            assert_eq!(state(&fixture.root), before);
        }
        key(&mut screen, KeyCode::Char('d'));
        // A confirmation starts on its real Submit button, not an editable target.
        key(&mut screen, KeyCode::Enter);
        assert!(screen.busy());
        finished(&mut screen);
        expected.remove(&reference).unwrap();
        assert_eq!(refs(&fixture.root), expected);
        assert_eq!(state(&fixture.root), before);
        wait(&mut screen, |screen| view(screen).contains("No matches"));
    }
}

#[test]
fn tag_write_refusal_keeps_confirmation_and_can_retry_after_a_real_ref_lock_is_released() {
    let fixture = Fixture::new();
    fixture.dirty();
    let before = state(&fixture.root);
    let mut expected = refs(&fixture.root);
    let mut screen = open(&fixture, true, "topic/selected");
    key(&mut screen, KeyCode::Char('d'));
    let lock = fixture.root.join(".git/refs/tags/topic/selected.lock");
    std::fs::write(&lock, "another writer owns this lock\n").unwrap();
    submit(&mut screen);
    assert!(screen.busy());
    finished(&mut screen);
    let displayed = view(&mut screen);
    assert!(displayed.contains("Delete local tag?"));
    assert!(displayed.contains("refs/tags/topic/selected"));
    assert!(displayed.contains("cannot lock ref"), "{displayed}");
    assert_eq!(refs(&fixture.root), expected);
    assert_eq!(state(&fixture.root), before);
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "another writer owns this lock\n"
    );
    std::fs::remove_file(lock).unwrap();
    submit(&mut screen);
    finished(&mut screen);
    expected.remove("refs/tags/topic/selected").unwrap();
    assert_eq!(refs(&fixture.root), expected);
    assert_eq!(state(&fixture.root), before);
    assert!(!view(&mut screen).contains("Delete local tag?"));
}

#[test]
fn normal_branch_deletion_refuses_unmerged_current_and_newly_linked_checkouts() {
    let fixture = Fixture::new();
    let unmerged = fixture.unmerged_commit();
    git(&fixture.root, &["branch", "topic/unmerged", &unmerged]);
    git(&fixture.root, &["branch", "topic/linked"]);
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    for name in ["topic/unmerged", "main", "topic/linked"] {
        let mut screen = open(&fixture, false, name);
        key(&mut screen, KeyCode::Char('d'));
        // Add the checkout AFTER the form captured its target: the Git job must
        // check live worktrees, not trust the earlier sidebar inventory.
        let linked = fixture.directory.path().join("linked checkout");
        let linked_before = if name == "topic/linked" {
            git(
                &fixture.root,
                &["worktree", "add", linked.to_str().unwrap(), name],
            );
            Some(state(&linked))
        } else {
            None
        };
        submit(&mut screen);
        assert!(screen.busy());
        finished(&mut screen);
        let displayed = view(&mut screen);
        assert!(displayed.contains("Delete local branch?"));
        assert!(displayed.contains(&format!("refs/heads/{name}")));
        if name == "topic/unmerged" {
            assert!(displayed.contains("not fully merged"), "{displayed}");
        } else {
            assert!(
                displayed.contains("checked out") || displayed.contains("used by worktree"),
                "{displayed}"
            );
        }
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(state(&fixture.root), before);
        if let Some(linked_before) = linked_before {
            assert_eq!(state(&linked), linked_before);
        }
        key(&mut screen, KeyCode::Escape);
        assert!(selected(&mut screen).contains(name));
    }
    let mut screen = open(&fixture, false, "topic/selected");
    key(&mut screen, KeyCode::Char('u'));
    assert!(view(&mut screen).contains("No remote-tracking branches; fetch first"));
    assert!(!sheet_open(&mut screen));
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
}

#[test]
fn upstream_uses_actual_choices_and_the_selected_branch_even_with_a_slash_remote_name() {
    let fixture = Fixture::new();
    fixture.remotes();
    // A local ref with the same short name cannot win Git's upstream resolution.
    git(&fixture.root, &["branch", "team/origin/topic/selected"]);
    git(
        &fixture.root,
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    git(
        &fixture.root,
        &[
            "branch",
            "--set-upstream-to=origin/topic/sibling",
            "topic/selected",
        ],
    );
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open(&fixture, false, "topic/selected");
    // Filtering may match the collision too: the first result remains the exact local target.
    assert!(selected(&mut screen).contains("topic/selected"));
    if selected(&mut screen).contains("team/origin/") {
        key(&mut screen, KeyCode::Down);
    }
    assert!(
        !selected(&mut screen).contains("team/origin/"),
        "{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Char('u'));
    assert!(
        view(&mut screen).contains("refs/heads/topic/selected"),
        "{}",
        view(&mut screen)
    );
    assert!(view(&mut screen).contains("origin/topic/sibling"));
    replace(&mut screen, "invented/main");
    submit(&mut screen);
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("Choose a valid Upstream with Up/Down"));
    // Use the actual choice control, not a production-only test seam.
    let desired = "team/origin/topic/selected";
    let mut found = false;
    for _ in 0..32 {
        key(&mut screen, KeyCode::Down);
        let frame = screen.frame(190, 48);
        let (x, y, _) = frame.cursor.unwrap();
        // The upstream is a picker (DESIGN 5.x `u`): the native cursor sits on the chosen
        // option's mark, so the choice is the text after the cursor. The sidebar can
        // contain a colliding branch name on this same screen row, before the sheet.
        let chosen = frame
            .grid
            .row_text(y)
            .chars()
            .skip(usize::from(x))
            .collect::<String>();
        let chosen = chosen.trim_start_matches(['●', ' ']).split("  ").next().unwrap_or("").trim().to_owned();
        assert!(
            chosen != "team/origin/HEAD",
            "symbolic remote HEAD is not a branch choice"
        );
        if chosen == desired {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "actual slash-remote upstream not offered: {}",
        view(&mut screen)
    );
    submit(&mut screen);
    assert!(screen.busy());
    finished(&mut screen);
    assert_eq!(
        git(
            &fixture.root,
            &["config", "--get", "branch.topic/selected.remote"]
        ),
        "team/origin",
        "{}\n{}",
        view(&mut screen),
        git(
            &fixture.root,
            &["config", "--get-regexp", "branch\\..*\\.remote"]
        )
    );
    assert_eq!(
        git(
            &fixture.root,
            &["config", "--get", "branch.topic/selected.merge"]
        ),
        "refs/heads/topic/selected"
    );
    assert_eq!(
        git(&fixture.root, &["config", "--get", "branch.main.remote"]),
        "origin"
    );
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
    // The refreshed selected branch remembers its real configured upstream.
    key(&mut screen, KeyCode::Char('u'));
    assert!(view(&mut screen).contains(desired));
    key(&mut screen, KeyCode::Escape);
}

#[test]
fn every_reference_submit_rechecks_host_authorization_and_retains_target_and_input() {
    let fixture = Fixture::new();
    fixture.remotes();
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let config_before = std::fs::read(fixture.root.join(".git/config")).unwrap();
    for (tag, action) in [(false, 'e'), (false, 'u'), (false, 'd'), (true, 'd')] {
        let mut screen = open(&fixture, tag, "topic/selected");
        key(&mut screen, KeyCode::Char(action));
        let input = match action {
            'e' => Some("topic/unauthorized"),
            'u' => Some("team/origin/topic/selected"),
            _ => None,
        };
        if let Some(input) = input {
            replace(&mut screen, input);
        }
        screen.write_refusal = Some("Host refuses reference writes".into());
        submit(&mut screen);
        assert!(!screen.busy());
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let displayed = view_at(&mut screen, width, height);
            assert!(
                displayed.contains("Host refuses reference writes"),
                "{displayed}"
            );
            let reference = if tag {
                "refs/tags/topic/selected"
            } else {
                "refs/heads/topic/selected"
            };
            assert!(displayed.contains(reference), "{displayed}");
            if let Some(input) = input {
                assert!(displayed.contains(input), "{displayed}");
            }
        }
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(state(&fixture.root), before);
        assert_eq!(
            std::fs::read(fixture.root.join(".git/config")).unwrap(),
            config_before
        );
        screen.write_refusal = None;
        key(&mut screen, KeyCode::Escape);
        assert!(!sheet_open(&mut screen));
    }
}

#[test]
fn moved_or_deleted_targets_and_disappearing_upstreams_are_not_silently_retargeted() {
    for (tag, action) in [(false, 'e'), (false, 'd'), (false, 'u'), (true, 'd')] {
        let fixture = Fixture::new();
        fixture.remotes();
        fixture.dirty();
        let before = state(&fixture.root);
        let mut screen = open(&fixture, tag, "topic/selected");
        key(&mut screen, KeyCode::Char(action));
        if action == 'e' {
            replace(&mut screen, "topic/must-not-exist");
        }
        let reference = if tag {
            "refs/tags/topic/selected"
        } else {
            "refs/heads/topic/selected"
        };
        let replacement = fixture.unmerged_commit();
        git(&fixture.root, &["update-ref", reference, &replacement]);
        let refs_after_external_change = refs(&fixture.root);
        submit(&mut screen);
        assert!(!screen.busy());
        assert!(view(&mut screen).contains("Selected local ref changed"));
        assert!(view(&mut screen).contains(reference));
        assert_eq!(refs(&fixture.root), refs_after_external_change);
        assert_eq!(state(&fixture.root), before);
    }
    let fixture = Fixture::new();
    fixture.remotes();
    fixture.dirty();
    let before = state(&fixture.root);
    let mut screen = open(&fixture, false, "topic/selected");
    key(&mut screen, KeyCode::Char('u'));
    replace(&mut screen, "team/origin/topic/selected");
    git(
        &fixture.root,
        &[
            "update-ref",
            "-d",
            "refs/remotes/team/origin/topic/selected",
        ],
    );
    let refs_after_external_change = refs(&fixture.root);
    submit(&mut screen);
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("Remote-tracking ref disappeared"));
    assert!(view(&mut screen).contains("team/origin/topic/selected"));
    assert_eq!(refs(&fixture.root), refs_after_external_change);
    assert_eq!(state(&fixture.root), before);
}

#[test]
fn contextual_reference_footer_and_help_fit_all_supported_geometries_without_moving_selection() {
    let fixture = Fixture::new();
    for tag in [false, true] {
        let mut screen = open(&fixture, tag, "topic/selected");
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let before = screen.frame(width, height);
            // The footer is the last row and holds at most five actions; rename
            // lives in the help sheet below.
            let footer = before.grid.row_text(height - 1);
            let actions: &[&str] = if tag {
                &["d delete", "inspect", "keys"]
            } else {
                &["merge", "rebase", "delete", "keys"]
            };
            for &action in actions {
                assert!(
                    footer.contains(action),
                    "{width}x{height}: {action}: {footer}"
                );
            }
            key(&mut screen, KeyCode::Char('?'));
            let displayed = view_at(&mut screen, width, height);
            let actions: &[&str] = if tag {
                &["Keys · Tags", "Delete local tag"]
            } else {
                &[
                    "Keys · Branches",
                    "Rename",
                    "Set upstream",
                    "Delete (confirm)",
                    "Checkout",
                ]
            };
            for &action in actions {
                assert!(
                    displayed.contains(action),
                    "{width}x{height}: {action}: {displayed}"
                );
            }
            key(&mut screen, KeyCode::Escape);
            let after = screen.frame(width, height);
            assert_eq!(before.cursor, after.cursor);
            for row in 0..height {
                assert_eq!(before.grid.row_text(row), after.grid.row_text(row));
            }
        }
    }
}

// The reader's left column: beside the rail at 100+ columns; narrow layouts show one
// surface at a time, so the reader then fills the body (see `with_reader`).
fn reader_left(width: u16, height: u16) -> u16 {
    if width < 100 {
        0
    } else if width >= 160 && height >= 40 {
        (25 + width / 7).clamp(44, 60) + 1
    } else {
        (20 + width / 10).clamp(28, 36) + 1
    }
}
// Only inspect the reader interior, not matching names in the rail.
fn preview_at(screen: &mut GitScreen, width: u16, height: u16) -> Vec<String> {
    let left = reader_left(width, height);
    let frame = screen.frame(width, height);
    (2..height - 2)
        .map(|row| {
            frame
                .grid
                .row_text(row)
                .chars()
                .skip(usize::from(left))
                .collect()
        })
        .collect()
}
/// Narrow layouts paint only the focused surface: show the reader for one read.
fn with_reader<T>(
    screen: &mut GitScreen,
    width: u16,
    read: impl FnOnce(&mut GitScreen) -> T,
) -> T {
    if width >= 100 {
        return read(screen);
    }
    key(screen, KeyCode::Char('0'));
    let value = read(screen);
    key(screen, KeyCode::Escape);
    value
}

#[test]
fn shared_tip_preview_identifies_head_current_and_selected_without_dates_or_unrelated_refs() {
    let fixture = Fixture::new();
    git(&fixture.root, &["branch", "feature/source-search"]);
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let id = git(&fixture.root, &["rev-parse", "HEAD"]);
    let date = git(&fixture.root, &["show", "-s", "--format=%as", "HEAD"]);
    let mut screen = open(&fixture, false, "feature/source-search");
    wait(&mut screen, |screen| {
        preview_at(screen, 190, 48)
            .iter()
            .any(|line| line.contains(&id[..7]) && line.contains("Baseline"))
            && ref_preview_header(screen, 190, 48).contains("feature/source-search")
    });
    for (width, height) in [(190, 48), (100, 30), (64, 20)] {
        let (rows, header) = with_reader(&mut screen, width, |screen| {
            (
                preview_at(screen, width, height),
                ref_preview_header(screen, width, height),
            )
        });
        let tip = rows.iter().find(|line| line.contains(&id[..7])).unwrap();
        assert!(tip.contains("Baseline"), "{width}x{height}: {tip}");
        assert!(
            header.contains("HEAD→main") && header.contains('='),
            "{header}"
        );
        if width >= 100 {
            assert!(header.contains("feature/source-search"), "{header}");
        } else {
            assert!(header.contains("selected"), "{header}");
        }
        for excluded in [
            &date[..],
            "HEAD",
            "feature/source-search",
            "aaa/unrelated",
            "topic/sibling",
            "tag:",
        ] {
            assert!(!tip.contains(excluded), "{excluded}: {tip}");
        }
    }
    assert!(selected(&mut screen).contains("feature/source-search"));
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
}

#[test]
fn selected_commit_decorations_use_the_existing_metadata_row_and_keep_current_distinct() {
    let fixture = Fixture::new();
    git(&fixture.root, &["branch", "feature/source-search"]);
    fixture.dirty();
    let id = git(&fixture.root, &["rev-parse", "HEAD"]);
    let author = git(&fixture.root, &["show", "-s", "--format=%an", "HEAD"]);
    let date = git(&fixture.root, &["show", "-s", "--format=%as", "HEAD"]);
    let files = git(&fixture.root, &["ls-tree", "--name-only", "HEAD"])
        .lines()
        .count();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open(&fixture, false, "feature/source-search");
    // Inspect the selected branch's history, then its commit. HEAD stays on main.
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |screen| {
        let rows = preview_at(screen, 190, 48);
        // Ref context can arrive with status before CommitInfo and its patch.
        // Require both reads, not just already-factual loading-state decorations.
        rows.iter()
            .any(|line| line.contains("HEAD→main") && line.contains("feature/source-search"))
            && rows.first().is_some_and(|line| line.contains("Baseline"))
            && rows.iter().skip(2).any(|line| line.contains("baseline"))
    });
    // Narrow layouts show the reader only while it is focused.
    let rows = with_reader(&mut screen, 64, |screen| preview_at(screen, 64, 20));
    assert!(
        rows.iter().any(|line| line.contains("HEAD→main")),
        "{rows:?}"
    );
    for (width, height) in [(190, 48), (100, 30)] {
        let rows = preview_at(&mut screen, width, height);
        let meta = rows.iter().find(|line| line.contains("HEAD→main")).unwrap();
        // Row 0 is the subject; row 1 is `hash  ref chips  author · date · files`.
        let text = meta.strip_prefix("│ ").unwrap_or(meta);
        assert!(
            text.starts_with(&format!("{} ", &id[..7]))
                && text.split_whitespace().nth(1) == Some("HEAD→main"),
            "the first relevant ref must be actual HEAD/current: {meta}"
        );
        if width == 190 {
            assert!(
                meta.contains("feature/source-search"),
                "{width}x{height}: {meta}"
            );
        }
        assert!(meta.contains(&author), "{width}x{height}: {meta}");
        assert!(meta.contains(&date), "{width}x{height}: {meta}");
        assert!(
            meta.contains(&format!("{files} files")),
            "{width}x{height}: {meta}"
        );
        assert!(!meta.contains("HEAD→feature/source-search"), "{meta}");
        // Decorations must share the existing metadata row, not push the patch down.
        let meta_row = rows
            .iter()
            .position(|line| line.contains("HEAD→main"))
            .unwrap();
        assert_eq!(meta_row, 1, "{rows:?}");
        assert!(
            rows.iter().any(|line| line.contains("baseline")),
            "{rows:?}"
        );
    }
    assert_eq!(
        git(&fixture.root, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
}

#[test]
fn same_commit_refresh_updates_decorations_after_external_head_changes_without_reloading_identity()
{
    let fixture = Fixture::new();
    git(&fixture.root, &["branch", "feature/source-search"]);
    fixture.dirty();
    let mut screen = open(&fixture, false, "feature/source-search");
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |screen| {
        let rows = preview_at(screen, 190, 48);
        rows.iter().any(|line| line.contains("HEAD→main"))
            && rows.iter().any(|line| line.contains("baseline"))
    });
    let id = git(&fixture.root, &["rev-parse", "HEAD"]);
    let refs_before = refs(&fixture.root);
    for branch in [None, Some("feature/source-search")] {
        // The metadata row shows decorations as separate chips: `HEAD→branch`, or a
        // bare `HEAD` chip beside the branch when HEAD is detached.
        let detached = branch.is_none();
        if let Some(branch) = branch {
            git(&fixture.root, &["checkout", branch]);
        } else {
            git(&fixture.root, &["checkout", "--detach", &id]);
        }
        let expected = move |line: &String| {
            let words = line.split_whitespace().collect::<Vec<_>>();
            if detached {
                words.contains(&"HEAD")
                    && words.contains(&"feature/source-search")
                    && !line.contains("HEAD→")
            } else {
                line.contains("HEAD→feature/source-search")
            }
        };
        let before = state(&fixture.root);
        key(&mut screen, KeyCode::Char('r'));
        wait(&mut screen, |screen| {
            preview_at(screen, 190, 48).iter().any(expected)
        });
        let rows = preview_at(&mut screen, 190, 48);
        let metadata = rows.iter().find(|line| expected(line)).unwrap();
        assert!(!metadata.contains("HEAD→main"), "{metadata}");
        assert!(metadata.contains(&id[..7]), "{metadata}");
        assert_eq!(refs(&fixture.root), refs_before);
        assert_eq!(state(&fixture.root), before);
    }
}

fn ref_preview_header(screen: &mut GitScreen, width: u16, height: u16) -> String {
    let left = reader_left(width, height);
    screen
        .frame(width, height)
        .grid
        .row_text(1)
        .chars()
        .skip(usize::from(left))
        .collect()
}

fn open_long_named_ref(fixture: &Fixture, name: &str) -> GitScreen {
    let mut screen = GitScreen::open(&fixture.root).unwrap();
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, |screen| {
        selected(screen).contains("aaa/unrelated")
    });
    filter(&mut screen, name);
    // The exact full query chooses the ref; a 52-cell sidebar cannot display it
    // in full. The caller verifies its unique returned OID, not a clipped name.
    screen
}

#[test]
fn branch_log_attributes_each_commit_without_reflow_or_repository_changes() {
    let fixture = Fixture::new();
    let tree = git(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
    let make_commit = |parent: &str, author: &str, date: &str, subject: &str| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&fixture.root)
            .args(["commit-tree", &tree, "-p", parent, "-m", subject])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", author)
            .env("GIT_AUTHOR_EMAIL", "attribution@example.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let older = make_commit(
        "HEAD",
        "Older Author",
        "2025-06-08T12:00:00Z",
        "Older literal subject 中文",
    );
    let tip = make_commit(
        &older,
        "Generated Recovery QA",
        "2026-11-12T12:00:00Z",
        "New literal subject (HEAD -> fake)",
    );
    git(&fixture.root, &["branch", "attribution/selected", &tip]);
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_long_named_ref(&fixture, "attribution/selected");
    wait(&mut screen, |screen| {
        preview_at(screen, 190, 48)
            .iter()
            .any(|line| line.contains("Generated Recovery QA · 2026-11-12"))
    });
    for (width, height) in [(190, 48), (100, 30), (64, 20)] {
        let rows = with_reader(&mut screen, width, |screen| preview_at(screen, width, height));
        for (id, subject, metadata) in [
            (
                &tip,
                "New literal subject (HEAD -> fake)",
                "Generated Recovery QA · 2026-11-12",
            ),
            (
                &older,
                "Older literal subject 中文",
                "Older Author · 2025-06-08",
            ),
        ] {
            let row = rows
                .iter()
                .position(|line| line.contains(&id[..7]))
                .unwrap();
            if width >= 100 {
                assert!(rows[row].contains(subject), "{}", rows[row]);
            }
            assert!(
                rows[row + 1].contains(metadata),
                "{width}: {}",
                rows[row + 1]
            );
        }
    }
    control(&mut screen, KeyCode::Char('l'));
    let frame = screen.frame(190, 48);
    let (x, y, _) = frame.cursor.unwrap();
    assert!(frame.grid.row_text(y).contains(&tip[..7]));
    key(&mut screen, KeyCode::Char('j'));
    let metadata_frame = screen.frame(190, 48);
    let (_, metadata_y, _) = metadata_frame.cursor.unwrap();
    assert_eq!(metadata_y, y + 1);
    assert!(
        metadata_frame
            .grid
            .row_text(metadata_y)
            .contains("Generated Recovery QA · 2026-11-12")
    );
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        screen.frame(width, height);
    }
    assert_eq!(screen.frame(190, 48).cursor, metadata_frame.cursor);
    key(&mut screen, KeyCode::Char('k'));
    assert_eq!(
        screen.frame(190, 48).cursor,
        Some((x, y, crossterm::cursor::SetCursorStyle::SteadyBlock))
    );
    assert_eq!(state(&fixture.root), before);
    assert_eq!(refs(&fixture.root), refs_before);
}

#[test]
fn ref_context_keeps_subject_visible_with_long_names_and_unshared_or_detached_tips() {
    let fixture = Fixture::new();
    let selected_name = "feature/an-extraordinarily-long-selected-name-with-punctuation,(v2)";
    let current_name = "current/an-extraordinarily-long-current-branch-name";
    let tree = git(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
    let subject = "Needle branch subject remains readable";
    let tip = git(
        &fixture.root,
        &["commit-tree", &tree, "-p", "HEAD", "-m", subject],
    );
    git(&fixture.root, &["branch", selected_name, &tip]);
    git(&fixture.root, &["branch", "-m", "main", current_name]);
    // Same short name in another namespace must not contaminate selected-tip context.
    git(&fixture.root, &["tag", selected_name]);
    fixture.dirty();
    let before = state(&fixture.root);
    let refs_before = refs(&fixture.root);
    let mut screen = open_long_named_ref(&fixture, selected_name);
    wait(&mut screen, |screen| {
        preview_at(screen, 190, 48)
            .iter()
            .any(|line| line.contains(&tip[..7]) && line.contains(subject))
            && ref_preview_header(screen, 190, 48).contains("HEAD")
    });
    for (width, height) in [(190, 48), (100, 30), (64, 20)] {
        let (rows, header) = with_reader(&mut screen, width, |screen| {
            (
                preview_at(screen, width, height),
                ref_preview_header(screen, width, height),
            )
        });
        let first = rows.iter().find(|line| line.contains(&tip[..7])).unwrap();
        assert!(first.contains("Needle branch"), "{width}x{height}: {first}");
        if width >= 100 {
            assert!(first.contains(subject), "{first}");
        }
        assert!(
            !first.contains("HEAD") && !first.contains("feature/"),
            "{first}"
        );
        assert!(
            !header.contains('='),
            "unshared refs must not imply equal tips: {header}"
        );
        assert!(header.contains("HEAD"), "{header}");
    }
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
    // Resize-only frames must keep the prepared row's native cursor on its subject.
    control(&mut screen, KeyCode::Char('l'));
    let before_frame = screen.frame(190, 48);
    let before_cursor = before_frame.cursor;
    let (_, row, _) = before_cursor.expect("native log-row cursor");
    assert!(before_frame.grid.row_text(row).contains(subject));
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let _ = screen.frame(width, height);
    }
    assert_eq!(screen.frame(190, 48).cursor, before_cursor);
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), before);
    drop(screen);
    git(&fixture.root, &["checkout", "--detach", &tip]);
    let detached_before = state(&fixture.root);
    let mut detached = open_long_named_ref(&fixture, selected_name);
    wait(&mut detached, |screen| {
        ref_preview_header(screen, 190, 48).contains("HEAD")
    });
    for (width, height) in [(190, 48), (100, 30), (64, 20)] {
        let header = with_reader(&mut detached, width, |screen| {
            ref_preview_header(screen, width, height)
        });
        assert!(header.contains("HEAD") && header.contains('='), "{header}");
        assert!(
            !header.contains("HEAD→"),
            "detached HEAD must not invent a branch: {header}"
        );
    }
    assert_eq!(refs(&fixture.root), refs_before);
    assert_eq!(state(&fixture.root), detached_before);
}

