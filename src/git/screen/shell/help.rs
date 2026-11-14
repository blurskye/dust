//! X-HELP (DESIGN.md §3.43): the keys of the focused pane, grouped, in columns.
use super::chrome::{self, bold, st};
use super::glyphs::{Glyph, glyph};
use super::layout::Rect;
use super::super::GitScreen;
use crate::ui::{Frame, GitRole, GitSurface};

const SURFACE: GitSurface = GitSurface::Sheet;
/// The help border and its title row.
const EDGE: GitSurface = GitSurface::Pane;

type Group = (&'static str, Vec<(&'static str, &'static str)>);

/// DESIGN.md §5.7: the Pull requests and Issues tabs.
fn forge(app: &GitScreen) -> (&'static str, Vec<Group>) {
    let pull = app.page == 2;
    let navigate: Group = (
        "Navigate",
        vec![
            ("2 · 3", "List · reader"),
            ("Tab", "Reader · next link"),
            ("[ ]", "Sections (list) · tabs (reader)"),
            ("< >", "Previous · next top tab"),
            ("j k", "Move"),
            ("Enter", "Open · follow link"),
            ("Esc", "Back to the list"),
            ("C-f", "Find in the reader"),
            ("C-n", "Load more"),
        ],
    );
    let filter: Group = (
        "Filter",
        vec![
            ("/", "Filter query"),
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
                ("c", "Check out · reply in the reader"),
                ("w", "Review in a worktree"),
                ("m", "Merge readiness"),
                ("d", "Files"),
                ("v", "Mark file viewed (Files)"),
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
        act.1.retain(|(key, _)| READS.contains(key) && (pull || *key != "m"));
        groups.push((
            "Read-only",
            vec![("a", "Accounts · how to sign in"), ("", "Writes say why when pressed")],
        ));
    }
    let mut all = vec![navigate, act, filter];
    all.extend(groups);
    (if pull { "Pull requests" } else { "Issues" }, all)
}

/// DESIGN.md §3.37–§3.41: Actions (GitLab: Pipelines).
fn actions(app: &GitScreen) -> (&'static str, Vec<Group>) {
    let navigate: Group = (
        "Navigate",
        vec![
            ("j k", "Move"),
            ("Enter · l", "Open run · drill in"),
            ("Esc · h", "Back"),
            ("Tab", "Next column"),
            ("C-h/j/k/l", "Move focus"),
            ("[ ]", "Jobs · Artifacts · Logs"),
            ("< >", "Previous · next top tab"),
            ("/", "Filter runs · find in a log"),
            ("R", "Refresh"),
        ],
    );
    let log: Group = (
        "Log",
        vec![
            ("f", "Full job log · step log"),
            ("a", "Attempts"),
            ("n N", "Next · previous match"),
            ("h l", "Pan 8 columns"),
            ("w", "Wrap"),
            ("Enter", "Open file:line"),
            ("C-s", "Save the log"),
        ],
    );
    let act: Group = (
        "Act",
        vec![
            ("r", "Rerun"),
            ("x", "Cancel"),
            ("d", "Dispatch"),
            ("A", "Approve a waiting run"),
            ("Enter", "Download artifact (Artifacts)"),
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
    let view: Group = (
        "View",
        vec![
            ("s", "Split · unified diff"),
            ("h l", "Fold tree · pan diff"),
            ("m", "Full commit message"),
            ("r", "Refresh"),
            (":tutor git", "Practice in a disposable repo"),
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
            ],
        )
    });
    let (title, act): (&str, Group) = if app.commit_open || (app.focus == 0 && app.last_list == 2 && app.commit.is_some()) {
        (
            "Commit files",
            (
                "Act",
                vec![
                    ("Enter", "Open the file's diff"),
                    ("J K", "Next · previous file"),
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
                        ("Space", "Stage · unstage hunk"),
                        ("a", "Stage · unstage line or range"),
                        ("V", "Start · finish a range"),
                        ("d", "Discard hunk (confirm)"),
                        ("D", "Discard line (confirm)"),
                        ("c", "Commit staged"),
                    ],
                ),
            ),
            0 if app.files_view == 1 => (
                "Worktrees",
                ("Act", vec![("n", "New worktree"), ("d", "Remove a clean worktree")]),
            ),
            0 if app.files_view == 2 => (
                "Submodules",
                ("Act", vec![("Space", "Toggle pointer"), ("u", "Init · update")]),
            ),
            0 => (
                "Changes",
                (
                    "Act",
                    vec![
                        ("Space", "Stage · unstage"),
                        ("a", "Stage all · unstage all"),
                        ("x", "Mark file"),
                        ("V", "Mark a range"),
                        ("d", "Discard (confirm)"),
                        ("D", "Discard marked (confirm)"),
                        ("c", "Commit staged"),
                        ("P", "Push"),
                        ("A W F", "Amend · reword · fixup"),
                        ("t", "Flat · tree"),
                        ("F", "Changed · all files"),
                        ("v", "Open in reader"),
                    ],
                ),
            ),
            1 if app.subview == 2 => (
                "Remotes",
                ("Act", vec![("f", "Fetch (confirm)"), ("Enter", "Show history")]),
            ),
            1 if app.subview == 3 => (
                "Tags",
                ("Act", vec![("Enter", "Inspect tag"), ("d", "Delete local tag")]),
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
                        ("Space", "Checkout detached"),
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
                        ("A", "Amend HEAD with staged"),
                        ("W", "Reword"),
                        ("F", "Fixup staged into commit"),
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
    let mut groups = vec![act];
    groups.extend(operation);
    groups.push(navigate);
    groups.push(view);
    (title, groups)
}

/// The sheet border with its title and `Esc close`; returns the right and bottom edges.
fn border(app: &GitScreen, frame: &mut Frame, rect: Rect, title: &str) -> (u16, u16) {
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
            (format!("{} ", glyph(Glyph::Search, app.icon_mode)), st(GitRole::Focus, EDGE)),
            ("Keys".into(), bold(GitRole::Text, EDGE)),
            (format!(" · {title} "), st(GitRole::Muted, EDGE)),
        ],
        close_at,
    );
    frame.grid.put_text_clipped(close_at, rect.y, close, st(GitRole::Muted, EDGE), right);
    (right, bottom)
}

/// Small terminals: every group stays, its keys flowed two or three to a row,
/// each `key  what` unaligned so long keys do not starve short labels. The sheet
/// grows over the top bar and footer before anything is cut.
fn compact(
    app: &GitScreen,
    frame: &mut Frame,
    width: u16,
    height: u16,
    body: Rect,
    title: &str,
    groups: &[Group],
) {
    let cell = |key: &str, what: &str| chrome::width(key) + 2 + chrome::width(what);
    let inner_w = body.w.saturating_sub(4);
    // As many cells per row as keep most labels whole.
    let mut per_row = 1u16;
    for n in (1..=3u16).rev() {
        let cell_w = inner_w.saturating_sub(2 * (n - 1)) / n;
        let whole = groups
            .iter()
            .flat_map(|group| group.1.iter())
            .filter(|(key, what)| cell(*key, *what) <= cell_w)
            .count();
        let total = groups.iter().map(|group| group.1.len()).sum::<usize>();
        if cell_w >= 24 && whole * 10 >= total * 8 {
            per_row = n;
            break;
        }
    }
    let cell_w = inner_w.saturating_sub(2 * (per_row - 1)) / per_row;
    let rows_of = |group: &Group| 1 + (group.1.len() as u16).div_ceil(per_row);
    let content: u16 = groups.iter().map(rows_of).sum();
    let gaps = groups.len().saturating_sub(1) as u16;
    // Borders plus content; gaps between groups only when there is room.
    let (rect, gap) = if content + gaps + 3 <= body.h {
        let h = content + gaps + 3;
        (Rect::new(body.x, body.y + (body.h - h) / 2, body.w, h), true)
    } else if content + 2 <= body.h {
        (Rect::new(body.x, body.y, body.w, body.h), false)
    } else {
        (Rect::new(0, 0, width, height), false)
    };
    let (right, bottom) = border(app, frame, rect, title);
    let mut y = rect.y + 1 + u16::from(gap);
    for group in groups {
        if y >= bottom {
            break;
        }
        frame.grid.put_text_clipped(rect.x + 2, y, group.0, bold(GitRole::Focus, SURFACE), right);
        y += 1;
        for row in group.1.chunks(usize::from(per_row)) {
            if y >= bottom {
                break;
            }
            for (index, (key, what)) in row.iter().enumerate() {
                let x = rect.x + 2 + index as u16 * (cell_w + 2);
                let limit = (x + cell_w).min(right);
                let at = frame.grid.put_text_clipped(x, y, key, bold(GitRole::Text, SURFACE), limit);
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

pub(in crate::git::screen) fn paint(app: &GitScreen, frame: &mut Frame, width: u16, height: u16) {
    let lay = super::layout::layout(width, height, app.operation_bar.is_some(), [false; 4]);
    let body = lay.body;
    if body.w < 40 || body.h < 8 {
        return;
    }
    let (title, groups) = context(app);
    let key_width = |group: &Group| {
        group
            .1
            .iter()
            .map(|(key, _)| chrome::width(key))
            .max()
            .unwrap_or(0)
    };
    let column_width = |group: &Group| {
        key_width(group)
            + 2
            + group
                .1
                .iter()
                .map(|(_, what)| chrome::width(what))
                .max()
                .unwrap_or(0)
    };
    let w = body.w.saturating_sub(8).min(160);
    let inner_w = w.saturating_sub(4);
    // Flow groups into columns, top to bottom, as many columns as fit.
    let max_h = body.h.saturating_sub(4);
    let mut columns: Vec<Vec<&Group>> = vec![Vec::new()];
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
            column.iter().map(|group| group.1.len() as u16 + 2).sum::<u16>() <= max_h.saturating_sub(1)
        });
    if !fits {
        compact(app, frame, width, height, body, title, &groups);
        return;
    }
    let content_h = columns
        .iter()
        .map(|column| column.iter().map(|group| group.1.len() as u16 + 2).sum::<u16>())
        .max()
        .unwrap_or(0);
    let total_w = (widths.iter().sum::<u16>() + 3 * widths.len().saturating_sub(1) as u16 + 4).max(48);
    let w = total_w.min(w).max(40.min(body.w));
    let h = (content_h + 3).min(body.h.saturating_sub(2));
    let rect = Rect::new(body.x + (body.w - w) / 2, body.y + (body.h.saturating_sub(h)) / 2, w, h);
    let (right, bottom) = border(app, frame, rect, title);
    let mut x = rect.x + 2;
    for (column, column_w) in columns.iter().zip(&widths) {
        let mut y = rect.y + 2;
        for group in column {
            if y >= bottom {
                break;
            }
            frame.grid.put_text_clipped(x, y, group.0, bold(GitRole::Focus, SURFACE), right);
            y += 1;
            let keys = key_width(group);
            for (key, what) in &group.1 {
                if y >= bottom {
                    break;
                }
                frame.grid.put_text_clipped(x, y, key, bold(GitRole::Text, SURFACE), right);
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
