//! Dusty Git's vim modes, the editor's leader and the staging keys.
use dusty::config::{Config, GitKeys, load_bundled_config};
use dusty::editor::input::{KeyCode, KeyEvent, Modifiers};
use dusty::git::screen::{GitScreen, InputMode};
use dusty::ui::{KeyInputKind, UiEvent};
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
    git(dir.path(), &["config", "user.name", "Vim Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "Initial commit"]);
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    dir
}

fn config(keys: GitKeys) -> Config {
    let mut config = load_bundled_config().config;
    config.ui.git.keys = keys;
    config
}

fn open(root: &Path, keys: GitKeys) -> GitScreen {
    let mut screen = GitScreen::open(root).unwrap();
    screen.configure_input(&config(keys));
    wait(&mut screen, |s| view(s).contains("changed"));
    screen
}

fn press(screen: &mut GitScreen, code: KeyCode, modifiers: Modifiers) {
    screen.event(UiEvent::Key {
        key: KeyEvent { code, modifiers },
        kind: KeyInputKind::Press,
    });
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    press(screen, code, Modifiers::default());
}

/// Each character as its own key press, as typed.
fn keys(screen: &mut GitScreen, text: &str) {
    for c in text.chars() {
        key(screen, KeyCode::Char(c));
    }
}

fn ctrl_enter(screen: &mut GitScreen) {
    press(
        screen,
        KeyCode::Enter,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
}

fn rows(screen: &mut GitScreen) -> Vec<String> {
    let frame = screen.frame(120, 36);
    (0..36).map(|y| frame.grid.row_text(y)).collect()
}

fn view(screen: &mut GitScreen) -> String {
    rows(screen).join("\n")
}

fn footer(screen: &mut GitScreen) -> String {
    rows(screen).pop().unwrap()
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

fn idle(screen: &mut GitScreen) {
    wait(screen, |s| !s.busy());
}

fn staged(root: &Path) -> String {
    git(root, &["diff", "--cached", "--name-only"])
}

fn label(mode: &str) -> String {
    format!(
        " {} ",
        load_bundled_config().config.ui.status_line.mode_labels[mode]
    )
}

#[test]
fn the_mode_chip_follows_normal_visual_and_insert() {
    let dir = repo();
    // A screen nobody configured is not modal: no chip, nothing changes.
    let mut plain = GitScreen::open(dir.path()).unwrap();
    assert_eq!(plain.input_mode(), None);
    assert!(!footer(&mut plain).starts_with(&label("normal")));

    let mut screen = open(dir.path(), GitKeys::Dusty);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    let normal = footer(&mut screen);
    assert!(normal.starts_with(&label("normal")), "{normal}");
    // Every footer leaves the chip its columns: the hints start after it, whole, at every
    // width, and drop by priority instead of running under it.
    for (width, height) in [(64, 20), (100, 30), (190, 48)] {
        let frame = screen.frame(width, height);
        let row = frame.grid.row_text(height - 1);
        let rest = row
            .strip_prefix(&label("normal"))
            .unwrap_or_else(|| panic!("{width}: {row}"));
        assert!(
            rest.trim_start().starts_with("s stage"),
            "{width}×{height}: {row}"
        );
    }
    assert!(
        normal.contains("s stage") && normal.contains("u unstage"),
        "{normal}"
    );

    // `V` in the reader is the line range: VISUAL.
    key(&mut screen, KeyCode::Char('v'));
    wait(&mut screen, |s| {
        key(s, KeyCode::Char('V'));
        s.input_mode() == Some(InputMode::Visual)
    });
    assert!(footer(&mut screen).starts_with(&label("visual_char")));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));

    // A text box opens in INSERT; Esc is NORMAL with the box still open; Esc again closes.
    key(&mut screen, KeyCode::Char('s'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    key(&mut screen, KeyCode::Char('c'));
    assert_eq!(
        screen.input_mode(),
        Some(InputMode::Insert),
        "{}",
        view(&mut screen)
    );
    let insert = footer(&mut screen);
    assert!(insert.starts_with(&label("insert")), "{insert}");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    assert!(view(&mut screen).contains("Summarise the change"));
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Summarise the change"));
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
}

#[test]
fn a_text_box_edits_with_the_editors_vim_keys_and_keeps_its_draft() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char('s'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    key(&mut screen, KeyCode::Char('c'));
    keys(&mut screen, "hello world");
    assert!(view(&mut screen).contains("hello world"));
    // Normal mode: `b` back a word, `cw` changes it, Esc, `0` `x` deletes, `u` undoes.
    key(&mut screen, KeyCode::Escape);
    keys(&mut screen, "bcwthere");
    key(&mut screen, KeyCode::Escape);
    assert!(
        view(&mut screen).contains("hello there"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "0x");
    assert!(view(&mut screen).contains("ello there"));
    assert!(!view(&mut screen).contains("hello there"));
    keys(&mut screen, "u");
    assert!(view(&mut screen).contains("hello there"));
    // `$` and `ciw`, then `dd` and `u`.
    keys(&mut screen, "$ciwfolks");
    key(&mut screen, KeyCode::Escape);
    assert!(
        view(&mut screen).contains("hello folks"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "dd");
    assert!(!view(&mut screen).contains("hello folks"));
    keys(&mut screen, "u");
    assert!(view(&mut screen).contains("hello folks"));
    // A linewise put cannot add a line to a one-line field.
    keys(&mut screen, "yyp");
    assert!(view(&mut screen).contains("hello folks"));
    // `q` in Normal mode closes the box; the draft comes back with the next `c`.
    keys(&mut screen, "q");
    assert!(!view(&mut screen).contains("hello folks"));
    key(&mut screen, KeyCode::Char('c'));
    assert!(view(&mut screen).contains("hello folks"));
    assert_eq!(screen.input_mode(), Some(InputMode::Insert));
    // Ctrl+Enter submits from Normal mode too.
    key(&mut screen, KeyCode::Escape);
    ctrl_enter(&mut screen);
    idle(&mut screen);
    wait(&mut screen, |_| {
        git(dir.path(), &["log", "-1", "--format=%s"]) == "hello folks"
    });
}

#[test]
fn question_mark_opens_the_keys_sheet_from_a_normal_box_and_is_text_in_insert() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char('s'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    key(&mut screen, KeyCode::Char('c'));
    // Insert mode: `?` is text.
    keys(&mut screen, "why?");
    let typed = view(&mut screen);
    assert!(typed.contains("why?"), "{typed}");
    assert!(!typed.contains("Esc close"), "{typed}");
    // Normal mode: `?` opens the keys sheet the footer offers, and Esc closes it again.
    key(&mut screen, KeyCode::Escape);
    assert!(footer(&mut screen).contains("? keys"));
    key(&mut screen, KeyCode::Char('?'));
    let sheet = view(&mut screen);
    assert!(
        sheet.contains("Esc close") && sheet.contains("Vim"),
        "{sheet}"
    );
    key(&mut screen, KeyCode::Escape);
    let back = view(&mut screen);
    assert!(!back.contains("Esc close"), "{back}");
    assert!(back.contains("why?"), "{back}");
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
}

#[test]
fn the_leader_shows_which_key_toggles_the_rail_and_cancels_cleanly() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    assert!(view(&mut screen).contains("Stashes"));
    key(&mut screen, KeyCode::Char(' '));
    let pending = view(&mut screen);
    assert!(pending.contains("toggle sidebar"), "{pending}");
    assert!(pending.contains("Space"), "{pending}");
    // Space did not stage.
    assert_eq!(staged(dir.path()), "");
    key(&mut screen, KeyCode::Char('e'));
    let hidden = view(&mut screen);
    assert!(!hidden.contains("toggle sidebar"), "{hidden}");
    assert!(!hidden.contains("Stashes"), "{hidden}");
    // An unknown key or Esc after the leader cancels it, with no other effect.
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('x'));
    assert!(!view(&mut screen).contains("toggle sidebar"));
    assert!(!view(&mut screen).contains("Stashes"));
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Stashes"));
    key(&mut screen, KeyCode::Char(' '));
    key(&mut screen, KeyCode::Char('e'));
    assert!(view(&mut screen).contains("Stashes"));
    assert_eq!(staged(dir.path()), "");
}

#[test]
fn s_stages_u_unstages_and_minus_toggles() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char('s'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    wait(&mut screen, |s| {
        footer(s).contains("u unstage") && !s.busy()
    });
    // `s` on a staged file says so and changes nothing.
    key(&mut screen, KeyCode::Char('s'));
    idle(&mut screen);
    assert!(
        view(&mut screen).contains("already staged"),
        "{}",
        view(&mut screen)
    );
    assert_eq!(staged(dir.path()), "file.txt");
    key(&mut screen, KeyCode::Char('u'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()).is_empty());
    wait(&mut screen, |s| !s.busy() && view(s).contains("changed"));
    key(&mut screen, KeyCode::Char('u'));
    idle(&mut screen);
    assert!(
        view(&mut screen).contains("Nothing staged"),
        "{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Char('-'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    wait(&mut screen, |s| !s.busy() && view(s).contains("changed"));
    key(&mut screen, KeyCode::Char('-'));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()).is_empty());
}

#[test]
fn the_lazygit_preset_stages_with_space_and_leads_with_comma() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Lazygit);
    key(&mut screen, KeyCode::Char(' '));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()) == "file.txt");
    wait(&mut screen, |s| !s.busy() && view(s).contains("changed"));
    key(&mut screen, KeyCode::Char(','));
    let pending = view(&mut screen);
    assert!(pending.contains("toggle sidebar"), "{pending}");
    key(&mut screen, KeyCode::Char('e'));
    assert!(!view(&mut screen).contains("Stashes"));
    key(&mut screen, KeyCode::Char(','));
    key(&mut screen, KeyCode::Char('e'));
    assert!(view(&mut screen).contains("Stashes"));
    key(&mut screen, KeyCode::Char(' '));
    idle(&mut screen);
    wait(&mut screen, |_| staged(dir.path()).is_empty());
}

#[test]
fn the_lazygit_preset_shows_and_runs_lazygits_keys_in_each_panel() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Lazygit);
    let files = footer(&mut screen);
    assert!(
        files.contains("Space stage") && files.contains("Enter lines"),
        "{files}"
    );
    key(&mut screen, KeyCode::Char('?'));
    let sheet = view(&mut screen);
    assert!(
        sheet.contains("Stash all changes") && sheet.contains("Refresh"),
        "{sheet}"
    );
    key(&mut screen, KeyCode::Escape);
    // lazygit's `s` stashes; Space still stages.
    key(&mut screen, KeyCode::Char('s'));
    assert!(
        view(&mut screen).contains("Save stash"),
        "{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("Save stash"));
    assert_eq!(staged(dir.path()), "");
    key(&mut screen, KeyCode::Char('3'));
    let branches = footer(&mut screen);
    for hint in ["Space checkout", "M merge", "r rebase"] {
        assert!(branches.contains(hint), "{hint}: {branches}");
    }
    // `r` rebases here, as in lazygit: the sheet drops refresh and shows `R` for rename.
    key(&mut screen, KeyCode::Char('?'));
    let sheet = view(&mut screen);
    assert!(
        !sheet.contains("Refresh") && sheet.contains("Rename"),
        "{sheet}"
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('5'));
    let stash = footer(&mut screen);
    assert!(
        stash.contains("Space apply") && stash.contains("g pop"),
        "{stash}"
    );
}

#[test]
fn the_default_keys_keep_dusty_gits_footer_and_sheet() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    let files = footer(&mut screen);
    assert!(
        files.contains("s stage") && files.contains("v lines"),
        "{files}"
    );
    key(&mut screen, KeyCode::Char('?'));
    assert!(!view(&mut screen).contains("Stash all changes"));
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Char('3'));
    let branches = footer(&mut screen);
    assert!(
        branches.contains("m merge") && branches.contains("R rebase"),
        "{branches}"
    );
}

/// The view without its footer, where the filter's query and the notices show.
fn body(screen: &mut GitScreen) -> String {
    let mut rows = rows(screen);
    rows.pop();
    rows.join("\n")
}

#[test]
fn the_colon_prompt_edits_with_vim_keys_and_submits_from_either_mode() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char(':'));
    assert_eq!(screen.input_mode(), Some(InputMode::Insert));
    assert!(
        footer(&mut screen).starts_with(&label("insert")),
        "{}",
        footer(&mut screen)
    );
    keys(&mut screen, "hello world");
    assert!(view(&mut screen).contains("hello world"));
    // Esc: Normal mode with the prompt still open.
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    assert!(
        footer(&mut screen).starts_with(&label("normal")),
        "{}",
        footer(&mut screen)
    );
    assert!(view(&mut screen).contains("hello world"));
    // h l w b e 0 $ x dw cw ciw D u, and i a I A.
    keys(&mut screen, "0x");
    assert!(
        view(&mut screen).contains("ello world"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "u");
    assert!(view(&mut screen).contains("hello world"));
    keys(&mut screen, "0wdw");
    assert!(
        !view(&mut screen).contains("world"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "u0cwhowdy");
    key(&mut screen, KeyCode::Escape);
    assert!(
        view(&mut screen).contains("howdy world"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "$bciwfolks");
    key(&mut screen, KeyCode::Escape);
    assert!(
        view(&mut screen).contains("howdy folks"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "0eD");
    assert!(view(&mut screen).contains("howd"));
    assert!(!view(&mut screen).contains("howdy") && !view(&mut screen).contains("folks"));
    keys(&mut screen, "uA!");
    key(&mut screen, KeyCode::Escape);
    keys(&mut screen, "I>");
    key(&mut screen, KeyCode::Escape);
    keys(&mut screen, "lla-");
    key(&mut screen, KeyCode::Escape);
    keys(&mut screen, "0hli<");
    key(&mut screen, KeyCode::Escape);
    assert!(
        view(&mut screen).contains("><ho-wdy folks!"),
        "{}",
        view(&mut screen)
    );
    // A second Esc closes it.
    key(&mut screen, KeyCode::Escape);
    assert!(!view(&mut screen).contains("folks!"));
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    // Enter runs the command from Normal mode as from Insert.
    key(&mut screen, KeyCode::Char(':'));
    keys(&mut screen, "zebra quux");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    key(&mut screen, KeyCode::Enter);
    assert!(
        !body(&mut screen).contains("zebra quux"),
        "{}",
        view(&mut screen)
    );
    assert!(
        view(&mut screen).contains("Tutor:"),
        "{}",
        view(&mut screen)
    );
    key(&mut screen, KeyCode::Char(':'));
    keys(&mut screen, "tutor nonsense");
    key(&mut screen, KeyCode::Enter);
    assert!(
        !body(&mut screen).contains("tutor nonsense"),
        "{}",
        view(&mut screen)
    );
}

#[test]
fn the_list_filter_edits_with_vim_keys_and_the_chip_follows() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char('/'));
    assert_eq!(screen.input_mode(), Some(InputMode::Insert));
    let typing = footer(&mut screen);
    assert!(
        typing.starts_with(&label("insert")) && typing.contains("Esc normal"),
        "{typing}"
    );
    keys(&mut screen, "xfile");
    assert!(footer(&mut screen).contains("xfile"));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    let normal = footer(&mut screen);
    assert!(
        normal.starts_with(&label("normal")) && normal.contains("Esc clear"),
        "{normal}"
    );
    // Normal mode: the query edits and the list narrows as it does.
    keys(&mut screen, "0x");
    assert!(footer(&mut screen).contains("file") && !footer(&mut screen).contains("xfile"));
    assert!(
        body(&mut screen).contains("file.txt"),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "u");
    assert!(footer(&mut screen).contains("xfile"));
    keys(&mut screen, "0cwtxt");
    assert_eq!(screen.input_mode(), Some(InputMode::Insert));
    key(&mut screen, KeyCode::Escape);
    assert!(footer(&mut screen).contains("txt") && !footer(&mut screen).contains("xfile"));
    keys(&mut screen, "0D");
    assert!(!footer(&mut screen).contains("txt"));
    keys(&mut screen, "uIfile.");
    key(&mut screen, KeyCode::Escape);
    assert!(
        footer(&mut screen).contains("file.txt"),
        "{}",
        footer(&mut screen)
    );
    // Visual mode inside the query.
    keys(&mut screen, "0ve");
    assert_eq!(screen.input_mode(), Some(InputMode::Visual));
    assert!(footer(&mut screen).starts_with(&label("visual_char")));
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    // Enter keeps the filter from Normal mode; the list keys come back.
    key(&mut screen, KeyCode::Enter);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    assert!(
        footer(&mut screen).contains("s stage"),
        "{}",
        footer(&mut screen)
    );
    // `/` again types at the end; Esc, then Esc again clears and closes the filter.
    key(&mut screen, KeyCode::Char('/'));
    assert_eq!(screen.input_mode(), Some(InputMode::Insert));
    keys(&mut screen, "zz");
    assert!(
        footer(&mut screen).contains("file.txtzz"),
        "{}",
        footer(&mut screen)
    );
    key(&mut screen, KeyCode::Escape);
    key(&mut screen, KeyCode::Escape);
    assert!(!footer(&mut screen).contains("file.txtzz"));
    assert!(
        footer(&mut screen).contains("s stage"),
        "{}",
        footer(&mut screen)
    );
    // A space in the query is text, never the leader.
    key(&mut screen, KeyCode::Char('/'));
    keys(&mut screen, "a b");
    assert!(
        footer(&mut screen).contains("a b"),
        "{}",
        footer(&mut screen)
    );
    assert!(!view(&mut screen).contains("toggle sidebar"));
    key(&mut screen, KeyCode::Enter);
    // Not modal: the filter types as it always has, Esc clears at once.
    let mut plain = GitScreen::open(dir.path()).unwrap();
    wait(&mut plain, |s| view(s).contains("changed"));
    key(&mut plain, KeyCode::Char('/'));
    keys(&mut plain, "0x");
    assert!(footer(&mut plain).contains("0x"), "{}", footer(&mut plain));
    key(&mut plain, KeyCode::Escape);
    assert!(!footer(&mut plain).contains("0x"));
}

#[test]
fn the_find_box_edits_with_vim_keys_and_finds_from_normal_mode() {
    let dir = repo();
    let mut screen = open(dir.path(), GitKeys::Dusty);
    key(&mut screen, KeyCode::Char('v'));
    key(&mut screen, KeyCode::Char('/'));
    assert_eq!(
        screen.input_mode(),
        Some(InputMode::Insert),
        "{}",
        view(&mut screen)
    );
    keys(&mut screen, "chang");
    key(&mut screen, KeyCode::Escape);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    keys(&mut screen, "Aed");
    key(&mut screen, KeyCode::Escape);
    assert!(view(&mut screen).contains("changed"));
    key(&mut screen, KeyCode::Enter);
    assert_eq!(screen.input_mode(), Some(InputMode::Normal));
    let frame = screen.frame(190, 48);
    let footer = frame.grid.row_text(47);
    assert!(footer.contains("/changed/"), "{footer}");
}

#[test]
fn the_keys_sheet_lists_the_vim_modes_and_the_leader_from_the_key_tables() {
    let dir = repo();
    for (keys, leader, chord) in [
        (GitKeys::Dusty, "Space", "Space e"),
        (GitKeys::Lazygit, ",", ", e"),
    ] {
        let mut screen = open(dir.path(), keys);
        key(&mut screen, KeyCode::Char('?'));
        let frame = screen.frame(190, 48);
        let sheet = (0..48)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sheet.contains("Vim"), "{sheet}");
        for words in [
            format!("{} · type", label("insert").trim()),
            format!("{} · again closes", label("normal").trim()),
            format!("{} · select", label("visual_char").trim()),
            "Leader · which-key".into(),
            "toggle sidebar".into(),
        ] {
            assert!(sheet.contains(&words), "{words}:\n{sheet}");
        }
        // The keys are the editor's bindings and the shared table's chords.
        assert!(sheet.contains("i I a A"), "{sheet}");
        assert!(
            sheet
                .lines()
                .any(|row| row.contains(chord) && row.contains("toggle sidebar")),
            "{sheet}"
        );
        assert!(
            sheet
                .lines()
                .any(|row| row.contains(leader) && row.contains("Leader")),
            "{sheet}"
        );
        let staging = sheet
            .lines()
            .any(|row| row.contains("toggle: stage · unstage"));
        assert_eq!(staging, keys == GitKeys::Lazygit, "{sheet}");
    }
    // A screen that is not modal has no Vim group.
    let mut plain = GitScreen::open(dir.path()).unwrap();
    wait(&mut plain, |s| view(s).contains("changed"));
    key(&mut plain, KeyCode::Char('?'));
    let frame = plain.frame(190, 48);
    let sheet = (0..48)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!sheet.contains("Leader · which-key"), "{sheet}");
}

#[cfg(unix)]
#[path = "support/forge_fake.rs"]
mod forge_fake;

/// The inbox's unread badge sits in the top bar while the chip holds the footer's left: at
/// 100×30 neither covers the footer's keys, on the Notifications page or on the Git tab.
#[cfg(unix)]
#[test]
fn the_unread_badge_and_the_chip_leave_the_footer_keys_whole() {
    const CHILD: &str = "DUSTY_GIT_VIM_CHILD";
    let Some(repo) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state.json");
        std::fs::write(&state, r#"{"actions_seed":"design","viewer":"maya"}"#).unwrap();
        let server = forge_fake::ForgeServer::start(&state, "actions");
        let repo = directory.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/octo/orbit.git",
            ],
        );
        let home = directory.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let missing = directory.path().join("no-cli");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "the_unread_badge_and_the_chip_leave_the_footer_keys_whole",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ]);
        server.configure(&mut command);
        command
            .env(CHILD, &repo)
            .env("DUSTY_FORGE_TOKEN", forge_fake::FAKE_TOKEN)
            .env("DUSTY_FORGE_GH", &missing)
            .env("DUSTY_FORGE_GLAB", &missing)
            .env("DUSTY_FORGE_TEA", &missing)
            .env("DUSTY_FORGE_CACHE_DIR", directory.path().join("cache"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("DUSTY_CONFIG_HOME", directory.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    screen.configure_input(&config(GitKeys::Dusty));
    screen.open_target("notifications", None);
    let small = |screen: &mut GitScreen| {
        let frame = screen.frame(100, 30);
        (frame.grid.row_text(0), frame.grid.row_text(29))
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let (top, bottom) = small(&mut screen);
        if top.contains("Notifications 2") && bottom.contains("Enter open") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no unread badge:\n{top}\n{bottom}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (top, bottom) = small(&mut screen);
    let rest = bottom
        .strip_prefix(&label("normal"))
        .unwrap_or_else(|| panic!("{bottom}"));
    assert!(
        rest.trim_start().starts_with("Enter open"),
        "{top}\n{bottom}"
    );
    assert!(bottom.contains("? keys"), "{bottom}");
    // The Git tab, badge still up.
    key(&mut screen, KeyCode::Char('>'));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let (top, bottom) = small(&mut screen);
        if top.contains("Notifications 2") && bottom.contains("? keys") && !screen.busy() {
            let rest = bottom
                .strip_prefix(&label("normal"))
                .unwrap_or_else(|| panic!("{bottom}"));
            let first = rest.trim_start();
            // The first hint starts after the chip's gap and is whole: a key, then its word.
            assert!(
                rest.starts_with(' ') && first.split_whitespace().count() >= 2,
                "{top}\n{bottom}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "Git tab:\n{top}\n{bottom}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
