//! Real index assertions for the native Git screen's sidebar and diff selections.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim_end().into()
}
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", "Workflow Test"]);
    git(root, &["config", "user.email", "workflow@example.invalid"]);
    for (path, text) in files {
        write(root, path, text);
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Baseline"]);
    directory
}
fn key(screen: &mut GitScreen, code: KeyCode) -> ScreenOutcome {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    })
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
fn text(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(screen: &mut GitScreen, mut ready: impl FnMut(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
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
/// The wide layout: its 52-column rail fits reflog subjects the medium rail ellipsizes.
fn wide(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn selected(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    let (_, y, _) = frame.cursor.expect("native cursor");
    frame.grid.row_text(y).chars().take(36).collect()
}
fn choose_file(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('2'));
    for _ in 0..100 {
        if selected(screen).contains(name) {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("missing file {name}: {}", view(screen));
}
fn assert_submodule_status(screen: &mut GitScreen, marker: &str, code: &str) {
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        let (_, y, _) = frame.cursor.expect("submodule cursor");
        assert_eq!(frame.grid.get(3, y).unwrap().symbol, marker);
        let row_style = dust::ui::Style::Git {
            role: dust::ui::GitRole::Text,
            surface: dust::ui::GitSurface::Cursor,
            emphasis: dust::ui::Emphasis::None,
        };
        for (column, expected) in code.chars().enumerate() {
            let cell = frame.grid.get(5 + column as u16, y).unwrap();
            assert_eq!(
                cell.symbol,
                expected.to_string(),
                "{width}×{height}: {code:?}"
            );
            assert_eq!(cell.style, row_style, "{width}×{height}: {code:?}");
        }
        assert_eq!(frame.grid.get(7, y).unwrap().symbol, " ");
        assert_eq!(
            frame.grid.get(8, y).unwrap().symbol,
            "v",
            "filename must follow the marker and XY at every width"
        );
        // Sub-tabs sit on the Changes pane's first interior row.
        let heading = frame.grid.row_text(2);
        // The active sub-tab (or the pane title that follows it) must name the view.
        assert!(
            heading.contains("Changes")
                && (heading.contains("Submodules")
                    || frame.grid.row_text(1).contains("Submodules")),
            "{width}×{height}: {heading}"
        );
        let active = (0..width)
            .filter(|&x| {
                frame.grid.get(x, 2).is_some_and(|cell| {
                    cell.style
                        == dust::ui::Style::Git {
                            role: dust::ui::GitRole::OnFocus,
                            surface: dust::ui::GitSurface::ChipActive,
                            emphasis: dust::ui::Emphasis::Bold,
                        }
                })
            })
            .map(|x| frame.grid.get(x, 2).unwrap().symbol.clone())
            .collect::<String>();
        assert!(active.contains("Sub"), "active file tab: {active:?}");
        let footer = frame.grid.row_text(height - 1);
        for action in ["s", "pointer", "Enter", "keys"] {
            assert!(
                footer.contains(action),
                "{width}×{height}: missing {action}: {footer}"
            );
        }
    }
}
/// The Changes pane lists paths flat by default; `t` switches to the directory tree.
fn tree(screen: &mut GitScreen) {
    key(screen, KeyCode::Char('t'));
}
fn filter(screen: &mut GitScreen, query: &str) {
    key(screen, KeyCode::Char('/'));
    text(screen, query);
    key(screen, KeyCode::Enter);
}
fn code_cursor(screen: &mut GitScreen, needle: &str) {
    for _ in 0..250 {
        let frame = screen.frame(120, 40);
        let (x, y, _) = frame.cursor.expect("native diff cursor");
        if frame.grid.row_text(y).contains(needle) {
            let symbol = &frame.grid.get(x, y).unwrap().symbol;
            assert!(
                !symbol.trim().is_empty() && !matches!(symbol.as_str(), "+" | "-" | "−" | "│"),
                "cursor is on a gutter, not code"
            );
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("missing code {needle}: {}", view(screen));
}
fn start_when_ready(screen: &mut GitScreen, code: KeyCode) {
    wait(screen, |screen| {
        screen.frame(120, 40);
        key(screen, code.clone());
        screen.busy()
    });
}
fn ready_hunks(screen: &mut GitScreen) {
    wait(screen, |screen| {
        screen.frame(120, 40);
        key(screen, KeyCode::Char('V'));
        view(screen).contains("Range started")
    });
    key(screen, KeyCode::Escape);
}
fn numbered() -> String {
    (1..=40).map(|line| format!("line {line:02}\n")).collect()
}

#[test]
fn directory_space_stages_descendants_then_unstages_without_touching_disk_or_other_index_entries() {
    let directory = repo(&[
        ("docs/one.md", "one\n"),
        ("docs/nested/two.md", "two\n"),
        ("z-other.txt", "before\n"),
    ]);
    let root = directory.path();
    write(root, "docs/one.md", "one edited\n");
    write(root, "docs/nested/two.md", "two edited\n");
    write(root, "docs/[literal].md", "literal pathspec\n");
    write(root, "z-other.txt", "unrelated staged bytes\n");
    git(root, &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("one.md"));
    tree(&mut screen);
    assert!(selected(&mut screen).contains("docs/"));
    screen.write_refusal = Some("Host refuses writes".into());
    key(&mut screen, KeyCode::Char('-'));
    assert!(view(&mut screen).contains("Host refuses writes"));
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('-'));
    assert!(screen.busy());
    // Repeated input while the command or its status refresh is active must not toggle back.
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":docs/one.md"]), "one edited");
    assert_eq!(git(root, &["show", ":docs/nested/two.md"]), "two edited");
    assert_eq!(
        git(root, &["show", ":docs/[literal].md"]),
        "literal pathspec"
    );
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(git(root, &["show", ":docs/one.md"]), "one");
    assert_eq!(git(root, &["show", ":docs/nested/two.md"]), "two");
    for (path, expected) in [
        ("docs/one.md", "one edited\n"),
        ("docs/nested/two.md", "two edited\n"),
        ("docs/[literal].md", "literal pathspec\n"),
    ] {
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), expected);
    }
}

#[test]
fn mixed_directory_finishes_partial_staging_and_unstages_both_rename_paths() {
    let directory = repo(&[
        ("old.txt", "renamed content\n"),
        ("docs/edit.txt", "original\n"),
        ("z-other.txt", "other\n"),
    ]);
    let root = directory.path();
    git(root, &["mv", "old.txt", "docs/renamed.txt"]);
    write(root, "docs/edit.txt", "staged\n");
    git(root, &["add", "docs/edit.txt"]);
    write(root, "docs/edit.txt", "remaining edit\n");
    write(root, "z-other.txt", "keep staged\n");
    git(root, &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("renamed.txt"));
    tree(&mut screen);
    assert!(selected(&mut screen).contains("docs/"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":docs/edit.txt"]), "remaining edit");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(git(root, &["show", ":old.txt"]), "renamed content");
    assert!(!root.join("old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("docs/renamed.txt")).unwrap(),
        "renamed content\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("docs/edit.txt")).unwrap(),
        "remaining edit\n"
    );
}

#[test]
fn directory_toggle_handles_an_unborn_index_and_deleted_descendants() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-b", "main"]);
    write(root, "docs/one.txt", "one\n");
    write(root, "docs/two.txt", "two\n");
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("one.txt"));
    tree(&mut screen);
    assert!(selected(&mut screen).contains("docs/"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["ls-files"]), "docs/one.txt\ndocs/two.txt");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["ls-files"]), "");
    assert_eq!(
        std::fs::read_to_string(root.join("docs/one.txt")).unwrap(),
        "one\n"
    );

    let directory = repo(&[("docs/one.txt", "one\n"), ("docs/two.txt", "two\n")]);
    let root = directory.path();
    std::fs::remove_file(root.join("docs/one.txt")).unwrap();
    write(root, "docs/two.txt", "changed\n");
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("one.txt"));
    tree(&mut screen);
    assert!(selected(&mut screen).contains("docs/"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-status"]),
        "D\tdocs/one.txt\nM\tdocs/two.txt"
    );
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    assert!(!root.join("docs/one.txt").exists());
}

#[test]
fn narrow_sidebar_shows_five_branches_without_changing_pane_boundaries_on_focus() {
    let directory = repo(&[("file.txt", "base\n")]);
    for branch in 0..12 {
        git(
            directory.path(),
            &["branch", &format!("visible-{branch:02}")],
        );
    }
    let mut screen = GitScreen::open(directory.path()).unwrap();
    wait(&mut screen, |screen| view(screen).contains("visible-00") && screen.panes_sized());
    let frame = screen.frame(100, 30);
    let headings = ["Changes", "Branches", "History", "Stashes"].map(|heading| {
        (0..30)
            .find(|&y| {
                frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(30)
                    .collect::<String>()
                    .contains(heading)
            })
            .unwrap()
    });
    let visible = (headings[1] + 1..headings[2] - 1)
        .filter(|&y| {
            let row = frame.grid.row_text(y).chars().take(30).collect::<String>();
            row.contains("visible-") || row.contains("main")
        })
        .count();
    // The Branches pane keeps its fixed budget at 100×30: every interior row below
    // the Local │ Remotes │ Tags sub-tab row shows a branch.
    let interior = headings[2] - headings[1] - 2;
    assert!(interior >= 4, "Branches budget shrank: {interior}");
    assert_eq!(visible, usize::from(interior - 1));
    let historical_heading = "Commit files".to_owned();
    for pane in ['3', '4', '5', '2'] {
        key(&mut screen, KeyCode::Char(pane));
        if pane == '4' {
            // History loads the selected commit's files in the background.
            wait(&mut screen, |screen| {
                let frame = screen.frame(100, 30);
                (0..30).any(|y| frame.grid.row_text(y).contains(&historical_heading))
            });
        }
        let focused = screen.frame(100, 30);
        let files_heading = if pane == '4' {
            historical_heading.as_str()
        } else {
            "Changes"
        };
        for (heading, y) in [files_heading, "Branches", "History", "Stashes"]
            .into_iter()
            .zip(headings)
        {
            assert!(
                focused.grid.row_text(y).contains(heading),
                "{pane}: {heading} moved from row {y}:\n{}",
                (0..30)
                    .map(|y| focused.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }
}

#[test]
fn branch_filter_restores_original_identity_after_cancel_clear_and_refresh() {
    let directory = repo(&[("file.txt", "base\n")]);
    let root = directory.path();
    for branch in 0..21 {
        git(root, &["branch", &format!("branch-{branch:02}")]);
    }
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("branch-00"));
    key(&mut screen, KeyCode::Char('3'));
    for _ in 0..6 {
        key(&mut screen, KeyCode::Down);
    }
    assert!(selected(&mut screen).contains("branch-06"));
    filter(&mut screen, "branch-19");
    assert!(selected(&mut screen).contains("branch-19"));
    assert_eq!(git(root, &["branch", "--show-current"]), "main");
    git(root, &["branch", "branch-000"]);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| !view(screen).contains("updating…"));
    assert!(selected(&mut screen).contains("branch-19"));
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Switch to branch-19?"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("branch-06"));
    key(&mut screen, KeyCode::Char('/'));
    text(&mut screen, "branch-20");
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("branch-06"));
    key(&mut screen, KeyCode::Char('/'));
    text(&mut screen, "branch-20");
    control(&mut screen, KeyCode::Char('u'));
    key(&mut screen, KeyCode::Enter);
    assert!(selected(&mut screen).contains("branch-06"));
}

#[test]
fn changes_filter_actions_target_the_visible_file_and_no_matches_cannot_stage_an_old_selection() {
    let directory = repo(&[
        ("docs/a.txt", "a\n"),
        ("docs/b.txt", "b\n"),
        ("docs/c.txt", "c\n"),
    ]);
    let root = directory.path();
    for name in ["a", "b", "c"] {
        write(
            root,
            &format!("docs/{name}.txt"),
            &format!("{name} changed\n"),
        );
    }
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("c.txt"));
    choose_file(&mut screen, "b.txt");
    filter(&mut screen, "c.txt");
    assert!(selected(&mut screen).contains("c.txt"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "docs/c.txt"
    );
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("b.txt"));
    filter(&mut screen, "NO-SUCH-FILE");
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy());
    assert_eq!(key(&mut screen, KeyCode::Enter), ScreenOutcome::Continue);
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "docs/c.txt"
    );
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("b.txt"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "docs/b.txt\ndocs/c.txt"
    );
}

#[test]
fn commit_and_commit_file_filters_keep_the_shared_changes_pane_read_only() {
    let directory = repo(&[
        ("src/git/screen/alpha.rs", "alpha\n"),
        ("src/git/screen/beta.rs", "beta\n"),
        ("src/git/screen/gamma.rs", "gamma\n"),
    ]);
    let root = directory.path();
    write(root, "z-later.txt", "later\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Later commit"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("Later commit"));
    wait(&mut screen, |screen| view(screen).contains("Local 1") && screen.panes_sized());
    key(&mut screen, KeyCode::Char('4'));
    let before = screen.frame(100, 30);
    let headings = ["Branches", "History", "Stashes"].map(|heading| {
        (0..30)
            .find(|&y| before.grid.row_text(y).contains(heading))
            .unwrap()
    });
    filter(&mut screen, "Baseline");
    wait(&mut screen, |screen| view(screen).contains("+ alpha"));
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |screen| view(screen).contains("+ beta"));
    assert!(selected(&mut screen).contains("beta.rs"));
    filter(&mut screen, "gamma.rs");
    wait(&mut screen, |screen| view(screen).contains("+ gamma"));
    assert!(selected(&mut screen).contains("gamma.rs"));
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy());
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Escape);
    wait(&mut screen, |screen| view(screen).contains("+ beta"));
    assert!(selected(&mut screen).contains("beta.rs"));
    let after = screen.frame(100, 30);
    for (heading, y) in ["Branches", "History", "Stashes"].into_iter().zip(headings) {
        assert!(after.grid.row_text(y).contains(heading));
    }
    assert_eq!(
        (0..30)
            .filter(|&y| {
                let title = after.grid.row_text(y);
                title.contains("Commit files")
            })
            .count(),
        1
    );
}

#[test]
fn native_diff_cursor_stages_and_unstages_one_hunk_not_the_whole_file() {
    let base = numbered();
    let changed = base
        .replace("line 02\n", "first hunk\n")
        .replace("line 30\n", "second hunk\n");
    let directory = repo(&[("file.txt", &base), ("z-other.txt", "other\n")]);
    let root = directory.path();
    write(root, "file.txt", &changed);
    write(root, "z-other.txt", "keep staged\n");
    git(root, &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("+ first hunk"));
    control(&mut screen, KeyCode::Char('l'));
    code_cursor(&mut screen, "+ first hunk");
    screen.write_refusal = Some("Unsaved host document".into());
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("Unsaved host document"));
    screen.write_refusal = None;
    start_when_ready(&mut screen, KeyCode::Char('-'));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["show", ":file.txt"]),
        base.replace("line 02\n", "first hunk\n").trim_end()
    );
    assert_eq!(git(root, &["show", ":z-other.txt"]), "keep staged");
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |screen| view(screen).contains("+ first hunk"));
    code_cursor(&mut screen, "+ first hunk");
    start_when_ready(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":file.txt"]), base.trim_end());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        changed
    );
}

#[test]
fn exact_added_line_then_range_staging_preserves_the_rest_of_the_worktree() {
    let directory = repo(&[("file.txt", "alpha\nomega\n")]);
    let root = directory.path();
    let changed = "alpha\nfirst added\nsecond added\nthird added\nomega\n";
    write(root, "file.txt", changed);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("+ first added"));
    control(&mut screen, KeyCode::Char('l'));
    code_cursor(&mut screen, "+ first added");
    start_when_ready(&mut screen, KeyCode::Char('a'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["show", ":file.txt"]),
        "alpha\nfirst added\nomega"
    );
    wait(&mut screen, |screen| view(screen).contains("+ second added"));
    code_cursor(&mut screen, "+ second added");
    ready_hunks(&mut screen);
    key(&mut screen, KeyCode::Char('V'));
    code_cursor(&mut screen, "+ third added");
    assert!(view(&mut screen).contains('▎'));
    key(&mut screen, KeyCode::Char('a'));
    assert!(screen.busy());
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":file.txt"]), changed.trim_end());
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        changed
    );
}

#[test]
fn selected_line_and_range_stage_unstage_are_symmetric_without_touching_disk() {
    let short = "alpha\nomega\n";
    let full = "alpha\nfirst selected\nsecond selected\nthird selected\nomega\n";
    let first = "alpha\nfirst selected\nomega\n";
    let rest = "alpha\nsecond selected\nthird selected\nomega\n";
    for (base, changed, marker, staged_line, unstaged_line) in [
        (short, full, '+', first, rest),
        (full, short, '−', rest, first),
    ] {
        let initial_index = format!("{base}kept staged\n");
        let original = format!("{base}kept original\n");
        let changed = format!("{changed}kept staged\n");
        let directory = repo(&[("file.txt", &original), ("z-other.txt", "other\n")]);
        let root = directory.path();
        write(root, "file.txt", &initial_index);
        write(root, "z-other.txt", "keep staged\n");
        git(root, &["add", "file.txt", "z-other.txt"]);
        write(root, "file.txt", &changed);
        let assert_contents = |expected: &str| {
            let repository = git2::Repository::open(root).unwrap();
            let index = repository.index().unwrap();
            for (path, expected) in [("file.txt", expected), ("z-other.txt", "keep staged\n")] {
                let entry = index.get_path(Path::new(path), 0).unwrap();
                let blob = repository.find_blob(entry.id).unwrap();
                assert_eq!(
                    blob.content(),
                    expected.as_bytes(),
                    "{marker}: index {path}"
                );
            }
            assert_eq!(
                std::fs::read(root.join("file.txt")).unwrap(),
                changed.as_bytes()
            );
            assert_eq!(
                std::fs::read(root.join("z-other.txt")).unwrap(),
                b"keep staged\n".as_slice()
            );
        };
        assert_contents(&initial_index);
        let code = |name| format!("{marker} {name} selected");
        let mut screen = GitScreen::open(root).unwrap();
        wait(&mut screen, |screen| view(screen).contains(&code("first")));
        control(&mut screen, KeyCode::Char('l'));
        code_cursor(&mut screen, &code("first"));
        start_when_ready(&mut screen, KeyCode::Char('a'));
        wait(&mut screen, |screen| !screen.busy());
        assert_contents(&format!("{staged_line}kept staged\n"));

        wait(&mut screen, |screen| view(screen).contains(&code("second")));
        code_cursor(&mut screen, &code("second"));
        ready_hunks(&mut screen);
        key(&mut screen, KeyCode::Char('V'));
        assert!(view(&mut screen).contains("stage selected diff lines"));
        code_cursor(&mut screen, &code("third"));
        key(&mut screen, KeyCode::Char('a'));
        assert!(screen.busy());
        wait(&mut screen, |screen| !screen.busy());
        assert_contents(&changed);

        // Pin the index layer so it remains selected after the first partial unstage.
        key(&mut screen, KeyCode::Char('i'));
        wait(&mut screen, |screen| {
            let displayed = view(screen);
            displayed.contains(&code("first")) && !displayed.contains("updating…")
        });
        code_cursor(&mut screen, &code("first"));
        ready_hunks(&mut screen);
        // The reader footer names the reverse action once the Staged layer is shown.
        assert!(view(&mut screen).contains("unstage hunk"));
        screen.write_refusal = Some("Host refuses precise unstaging".into());
        key(&mut screen, KeyCode::Char('a'));
        assert!(!screen.busy());
        assert!(view(&mut screen).contains("Host refuses precise unstaging"));
        assert_contents(&changed);
        screen.write_refusal = None;
        start_when_ready(&mut screen, KeyCode::Char('a'));
        wait(&mut screen, |screen| !screen.busy());
        assert_contents(&format!("{unstaged_line}kept staged\n"));

        wait(&mut screen, |screen| {
            let displayed = view(screen);
            displayed.contains(&code("second")) && !displayed.contains("updating…")
        });
        code_cursor(&mut screen, &code("second"));
        ready_hunks(&mut screen);
        key(&mut screen, KeyCode::Char('V'));
        assert!(view(&mut screen).contains("unstage selected diff lines"));
        code_cursor(&mut screen, &code("third"));
        key(&mut screen, KeyCode::Char('-'));
        assert!(
            !screen.busy(),
            "- must not silently expand a selected range"
        );
        assert!(view(&mut screen).contains("a unstages the selected range"));
        assert_contents(&format!("{unstaged_line}kept staged\n"));
        let cursor = screen.frame(120, 40).cursor;
        key(&mut screen, KeyCode::Escape);
        assert_eq!(
            screen.frame(120, 40).cursor,
            cursor,
            "Esc cancels the range, not code focus"
        );
        assert!(!view(&mut screen).contains("Range started"));
        assert!(view(&mut screen).contains("unstage hunk"));
        key(&mut screen, KeyCode::Char('V'));
        key(&mut screen, KeyCode::Up);
        code_cursor(&mut screen, &code("second"));
        key(&mut screen, KeyCode::Char('a'));
        assert!(screen.busy());
        wait(&mut screen, |screen| !screen.busy());
        // The unrelated replacement in this same hunk stays staged, as does the other file.
        assert_contents(&initial_index);
        assert_eq!(
            git(root, &["diff", "--cached", "--name-only"]),
            "file.txt\nz-other.txt"
        );
    }
}

#[test]
fn exact_removed_line_staging_never_restores_a_disk_line() {
    let directory = repo(&[("file.txt", "alpha\nremove one\nremove two\nomega\n")]);
    let root = directory.path();
    write(root, "file.txt", "alpha\nomega\n");
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("− remove one"));
    control(&mut screen, KeyCode::Char('l'));
    code_cursor(&mut screen, "− remove one");
    start_when_ready(&mut screen, KeyCode::Char('a'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["show", ":file.txt"]),
        "alpha\nremove two\nomega"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        "alpha\nomega\n"
    );
}

#[test]
fn range_crossing_hunks_refuses_instead_of_staging_an_unintended_subset() {
    let base = numbered();
    let changed = base
        .replace("line 02\n", "first hunk\n")
        .replace("line 30\n", "second hunk\n");
    let directory = repo(&[("file.txt", &base)]);
    let root = directory.path();
    write(root, "file.txt", &changed);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("+ first hunk"));
    control(&mut screen, KeyCode::Char('l'));
    code_cursor(&mut screen, "+ first hunk");
    ready_hunks(&mut screen);
    key(&mut screen, KeyCode::Char('V'));
    code_cursor(&mut screen, "+ second hunk");
    key(&mut screen, KeyCode::Char('a'));
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("within one hunk"));
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('-'));
    assert!(screen.busy());
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["show", ":file.txt"]),
        base.replace("line 30\n", "second hunk\n").trim_end()
    );
}

#[test]
fn stale_hunk_captures_refuse_external_index_worktree_and_head_changes() {
    for change in ["index", "worktree", "head"] {
        let directory = repo(&[("file.txt", "before\n"), ("z-other.txt", "other\n")]);
        let root = directory.path();
        write(root, "file.txt", "selected edit\n");
        let mut screen = GitScreen::open(root).unwrap();
        wait(&mut screen, |screen| {
            view(screen).contains("+ selected edit")
        });
        control(&mut screen, KeyCode::Char('l'));
        code_cursor(&mut screen, "+ selected edit");
        ready_hunks(&mut screen);
        match change {
            "index" => {
                write(root, "z-other.txt", "external staged\n");
                git(root, &["add", "z-other.txt"]);
            }
            "worktree" => write(root, "file.txt", "external disk edit\n"),
            _ => {
                git(root, &["commit", "--allow-empty", "-m", "External HEAD"]);
            }
        }
        key(&mut screen, KeyCode::Char('-'));
        assert!(screen.busy(), "the checked plan must reach the worker");
        wait(&mut screen, |screen| !screen.busy());
        let displayed = view(&mut screen);
        assert!(
            displayed.contains("stale") || displayed.contains("HEAD changed"),
            "{change}: {displayed}"
        );
        assert_eq!(git(root, &["show", ":file.txt"]), "before", "{change}");
        if change == "index" {
            assert_eq!(git(root, &["show", ":z-other.txt"]), "external staged");
        }
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            if change == "worktree" {
                "external disk edit\n"
            } else {
                "selected edit\n"
            }
        );
    }
}

#[test]
fn compact_tree_status_offsets_and_file_identity_survive_refresh_and_folding() {
    let directory = repo(&[
        ("src/git/screen/sample.rs", "before\n"),
        ("src/git/screen/second.rs", "second\n"),
    ]);
    let root = directory.path();
    write(root, "src/git/screen/sample.rs", "after\n");
    write(root, "src/git/screen/second.rs", "second changed\n");
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| {
        view(screen).contains("src/git/screen/")
    });
    tree(&mut screen);
    assert!(selected(&mut screen).contains("src/git/screen/"));
    choose_file(&mut screen, "sample.rs");
    let frame = screen.frame(120, 40);
    let (x, y, _) = frame.cursor.unwrap();
    assert_eq!(x, 1, "the cursor sits in the pane's first interior column");
    assert_eq!(
        frame.grid.get(3, y).unwrap().symbol,
        " ",
        "clean index column"
    );
    assert_eq!(
        frame.grid.get(4, y).unwrap().symbol,
        "M",
        "modified working-tree column"
    );
    assert_eq!(
        frame.grid.get(8, y).unwrap().symbol,
        "◇",
        "one displayed level (two cells), not three filesystem levels"
    );
    assert_eq!(
        frame.grid.get(10, y).unwrap().symbol,
        "s",
        "filename follows fixed XY columns and the two-cell glyph slot"
    );
    assert!(frame.grid.row_text(y).contains("sample.rs"));
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| view(screen).contains("+ after"));
    assert!(selected(&mut screen).contains("sample.rs"));
    key(&mut screen, KeyCode::Char('h'));
    assert!(selected(&mut screen).contains("▸ src/git/screen/"));
    key(&mut screen, KeyCode::Char('l'));
    choose_file(&mut screen, "sample.rs");
    assert_eq!(
        key(&mut screen, KeyCode::Enter),
        ScreenOutcome::OpenFile(root.join("src/git/screen/sample.rs"))
    );
}

#[test]
fn sidebar_tabs_use_real_worktrees_remote_refs_tags_and_reflog_events() {
    let directory = repo(&[("file.txt", "local content\n")]);
    let remote = repo(&[("remote.txt", "remote content\n")]);
    let owner = tempfile::tempdir().unwrap();
    let root = directory.path();
    let linked = owner.path().join("linked-review");
    git(remote.path(), &["branch", "remote-topic"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git(
        root,
        &["-c", "protocol.file.allow=always", "fetch", "origin"],
    );
    git(root, &["tag", "v1"]);
    git(
        root,
        &["worktree", "add", "-b", "side", linked.to_str().unwrap()],
    );
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("Baseline") && displayed.contains("Worktrees 2") && screen.panes_sized()
    });
    let frame = screen.frame(190, 48);
    let labels = (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(labels.contains("Changes"));
    assert!(labels.contains("Worktrees"));
    assert!(labels.contains("Submodules"));
    assert!(labels.contains("Branches"));
    assert!(labels.contains("Remotes"));
    assert!(labels.contains("Tags"));
    assert!(labels.contains("History"));
    let headings = ["Changes", "Branches", "History", "Stashes"].map(|name| {
        (0..48)
            .find(|&y| {
                frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(52)
                    .collect::<String>()
                    .contains(name)
            })
            .unwrap()
    });

    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| view(screen).contains("linked-review"));
    filter(&mut screen, "linked-review");
    assert_eq!(
        key(&mut screen, KeyCode::Enter),
        ScreenOutcome::OpenRepository(linked.clone())
    );
    key(&mut screen, KeyCode::Char('3'));
    filter(&mut screen, "origin/");
    assert!(
        view(&mut screen).contains("No matches"),
        "remote refs must not leak into local branches"
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('L'));
    filter(&mut screen, "origin/remote-topic");
    assert!(selected(&mut screen).contains("origin/remote-topic"));
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Create a local branch tracking"));
    screen.write_refusal = Some("Host is read-only".into());
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Host is read-only"));
    assert_eq!(git(root, &["branch", "--show-current"]), "main");
    screen.write_refusal = None;
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["branch", "--show-current"]), "remote-topic");
    assert_eq!(
        git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/remote-topic"
    );
    key(&mut screen, KeyCode::Char('L'));
    assert!(view(&mut screen).contains("v1"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| view(screen).contains("HEAD@{0}"));
    // The History pane's title follows its sub-tab.
    assert!(view(&mut screen).contains("Reflog"));
    let id = git(root, &["rev-parse", "HEAD"]);
    assert!(selected(&mut screen).contains(&id[..7]));
    assert!(wide(&mut screen).contains("checkout: moving from main"));
    git(root, &["commit", "--allow-empty", "-m", "Above"]);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| wide(screen).contains("commit: Above"));
    assert!(
        selected(&mut screen).contains(&id[..7]),
        "refresh must preserve the selected reflog event, not HEAD@{{0}}"
    );
    let head_before_confirmation = git(root, &["rev-parse", "HEAD"]);
    key(&mut screen, KeyCode::Char('o'));
    assert!(view(&mut screen).contains("Check out commit with detached HEAD?"));
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head_before_confirmation);
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    let frame = screen.frame(190, 48);
    let historical_heading = "Commit files".to_owned();
    for (name, y) in [
        historical_heading.as_str(),
        "Branches",
        "Reflog",
        "Stashes",
    ]
    .into_iter()
    .zip(headings)
    {
        assert!(
            frame
                .grid
                .row_text(y)
                .chars()
                .take(52)
                .collect::<String>()
                .contains(name),
            "{name} moved from row {y}: {:?}",
            frame.grid.row_text(y)
        );
    }

    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Check out commit with detached HEAD?"));
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head_before_confirmation);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('d'));
    assert!(view(&mut screen).contains("Remove clean, unlocked worktree"));
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| !screen.busy());
    assert!(!linked.exists());
    assert!(git(root, &["worktree", "list"]).lines().count() == 1);
}

#[test]
fn native_submodules_open_init_and_reverse_gitlink_staging_without_staging_child_files() {
    let directory = repo(&[("file.txt", "parent content\n")]);
    let source = repo(&[("library.txt", "original child\n")]);
    let root = directory.path();
    let relative = "vendor/library with spaces";
    let child = root.join(relative);
    git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.path().to_str().unwrap(),
            relative,
        ],
    );
    git(root, &["commit", "-m", "Add generated submodule"]);
    let original = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "library.txt", "new child revision\n");
    git(&child, &["add", "library.txt"]);
    git(
        &child,
        &[
            "-c",
            "user.name=Workflow Test",
            "-c",
            "user.email=workflow@example.invalid",
            "commit",
            "-m",
            "New child revision",
        ],
    );
    let changed = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "draft.txt", "untracked child draft\n");
    let mut screen = GitScreen::open(root).unwrap();
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains(&changed) && displayed.contains("Parent index: no staged pointer")
    });
    assert_submodule_status(&mut screen, " ", " M");
    assert!(view(&mut screen).contains("Expected index commit"));
    assert_eq!(
        key(&mut screen, KeyCode::Enter),
        ScreenOutcome::OpenRepository(child.clone())
    );
    key(&mut screen, KeyCode::Char('u'));
    assert!(view(&mut screen).contains("Commit or save the submodule"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&changed));
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), relative);
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("Parent index: staged pointer")
            && displayed.contains("Checkout pointer: matches index")
    });
    assert_submodule_status(&mut screen, "◐", "MM");
    assert!(view(&mut screen).contains("Working files: modified"));
    // The untracked child draft makes the parent report unstaged submodule changes too.
    // A second - must still unstage the pointer rather than trying to stage that draft.
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&original));
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("Parent index: no staged pointer")
            && displayed.contains("Checkout pointer: unstaged revision")
    });
    assert_submodule_status(&mut screen, " ", " M");
    assert_eq!(
        std::fs::read_to_string(child.join("library.txt")).unwrap(),
        "new child revision\n"
    );
    assert_eq!(
        std::fs::read_to_string(child.join("draft.txt")).unwrap(),
        "untracked child draft\n"
    );

    std::fs::remove_file(child.join("draft.txt")).unwrap();
    git(root, &["add", "--", relative]);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("Parent index: staged pointer")
            && displayed.contains("Checkout pointer: matches index")
            && displayed.contains("Working files: clean")
    });
    assert_submodule_status(&mut screen, "✓", "M ");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        !screen.busy()
            && displayed.contains("Parent index: no staged pointer")
            && displayed.contains("Checkout pointer: unstaged revision")
    });
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| {
        !screen.busy() && view(screen).contains("Parent index: staged pointer")
    });
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&changed));
    git(root, &["commit", "-m", "Keep the generated child revision"]);
    git(root, &["submodule", "deinit", "--", relative]);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| {
        view(screen).contains("Checkout: not initialized")
    });
    assert_eq!(key(&mut screen, KeyCode::Enter), ScreenOutcome::Continue);
    key(&mut screen, KeyCode::Char('u'));
    assert!(view(&mut screen).contains("Initialize / update"));
    screen.write_refusal = Some("Host is read-only".into());
    control(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Host is read-only"));
    assert!(!child.join(".git").exists());
    screen.write_refusal = None;
    control(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| {
        !screen.busy() && child.join(".git").is_file()
    });
    assert_eq!(git(&child, &["rev-parse", "HEAD"]), changed);
    assert_eq!(
        std::fs::read_to_string(child.join("library.txt")).unwrap(),
        "new child revision\n"
    );
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn directory_space_reverses_gitlinks_even_when_child_files_remain_dirty() {
    let directory = repo(&[
        ("vendor/ordinary.txt", "parent base\n"),
        ("z-other.txt", "other\n"),
    ]);
    let source = repo(&[("library.txt", "child base\n")]);
    let root = directory.path();
    let relative = "vendor/library";
    git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.path().to_str().unwrap(),
            relative,
        ],
    );
    git(root, &["commit", "-m", "Add child"]);
    let child = root.join(relative);
    let original = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "library.txt", "child revision\n");
    git(&child, &["add", "library.txt"]);
    git(
        &child,
        &[
            "-c",
            "user.name=Workflow Test",
            "-c",
            "user.email=workflow@example.invalid",
            "commit",
            "-m",
            "Child revision",
        ],
    );
    let changed = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "draft.txt", "keep child draft\n");
    write(root, "vendor/ordinary.txt", "parent edit\n");
    write(root, "z-other.txt", "keep staged\n");
    git(root, &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(root).unwrap();
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| view(screen).contains(&changed));
    key(&mut screen, KeyCode::Char('H'));
    key(&mut screen, KeyCode::Char('H'));
    tree(&mut screen);
    // Inventory and parent status are independent reads. A loaded submodule
    // preview does not imply the Changes tree is ready on the first open.
    wait(&mut screen, |screen| selected(screen).contains("vendor/"));
    assert!(selected(&mut screen).contains("vendor/"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":vendor/ordinary.txt"]), "parent edit");
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&changed));
    // Observe the refreshed submodule inventory through native tabs, then reverse
    // the whole parent directory rather than opening/staging its child repository.
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| {
        view(screen).contains("Checkout pointer: matches index")
    });
    assert_submodule_status(&mut screen, "◐", "MM");
    key(&mut screen, KeyCode::Char('H'));
    key(&mut screen, KeyCode::Char('H'));
    assert!(selected(&mut screen).contains("vendor/"));
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(git(root, &["show", ":vendor/ordinary.txt"]), "parent base");
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&original));
    assert_eq!(
        std::fs::read_to_string(root.join("vendor/ordinary.txt")).unwrap(),
        "parent edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(child.join("library.txt")).unwrap(),
        "child revision\n"
    );
    assert_eq!(
        std::fs::read_to_string(child.join("draft.txt")).unwrap(),
        "keep child draft\n"
    );
    assert_eq!(git(root, &["show", ":z-other.txt"]), "keep staged");
}

#[test]
fn filter_dead_end_recovers_the_last_match_without_changing_saved_directory_folds() {
    let directory = repo(&[
        ("docs/a.txt", "a\n"),
        ("docs/b.txt", "b\n"),
        ("docs/c.txt", "c\n"),
    ]);
    let root = directory.path();
    for name in ["a", "b", "c"] {
        write(
            root,
            &format!("docs/{name}.txt"),
            &format!("{name} edited\n"),
        );
    }
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("c.txt"));
    tree(&mut screen);
    key(&mut screen, KeyCode::Char('h'));
    assert!(selected(&mut screen).contains("▸ docs/"));
    filter(&mut screen, "docs");
    assert!(selected(&mut screen).contains("a.txt"));
    key(&mut screen, KeyCode::Down);
    assert!(selected(&mut screen).contains("b.txt"));
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Char('!'));
    assert!(view(&mut screen).contains("No matches"));
    // Navigating an empty result must not replace the remembered file with row 0.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Up);
    text(&mut screen, "still-no-match");
    for _ in "still-no-match".chars() {
        key(&mut screen, KeyCode::Backspace);
    }
    assert!(view(&mut screen).contains("No matches"));
    key(&mut screen, KeyCode::Backspace);
    // While editing, the native cursor belongs to the query, not the list.
    // Accept the recovered query before inspecting the selected sidebar row.
    key(&mut screen, KeyCode::Enter);
    assert!(
        selected(&mut screen).contains("b.txt"),
        "backspacing a dead-end query must not jump to a.txt"
    );
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "docs/b.txt"
    );
    key(&mut screen, KeyCode::Escape);
    assert!(selected(&mut screen).contains("▸ docs/"));
    assert!(!view(&mut screen).contains("a.txt") && !view(&mut screen).contains("c.txt"));
}

#[test]
fn file_status_columns_and_filename_do_not_move_between_worktree_mixed_and_index_states() {
    let directory = repo(&[("file.txt", "base\n")]);
    let root = directory.path();
    write(root, "file.txt", "first edit\n");
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("+ first edit"));
    // Fixed Changes row anatomy (DESIGN.md §3.1): `▸` cursor, a mark column
    // (`◐` when partly staged), the XY status letters, the file glyph, then the path.
    let assert_status = |screen: &mut GitScreen, marker: &str, code: &str| {
        let cursor = dust::ui::GitSurface::Cursor;
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let frame = screen.frame(width, height);
            let (_, y, _) = frame.cursor.expect("file cursor");
            assert!(
                !frame.grid.get(1, y).unwrap().symbol.trim().is_empty(),
                "{width}×{height}: cursor marker"
            );
            assert_eq!(
                frame.grid.get(2, y).unwrap().symbol,
                marker,
                "{width}×{height}"
            );
            for (column, expected) in code.chars().enumerate() {
                let cell = frame.grid.get(3 + column as u16, y).unwrap();
                assert_eq!(
                    cell.symbol,
                    expected.to_string(),
                    "{width}×{height}: {code:?}"
                );
                if expected == 'M' {
                    let role = if column == 0 {
                        dust::ui::GitRole::Added
                    } else {
                        dust::ui::GitRole::Modified
                    };
                    assert_eq!(
                        cell.style,
                        dust::ui::Style::Git {
                            role,
                            surface: cursor,
                            emphasis: dust::ui::Emphasis::None,
                        },
                        "{width}×{height}: {code:?}"
                    );
                }
            }
            assert_eq!(frame.grid.get(6, y).unwrap().symbol, "◇");
            assert_eq!(frame.grid.get(7, y).unwrap().symbol, " ");
            let name_role = match code {
                "M " => dust::ui::GitRole::Added,
                "MM" => dust::ui::GitRole::Modified,
                _ => dust::ui::GitRole::Text,
            };
            for (column, expected) in "file.txt".chars().enumerate() {
                let cell = frame.grid.get(8 + column as u16, y).unwrap();
                assert_eq!(
                    cell.symbol,
                    expected.to_string(),
                    "filename shifted at {width}×{height}"
                );
                assert_eq!(
                    cell.style,
                    dust::ui::Style::Git {
                        role: name_role,
                        surface: cursor,
                        emphasis: dust::ui::Emphasis::None,
                    }
                );
            }
        }
    };
    assert_status(&mut screen, " ", " M");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":file.txt"]), "first edit");
    assert_status(&mut screen, " ", "M ");
    write(root, "file.txt", "later edit\n");
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| view(screen).contains("+ later edit"));
    assert_status(&mut screen, "◐", "MM");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":file.txt"]), "later edit");
    assert_status(&mut screen, " ", "M ");
    key(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":file.txt"]), "base");
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        "later edit\n"
    );
    assert_status(&mut screen, " ", " M");
}

#[test]
fn compact_help_exposes_submodule_remote_stash_and_diff_actions_at_every_supported_geometry() {
    let directory = repo(&[("file.txt", "base\n")]);
    write(directory.path(), "file.txt", "edit\n");
    let mut screen = GitScreen::open(directory.path()).unwrap();
    wait(&mut screen, |screen| view(screen).contains("+ edit"));
    let help = |screen: &mut GitScreen, width: u16, height: u16| {
        key(screen, KeyCode::Char('?'));
        let frame = screen.frame(width, height);
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        key(screen, KeyCode::Escape);
        text
    };
    let assert_help = |text: &str, width: u16, height: u16, actions: &[&str]| {
        for action in actions {
            assert!(
                text.contains(action),
                "{width}×{height}: missing {action}: {text}"
            );
        }
    };
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        key(&mut screen, KeyCode::Char('2'));
        let before = screen.frame(width, height);
        let footer = before.grid.row_text(height - 1);
        for action in ["s", "stage", "commit", "keys"] {
            assert!(
                footer.contains(action),
                "{width}×{height}: missing {action}: {footer}"
            );
        }
        // Help is grouped per focused pane: its actions, then navigation and view keys.
        let changes = help(&mut screen, width, height);
        assert_help(
            &changes,
            width,
            height,
            &[
                "Keys · Changes",
                "Stage · unstage",
                "Mark a range",
                "Commit staged",
                "A W F",
                "Flat · tree",
                "Sub-tabs of this pane",
                "C-h/j/k/l",
                "Filter list",
                "Split · unified",
                "Refresh",
                "Esc close",
            ],
        );
        let after = screen.frame(width, height);
        assert_eq!(before.cursor, after.cursor);
        for y in 0..height {
            assert_eq!(before.grid.row_text(y), after.grid.row_text(y));
        }
        key(&mut screen, KeyCode::Char(']'));
        key(&mut screen, KeyCode::Char(']'));
        let submodules = help(&mut screen, width, height);
        assert_help(
            &submodules,
            width,
            height,
            &["Keys · Submodules", "Toggle pointer", "Init · update"],
        );
        key(&mut screen, KeyCode::Char(']'));
        key(&mut screen, KeyCode::Char('3'));
        let refs = screen.frame(width, height);
        let heading = (0..height)
            .find(|&y| refs.grid.row_text(y).contains("╭ ⎇ Branches"))
            .unwrap();
        for name in ["Local", "Remotes", "Tags"] {
            assert!(refs.grid.row_text(heading + 1).contains(name));
        }
        key(&mut screen, KeyCode::Char(']'));
        let remotes = help(&mut screen, width, height);
        assert_help(&remotes, width, height, &["Keys · Remotes", "Fetch"]);
        key(&mut screen, KeyCode::Char('['));
        key(&mut screen, KeyCode::Char('5'));
        let stashes = help(&mut screen, width, height);
        assert_help(&stashes, width, height, &["Keys · Stashes", "New stash", "Apply"]);
        key(&mut screen, KeyCode::Char('0'));
        let reader = help(&mut screen, width, height);
        assert_help(
            &reader,
            width,
            height,
            &["Keys · Reader", "Stage · unstage hunk", "Start · finish a range"],
        );
    }
}

#[test]
fn filtered_partial_hunk_summary_distinguishes_repo_layer_and_remaining_work_without_changing_data()
{
    let base = (1..=90)
        .map(|n| format!("line {n:02}\n"))
        .collect::<String>();
    let changed = base
        .replace("line 12\n", "needle_12\n")
        .replace("line 30\n", "needle_30\n")
        .replace("line 48\n", "needle_48\n")
        .replace("line 68\n", "needle_68\n");
    let directory = repo(&[("search.rs", &base), ("notes.md", "notes before\n")]);
    let root = directory.path();
    write(root, "search.rs", &changed);
    write(root, "notes.md", "unrelated staged notes\n");
    git(root, &["add", "notes.md"]);
    write(root, "draft.txt", "untracked draft\n");
    let index_before = git(root, &["ls-files", "--stage"]);
    let refs_before = git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let head_before = git(root, &["rev-parse", "HEAD"]);
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains("search.rs"));
    choose_file(&mut screen, "search.rs");
    filter(&mut screen, "search.rs");
    wait(&mut screen, |screen| view(screen).contains("+ needle_12"));
    control(&mut screen, KeyCode::Char('l'));
    ready_hunks(&mut screen);
    let find_first = |screen: &mut GitScreen| {
        key(screen, KeyCode::Char('/'));
        control(screen, KeyCode::Char('u'));
        text(screen, "needle");
        key(screen, KeyCode::Enter);
        code_cursor(screen, "+ needle_12");
    };
    find_first(&mut screen);
    // The Changes title carries the filter and its match count, the reader's file
    // band names the layer and its hunks, and the footer the find position.
    let assert_summary = |screen: &mut GitScreen, staged: bool| {
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let frame = screen.frame(width, height);
            let rows = (0..height)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>();
            let band = if staged {
                "search.rs · Staged · 1 hunk"
            } else {
                "search.rs · Unstaged · 4 hunks"
            };
            assert!(
                rows.iter().any(|row| row.contains(band)),
                "{width}×{height}: {}",
                rows.join("\n")
            );
            if width >= 100 {
                assert!(
                    rows[1].contains("Changes") && rows[1].contains("/search.rs · 1"),
                    "{width}×{height}: {}",
                    rows[1]
                );
            }
            let find = &rows[usize::from(height - 1)];
            assert!(find.contains("/needle/"), "{find}");
            assert!(find.contains(if staged { "1/1" } else { "1/4" }), "{find}");
            if staged {
                assert!(rows.iter().any(|row| row.contains("Working tree differs")));
            }
        }
    };
    assert_summary(&mut screen, false);
    start_when_ready(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    // Partial staging leaves the remaining working-tree hunks visible. Select
    // Index explicitly before locating the staged hunk and reversing it.
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |screen| {
        view(screen).contains("search.rs · Staged · 1 hunk · Working tree differs")
    });
    // A changed layer owns fresh search coordinates; reapply the same literal.
    find_first(&mut screen);
    assert_summary(&mut screen, true);
    assert_eq!(
        git(root, &["show", ":search.rs"]),
        base.replace("line 12\n", "needle_12\n").trim_end()
    );
    assert_eq!(git(root, &["show", ":notes.md"]), "unrelated staged notes");
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "notes.md\nsearch.rs"
    );
    start_when_ready(&mut screen, KeyCode::Char('-'));
    wait(&mut screen, |screen| !screen.busy());
    wait(&mut screen, |screen| {
        view(screen).contains("search.rs · Unstaged · 4 hunks")
            && view(screen).contains("+ needle_48")
    });
    find_first(&mut screen);
    assert_summary(&mut screen, false);
    assert_eq!(git(root, &["ls-files", "--stage"]), index_before);
    assert_eq!(
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        refs_before
    );
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head_before);
    for (path, expected) in [
        ("search.rs", changed.as_str()),
        ("notes.md", "unrelated staged notes\n"),
        ("draft.txt", "untracked draft\n"),
    ] {
        assert_eq!(std::fs::read(root.join(path)).unwrap(), expected.as_bytes());
    }
}
