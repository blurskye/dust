//! Marked-file batches through native input/frame APIs and real disposable Git indexes.
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::{GitScreen, ScreenOutcome};
use dusty::ui::{Frame, GitRole, KeyInputKind, Style, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const SIZES: [(u16, u16); 3] = [(64, 20), (100, 30), (190, 48)];

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Batch Test",
            "-c",
            "user.email=batch@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim_end().into()
}
fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    initialize_repo(tempfile::tempdir().unwrap(), files)
}
fn initialize_repo(owner: tempfile::TempDir, files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = owner.path();
    git(root, &["init", "-b", "main"]);
    for (name, source) in files {
        write(root, name, source);
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Baseline"]);
    owner
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
fn text(frame: &Frame) -> String {
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
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
        assert!(
            Instant::now() < deadline,
            "batch timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn open(root: &Path) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    let has_head = git2::Repository::open(root).unwrap().head().is_ok();
    wait(&mut screen, |screen| {
        let frame = screen.frame(190, 48);
        let rail = |y: u16| {
            frame
                .grid
                .row_text(y)
                .chars()
                .take(RAIL)
                .collect::<String>()
        };
        // The Changes count replaces `…` once status has loaded.
        let status_ready = rail(1).contains("Changes") && !rail(1).contains('…');
        // The checked-out branch row carries the HEAD dot once refs have loaded.
        let refs_ready = !has_head || (2..47).any(|y| rail(y).contains("● ⎇ "));
        status_ready && refs_ready
    });
    screen
}
/// Rail columns: wide and medium layouts; narrow layouts show one pane at a time.
fn rail(width: u16, height: u16) -> u16 {
    if width < 100 {
        width
    } else if width >= 160 && height >= 40 {
        (25 + width / 7).clamp(44, 60)
    } else {
        (width / 4 + 7).clamp(32, 44)
    }
}
/// Rail columns at 190×48.
const RAIL: usize = 52;
// Read exactly the selected list field. Never match an unrelated sidebar/preview name.
// Returns the path (flat) or name (tree) and column 1: `✓` marked, `◐` partly staged.
fn selected(screen: &mut GitScreen) -> (String, String) {
    let frame = screen.frame(190, 48);
    let (x, y, _) = frame.cursor.expect("native list cursor");
    assert!(x < RAIL as u16, "expected list focus");
    let marker = frame.grid.get(x + 1, y).unwrap().symbol.clone();
    // After the XY status: a file-kind glyph (or fold glyph), a space, the name, counts.
    let field = (x + 4..RAIL as u16 - 1)
        .map(|at| frame.grid.get(at, y).unwrap().symbol.as_str())
        .collect::<String>();
    let field = field.trim();
    let (glyph, rest) = field.split_once(' ').expect("glyph and name");
    assert!(matches!(glyph, "◇" | "▾" | "▸"), "row glyph {glyph:?}");
    let name = rest
        .trim_end()
        .rsplit_once("  ")
        .map_or(rest.trim_end(), |(name, _)| name)
        .trim()
        .to_owned();
    (name, marker)
}
fn choose(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('2'));
    for _ in 0..32 {
        key(screen, KeyCode::PageUp);
    }
    for _ in 0..500 {
        let (field, _) = selected(screen);
        if field == name
            || field.ends_with(&format!("/{name}"))
            || name.ends_with('/') && field.starts_with(name)
        {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("missing list field {name:?}:\n{}", view(screen));
}
fn mark(screen: &mut GitScreen, name: &str) {
    choose(screen, name);
    key(screen, KeyCode::Char('x'));
    assert_eq!(selected(screen).1, "✓", "mark {name}");
}
fn count(screen: &mut GitScreen, expected: usize) {
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let shown = text(&frame);
        let footer = frame.grid.row_text(height - 1);
        // The rail and footer, not the reader's own prose about marked files.
        let marked = (0..height)
            .map(|y| {
                let row = frame.grid.row_text(y);
                if y + 1 == height {
                    row
                } else {
                    row.chars().take(usize::from(rail(width, height))).collect()
                }
            })
            .filter(|line| line.contains("marked"))
            .collect::<Vec<_>>();
        if expected == 0 {
            assert!(marked.is_empty(), "{width}×{height}: {marked:?}");
        } else {
            // The Changes title counts the set: `Changes 5 · 4 marked`.
            assert!(
                marked
                    .iter()
                    .any(|line| line.contains(&format!("· {expected} marked"))),
                "{width}×{height}: {shown}"
            );
            // The footer offers the marked-set actions. A notice (refusal, Git output) sits
            // beside them on the right and the hints that no longer fit drop out (r8 R7-05):
            // beside a long refusal at 100 columns only `x mark` is left. At full width they
            // all fit. A notice too long to share the row takes it (no `? keys`).
            if footer.contains("? k") {
                assert!(
                    footer.contains("stage marked") || footer.contains("x mark"),
                    "{width}×{height}: {footer}"
                );
                if width >= 190 {
                    assert!(
                        footer.contains("stage marked"),
                        "{width}×{height}: {footer}"
                    );
                }
            }
        }
    }
}
fn space(screen: &mut GitScreen) {
    assert_eq!(key(screen, KeyCode::Char('-')), ScreenOutcome::Continue);
    assert!(screen.busy(), "write did not start:\n{}", view(screen));
    wait(screen, |screen| !screen.busy());
    eprintln!("after -:\n{}", view(screen));
}
fn disk(root: &Path, paths: &[&str]) -> Vec<Option<Vec<u8>>> {
    paths
        .iter()
        .map(|path| std::fs::read(root.join(path)).ok())
        .collect()
}
fn cached(root: &Path) -> String {
    git(root, &["diff", "--cached", "--no-renames", "--name-only"])
}
fn filter(screen: &mut GitScreen, query: &str) {
    key(screen, KeyCode::Char('/'));
    screen.event(UiEvent::Paste(query.into()));
    key(screen, KeyCode::Enter);
}
/// Every pane rectangle (its four rounded corners) and whether it has focus.
fn borders(frame: &Frame) -> Vec<(u16, u16, String, bool)> {
    let mut result = Vec::new();
    for y in 0..frame.grid.height() - 1 {
        for x in 0..frame.grid.width() {
            let cell = frame.grid.get(x, y).unwrap();
            if matches!(cell.symbol.as_str(), "╭" | "╮" | "╰" | "╯") {
                let focused = matches!(
                    cell.style,
                    Style::Git {
                        role: GitRole::Focus,
                        ..
                    }
                );
                result.push((x, y, cell.symbol.clone(), focused));
            }
        }
    }
    result
}

#[test]
fn disjoint_marks_finish_partial_rename_binary_and_untracked_then_reverse_only_that_set() {
    let owner = repo(&[
        ("alpha/edit.txt", "original\n"),
        ("beta/keep.txt", "keep base\n"),
        ("delta/l.txt", "literal neighbour base\n"),
        ("gamma/image.bin", "old\0binary\n"),
        ("old.txt", "renamed content\n"),
        ("z-unrelated.txt", "unrelated base\n"),
    ]);
    let root = owner.path();
    write(root, "alpha/edit.txt", "partially staged\n");
    git(root, &["add", "alpha/edit.txt"]);
    write(root, "alpha/edit.txt", "remaining edit\n");
    git(root, &["mv", "old.txt", "beta/moved.txt"]);
    write(root, "beta/keep.txt", "must remain unstaged\n");
    write(root, "delta/[literal].txt", "untracked literal path\n");
    write(root, "delta/l.txt", "must not match the literal pathspec\n");
    write(root, "gamma/image.bin", b"new\0binary\xff\n");
    write(root, "z-unrelated.txt", "unrelated staged bytes\n");
    git(root, &["add", "z-unrelated.txt"]);
    let unrelated = git(root, &["ls-files", "--stage", "--", "z-unrelated.txt"]);
    let paths = [
        "alpha/edit.txt",
        "beta/keep.txt",
        "beta/moved.txt",
        "delta/[literal].txt",
        "delta/l.txt",
        "gamma/image.bin",
        "old.txt",
        "z-unrelated.txt",
    ];
    let working = disk(root, &paths);
    let mut screen = open(root);
    for name in ["edit.txt", "moved.txt", "image.bin", "[literal].txt"] {
        mark(&mut screen, name);
    }
    count(&mut screen, 4);
    let before_refusal = std::fs::read(root.join(".git/index")).unwrap();
    screen.write_refusal = Some("Host refuses batch writes".into());
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("Host refuses batch writes"));
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        before_refusal
    );
    count(&mut screen, 4);
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('-'));
    assert!(screen.busy());
    key(&mut screen, KeyCode::Char('-')); // busy/post-write refresh cannot reverse early
    wait(&mut screen, |screen| !screen.busy());
    assert_eq!(git(root, &["show", ":alpha/edit.txt"]), "remaining edit");
    assert_eq!(git(root, &["show", ":beta/moved.txt"]), "renamed content");
    assert_eq!(
        git(root, &["show", ":delta/[literal].txt"]),
        "untracked literal path"
    );
    let repository = git2::Repository::open(root).unwrap();
    let index = repository.index().unwrap();
    let binary = index.get_path(Path::new("gamma/image.bin"), 0).unwrap();
    assert_eq!(
        repository.find_blob(binary.id).unwrap().content(),
        b"new\0binary\xff\n"
    );
    assert_eq!(git(root, &["show", ":beta/keep.txt"]), "keep base");
    assert_eq!(
        git(root, &["show", ":delta/l.txt"]),
        "literal neighbour base"
    );
    count(&mut screen, 4);
    space(&mut screen);
    assert_eq!(cached(root), "z-unrelated.txt");
    assert_eq!(git(root, &["show", ":alpha/edit.txt"]), "original");
    assert_eq!(git(root, &["show", ":old.txt"]), "renamed content");
    assert_eq!(
        git(root, &["ls-files", "--stage", "--", "z-unrelated.txt"]),
        unrelated
    );
    assert_eq!(disk(root, &paths), working);
    count(&mut screen, 4);
}

#[test]
fn visible_range_extends_and_shrinks_without_losing_disjoint_marks_or_including_folded_files() {
    let owner = repo(&[
        ("docs/a.txt", "a\n"),
        ("docs/b.txt", "b\n"),
        ("docs/c.txt", "c\n"),
        ("middle/hidden.txt", "hidden\n"),
        ("z-root.txt", "z\n"),
    ]);
    let root = owner.path();
    for name in [
        "docs/a.txt",
        "docs/b.txt",
        "docs/c.txt",
        "middle/hidden.txt",
        "z-root.txt",
    ] {
        write(root, name, format!("edited {name}\n"));
    }
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('t')); // folding needs the tree view
    choose(&mut screen, "middle/");
    key(&mut screen, KeyCode::Left);
    key(&mut screen, KeyCode::Char('x')); // headings do not implicitly mark descendants
    count(&mut screen, 0);
    mark(&mut screen, "z-root.txt");
    choose(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('V'));
    key(&mut screen, KeyCode::Char('j'));
    key(&mut screen, KeyCode::Down);
    count(&mut screen, 4);
    key(&mut screen, KeyCode::Char('k'));
    count(&mut screen, 3);
    key(&mut screen, KeyCode::Up);
    count(&mut screen, 2);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Char('V')); // finish, not clear
    count(&mut screen, 3);
    choose(&mut screen, "c.txt");
    assert_ne!(selected(&mut screen).1, "✓");
    choose(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('V'));
    for _ in 0..4 {
        key(&mut screen, KeyCode::Down); // b, c, folded middle heading, z
    }
    assert_eq!(selected(&mut screen).0, "z-root.txt");
    count(&mut screen, 4);
    key(&mut screen, KeyCode::Char('V'));
    space(&mut screen);
    assert_eq!(
        cached(root),
        "docs/a.txt\ndocs/b.txt\ndocs/c.txt\nz-root.txt"
    );
    assert_eq!(git(root, &["show", ":middle/hidden.txt"]), "hidden");
    count(&mut screen, 4);
    space(&mut screen);
    assert_eq!(cached(root), "");
    assert_eq!(
        std::fs::read_to_string(root.join("middle/hidden.txt")).unwrap(),
        "edited middle/hidden.txt\n"
    );
}

#[test]
fn folded_marked_descendants_are_counted_and_space_never_expands_the_target_set() {
    let owner = repo(&[
        ("docs/a.txt", "a\n"),
        ("docs/b.txt", "b\n"),
        ("docs/c.txt", "c\n"),
        ("z.txt", "z\n"),
    ]);
    let root = owner.path();
    for name in ["docs/a.txt", "docs/b.txt", "docs/c.txt", "z.txt"] {
        write(root, name, format!("edited {name}\n"));
    }
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('t')); // folding needs the tree view
    mark(&mut screen, "a.txt");
    choose(&mut screen, "docs/");
    key(&mut screen, KeyCode::Left);
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.unwrap();
        assert_eq!(frame.grid.get(x + 5, y).unwrap().symbol, "▸");
        assert_eq!(frame.grid.get(x + 1, y).unwrap().symbol, "✓");
        assert!(text(&frame).contains("· 1 marked"), "{width}×{height}");
        // The footer context starts with the counts; narrower footers keep its end.
        if width == 190 {
            assert!(
                frame
                    .grid
                    .row_text(height - 1)
                    .contains("1 marked · 1 folded")
            );
        }
    }
    mark(&mut screen, "z.txt");
    choose(&mut screen, "docs/");
    count(&mut screen, 2);
    space(&mut screen); // directory cursor still toggles only the two explicit marks
    assert_eq!(cached(root), "docs/a.txt\nz.txt");
    assert_eq!(git(root, &["show", ":docs/b.txt"]), "b");
    assert_eq!(git(root, &["show", ":docs/c.txt"]), "c");
    space(&mut screen);
    assert_eq!(cached(root), "");
    assert_eq!(
        std::fs::read_to_string(root.join("docs/a.txt")).unwrap(),
        "edited docs/a.txt\n"
    );
}

#[test]
fn escape_filter_all_files_sidebar_history_and_repository_contexts_clear_marks() {
    let owner = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let root = owner.path();
    write(root, "a.txt", "a edited\n");
    write(root, "b.txt", "b edited\n");
    let mut screen = open(root);
    mark(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('x'));
    count(&mut screen, 0);
    key(&mut screen, KeyCode::Char('V'));
    key(&mut screen, KeyCode::Down);
    count(&mut screen, 2);
    let cursor = screen.frame(100, 30).cursor;
    assert_eq!(key(&mut screen, KeyCode::Escape), ScreenOutcome::Continue);
    assert_eq!(screen.frame(100, 30).cursor, cursor);
    count(&mut screen, 0);
    mark(&mut screen, "b.txt");
    filter(&mut screen, "a.txt");
    count(&mut screen, 0);
    assert_eq!(selected(&mut screen).0, "a.txt");
    key(&mut screen, KeyCode::Char('x'));
    assert_eq!(key(&mut screen, KeyCode::Escape), ScreenOutcome::Continue);
    count(&mut screen, 0);
    assert!(view(&mut screen).contains("/a.txt · 1"));
    key(&mut screen, KeyCode::Escape); // now clear the filter, not navigation
    assert!(!view(&mut screen).contains("/a.txt · 1"));
    mark(&mut screen, "a.txt");
    filter(&mut screen, "b.txt");
    space(&mut screen);
    assert_eq!(
        cached(root),
        "b.txt",
        "old marks cannot survive a query change"
    );
    space(&mut screen);
    key(&mut screen, KeyCode::Escape);
    assert_eq!(cached(root), "");
    mark(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('F')); // all-file binding
    key(&mut screen, KeyCode::Char('x'));
    key(&mut screen, KeyCode::Char('V'));
    key(&mut screen, KeyCode::Char('F'));
    count(&mut screen, 0);
    mark(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('L')); // Worktrees
    key(&mut screen, KeyCode::Char('H')); // Changes
    count(&mut screen, 0);
    mark(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('4')); // History
    wait(&mut screen, |screen| view(screen).contains("Baseline"));
    key(&mut screen, KeyCode::Enter);
    for code in ['x', 'V', '-'] {
        key(&mut screen, KeyCode::Char(code));
    }
    assert!(!screen.busy(), "historical files remain read-only");
    key(&mut screen, KeyCode::Escape); // historical files -> history
    key(&mut screen, KeyCode::Escape); // history -> Changes
    count(&mut screen, 0);
    assert_eq!(cached(root), "");
    mark(&mut screen, "a.txt");
    let other = repo(&[("a.txt", "other\n")]);
    write(other.path(), "a.txt", "other edited\n");
    let mut next = open(other.path());
    count(&mut next, 0);
    space(&mut next);
    assert_eq!(cached(other.path()), "a.txt");
    assert_eq!(cached(root), "");
}

#[test]
fn status_refresh_prunes_stale_marks_and_finishes_ranges_without_marking_new_rows() {
    let owner = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let root = owner.path();
    write(root, "a.txt", "a edited\n");
    write(root, "b.txt", "b edited\n");
    let mut screen = open(root);
    choose(&mut screen, "a.txt");
    key(&mut screen, KeyCode::Char('V'));
    key(&mut screen, KeyCode::Down);
    count(&mut screen, 2);
    write(root, "b.txt", "b\n");
    write(root, "aa.txt", "new row must stay unmarked\n");
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| selected(screen).0 == "aa.txt");
    assert_ne!(selected(&mut screen).1, "✓");
    count(&mut screen, 1);
    key(&mut screen, KeyCode::Up);
    key(&mut screen, KeyCode::Down);
    count(&mut screen, 1);
    space(&mut screen);
    assert_eq!(cached(root), "a.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("aa.txt")).unwrap(),
        "new row must stay unmarked\n"
    );
}

#[test]
fn markers_keep_xy_filename_native_source_cursor_and_fixed_rectangles_and_never_hijack_diff_space()
{
    let original = (1..=40)
        .map(|n| format!("let line_{n:02} = {n};\n"))
        .collect::<String>();
    let edited = original
        .replace("let line_05 = 5;", "let needle_one = 5;")
        .replace("let line_33 = 33;", "let needle_two = 33;");
    let owner = repo(&[("code.rs", &original), ("z.rs", "fn before() {}\n")]);
    let root = owner.path();
    write(root, "code.rs", &edited);
    write(root, "z.rs", "fn after() {}\n");
    let mut screen = open(root);
    mark(&mut screen, "z.rs");
    choose(&mut screen, "code.rs");
    let before = SIZES.map(|(w, h)| screen.frame(w, h));
    key(&mut screen, KeyCode::Char('x'));
    for ((width, height), before) in SIZES.into_iter().zip(before) {
        let after = screen.frame(width, height);
        assert_eq!(after.cursor, before.cursor);
        assert_eq!(borders(&after), borders(&before), "{width}×{height}");
        let (x, y, _) = after.cursor.unwrap();
        assert_eq!(x, 1);
        assert_eq!(after.grid.get(2, y).unwrap().symbol, "✓");
        // XY status, file glyph, then the path.
        for (column, expected) in [(3, " "), (4, "M"), (5, " "), (6, "◇"), (7, " "), (8, "c")] {
            assert_eq!(after.grid.get(column, y).unwrap().symbol, expected);
            assert_eq!(
                after.grid.get(column, y).unwrap().symbol,
                before.grid.get(column, y).unwrap().symbol
            );
        }
        for heading in ["Changes", "Branches", "History", "Stashes"] {
            let left = rail(width, height);
            let heading_row = |frame: &Frame| {
                (0..height).find(|&y| {
                    frame
                        .grid
                        .row_text(y)
                        .chars()
                        .take(usize::from(left))
                        .collect::<String>()
                        .contains(heading)
                })
            };
            assert_eq!(heading_row(&after), heading_row(&before));
        }
    }
    key(&mut screen, KeyCode::Char('x')); // leave only the OTHER file marked
    count(&mut screen, 1);
    control(&mut screen, KeyCode::Char('l'));
    wait(&mut screen, |screen| {
        view(screen).contains("needle_one") && !view(screen).contains("updating…")
    });
    let source_geometry = SIZES.map(|(w, h)| borders(&screen.frame(w, h)));
    filter(&mut screen, "needle_one"); // '/' is source find in preview focus
    for ((width, height), geometry) in SIZES.into_iter().zip(source_geometry) {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.expect("native source cursor");
        assert!(x > rail(width, height) || width < 100 && x > 0);
        assert!(y < height - 1);
        assert_eq!(frame.grid.get(x, y).unwrap().style, Style::Search);
        let field = (x..width)
            .map(|at| frame.grid.get(at, y).unwrap().symbol.as_str())
            .collect::<String>();
        assert!(
            field.starts_with("needle_one"),
            "actual cursor field: {field:?}"
        );
        assert_eq!(borders(&frame), geometry);
    }
    wait(&mut screen, |screen| {
        key(screen, KeyCode::Char('V'));
        view(screen).contains("Range started")
    });
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy(), "diff range still refuses whole-hunk -");
    assert_eq!(cached(root), "");
    key(&mut screen, KeyCode::Escape); // cancel exact diff range, not file marks
    space(&mut screen); // preview hunk, never the marked z.rs
    let indexed = git(root, &["show", ":code.rs"]);
    assert!(indexed.contains("needle_one"));
    assert!(!indexed.contains("needle_two"));
    assert_eq!(git(root, &["show", ":z.rs"]), "fn before() {}");
    assert_eq!(
        std::fs::read_to_string(root.join("code.rs")).unwrap(),
        edited
    );
    control(&mut screen, KeyCode::Char('h'));
    count(&mut screen, 1);
}

#[test]
fn marked_dirty_gitlink_and_disjoint_file_reverse_parent_pointers_without_staging_child_bytes() {
    let owner = repo(&[
        ("docs/parent.txt", "parent base\n"),
        ("z-other.txt", "other base\n"),
    ]);
    let source = repo(&[("library.txt", "child base\n")]);
    let root = owner.path();
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
    let old = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "library.txt", "child revision\n");
    git(&child, &["add", "library.txt"]);
    git(&child, &["commit", "-m", "Child revision"]);
    let new = git(&child, &["rev-parse", "HEAD"]);
    write(&child, "library.txt", "uncommitted child bytes\n");
    write(&child, "draft.txt", "untracked child bytes\n");
    write(root, "docs/parent.txt", "parent edit\n");
    write(root, "z-other.txt", "keep staged\n");
    git(root, &["add", "z-other.txt"]);
    let child_index = std::fs::read(root.join(".git/modules/vendor/library/index")).unwrap();
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |screen| view(screen).contains(&new));
    key(&mut screen, KeyCode::Char('H'));
    key(&mut screen, KeyCode::Char('H'));
    mark(&mut screen, "parent.txt");
    mark(&mut screen, "library");
    space(&mut screen);
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&new));
    assert_eq!(git(root, &["show", ":docs/parent.txt"]), "parent edit");
    count(&mut screen, 2);
    space(&mut screen); // refreshed refs know dirty child files do not change the staged pointer
    assert!(git(root, &["ls-files", "--stage", "--", relative]).contains(&old));
    assert_eq!(cached(root), "z-other.txt");
    assert_eq!(
        std::fs::read_to_string(child.join("library.txt")).unwrap(),
        "uncommitted child bytes\n"
    );
    assert_eq!(
        std::fs::read_to_string(child.join("draft.txt")).unwrap(),
        "untracked child bytes\n"
    );
    assert_eq!(
        std::fs::read(root.join(".git/modules/vendor/library/index")).unwrap(),
        child_index
    );
    assert_eq!(git(root, &["show", ":z-other.txt"]), "keep staged");
}

#[test]
fn a_marked_file_replaced_by_a_directory_refuses_to_sweep_in_hidden_descendants() {
    let owner = repo(&[("a.txt", "old file\n"), ("z.txt", "other\n")]);
    let root = owner.path();
    std::fs::remove_file(root.join("a.txt")).unwrap();
    write(root, "a.txt/hidden.txt", "unmarked new descendant\n");
    let mut screen = open(root);
    mark(&mut screen, "a.txt");
    let index = std::fs::read(root.join(".git/index")).unwrap();
    key(&mut screen, KeyCode::Char('-'));
    assert!(!screen.busy());
    assert!(view(&mut screen).contains("selected file is now a directory"));
    assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
    assert_eq!(cached(root), "");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt/hidden.txt")).unwrap(),
        "unmarked new descendant\n"
    );
    count(&mut screen, 1);
}

#[test]
fn marked_unborn_paths_reverse_without_removing_working_files_or_unmarked_entries() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    git(root, &["init", "-b", "main"]);
    write(root, "a.txt", "a\n");
    write(root, "docs/b.txt", "b\n");
    write(root, "z.txt", "unmarked staged\n");
    git(root, &["add", "z.txt"]);
    let mut screen = open(root);
    mark(&mut screen, "a.txt");
    mark(&mut screen, "b.txt");
    space(&mut screen);
    assert_eq!(git(root, &["ls-files"]), "a.txt\ndocs/b.txt\nz.txt");
    space(&mut screen);
    assert_eq!(git(root, &["ls-files"]), "z.txt");
    assert_eq!(
        disk(root, &["a.txt", "docs/b.txt", "z.txt"]),
        vec![
            Some(b"a\n".to_vec()),
            Some(b"b\n".to_vec()),
            Some(b"unmarked staged\n".to_vec()),
        ]
    );
    count(&mut screen, 2);
}

// macOS file names must be UTF-8, so the name this test needs cannot exist there.
#[cfg(target_os = "linux")]
#[test]
fn marked_paths_use_literal_nul_input_without_round_tripping_non_utf8_names() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    // Some /tmp filesystems reject native bytes; Linux tmpfs supports them.
    let owner = tempfile::tempdir_in("/dev/shm").unwrap();
    let owner = initialize_repo(owner, &[("l.txt", "neighbour base\n")]);
    let root = owner.path();
    let raw = OsString::from_vec(b"b-\xff.txt".to_vec());
    std::fs::write(root.join(&raw), b"non-UTF8 path bytes\n").unwrap();
    write(root, "[literal].txt", "literal bytes\n");
    write(root, "l.txt", "unmarked neighbour\n");
    let mut screen = open(root);
    mark(&mut screen, &raw.to_string_lossy());
    mark(&mut screen, "[literal].txt");
    space(&mut screen);
    let repository = git2::Repository::open(root).unwrap();
    let index = repository.index().unwrap();
    let entry = index.get_path(Path::new(&raw), 0).unwrap();
    assert_eq!(
        repository.find_blob(entry.id).unwrap().content(),
        b"non-UTF8 path bytes\n"
    );
    assert_eq!(git(root, &["show", ":l.txt"]), "neighbour base");
    space(&mut screen);
    assert_eq!(cached(root), "");
    assert_eq!(
        std::fs::read(root.join(&raw)).unwrap(),
        b"non-UTF8 path bytes\n"
    );
    assert_eq!(
        std::fs::read(root.join("[literal].txt")).unwrap(),
        b"literal bytes\n"
    );
}

#[test]
fn home_end_and_pages_extend_and_shrink_file_ranges_without_writing() {
    let owner = repo(&[
        ("a.txt", "old a\n"),
        ("b.txt", "old b\n"),
        ("z.txt", "old z\n"),
    ]);
    let root = owner.path();
    for name in ["a.txt", "b.txt", "z.txt"] {
        write(root, name, format!("new {name}\n"));
    }
    let working = disk(root, &["a.txt", "b.txt", "z.txt"]);
    let before = git(root, &["ls-files", "--stage"]);
    for (width, height) in SIZES {
        let mut screen = open(root);
        screen.frame(width, height);
        choose(&mut screen, "a.txt");
        key(&mut screen, KeyCode::Char('V'));
        for (code, name, marked) in [
            (KeyCode::End, "z.txt", 3),
            (KeyCode::Home, "a.txt", 1),
            (KeyCode::PageDown, "z.txt", 3),
            (KeyCode::PageUp, "a.txt", 1),
            (KeyCode::End, "z.txt", 3),
        ] {
            key(&mut screen, code);
            assert_eq!(selected(&mut screen).0, name);
            count(&mut screen, marked);
            assert_eq!(git(root, &["ls-files", "--stage"]), before);
        }
        key(&mut screen, KeyCode::Char('V'));
        space(&mut screen);
        assert_eq!(cached(root), "a.txt\nb.txt\nz.txt");
        space(&mut screen);
        assert_eq!(git(root, &["ls-files", "--stage"]), before);
        assert_eq!(disk(root, &["a.txt", "b.txt", "z.txt"]), working);
        key(&mut screen, KeyCode::Escape);
        key(&mut screen, KeyCode::Char('5')); // Stashes
        key(&mut screen, KeyCode::End);
        key(&mut screen, KeyCode::Home);
        assert!(view(&mut screen).contains("No stashes"));
    }
}
