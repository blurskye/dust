//! Pull, undo and redo, history rewriting and tag push from the Git tab (DESIGN.md §5.11),
//! on temporary repositories and local bare remotes only.
use dust::config::{GitKeys, load_bundled_config};
use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
use dust::git::screen::GitScreen;
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
        .env_remove("GIT_REFLOG_ACTION")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().into()
}

fn configure(root: &Path) {
    git(root, &["config", "user.name", "History Test"]);
    git(root, &["config", "user.email", "history@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "tag.gpgsign", "false"]);
}

fn commit(root: &Path, subject: &str) -> String {
    std::fs::write(root.join(format!("{subject}.txt")), format!("{subject}\n")).unwrap();
    git(root, &["add", "--", &format!("{subject}.txt")]);
    git(root, &["commit", "-q", "-m", subject]);
    git(root, &["rev-parse", "HEAD"])
}

/// A repository with one commit per subject, oldest first.
fn repo(subjects: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    configure(dir.path());
    for subject in subjects {
        commit(dir.path(), subject);
    }
    dir
}

/// A bare remote with `root`'s main pushed and tracked.
fn remote(root: &Path) -> tempfile::TempDir {
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(root, &["remote", "add", "origin", bare.path().to_str().unwrap()]);
    git(root, &["push", "-q", "-u", "origin", "main"]);
    bare
}

/// Another clone of `bare` that commits `subject` and pushes it.
fn push_from_elsewhere(bare: &Path, subject: &str) {
    let other = tempfile::tempdir().unwrap();
    let clone = other.path().join("clone");
    git(other.path(), &["clone", "-q", bare.to_str().unwrap(), "clone"]);
    configure(&clone);
    commit(&clone, subject);
    git(&clone, &["push", "-q", "origin", "main"]);
}

fn subjects(root: &Path) -> String {
    git(root, &["log", "--format=%s"]).replace('\n', " ")
}

fn short(id: &str) -> &str {
    &id[..7]
}

fn press(screen: &mut GitScreen, code: KeyCode, modifiers: Modifiers) {
    screen.event(UiEvent::Key { key: KeyEvent { code, modifiers }, kind: KeyInputKind::Press });
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    press(screen, code, Modifiers::default());
}

fn ctrl(screen: &mut GitScreen, code: KeyCode) {
    press(screen, code, Modifiers { control: true, ..Modifiers::default() });
}

fn view(screen: &mut GitScreen) -> String {
    let frame = screen.frame(140, 40);
    (0..40).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
}

fn wait(screen: &mut GitScreen, ready: impl Fn(&mut GitScreen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        screen.poll();
        if ready(screen) {
            return;
        }
        assert!(Instant::now() < deadline, "screen timed out:\n{}", view(screen));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shows(screen: &mut GitScreen, text: &str) {
    wait(screen, |s| view(s).contains(text));
}

/// Confirms the open sheet and waits for Git to finish.
fn confirm(screen: &mut GitScreen) {
    ctrl(screen, KeyCode::Enter);
    wait(screen, |s| !s.busy());
    // The refresh that follows a write settles before the next key.
    std::thread::sleep(Duration::from_millis(50));
    screen.poll();
}

/// The sheet's confirm button, bracketed: `[ Pull ]`.
fn button(verb: &str) -> String {
    format!("[ {verb} ]")
}

fn open(root: &Path) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    shows(&mut screen, "main");
    screen
}

/// The History list with the `index`th newest commit selected.
fn history(root: &Path, oldest: &str, index: usize) -> GitScreen {
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('4'));
    shows(&mut screen, oldest);
    for _ in 0..index {
        key(&mut screen, KeyCode::Char('j'));
    }
    screen
}

#[test]
fn p_pulls_with_the_configured_rebase_and_names_the_upstream() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    let bare = remote(root);
    push_from_elsewhere(bare.path(), "theirs");
    commit(root, "ours");
    git(root, &["config", "pull.rebase", "true"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('p'));
    let sheet = view(&mut screen);
    for text in ["Pull origin/main into main?".to_owned(), "rebase".to_owned(), button("Pull")] {
        assert!(sheet.contains(&text), "{text}:\n{sheet}");
    }
    confirm(&mut screen);
    assert_eq!(subjects(root), "ours theirs alpha");
    assert_eq!(git(root, &["rev-list", "--merges", "HEAD"]), "");
    shows(&mut screen, "Pulled origin/main into main (rebase)");

    // `pull.rebase = false` merges.
    push_from_elsewhere(bare.path(), "again");
    commit(root, "mine");
    git(root, &["config", "pull.rebase", "false"]);
    key(&mut screen, KeyCode::Char('p'));
    shows(&mut screen, "Pull origin/main into main?");
    assert!(view(&mut screen).contains("merge"));
    confirm(&mut screen);
    assert_eq!(git(root, &["rev-list", "--merges", "--count", "HEAD"]), "1", "{}", view(&mut screen));
    shows(&mut screen, "(merge)");
}

#[test]
fn a_pull_that_conflicts_stops_in_the_operation_banner() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    let bare = remote(root);
    let other = tempfile::tempdir().unwrap();
    let clone = other.path().join("clone");
    git(other.path(), &["clone", "-q", bare.path().to_str().unwrap(), "clone"]);
    configure(&clone);
    std::fs::write(clone.join("alpha.txt"), "theirs\n").unwrap();
    git(&clone, &["commit", "-q", "-am", "theirs"]);
    git(&clone, &["push", "-q", "origin", "main"]);
    std::fs::write(root.join("alpha.txt"), "ours\n").unwrap();
    git(root, &["commit", "-q", "-am", "ours"]);
    git(root, &["config", "pull.rebase", "true"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('p'));
    shows(&mut screen, &button("Pull"));
    confirm(&mut screen);
    shows(&mut screen, "Stopped on a conflict");
    assert!(root.join(".git/rebase-merge").exists() || root.join(".git/rebase-apply").exists());
    git(root, &["rebase", "--abort"]);
}

#[test]
fn p_without_an_upstream_says_how_to_set_one() {
    let dir = repo(&["alpha"]);
    let mut screen = open(dir.path());
    key(&mut screen, KeyCode::Char('p'));
    shows(&mut screen, "main has no upstream");
}

#[test]
fn z_undoes_and_shift_z_redoes_with_a_sheet_that_shows_the_move() {
    let dir = repo(&["alpha", "bravo", "charlie"]);
    let root = dir.path();
    let bravo = git(root, &["rev-parse", "HEAD~1"]);
    let charlie = git(root, &["rev-parse", "HEAD"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('z'));
    let sheet = view(&mut screen);
    for text in [
        "Undo the last action?".to_owned(),
        "commit: charlie".to_owned(),
        format!("{} → {}", short(&charlie), short(&bravo)),
        format!("git reset --keep {}", short(&bravo)),
        button("Undo"),
    ] {
        assert!(sheet.contains(&text), "{text}:\n{sheet}");
    }
    confirm(&mut screen);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), bravo);
    shows(&mut screen, "Undid commit: charlie");
    // A second undo walks one action further back.
    key(&mut screen, KeyCode::Char('z'));
    shows(&mut screen, "commit: bravo");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('Z'));
    shows(&mut screen, "Redo the undone action?");
    shows(&mut screen, &button("Redo"));
    confirm(&mut screen);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), charlie);
    key(&mut screen, KeyCode::Char('Z'));
    shows(&mut screen, "Nothing to redo");

    // `reset --keep` refuses rather than overwrite an uncommitted change to a file it moves.
    std::fs::write(root.join("charlie.txt"), "draft\n").unwrap();
    key(&mut screen, KeyCode::Char('z'));
    shows(&mut screen, &button("Undo"));
    confirm(&mut screen);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), charlie);
    assert_eq!(std::fs::read_to_string(root.join("charlie.txt")).unwrap(), "draft\n");
}

#[test]
fn z_switches_back_after_a_branch_switch() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    git(root, &["switch", "-q", "-c", "topic"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('z'));
    shows(&mut screen, "topic → main");
    confirm(&mut screen);
    assert_eq!(git(root, &["branch", "--show-current"]), "main");
}

/// Runs one History key on bravo (the third newest of four) and confirms its sheet.
fn rewrite(key_code: KeyCode, title: &str) -> (tempfile::TempDir, GitScreen) {
    let dir = repo(&["alpha", "bravo", "charlie", "delta"]);
    let bravo = git(dir.path(), &["rev-parse", "HEAD~2"]);
    let mut screen = history(dir.path(), "alpha", 2);
    key(&mut screen, key_code);
    let sheet = view(&mut screen);
    assert!(sheet.contains(title), "{title}:\n{sheet}");
    assert!(sheet.contains(short(&bravo)), "{sheet}");
    assert!(sheet.contains("git rebase -i --autostash"), "{sheet}");
    confirm(&mut screen);
    (dir, screen)
}

#[test]
fn history_keys_squash_fixup_drop_and_move_the_selected_commit() {
    let (dir, mut screen) = rewrite(KeyCode::Char('s'), "Squash into the commit below");
    assert_eq!(subjects(dir.path()), "delta charlie alpha");
    assert_eq!(git(dir.path(), &["log", "-1", "--format=%B", "HEAD~2"]), "alpha\n\nbravo");
    shows(&mut screen, "Squashed");
    let (dir, _) = rewrite(KeyCode::Char('f'), "Fixup into the commit below");
    assert_eq!(subjects(dir.path()), "delta charlie alpha");
    assert_eq!(git(dir.path(), &["log", "-1", "--format=%B", "HEAD~2"]), "alpha");
    assert_eq!(git(dir.path(), &["show", "HEAD~2:bravo.txt"]), "bravo");
    let (dir, _) = rewrite(KeyCode::Char('d'), "Drop");
    assert_eq!(subjects(dir.path()), "delta charlie alpha");
    let (dir, _) = rewrite(KeyCode::Char('J'), "Move down");
    assert_eq!(subjects(dir.path()), "delta charlie alpha bravo");
    let (dir, _) = rewrite(KeyCode::Char('K'), "Move up");
    assert_eq!(subjects(dir.path()), "delta bravo charlie alpha");
}

#[test]
fn e_stops_the_rebase_at_the_commit_and_c_continues_it() {
    let (dir, mut screen) = rewrite(KeyCode::Char('e'), "Edit");
    let root = dir.path();
    shows(&mut screen, "Stopped at");
    assert!(root.join(".git/rebase-merge").exists());
    assert_eq!(git(root, &["log", "-1", "--format=%s"]), "bravo");
    key(&mut screen, KeyCode::Char('C'));
    shows(&mut screen, "Continue resolved rebase?");
    ctrl(&mut screen, KeyCode::Enter);
    wait(&mut screen, |_| !root.join(".git/rebase-merge").exists());
    assert_eq!(subjects(root), "delta charlie bravo alpha");
}

#[test]
fn w_rewords_and_a_amends_a_commit_below_head() {
    let dir = repo(&["alpha", "bravo", "charlie", "delta"]);
    let root = dir.path();
    let mut screen = history(root, "alpha", 2);
    key(&mut screen, KeyCode::Char('W'));
    shows(&mut screen, "Reword ·");
    for _ in 0.."bravo".len() {
        key(&mut screen, KeyCode::Backspace);
    }
    for c in "Bravo, reworded".chars() {
        key(&mut screen, KeyCode::Char(c));
    }
    confirm(&mut screen);
    assert_eq!(subjects(root), "delta charlie Bravo, reworded alpha", "{}", view(&mut screen));

    std::fs::write(root.join("alpha.txt"), "amended\n").unwrap();
    git(root, &["add", "alpha.txt"]);
    std::fs::write(root.join("delta.txt"), "left unstaged\n").unwrap();
    // The list keeps the reworded commit selected (third newest).
    shows(&mut screen, "Bravo, reworded");
    key(&mut screen, KeyCode::Char('A'));
    shows(&mut screen, "Amend with the staged changes");
    confirm(&mut screen);
    assert_eq!(subjects(root), "delta charlie Bravo, reworded alpha", "{}", view(&mut screen));
    assert_eq!(git(root, &["show", "HEAD~2:alpha.txt"]), "amended");
    assert_eq!(std::fs::read_to_string(root.join("delta.txt")).unwrap(), "left unstaged\n");
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn p_on_a_tag_pushes_only_that_tag() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(root, &["remote", "add", "origin", bare.path().to_str().unwrap()]);
    git(root, &["tag", "v1.0"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('3'));
    // History shows the tag on its commit first; wait for the Tags list itself.
    shows(&mut screen, "Tags 1");
    key(&mut screen, KeyCode::Char('L'));
    key(&mut screen, KeyCode::Char('L'));
    shows(&mut screen, "v1.0");
    key(&mut screen, KeyCode::Char('P'));
    let sheet = view(&mut screen);
    for text in ["Push tag v1.0?".to_owned(), "origin".to_owned(), button("Push tag")] {
        assert!(sheet.contains(&text), "{text}:\n{sheet}");
    }
    confirm(&mut screen);
    assert_eq!(
        git(bare.path(), &["for-each-ref", "--format=%(refname)"]),
        "refs/tags/v1.0"
    );
    shows(&mut screen, "Pushed tag v1.0 to origin");
}

#[test]
fn the_footer_and_help_offer_the_new_keys() {
    let dir = repo(&["alpha", "bravo"]);
    let mut screen = history(dir.path(), "alpha", 1);
    let footer = view(&mut screen).lines().last().unwrap().to_owned();
    for text in ["s squash", "d drop", "W reword"] {
        assert!(footer.contains(text), "{text}: {footer}");
    }
    key(&mut screen, KeyCode::Char('?'));
    let help = view(&mut screen);
    for text in ["Squash into the commit below", "Move down · up", "Pull (confirm)", "Undo · redo"] {
        assert!(help.contains(text), "{text}:\n{help}");
    }
}

#[test]
fn the_lazygit_preset_runs_the_new_operations_on_lazygits_keys() {
    let dir = repo(&["alpha", "bravo", "charlie"]);
    let root = dir.path();
    let mut config = load_bundled_config().config;
    config.ui.git.keys = GitKeys::Lazygit;
    let mut screen = GitScreen::open(root).unwrap();
    screen.configure_input(&config);
    shows(&mut screen, "main");
    key(&mut screen, KeyCode::Char('4'));
    shows(&mut screen, "alpha");
    key(&mut screen, KeyCode::Char('j'));
    // lazygit's Ctrl-j moves the commit down.
    ctrl(&mut screen, KeyCode::Char('j'));
    shows(&mut screen, "Move down");
    confirm(&mut screen);
    assert_eq!(subjects(root), "charlie alpha bravo", "{}", view(&mut screen));
    // `z` undoes the move, Ctrl-z redoes it.
    key(&mut screen, KeyCode::Char('z'));
    shows(&mut screen, &button("Undo"));
    confirm(&mut screen);
    assert_eq!(subjects(root), "charlie bravo alpha");
    ctrl(&mut screen, KeyCode::Char('z'));
    shows(&mut screen, &button("Redo"));
    confirm(&mut screen);
    assert_eq!(subjects(root), "charlie alpha bravo");
    // `p` pulls from Stashes too in lazygit's keys.
    key(&mut screen, KeyCode::Char('5'));
    key(&mut screen, KeyCode::Char('p'));
    shows(&mut screen, "main has no upstream");
}

// Round 15 of the Git tab's review (DESIGN.md §5.11).

#[test]
fn a_browsed_branch_lists_its_log_once_in_the_reader_and_history_gives_way() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    git(root, &["switch", "-q", "-c", "feature"]);
    commit(root, "Feature base");
    commit(root, "Feature tip");
    git(root, &["switch", "-q", "main"]);
    let mut screen = open(root);
    // The panes take their sizes once status and branches have both arrived.
    wait(&mut screen, |s| {
        let shown = view(s);
        shown.contains("Local 2") && shown.contains("Working tree clean")
    });
    let before = screen.frame(120, 36).grid;
    key(&mut screen, KeyCode::Char('3'));
    // The list has loaded once the reader shows the selected branch's log.
    shows(&mut screen, "Feature base");
    // Down and back up: History follows the cursor to `feature`.
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Up);
    shows(&mut screen, "History · feature");
    shows(&mut screen, "Its log is in the reader");
    let browsing = view(&mut screen);
    // The reader is the branch's one log; History keeps its title and count.
    assert_eq!(browsing.matches("Feature base").count(), 1, "{browsing}");
    assert_eq!(browsing.matches("Feature tip").count(), 1, "{browsing}");
    // The panes keep their rectangles: browsing folds nothing.
    let after = screen.frame(120, 36).grid;
    let corners = |grid: &dust::ui::CellGrid| {
        (0..36).filter(|&y| grid.row_text(y).starts_with('╭')).collect::<Vec<_>>()
    };
    assert_eq!(corners(&before), corners(&after));
    // History lists the log again when it takes focus.
    key(&mut screen, KeyCode::Char('4'));
    shows(&mut screen, "Feature base");
    assert!(!view(&mut screen).contains("Its log is in the reader"));
}

#[test]
fn the_changes_title_totals_the_working_tree_and_a_split_file_names_its_layer() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    std::fs::write(root.join("alpha.txt"), "staged\n").unwrap();
    git(root, &["add", "alpha.txt"]);
    std::fs::write(root.join("alpha.txt"), "staged\nand more\n").unwrap();
    std::fs::write(root.join("new.txt"), "one\ntwo\n").unwrap();
    let mut screen = open(root);
    // alpha.txt is +2 −1 against HEAD, new.txt +2.
    shows(&mut screen, "2 · +4 −1");
    // The reader's band is the unstaged layer's stat, and says so.
    shows(&mut screen, "unstaged +1");
}

#[test]
fn the_commit_view_names_its_hash_once_in_the_header() {
    let dir = repo(&["alpha", "bravo"]);
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut screen = open(dir.path());
    key(&mut screen, KeyCode::Char('4'));
    shows(&mut screen, "+ bravo");
    key(&mut screen, KeyCode::Enter);
    shows(&mut screen, "Commit files");
    let frame = view(&mut screen);
    // The History row and the reader's header block; neither title repeats it.
    assert_eq!(frame.matches(short(&head)).count(), 2, "{frame}");
    assert!(frame.lines().any(|line| line.contains("╭") && line.contains("bravo.txt")), "{frame}");
}

#[test]
fn y_copies_a_commit_attribute_and_o_needs_a_forge_remote() {
    let dir = repo(&["alpha", "bravo"]);
    let root = dir.path();
    let head = git(root, &["rev-parse", "HEAD"]);
    let mut screen = open(root);
    key(&mut screen, KeyCode::Char('4'));
    shows(&mut screen, "bravo");
    key(&mut screen, KeyCode::Char('o'));
    shows(&mut screen, "No remote to open the commit on");
    git(root, &["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    key(&mut screen, KeyCode::Char('y'));
    let menu = view(&mut screen);
    for text in [
        format!("Copy from {}", short(&head)),
        "Short SHA".to_owned(),
        format!("https://github.com/acme/widgets/commit/{}", short(&head)),
        button("Copy"),
    ] {
        assert!(menu.contains(&text), "{text}:\n{menu}");
    }
    let copied = |screen: &mut GitScreen| {
        screen.event(UiEvent::Key {
            key: KeyEvent { code: KeyCode::Enter, modifiers: Modifiers::default() },
            kind: KeyInputKind::Press,
        })
    };
    assert_eq!(copied(&mut screen), dust::git::screen::ScreenOutcome::Copy(head.clone()));
    shows(&mut screen, "Copied the SHA");
    key(&mut screen, KeyCode::Char('y'));
    key(&mut screen, KeyCode::Char('j'));
    assert_eq!(copied(&mut screen), dust::git::screen::ScreenOutcome::Copy(short(&head).to_owned()));
    key(&mut screen, KeyCode::Char('y'));
    key(&mut screen, KeyCode::Down);
    key(&mut screen, KeyCode::Down);
    assert_eq!(copied(&mut screen), dust::git::screen::ScreenOutcome::Copy("bravo".into()));
}

#[test]
fn the_commit_box_shows_its_visual_selection_and_verbs_and_sheets_clear_the_panes() {
    let dir = repo(&["alpha"]);
    let root = dir.path();
    std::fs::write(root.join("alpha.txt"), "changed\n").unwrap();
    git(root, &["add", "alpha.txt"]);
    let mut screen = GitScreen::open(root).unwrap();
    screen.configure_input(&load_bundled_config().config);
    shows(&mut screen, "alpha.txt");
    key(&mut screen, KeyCode::Char('c'));
    shows(&mut screen, "Commit to main");
    for c in "Explain the modes".chars() {
        key(&mut screen, KeyCode::Char(c));
    }
    key(&mut screen, KeyCode::Escape);
    for c in "0wve".chars() {
        key(&mut screen, KeyCode::Char(c));
    }
    let boxed = view(&mut screen);
    assert!(boxed.contains("3 selected"), "{boxed}");
    assert!(boxed.contains("y yank · d delete · c change"), "{boxed}");
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    // A centred sheet dims the panes behind it, and a blank cell keeps panel text off
    // its edge: no `Ta╭─`.
    let branches = |frame: &dust::ui::Frame| {
        (0..40u16)
            .find_map(|y| {
                let row = frame.grid.row_text(y);
                let at = row.find("Branches")?;
                Some((row[..at].chars().count() as u16, y))
            })
            .unwrap()
    };
    let plain = screen.frame(140, 40);
    let (x, y) = branches(&plain);
    key(&mut screen, KeyCode::Char(':'));
    let dimmed = screen.frame(140, 40);
    let sheet = view(&mut screen);
    assert!(sheet.contains("Command") && sheet.contains("Branches"), "{sheet}");
    assert_eq!(dimmed.grid.get(x, y).unwrap().symbol, plain.grid.get(x, y).unwrap().symbol);
    assert_ne!(dimmed.grid.get(x, y).unwrap().style, plain.grid.get(x, y).unwrap().style, "{sheet}");
    let title = sheet.lines().find(|row| row.contains("Command")).unwrap();
    let before: Vec<char> = title[..title.find("Command").unwrap()].chars().collect();
    let edge = before.iter().rposition(|&c| c == '╭').unwrap();
    assert!(edge == 0 || before[edge - 1] == ' ', "{title}");
}
