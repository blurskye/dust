//! Source find through the public native input/frame boundary, against real repositories.
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{Emphasis, Frame, GitRole, GitSurface, KeyInputKind, Style, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const SIZES: [(u16, u16); 3] = [(64, 20), (100, 30), (190, 48)];

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
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
fn write(root: &Path, name: &str, source: &str) {
    std::fs::write(root.join(name), source).unwrap();
}
fn repository() -> tempfile::TempDir {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", "Source Search Test"]);
    git(root, &["config", "user.email", "search@example.invalid"]);
    write(root, "a.rs", "fn alpha_original() {}\n");
    write(root, "b.rs", "fn beta_original() {}\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Metadata-only initial subject"]);
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
            "source preview timed out:\n{}",
            view(screen)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn open(root: &Path, source: &str) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    wait(&mut screen, |screen| view(screen).contains(source));
    control(&mut screen, KeyCode::Char('l'));
    screen
}
fn find(screen: &mut GitScreen, query: &str) {
    assert_eq!(key(screen, KeyCode::Char('/')), ScreenOutcome::Continue);
    assert!(view(screen).contains("Find source"));
    control(screen, KeyCode::Char('u'));
    paste(screen, query);
    assert_eq!(key(screen, KeyCode::Enter), ScreenOutcome::Continue);
    assert!(
        !view(screen).contains("Find source"),
        "Enter applies the native find form"
    );
    assert!(!screen.busy(), "finding source must not start a write");
}
fn cursor(frame: &Frame) -> (u16, u16, String) {
    let (x, y, _) = frame.cursor.expect("real native cursor");
    let cell = frame.grid.get(x, y).unwrap();
    assert!(!cell.continuation, "cursor must start a complete grapheme");
    let suffix = (x..frame.grid.width())
        .filter_map(|at| frame.grid.get(at, y))
        .filter(|cell| !cell.continuation)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    (x, y, suffix.trim_end().into())
}
fn matched(screen: &mut GitScreen, size: (u16, u16), expected: &str) -> (u16, u16, String) {
    let frame = screen.frame(size.0, size.1);
    let found = cursor(&frame);
    assert!(
        found.0 > reader_x(size.0, size.1),
        "source, not a list/gutter cursor"
    );
    assert!(found.1 < size.1 - 1, "match is revealed inside the preview");
    assert_eq!(
        frame.grid.get(found.0, found.1).unwrap().style,
        Style::Search
    );
    assert!(
        found.2.starts_with(expected),
        "expected {expected:?} at cursor, got {found:?}\n{}",
        text(&frame)
    );
    found
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
/// Every pane rectangle (its four rounded corners) and whether it has focus.
fn borders(frame: &Frame) -> Vec<(u16, u16, String, bool)> {
    let mut result = Vec::new();
    for y in 0..frame.grid.height() - 1 {
        for x in 0..frame.grid.width() {
            let cell = frame.grid.get(x, y).unwrap();
            if matches!(cell.symbol.as_str(), "╭" | "╮" | "╰" | "╯") {
                result.push((x, y, cell.symbol.clone(), focus_role(cell.style)));
            }
        }
    }
    // Source frames focus the reader: its border carries the accent.
    let reader = reader_x(frame.grid.width(), frame.grid.height());
    assert!(
        result.iter().any(|(x, y, symbol, focused)| *x == reader
            && *y == 1
            && symbol == "╭"
            && *focused),
        "focused reader border"
    );
    result
}
fn focus_role(style: Style) -> bool {
    matches!(
        style,
        Style::Git {
            role: GitRole::Focus,
            ..
        }
    )
}
/// The footer: while a find is active its right side is `/query/ · i/n · n N`.
fn status(screen: &mut GitScreen) -> String {
    screen.frame(190, 48).grid.row_text(47)
}
fn finding(screen: &mut GitScreen) -> bool {
    status(screen).trim_end().ends_with("· n N")
}
#[derive(Debug, PartialEq)]
struct RepositoryState {
    head_file: Vec<u8>,
    head: String,
    refs: String,
    index: Vec<u8>,
    status: String,
    files: Vec<Option<Vec<u8>>>,
}
fn state(root: &Path) -> RepositoryState {
    RepositoryState {
        head_file: std::fs::read(root.join(".git/HEAD")).unwrap(),
        head: git(root, &["rev-parse", "HEAD"]),
        refs: git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        index: std::fs::read(root.join(".git/index")).unwrap(),
        status: git(root, &["status", "--porcelain=v1", "--untracked-files=all"]),
        files: ["a.rs", "b.rs", "z.rs", "zz-refresh.txt"]
            .iter()
            .map(|name| std::fs::read(root.join(name)).ok())
            .collect(),
    }
}

#[test]
fn multiple_occurrences_next_previous_wrap_and_fixed_geometry_at_all_sizes() {
    let owner = repository();
    let root = owner.path();
    let middle = (0..60)
        .map(|n| format!("let line_{n:02} = {n};\n"))
        .collect::<String>();
    write(
        root,
        "a.rs",
        &format!("\tlet needle = \"needle\";\n{middle}let final_hit = needle;\n"),
    );
    let before = state(root);
    for size in SIZES {
        let mut screen = open(root, "let needle");
        let geometry = borders(&screen.frame(size.0, size.1));
        find(&mut screen, "needle");
        let first = matched(&mut screen, size, "needle =");
        assert!(text(&screen.frame(size.0, size.1)).contains("1/3 · n N"));
        key(&mut screen, KeyCode::Char('n'));
        let second = matched(&mut screen, size, "needle\"");
        assert_eq!(first.1, second.1, "two distinct matches on one source row");
        assert!(second.0 > first.0);
        key(&mut screen, KeyCode::Char('n'));
        matched(&mut screen, size, "needle;");
        assert!(text(&screen.frame(size.0, size.1)).contains("3/3 · n N"));
        key(&mut screen, KeyCode::Char('n'));
        matched(&mut screen, size, "needle =");
        assert!(text(&screen.frame(size.0, size.1)).contains("1/3 · n N"));
        key(&mut screen, KeyCode::Char('N'));
        matched(&mut screen, size, "needle;");
        key(&mut screen, KeyCode::Char('N'));
        matched(&mut screen, size, "needle\"");
        assert_eq!(
            borders(&screen.frame(size.0, size.1)),
            geometry,
            "find must not resize panes at {size:?}"
        );
    }
    assert_eq!(state(root), before);
}

#[test]
fn cancel_no_match_empty_query_and_literal_case_sensitive_find_preserve_the_code_cursor() {
    let owner = repository();
    let root = owner.path();
    write(
        root,
        "a.rs",
        "    let needle = \"needle\";\nlet literal = \"[a-z].*\";\n",
    );
    let before = state(root);
    let mut screen = open(root, "let needle");
    screen.write_refusal = Some("Repository writes are refused for this host".into());
    find(&mut screen, "needle");
    key(&mut screen, KeyCode::Char('n'));
    let original = cursor(&screen.frame(100, 30));
    key(&mut screen, KeyCode::Char('/'));
    control(&mut screen, KeyCode::Char('u'));
    // These are source-find input, not stage/commit/index/range commands.
    for c in "a c i V n/N missing".chars() {
        key(&mut screen, KeyCode::Char(c));
    }
    key(&mut screen, KeyCode::Escape);
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    assert!(status(&mut screen).contains("/needle/ · 2/2"));
    key(&mut screen, KeyCode::Char('/'));
    key(&mut screen, KeyCode::Tab); // Find button.
    key(&mut screen, KeyCode::Tab); // Cancel button.
    key(&mut screen, KeyCode::Enter);
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    assert!(status(&mut screen).contains("/needle/ · 2/2"));
    find(&mut screen, "NEEDLE");
    assert!(status(&mut screen).contains("no matches"));
    assert_eq!(
        cursor(&screen.frame(100, 30)),
        original,
        "a failed find leaves the actual code cursor alone"
    );
    for c in ['n', 'N'] {
        key(&mut screen, KeyCode::Char(c));
    }
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    assert_eq!(key(&mut screen, KeyCode::Escape), ScreenOutcome::Continue);
    assert!(!finding(&mut screen));
    assert_eq!(
        cursor(&screen.frame(100, 30)),
        original,
        "clearing find does not leave the preview"
    );
    find(&mut screen, "[a-z].*");
    matched(&mut screen, (100, 30), "[a-z].*");
    let literal = cursor(&screen.frame(100, 30));
    find(&mut screen, "");
    assert!(!finding(&mut screen));
    assert_eq!(cursor(&screen.frame(100, 30)), literal);
    // Ctrl+Enter is also a read-only submit, even under host write refusal.
    key(&mut screen, KeyCode::Char('/'));
    paste(&mut screen, "needle");
    control(&mut screen, KeyCode::Enter);
    assert!(!view(&mut screen).contains("Repository writes are refused"));
    matched(&mut screen, (100, 30), "needle");
    key(&mut screen, KeyCode::Char('c'));
    assert!(
        view(&mut screen).contains("Nothing staged · Space stages the selected change"),
        "c remains the global commit action (refused: nothing is staged)"
    );
    key(&mut screen, KeyCode::Escape);
    assert_eq!(state(root), before);
}

#[test]
fn unicode_tabs_long_lines_and_both_split_sides_keep_the_cursor_on_the_matching_grapheme() {
    let owner = repository();
    let root = owner.path();
    let prefix = format!("\t  界 e\u{301} 👩‍💻 {}", "padding_".repeat(30));
    write(
        root,
        "a.rs",
        &format!("{prefix}旧印_tail_after_the_match\n"),
    );
    git(root, &["add", "a.rs"]);
    git(root, &["commit", "-m", "Long Unicode source"]);
    write(
        root,
        "a.rs",
        &format!("{prefix}新印_tail_after_the_match\n"),
    );
    let before = state(root);
    let mut screen = open(root, "padding_");
    find(&mut screen, "旧印");
    for size in SIZES {
        matched(&mut screen, size, "旧印");
        let frame = screen.frame(size.0, size.1);
        let (_, y, _) = cursor(&frame);
        assert!(
            frame.grid.row_text(y).contains('‹'),
            "long source is horizontally revealed"
        );
    }
    key(&mut screen, KeyCode::Char('s'));
    let removed = matched(&mut screen, (190, 48), "旧印");
    // The split divider sits in the middle of the reader's interior.
    let inner = reader_x(190, 48) + 1;
    let middle = inner + (190 - 1 - inner) / 2;
    assert!(
        removed.0 < middle,
        "a removed-line match must stay in BEFORE, not normalize onto AFTER"
    );
    find(&mut screen, "新印");
    assert!(matched(&mut screen, (190, 48), "新印").0 > middle);
    for size in SIZES {
        matched(&mut screen, size, "新印");
    }
    key(&mut screen, KeyCode::Char('s'));
    key(&mut screen, KeyCode::Home);
    find(&mut screen, "界");
    // Reader interior + range edge1 + number3 + two spaces + sign1 + space1.
    let code = reader_x(100, 30) + 1 + 8;
    let indented = matched(&mut screen, (100, 30), "界 e\u{301}");
    assert_eq!(
        indented.0,
        code + 6,
        "tab stops are source-relative, after unified numbers and marker"
    );
    for size in [(64, 20), (79, 20)] {
        let compact = matched(&mut screen, size, "界 e\u{301}");
        // Narrow layouts show the reader alone: border0, interior from 1,
        // then the same gutter and six source-relative indentation cells as at 100.
        // Readers narrower than 70 interior cells keep one number column.
        let frame = screen.frame(size.0, size.1);
        let sign = if size.0 - 2 < 70 { 7 } else { 12 };
        assert_eq!(compact.0, sign + 2 + 6, "{}", text(&frame));
        assert_eq!(frame.grid.get(4, compact.1).unwrap().symbol, "1");
        assert!(matches!(
            frame.grid.get(sign, compact.1).unwrap().symbol.as_str(),
            "+" | "−"
        ));
        assert_eq!(frame.grid.get(0, compact.1).unwrap().symbol, "│");
    }
    find(&mut screen, "e\u{301}");
    matched(&mut screen, (100, 30), "e\u{301}");
    find(&mut screen, "\u{301}");
    matched(&mut screen, (100, 30), "e\u{301}");
    find(&mut screen, "💻");
    matched(&mut screen, (100, 30), "👩‍💻");
    find(&mut screen, "\t  界");
    let tab = matched(&mut screen, (100, 30), "      界");
    assert_eq!(
        tab.0, code,
        "matching indentation is code, not the number or diff gutter"
    );
    assert_eq!(state(root), before);
}

#[test]
fn unchanged_refresh_keeps_matches_but_file_layer_and_changed_content_do_not_reuse_them() {
    let owner = repository();
    let root = owner.path();
    write(root, "a.rs", "let staged_only = 1;\n");
    git(root, &["add", "a.rs"]);
    write(root, "a.rs", "    let working_only = 2;\n");
    write(root, "b.rs", "let beta_working = 3;\n");
    let mut screen = open(root, "working_only");
    find(&mut screen, "working_only");
    let original = cursor(&screen.frame(100, 30));
    write(root, "z.rs", "let refresh_marker = 4;\n");
    let before = state(root);
    key(&mut screen, KeyCode::Char('r'));
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    wait(&mut screen, |screen| {
        let displayed = view(screen);
        displayed.contains("z.rs") && !displayed.contains("updating…")
    });
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    assert!(status(&mut screen).contains("/working_only/ · 1/1"));
    key(&mut screen, KeyCode::Char('i'));
    let loading = view(&mut screen);
    assert!(loading.contains("Loading…") && !loading.contains("let working_only"));
    assert!(!finding(&mut screen));
    // Loading is not source, even when queried before the child result is polled.
    find(&mut screen, "Loading");
    assert!(status(&mut screen).contains("no matches"));
    wait(&mut screen, |screen| {
        view(screen).contains("let staged_only")
    });
    assert!(!status(&mut screen).contains("/Loading/"));
    find(&mut screen, "staged_only");
    matched(&mut screen, (100, 30), "staged_only");
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |screen| {
        view(screen).contains("let working_only")
    });
    assert!(!finding(&mut screen));
    find(&mut screen, "working_only");
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down);
    assert!(!view(&mut screen).contains("let working_only"));
    control(&mut screen, KeyCode::Char('l'));
    assert!(!finding(&mut screen));
    wait(&mut screen, |screen| {
        view(screen).contains("let beta_working")
    });
    find(&mut screen, "beta_working");
    assert_eq!(state(root), before);
    write(
        root,
        "b.rs",
        "let entirely_new_source = 5;\nlet another_new_line = 6;\n",
    );
    let after_edit = state(root);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| {
        view(screen).contains("entirely_new_source")
    });
    assert!(
        !finding(&mut screen),
        "changed source must not retain old row/byte matches"
    );
    key(&mut screen, KeyCode::Char('n'));
    assert!(!view(&mut screen).contains("beta_working"));
    assert_eq!(state(root), after_edit);
}

#[test]
fn untracked_source_refresh_and_all_file_source_are_searchable_but_the_heading_is_not() {
    let owner = repository();
    let root = owner.path();
    write(root, "z.rs", "\tlet untracked_hit = \"untracked_hit\";\n");
    let mut screen = open(root, "untracked_hit");
    find(&mut screen, "z.rs");
    assert!(status(&mut screen).contains("no matches"));
    find(&mut screen, "untracked_hit");
    key(&mut screen, KeyCode::Char('n'));
    let original = cursor(&screen.frame(100, 30));
    write(root, "zz-refresh.txt", "read-only refresh marker\n");
    let before = state(root);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| {
        view(screen).contains("zz-refresh.txt")
    });
    assert_eq!(cursor(&screen.frame(100, 30)), original);
    assert!(status(&mut screen).contains("/untracked_hit/ · 2/2"));
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Char('F')); // every file
    wait(&mut screen, |screen| view(screen).contains("a.rs"));
    key(&mut screen, KeyCode::Char('/'));
    paste(&mut screen, "a.rs");
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| {
        view(screen).contains("fn alpha_original")
    });
    control(&mut screen, KeyCode::Char('l'));
    find(&mut screen, "alpha_original");
    for size in SIZES {
        matched(&mut screen, size, "alpha_original");
    }
    assert_eq!(state(root), before);
}

#[test]
fn historical_files_support_find_refresh_and_file_switching_without_searching_commit_metadata() {
    let owner = repository();
    let root = owner.path();
    write(root, "a.rs", "let historical_alpha = 1;\n");
    write(root, "b.rs", "let historical_beta = historical_beta;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "Metadata-only reviewed subject"]);
    write(root, "b.rs", "let current_disk_only = 9;\n");
    let before = state(root);
    let mut screen = GitScreen::open(root).unwrap();
    key(&mut screen, KeyCode::Char('4')); // History
    wait(&mut screen, |screen| {
        view(screen).contains("historical_alpha")
    });
    key(&mut screen, KeyCode::Enter);
    // Focused list / must still be its native identity-preserving file filter.
    key(&mut screen, KeyCode::Char('/'));
    assert!(status(&mut screen).contains("Enter keep · Esc clear"));
    paste(&mut screen, "b.rs");
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| {
        view(screen).contains("let historical_beta")
    });
    control(&mut screen, KeyCode::Char('l'));
    screen.write_refusal = Some("host blocks every repository write".into());
    for query in [
        "Metadata-only reviewed subject",
        "Source Search Test",
        "@@",
        "b.rs",
        "index ",
    ] {
        find(&mut screen, query);
        assert!(
            status(&mut screen).contains("no matches"),
            "metadata is not source: {query}"
        );
    }
    find(&mut screen, "historical_beta");
    key(&mut screen, KeyCode::Char('n'));
    for size in SIZES {
        matched(&mut screen, size, "historical_beta;");
    }
    let original = cursor(&screen.frame(100, 30));
    key(&mut screen, KeyCode::Char('r'));
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        screen.poll();
        assert_eq!(cursor(&screen.frame(100, 30)), original);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(status(&mut screen).contains("/historical_beta/ · 2/2"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        cursor(&screen.frame(100, 30)),
        original,
        "first Esc clears find, not historical inspection"
    );
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Escape); // Clear the file filter and restore a.rs.
    wait(&mut screen, |screen| {
        view(screen).contains("let historical_alpha")
    });
    control(&mut screen, KeyCode::Char('l'));
    find(&mut screen, "historical_alpha");
    key(&mut screen, KeyCode::Char('J'));
    assert!(!view(&mut screen).contains("let historical_alpha"));
    assert!(!finding(&mut screen));
    wait(&mut screen, |screen| {
        view(screen).contains("let historical_beta")
    });
    assert_eq!(state(root), before);
}

#[test]
fn list_filter_restores_its_original_file_and_reflog_n_is_still_rescue_not_find() {
    let owner = repository();
    let root = owner.path();
    write(root, "a.rs", "let alpha_working = 1;\n");
    write(root, "b.rs", "let beta_working = 2;\n");
    let before = state(root);
    let mut screen = open(root, "alpha_working");
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |screen| {
        view(screen).contains("let beta_working")
    });
    key(&mut screen, KeyCode::Char('/'));
    paste(&mut screen, "a.rs");
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |screen| {
        view(screen).contains("let alpha_working")
    });
    control(&mut screen, KeyCode::Char('l'));
    find(&mut screen, "alpha_working");
    key(&mut screen, KeyCode::Escape);
    control(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Escape);
    wait(&mut screen, |screen| {
        view(screen).contains("let beta_working")
    });
    let frame = screen.frame(100, 30);
    let (_, y, _) = frame.cursor.unwrap();
    assert!(
        frame
            .grid
            .row_text(y)
            .chars()
            .take(30)
            .collect::<String>()
            .contains("b.rs")
    );
    key(&mut screen, KeyCode::Char('4')); // History
    key(&mut screen, KeyCode::Char(']')); // Reflog
    wait(&mut screen, |screen| view(screen).contains("HEAD@{0}"));
    key(&mut screen, KeyCode::Char('n'));
    assert!(view(&mut screen).contains("Create rescue branch at commit"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(state(root), before);
}

#[test]
fn summaries_diff_headers_and_binary_metadata_are_not_source_matches() {
    let owner = repository();
    let root = owner.path();
    write(root, "a.rs", "let changed_source = 1;\n");
    let mut screen = open(root, "let changed_source");
    let before = state(root);
    for query in [
        "a.rs",
        "@@",
        "working tree",
        "index ",
        "Changes",
        "No source matches",
    ] {
        find(&mut screen, query);
        assert!(
            status(&mut screen).contains("no matches"),
            "not source: {query}"
        );
    }
    assert_eq!(state(root), before);
    std::fs::write(root.join("a.rs"), b"\0binary\0payload\n").unwrap();
    let binary = state(root);
    key(&mut screen, KeyCode::Char('r'));
    wait(&mut screen, |screen| view(screen).contains("Binary files"));
    find(&mut screen, "Binary files");
    assert!(status(&mut screen).contains("no matches"));
    assert_eq!(state(root), binary);
}

#[cfg(unix)]
#[test]
fn unavailable_source_messages_are_not_matches() {
    let owner = repository();
    let root = owner.path();
    std::os::unix::fs::symlink(root.join("a.rs"), root.join("z.rs")).unwrap();
    let before = state(root);
    let mut screen = open(root, "Preview unavailable for a symlink");
    find(&mut screen, "symlink");
    assert!(status(&mut screen).contains("no matches"));
    for size in SIZES {
        let frame = screen.frame(size.0, size.1);
        for y in 0..size.1 {
            for x in 0..size.0 {
                assert_ne!(frame.grid.get(x, y).unwrap().style, Style::Search);
            }
        }
    }
    assert_eq!(state(root), before);
}

#[test]
fn active_hunk_scope_survives_find_clear_and_tracks_the_native_match_without_geometry_changes() {
    let owner = repository();
    let root = owner.path();
    let base = (0..72)
        .map(|n| format!("let value_{n:02} = {n};\n"))
        .collect::<String>();
    write(root, "a.rs", &base);
    git(root, &["add", "a.rs"]);
    git(root, &["commit", "-m", "Two separated source changes"]);
    write(
        root,
        "a.rs",
        &base
            .replace("let value_12 = 12;", "let needle_first = 12;")
            .replace("let value_48 = 48;", "let needle_second = 48;"),
    );
    let before = state(root);
    for size in SIZES {
        let mut screen = open(root, "needle_first");
        find(&mut screen, "needle");
        let found = screen.frame(size.0, size.1);
        let geometry = borders(&found);
        let (_, y, _) = matched(&mut screen, size, "needle_first");
        let inner = reader_x(size.0, size.1) + 1;
        // Hunk bands start their `@@` range one cell into the reader; only the
        // cursor's hunk is bold.
        let band = inner + 1;
        let active = |frame: &Frame, row: u16| {
            frame.grid.get(band, row).unwrap().style
                == Style::Git {
                    role: GitRole::Focus,
                    surface: GitSurface::HunkBand,
                    emphasis: Emphasis::Bold,
                }
        };
        // The cursor row carries the cursor fill from the gutter on.
        let cursor_row = |frame: &Frame, row: u16, x: u16| {
            matches!(
                frame.grid.get(x, row).unwrap().style,
                Style::Git {
                    surface: GitSurface::Cursor,
                    ..
                }
            )
        };
        assert!(cursor_row(&found, y, band));
        let visible_headers = (0..size.1 - 1)
            .filter(|&row| found.grid.row_text(row).contains("@@"))
            .collect::<Vec<_>>();
        assert!(!visible_headers.is_empty());
        assert_eq!(
            visible_headers
                .iter()
                .filter(|&&row| active(&found, row))
                .count(),
            1,
            "only the cursor's hunk owns an active header"
        );
        if size.0 == 190 {
            assert_eq!(visible_headers.len(), 2, "both hunks fit at wide size");
            assert_eq!(
                found.grid.get(band, visible_headers[1]).unwrap().style,
                Style::Git {
                    role: GitRole::Focus,
                    surface: GitSurface::HunkBand,
                    emphasis: Emphasis::None,
                }
            );
        }
        key(&mut screen, KeyCode::Escape); // clear Find, not the native source cursor
        let cleared = screen.frame(size.0, size.1);
        assert_eq!(cleared.cursor, found.cursor);
        assert!(cursor_row(&cleared, y, band));
        assert_eq!(borders(&cleared), geometry);
        let (x, y, _) = cleared.cursor.unwrap();
        assert_ne!(cleared.grid.get(x, y).unwrap().style, Style::Search);
        find(&mut screen, "needle");
        key(&mut screen, KeyCode::Char('n'));
        let second = matched(&mut screen, size, "needle_second");
        let moved = screen.frame(size.0, size.1);
        assert!(cursor_row(&moved, second.1, band));
        assert_eq!(borders(&moved), geometry);
        if size.0 == 190 {
            key(&mut screen, KeyCode::Char('s'));
            matched(&mut screen, size, "needle_second");
            let split = screen.frame(size.0, size.1);
            let (x, y, _) = split.cursor.unwrap();
            let middle = inner + (size.0 - 1 - inner) / 2;
            assert!(x > middle, "added match belongs to the AFTER side");
            assert!(cursor_row(&split, y, middle + 2));
        }
    }
    assert_eq!(state(root), before);
}
