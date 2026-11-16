//! X-HELP: the keys of the focused pane, grouped, in columns.
use super::super::GitScreen;
use super::chrome::{self, bold, st};
use super::glyphs::{Glyph, glyph};
use super::layout::Rect;
use crate::ui::{Frame, GitRole, GitSurface};

const SURFACE: GitSurface = GitSurface::Sheet;
/// The help border and its title row.
const EDGE: GitSurface = GitSurface::Pane;

type Group = (&'static str, Vec<(&'static str, &'static str)>);
/// The rows every Git pane ends its Act group with (lazygit's `Pull` extra among them).
const SYNC: [&str; 3] = ["Pull (confirm)", "Push (confirm)", "Undo · redo (confirm)"];
/// A group as the sheet paints it: the Vim group's keys are read from the key tables.
type Sheet = (&'static str, Vec<(String, String)>);

/// The Vim group of a modal screen, read from the tables that bind the keys: the modes and
/// the keys that enter them from the editor's bundled bindings (the ones a text box runs),
/// the mode names from the editor's statusline settings, and the leader and its chords from
/// the shared key table (`github/keymap.rs`).
fn vim(app: &GitScreen) -> Option<Sheet> {
    use super::super::github::keymap::TABLE;
    use super::super::textbox::keys_for;
    if !app.modal {
        return None;
    }
    let [normal, insert, visual] = &app.mode_labels;
    let joined = |mode: &str, commands: &[&str]| {
        commands
            .iter()
            .flat_map(|command| keys_for(mode, command))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut rows = Vec::new();
    let enter = joined("normal", &["mode.insert"]);
    if !enter.is_empty() {
        rows.push((enter, format!("{insert} · type")));
    }
    let leave = joined("insert", &["input.cancel"]);
    if !leave.is_empty() {
        rows.push((leave, format!("{normal} · again closes")));
    }
    let select = joined(
        "normal",
        &[
            "mode.visual_char",
            "mode.visual_line",
            "mode.visual_block",
            "mode.visual_reselect",
        ],
    );
    if !select.is_empty() {
        rows.push((select, format!("{visual} · select")));
    }
    let keys = app.github.key_table();
    rows.push((keys.leader_name(), "Leader · which-key".into()));
    for (named, ..) in TABLE {
        if let Some(chord) = keys.keys(*named) {
            rows.push((chord, super::super::modal::chord_words(named.label())));
        }
    }
    Some(("Vim", rows))
}

/// The Git tab's rows in the keys in force (`github/keymap.rs`): the lazygit preset shows
/// its own key for each action, drops what it leaves out of this panel, and adds the keys
/// that reach an action Dusty Git keeps elsewhere.
fn in_force(app: &GitScreen, groups: &mut [Sheet]) {
    let keys = app.github.key_table();
    let panel = app.key_panel();
    // The leader's chord row is already in the keys in force.
    let chord = keys.keys(super::super::github::keymap::Named::ToggleSidebar);
    for (title, rows) in groups.iter_mut() {
        let operation = *title == "Operation";
        *rows = std::mem::take(rows)
            .into_iter()
            .filter_map(|(key, what)| {
                // The conflict keys' own lazygit keys: Space takes a side, `m` the menu.
                if operation && matches!(key.as_str(), "Enter" | "M" | "b" | "e") {
                    return Some((super::super::conflicts::operation_key(app, &key), what));
                }
                if chord.as_deref() == Some(key.as_str()) {
                    return Some((key, what));
                }
                Some((keys.show(panel, &key)?, what))
            })
            .collect();
    }
    if let Some((_, rows)) = groups.first_mut() {
        rows.extend(
            keys.extras(panel)
                .into_iter()
                .map(|(key, what)| (key, what.to_owned())),
        );
    }
}

fn owned(groups: Vec<Group>) -> Vec<Sheet> {
    groups
        .into_iter()
        .map(|(title, rows)| {
            (
                title,
                rows.into_iter()
                    .map(|(key, what)| (key.to_owned(), what.to_owned()))
                    .collect(),
            )
        })
        .collect()
}

/// The Pull requests and Issues tabs.
fn forge(app: &GitScreen) -> (&'static str, Vec<Group>) {
    let pull = app.page == 2;
    let mut navigate: Group = (
        "Navigate",
        vec![
            ("2 · 3", "List · reader"),
            ("Tab", "Reader · next link"),
            ("[ ]", "Sections · reader tabs"),
            ("< >", "Previous · next top tab"),
            ("j k", "Move"),
            ("Enter", "Open · follow link"),
            ("Esc", "Back to the list"),
            ("C-f", "Find in the reader"),
            ("C-n", "Load more"),
        ],
    );
    // The shared key table's chord (`github/keymap.rs`): the list hides beside the reader.
    let sidebar = match app
        .github
        .key_table()
        .keys(super::super::github::keymap::Named::ToggleSidebar)
        .as_deref()
    {
        Some("Space e") => "Space e",
        Some(", e") => ", e",
        Some(_) => "Leader e",
        None => "",
    };
    if !sidebar.is_empty() {
        navigate.1.push((sidebar, "Hide · show the list"));
    }
    // The accounts popover (who is signed in, how to sign in or switch); signed out, the
    // `Read-only` group below names it instead.
    if !app.github.read_only() {
        navigate.1.push(("@", "Accounts · how to sign in"));
    }
    let filter: Group = (
        "Filter",
        vec![
            ("/", "Filter query"),
            ("s", "Open · Closed"),
            ("F", "Author · Label · Sort menus"),
            ("S · D", "Save · delete a section"),
            ("v", "Saved views"),
            ("i", "Cross-repository inbox"),
            ("R", "Refresh"),
            ("o", "Open in the browser"),
        ],
    );
    let act: Group = if pull {
        (
            "Pull request",
            vec![
                ("r", "Review"),
                ("c", "Check out · reply"),
                ("w", "Review in a worktree"),
                ("m", "Merge readiness"),
                ("d", "Files"),
                ("C", "Comment"),
                ("e", "Edit title and description"),
                ("x · X", "Close · reopen"),
                ("l · a", "Labels · assignees"),
                ("n", "Create"),
            ],
        )
    } else {
        (
            "Issue",
            vec![
                ("C", "Comment"),
                ("e", "Edit title and description"),
                ("x · X", "Close (with a reason) · reopen"),
                ("l", "Labels"),
                ("a", "Assignees"),
                ("m", "Milestone"),
                ("n", "New issue"),
            ],
        )
    };
    let mut act = act;
    let mut groups = Vec::new();
    if app.github.read_only() {
        // Signed out: only the keys that read stay; the rest are named once, with the way in.
        const READS: [&str; 5] = ["c", "w", "m", "d", "o"];
        act.1
            .retain(|(key, _)| READS.contains(key) && (pull || *key != "m"));
        groups.push((
            "Read-only",
            vec![
                ("@", "Accounts · how to sign in"),
                ("", "Unavailable actions say why"),
            ],
        ));
    }
    let mut all = vec![navigate, act, filter];
    if pull {
        // The Files tab's own keys, as its footer offers them (r12 prs-06).
        all.push((
            "Files changed tab",
            vec![
                ("Enter", "Comment on the line"),
                ("V", "Comment on a range"),
                ("w", "Wrap · pan long lines"),
                ("f", "Choose a file"),
                ("v", "Mark the file viewed"),
                ("e", "Open in the editor"),
            ],
        ));
        // The mark at a list row's end is its checks (r12 prs-05).
        all.push((
            "List marks",
            vec![
                (glyph(Glyph::CheckPassed, app.icon_mode), "Checks passed"),
                (glyph(Glyph::CheckFailed, app.icon_mode), "Checks failed"),
                (glyph(Glyph::CheckPending, app.icon_mode), "Checks running"),
            ],
        ));
    }
    all.extend(groups);
    (if pull { "Pull requests" } else { "Issues" }, all)
}

/// Actions (GitLab: Pipelines).
fn actions(app: &GitScreen) -> (&'static str, Vec<Group>) {
    // The shared key table's chord (`github/keymap.rs`), in the leader the session uses.
    let sidebar = match app
        .github
        .key_table()
        .keys(super::super::github::keymap::Named::ToggleSidebar)
        .as_deref()
    {
        Some("Space e") => "Space e",
        Some(", e") => ", e",
        Some(_) => "Leader e",
        None => "",
    };
    let mut navigate: Group = (
        "Navigate",
        vec![
            ("j k", "Move"),
            ("1 2 3 · Tab", "Workflows · runs · run"),
            ("h l", "Back · into the run"),
            ("Enter", "Open · fold the job or step"),
            ("Esc", "Back to the run history"),
            ("[ ]", "All · Failing · Running · Mine"),
            ("/", "Filter runs · find in the logs"),
            ("F", "Narrow by workflow · branch · event"),
            ("< >", "Previous · next top tab"),
            ("R", "Refresh"),
        ],
    );
    if !sidebar.is_empty() {
        navigate.1.push((sidebar, "Hide · show the left column"));
    }
    navigate.1.push(("@", "Accounts · how to sign in"));
    let log: Group = (
        "Log",
        vec![
            ("f", "Full job log · steps"),
            ("z", "Zoom the run · Esc returns"),
            ("n N", "Next · previous error or match"),
            ("a", "Attempts"),
            ("w", "Long lines wrap · run past the edge"),
            ("H L", "Pan 8 columns, unwrapped"),
            ("Enter", "Open file:line"),
            ("C-s", "Save the log"),
        ],
    );
    let act: Group = (
        "Act",
        vec![
            ("r", "Re-run jobs"),
            ("x", "Cancel"),
            ("d", "Run workflow"),
            ("A", "Approve a waiting run"),
            ("Enter · s", "An artifact's files · download it"),
        ],
    );
    (app.github.terms().actions_tab, vec![navigate, log, act])
}

fn context(app: &GitScreen) -> (&'static str, Vec<Group>) {
    if matches!(app.page, 1 | 2) {
        return forge(app);
    }
    if app.page == 3 {
        return actions(app);
    }
    let navigate: Group = (
        "Navigate",
        vec![
            ("2 3 4 5", "Changes · Branches · History · Stashes"),
            ("0", "Reader"),
            ("Tab", "Next pane"),
            ("C-h/j/k/l", "Move focus"),
            ("[ ]", "Sub-tabs of this pane"),
            ("< >", "Pull requests · Issues · Actions"),
            ("j k", "Move"),
            ("Home End", "First · last"),
            ("PgUp PgDn", "Scroll"),
            ("/", "Filter list · find in reader"),
            ("n N", "Next · previous match"),
            ("Esc", "Back"),
        ],
    );
    // The shared key table's chord (`github/keymap.rs`), in the leader the session uses.
    let rail = match app
        .github
        .key_table()
        .keys(super::super::github::keymap::Named::ToggleSidebar)
        .as_deref()
    {
        Some("Space e") => "Space e",
        Some(", e") => ", e",
        Some(_) => "Leader e",
        None => "",
    };
    let mut view: Group = (
        "View",
        vec![
            ("|", "Split · unified diff"),
            // Refresh ranks high: a compact sheet that runs out of rows keeps it.
            ("r", "Refresh"),
            ("h l", "Fold tree · pan diff"),
            ("m", "Full commit message"),
            (":tutor git", "Practice in a disposable repo"),
            (":tutor forge", "Practice the forge tabs"),
        ],
    );
    let operation: Option<Group> = app.operation_bar.is_some().then(|| {
        (
            "Operation",
            vec![
                ("C", "Continue"),
                ("S", "Skip commit"),
                ("X", "Abort"),
                ("o t", "Take ours · theirs"),
                // The reader on a conflicted file (`conflicts.rs`): per hunk.
                ("b", "Take both"),
                ("Enter", "Take the side under the cursor"),
                ("e", "Edit at the conflict"),
                ("M", "Continue · skip · abort menu"),
            ],
        )
    });
    let (title, act): (&str, Group) =
        if app.commit_open || (app.focus == 0 && app.last_list == 2 && app.commit.is_some()) {
            (
                "Commit files",
                (
                    "Act",
                    vec![
                        ("Enter", "Open the file's diff"),
                        ("J K", "Next · previous file"),
                        ("y", "Copy SHA · short SHA · subject · URL"),
                        ("o", "Open the commit on the forge"),
                        ("e", "Open the line in the editor"),
                        ("/", "Filter files"),
                        ("Esc", "Back to History"),
                    ],
                ),
            )
        } else {
            match app.focus {
                4 => (
                    "Reader",
                    (
                        "Act",
                        vec![
                            ("s u -", "Stage · unstage hunk · toggle"),
                            ("a", "Stage · unstage line or range"),
                            ("V", "Start · finish a range"),
                            ("d", "Discard hunk (confirm)"),
                            ("D", "Discard line (confirm)"),
                            ("c", "Commit staged"),
                            ("e", "Open the line in the editor"),
                        ],
                    ),
                ),
                0 if app.files_view == 1 => (
                    "Worktrees",
                    (
                        "Act",
                        vec![("n", "New worktree"), ("d", "Remove a clean worktree")],
                    ),
                ),
                0 if app.files_view == 2 => (
                    "Submodules",
                    (
                        "Act",
                        vec![("- s", "Toggle pointer · stage it"), ("u", "Init · update")],
                    ),
                ),
                0 => (
                    "Changes",
                    (
                        "Act",
                        vec![
                            ("s u -", "Stage · unstage · toggle"),
                            ("a", "Stage all · unstage all"),
                            ("x", "Mark file"),
                            ("V", "Mark a range"),
                            ("d", "Discard (confirm)"),
                            ("D", "Discard marked (confirm)"),
                            ("c", "Commit staged"),
                            ("A W F", "Amend · reword · fixup"),
                            ("t", "Flat · tree"),
                            ("F", "Changed · all files"),
                            ("v", "Open in reader"),
                        ],
                    ),
                ),
                1 if app.subview == 2 => (
                    "Remotes",
                    (
                        "Act",
                        vec![
                            ("f", "Fetch (confirm)"),
                            ("Enter", "Track as a local branch"),
                        ],
                    ),
                ),
                1 if app.subview == 3 => (
                    "Tags",
                    (
                        "Act",
                        vec![
                            ("Enter", "Inspect tag"),
                            ("d", "Delete local tag"),
                            ("P", "Push tag (confirm)"),
                        ],
                    ),
                ),
                1 => (
                    "Branches",
                    (
                        "Act",
                        vec![
                            ("Enter", "Checkout"),
                            ("n", "New branch"),
                            ("m", "Merge into current"),
                            ("R", "Rebase current onto"),
                            (
                                "o",
                                if app.github.terms().pr_singular.contains("merge") {
                                    "Its merge request, or open one"
                                } else {
                                    "Its pull request, or open one"
                                },
                            ),
                            ("e", "Rename"),
                            ("u", "Set upstream"),
                            ("d", "Delete (confirm)"),
                        ],
                    ),
                ),
                2 if app.history_view == 1 => (
                    "Reflog",
                    (
                        "Act",
                        vec![
                            ("n", "Rescue branch here"),
                            ("o", "Checkout detached"),
                            ("w", "Detached worktree"),
                            ("Enter", "Browse files"),
                        ],
                    ),
                ),
                2 => (
                    "History",
                    (
                        "Act",
                        vec![
                            ("Enter", "Commit files"),
                            ("A", "Amend with staged (confirm)"),
                            ("W", "Reword"),
                            ("s", "Squash into the commit below"),
                            ("f", "Fixup into the commit below"),
                            ("d", "Drop (confirm)"),
                            ("e", "Edit: stop the rebase here"),
                            ("J K", "Move down · up"),
                            ("F", "Commit staged as fixup!"),
                            ("y", "Copy SHA · short SHA · subject · URL"),
                            ("o", "Open the commit on the forge"),
                        ],
                    ),
                ),
                _ => (
                    "Stashes",
                    (
                        "Act",
                        vec![
                            ("n", "New stash"),
                            ("a", "Apply"),
                            ("p", "Pop"),
                            ("d", "Drop (confirm)"),
                        ],
                    ),
                ),
            }
        };
    // A modal screen lists the shared chords in its Vim group.
    if !rail.is_empty() && !app.modal {
        view.1.insert(1, (rail, "Hide · show the rail"));
    }
    // Pull, push, undo and redo work from every pane; `p` pops in
    // Stashes and `P` pushes the selected tag in Tags.
    let stashes = app.focus == 3 && !app.commit_open;
    let tags = app.focus == 1 && app.subview == 3;
    // They close the pane's own Act rows rather than open a group of their own: a heading
    // row would push View off the compact sheet.
    let mut act = act;
    if !stashes {
        act.1.push(("p", SYNC[0]));
    }
    if !tags {
        act.1.push(("P", SYNC[1]));
    }
    act.1.push(("z Z", SYNC[2]));
    let mut groups = vec![act];
    groups.extend(operation);
    groups.push(navigate);
    groups.push(view);
    (title, groups)
}

/// The sheet border with its title and `Esc close`; returns the right and bottom edges.
fn border(app: &GitScreen, frame: &mut Frame, rect: Rect, title: &str) -> (u16, u16) {
    // Everything behind the sheet recedes, so no backdrop text reads as part of it.
    chrome::scrim(&mut frame.grid, rect);
    // The border sits on the pane colour and the sheet fills only inside it, so the box
    // line is the overlay's edge: no half cell of backdrop shows past it.
    chrome::fill(&mut frame.grid, rect, st(GitRole::Text, EDGE));
    if rect.w > 2 && rect.h > 2 {
        chrome::fill(
            &mut frame.grid,
            Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2),
            st(GitRole::Text, SURFACE),
        );
    }
    let edge = st(GitRole::Focus, EDGE);
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    for x in rect.x + 1..right {
        frame.grid.set_cell(x, rect.y, "─", edge);
        frame.grid.set_cell(x, bottom, "─", edge);
    }
    for y in rect.y + 1..bottom {
        frame.grid.set_cell(rect.x, y, "│", edge);
        frame.grid.set_cell(right, y, "│", edge);
    }
    frame.grid.set_cell(rect.x, rect.y, "╭", edge);
    frame.grid.set_cell(right, rect.y, "╮", edge);
    frame.grid.set_cell(rect.x, bottom, "╰", edge);
    frame.grid.set_cell(right, bottom, "╯", edge);
    let close = " Esc close ";
    let close_at = right.saturating_sub(chrome::width(close) + 1);
    chrome::put_spans(
        &mut frame.grid,
        rect.x + 2,
        rect.y,
        &[
            (" ".into(), edge),
            (
                format!("{} ", glyph(Glyph::Search, app.icon_mode)),
                st(GitRole::Focus, EDGE),
            ),
            ("Keys".into(), bold(GitRole::Text, EDGE)),
            (format!(" · {title} "), st(GitRole::Muted, EDGE)),
        ],
        close_at,
    );
    frame
        .grid
        .put_text_clipped(close_at, rect.y, close, st(GitRole::Muted, EDGE), right);
    (right, bottom)
}

/// The terminal caret moves into the sheet, on its first heading (r9 R8-10): left where
/// it was, it blinks alone on the cleared screen behind.
fn caret(frame: &mut Frame, x: u16, y: u16) {
    if let Some((_, _, shape)) = frame.cursor {
        frame.cursor = Some((x, y, shape));
    }
}

/// From this many columns up the sheet is two columns at [`WIDE_SHEET`] cells, centred:
/// a bigger terminal widens the margins, never shrinks the sheet (r16 git-tab-05).
const WIDE_FROM: u16 = 120;
/// The two-column sheet's width: two 53-cell columns, a gap, borders.
const WIDE_SHEET: u16 = 112;

/// Small terminals: every group stays, its keys flowed two or three to a row,
/// each `key  what` unaligned so long keys do not starve short labels. The sheet
/// grows over the top bar and footer before anything is cut. `wide` (≥ [`WIDE_FROM`]
/// columns) fixes two cells per row in a centred [`WIDE_SHEET`]-cell sheet.
#[allow(clippy::too_many_arguments)]
fn compact(
    app: &GitScreen,
    frame: &mut Frame,
    width: u16,
    height: u16,
    body: Rect,
    title: &str,
    groups: &[Sheet],
    wide: bool,
) {
    let cell = |key: &str, what: &str| chrome::width(key) + 2 + chrome::width(what);
    let sheet_w = if wide { WIDE_SHEET.min(body.w) } else { body.w };
    let body = Rect::new(body.x + (body.w - sheet_w) / 2, body.y, sheet_w, body.h);
    let inner_w = body.w.saturating_sub(4);
    // As many cells per row as keep most labels whole.
    let mut per_row = if wide { 2 } else { 1u16 };
    for n in (1..=3u16).rev().filter(|_| !wide) {
        let cell_w = inner_w.saturating_sub(2 * (n - 1)) / n;
        let whole = groups
            .iter()
            .flat_map(|group| group.1.iter())
            .filter(|(key, what)| cell(key, what) <= cell_w)
            .count();
        let total = groups.iter().map(|group| group.1.len()).sum::<usize>();
        if cell_w >= 24 && whole * 10 >= total * 8 {
            per_row = n;
            break;
        }
    }
    let cell_w = inner_w.saturating_sub(2 * (per_row - 1)) / per_row;
    let rows_of = |group: &Sheet| 1 + (group.1.len() as u16).div_ceil(per_row);
    let content: u16 = groups.iter().map(rows_of).sum();
    let gaps = groups.len().saturating_sub(1) as u16;
    // Borders plus content; gaps between groups only when there is room.
    let (rect, gap) = if content + gaps + 3 <= body.h {
        let h = content + gaps + 3;
        (
            Rect::new(body.x, body.y + (body.h - h) / 2, body.w, h),
            true,
        )
    } else if content + 2 <= body.h {
        (Rect::new(body.x, body.y, body.w, body.h), false)
    } else if wide {
        (Rect::new(body.x, 0, body.w, height), false)
    } else {
        (Rect::new(0, 0, width, height), false)
    };
    let (right, bottom) = border(app, frame, rect, title);
    let mut y = rect.y + 1 + u16::from(gap);
    caret(frame, rect.x + 2, y);
    for group in groups {
        if y >= bottom {
            break;
        }
        frame
            .grid
            .put_text_clipped(rect.x + 2, y, group.0, bold(GitRole::Focus, SURFACE), right);
        y += 1;
        for row in group.1.chunks(usize::from(per_row)) {
            if y >= bottom {
                break;
            }
            for (index, (key, what)) in row.iter().enumerate() {
                let x = rect.x + 2 + index as u16 * (cell_w + 2);
                let limit = (x + cell_w).min(right);
                let at =
                    frame
                        .grid
                        .put_text_clipped(x, y, key, bold(GitRole::Text, SURFACE), limit);
                frame.grid.put_text_clipped(
                    at + 2,
                    y,
                    &chrome::ellipsize(what, usize::from(limit.saturating_sub(at + 2))),
                    st(GitRole::Muted, SURFACE),
                    limit,
                );
            }
            y += 1;
        }
        y += u16::from(gap);
    }
}

/// Every row the keys sheet lists for the screen's current context, as `(key, item)`; the
/// item is `Scope · label`, its scope the pane for an `Act` group and the group otherwise
/// (`Changes · Stage all · unstage all`, `Navigate · Move`). The tutor's coverage test reads
/// it: each item names the lesson that teaches it.
#[cfg(test)]
pub(in crate::git::screen) fn rows(app: &GitScreen) -> Vec<(String, String)> {
    let (title, groups) = context(app);
    let mut groups = owned(groups);
    if app.page == 0 {
        in_force(app, &mut groups);
    }
    groups.extend(vim(app));
    groups
        .into_iter()
        .flat_map(|(group, rows)| {
            rows.into_iter().map(move |(key, what)| {
                // Pull, push, undo and redo close every pane's Act rows: one item each.
                let scope = if SYNC.contains(&what.as_str()) {
                    "Sync"
                } else if group == "Act" {
                    title
                } else {
                    group
                };
                (key, format!("{scope} · {what}"))
            })
        })
        .collect()
}

pub(in crate::git::screen) fn paint(app: &GitScreen, frame: &mut Frame, width: u16, height: u16) {
    let lay = super::layout::layout(width, height, app.operation_bar.is_some(), [false; 4]);
    let body = lay.body;
    if body.w < 40 || body.h < 8 {
        return;
    }
    let (title, groups) = context(app);
    let mut groups = owned(groups);
    if app.page == 0 {
        in_force(app, &mut groups);
    }
    // One meaning and one wording per key on a screen (r15 ISS-09): a chord the Vim group
    // lists (the sidebar's `Space e`) is not named again, in other words, above it.
    if let Some(vim) = vim(app) {
        let chords = vim.1.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
        for (_, rows) in &mut groups {
            rows.retain(|(key, _)| !chords.contains(key));
        }
        groups.push(vim);
    }
    let key_width = |group: &Sheet| {
        group
            .1
            .iter()
            .map(|(key, _)| chrome::width(key))
            .max()
            .unwrap_or(0)
    };
    let column_width = |group: &Sheet| {
        key_width(group)
            + 2
            + group
                .1
                .iter()
                .map(|(_, what)| chrome::width(what))
                .max()
                .unwrap_or(0)
    };
    if width >= WIDE_FROM {
        compact(app, frame, width, height, body, title, &groups, true);
        return;
    }
    let w = body.w.saturating_sub(8).min(160);
    let inner_w = w.saturating_sub(4);
    // Flow groups into columns, top to bottom, as many columns as fit.
    let max_h = body.h.saturating_sub(4);
    let mut columns: Vec<Vec<&Sheet>> = vec![Vec::new()];
    let mut used = 0u16;
    let mut widths = vec![0u16];
    for group in &groups {
        let rows = group.1.len() as u16 + 2;
        if used > 0 && used + rows > max_h.saturating_sub(2) {
            columns.push(Vec::new());
            widths.push(0);
            used = 0;
        }
        columns.last_mut().unwrap().push(group);
        let last = widths.last_mut().unwrap();
        *last = (*last).max(column_width(group));
        used += rows;
    }
    // Fewer groups per column reads better when there is room: one group per column.
    if groups.len() <= 4 && widths.iter().sum::<u16>() + 3 * widths.len() as u16 <= inner_w {
        let flat = groups.iter().map(|group| vec![group]).collect::<Vec<_>>();
        let flat_w = groups.iter().map(column_width).collect::<Vec<_>>();
        if flat_w.iter().sum::<u16>() + 3 * flat_w.len() as u16 <= inner_w {
            columns = flat;
            widths = flat_w;
        }
    }
    let fits = widths.iter().sum::<u16>() + 3 * widths.len().saturating_sub(1) as u16 <= inner_w
        && columns.iter().all(|column| {
            column
                .iter()
                .map(|group| group.1.len() as u16 + 2)
                .sum::<u16>()
                <= max_h.saturating_sub(1)
        });
    if !fits {
        compact(app, frame, width, height, body, title, &groups, false);
        return;
    }
    let content_h = columns
        .iter()
        .map(|column| {
            column
                .iter()
                .map(|group| group.1.len() as u16 + 2)
                .sum::<u16>()
        })
        .max()
        .unwrap_or(0);
    let total_w =
        (widths.iter().sum::<u16>() + 3 * widths.len().saturating_sub(1) as u16 + 4).max(48);
    let w = total_w.min(w).max(40.min(body.w));
    let h = (content_h + 3).min(body.h.saturating_sub(2));
    let rect = Rect::new(
        body.x + (body.w - w) / 2,
        body.y + (body.h.saturating_sub(h)) / 2,
        w,
        h,
    );
    let (right, bottom) = border(app, frame, rect, title);
    let mut x = rect.x + 2;
    caret(frame, x, rect.y + 2);
    for (column, column_w) in columns.iter().zip(&widths) {
        let mut y = rect.y + 2;
        for group in column {
            if y >= bottom {
                break;
            }
            frame
                .grid
                .put_text_clipped(x, y, group.0, bold(GitRole::Focus, SURFACE), right);
            y += 1;
            let keys = key_width(group);
            for (key, what) in &group.1 {
                if y >= bottom {
                    break;
                }
                frame
                    .grid
                    .put_text_clipped(x, y, key, bold(GitRole::Text, SURFACE), right);
                frame.grid.put_text_clipped(
                    x + keys + 2,
                    y,
                    &chrome::ellipsize(what, usize::from(right.saturating_sub(x + keys + 3))),
                    st(GitRole::Muted, SURFACE),
                    right,
                );
                y += 1;
            }
            y += 1;
        }
        x += column_w + 3;
        if x >= right {
            break;
        }
    }
}
