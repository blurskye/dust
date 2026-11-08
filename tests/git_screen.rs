use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().into()
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Screen Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Initial commit"]);
    dir
}
fn key(screen: &mut GitScreen, code: KeyCode) -> ScreenOutcome {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    })
}
fn text(screen: &mut GitScreen, text: &str) {
    screen.event(UiEvent::Paste(text.into()));
}
fn submit(screen: &mut GitScreen) {
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
}
fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(120, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(screen: &mut GitScreen, ready: impl Fn(&mut GitScreen) -> bool) {
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

#[test]
fn compact_help_and_long_names_preserve_selection() {
    let dir = repo();
    let path = format!("{}-tail.rs", "long文件".repeat(12));
    std::fs::write(dir.path().join(&path), "fn visible() {}\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("fn visible()"));
    let before = screen.frame(80, 24);
    let (_, y, _) = before.cursor.unwrap();
    assert!(before.grid.row_text(y).contains('…'));
    let footer = before.grid.row_text(22);
    assert!(footer.contains("keys") && footer.contains("refresh") && footer.contains("close"));
    key(&mut screen, KeyCode::Char('?'));
    let help = screen.frame(80, 24);
    let text = (0..24)
        .map(|y| help.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("Keyboard")
            && text.contains("Split / unified")
            && text.contains("Ctrl+h/j/k/l")
    );
    assert!(help.cursor.is_some());
    key(&mut screen, KeyCode::Escape);
    let after = screen.frame(80, 24);
    assert_eq!(before.cursor, after.cursor);
    for y in 0..24 {
        assert_eq!(before.grid.row_text(y), after.grid.row_text(y));
    }
}

#[test]
fn preview_focus_updates_index_diff_when_toggling_layers() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "index version\n").unwrap();
    git(dir.path(), &["add", "file.txt"]);
    std::fs::write(dir.path().join("file.txt"), "working version\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("+working version"));
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| {
        view(s).contains("+index version") && !view(s).contains("+working version")
    });
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| view(s).contains("+working version"));
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("Source"));
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| {
        view(s).contains("+index version") && !view(s).contains("+working version")
    });
    assert_eq!(git(dir.path(), &["show", ":file.txt"]), "index version");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "working version\n"
    );
}

#[test]
fn wide_diff_aligns_old_and_new_code_with_tinted_syntax() {
    let dir = repo();
    std::fs::write(dir.path().join("sample.rs"), "fn before() {}\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Source"]);
    std::fs::write(dir.path().join("sample.rs"), "fn after() {}\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("+fn after()"));
    let frame = screen.frame(180, 40);
    let row = (0..40)
        .find(|y| frame.grid.row_text(*y).contains("fn before()"))
        .unwrap();
    assert!(frame.grid.row_text(row).contains("fn after()"));
    assert!((0..180).any(|x| matches!(
        frame.grid.get(x, row).unwrap().style,
        dust::ui::Style::GitDiff {
            added: true,
            syntax: Some(_)
        }
    )));
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
    key(&mut screen, KeyCode::Char('s'));
    let frame = screen.frame(180, 40);
    let old = (0..40)
        .find(|y| frame.grid.row_text(*y).contains("fn before()"))
        .unwrap();
    let new = (0..40)
        .find(|y| frame.grid.row_text(*y).contains("fn after()"))
        .unwrap();
    assert_ne!(old, new);
}

#[test]
fn multiline_prompt_moves_by_display_column_without_splitting_graphemes() {
    let mut prompt = dust::ui::PromptState::new(dust::ui::PromptKind::Text);
    prompt.input = "界áx\n12\n👩‍💻bc".into();
    prompt.cursor = "界áx\n12\n👩‍💻".len();
    prompt.move_vertical(true);
    assert_eq!(prompt.cursor, "界áx\n12".len());
    prompt.move_vertical(true);
    assert_eq!(prompt.cursor, "界".len());
    prompt.move_vertical(true);
    assert_eq!(prompt.cursor, "界".len());
    prompt.move_vertical(false);
    prompt.move_vertical(false);
    assert_eq!(prompt.cursor, "界áx\n12\n👩‍💻".len());
    prompt.move_vertical(false);
    assert_eq!(prompt.cursor, "界áx\n12\n👩‍💻".len());
}

#[test]
fn floating_is_default_and_fullscreen_is_a_persisted_setting() {
    use dust::config::{GitPlacement, GitWindowConfig};
    use dust::ui::{CellGrid, Frame, Style};
    let dir = repo();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    let config = dust::config::load_bundled_config().config.ui.git;
    assert_eq!(config.placement, GitPlacement::Float);
    let (rect, foreground) = screen.window_frame(160, 60, &config);
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (12, 4, 136, 51));
    let mut background = Frame {
        grid: CellGrid::new(160, 60),
        cursor: None,
    };
    background
        .grid
        .put_text(0, 0, "Editor remains visible", Style::Default);
    GitScreen::compose(&mut background, rect, &foreground);
    assert!(
        background
            .grid
            .row_text(0)
            .contains("Editor remains visible")
    );
    assert!(background.grid.row_text(rect.y).contains("Dusty Git"));
    assert_eq!(
        background.cursor,
        foreground
            .cursor
            .map(|(x, y, style)| (x + rect.x, y + rect.y, style))
    );
    let fullscreen: GitWindowConfig =
        toml::from_str("placement = 'fullscreen'\nwidth_percent = 85\nheight_percent = 85")
            .unwrap();
    let (rect, _) = screen.window_frame(160, 60, &fullscreen);
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 160, 60));
    let (small, _) = screen.window_frame(70, 22, &config);
    assert_eq!((small.width, small.height), (64, 20));
}

fn code_style(screen: &mut GitScreen, line: &str, token: &str) -> dust::ui::Style {
    use unicode_width::UnicodeWidthStr;
    let frame = screen.frame(160, 60);
    for y in 2..55 {
        let row = frame.grid.row_text(y);
        if let Some(start) = row.find(line) {
            let at = start + line.find(token).unwrap();
            return match frame.grid.get(row[..at].width() as u16, y).unwrap().style {
                dust::ui::Style::GitDiff {
                    syntax: Some(role), ..
                } => dust::ui::Style::Syntax(role),
                style => style,
            };
        }
    }
    panic!("missing preview line {line}: {}", view(screen));
}

#[test]
fn untracked_source_has_syntax_line_numbers_and_horizontal_scrolling() {
    let dir = repo();
    let code = format!(
        "// a comment\nfn main() {{\n    let message = \"hello λ\";\n    let long = \"{}TAIL_VISIBLE\";\n}}\n",
        "x".repeat(100)
    );
    std::fs::write(dir.path().join("sample.rs"), code).unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("fn main()"));
    let keyword = code_style(&mut screen, "fn main()", "fn");
    let string = code_style(&mut screen, "\"hello λ\"", "hello");
    let comment = code_style(&mut screen, "// a comment", "comment");
    assert!(matches!(keyword, dust::ui::Style::Syntax(_)));
    assert!(matches!(string, dust::ui::Style::Syntax(_)));
    assert!(matches!(comment, dust::ui::Style::Syntax(_)));
    assert_ne!(keyword, string);
    assert_ne!(keyword, comment);
    assert!(view(&mut screen).contains("2 │ fn main()"));
    assert!(!view(&mut screen).contains("TAIL_VISIBLE"));
    screen.event(UiEvent::Key {
        key: KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        },
        kind: KeyInputKind::Press,
    });
    for _ in 0..10 {
        key(&mut screen, KeyCode::Right);
    }
    assert!(view(&mut screen).contains("TAIL_VISIBLE"));
    assert!(matches!(
        code_style(&mut screen, "TAIL_VISIBLE", "TAIL_VISIBLE"),
        dust::ui::Style::Syntax(_)
    ));
    for _ in 0..10 {
        key(&mut screen, KeyCode::Left);
    }
    assert!(view(&mut screen).contains("2 │ fn main()"));
}

#[test]
fn working_index_commit_and_stash_diffs_have_source_syntax_and_numbered_markers() {
    let dir = repo();
    std::fs::write(
        dir.path().join("sample.rs"),
        "fn before() { let value = \"before\"; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("sample.py"),
        "def before():\n    return 'before'\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Code baseline"]);
    std::fs::write(
        dir.path().join("sample.rs"),
        "fn after() { let value = \"after\"; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("sample.py"),
        "def after():\n    return 'after'\n",
    )
    .unwrap();
    for staged in [false, true] {
        if staged {
            git(dir.path(), &["add", "."]);
        }
        let mut screen = GitScreen::open(dir.path()).unwrap();
        if staged {
            key(&mut screen, KeyCode::Char('i'));
        }
        wait(&mut screen, |s| view(s).contains("+def after()"));
        assert!(matches!(
            code_style(&mut screen, "+def after()", "def"),
            dust::ui::Style::Syntax(_)
        ));
        key(&mut screen, KeyCode::Down);
        wait(&mut screen, |s| view(s).contains("+fn after()"));
        assert!(matches!(
            code_style(&mut screen, "-fn before()", "fn"),
            dust::ui::Style::Syntax(_)
        ));
        assert!(matches!(
            code_style(&mut screen, "+fn after()", "after"),
            dust::ui::Style::Syntax(_)
        ));
        assert!(view(&mut screen).contains("1 +fn after()"));
    }
    git(dir.path(), &["commit", "-m", "Changed both languages"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, |s| view(s).contains("+fn after()"));
    assert!(matches!(
        code_style(&mut screen, "+def after()", "def"),
        dust::ui::Style::Syntax(_)
    ));
    assert!(matches!(
        code_style(&mut screen, "+fn after()", "fn"),
        dust::ui::Style::Syntax(_)
    ));
    std::fs::write(dir.path().join("sample.rs"), "fn stashed() {}\n").unwrap();
    git(dir.path(), &["stash", "push", "-m", "Colored stash"]);
    key(&mut screen, KeyCode::Char('r'));
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+fn stashed()"));
    assert!(matches!(
        code_style(&mut screen, "+fn stashed()", "fn"),
        dust::ui::Style::Syntax(_)
    ));
}

#[test]
fn unstaging_rename_restores_both_paths_and_preserves_other_staged_changes() {
    let dir = repo();
    git(dir.path(), &["mv", "file.txt", "renamed.txt"]);
    std::fs::write(dir.path().join("z-other.txt"), "unrelated staged content\n").unwrap();
    git(dir.path(), &["add", "z-other.txt"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| view(s).contains("renamed.txt"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"]),
        "z-other.txt"
    );
    assert_eq!(git(dir.path(), &["show", ":file.txt"]), "old");
    assert!(!dir.path().join("file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("renamed.txt")).unwrap(),
        "old\n"
    );
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "Only unrelated change");
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(
            dir.path(),
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        ),
        "z-other.txt"
    );
    assert_eq!(git(dir.path(), &["show", "HEAD:file.txt"]), "old");
}

#[test]
fn stash_preview_includes_untracked_files() {
    let dir = repo();
    std::fs::write(dir.path().join("new.txt"), "untracked stash contents\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("new.txt"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "Untracked-only stash");
    submit(&mut screen);
    wait(&mut screen, |s| {
        !s.busy() && view(s).contains("+untracked stash contents")
    });
    assert!(!dir.path().join("new.txt").exists());
    assert!(view(&mut screen).contains("new.txt"));
    key(&mut screen, KeyCode::Char('p'));
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "untracked stash contents\n"
    );
    assert_eq!(git(dir.path(), &["stash", "list"]), "");
}

#[test]
fn stages_unstages_and_commits_through_real_screen_input() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("changed"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"]),
        "file.txt"
    );
    key(&mut screen, KeyCode::Char('i'));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| view(s).contains("changed"));
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "Unicode λ message");
    key(&mut screen, KeyCode::Left);
    key(&mut screen, KeyCode::Right);
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "Unicode λ message"
    );
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
}

#[test]
fn branch_preview_is_selected_branch_not_current_head() {
    let dir = repo();
    git(dir.path(), &["checkout", "-b", "feature"]);
    std::fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Feature only"]);
    git(dir.path(), &["checkout", "main"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("feature"));
    key(&mut screen, KeyCode::Char('2'));
    wait(&mut screen, |s| view(s).contains("Feature only"));
    key(&mut screen, KeyCode::Char('r'));
    key(&mut screen, KeyCode::Char('3'));
    wait(&mut screen, |s| view(s).contains("+feature"));
    assert!(view(&mut screen).contains("3 Commits · feature"));
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| {
        view(s).contains("Author: Screen Test") && !view(s).contains("Feature only")
    });
    assert_eq!(git(dir.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn file_tree_and_real_cursor_do_not_shift_row_text() {
    let dir = repo();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/new.rs"), "fn main() {}\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("new.rs"));
    let before = screen.frame(120, 40);
    assert!(before.cursor.is_some());
    key(&mut screen, KeyCode::Char('2'));
    let after = screen.frame(120, 40);
    assert_eq!(before.grid.row_text(3), after.grid.row_text(3));
    key(&mut screen, KeyCode::Char('1'));
    key(&mut screen, KeyCode::Char('h'));
    assert!(!view(&mut screen).contains("new.rs"));
    key(&mut screen, KeyCode::Char('l'));
    assert!(view(&mut screen).contains("new.rs"));
    key(&mut screen, KeyCode::Char('j'));
    assert_eq!(
        key(&mut screen, KeyCode::Enter),
        ScreenOutcome::OpenFile(dir.path().join("src/new.rs"))
    );
}

#[test]
fn host_write_refusal_and_cancelled_form_preserve_repo() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("changed"));
    screen.write_refusal = Some("Unsaved editor buffer".into());
    key(&mut screen, KeyCode::Char(' '));
    assert!(view(&mut screen).contains("Unsaved editor buffer"));
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "cancelled");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "Initial commit"
    );
}

#[test]
fn worktrees_tags_and_stashes_are_real_repository_data() {
    let dir = repo();
    let target = dir.path().join("../screen-review");
    let owner = tempfile::tempdir().unwrap();
    let target = owner.path().join(target.file_name().unwrap());
    git(dir.path(), &["branch", "review"]);
    git(dir.path(), &["tag", "v1"]);
    git(
        dir.path(),
        &["worktree", "add", target.to_str().unwrap(), "review"],
    );
    std::fs::write(dir.path().join("file.txt"), "stashed\n").unwrap();
    git(dir.path(), &["stash", "push", "-m", "my work"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("my work"));
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char('L'));
    wait(&mut screen, |s| view(s).contains("screen-review"));
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    assert!(view(&mut screen).contains("v1"));
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('p'));
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "stashed\n"
    );
    assert_eq!(git(dir.path(), &["stash", "list"]), "");
}

#[test]
fn all_files_browses_clean_tracked_source_with_syntax() {
    let dir = repo();
    std::fs::write(
        dir.path().join("sample.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Add source"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("Working tree clean"));
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("sample.rs"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| view(s).contains("pub fn answer"));
    assert!(matches!(
        code_style(&mut screen, "pub fn answer", "fn"),
        dust::ui::Style::Syntax(_)
    ));
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("Working tree clean"));
}

fn select_file(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('1'));
    for _ in 0..20 {
        let frame = screen.frame(120, 40);
        if frame
            .cursor
            .is_some_and(|(_, y, _)| frame.grid.row_text(y).contains(name))
        {
            return;
        }
        key(screen, KeyCode::Down);
    }
    panic!("Missing file {name}: {}", view(screen));
}

#[test]
fn tutor_completes_real_git_lessons_and_restores_original_screen() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "untouched original work\n").unwrap();
    let original_head = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    screen.write_refusal = Some("Original workspace is untrusted".into());
    wait(&mut screen, |s| view(s).contains("untouched original work"));
    let before = view(&mut screen);
    key(&mut screen, KeyCode::Char(':'));
    text(&mut screen, "tutor git");
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| view(s).contains("1/6 · Files"));
    let practice = screen.root().to_owned();
    let owner = practice.parent().unwrap().to_owned();
    assert_ne!(practice, dir.path());
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("README.md"));
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| view(s).contains("2/6 · Stage"));
    select_file(&mut screen, "greet.rs");
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| view(s).contains("3/6 · Commit and push"));
    assert_eq!(
        git(&practice, &["diff", "--cached", "--name-only"]),
        "src/greet.rs"
    );
    key(&mut screen, KeyCode::Char('c'));
    text(&mut screen, "Complete the practice change");
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy());
    key(&mut screen, KeyCode::Char('P'));
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| view(s).contains("4/6 · Worktrees"));
    assert_eq!(
        git(&owner.join("remote.git"), &["rev-parse", "practice"]),
        git(&practice, &["rev-parse", "HEAD"])
    );
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "lesson-review");
    submit(&mut screen);
    let review = owner.join("review");
    wait(&mut screen, |s| {
        !s.busy() && review.join(".git").is_file() && view(s).contains("lesson-review")
    });
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| view(s).contains("5/6 · Stashes"));
    assert_eq!(screen.root(), review);
    assert!(review.join("notes.md").is_file());
    key(&mut screen, KeyCode::Char('4'));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "Practice stash");
    submit(&mut screen);
    wait(&mut screen, |s| !s.busy() && view(s).contains("stash@{0}"));
    assert!(!review.join("notes.md").exists());
    key(&mut screen, KeyCode::Char('p'));
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| {
        view(s).contains("6/6 · Conflict resolution")
    });
    assert_eq!(
        git(&review, &["diff", "--name-only", "--diff-filter=U"]),
        "src/greet.rs"
    );
    wait(&mut screen, |s| view(s).contains("UU greet.rs"));
    select_file(&mut screen, "greet.rs");
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    submit(&mut screen);
    assert!(
        !std::fs::read_to_string(review.join("src/greet.rs"))
            .unwrap()
            .contains("<<<<<<<")
    );
    assert!(
        std::fs::read_to_string(review.join("src/greet.rs"))
            .unwrap()
            .contains("their branch")
    );
    key(&mut screen, KeyCode::Char(' '));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(
        git(&review, &["diff", "--name-only", "--diff-filter=U"]),
        ""
    );
    key(&mut screen, KeyCode::Char('C'));
    key(&mut screen, KeyCode::Enter);
    wait(&mut screen, |s| {
        view(s).contains("completed all six lessons")
    });
    assert_eq!(
        git(&review, &["rev-list", "--parents", "-1", "HEAD"])
            .split_whitespace()
            .count(),
        3
    );
    key(&mut screen, KeyCode::Char('q'));
    assert_eq!(screen.root(), dir.path());
    assert_eq!(view(&mut screen), before);
    assert_eq!(
        screen.write_refusal.as_deref(),
        Some("Original workspace is untrusted")
    );
    assert!(!owner.exists());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "untouched original work\n"
    );
}
