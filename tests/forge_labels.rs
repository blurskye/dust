#![cfg(unix)]
//! Labels never rename with width (DESIGN.md §4.3, r7 N-01/02/03): the top bar, the list's
//! section tabs and the reader's tabs read the same words at 100, 120 and 190 columns, on
//! every forge, in that forge's own terms. Under width pressure a tab loses its icon, then
//! the row's counts all at once (r8 R7-03: counts are on every tab that has one, or on
//! none); it never turns into `PRs`, `MRs`, `CI`, `Pipes`, `Feat`, `Misc`, `Info` or `Log`.
//! A list row's review state is one phrase at every width (r8 R7-02: `Review requested`,
//! never `Requested`); a row's age keeps its slot in one short form and its CI mark sits
//! on the title row at every width (r11); a row never shows `+0 −0` (r9 R8-11); and the keys sheet clears the screen behind it (r9 R8-10). The list's
//! section tabs keep their counts at 100 columns (r10 R9-01); the reader's tabs keep every
//! name and drop their counts all at once there, never showing a bare icon (r11). Each forge's world runs in its own child, as in `forge_wording`.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::editor::input::{KeyCode, KeyEvent};
use dust::git::screen::GitScreen;
use dust::ui::{KeyInputKind, UiEvent};
use forge_world::{Forge, World};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CHILD: &str = "DUSTY_FORGE_LABELS_CHILD";
const SIZES: [(u16, u16); 3] = [(100, 30), (120, 36), (190, 48)];

/// Every word a tab, section or reader tab may carry, in any forge's terms.
const VOCABULARY: [&str; 25] = [
    "Git",
    "Pull requests",
    "Merge requests",
    "Issues",
    "Actions",
    "Pipelines",
    "Notifications",
    "To-Do List",
    "Conversation",
    "Commits",
    "Checks",
    "Pipeline",
    "Files",
    "changed",
    "Merge",
    "Review",
    "Mine",
    "Drafts",
    "All",
    "Bugs",
    "Features",
    "Other",
    "Mentions",
    "Assigned",
    "Created",
];

/// Abbreviations an earlier squeeze used; none may appear as a whole word in the chrome.
const ABBREVIATIONS: [&str; 8] = ["PRs", "MRs", "CI", "Pipes", "Feat", "Misc", "Info", "Log"];

fn rerun_in_child(test: &str, world: &World) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("DUSTY_LABELS_REPO", &world.repo);
    world.configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child_repo() -> Option<PathBuf> {
    std::env::var_os(CHILD)?;
    Some(PathBuf::from(std::env::var_os("DUSTY_LABELS_REPO").unwrap()))
}

fn key(screen: &mut GitScreen, code: KeyCode) {
    screen.event(UiEvent::Key {
        key: KeyEvent::plain(code),
        kind: KeyInputKind::Press,
    });
}

fn rows(screen: &mut GitScreen, width: u16, height: u16) -> Vec<String> {
    let frame = screen.frame(width, height);
    (0..height).map(|y| frame.grid.row_text(y)).collect()
}

fn settle(screen: &mut GitScreen, millis: u64) {
    let quiet = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < quiet {
        screen.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for(screen: &mut GitScreen, what: &str, done: impl Fn(&str) -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        screen.poll();
        let frame = rows(screen, 190, 48).join("\n");
        if done(&frame) {
            break;
        }
        assert!(Instant::now() < end, "missing {what}:\n{frame}");
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(screen, 600);
}

/// Whether `word` stands alone in `text`: not part of a longer word (`Merge` in
/// `Merge requests` is its own tab only when no letter follows the space).
fn has_word(text: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let mut rest = text[end..].chars();
        let joins = matches!(after, Some(' ')) && rest.nth(1).is_some_and(char::is_lowercase);
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) && !joins {
            return true;
        }
        from = end;
    }
    false
}

fn words(text: &str) -> BTreeSet<&'static str> {
    VOCABULARY.iter().copied().filter(|word| has_word(text, word)).collect()
}

/// The count `word` carries in `text` (`Issues 10` → `10`), if it stands alone and has one.
fn count_of(text: &str, word: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        from = end;
        if !has_word(&text[start..], word) || text[..start].chars().next_back().is_some_and(char::is_alphanumeric) {
            continue;
        }
        let digits = text[end..].strip_prefix(' ').map(|rest| rest.chars().take_while(char::is_ascii_digit).collect::<String>());
        return digits.filter(|digits| !digits.is_empty());
    }
    None
}

/// The tabs of a row that carry a count. The Notifications tab's count is an unread badge
/// that stays on every screen (r9 R8-01), so it is not one of the row's totals.
fn counted(text: &str) -> BTreeSet<&'static str> {
    VOCABULARY
        .iter()
        .copied()
        .filter(|word| !matches!(*word, "Notifications" | "To-Do List"))
        .filter(|word| count_of(text, word).is_some())
        .collect()
}

/// The review phrases a list row may carry, and the short forms an earlier squeeze used.
const STATES: [&str; 6] = ["Review requested", "Changes requested", "Approved", "Commented", "Dismissed", "Pending"];

/// `#18` (or `!13`) → the review phrases on that row's meta line, per list row. GitHub's
/// row: the title row, then `#18 opened 2h ago by leo · ◌ Review requested`.
fn row_states(rows: &[String], sigil: &str) -> Vec<(String, BTreeSet<&'static str>, String, String)> {
    let left = |row: &String| row.find("││").map_or(row.clone(), |at| row[..at].to_owned());
    let mut found = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(3) {
        let meta = left(row);
        let trimmed = meta.trim_start_matches(|c: char| c == '│' || c == '▎' || c.is_whitespace());
        let Some(rest) = trimmed.strip_prefix(sigil) else { continue };
        let number = rest.chars().take_while(char::is_ascii_digit).collect::<String>();
        if number.is_empty() {
            continue;
        }
        let title = left(&rows[index - 1]);
        let phrases = STATES.iter().copied().filter(|state| meta.contains(state)).collect();
        found.push((number, phrases, meta, title));
    }
    found
}

/// A short review word on a list row: `Requested` or `Changes` not inside the full phrase.
fn short_state(meta: &str) -> Option<&'static str> {
    let bare = meta.replace("Review requested", "").replace("Changes requested", "");
    ["Requested", "Changes"].into_iter().find(|short| has_word(&bare, short))
}

/// Every list row's review phrase is the same at every width, and never a short form.
fn same_row_states(screen: &mut GitScreen, forge: Forge, sigil: &str, problems: &mut Vec<String>) {
    let shots = SIZES.iter().map(|&(width, height)| ((width, height), rows(screen, width, height))).collect::<Vec<_>>();
    let wide = row_states(&shots[2].1, sigil);
    if wide.iter().all(|(_, phrases, ..)| phrases.is_empty()) && forge != Forge::Gitea && forge != Forge::Forgejo {
        problems.push(format!("{}: no review state on any list row at 190:\n{}", forge.name(), shots[2].1.join("\n")));
    }
    for ((width, height), rows) in &shots {
        for (number, phrases, meta, title) in row_states(rows, sigil) {
            if let Some(short) = short_state(&meta) {
                problems.push(format!("{} {width}×{height} row {sigil}{number}: short state {short:?} in {}", forge.name(), meta.trim()));
            }
            // The age is GitHub's `opened 9m ago`, whole, or not on the row at all.
            if meta.contains(" opened ") && !meta.contains(" ago") && !meta.contains("just now") {
                problems.push(format!("{} {width}×{height} row {sigil}{number}: cut age in {}", forge.name(), meta.trim()));
            }
            if let Some((_, wide_phrases, wide_meta, wide_title)) = wide.iter().find(|(wide_number, ..)| *wide_number == number) {
                // The CI mark sits on the title row at every width (r11): a failing CI is
                // never dropped.
                if wide_title.contains('✕') && !title.contains('✕') {
                    problems.push(format!("{} {width}×{height} row {sigil}{number}: failing CI dropped in {}", forge.name(), title.trim()));
                }
                if &phrases != wide_phrases {
                    problems.push(format!(
                        "{} row {sigil}{number}: {width}×{height} reads {phrases:?}, 190×48 reads {wide_phrases:?}\n  {}\n  {}",
                        forge.name(),
                        meta.trim(),
                        wide_meta.trim()
                    ));
                }
            }
        }
    }
}

/// The keys sheet at every size: no cell of the screen behind survives inside the sheet's
/// rectangle and a one-cell margin, and no backdrop text is left beside it on its rows.
fn clean_sheet(screen: &mut GitScreen, forge: Forge, what: &str, problems: &mut Vec<String>) {
    for &(width, height) in &SIZES {
        let frame = screen.frame(width, height);
        let grid = &frame.grid;
        let symbol = |x: u16, y: u16| grid.get(x, y).map_or(" ".to_owned(), |cell| cell.symbol.to_string());
        let Some(top) = (1..height).find(|&y| grid.row_text(y).contains("Keys ·")) else {
            problems.push(format!("{} {what} {width}×{height}: no keys sheet", forge.name()));
            continue;
        };
        let Some(left) = (0..width).find(|&x| symbol(x, top) == "╭") else { continue };
        let Some(right) = (left + 1..width).find(|&x| symbol(x, top) == "╮") else { continue };
        let Some(bottom) = (top + 1..height).find(|&y| symbol(left, y) == "╰") else { continue };
        // r9 R8-10: every row between the top bar and the footer is blank beside the sheet:
        // no pane rule, title or tab separator from the screen behind.
        for y in 1..height - 1 {
            for x in 0..width {
                let inside = x >= left && x <= right && y >= top && y <= bottom;
                let cell = symbol(x, y);
                if !inside && cell != " " {
                    problems.push(format!(
                        "{} {what} {width}×{height}: `{cell}` at ({x},{y}) survives beside the sheet\n{}",
                        forge.name(),
                        (0..height).map(|y| grid.row_text(y)).collect::<Vec<_>>().join("\n")
                    ));
                    break;
                }
            }
        }
        // The caret, if any, is inside the sheet.
        if let Some((x, y, _)) = frame.cursor {
            if !(x > left && x < right && y > top && y < bottom) {
                problems.push(format!("{} {what} {width}×{height}: the caret at ({x},{y}) is outside the sheet", forge.name()));
            }
        }
    }
}

/// The chrome of one frame: the top bar, the list's section tab row and the reader's tab
/// row. The list and the reader share rows, split at their facing borders (`││`).
struct Chrome {
    top: String,
    sections: String,
    reader: String,
}

fn chrome(rows: &[String], section_marker: &str, reader_marker: Option<&str>) -> Chrome {
    let halves = |row: &String| -> (String, String) {
        match row.find("││") {
            Some(at) => (row[..at].to_owned(), row[at..].to_owned()),
            None => (row.clone(), String::new()),
        }
    };
    let sections = rows
        .iter()
        .skip(1)
        .take(4)
        .map(|row| halves(row).0)
        .find(|left| has_word(left, section_marker))
        .unwrap_or_default();
    let reader = reader_marker
        .and_then(|marker| {
            rows.iter().skip(1).map(|row| halves(row).1).find(|right| has_word(right, marker) && right.contains('│'))
        })
        .unwrap_or_default();
    Chrome {
        top: rows[0].clone(),
        sections,
        reader,
    }
}

/// One screen at the three sizes: every label set the same, no abbreviation anywhere.
fn same_labels(
    screen: &mut GitScreen,
    forge: Forge,
    what: &str,
    section_marker: &str,
    reader_marker: Option<&str>,
    problems: &mut Vec<String>,
) {
    let shots = SIZES
        .iter()
        .map(|&(width, height)| ((width, height), rows(screen, width, height)))
        .collect::<Vec<_>>();
    let parts = shots
        .iter()
        .map(|(size, rows)| (*size, chrome(rows, section_marker, reader_marker)))
        .collect::<Vec<_>>();
    type Pick = fn(&Chrome) -> &String;
    let picks: [(&str, Pick); 3] = [
        ("top bar", |chrome| &chrome.top),
        ("section tabs", |chrome| &chrome.sections),
        ("reader tabs", |chrome| &chrome.reader),
    ];
    for (row, pick) in picks {
        if row == "reader tabs" && reader_marker.is_none() {
            continue;
        }
        let wide = words(pick(&parts[2].1));
        // The Files tab takes the whole screen, so the list's sections go at every width;
        // the comparison below still holds them to one answer.
        if wide.is_empty() && what.ends_with(" 0") {
            problems.push(format!("{} {what}: no {row} at 190 columns:\n{}", forge.name(), shots[2].1.join("\n")));
            continue;
        }
        for ((width, height), chrome) in &parts[..2] {
            let narrow = words(pick(chrome));
            if narrow != wide {
                problems.push(format!(
                    "{} {what} {row}: {width}×{height} reads {narrow:?}, 190×48 reads {wide:?}\n  {}\n  {}",
                    forge.name(),
                    pick(chrome).trim(),
                    pick(&parts[2].1).trim()
                ));
            }
        }
    }
    // Counts are on every tab that has one, or on none (r8 R7-03). The list's section tabs
    // keep theirs at 100 and 120 (r10 R9-01). The reader keeps its counts at 120; at 100
    // its six names fit and the counts may drop, all at once, since no tab is ever a bare
    // icon (r11). The top bar may drop its counts, all at once.
    for (row, pick) in picks {
        let wide = counted(pick(&parts[2].1));
        for ((width, height), chrome) in &parts[..2] {
            let narrow = counted(pick(chrome));
            let strict = row == "section tabs" || row == "reader tabs" && *width > 100;
            if (strict || !narrow.is_empty()) && narrow != wide {
                problems.push(format!(
                    "{} {what} {row}: {width}×{height} counts {narrow:?}, 190×48 counts {wide:?}\n  {}\n  {}",
                    forge.name(),
                    pick(chrome).trim(),
                    pick(&parts[2].1).trim()
                ));
            }
        }
    }
    for ((width, height), chrome) in &parts {
        for row in [&chrome.top, &chrome.sections, &chrome.reader] {
            // A zero count is no count (r9 R8-02): `Other`, never `Other 0`.
            for word in VOCABULARY {
                if count_of(row, word).as_deref() == Some("0") {
                    problems.push(format!("{} {what} {width}×{height}: `{word} 0` in {}", forge.name(), row.trim()));
                }
            }
            for short in ABBREVIATIONS {
                if has_word(row, short) {
                    problems.push(format!("{} {what} {width}×{height}: {short:?} in {}", forge.name(), row.trim()));
                }
            }
        }
    }
}

fn labels(forge: Forge, test: &str) {
    let Some(repo) = child_repo() else {
        let world = World::new(forge);
        rerun_in_child(test, &world);
        return;
    };
    let mut screen = GitScreen::open(&repo).unwrap();
    let mut problems = Vec::new();
    let sigil = if forge == Forge::GitLab { "!" } else { "#" };
    let pull = format!("{sigil}{}", forge.pull());
    screen.open_target(if forge == Forge::GitLab { "mrs" } else { "prs" }, Some(forge.pull()));
    wait_for(&mut screen, &pull, |frame| frame.contains(&pull) && frame.contains("Commits") && !frame.contains("Loading"));
    same_row_states(&mut screen, forge, sigil, &mut problems);
    // Every reader tab of the pull request (GitHub's four, §3.26).
    for tab in 0..4 {
        settle(&mut screen, 400);
        same_labels(&mut screen, forge, &format!("pull request tab {tab}"), "Mine", Some("Commits"), &mut problems);
        key(&mut screen, KeyCode::Char(']'));
    }
    // The Review section once read: the row still reads the same at every width. Its
    // counts are on every section or none, whichever section was visited (r11).
    key(&mut screen, KeyCode::Char('2'));
    key(&mut screen, KeyCode::Char(']'));
    settle(&mut screen, 1500);
    same_labels(&mut screen, forge, "review section", "Mine", Some("Commits"), &mut problems);
    key(&mut screen, KeyCode::Char('3'));
    key(&mut screen, KeyCode::Char('?'));
    settle(&mut screen, 300);
    clean_sheet(&mut screen, forge, "pull request keys", &mut problems);
    key(&mut screen, KeyCode::Escape);
    let issue = format!("#{}", forge.issue());
    screen.open_target("issues", Some(forge.issue()));
    wait_for(&mut screen, &issue, |frame| frame.contains(&issue) && !frame.contains("Loading"));
    // An issue is one Conversation page with no reader tabs (§3.33).
    settle(&mut screen, 400);
    same_labels(&mut screen, forge, "issue", "All", None, &mut problems);
    key(&mut screen, KeyCode::Char('?'));
    settle(&mut screen, 300);
    clean_sheet(&mut screen, forge, "issue keys", &mut problems);
    key(&mut screen, KeyCode::Escape);
    // The notifications list: its sections and the top bar, whatever the world holds.
    screen.open_target("notifications", None);
    settle(&mut screen, 1500);
    same_labels(&mut screen, forge, "notifications", "All", None, &mut problems);
    assert!(problems.is_empty(), "labels change with width:\n{}", problems.join("\n\n"));
}

#[test]
fn github_labels_hold_at_every_width() {
    labels(Forge::GitHub, "github_labels_hold_at_every_width");
}

#[test]
fn gitlab_labels_hold_at_every_width() {
    labels(Forge::GitLab, "gitlab_labels_hold_at_every_width");
}

#[test]
fn gitea_labels_hold_at_every_width() {
    labels(Forge::Gitea, "gitea_labels_hold_at_every_width");
}

#[test]
fn forgejo_labels_hold_at_every_width() {
    labels(Forge::Forgejo, "forgejo_labels_hold_at_every_width");
}
