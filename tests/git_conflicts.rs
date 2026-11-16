//! Conflict resolution from the Git tab: merge, rebase, cherry-pick and
//! pull stop in one place, label each hunk's sides, pick per hunk, count what is left and
//! name the operation on continue, skip and abort. Temporary repositories and local bare
//! remotes only.
use dust::config::{GitKeys, load_bundled_config};
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{KeyInputKind, UiEvent};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const STORY: &str = "story.txt";

fn output(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env_remove("GIT_REFLOG_ACTION")
        .output()
        .unwrap()
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = output(root, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().into()
}

/// A Git step expected to stop on a conflict.
fn attempt(root: &Path, args: &[&str]) {
    let out = output(root, args);
    assert!(
        !out.status.success(),
        "git {args:?} was expected to stop on a conflict"
    );
}

fn configure(root: &Path) {
    git(root, &["config", "user.name", "Conflict Test"]);
    git(root, &["config", "user.email", "conflicts@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "merge.conflictStyle", "merge"]);
}

/// Fifteen lines, `line N`, with `edits` in place: conflicts at lines 3, 9 and 15 stay
/// apart (five unchanged lines between them).
fn story(edits: &[(usize, &str)]) -> String {
    (1..=15)
        .map(|number| {
            edits.iter().find(|(line, _)| *line == number).map_or_else(
                || format!("line {number}\n"),
                |(_, text)| format!("{text}\n"),
            )
        })
        .collect()
}

fn commit_story(root: &Path, edits: &[(usize, &str)], subject: &str) -> String {
    std::fs::write(root.join(STORY), story(edits)).unwrap();
    git(root, &["add", "--", STORY]);
    git(root, &["commit", "-q", "-m", subject]);
    git(root, &["rev-parse", "HEAD"])
}

/// A repository on `main` with the base story committed.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    configure(dir.path());
    commit_story(dir.path(), &[], "Base story");
    dir
}

fn on_disk(root: &Path) -> String {
    std::fs::read_to_string(root.join(STORY)).unwrap()
}

fn unmerged(root: &Path) -> String {
    git(root, &["ls-files", "--unmerged"])
}

fn press(screen: &mut GitScreen, code: KeyCode, modifiers: Modifiers) -> ScreenOutcome {
    screen.event(UiEvent::Key {
        key: KeyEvent { code, modifiers },
        kind: KeyInputKind::Press,
    })
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    press(screen, code, Modifiers::default());
}

fn ctrl(screen: &mut GitScreen, code: KeyCode) {
    press(
        screen,
        code,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(140, 40);
    (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The operation banner (row 1); empty when no operation is paused.
fn banner(screen: &mut GitScreen) -> String {
    let banner = screen.frame(140, 40).grid.row_text(1);
    if banner.contains('⟳') {
        banner
    } else {
        String::new()
    }
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

fn shows(screen: &mut GitScreen, text: &str) {
    wait(screen, |s| view(s).contains(text));
}

fn settle(screen: &mut GitScreen) {
    wait(screen, |s| !s.busy());
    std::thread::sleep(Duration::from_millis(50));
    screen.poll();
}

/// Confirms the open sheet and waits for Git to finish.
fn confirm(screen: &mut GitScreen) {
    ctrl(screen, KeyCode::Enter);
    settle(screen);
}

fn button(verb: &str) -> String {
    format!("[ {verb} ]")
}

/// The screen on Changes with the conflicted story selected, then the reader focused.
fn reader(root: &Path, preset: Option<GitKeys>) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    if let Some(preset) = preset {
        let mut config = load_bundled_config().config;
        config.ui.git.keys = preset;
        screen.configure_input(&config);
    }
    wait(&mut screen, |s| banner(s).contains('⟳'));
    key(&mut screen, KeyCode::Char('2'));
    shows(&mut screen, "<<<<<<< ours");
    settle(&mut screen);
    key(&mut screen, KeyCode::Char('0'));
    screen
}

/// One pick: the key, then the count the banner moves to.
fn pick(screen: &mut GitScreen, code: KeyCode, progress: &str) {
    settle(screen);
    key(screen, code);
    shows(screen, progress);
    settle(screen);
}

/// `main` and `feature` both change lines 3, 9 and 15; merging `feature` stops on three
/// conflicts, written with their base (diff3).
fn three_way() -> tempfile::TempDir {
    let dir = repo();
    let root = dir.path();
    git(root, &["config", "merge.conflictStyle", "diff3"]);
    git(root, &["switch", "-q", "-c", "feature"]);
    commit_story(
        root,
        &[(3, "feature 3"), (9, "feature 9"), (15, "feature 15")],
        "Feature story",
    );
    git(root, &["switch", "-q", "main"]);
    commit_story(
        root,
        &[(3, "main 3"), (9, "main 9"), (15, "main 15")],
        "Main story",
    );
    attempt(root, &["merge", "--no-edit", "feature"]);
    dir
}

#[test]
fn a_merge_labels_each_side_and_picks_ours_theirs_or_both_per_hunk() {
    let dir = three_way();
    let root = dir.path();
    let base = git(root, &["merge-base", "main", "feature"]);
    let mut screen = reader(root, None);
    let shown = view(&mut screen);
    for label in [
        "<<<<<<< ours · main".to_owned(),
        format!("||||||| base · {}", &base[..7]),
        ">>>>>>> theirs · feature".to_owned(),
    ] {
        assert!(shown.contains(&label), "{label}:\n{shown}");
    }
    // The banner: what is merged into what, the hunks left, and only the keys Git has.
    let top = banner(&mut screen);
    assert!(top.contains("Merge · feature into main"), "{top}");
    assert!(
        top.contains("merge: 0 of 3 conflict hunks resolved"),
        "{top}"
    );
    assert!(
        top.contains("X abort merge") && !top.contains("skip"),
        "{top}"
    );
    assert!(
        view(&mut screen).contains("o ours"),
        "the footer offers the picks"
    );

    // The cursor starts on the first conflict: o keeps ours there.
    pick(
        &mut screen,
        KeyCode::Char('o'),
        "merge: 1 of 3 conflict hunks resolved",
    );
    assert!(
        on_disk(root).starts_with("line 1\nline 2\nmain 3\nline 4\n"),
        "{}",
        on_disk(root)
    );
    assert!(view(&mut screen).contains("2 conflicts left in story.txt"));
    pick(
        &mut screen,
        KeyCode::Char('t'),
        "merge: 2 of 3 conflict hunks resolved",
    );
    assert!(
        on_disk(root).contains("line 8\nfeature 9\nline 10\n"),
        "{}",
        on_disk(root)
    );
    assert!(
        !unmerged(root).is_empty(),
        "the file stays conflicted until its last pick"
    );
    settle(&mut screen);
    key(&mut screen, KeyCode::Char('b'));
    shows(&mut screen, "every conflict is resolved");
    settle(&mut screen);
    assert_eq!(
        on_disk(root),
        story(&[(3, "main 3"), (9, "feature 9"), (15, "main 15")])
            .replace("main 15\n", "main 15\nfeature 15\n")
    );
    // The last pick staged the file.
    assert!(unmerged(root).is_empty());
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), STORY);

    // Git has no skip for a merge.
    key(&mut screen, KeyCode::Char('S'));
    shows(&mut screen, "Git has no skip for a merge");
    wait(&mut screen, |s| banner(s).contains("C continue merge"));
    key(&mut screen, KeyCode::Char('C'));
    shows(&mut screen, "Continue resolved merge?");
    assert!(
        view(&mut screen).contains(&button("Continue merge")),
        "{}",
        view(&mut screen)
    );
    confirm(&mut screen);
    wait(&mut screen, |s| banner(s).is_empty());
    assert_eq!(git(root, &["rev-list", "--merges", "--count", "HEAD"]), "1");
}

#[test]
fn e_opens_a_conflicted_file_at_its_first_conflict() {
    let dir = three_way();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| banner(s).contains('⟳'));
    key(&mut screen, KeyCode::Char('2'));
    shows(&mut screen, "<<<<<<< ours");
    match press(&mut screen, KeyCode::Char('e'), Modifiers::default()) {
        ScreenOutcome::OpenFileAt(path, line) => {
            assert!(path.ends_with(STORY), "{}", path.display());
            assert_eq!(line, 3);
        }
        other => panic!("e asked for {other:?}"),
    }
    // The help sheet lists the conflict keys.
    key(&mut screen, KeyCode::Char('?'));
    let frame = screen.frame(190, 60);
    let help = (0..60)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    for row in [
        "Take both",
        "Take the side under the cursor",
        "Edit at the conflict",
        "Continue · skip · abort menu",
    ] {
        assert!(help.contains(row), "{row}:\n{help}");
    }
}

/// `feature` replays two commits onto `main`; both conflict.
fn rebasing() -> (tempfile::TempDir, String) {
    let dir = repo();
    let root = dir.path();
    git(root, &["switch", "-q", "-c", "feature"]);
    let shout = commit_story(root, &[(3, "FEATURE 3")], "Shout the greeting");
    commit_story(
        root,
        &[(3, "FEATURE 3"), (15, "feature 15")],
        "Wave goodbye",
    );
    git(root, &["switch", "-q", "main"]);
    commit_story(root, &[(3, "main 3"), (15, "main 15")], "Main story");
    git(root, &["switch", "-q", "feature"]);
    attempt(root, &["rebase", "main"]);
    (dir, shout)
}

#[test]
fn a_rebase_names_the_upstream_ours_and_the_replayed_commit_theirs() {
    let (dir, shout) = rebasing();
    let root = dir.path();
    let mut screen = reader(root, None);
    let shown = view(&mut screen);
    assert!(shown.contains("<<<<<<< ours · main"), "{shown}");
    let theirs = format!(">>>>>>> theirs · {} Shout the greeting", &shout[..7]);
    assert!(shown.contains(&theirs), "{theirs}:\n{shown}");
    let top = banner(&mut screen);
    for text in [
        "Rebase · rebase · feature onto main",
        "commit 1 of 2",
        "rebase: 0 of 1 conflict hunk resolved",
        "S skip commit",
    ] {
        assert!(top.contains(text), "{text}: {top}");
    }

    // Enter takes the side the cursor is on: down to the replayed commit's line.
    for _ in 0..3 {
        key(&mut screen, KeyCode::Char('j'));
    }
    settle(&mut screen);
    key(&mut screen, KeyCode::Enter);
    shows(&mut screen, "Took theirs");
    settle(&mut screen);
    assert!(
        on_disk(root).contains("line 2\nFEATURE 3\nline 4\n"),
        "{}",
        on_disk(root)
    );
    assert!(unmerged(root).is_empty());

    wait(&mut screen, |s| banner(s).contains("C continue rebase"));
    key(&mut screen, KeyCode::Char('C'));
    shows(&mut screen, "Continue resolved rebase?");
    assert!(view(&mut screen).contains(&button("Continue rebase")));
    confirm(&mut screen);
    // The next commit conflicts too: skip it.
    wait(&mut screen, |s| banner(s).contains("commit 2 of 2"));
    key(&mut screen, KeyCode::Char('S'));
    shows(&mut screen, "Skip current rebase commit?");
    assert!(view(&mut screen).contains(&button("Skip commit")));
    confirm(&mut screen);
    wait(&mut screen, |s| banner(s).is_empty());
    let subjects = git(root, &["log", "--format=%s"]);
    assert!(
        subjects.contains("Shout the greeting") && !subjects.contains("Wave goodbye"),
        "{subjects}"
    );
}

/// `main` cherry-picks `topic`'s commit; both change line 3.
fn picking() -> (tempfile::TempDir, String) {
    let dir = repo();
    let root = dir.path();
    git(root, &["switch", "-q", "-c", "topic"]);
    let picked = commit_story(root, &[(3, "topic 3")], "Greet from topic");
    git(root, &["switch", "-q", "main"]);
    commit_story(root, &[(3, "main 3")], "Main story");
    attempt(root, &["cherry-pick", "topic"]);
    (dir, picked)
}

#[test]
fn a_cherry_pick_keeps_both_sides_and_continues_skips_or_aborts_by_name() {
    let (dir, picked) = picking();
    let root = dir.path();
    let mut screen = reader(root, None);
    let line = format!("{} Greet from topic", &picked[..7]);
    let shown = view(&mut screen);
    assert!(
        shown.contains(&format!(">>>>>>> theirs · {line}")),
        "{shown}"
    );
    let top = banner(&mut screen);
    assert!(
        top.contains(&format!("Cherry-pick · {line} onto main")),
        "{top}"
    );
    assert!(
        top.contains("S skip commit") && top.contains("X abort cherry-pick"),
        "{top}"
    );

    key(&mut screen, KeyCode::Char('X'));
    shows(&mut screen, "Abort cherry-pick?");
    assert!(view(&mut screen).contains(&button("Abort cherry-pick")));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('S'));
    shows(&mut screen, "Skip current cherry-pick commit?");
    assert!(view(&mut screen).contains(&button("Skip commit")));
    key(&mut screen, KeyCode::Escape);

    pick(
        &mut screen,
        KeyCode::Char('b'),
        "every conflict is resolved",
    );
    assert!(
        on_disk(root).contains("line 2\nmain 3\ntopic 3\nline 4\n"),
        "{}",
        on_disk(root)
    );
    wait(&mut screen, |s| {
        banner(s).contains("C continue cherry-pick")
    });
    key(&mut screen, KeyCode::Char('C'));
    shows(&mut screen, "Continue resolved cherry-pick?");
    assert!(view(&mut screen).contains(&button("Continue cherry-pick")));
    confirm(&mut screen);
    wait(&mut screen, |s| banner(s).is_empty());
    assert_eq!(git(root, &["log", "-1", "--format=%s"]), "Greet from topic");
}

#[test]
fn a_cherry_pick_skip_drops_the_commit() {
    let (dir, _) = picking();
    let root = dir.path();
    let mut screen = reader(root, None);
    key(&mut screen, KeyCode::Char('S'));
    shows(&mut screen, "Skip current cherry-pick commit?");
    confirm(&mut screen);
    wait(&mut screen, |s| banner(s).is_empty());
    assert_eq!(git(root, &["log", "-1", "--format=%s"]), "Main story");
    assert_eq!(on_disk(root), story(&[(3, "main 3")]));
}

/// `main` and its upstream both change line 3; `pull.rebase = false` merges.
fn pulled() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = repo();
    let root = dir.path();
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(
        root,
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    git(root, &["push", "-q", "-u", "origin", "main"]);
    let other = tempfile::tempdir().unwrap();
    let clone = other.path().join("clone");
    git(
        other.path(),
        &["clone", "-q", bare.path().to_str().unwrap(), "clone"],
    );
    configure(&clone);
    commit_story(&clone, &[(3, "origin 3")], "Origin story");
    git(&clone, &["push", "-q", "origin", "main"]);
    commit_story(root, &[(3, "main 3")], "Main story");
    git(root, &["config", "pull.rebase", "false"]);
    (dir, bare)
}

#[test]
fn a_pull_that_merges_stops_with_named_sides_and_a_menu_continues_it() {
    let (dir, _bare) = pulled();
    let root = dir.path();
    let mut screen = GitScreen::open(root).unwrap();
    shows(&mut screen, "main");
    key(&mut screen, KeyCode::Char('p'));
    shows(&mut screen, &button("Pull"));
    confirm(&mut screen);
    shows(&mut screen, "Stopped on a conflict");
    assert!(
        !view(&mut screen).contains("S skips"),
        "a merge has no skip"
    );
    let top = banner(&mut screen);
    assert!(top.contains("Pull · origin/main into main"), "{top}");
    key(&mut screen, KeyCode::Char('2'));
    shows(&mut screen, ">>>>>>> theirs · origin/main");
    assert!(view(&mut screen).contains("<<<<<<< ours · main"));
    settle(&mut screen);
    key(&mut screen, KeyCode::Char('0'));
    pick(
        &mut screen,
        KeyCode::Char('o'),
        "every conflict is resolved",
    );
    assert_eq!(on_disk(root), story(&[(3, "main 3")]));

    // M opens the operation's options; skip is not one for a merge.
    wait(&mut screen, |s| banner(s).contains("C continue merge"));
    key(&mut screen, KeyCode::Char('M'));
    shows(&mut screen, "Pull options");
    let menu = view(&mut screen);
    assert!(
        menu.contains("Continue merge") && menu.contains("Abort merge"),
        "{menu}"
    );
    assert!(!menu.contains("Skip commit"), "{menu}");
    key(&mut screen, KeyCode::Enter);
    shows(&mut screen, "Continue resolved merge?");
    confirm(&mut screen);
    wait(&mut screen, |s| banner(s).is_empty());
    assert_eq!(git(root, &["rev-list", "--merges", "--count", "HEAD"]), "1");
}

#[test]
fn the_lazygit_preset_picks_with_space_and_b_and_opens_the_menu_with_m() {
    let dir = three_way();
    let root = dir.path();
    let mut screen = reader(root, Some(GitKeys::Lazygit));
    // Space takes the side under the cursor: down from the first marker to ours.
    key(&mut screen, KeyCode::Char('j'));
    settle(&mut screen);
    key(&mut screen, KeyCode::Char(' '));
    shows(&mut screen, "merge: 1 of 3 conflict hunks resolved");
    assert!(
        on_disk(root).contains("line 2\nmain 3\nline 4\n"),
        "{}",
        on_disk(root)
    );
    pick(
        &mut screen,
        KeyCode::Char('b'),
        "merge: 2 of 3 conflict hunks resolved",
    );
    assert!(
        on_disk(root).contains("line 8\nmain 9\nfeature 9\nline 10\n"),
        "{}",
        on_disk(root)
    );
    settle(&mut screen);
    key(&mut screen, KeyCode::Char('m'));
    shows(&mut screen, "Merge options");
    let menu = view(&mut screen);
    assert!(
        menu.contains("Continue merge") && menu.contains("Abort merge"),
        "{menu}"
    );
}

// The tutor's Conflicts chapter (`src/git/screen/tutor/conflicts.rs`), walked through on
// real input: each lesson advances only once the practice repository shows it done.

fn tutor_banner(screen: &mut GitScreen) -> String {
    let frame = screen.frame(190, 48);
    (0..48)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("Git tutor ·"))
        .unwrap_or_default()
}

fn tutor_number(name: &str) -> usize {
    GitScreen::tutor_outline(false, false)
        .iter()
        .position(|lesson| lesson == name)
        .unwrap_or_else(|| panic!("no lesson {name}"))
        + 1
}

/// Waits for the lesson `Chapter · Title`, on its own practice repository.
fn tutor_at(screen: &mut GitScreen, name: &str) {
    let number = tutor_number(name);
    let total = GitScreen::tutor_outline(false, false).len();
    let expected = format!("· {number}/{total} · {name}");
    wait(screen, |s| tutor_banner(s).contains(&expected));
    settle(screen);
}

/// Changes, then the reader on the one conflicted file.
fn tutor_reader(screen: &mut GitScreen) {
    key(screen, KeyCode::Char('2'));
    shows(screen, "<<<<<<< ours");
    settle(screen);
    key(screen, KeyCode::Char('0'));
}

#[test]
fn the_tutor_conflicts_chapter_advances_on_real_picks() {
    let dir = repo();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    let s = &mut screen;
    shows(s, "main");
    let first = tutor_number("Conflicts · Merge conflict");
    key(s, KeyCode::Char(':'));
    s.event(UiEvent::Paste(format!("tutor git {first}")));
    key(s, KeyCode::Enter);

    // Merge: b keeps both sides, then C and Enter record the merge.
    tutor_at(s, "Conflicts · Merge conflict");
    tutor_reader(s);
    key(s, KeyCode::Char('b'));
    shows(s, "every conflict is resolved");
    settle(s);
    key(s, KeyCode::Char('C'));
    shows(s, "Continue resolved merge?");
    key(s, KeyCode::Enter);

    // Rebase: Enter on the replayed commit's line takes it, then C and Enter.
    tutor_at(s, "Conflicts · Rebase conflict");
    tutor_reader(s);
    for _ in 0..3 {
        key(s, KeyCode::Char('j'));
    }
    key(s, KeyCode::Enter);
    shows(s, "Took theirs");
    settle(s);
    key(s, KeyCode::Char('C'));
    shows(s, "Continue resolved rebase?");
    key(s, KeyCode::Enter);

    // Cherry-pick: t takes the picked commit's side.
    tutor_at(s, "Conflicts · Cherry-pick conflict");
    tutor_reader(s);
    key(s, KeyCode::Char('t'));
    shows(s, "every conflict is resolved");
    settle(s);
    key(s, KeyCode::Char('C'));
    shows(s, "Continue resolved cherry-pick?");
    key(s, KeyCode::Enter);

    // Pull: o keeps ours, then the M menu continues the merge.
    tutor_at(s, "Conflicts · Pull conflict");
    tutor_reader(s);
    key(s, KeyCode::Char('o'));
    shows(s, "every conflict is resolved");
    settle(s);
    key(s, KeyCode::Char('M'));
    shows(s, "Pull options");
    key(s, KeyCode::Enter);
    shows(s, "Continue resolved merge?");
    key(s, KeyCode::Enter);

    // Per hunk: o on the first conflict, t on the other.
    tutor_at(s, "Conflicts · Pick per hunk");
    tutor_reader(s);
    key(s, KeyCode::Char('o'));
    shows(s, "1 conflict left in notes.txt");
    settle(s);
    key(s, KeyCode::Char('t'));
    let next = tutor_number("Conflicts · Pick per hunk") + 1;
    wait(s, |s| tutor_banner(s).contains(&format!("· {next}/")));
}

/// The cells from `from` to `to` (exclusive) on row `y`, one symbol each.
fn cells(frame: &dust::ui::Frame, y: u16, from: u16, to: u16) -> String {
    (from..to)
        .filter_map(|x| frame.grid.get(x, y))
        .map(|cell| cell.symbol.to_string())
        .collect()
}

/// The box of the overlay titled `title`: its left, top, right and bottom edges.
fn overlay(frame: &dust::ui::Frame, title: &str) -> Option<(u16, u16, u16, u16)> {
    let (width, height) = (frame.grid.width(), frame.grid.height());
    let symbol = |x: u16, y: u16| {
        frame
            .grid
            .get(x, y)
            .map_or(String::new(), |cell| cell.symbol.to_string())
    };
    for top in 1..height {
        // Right to left: a sidebar box can start on the same row, left of the sheet.
        for left in (0..width).rev() {
            if !["╭", "┌"].contains(&symbol(left, top).as_str())
                || !cells(frame, top, left, (left + 40).min(width)).contains(title)
            {
                continue;
            }
            let right =
                (left + 1..width).find(|&x| ["╮", "┐"].contains(&symbol(x, top).as_str()))?;
            let bottom =
                (top + 1..height).find(|&y| ["╰", "└"].contains(&symbol(left, y).as_str()))?;
            return Some((left, top, right, bottom));
        }
    }
    None
}

const BOX: &[&str] = &[
    "│", "─", "╭", "╮", "╰", "╯", "┌", "┐", "└", "┘", "┬", "┴", "├", "┤", "┼", "━", "┃",
];

/// R16 #1: an overlay's box is opaque (no glyph of the panes behind it shows inside) and
/// the panes recede to the border colour around it, their text kept, never blanked.
fn occludes(screen: &mut GitScreen, plain: &dust::ui::Frame, width: u16, height: u16, title: &str) {
    use dust::ui::{GitRole, GitSurface, Style};
    let frame = screen.frame(width, height);
    let shown = (0..height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    let (left, top, right, bottom) =
        overlay(&frame, title).unwrap_or_else(|| panic!("no `{title}` overlay:\n{shown}"));
    for y in top + 1..bottom {
        for x in left + 1..right {
            let symbol = frame.grid.get(x, y).unwrap().symbol.to_string();
            assert!(
                !BOX.contains(&symbol.as_str()),
                "`{symbol}` at ({x},{y}) inside `{title}` at {width}x{height}:\n{shown}"
            );
        }
    }
    let mut dimmed = 0;
    for y in 2..height - 1 {
        for x in 0..width {
            let near = x + 1 >= left && x <= right + 1 && y + 1 >= top && y <= bottom + 1;
            let before = plain.grid.get(x, y).unwrap();
            if near || before.continuation || !BOX.contains(&before.symbol.to_string().as_str()) {
                continue;
            }
            let after = frame.grid.get(x, y).unwrap();
            assert_eq!(
                after.symbol, before.symbol,
                "the pane at ({x},{y}) is blanked at {width}x{height}:\n{shown}"
            );
            assert!(
                matches!(
                    after.style,
                    Style::Git {
                        role: GitRole::Border,
                        surface: GitSurface::Canvas,
                        ..
                    }
                ),
                "the pane at ({x},{y}) is not dimmed at {width}x{height}: {:?}\n{shown}",
                after.style
            );
            dimmed += 1;
        }
    }
    assert!(
        dimmed > 0,
        "no pane border behind `{title}` at {width}x{height}:\n{shown}"
    );
}

#[test]
fn a_sheet_hides_the_panes_under_it_and_dims_the_rest() {
    let dir = three_way();
    let mut screen = GitScreen::open(dir.path()).unwrap();
    wait(&mut screen, |s| banner(s).contains('⟳'));
    settle(&mut screen);
    for (width, height) in [(120, 36), (190, 48)] {
        // Each sheet is compared with the frame just before it opened: a preview that lands
        // late (a loaded machine) changes the panes, never what a sheet does to them.
        let plain = screen.frame(width, height);
        // The `:` prompt (a generic sheet) and the abort confirmation (the operation dialog).
        key(&mut screen, KeyCode::Char(':'));
        shows(&mut screen, "Command");
        occludes(&mut screen, &plain, width, height, "Command");
        key(&mut screen, KeyCode::Escape);
        settle(&mut screen);
        let plain = screen.frame(width, height);
        key(&mut screen, KeyCode::Char('X'));
        shows(&mut screen, "Abort merge?");
        occludes(&mut screen, &plain, width, height, "Abort merge?");
        key(&mut screen, KeyCode::Escape);
        settle(&mut screen);
    }
}
