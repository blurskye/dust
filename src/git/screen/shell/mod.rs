//! The Dusty Git shell (DESIGN.md §2–§4): one top bar, an optional operation banner,
//! the rail of panes beside the reader, and a one-row footer.
pub(crate) mod chrome;
pub(crate) mod glyphs;
pub(crate) mod graph;
pub(crate) mod layout;
mod rail;
mod reader;
pub(super) mod sheet;
pub(super) mod help;

use super::GitScreen;
use chrome::{Tab, TopBar, bold, st};
use glyphs::Glyph;
use crate::ui::{CellGrid, Frame, GitRole, GitSurface, Style};
use layout::{Mode, Rect};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct HistoryInfo {
    pub parents: Vec<String>,
    pub author: String,
    pub date: String,
    /// `%D` decorations: `HEAD -> main`, `origin/main`, `tag: v1`.
    pub refs: Vec<String>,
}

/// `%H %P %an %ad %D %s`, tab separated.
pub(super) fn parse_history(text: &str) -> (Vec<(String, String)>, Vec<HistoryInfo>) {
    let mut history = Vec::new();
    let mut info = Vec::new();
    for line in text.lines() {
        let mut fields = line.splitn(6, '\t');
        let (Some(id), Some(parents), Some(author), Some(date), Some(refs), Some(subject)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        history.push((id.to_owned(), subject.to_owned()));
        info.push(HistoryInfo {
            parents: parents.split_whitespace().map(Into::into).collect(),
            author: author.into(),
            date: date.into(),
            refs: refs
                .split(", ")
                .filter(|reference| !reference.is_empty())
                .map(Into::into)
                .collect(),
        });
    }
    (history, info)
}

/// `git diff --numstat`: `added<TAB>removed<TAB>path`, `-` for binary files.
pub(super) fn parse_numstat(text: &str) -> HashMap<PathBuf, Option<(u32, u32)>> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?;
            let removed = fields.next()?;
            let path = fields.next()?;
            let counts = added.parse().ok().zip(removed.parse().ok());
            Some((PathBuf::from(path), counts))
        })
        .collect()
}

impl GitScreen {
    /// The Changes pane lists paths flat unless asked for a tree or flooded with paths.
    pub(super) fn changes_flat(&self) -> bool {
        !self.changes_tree
            && !self.all_files
            && self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.entries.len() <= 40)
    }

    /// Untracked files have no numstat; count their lines as an all-added patch.
    pub(super) fn count_untracked(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let missing = snapshot
            .entries
            .iter()
            .filter(|entry| entry.status == crate::git::GitFileStatus::Untracked)
            .map(|entry| entry.relative_path.clone())
            .filter(|path| !self.numstat.contains_key(path))
            .take(256)
            .collect::<Vec<_>>();
        for path in missing {
            use std::io::Read;
            let mut bytes = Vec::new();
            let read = std::fs::File::open(self.root.join(&path))
                .and_then(|file| file.take(1 << 20).read_to_end(&mut bytes));
            let counts = match read {
                Ok(_) if bytes.contains(&0) => None,
                Ok(_) => {
                    let lines = bytes.iter().filter(|&&byte| byte == b'\n').count()
                        + usize::from(bytes.last().is_some_and(|&byte| byte != b'\n'));
                    Some((lines as u32, 0))
                }
                Err(_) => continue,
            };
            self.numstat.insert(path, counts);
        }
    }

    /// Top tabs in display order, mapped to `page` numbers.
    pub(super) const TAB_PAGES: [usize; 5] = [0, 2, 1, 3, 4];
}

/// Row 0, shared by every top-level tab.
pub(super) fn top_bar(app: &GitScreen, grid: &mut CellGrid, width: u16) {
    let mode = app.icon_mode;
    let head = app.head_label();
    let has_upstream = app.refs.as_ref().is_some_and(|refs| {
        refs.branches
            .iter()
            .any(|branch| branch.head && branch.upstream.is_some())
    });
    let tab = |glyph, role, label, short| Tab {
        glyph,
        role,
        label,
        short,
        count: None,
    };
    // GitLab says "Merge requests" and "Pipelines" (FORGE.md §4.3).
    let terms = app.github.terms();
    let pr_short = if terms.pr_glyph == "!" { "MRs" } else { "PRs" };
    let active = GitScreen::TAB_PAGES
        .iter()
        .position(|&page| page == app.page)
        .unwrap_or(0);
    chrome::top_bar(
        grid,
        width,
        &TopBar {
            repo: &app.project,
            head: if head.is_empty() { "HEAD" } else { head },
            detached: false,
            operation: app.operation_bar.is_some(),
            divergence: has_upstream.then(|| app.divergence()),
            tabs: vec![
                tab(Glyph::Repo, GitRole::Focus, "Git", "Git"),
                Tab {
                    count: app.github.tab_count(2).map(|count| count.to_string()),
                    ..tab(Glyph::PullRequest, GitRole::Ref, terms.pr_plural, pr_short)
                },
                Tab {
                    count: app.github.tab_count(1).map(|count| count.to_string()),
                    ..tab(Glyph::Issue, GitRole::Added, "Issues", "Issues")
                },
                tab(
                    Glyph::Actions,
                    GitRole::Modified,
                    terms.actions_tab,
                    // GitLab calls them pipelines at every width, as the reader's tabs do.
                    if terms.actions_tab == "Pipelines" { "Pipes" } else { "CI" },
                ),
            ]
            .into_iter()
            .chain(app.github.notifications_available().then(|| Tab {
                // Counted once the page has been read (§3.35): no request just to paint.
                count: app.github.unread_notifications().map(|count| count.to_string()),
                ..tab(Glyph::Notification, GitRole::Focus, terms.notifications_tab, "")
            }))
            .collect(),
            active,
            mode,
        },
    );
}

fn too_small(frame: &mut Frame, width: u16, height: u16) {
    chrome::fill(
        &mut frame.grid,
        Rect::new(0, 0, width, height),
        st(GitRole::Text, GitSurface::Canvas),
    );
    let text = format!(
        "Dusty Git needs {}×{}",
        layout::MIN_WIDTH,
        layout::MIN_HEIGHT
    );
    let x = width.saturating_sub(chrome::width(&text)) / 2;
    frame.grid.put_text_clipped(
        x,
        height / 2,
        &text,
        bold(GitRole::Modified, GitSurface::Canvas),
        width,
    );
}

/// The Git tab.
pub(super) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if layout::mode(width, height) == Mode::TooSmall {
        too_small(&mut frame, width, height);
        return frame;
    }
    chrome::fill(
        &mut frame.grid,
        Rect::new(0, 0, width, height),
        st(GitRole::Text, GitSurface::Canvas),
    );
    top_bar(app, &mut frame.grid, width);
    let empty = [false, false, false, app.stashes.is_empty()];
    // The tutor banner gets its own three rows above the footer instead of covering
    // the bottom of the rail, where the Stashes pane lives.
    let tutor_rows = if app.tutor.is_some()
        && layout::mode(width, height.saturating_sub(3)) != Mode::TooSmall
    {
        3
    } else {
        0
    };
    let mut lay = layout::layout(
        width,
        height - tutor_rows,
        app.operation_bar.is_some(),
        empty,
    );
    lay.footer.y = height.saturating_sub(1);
    if let Some(rect) = lay.banner {
        banner(app, &mut frame.grid, rect);
    }
    if lay.mode == Mode::Narrow {
        if app.focus == 4 {
            reader::paint(app, &mut frame, lay.body);
        } else {
            rail::paint(app, &mut frame, app.focus.min(3), lay.body);
        }
    } else {
        for pane in 0..4 {
            rail::paint(app, &mut frame, pane, lay.rail[pane]);
        }
        reader::paint(app, &mut frame, lay.reader);
    }
    if let Some(tutor) = &app.tutor {
        let rect = Rect::new(0, lay.footer.y.saturating_sub(3), width, 3);
        chrome::fill(&mut frame.grid, rect, st(GitRole::Text, GitSurface::Sheet));
        frame.grid.put_text_clipped(
            1,
            rect.y,
            &tutor.title(),
            bold(GitRole::Focus, GitSurface::Sheet),
            width - 1,
        );
        frame.grid.put_text_clipped(
            1,
            rect.y + 1,
            &chrome::ellipsize(tutor.instruction(), usize::from(width - 2)),
            st(GitRole::Text, GitSurface::Sheet),
            width - 1,
        );
        frame.grid.put_text_clipped(
            1,
            rect.y + 2,
            tutor.hint(),
            st(GitRole::Muted, GitSurface::Sheet),
            width - 1,
        );
    }
    footer(app, &mut frame, lay.footer);
    if !sheet::paint(app, &mut frame, &lay) {
        super::render::forms(app, &mut frame, width, height);
    }
    frame
}

fn banner(app: &GitScreen, grid: &mut CellGrid, rect: Rect) {
    let Some(bar) = &app.operation_bar else {
        return;
    };
    let surface = GitSurface::Warning;
    let mode = app.icon_mode;
    let mut spans = vec![(
        format!(
            "{} {}",
            glyphs::glyph(Glyph::Running, mode),
            capitalise(&bar.operation)
        ),
        bold(GitRole::Modified, surface),
    )];
    if let Some(replay) = &app.paused_replay {
        spans.push((
            format!(" · {}", app.head_label()),
            st(GitRole::Text, surface),
        ));
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push((bar.phase.clone(), bold(GitRole::Text, surface)));
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push(("Current ".into(), st(GitRole::Muted, surface)));
        spans.push((
            replay.id.to_string()[..7].to_owned(),
            st(GitRole::Hash, surface),
        ));
        spans.push((format!(" {}", replay.subject), st(GitRole::Text, surface)));
    } else {
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push((bar.phase.clone(), bold(GitRole::Text, surface)));
    }
    let mut actions = Vec::new();
    if bar.offers(crate::git::GitOperationAction::Continue) {
        actions.push(("C", "continue"));
    }
    if bar.offers(crate::git::GitOperationAction::Skip) {
        actions.push(("S", "skip"));
    }
    actions.push(("X", "abort"));
    chrome::banner(grid, rect, &spans, &actions);
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn footer(app: &GitScreen, frame: &mut Frame, rect: Rect) {
    // While a filter is being typed the footer is its input line.
    if let Some(target) = app.filter_target() {
        let filter = app.list_filter(target);
        if filter.editing {
            let surface = GitSurface::TopBar;
            chrome::fill(&mut frame.grid, rect, st(GitRole::Text, surface));
            let x = frame.grid.put_text_clipped(
                rect.x + 1,
                rect.y,
                &format!("{} ", glyphs::glyph(Glyph::Search, app.icon_mode)),
                bold(GitRole::Focus, surface),
                rect.right(),
            );
            let input = &filter.input;
            let available = usize::from(rect.right().saturating_sub(x + 30)).max(4);
            let shown = chrome::ellipsize_start(&input.input[..input.cursor], available);
            let cursor = frame.grid.put_text_clipped(x, rect.y, &shown, st(GitRole::Text, surface), rect.right());
            frame.grid.put_text_clipped(
                cursor,
                rect.y,
                &input.input[input.cursor..],
                st(GitRole::Text, surface),
                rect.right().saturating_sub(28),
            );
            let help = "Enter keep · Esc clear · ↑↓ matches";
            frame.grid.put_text_clipped(
                rect.right().saturating_sub(chrome::width(help) + 1),
                rect.y,
                help,
                st(GitRole::Muted, surface),
                rect.right(),
            );
            frame.cursor = Some((cursor, rect.y, crossterm::cursor::SetCursorStyle::SteadyBar));
            return;
        }
    }
    let grid = &mut frame.grid;
    if app.form.is_some() {
        let multiline = app
            .form
            .as_ref()
            .is_some_and(|form| form.fields.iter().any(|field| field.multiline));
        let commit = app
            .form
            .as_ref()
            .is_some_and(|form| matches!(form.action, super::FormAction::Commit));
        let hints: &[(&str, &str)] = if commit {
            &[
                ("C-Enter", "commit"),
                ("Tab", "next field"),
                ("M-s M-a M-n", "sign-off · amend · skip hooks"),
                ("Esc", "back"),
            ]
        } else if multiline {
            &[("C-Enter", "confirm"), ("Tab", "next field"), ("Esc", "cancel")]
        } else {
            &[("Enter", "confirm"), ("Tab", "next field"), ("Esc", "cancel")]
        };
        // A refusal answering this form stays visible beside it.
        let message = (!app.busy && !app.notice.is_empty()).then(|| (app.notice.as_str(), true));
        chrome::footer(grid, rect, hints, "", message);
        return;
    }
    let message = if app.busy {
        Some(("Git operation running…", false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), looks_like_error(&app.notice)))
    } else {
        None
    };
    let staged = app.preview_staged();
    let marked = app.file_selection_context() && !app.file_selection.marked.is_empty();
    let hints: Vec<(&str, &str)> = if app.focus == 4 && app.preview_anchor.is_some() {
        vec![
            ("a", if staged { "unstage range" } else { "stage range" }),
            ("d", "discard"),
            ("Esc", "cancel"),
        ]
    } else if app.focus == 4 && app.last_list == 0 && app.preview.patch {
        vec![
            ("Space", if staged { "unstage hunk" } else { "stage hunk" }),
            ("a", "line"),
            ("V", "range"),
            ("d", "discard"),
            ("s", "split"),
        ]
    } else if app.focus == 4 && app.last_list == 2 {
        vec![
            ("Enter", "files"),
            ("m", "message"),
            ("/", "find"),
            ("s", "split"),
            ("Esc", "back"),
        ]
    } else if app.focus == 4 {
        vec![("/", "find"), ("s", "split"), ("Esc", "back")]
    } else if marked {
        vec![
            ("Space", "stage marked"),
            ("x", "mark"),
            ("V", "range"),
            ("Esc", "clear"),
        ]
    } else {
        match (app.focus, app.files_view, app.subview, app.history_view) {
            (0, 1, ..) => vec![("Enter", "open"), ("n", "new worktree"), ("d", "remove")],
            (0, 2, ..) => vec![("Space", "pointer"), ("Enter", "open"), ("u", "update")],
            (0, ..) => vec![
                ("Space", if staged { "unstage" } else { "stage" }),
                ("v", "lines"),
                ("c", "commit"),
                ("d", "discard"),
                ("/", "find"),
            ],
            (1, _, 2, _) => vec![("Enter", "open"), ("f", "fetch"), ("/", "find")],
            (1, _, 3, _) => vec![("Enter", "inspect"), ("d", "delete"), ("/", "find")],
            (1, ..) => vec![
                ("Enter", "checkout"),
                ("n", "new"),
                ("m", "merge"),
                ("R", "rebase"),
                ("d", "delete"),
            ],
            (2, _, _, 1) => vec![("n", "rescue branch"), ("Space", "detach"), ("Enter", "files")],
            (2, ..) => vec![
                ("Enter", "files"),
                ("A", "amend"),
                ("W", "reword"),
                ("F", "fixup"),
                ("/", "find"),
            ],
            _ => vec![("a", "apply"), ("p", "pop"), ("d", "drop"), ("n", "save")],
        }
    };
    let search = &app.preview.search;
    let context = if app.focus == 4 && search.active() {
        let position = if search.matches.is_empty() {
            "no matches".into()
        } else {
            let current = search
                .current
                .filter(|_| search.selected(app.preview_cursor).is_some())
                .map_or_else(|| "-".into(), |index| (index + 1).to_string());
            format!("{current}/{}", search.matches.len())
        };
        format!("/{}/ · {position} · n N", search.query)
    } else if marked {
        app.file_selection_summary()
    } else if app.focus == 4 || app.last_list == 0 {
        app.selected_row()
            .filter(|_| app.last_list == 0)
            .map(|row| row.value.display().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    chrome::footer(grid, rect, &hints, &context, message);
}

fn looks_like_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["error", "fatal", "cannot", "failed", "refuse", "invalid", "not ", "nothing"]
        .iter()
        .any(|word| lower.contains(word))
}

/// A path split into a muted directory and a file name, fitted into `cells`.
pub(super) fn fit_path(path: &str, cells: usize) -> (String, String) {
    let (dir, name) = match path.rfind('/') {
        Some(at) => (&path[..=at], &path[at + 1..]),
        None => ("", path),
    };
    let name_width = chrome::width(name) as usize;
    if chrome::width(dir) as usize + name_width <= cells {
        return (dir.into(), name.into());
    }
    if !dir.is_empty() && name_width + 2 <= cells {
        let room = cells - name_width;
        return (chrome::ellipsize_start(dir, room), name.into());
    }
    (String::new(), chrome::ellipsize(name, cells))
}

pub(super) fn surface_for(selected: bool, focused: bool) -> GitSurface {
    match (selected, focused) {
        (true, true) => GitSurface::Cursor,
        (true, false) => GitSurface::CursorInactive,
        _ => GitSurface::Pane,
    }
}

pub(super) fn syntax(group: crate::config::Highlight, surface: GitSurface) -> Style {
    Style::GitSyntax { group, surface }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_lines_keep_parents_refs_and_tabbed_subjects() {
        let (history, info) = parse_history(
            "a1\tb2 c3\tSam Rao\t2026-11-12\tHEAD -> main, tag: v1\tMerge\tit\nbad line\n",
        );
        assert_eq!(history, [("a1".into(), "Merge\tit".into())]);
        assert_eq!(info[0].parents, ["b2", "c3"]);
        assert_eq!(info[0].refs, ["HEAD -> main", "tag: v1"]);
    }

    #[test]
    fn numstat_marks_binary_files() {
        let stats = parse_numstat("3\t1\torbit/queue.py\n-\t-\tlogo.png\n");
        assert_eq!(stats[&PathBuf::from("orbit/queue.py")], Some((3, 1)));
        assert_eq!(stats[&PathBuf::from("logo.png")], None);
    }

    #[test]
    fn paths_drop_directories_before_names() {
        assert_eq!(
            fit_path("orbit/queue.py", 20),
            ("orbit/".into(), "queue.py".into())
        );
        assert_eq!(
            fit_path("src/very/deep/queue.py", 14),
            ("…deep/".into(), "queue.py".into())
        );
        assert_eq!(fit_path("a/long_file_name.py", 8), (String::new(), "long_fi…".into()));
    }
}
