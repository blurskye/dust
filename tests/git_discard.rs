//! Confirmed, exact worktree discard through native input/frame APIs and real Git repositories.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{Frame, GitRole, KeyInputKind, Style, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const SIZES: [(u16, u16); 3] = [(64, 20), (100, 30), (190, 48)];
const DIALOG: &str = "Discard unstaged change?";
const FILE_DIALOG: &str = "Discard changes in change.rs?";
const OTHERS: [&str; 3] = ["keep-index.txt", "other.rs", "untracked.txt"];

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Discard Test",
            "-c",
            "user.email=discard@example.invalid",
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
fn write(root: &Path, path: &str, source: &str) {
    std::fs::write(root.join(path), source).unwrap();
}
fn numbered() -> String {
    (1..=120)
        .map(|line| format!("let line_{line:03} = {line};\n"))
        .collect()
}
fn repository(base: &str, staged: &str, working: &str) -> tempfile::TempDir {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    git(root, &["init", "-b", "main"]);
    write(root, "change.rs", base);
    write(root, "keep-index.txt", "keep base\n");
    write(root, "other.rs", "fn other_base() {}\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Baseline discard fixture"]);
    write(root, "change.rs", staged);
    write(root, "keep-index.txt", "keep staged\n");
    git(root, &["add", "change.rs", "keep-index.txt"]);
    write(root, "change.rs", working);
    write(root, "other.rs", "fn other_work() {}\n");
    write(root, "untracked.txt", "untracked must never be deleted\n");
    owner
}
fn mixed() -> (tempfile::TempDir, String, String) {
    let base = numbered();
    let staged = base.replace("line_040", "staged_040");
    let working = staged
        .replace("line_005", "first_work")
        .replace("line_100", "second_work");
    (repository(&base, &staged, &working), staged, working)
}
fn index_bytes(root: &Path) -> Vec<u8> {
    let repository = git2::Repository::open(root).unwrap();
    std::fs::read(repository.index().unwrap().path().unwrap()).unwrap()
}
fn disk(root: &Path, paths: &[&str]) -> Vec<Option<Vec<u8>>> {
    paths
        .iter()
        .map(|path| std::fs::read(root.join(path)).ok())
        .collect()
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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "discard timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn ready_source(screen: &mut GitScreen, needle: &str) {
    wait(screen, |screen| {
        let source = view(screen);
        source.contains(needle) && !source.contains("updating…") && !source.contains("Loading…")
    });
}
// Inspect only the cursor's content field, never a whole row that can match a sidebar filename.
fn code_field(frame: &Frame) -> String {
    let (x, y, _) = frame.cursor.expect("real native source cursor");
    let left = reader_x(frame.grid.width(), frame.grid.height());
    assert!(x > left && y < frame.grid.height() - 1, "cursor {x},{y}");
    let symbol = frame.grid.get(x, y).unwrap().symbol.as_str();
    assert!(!symbol.trim().is_empty() && !matches!(symbol, "+" | "−" | "│"));
    (x..frame.grid.width() - 1)
        .map(|at| frame.grid.get(at, y).unwrap().symbol.as_str())
        .collect::<String>()
        .trim_end()
        .into()
}
/// The reader's left border column: the rail fills the left in wide and medium
/// layouts; narrow layouts show the reader alone.
fn reader_x(width: u16, height: u16) -> u16 {
    if width < 100 {
        0
    } else if width >= 160 && height >= 40 {
        (25 + width / 7).clamp(44, 60) + 1
    } else {
        (20 + width / 10).clamp(28, 36) + 1
    }
}
/// The rail's right edge, for headings that belong to the rail only.
fn rail_width(width: u16, height: u16) -> u16 {
    reader_x(width, height).saturating_sub(1)
}
fn selected_file(screen: &mut GitScreen) -> (String, String) {
    let frame = screen.frame(190, 48);
    let (x, y, _) = frame.cursor.expect("real native list cursor");
    assert_eq!(x, 1, "expected Changes list focus");
    let marker = frame.grid.get(x + 1, y).unwrap().symbol.clone();
    assert_eq!(frame.grid.get(x + 5, y).unwrap().symbol, "◇");
    assert_eq!(frame.grid.get(x + 6, y).unwrap().symbol, " ");
    let field = (x + 7..51)
        .map(|at| frame.grid.get(at, y).unwrap().symbol.as_str())
        .collect::<String>();
    // Drop the right-aligned `+n −m` counts.
    let name = field
        .trim_end()
        .rsplit_once("  ")
        .map_or(field.trim_end(), |(name, _)| name)
        .trim_end()
        .into();
    (name, marker)
}
fn choose_file(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('2'));
    key(screen, KeyCode::Home);
    for _ in 0..20 {
        if selected_file(screen).0 == name {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("missing selected list field {name:?}:\n{}", view(screen));
}
fn seek_code(screen: &mut GitScreen, needle: &str) {
    key(screen, KeyCode::Home);
    for _ in 0..300 {
        if code_field(&screen.frame(190, 48)).starts_with(needle) {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("missing source cursor field {needle:?}:\n{}", view(screen));
}
fn ready_hunks(screen: &mut GitScreen) {
    wait(screen, |screen| {
        screen.frame(190, 48);
        key(screen, KeyCode::Char('V'));
        view(screen).contains("Range started")
    });
    key(screen, KeyCode::Escape);
}
fn open_code(root: &Path, needle: &str) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    ready_source(&mut screen, needle);
    control(&mut screen, KeyCode::Char('l'));
    seek_code(&mut screen, needle);
    ready_hunks(&mut screen);
    screen
}
fn confirm(screen: &mut GitScreen) {
    assert!(view(screen).contains(DIALOG));
    key(screen, KeyCode::Enter);
    assert!(
        screen.busy(),
        "confirmed reset did not start:\n{}",
        view(screen)
    );
    wait(screen, |screen| !screen.busy());
}
fn refused(screen: &mut GitScreen, code: char) -> String {
    key(screen, KeyCode::Char(code));
    let shown = view(screen);
    assert!(!screen.busy() && !shown.contains(DIALOG), "{shown}");
    shown
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
fn footer(frame: &Frame) -> String {
    frame.grid.row_text(frame.grid.height() - 1)
}

#[test]
fn discard_confirmation_and_escape_keep_disk_index_and_native_cursor_unchanged() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let before = screen.frame(190, 48);
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    key(&mut screen, KeyCode::Char('d'));
    assert!(!screen.busy());
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let shown = text(&frame);
        for label in [
            DIALOG,
            "File",
            "change.rs",
            "Hunk 1/2 · @@",
            "index is kept",
            "Discard ",
            "Cancel",
        ] {
            assert!(shown.contains(label), "{width}×{height}: {shown}");
        }
        // The native cursor rests on the primary Discard button.
        let (x, y, _) = frame.cursor.expect("confirmation button cursor");
        let row = frame.grid.row_text(y);
        assert!(
            row.chars()
                .skip(usize::from(x))
                .collect::<String>()
                .starts_with("Discard"),
            "{width}×{height}: {row}"
        );
        assert!(row.contains("Cancel"), "{width}×{height}: {row}");
    }
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
    // Read-only scope fields cannot be edited into another file or line selection.
    key(&mut screen, KeyCode::Tab); // Cancel
    key(&mut screen, KeyCode::Tab); // File
    screen.event(UiEvent::Paste("other.rs".into()));
    key(&mut screen, KeyCode::End);
    key(&mut screen, KeyCode::Escape);
    let after = screen.frame(190, 48);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(code_field(&after), code_field(&before));
    assert_eq!(borders(&after), borders(&before));
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
}

#[test]
fn discard_current_hunk_keeps_mixed_index_second_hunk_and_unrelated_bytes() {
    let (owner, staged, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let index = index_bytes(root);
    let cached = git(root, &["diff", "--cached"]);
    let others = disk(root, &OTHERS);
    key(&mut screen, KeyCode::Char('d'));
    // Dialog navigation must not retarget the captured hunk to the end of the source.
    key(&mut screen, KeyCode::End);
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    assert!(screen.busy());
    wait(&mut screen, |screen| {
        view(screen).contains("Working tree updated")
    });
    let completed = view(&mut screen);
    assert!(completed.contains("index unchanged"), "{completed}");
    assert!(!completed.contains("Index updated") && !completed.contains("let first_work"));
    assert!(
        completed.contains("Loading…"),
        "obsolete code must be hidden: {completed}"
    );
    assert!(
        screen.busy(),
        "post-write refresh must still block another write"
    );
    key(&mut screen, KeyCode::Char('D'));
    assert!(!view(&mut screen).contains(DIALOG));
    assert!(view(&mut screen).contains("Wait for the current Git operation"));
    wait(&mut screen, |screen| !screen.busy());
    ready_source(&mut screen, "let second_work");
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working.replace("first_work", "line_005")
    );
    assert_eq!(git(root, &["show", ":change.rs"]), staged.trim_end());
    assert_eq!(git(root, &["diff", "--cached"]), cached);
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
}

#[test]
fn exact_added_removed_line_and_active_ranges_discard_only_selected_diff_lines() {
    let short = "alpha\nomega\n";
    let full = "alpha\nfirst selected\nsecond selected\nthird selected\nomega\n";
    let rest = "alpha\nsecond selected\nthird selected\nomega\n";
    let first = "alpha\nfirst selected\nomega\n";
    for (base_prefix, changed_prefix, after_one_prefix, marker) in
        [(short, full, rest, '+'), (full, short, first, '-')]
    {
        for range_key in ['d', 'D'] {
            let base = format!("{base_prefix}{}", numbered());
            let staged = base.replace("line_040", "staged_040");
            let suffix = numbered()
                .replace("line_040", "staged_040")
                .replace("line_100", "second_work");
            let working = format!("{changed_prefix}{suffix}");
            let owner = repository(&base, &staged, &working);
            let root = owner.path();
            let mut screen = open_code(root, "first selected");
            let index = index_bytes(root);
            let others = disk(root, &OTHERS);
            key(&mut screen, KeyCode::Char('D'));
            let dialog = view(&mut screen);
            assert!(
                dialog.contains("diff line ") && dialog.contains(&format!("({marker}")),
                "{dialog}"
            );
            confirm(&mut screen);
            let after_one = format!("{after_one_prefix}{suffix}");
            assert_eq!(
                std::fs::read_to_string(root.join("change.rs")).unwrap(),
                after_one
            );
            assert_eq!(index_bytes(root), index);
            ready_source(&mut screen, "second selected");
            seek_code(&mut screen, "second selected");
            ready_hunks(&mut screen);
            key(&mut screen, KeyCode::Char('V'));
            seek_code(&mut screen, "third selected");
            key(&mut screen, KeyCode::Char(range_key));
            let dialog = view(&mut screen);
            assert!(
                dialog.contains("diff lines ") && dialog.contains("(2 changes)"),
                "{dialog}"
            );
            key(&mut screen, KeyCode::Escape);
            assert_eq!(
                std::fs::read_to_string(root.join("change.rs")).unwrap(),
                after_one
            );
            assert_eq!(index_bytes(root), index);
            // Cancel leaves the active range intact; d must still not expand to the whole hunk.
            key(&mut screen, KeyCode::Char(range_key));
            confirm(&mut screen);
            assert_eq!(
                std::fs::read_to_string(root.join("change.rs")).unwrap(),
                format!("{base_prefix}{suffix}"),
                "{marker} range {range_key}"
            );
            assert_eq!(git(root, &["show", ":change.rs"]), staged.trim_end());
            assert_eq!(index_bytes(root), index);
            assert_eq!(disk(root, &OTHERS), others);
        }
    }
}

#[test]
fn active_file_marks_and_file_range_cannot_broaden_code_discard() {
    for mark_key in ['x', 'V'] {
        let (owner, _, working) = mixed();
        let root = owner.path();
        let mut screen = GitScreen::open(root).unwrap();
        ready_source(&mut screen, "let first_work");
        if mark_key == 'x' {
            for path in ["other.rs", "change.rs"] {
                choose_file(&mut screen, path);
                key(&mut screen, KeyCode::Char('x'));
                assert_eq!(selected_file(&mut screen).1, "✓");
            }
        } else {
            choose_file(&mut screen, "change.rs");
            key(&mut screen, KeyCode::Char('V'));
            key(&mut screen, KeyCode::End);
            key(&mut screen, KeyCode::Char('V'));
        }
        choose_file(&mut screen, "change.rs");
        assert_eq!(selected_file(&mut screen).1, "✓");
        ready_source(&mut screen, "let first_work");
        control(&mut screen, KeyCode::Char('l'));
        seek_code(&mut screen, "let first_work");
        ready_hunks(&mut screen);
        let index = index_bytes(root);
        let others = disk(root, &OTHERS);
        key(&mut screen, KeyCode::Char('d'));
        assert!(view(&mut screen).contains("Hunk 1/2 · @@"));
        confirm(&mut screen);
        assert_eq!(
            std::fs::read_to_string(root.join("change.rs")).unwrap(),
            working.replace("first_work", "line_005")
        );
        assert_eq!(index_bytes(root), index);
        assert_eq!(disk(root, &OTHERS), others);
        control(&mut screen, KeyCode::Char('h'));
        choose_file(&mut screen, "other.rs");
        assert_eq!(
            selected_file(&mut screen).1,
            "✓",
            "marks survive a code discard"
        );
    }
}

#[test]
fn discard_dialog_rejects_stale_disk_or_index_without_retargeting() {
    for change in ["worktree", "index"] {
        let (owner, _, _) = mixed();
        let root = owner.path();
        let mut screen = open_code(root, "let first_work");
        key(&mut screen, KeyCode::Char('d'));
        assert!(view(&mut screen).contains(DIALOG));
        if change == "worktree" {
            write(root, "change.rs", "external disk content must survive\n");
        } else {
            write(
                root,
                "keep-index.txt",
                "external staged content must survive\n",
            );
            git(root, &["add", "keep-index.txt"]);
        }
        let index = index_bytes(root);
        let paths = ["change.rs", "keep-index.txt", "other.rs", "untracked.txt"];
        let files = disk(root, &paths);
        confirm(&mut screen);
        let shown = view(&mut screen);
        assert!(
            shown.contains(&format!("Git {change} identity is stale")),
            "{shown}"
        );
        assert_eq!(index_bytes(root), index, "{change}");
        assert_eq!(disk(root, &paths), files);
    }
}

#[test]
fn discard_dialog_rejects_a_path_replaced_by_a_directory() {
    let (owner, _, _) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    key(&mut screen, KeyCode::Char('d'));
    assert!(view(&mut screen).contains(DIALOG));
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    std::fs::remove_file(root.join("change.rs")).unwrap();
    std::fs::create_dir(root.join("change.rs")).unwrap();
    write(
        root,
        "change.rs/inside.txt",
        "new descendant must survive\n",
    );
    confirm(&mut screen);
    let shown = view(&mut screen);
    assert!(shown.contains("path type is unsupported"), "{shown}");
    assert!(root.join("change.rs").is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs/inside.txt")).unwrap(),
        "new descendant must survive\n"
    );
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
}

#[test]
fn authorization_changes_keep_discard_dialog_and_other_form_input() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    screen.write_refusal = Some("Host denies discard".into());
    assert!(refused(&mut screen, 'd').contains("Host denies discard"));
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('d'));
    screen.write_refusal = Some("Host denies discard".into());
    key(&mut screen, KeyCode::Enter);
    assert!(!screen.busy());
    for (width, height) in SIZES {
        let shown = text(&screen.frame(width, height));
        for label in [DIALOG, "Host denies discard", "Hunk 1/2 · @@", "change.rs"] {
            assert!(shown.contains(label), "{width}×{height}: {shown}");
        }
    }
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
    screen.write_refusal = None;
    confirm(&mut screen);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working.replace("first_work", "line_005")
    );
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);

    // The submit authorization check must also keep existing editable forms and drafts.
    key(&mut screen, KeyCode::Char('c'));
    screen.event(UiEvent::Paste("Preserve this commit draft".into()));
    screen.write_refusal = Some("Host denies commit".into());
    control(&mut screen, KeyCode::Enter);
    let shown = view(&mut screen);
    assert!(
        shown.contains("Commit to main") && shown.contains("Preserve this commit draft")
    );
    assert!(!screen.busy());
    assert_eq!(index_bytes(root), index);
    // The refusal must be visible while the sheet keeps the draft.
    assert!(shown.contains("Host denies commit"), "{shown}");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(index_bytes(root), index);
}

#[test]
fn staged_historical_all_file_and_non_code_discard_refuse_without_writes() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    control(&mut screen, KeyCode::Char('h'));
    // In Changes, `d` asks to discard the selected file (DESIGN.md §5.2); Esc keeps it.
    key(&mut screen, KeyCode::Char('d'));
    assert!(
        view(&mut screen).contains(FILE_DIALOG),
        "{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Escape);
    // `D` is the marked set, and nothing is marked.
    assert!(refused(&mut screen, 'D').contains("No marked files"));
    control(&mut screen, KeyCode::Char('l'));
    key(&mut screen, KeyCode::Char('i'));
    ready_source(&mut screen, "let staged_040");
    seek_code(&mut screen, "let staged_040");
    ready_hunks(&mut screen);
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("Discard requires an unstaged diff"));
    }
    key(&mut screen, KeyCode::Char('F')); // every file
    ready_source(&mut screen, "let first_work");
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("Select a working-tree or index diff"));
    }
    key(&mut screen, KeyCode::Char('F'));
    ready_source(&mut screen, "let first_work");
    control(&mut screen, KeyCode::Char('h'));
    choose_file(&mut screen, "untracked.txt");
    ready_source(&mut screen, "untracked must never be deleted");
    control(&mut screen, KeyCode::Char('l'));
    for code in ['d', 'D'] {
        let shown = refused(&mut screen, code);
        assert!(shown.contains("untracked files"), "{shown}");
    }
    key(&mut screen, KeyCode::Char('4')); // History
    ready_source(&mut screen, "Baseline discard fixture");
    control(&mut screen, KeyCode::Char('l'));
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("Select a working-tree or index diff"));
    }
    key(&mut screen, KeyCode::Enter); // historical file list, whose key handler consumes keys
    control(&mut screen, KeyCode::Char('l'));
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("Select a working-tree or index diff"));
    }
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
}

#[test]
fn context_only_and_cross_hunk_ranges_refuse_without_applying_a_subset() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    seek_code(&mut screen, "let line_002");
    assert!(refused(&mut screen, 'D').contains("Select an added or removed line"));
    key(&mut screen, KeyCode::Char('V'));
    key(&mut screen, KeyCode::Down); // two context lines, no selected change
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("Select an added or removed line"));
    }
    key(&mut screen, KeyCode::Escape);
    seek_code(&mut screen, "let first_work");
    key(&mut screen, KeyCode::Char('V'));
    seek_code(&mut screen, "let second_work");
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("within one hunk"));
    }
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
}

#[test]
fn pending_file_layer_and_status_refreshes_refuse_discard_and_hide_wrong_source() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = open_code(root, "let first_work");
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    control(&mut screen, KeyCode::Char('h'));
    choose_file(&mut screen, "other.rs");
    let pending = view(&mut screen); // no poll: child results have not been accepted
    assert!(pending.contains("Loading…") && !pending.contains("let first_work"));
    control(&mut screen, KeyCode::Char('l'));
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("diff is refreshing"));
    }
    ready_source(&mut screen, "fn other_work");
    control(&mut screen, KeyCode::Char('h'));
    choose_file(&mut screen, "change.rs");
    ready_source(&mut screen, "let first_work");
    control(&mut screen, KeyCode::Char('l'));
    seek_code(&mut screen, "let first_work");
    ready_hunks(&mut screen);
    key(&mut screen, KeyCode::Char('i'));
    let pending = view(&mut screen);
    assert!(pending.contains("Loading…") && !pending.contains("let first_work"));
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("diff is refreshing"));
    }
    ready_source(&mut screen, "let staged_040");
    key(&mut screen, KeyCode::Char('i'));
    ready_source(&mut screen, "let first_work");
    seek_code(&mut screen, "let first_work");
    ready_hunks(&mut screen);
    let before = screen.frame(190, 48);
    key(&mut screen, KeyCode::Char('r'));
    // Same-source manual refresh retains the cursor/code, but must not authorize a reset.
    assert_eq!(code_field(&screen.frame(190, 48)), code_field(&before));
    for code in ['d', 'D'] {
        assert!(refused(&mut screen, code).contains("diff is refreshing"));
    }
    ready_source(&mut screen, "let first_work");
    assert_eq!(index_bytes(root), index);
    assert_eq!(disk(root, &OTHERS), others);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
}

#[test]
fn native_discard_cursor_geometry_and_help_are_usable_at_all_three_sizes() {
    let (owner, _, _) = mixed();
    let mut screen = open_code(owner.path(), "let first_work");
    for (width, height) in SIZES {
        seek_code(&mut screen, "let first_work");
        let before = screen.frame(width, height);
        assert!(code_field(&before).starts_with("let first_work"));
        let hints = footer(&before);
        // Narrow footers drop later hints to keep the context; `?` lists them all.
        let expected: &[&str] = if width < 100 {
            &["Space stage hunk"]
        } else {
            &["Space stage hunk", "d discard", "? keys"]
        };
        for hint in expected {
            assert!(hints.contains(hint), "{width}×{height}: {hints}");
        }
        assert!(
            !hints.contains("n N"),
            "find navigation only appears while a find is active"
        );
        let geometry = borders(&before);
        // Narrow layouts show the focused reader alone; wider ones keep the rail.
        let left = rail_width(width, height);
        let headings = ["Changes", "Branches", "History", "Stashes"].map(|heading| {
            (0..height).find(|&y| {
                left > 0
                    && before
                        .grid
                        .row_text(y)
                        .chars()
                        .take(usize::from(left))
                        .collect::<String>()
                        .contains(heading)
            })
        });
        assert_eq!(headings.iter().all(Option::is_some), left > 0, "{width}×{height}");
        key(&mut screen, KeyCode::Char('/'));
        screen.event(UiEvent::Paste("first_work".into()));
        key(&mut screen, KeyCode::Enter);
        let found = screen.frame(width, height);
        assert!(code_field(&found).starts_with("first_work"));
        let find_status = footer(&found);
        for hint in ["1/1", "n N"] {
            assert!(
                find_status.contains(hint),
                "{width}×{height}: {find_status}"
            );
        }
        let (x, y, _) = found.cursor.unwrap();
        assert_eq!(found.grid.get(x, y).unwrap().style, Style::Search);
        for code in ['n', 'N'] {
            key(&mut screen, KeyCode::Char(code));
            assert_eq!(screen.frame(width, height).cursor, found.cursor);
        }
        key(&mut screen, KeyCode::Char('?'));
        let help = text(&screen.frame(width, height));
        for scope in ["Discard hunk (confirm)", "Discard line (confirm)"] {
            assert!(help.contains(scope), "{width}×{height}: {help}");
        }
        key(&mut screen, KeyCode::Escape);
        let restored = screen.frame(width, height);
        assert_eq!(restored.cursor, found.cursor);
        assert_eq!(borders(&restored), geometry);
        for (heading, y) in ["Changes", "Branches", "History", "Stashes"]
            .into_iter()
            .zip(headings)
        {
            if let Some(y) = y {
                assert!(
                    restored
                        .grid
                        .row_text(y)
                        .chars()
                        .take(usize::from(left))
                        .collect::<String>()
                        .contains(heading)
                );
            }
        }
        key(&mut screen, KeyCode::Char('d'));
        assert!(text(&screen.frame(width, height)).contains(DIALOG));
        key(&mut screen, KeyCode::Escape);
        let cancelled = screen.frame(width, height);
        assert_eq!(cancelled.cursor, found.cursor);
        assert_eq!(borders(&cancelled), geometry);
        key(&mut screen, KeyCode::Char('V'));
        let range = footer(&screen.frame(width, height));
        for hint in ["Range started", "stage selected diff lines"] {
            assert!(range.contains(hint), "{width}×{height}: {range}");
        }
        key(&mut screen, KeyCode::Escape); // range, not source find
        key(&mut screen, KeyCode::Home);
        assert!(code_field(&screen.frame(width, height)).starts_with("let line_002"));
        key(&mut screen, KeyCode::End);
        assert!(code_field(&screen.frame(width, height)).starts_with("let line_103"));
        key(&mut screen, KeyCode::Escape); // clear source find before the next geometry
    }
}

/// Confirm an open Changes-pane file discard and wait for its receipt and the refresh.
fn confirm_files(screen: &mut GitScreen, receipt: &str) {
    key(screen, KeyCode::Enter);
    wait(screen, |screen| {
        let shown = view(screen);
        !screen.busy() && shown.contains(receipt) && !shown.contains("Discard changes in")
    });
}

#[test]
fn changes_d_confirms_then_discards_a_tracked_file_and_deletes_an_untracked_one() {
    let (owner, staged, working) = mixed();
    let root = owner.path();
    let mut screen = GitScreen::open(root).unwrap();
    ready_source(&mut screen, "let first_work");
    // Staged content never changes; restoring a file may refresh its stat cache, as
    // `git restore` does, so compare what is staged rather than the raw index file.
    let index = git(root, &["ls-files", "--stage"]);

    // A tracked file with only worktree changes: the sheet names it and the scope.
    choose_file(&mut screen, "other.rs");
    key(&mut screen, KeyCode::Char('d'));
    assert!(!screen.busy());
    for (width, height) in SIZES {
        let frame = screen.frame(width, height);
        let shown = text(&frame);
        for label in [
            "Discard changes in other.rs?",
            "Target",
            "other.rs · M",
            "Worktree only; the index keeps its copy.",
            "Discard",
            "Cancel",
        ] {
            assert!(shown.contains(label), "{width}×{height}: {shown}");
        }
        assert!(!shown.contains("Deleted from disk"), "{width}×{height}: {shown}");
        // The native cursor rests on the primary Discard button.
        let (x, y, _) = frame.cursor.expect("confirmation button cursor");
        let row = frame.grid.row_text(y);
        assert!(
            row.chars()
                .skip(usize::from(x))
                .collect::<String>()
                .starts_with("Discard"),
            "{width}×{height}: {row}"
        );
    }
    // Esc keeps everything.
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Discard changes in"));
    assert_eq!(
        std::fs::read_to_string(root.join("other.rs")).unwrap(),
        "fn other_work() {}\n"
    );
    key(&mut screen, KeyCode::Char('d'));
    confirm_files(&mut screen, "Discarded worktree changes in other.rs");
    assert_eq!(
        std::fs::read_to_string(root.join("other.rs")).unwrap(),
        "fn other_base() {}\n"
    );
    assert_eq!(git(root, &["ls-files", "--stage"]), index, "the index keeps its copy");
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
    assert_eq!(
        std::fs::read_to_string(root.join("untracked.txt")).unwrap(),
        "untracked must never be deleted\n"
    );

    // An untracked file is deleted, and the sheet says so before anything happens.
    choose_file(&mut screen, "untracked.txt");
    key(&mut screen, KeyCode::Char('d'));
    for (width, height) in SIZES {
        let shown = text(&screen.frame(width, height));
        for label in [
            "Delete untracked file untracked.txt?",
            "untracked.txt · untracked",
            "Deleted from disk; Git keeps no copy.",
        ] {
            assert!(shown.contains(label), "{width}×{height}: {shown}");
        }
    }
    assert!(root.join("untracked.txt").exists());
    confirm_files(&mut screen, "Deleted untracked file untracked.txt");
    assert!(!root.join("untracked.txt").exists());
    assert_eq!(git(root, &["ls-files", "--stage"]), index);

    // Staged and unstaged: only the worktree side goes; the staged change stays.
    choose_file(&mut screen, "change.rs");
    key(&mut screen, KeyCode::Char('d'));
    let shown = view(&mut screen);
    assert!(
        shown.contains("Discard changes in change.rs?") && shown.contains("staged part kept"),
        "{shown}"
    );
    confirm_files(&mut screen, "Discarded worktree changes in change.rs");
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        staged
    );
    assert_eq!(git(root, &["show", ":change.rs"]), staged.trim_end());
    assert_eq!(git(root, &["ls-files", "--stage"]), index);
    assert_eq!(
        std::fs::read_to_string(root.join("keep-index.txt")).unwrap(),
        "keep staged\n"
    );

    // Only staged changes remain: nothing in the worktree to discard.
    choose_file(&mut screen, "change.rs");
    let shown = refused(&mut screen, 'd');
    assert!(shown.contains("only staged changes"), "{shown}");
    assert!(!shown.contains("Discard changes in"), "{shown}");
}

#[test]
fn changes_marked_set_and_rename_discard_with_one_confirmation() {
    let (owner, staged, _) = mixed();
    let root = owner.path();
    // other.rs moves with its worktree edit: a staged rename plus an unstaged change (RM).
    git(root, &["mv", "other.rs", "moved.rs"]);
    let mut screen = GitScreen::open(root).unwrap();
    ready_source(&mut screen, "let first_work");
    // `git mv` leaves the moved entry's stat cache unset, so compare what is staged.
    let index = git(root, &["ls-files", "--stage"]);

    choose_file(&mut screen, "moved.rs");
    key(&mut screen, KeyCode::Char('d'));
    let shown = view(&mut screen);
    for label in [
        "Discard changes in moved.rs?",
        "moved.rs · RM · renamed from other.rs",
        "Worktree only; the index keeps its copy.",
    ] {
        assert!(shown.contains(label), "{shown}");
    }
    key(&mut screen, KeyCode::Escape);

    // `D` without marks refuses; with marks, one sheet covers the whole set.
    assert!(refused(&mut screen, 'D').contains("No marked files"));
    for path in ["moved.rs", "untracked.txt"] {
        choose_file(&mut screen, path);
        key(&mut screen, KeyCode::Char('x'));
        assert_eq!(selected_file(&mut screen).1, "✓");
    }
    key(&mut screen, KeyCode::Char('D'));
    for (width, height) in SIZES {
        let shown = text(&screen.frame(width, height));
        for label in [
            "Discard changes in 2 files?",
            "2 files",
            "Deleted from disk; Git keeps no copy.",
        ] {
            assert!(shown.contains(label), "{width}×{height}: {shown}");
        }
    }
    let shown = view(&mut screen);
    for label in ["moved.rs · RM", "untracked.txt · untracked"] {
        assert!(shown.contains(label), "{shown}");
    }
    confirm_files(&mut screen, "Discarded changes in 2 files · 1 untracked deleted");
    assert_eq!(
        std::fs::read_to_string(root.join("moved.rs")).unwrap(),
        "fn other_base() {}\n"
    );
    assert!(!root.join("other.rs").exists(), "the staged rename stays");
    assert!(!root.join("untracked.txt").exists());
    assert_eq!(git(root, &["ls-files", "--stage"]), index);
    assert_eq!(git(root, &["show", ":change.rs"]), staged.trim_end());
    let status = git(root, &["status", "--porcelain"]);
    assert!(status.contains("R  other.rs -> moved.rs"), "{status}");
    assert!(!status.contains("??"), "{status}");
}

#[test]
fn changes_discard_rechecks_host_authorization_and_refuses_changed_files() {
    let (owner, _, working) = mixed();
    let root = owner.path();
    let mut screen = GitScreen::open(root).unwrap();
    ready_source(&mut screen, "let first_work");
    let index = index_bytes(root);
    let others = disk(root, &OTHERS);
    choose_file(&mut screen, "other.rs");

    // Refused before the sheet opens.
    for reason in ["Host denies discard", "Host changed to read-only"] {
        screen.write_refusal = Some(reason.into());
        let shown = refused(&mut screen, 'd');
        assert!(shown.contains(reason) && !shown.contains("Discard changes in"), "{shown}");
    }
    // Refused at confirmation: the sheet stays, shows why, and nothing is written.
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('d'));
    for reason in ["Host denies discard", "Host changed to read-only"] {
        screen.write_refusal = Some(reason.into());
        key(&mut screen, KeyCode::Enter);
        assert!(!screen.busy());
        for (width, height) in SIZES {
            let shown = text(&screen.frame(width, height));
            for label in ["Discard changes in other.rs?", reason] {
                assert!(shown.contains(label), "{width}×{height}: {shown}");
            }
        }
        assert_eq!(index_bytes(root), index);
        assert_eq!(disk(root, &OTHERS), others);
    }
    screen.write_refusal = None;

    // A file that changed after the sheet opened is not discarded.
    write(root, "other.rs", "fn external_edit() {}\n");
    key(&mut screen, KeyCode::Enter);
    let shown = view(&mut screen);
    assert!(
        shown.contains("changed since this sheet opened"),
        "{shown}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("other.rs")).unwrap(),
        "fn external_edit() {}\n"
    );
    assert_eq!(index_bytes(root), index);
    assert_eq!(
        std::fs::read_to_string(root.join("change.rs")).unwrap(),
        working
    );
    key(&mut screen, KeyCode::Escape);

    // A path replaced by a directory is refused, and its contents survive.
    choose_file(&mut screen, "untracked.txt");
    key(&mut screen, KeyCode::Char('d'));
    std::fs::remove_file(root.join("untracked.txt")).unwrap();
    std::fs::create_dir(root.join("untracked.txt")).unwrap();
    write(root, "untracked.txt/inside.txt", "new descendant must survive\n");
    key(&mut screen, KeyCode::Enter);
    let shown = view(&mut screen);
    assert!(shown.contains("is now a directory"), "{shown}");
    assert_eq!(
        std::fs::read_to_string(root.join("untracked.txt/inside.txt")).unwrap(),
        "new descendant must survive\n"
    );
    assert_eq!(index_bytes(root), index);
}
