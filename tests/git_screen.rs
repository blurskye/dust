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

#[test]
fn project_header_is_compact_and_branch_badges_are_separate() {
    use unicode_width::UnicodeWidthStr;
    let dir = repo();
    let checkout = tempfile::tempdir().unwrap();
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            checkout.path().to_str().unwrap(),
        ],
    );
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| {
        view(s).contains("feature") && view(s).contains("wt")
    });
    // Narrow layouts show only the focused pane; focus Branches so its row is visible.
    key(&mut screen, KeyCode::Char('3'));
    for (width, height) in [(64, 20), (80, 24), (120, 40), (190, 48)] {
        let frame = screen.frame(width, height);
        let header = frame.grid.row_text(0);
        // Top tabs always keep their glyphs; full labels shorten on narrow bars.
        for glyph in ["◆", "○", "●"] {
            assert!(header.contains(glyph), "{width}: {header}");
        }
        if width >= 120 {
            for label in ["Git", "Issues", "Pull requests", "Actions"] {
                assert!(header.contains(label), "{width}: {header}");
            }
        }
        assert!(!header.contains("keys"), "{width}: {header}");
        let footer = frame.grid.row_text(height - 1);
        let footer_words = footer.split_whitespace().collect::<Vec<_>>();
        assert!(
            footer_words.windows(2).any(|pair| pair == ["?", "keys"]),
            "{width}: {footer}"
        );
        let rendered = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        let words = rendered.split_whitespace().collect::<Vec<_>>();
        assert_eq!(
            words
                .windows(2)
                .filter(|pair| *pair == ["?", "keys"])
                .count(),
            1,
            "help must appear once in the footer at {width} columns"
        );
        // The top bar names the checked-out head but never a forge host.
        assert!(
            !header.contains("Dusty Git")
                && header.contains("⎇ main")
                && !header.contains("github.com"),
            "{width}: {header}"
        );
        // The Branches row (not the reader's title, which also names the branch).
        let branch = (2..height - 1)
            .find(|&y| {
                let text = frame.grid.row_text(y);
                text.starts_with('│') && text.contains("⎇ feature")
            })
            .unwrap_or_else(|| panic!("{width}x{height}: no feature branch row"));
        let text = frame.grid.row_text(branch);
        let badge = text.find("wt").unwrap();
        assert!(badge > text.find("feature").unwrap() + "feature".len() + 2);
        assert_eq!(
            frame
                .grid
                .get(text[..badge].width() as u16, branch)
                .unwrap()
                .style,
            dust::ui::Style::Git {
                role: dust::ui::GitRole::Modified,
                surface: dust::ui::GitSurface::Chip(dust::ui::GitChip::Modified),
                emphasis: dust::ui::Emphasis::None,
            }
        );
    }
    let mut linked = GitScreen::open(checkout.path()).unwrap();
    for _ in 0..4 {
        let header = linked.frame(120, 40).grid.row_text(0);
        assert!(header.contains(dir.path().file_name().unwrap().to_str().unwrap()));
        assert!(!header.contains(checkout.path().file_name().unwrap().to_str().unwrap()));
        key(&mut linked, KeyCode::Char('>'));
    }
}

#[test]
fn changes_and_commit_files_share_a_single_collapsible_sidebar_tree() {
    let dir = repo();
    std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
    std::fs::write(dir.path().join("src/nested/alpha.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(dir.path().join("src/nested/beta.rs"), "fn beta() {}\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| {
        view(s).contains("alpha.rs") && view(s).contains("beta.rs")
    });
    // Changes is flat by default; `t` shows it as a foldable tree.
    key(&mut screen, KeyCode::Char('t'));
    wait(&mut screen, |s| view(s).contains("nested/"));
    key(&mut screen, KeyCode::Char('h'));
    key(&mut screen, KeyCode::Char('h'));
    let folded = view(&mut screen);
    let rail = {
        let frame = screen.frame(120, 40);
        let right = rail_right(&frame) as usize;
        (0..40)
            .map(|y| {
                frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(right + 1)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        rail.contains("l / Enter expand") && !rail.contains("nested/"),
        "folded view:\n{folded}"
    );
    // Checked at the end so the rest of the test still runs: with every row folded
    // away nothing is selected, so the reader must not keep showing a hidden file.
    let stale_reader = folded.contains("nested/");
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char('l'));
    assert!(view(&mut screen).contains("nested/"));
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Nested commit"]);
    screen = GitScreen::open(dir.path()).unwrap();
    // Narrow layouts show one pane at a time, so pane rectangles are compared where
    // the rail is visible.
    let sizes = [(100, 30), (120, 40), (190, 48)];
    let headers = |screen: &mut GitScreen, width, height| {
        let frame = screen.frame(width, height);
        ["Branches", "History", "Stashes"].map(|title| {
            (0..height)
                .find(|&y| {
                    let text = frame.grid.row_text(y);
                    text.starts_with('╭') && text.contains(title)
                })
                .unwrap()
        })
    };
    // History can paint before the panes take their sizes for the session.
    wait(&mut screen, |s| {
        let view = view(s);
        view.contains("Nested commit") && view.contains("Local 1") && s.panes_sized()
    });
    let initial = sizes.map(|(width, height)| headers(&mut screen, width, height));
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    assert!(view(&mut screen).contains("Commit files"));
    for (index, (width, height)) in sizes.into_iter().enumerate() {
        assert_eq!(headers(&mut screen, width, height), initial[index]);
    }
    key(&mut screen, KeyCode::Enter);
    let frame = screen.frame(120, 40);
    let (_, row, _) = frame.cursor.unwrap();
    assert!(frame.grid.row_text(row).contains("alpha.rs"));
    key(&mut screen, KeyCode::Char('h'));
    let frame = screen.frame(120, 40);
    let (_, row, _) = frame.cursor.unwrap();
    assert!(frame.grid.row_text(row).contains("▸ src/nested/"));
    assert!(!(0..40).any(|y| frame.grid.row_text(y).contains("beta.rs")));
    assert!(view(&mut screen).contains("+ fn alpha()"));
    key(&mut screen, KeyCode::Char('J'));
    wait(&mut screen, |s| view(s).contains("+ fn beta()"));
    for (index, (width, height)) in sizes.into_iter().enumerate() {
        assert_eq!(headers(&mut screen, width, height), initial[index]);
        let frame = screen.frame(width, height);
        let (_, row, _) = frame.cursor.unwrap();
        assert!(
            row < initial[index][0],
            "files belong inside the commit-file pane"
        );
        let rail = rail_right(&frame);
        assert!(frame.grid.row_text(row).contains("beta.rs"));
        assert_eq!(
            (0..height)
                .filter(|&y| frame.grid.row_text(y).contains("Commit files"))
                .count(),
            1
        );
        // No interior divider may split the reader's code field; its outer edge is reserved.
        assert!(!(rail + 3..width - 1).any(|x| frame.grid.get(x, row).unwrap().symbol == "│"));
        assert_eq!(frame.grid.get(rail + 2, row).unwrap().symbol, "│");
        assert_eq!(frame.grid.get(width - 1, row).unwrap().symbol, "│");
    }
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Char('K'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    key(&mut screen, KeyCode::Escape);
    assert!(view(&mut screen).contains("History"));
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
    assert!(
        !stale_reader,
        "folding every Changes row away left the reader showing the hidden file:\n{folded}"
    );
}

/// The x of the rail's right border: the first `╮` on the top pane row.
fn rail_right(frame: &dust::ui::Frame) -> u16 {
    (0..frame.grid.width())
        .find(|&x| frame.grid.get(x, 1).unwrap().symbol == "╮")
        .unwrap()
}

/// The two-letter XY status painted for `name` in the Changes pane.
fn change_code(screen: &mut GitScreen, name: &str) -> Option<String> {
    let frame = screen.frame(120, 40);
    let rail = rail_right(&frame) as usize;
    (2..39).find_map(|y| {
        let row = frame
            .grid
            .row_text(y)
            .chars()
            .take(rail)
            .collect::<String>();
        row.contains(name).then(|| {
            format!(
                "{}{}",
                frame.grid.get(3, y).unwrap().symbol,
                frame.grid.get(4, y).unwrap().symbol
            )
        })
    })
}

#[test]
fn commit_selection_updates_changes_without_hiding_other_panes_or_staging() {
    let dir = repo();
    std::fs::write(dir.path().join("alpha.rs"), "fn alpha() {}\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Add alpha"]);
    std::fs::write(dir.path().join("file.txt"), "working change\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("+ working change"));
    let changes = |screen: &mut GitScreen| {
        let frame = screen.frame(120, 40);
        let left = rail_right(&frame) + 1;
        (1..40)
            .take_while(|&y| !frame.grid.row_text(y).contains("Branches"))
            .map(|y| {
                frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(left as usize)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    assert!(changes(&mut screen).contains("Commit files"));
    assert!(changes(&mut screen).contains("alpha.rs"));
    assert!(!changes(&mut screen).contains("file.txt"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| view(s).contains("+ old"));
    assert!(changes(&mut screen).contains("Commit files"));
    assert!(changes(&mut screen).contains("file.txt"));
    assert!(!changes(&mut screen).contains("alpha.rs"));
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char('-'));
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    key(&mut screen, KeyCode::Char('3'));
    assert!(!changes(&mut screen).contains("Commit files"));
    assert!(changes(&mut screen).contains("file.txt"));
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Char('5'));
    assert!(view(&mut screen).contains("No stashes"));
    assert!(
        !changes(&mut screen).contains("Commit files"),
        "`5` must leave the open commit's files for Stashes:\n{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    key(&mut screen, KeyCode::Enter);
    assert_eq!(key(&mut screen, KeyCode::Escape), ScreenOutcome::Continue);
    assert!(changes(&mut screen).contains("Commit files"));
    assert_eq!(key(&mut screen, KeyCode::Escape), ScreenOutcome::Continue);
    wait(&mut screen, |s| view(s).contains("+ working change"));
    assert!(!changes(&mut screen).contains("Commit files"));
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "M file.txt");
}

#[test]
fn file_kind_modes_preserve_status_cells_identity_and_native_cursor() {
    use dust::config::ExplorerIconMode;
    let dir = repo();
    std::fs::write(
        dir.path().join("queue.py"),
        "def size(items):\n    return len(items)\n",
    )
    .unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("queue.py"));
    let original = git(dir.path(), &["status", "--porcelain"]);
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        screen.set_icon_mode(ExplorerIconMode::Ascii);
        let plain = screen.frame(width, height);
        let (_, row, _) = plain.cursor.unwrap();
        // XY status sits in the pane's interior columns 2–3, the file glyph at 5.
        assert!(
            plain.grid.row_text(row).contains("?? - queue.py"),
            "{}",
            plain.grid.row_text(row)
        );
        assert_eq!(plain.grid.get(3, row).unwrap().symbol, "?");
        assert_eq!(plain.grid.get(4, row).unwrap().symbol, "?");
        for mode in [ExplorerIconMode::Unicode, ExplorerIconMode::NerdFont] {
            screen.set_icon_mode(mode);
            let rich = screen.frame(width, height);
            assert_eq!(plain.cursor, rich.cursor);
            assert_eq!(rich.grid.get(3, row), plain.grid.get(3, row));
            assert_eq!(rich.grid.get(4, row), plain.grid.get(4, row));
            assert!(rich.grid.row_text(row).contains("queue.py"));
            assert_ne!(rich.grid.get(6, row).unwrap().symbol, "-");
        }
    }
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), original);
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
    // The compact footer keeps the focused pane's main actions and the help key.
    let footer = before.grid.row_text(23);
    assert!(
        footer.contains("stage") && footer.contains("commit"),
        "{footer}"
    );
    // Checked at the end: `? keys` must survive a long path in the footer context.
    let help_hint = footer.contains("? keys");
    key(&mut screen, KeyCode::Char('?'));
    let help = screen.frame(80, 24);
    let text = (0..24)
        .map(|y| help.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("Keys · Changes") && text.contains("C-h/j/k/l"),
        "{text}"
    );
    // Checked at the end: the compact help still lists the split/unified toggle.
    let split_listed = text.contains("Split · unified");
    assert!(help.cursor.is_some());
    key(&mut screen, KeyCode::Escape);
    let after = screen.frame(80, 24);
    assert_eq!(before.cursor, after.cursor);
    for y in 0..24 {
        assert_eq!(before.grid.row_text(y), after.grid.row_text(y));
    }
    assert!(
        split_listed && help_hint,
        "compact help must list `s` split/unified ({split_listed}) and the footer must keep `? keys` ({help_hint}): {footer}\n{text}"
    );
}

#[test]
fn preview_focus_updates_index_diff_when_toggling_layers() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "index version\n").unwrap();
    git(dir.path(), &["add", "file.txt"]);
    std::fs::write(dir.path().join("file.txt"), "working version\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("+ working version"));
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
        view(s).contains("+ index version") && !view(s).contains("+ working version")
    });
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| view(s).contains("+ working version"));
    // `F` browses every file; the rail retitles itself and the reader shows source.
    key(&mut screen, KeyCode::Char('F'));
    wait(&mut screen, |s| {
        view(s).contains("± Files") && view(s).contains("working version")
    });
    key(&mut screen, KeyCode::Char('i'));
    wait(&mut screen, |s| {
        view(s).contains("+ index version") && !view(s).contains("+ working version")
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
    std::fs::write(dir.path().join("sample.rs"), "fn before() {}\n".repeat(80)).unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Source"]);
    std::fs::write(dir.path().join("sample.rs"), "fn after() {}\n".repeat(80)).unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("− fn before()"));
    let unified = screen.frame(180, 40);
    assert!(
        !(0..40).any(|y| unified.grid.row_text(y).contains("Before")),
        "split must be opt-in, even on wide terminals"
    );
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
    key(&mut screen, KeyCode::Down);
    let frame = screen.frame(180, 40);
    let (x, y, _) = frame.cursor.unwrap();
    assert_eq!(frame.grid.get(x, y).unwrap().symbol, "f");
    key(&mut screen, KeyCode::Home);
    key(&mut screen, KeyCode::Char('|'));
    screen.frame(180, 40);
    key(&mut screen, KeyCode::Down);
    let frame = screen.frame(180, 40);
    let (x, y, _) = frame.cursor.unwrap();
    assert_eq!(frame.grid.get(x, y).unwrap().symbol, "f");
    let row = (0..40)
        .find(|y| frame.grid.row_text(*y).contains("fn before()"))
        .unwrap();
    assert!(frame.grid.row_text(row).contains("fn after()"));
    // Added code keeps its syntax colours on top of the added band.
    assert!((0..40).any(|y| {
        frame.grid.row_text(y).contains("fn after()")
            && (0..180).any(|x| {
                matches!(
                    frame.grid.get(x, y).unwrap().style,
                    dust::ui::Style::GitSyntax {
                        surface: dust::ui::GitSurface::AddedBand
                            | dust::ui::GitSurface::AddedEmphasis,
                        ..
                    }
                )
            })
    }));
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
    key(&mut screen, KeyCode::Char('|'));
    screen.frame(180, 40);
    for _ in 0..78 {
        key(&mut screen, KeyCode::Down);
    }
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
fn commits_browse_one_changed_file_at_a_time_without_raw_statistics() {
    let dir = repo();
    std::fs::write(dir.path().join("alpha.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "fn beta() {}\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "A multi-file commit"]);
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("A multi-file c"));
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    let overview = view(&mut screen);
    assert!(
        overview.contains("2 files")
            && !overview.contains("fn beta()")
            && !overview.contains("files changed,")
    );
    key(&mut screen, KeyCode::Enter);
    assert!(view(&mut screen).contains("Commit files"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| view(s).contains("+ fn beta()"));
    assert!(!view(&mut screen).contains("fn alpha()"));
    key(&mut screen, KeyCode::Tab);
    key(&mut screen, KeyCode::Char('K'));
    wait(&mut screen, |s| view(s).contains("+ fn alpha()"));
    key(&mut screen, KeyCode::Char('?'));
    assert!(view(&mut screen).contains("Next · previous file"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    assert!(view(&mut screen).contains("Commit files"));
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Commit files"));
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
}

#[test]
fn commit_scrolls_immediately_pins_identity_and_exposes_the_full_message() {
    let dir = repo();
    let source = (0..180)
        .map(|i| {
            format!("fn numbered_{i:03}() {{ let description = \"a long readable code line\"; }}\n")
        })
        .collect::<String>();
    std::fs::write(dir.path().join("scroll.rs"), source).unwrap();
    git(dir.path(), &["add", "."]);
    let subject = "A deliberately long commit subject describing the complete behavior of the native workspace, its keyboard navigation, scrolling, focus restoration, and careful preservation of all user drafts";
    let body = (0..120)
        .map(|i| format!("Detailed message paragraph {i:02}.\n"))
        .collect::<String>()
        + "FULL MESSAGE END";
    git(dir.path(), &["commit", "-m", subject, "-m", &body]);
    for (width, height) in [(100, 30), (190, 48)] {
        let mut screen = GitScreen::open(dir.path()).unwrap();
        key(&mut screen, KeyCode::Char('4'));
        wait(&mut screen, |s| view(s).contains("numbered_000"));
        let text_at = |s: &mut GitScreen| {
            let frame = s.frame(width, height);
            (0..height)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let overview = text_at(&mut screen);
        let footer = overview.lines().nth(usize::from(height - 1)).unwrap();
        assert!(footer.contains("Enter files"), "{overview}");
        // The long subject is clipped in the overview; `m` shows the whole message.
        assert!(
            overview.contains("A deliberately long commit subject"),
            "{overview}"
        );
        assert!(!overview.contains("careful preservation of all user drafts"));
        key(&mut screen, KeyCode::Enter);
        let files = screen.frame(width, height);
        let (_, file_y, _) = files.cursor.unwrap();
        assert!(files.grid.row_text(file_y).contains("scroll.rs"));
        let added = |surface| dust::ui::Style::Git {
            role: dust::ui::GitRole::Added,
            surface,
            emphasis: dust::ui::Emphasis::None,
        };
        let stats_x = (0..width)
            .find(|&x| {
                files.grid.get(x, file_y).unwrap().style == added(dust::ui::GitSurface::Cursor)
            })
            .expect("selected file must show added-line statistics");
        key(&mut screen, KeyCode::Tab);
        assert_eq!(
            screen
                .frame(width, height)
                .grid
                .get(stats_x, file_y)
                .unwrap()
                .style,
            added(dust::ui::GitSurface::CursorInactive)
        );
        let first = text_at(&mut screen);
        let footer = first.lines().nth(usize::from(height - 1)).unwrap();
        assert!(footer.contains("find") && footer.contains("message") && footer.contains("keys"));
        let focused = screen.frame(width, height);
        let (x, y, _) = focused.cursor.expect("native source cursor");
        assert!(x > (width * 3 / 10).clamp(26, 52));
        assert!(!focused.grid.get(x, y).unwrap().symbol.trim().is_empty());
        key(&mut screen, KeyCode::Char('?'));
        let help = text_at(&mut screen);
        assert!(help.contains("Keys ·") && help.contains("Scroll"), "{help}");
        key(&mut screen, KeyCode::Escape);
        assert_eq!(
            text_at(&mut screen),
            first,
            "help must preserve the source viewport"
        );
        key(&mut screen, KeyCode::Char('j'));
        let moved = text_at(&mut screen);
        assert_ne!(
            first, moved,
            "the first j must scroll, not move an invisible cursor"
        );
        key(&mut screen, KeyCode::Home);
        text_at(&mut screen);
        key(&mut screen, KeyCode::PageDown);
        let paged = text_at(&mut screen);
        assert!(
            !paged.contains("numbered_000"),
            "the first page-down must expose new code"
        );
        // The reader's pinned file band names the file while its code scrolls.
        let rail = if width >= 160 { 52 } else { 30 };
        let identity = paged
            .lines()
            .skip(2)
            .find(|line| {
                line.chars()
                    .skip(rail + 1)
                    .collect::<String>()
                    .contains("scroll.rs")
            })
            .unwrap_or_else(|| panic!("{paged}"))
            .to_owned();
        key(&mut screen, KeyCode::Char('l'));
        let panned = text_at(&mut screen);
        assert!(panned.lines().any(|line| line == identity));
        assert!(
            panned.contains('‹'),
            "panned code must signal hidden text on the left"
        );
        key(&mut screen, KeyCode::Char('m'));
        let message = text_at(&mut screen);
        let message_text = message
            .lines()
            .map(|line| line.trim().trim_matches('│').trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            message.contains("Commit message") && message_text.contains(subject),
            "{width}x{height}:\n{message}"
        );
        let next_page_line = message
            .lines()
            .nth(usize::from(height - 5))
            .unwrap()
            .trim()
            .trim_matches('│')
            .trim();
        let paragraph = next_page_line
            .strip_prefix("Detailed message paragraph ")
            .unwrap()
            .trim_end_matches('.')
            .parse::<usize>()
            .unwrap();
        key(&mut screen, KeyCode::PageDown);
        assert!(
            text_at(&mut screen)
                .lines()
                .nth(2)
                .unwrap()
                .contains(&format!("Detailed message paragraph {:02}.", paragraph + 1))
        );
        key(&mut screen, KeyCode::PageUp);
        assert_eq!(text_at(&mut screen), message);
        key(&mut screen, KeyCode::End);
        assert!(text_at(&mut screen).contains("FULL MESSAGE END"));
        key(&mut screen, KeyCode::Escape);
        assert_eq!(
            text_at(&mut screen),
            panned,
            "closing the message must preserve the diff position"
        );
    }
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
    background
        .grid
        .put_text(0, 59, " NORMAL  editor status line", Style::Default);
    GitScreen::compose(&mut background, rect, &foreground);
    assert!(
        background
            .grid
            .row_text(0)
            .contains("Editor remains visible")
    );
    // The editor around a float recedes into the border colour, so its status line never
    // reads as a second footer under Dusty Git's own.
    for y in [0, 59] {
        assert!(
            matches!(
                background.grid.get(1, y).unwrap().style,
                Style::Git {
                    role: dust::ui::GitRole::Border,
                    ..
                }
            ),
            "row {y}: {:?}",
            background.grid.get(1, y).unwrap().style
        );
    }
    assert!(background.grid.row_text(59).contains("editor status line"));
    assert!(
        background.grid.row_text(rect.y).contains(
            &dir.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        )
    );
    assert!(!background.grid.row_text(rect.y).contains("Dusty Git"));
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
                dust::ui::Style::GitSyntax { group, .. } => dust::ui::Style::Syntax(group),
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
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        let (x, y, _) = frame.cursor.unwrap();
        assert_eq!(frame.grid.get(x, y).unwrap().symbol, "/");
        assert!(frame.grid.row_text(y).contains("│ // a comment"));
    }
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
        wait(&mut screen, |s| view(s).contains("+ def after()"));
        assert!(matches!(
            code_style(&mut screen, "+ def after()", "def"),
            dust::ui::Style::Syntax(_)
        ));
        key(&mut screen, KeyCode::Down);
        wait(&mut screen, |s| view(s).contains("+ fn after()"));
        assert!(matches!(
            code_style(&mut screen, "− fn before()", "fn"),
            dust::ui::Style::Syntax(_)
        ));
        assert!(matches!(
            code_style(&mut screen, "+ fn after()", "after"),
            dust::ui::Style::Syntax(_)
        ));
        assert!(view(&mut screen).contains("1 │ + fn after()"));
    }
    git(dir.path(), &["commit", "-m", "Changed both languages"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ def after()"));
    assert!(matches!(
        code_style(&mut screen, "+ def after()", "def"),
        dust::ui::Style::Syntax(_)
    ));
    key(&mut screen, KeyCode::Enter);
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| view(s).contains("+ fn after()"));
    assert!(matches!(
        code_style(&mut screen, "+ fn after()", "fn"),
        dust::ui::Style::Syntax(_)
    ));
    key(&mut screen, KeyCode::Escape);
    std::fs::write(dir.path().join("sample.rs"), "fn stashed() {}\n").unwrap();
    git(dir.path(), &["stash", "push", "-m", "Colored stash"]);
    key(&mut screen, KeyCode::Char('r'));
    key(&mut screen, KeyCode::Char('5'));
    wait(&mut screen, |s| view(s).contains("+ fn stashed()"));
    assert!(matches!(
        code_style(&mut screen, "+ fn stashed()", "fn"),
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
    key(&mut screen, KeyCode::Char('u'));
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
    key(&mut screen, KeyCode::Char('5'));
    key(&mut screen, KeyCode::Char('n'));
    text(&mut screen, "Untracked-only stash");
    submit(&mut screen);
    wait(&mut screen, |s| {
        !s.busy() && view(s).contains("+ untracked stash contents")
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
fn commit_box_insert_hints_are_clear_without_duplicate_text() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    screen.set_modal(true);
    wait(&mut screen, |s| view(s).contains("changed"));
    key(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, |s| !s.busy());
    key(&mut screen, KeyCode::Char('c'));
    assert_eq!(
        screen.input_mode(),
        Some(dust::git::screen::InputMode::Insert)
    );
    let frame = screen.frame(120, 40);
    let shown = (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("Commit to main"), "{shown}");
    assert!(
        (0..39).any(|y| {
            let row = frame.grid.row_text(y);
            row.contains("Ctrl+Enter commit · Tab next · Esc normal")
                && row.trim_end().ends_with('│')
        }),
        "{shown}"
    );
    assert!(!shown.contains("(optional)"), "{shown}");
    let footer = frame.grid.row_text(39);
    assert!(!footer.contains("sign-off · amend"), "{footer}");

    // The sheet covers the whole body, so only a narrow screen drops `Tab next`.
    let narrow = screen.frame(70, 24);
    assert!(
        (0..23).any(|y| {
            let row = narrow.grid.row_text(y);
            row.contains("Ctrl+Enter commit · Esc normal") && row.trim_end().ends_with('│')
        }),
        "{}",
        (0..24)
            .map(|y| narrow.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let footer = narrow.grid.row_text(23);
    assert!(!footer.contains("C-Enter"), "{footer}");
    assert!(!footer.contains("sign-off · amend"), "{footer}");
    assert!(!footer.contains("normal mode"), "{footer}");
}

#[test]
fn stages_unstages_and_commits_through_real_screen_input() {
    use unicode_width::UnicodeWidthStr;
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("changed"));
    let assert_file_action = |screen: &mut GitScreen, verb: &str| {
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let frame = screen.frame(width, height);
            let footer = frame.grid.row_text(height - 1);
            let words = footer.split_whitespace().collect::<Vec<_>>();
            assert!(
                words
                    .windows(2)
                    .any(|words| words == [if verb == "stage" { "s" } else { "u" }, verb]),
                "{width}×{height}: {footer}"
            );
        }
    };
    assert_file_action(&mut screen, "stage");
    screen.write_refusal = Some("Test write refused".into());
    key(&mut screen, KeyCode::Char('s'));
    assert!(view(&mut screen).contains("Test write refused"));
    screen.write_refusal = None;
    key(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, |s| !s.busy());
    assert!(!view(&mut screen).contains("Test write refused"));
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"]),
        "file.txt"
    );
    wait(&mut screen, |s| {
        change_code(s, "file.txt").as_deref() == Some("M ") && view(s).contains("+ changed")
    });
    assert_file_action(&mut screen, "unstage");
    let staged = screen.frame(120, 40);
    let (_, row, _) = staged.cursor.unwrap();
    let label = staged.grid.row_text(row);
    let filename = label[..label.find("file.txt").unwrap()].width();
    // A fully staged name is painted in the Staged (added) role on the cursor row.
    assert_eq!(
        staged.grid.get(filename as u16, row).unwrap().style,
        dust::ui::Style::Git {
            role: dust::ui::GitRole::Added,
            surface: dust::ui::GitSurface::Cursor,
            emphasis: dust::ui::Emphasis::None,
        }
    );
    // No index-layer switch: `u` itself must unstage the selected file.
    key(&mut screen, KeyCode::Char('u'));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    wait(&mut screen, |s| view(s).contains("+ changed"));
    assert!(!view(&mut screen).contains("operation would change nothing"));
    assert_file_action(&mut screen, "stage");
    let unstaged = screen.frame(120, 40);
    let (_, row, _) = unstaged.cursor.unwrap();
    let label = unstaged.grid.row_text(row);
    let unstaged_name = label[..label.find("file.txt").unwrap()].width();
    assert_eq!(
        filename, unstaged_name,
        "staging must not move the filename"
    );
    assert_eq!(
        unstaged.grid.get(unstaged_name as u16, row).unwrap().style,
        dust::ui::Style::Git {
            role: dust::ui::GitRole::Text,
            surface: dust::ui::GitSurface::Cursor,
            emphasis: dust::ui::Emphasis::None,
        }
    );
    key(&mut screen, KeyCode::Char('s'));
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
fn partial_staging_finishes_the_file_then_space_unstages_without_changing_disk() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "staged version\n").unwrap();
    git(dir.path(), &["add", "file.txt"]);
    std::fs::write(dir.path().join("file.txt"), "working version\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| {
        view(s).contains("◐MM") && view(s).contains("+ working version")
    });
    key(&mut screen, KeyCode::Char('s'));
    wait(&mut screen, |s| !s.busy());
    wait(&mut screen, |s| {
        change_code(s, "file.txt").as_deref() == Some("M ")
            && !view(s).contains("◐MM")
            && view(s).contains("+ working version")
    });
    assert_eq!(git(dir.path(), &["show", ":file.txt"]), "working version");
    key(&mut screen, KeyCode::Char('u'));
    wait(&mut screen, |s| !s.busy());
    assert_eq!(git(dir.path(), &["diff", "--cached", "--name-only"]), "");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "working version\n"
    );
}

#[test]
fn sidebar_rectangles_and_focus_surfaces_survive_folding_refresh_and_navigation() {
    let dir = repo();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    for index in 0..18 {
        std::fs::write(
            dir.path().join(format!("docs/file-{index:02}.md")),
            "sample\n",
        )
        .unwrap();
        git(dir.path(), &["branch", &format!("branch-{index:02}")]);
    }
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| {
        view(s).contains("file-00.md") && view(s).contains("Initial commit") && s.panes_sized()
    });
    let headers = |screen: &mut GitScreen, width, height| {
        let frame = screen.frame(width, height);
        ["Branches", "History", "Stashes"].map(|name| {
            (0..height)
                .find(|&y| {
                    let text = frame.grid.row_text(y);
                    text.starts_with('╭') && text.contains(name)
                })
                .unwrap_or_else(|| panic!("{width}x{height} missing {name}"))
        })
    };
    let edge = |role| dust::ui::Style::Git {
        role,
        surface: dust::ui::GitSurface::Pane,
        emphasis: dust::ui::Emphasis::None,
    };
    // Narrow layouts show one pane at a time; pane rectangles are compared where the
    // rail is visible.
    for (width, height) in [(100, 30), (120, 40), (190, 48)] {
        let initial = headers(&mut screen, width, height);
        let right = rail_right(&screen.frame(width, height));
        // The focused Changes pane carries the accent border.
        for (x, y) in [(0, 2), (right, 2), (right, 3)] {
            assert_eq!(
                screen.frame(width, height).grid.get(x, y).unwrap().style,
                edge(dust::ui::GitRole::Focus)
            );
        }
        key(&mut screen, KeyCode::Char('t'));
        key(&mut screen, KeyCode::Char('h'));
        assert_eq!(headers(&mut screen, width, height), initial);
        key(&mut screen, KeyCode::Char('h'));
        assert_eq!(headers(&mut screen, width, height), initial);
        key(&mut screen, KeyCode::Enter);
        key(&mut screen, KeyCode::Char('l'));
        key(&mut screen, KeyCode::Char('t'));
        key(&mut screen, KeyCode::Char('3'));
        for (x, y) in [(0, 2), (right, 2), (right, 3)] {
            assert_eq!(
                screen.frame(width, height).grid.get(x, y).unwrap().style,
                edge(dust::ui::GitRole::Border)
            );
        }
        for _ in 0..5 {
            key(&mut screen, KeyCode::Down);
            let during = view(&mut screen);
            assert!(during.contains("Initial commit") && !during.contains("No commits yet"));
            assert_eq!(headers(&mut screen, width, height), initial);
            screen.poll();
        }
        key(&mut screen, KeyCode::Char('2'));
    }
    select_file(&mut screen, "file-00.md");
    wait(&mut screen, |s| view(s).contains("│ sample"));
    key(&mut screen, KeyCode::Char('r'));
    assert!(
        view(&mut screen).contains("│ sample"),
        "refresh must keep the loaded preview"
    );
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
    key(&mut screen, KeyCode::Char('3'));
    // History lists "Feature only" before the branch list arrives; only a selected branch
    // pins History to it, so wait for that rather than for the commit's name.
    wait(&mut screen, |s| {
        let frame = s.frame(190, 48);
        (0..48).any(|y| frame.grid.row_text(y).contains("History · feature"))
    });
    key(&mut screen, KeyCode::Char('r'));
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| view(s).contains("+ feature"));
    // The History pane's border title names its pinned reference.
    let pinned = screen.frame(190, 48);
    let history_heading = (0..48)
        .map(|y| pinned.grid.row_text(y))
        .find(|row| row.contains("History · feature"))
        .unwrap_or_else(|| {
            let shown = (0..48)
                .map(|y| pinned.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            panic!("history heading names its pinned reference:\n{shown}")
        });
    assert!(history_heading.starts_with('╭'), "{history_heading}");
    key(&mut screen, KeyCode::Char('3'));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| {
        view(s).contains("Initial commit") && !view(s).contains("Feature only")
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
    key(&mut screen, KeyCode::Char('3'));
    let after = screen.frame(120, 40);
    let (_, y, _) = before.cursor.unwrap();
    let separator = rail_right(&before);
    // Column 1 holds the focused cursor marker; everything right of it stays put.
    for x in 2..separator {
        assert_eq!(
            before.grid.get(x, y).unwrap().symbol,
            after.grid.get(x, y).unwrap().symbol
        );
    }
    key(&mut screen, KeyCode::Char('2'));
    // Folding needs the tree view; Changes is flat by default.
    key(&mut screen, KeyCode::Char('t'));
    key(&mut screen, KeyCode::Char('h'));
    let folded = view(&mut screen);
    let rail = {
        let frame = screen.frame(120, 40);
        let right = rail_right(&frame) as usize;
        (0..40)
            .map(|y| {
                frame
                    .grid
                    .row_text(y)
                    .chars()
                    .take(right + 1)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    // The reader may list a selected directory's descendants, so only
    // the rail must hide the folded file.
    assert!(!rail.contains("new.rs"), "{folded}");
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
    key(&mut screen, KeyCode::Char('s'));
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
    key(&mut screen, KeyCode::Char('3'));
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    // Tags are read in the background like the other lists.
    wait(&mut screen, |s| view(s).contains("v1"));
    key(&mut screen, KeyCode::Char('5'));
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
    key(&mut screen, KeyCode::Char('F'));
    wait(&mut screen, |s| view(s).contains("sample.rs"));
    key(&mut screen, KeyCode::Down);
    wait(&mut screen, |s| view(s).contains("pub fn answer"));
    assert!(matches!(
        code_style(&mut screen, "pub fn answer", "fn"),
        dust::ui::Style::Syntax(_)
    ));
    key(&mut screen, KeyCode::Char('F'));
    wait(&mut screen, |s| view(s).contains("Working tree clean"));
}

fn select_file(screen: &mut GitScreen, name: &str) {
    key(screen, KeyCode::Char('2'));
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
fn short_current_ref_header_has_no_old_title_suffix_and_resize_keeps_native_cursor_geometry() {
    let dir = repo();
    git(dir.path(), &["branch", "-m", "main", "a"]);
    let head = std::fs::read(dir.path().join(".git/HEAD")).unwrap();
    let index = std::fs::read(dir.path().join(".git/index")).unwrap();
    let refs = git(
        dir.path(),
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let source = std::fs::read(dir.path().join("file.txt")).unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |screen| view(screen).contains("Initial comm"));
    key(&mut screen, KeyCode::Char('3'));
    // The reader's border title names the selected branch and nothing else.
    let reader_title = |frame: &dust::ui::Frame| {
        let rail = rail_right(frame);
        (rail + 1..frame.grid.width())
            .map(|x| frame.grid.get(x, 1).unwrap().symbol.clone())
            .collect::<String>()
            .trim_matches(|c| matches!(c, '╭' | '╮' | '─' | ' '))
            .to_owned()
    };
    wait(&mut screen, |screen| {
        let frame = screen.frame(190, 48);
        reader_title(&frame) == "⎇ a" && view(screen).contains("Initial commit")
    });
    let native = screen.frame(190, 48).cursor;
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        if width >= 100 {
            let left = rail_right(&frame) + 2;
            assert_eq!(reader_title(&frame), "⎇ a", "{width}×{height}");
            assert!(frame.grid.row_text(2).contains("Initial commit"));
            for y in 1..height - 1 {
                let expected = if y == 1 {
                    "╭"
                } else if y == height - 2 {
                    "╰"
                } else {
                    "│"
                };
                assert_eq!(frame.grid.get(left, y).unwrap().symbol, expected);
            }
        }
        let (x, y, _) = frame.cursor.expect("native branch-list cursor");
        assert_eq!(x, 1);
        assert!(
            frame.grid.row_text(y).contains("● ⎇ a"),
            "{}",
            frame.grid.row_text(y)
        );
    }
    assert_eq!(screen.frame(190, 48).cursor, native);
    assert_eq!(std::fs::read(dir.path().join(".git/HEAD")).unwrap(), head);
    assert_eq!(std::fs::read(dir.path().join(".git/index")).unwrap(), index);
    assert_eq!(
        git(
            dir.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs
    );
    assert_eq!(std::fs::read(dir.path().join("file.txt")).unwrap(), source);
}

/// The cell column where `token` starts on row `y`, read cell by cell (glyphs in the rail
/// can be wide, so a `row_text` char index is not a column).
fn token_column(frame: &dust::ui::Frame, y: u16, token: &str) -> Option<u16> {
    let width = frame.grid.width();
    let chars = token.chars().map(String::from).collect::<Vec<_>>();
    (0..width.saturating_sub(chars.len() as u16)).find(|&x| {
        chars.iter().enumerate().all(|(at, want)| {
            frame
                .grid
                .get(x + at as u16, y)
                .is_some_and(|cell| cell.symbol == *want)
        })
    })
}

/// R16 git-tab-04: context, removed and added rows share one gutter (`old new │ ± code`),
/// so code starts in one column, in the Changes reader and in the commit reader, with one
/// number column (100×30) or two.
#[test]
fn diff_code_starts_in_one_column_on_every_row_kind() {
    let dir = repo();
    std::fs::write(dir.path().join("align.txt"), "ctxa\nolda\nctxb\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Align baseline"]);
    std::fs::write(dir.path().join("align.txt"), "ctxa\nnewa\nctxb\n").unwrap();
    let check = |screen: &mut GitScreen, label: &str| {
        for (width, height) in [(100, 30), (120, 36), (190, 48)] {
            let frame = screen.frame(width, height);
            let at = ["ctxa", "olda", "newa", "ctxb"].map(|token| {
                (0..height)
                    .find_map(|y| token_column(&frame, y, token).map(|x| (x, y)))
                    .unwrap_or_else(|| panic!("{label} {width}x{height}: no {token}"))
            });
            for (token, (x, y)) in ["ctxa", "olda", "newa", "ctxb"].iter().zip(at) {
                assert_eq!(
                    x, at[0].0,
                    "{label} {width}x{height}: {token} starts at {x}"
                );
                assert_eq!(
                    frame.grid.get(x - 4, y).unwrap().symbol,
                    "│",
                    "{label} {width}x{height}: {token}"
                );
                let sign = frame.grid.get(x - 2, y).unwrap().symbol.clone();
                let expected = match *token {
                    "olda" => "−",
                    "newa" => "+",
                    _ => " ",
                };
                assert_eq!(sign, expected, "{label} {width}x{height}: {token}");
            }
        }
    };
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("newa"));
    check(&mut screen, "changes");
    git(dir.path(), &["commit", "-am", "Align change"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    key(&mut screen, KeyCode::Char('4'));
    wait(&mut screen, |s| {
        view(s).contains("newa") && view(s).contains("Align change")
    });
    check(&mut screen, "commit");
}

/// R16 git-tab-05: from 120 columns the keys sheet is two columns of one fixed width,
/// centred; a bigger terminal widens the margins, not the sheet, and never drops a column.
#[test]
fn keys_sheet_keeps_two_columns_and_its_width_as_the_terminal_grows() {
    let dir = repo();
    std::fs::write(dir.path().join("file.txt"), "new\n").unwrap();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("+ new"));
    key(&mut screen, KeyCode::Char('?'));
    let mut widths = Vec::new();
    for (width, height) in [(120, 36), (160, 40), (190, 48), (240, 60)] {
        let frame = screen.frame(width, height);
        let top = (0..height)
            .find(|&y| frame.grid.row_text(y).contains("Keys · Changes"))
            .unwrap_or_else(|| panic!("{width}x{height}: no sheet"));
        let left = (0..width)
            .find(|&x| frame.grid.get(x, top).unwrap().symbol == "╭")
            .unwrap();
        let right = (0..width)
            .rev()
            .find(|&x| frame.grid.get(x, top).unwrap().symbol == "╮")
            .unwrap();
        let sheet = right - left + 1;
        assert!(sheet <= 112, "{width}x{height}: {sheet} wide");
        assert!(
            left.abs_diff(width - 1 - right) <= 1,
            "{width}x{height}: not centred ({left}, {right})"
        );
        widths.push(sheet);
        // Two `key  label` cells on one row: Navigate's `Tab` beside `C-h/j/k/l`.
        let rows = (top..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>();
        assert!(
            rows.iter()
                .any(|row| row.contains("Next pane") && row.contains("Move focus")),
            "{width}x{height}:\n{}",
            rows.join("\n")
        );
    }
    assert!(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        "{widths:?}"
    );
}

/// R16 git-tab-11: every Branches sub-tab carries its count, and an empty Stashes pane says
/// so on a body row inside its frame, with the count chip in its title.
#[test]
fn branch_tabs_all_count_and_an_empty_pane_speaks_inside_its_frame() {
    let dir = repo();
    git(dir.path(), &["branch", "side"]);
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| view(s).contains("side"));
    for (width, height) in [(100, 30), (120, 36), (190, 48)] {
        let frame = screen.frame(width, height);
        let rows = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>();
        assert!(
            rows.iter()
                .any(|row| row.contains("Local 2 │ Remotes 0 │ Tags 0")),
            "{}",
            rows.join("\n")
        );
        let empty = rows
            .iter()
            .position(|row| row.contains("No stashes"))
            .unwrap_or_else(|| panic!("{width}x{height}:\n{}", rows.join("\n")));
        assert!(
            rows[empty].starts_with('│'),
            "{width}x{height}: {}",
            rows[empty]
        );
        assert!(rows[empty - 1].starts_with('╭') && rows[empty - 1].contains("Stashes"));
        assert!(rows[empty - 1].contains(" 0 "), "{}", rows[empty - 1]);
        assert!(rows[empty + 1].starts_with('╰') && !rows[empty + 1].contains("No stashes"));
    }
}
