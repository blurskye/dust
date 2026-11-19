//! The Dusty Git shell: one top bar, an optional operation banner,
//! the rail of panes beside the reader, and a one-row footer.
pub mod chrome;
pub mod glyphs;
pub mod graph;
pub(super) mod help;
pub mod layout;
mod rail;
mod reader;
pub(super) mod sheet;

use super::GitScreen;
use crate::ui::{CellGrid, Frame, GitRole, GitSurface, Style};
use chrome::{Tab, TopBar, bold, st};
use glyphs::Glyph;
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
            .filter(|entry| entry.status == crate::GitFileStatus::Untracked)
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
    let tab = |glyph, role, label| Tab {
        glyph,
        role,
        label,
        count: None,
        badge: false,
        marks: Vec::new(),
    };
    // GitLab says "Merge requests" and "Pipelines".
    let terms = app.github.terms();
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
                tab(Glyph::Repo, GitRole::Focus, "Git"),
                Tab {
                    count: app.github.tab_count(2).map(|count| count.to_string()),
                    ..tab(Glyph::PullRequest, GitRole::Ref, terms.pr_plural)
                },
                Tab {
                    count: app.github.tab_count(1).map(|count| count.to_string()),
                    ..tab(Glyph::Issue, GitRole::Added, "Issues")
                },
                // GitLab calls them pipelines at every width, as the reader's tabs do.
                Tab {
                    // Failing runs and a running mark, as GitHub badges checks.
                    marks: app
                        .github
                        .actions_badge()
                        .map(|(failing, running)| {
                            let failed = (failing > 0).then(|| {
                                (
                                    format!(
                                        "{} {failing}",
                                        glyphs::glyph(Glyph::CheckFailed, mode)
                                    ),
                                    GitRole::Removed,
                                )
                            });
                            // Running is CI's pending mark, not the unread `●`.
                            let dot = running.then(|| {
                                (
                                    glyphs::glyph(Glyph::CheckPending, mode).to_owned(),
                                    GitRole::Modified,
                                )
                            });
                            failed.into_iter().chain(dot).collect()
                        })
                        .unwrap_or_default(),
                    ..tab(Glyph::Actions, GitRole::Modified, terms.actions_tab)
                },
            ]
            .into_iter()
            .chain(app.github.notifications_available().then(|| {
                Tab {
                    // The unread badge, on every screen: the inbox's first page is
                    // read behind each forge page's own data. No badge for nothing unread.
                    count: app
                        .github
                        .unread_notifications()
                        .filter(|count| *count > 0)
                        .map(|count| count.to_string()),
                    badge: true,
                    ..tab(Glyph::Notification, GitRole::Focus, terms.notifications_tab)
                }
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
    let tutor_rows =
        if app.tutor.is_some() && layout::mode(width, height.saturating_sub(3)) != Mode::TooSmall {
            3
        } else {
            0
        };
    let mut lay = layout::fitted(
        width,
        height - tutor_rows,
        app.operation_bar.is_some(),
        empty,
        rail_rows(app),
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
    } else if app.rail_hidden && app.focus == 4 {
        // `<leader> e` hid the rail: the reader takes the body until a list has focus.
        reader::paint(app, &mut frame, lay.body);
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
        if let Some(completed) = tutor.completed() {
            super::tutor::completed_mark(
                &mut frame.grid,
                rect.y,
                &completed,
                &tutor.title(),
                width,
            );
        }
        frame.grid.put_text_clipped(
            1,
            rect.y + 1,
            &chrome::ellipsize(
                &super::tutor::keyed(tutor.instruction(), app.github.key_table()),
                usize::from(width - 2),
            ),
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

/// How long the rail may wait at its budgets for content counts (a cold open, with no
/// warm cache): long enough for a usual status read, short enough not to be noticed.
const RAIL_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The interior rows each rail pane's content wants: changed files, the
/// sub-tab row plus local branches, and stashes. History takes what is left, so its own
/// count is not an input; nor is an open commit's file count (Changes keeps its budget).
/// Counted once, when status and refs first arrive, and kept for the session: focus,
/// cursor, refreshes, new branches or files never move a rectangle. Until then every
/// count is 0 (unknown) and the panes keep their budgets. Panes shown at their budgets
/// for [`RAIL_SETTLE`] keep them: counts that come later would move what was read.
fn rail_rows(app: &mut GitScreen) -> [u16; 4] {
    if let Some(rows) = app.rail_rows {
        return rows;
    }
    let settled = app
        .rail_unsized
        .is_some_and(|since| since.elapsed() >= RAIL_SETTLE);
    if settled {
        app.rail_rows = Some([0; 4]);
        return [0; 4];
    }
    if app.snapshot.is_none() || app.refs.is_none() {
        app.rail_unsized.get_or_insert_with(std::time::Instant::now);
        return [0; 4];
    }
    let clamp = |rows: usize| u16::try_from(rows).unwrap_or(u16::MAX);
    let tabs = app.refs.as_ref().is_some_and(|refs| {
        refs.worktrees.len() > 1 || refs.submodules.as_ref().is_ok_and(|subs| !subs.is_empty())
    });
    let changes = app
        .snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.entries.len())
        + usize::from(tabs);
    let branches = app.refs.as_ref().map_or(0, |refs| {
        refs.branches.iter().filter(|branch| !branch.remote).count()
    });
    let rows = [
        clamp(changes),
        clamp(1 + branches),
        0,
        clamp(app.stashes.len()),
    ];
    app.rail_rows = Some(rows);
    rows
}

fn banner(app: &GitScreen, grid: &mut CellGrid, rect: Rect) {
    let Some(bar) = &app.operation_bar else {
        return;
    };
    let surface = GitSurface::Warning;
    let mode = app.icon_mode;
    // What is applied onto what and how many conflict hunks are left (`conflicts.rs`);
    // Git's own phase when the repository does not say.
    let context = app.conflict.as_ref();
    let title = context.map_or_else(|| capitalise(&bar.operation), |context| context.title());
    let mut spans = vec![(
        format!("{} {title}", glyphs::glyph(Glyph::Running, mode)),
        bold(GitRole::Modified, surface),
    )];
    let what = context.and_then(|context| context.what.clone());
    if let Some(what) = &what {
        spans.push((format!(" · {what}"), st(GitRole::Text, surface)));
    }
    if let Some((step, of)) = context.and_then(|context| context.step) {
        spans.push((
            format!(" · commit {step} of {of}"),
            st(GitRole::Muted, surface),
        ));
    }
    let phase = context
        .and_then(|context| context.progress())
        .unwrap_or_else(|| bar.phase.clone());
    if let Some(replay) = &app.paused_replay {
        if what.is_none() {
            spans.push((
                format!(" · {}", app.head_label()),
                st(GitRole::Text, surface),
            ));
        }
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push((phase, bold(GitRole::Text, surface)));
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push(("Current ".into(), st(GitRole::Muted, surface)));
        spans.push((
            replay.id.to_string()[..7].to_owned(),
            st(GitRole::Hash, surface),
        ));
        spans.push((format!(" {}", replay.subject), st(GitRole::Text, surface)));
    } else {
        spans.push(("  │  ".into(), st(GitRole::Border, surface)));
        spans.push((phase, bold(GitRole::Text, surface)));
    }
    // Each action names the operation it acts on, where the banner has room; skip only
    // where Git has one.
    let wide = rect.w >= 120;
    let named = |verb: &str| {
        if wide {
            format!("{verb} {}", bar.operation)
        } else {
            verb.to_owned()
        }
    };
    let (continue_label, abort_label) = (named("continue"), named("abort"));
    let mut actions = Vec::new();
    if bar.offers(crate::GitOperationAction::Continue) {
        actions.push(("C", continue_label.as_str()));
    }
    if bar.offers(crate::GitOperationAction::Skip) {
        actions.push(("S", if wide { "skip commit" } else { "skip" }));
    }
    actions.push(("X", abort_label.as_str()));
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
            let rect = chrome::footer_rect(rect);
            let x = frame.grid.put_text_clipped(
                rect.x + 1,
                rect.y,
                &format!("{} ", glyphs::glyph(Glyph::Search, app.icon_mode)),
                bold(GitRole::Focus, surface),
                rect.right(),
            );
            let input = &filter.input;
            let typing = app.input_mode() == Some(super::modal::InputMode::Insert);
            let help = if app.modal && typing {
                "Enter keep · Esc normal · ↑↓ matches"
            } else {
                "Enter keep · Esc clear · ↑↓ matches"
            };
            // The query keeps clear of the keys at the right.
            let reserved = chrome::width(help) + 2;
            let available = usize::from(rect.right().saturating_sub(x + reserved)).max(4);
            let before = &input.input[..input.cursor];
            let shown = chrome::ellipsize_start(before, available);
            let cursor = frame.grid.put_text_clipped(
                x,
                rect.y,
                &shown,
                st(GitRole::Text, surface),
                rect.right(),
            );
            frame.grid.put_text_clipped(
                cursor,
                rect.y,
                &input.input[input.cursor..],
                st(GitRole::Text, surface),
                rect.right().saturating_sub(reserved),
            );
            // A modal screen's query box: its Visual selection, and a block cursor out of
            // Insert mode, as a form's box shows them.
            let (highlight, block) = app.filter_box_marks();
            if let Some(range) = highlight {
                let cut = shown != before;
                let visible = if cut {
                    input.cursor - (shown.len() - '…'.len_utf8())
                } else {
                    0
                };
                let (start, end) = (range.start.max(visible), range.end.min(input.input.len()));
                if start < end {
                    let at = x + u16::from(cut) + chrome::width(&input.input[visible..start]);
                    frame.grid.put_text_clipped(
                        at,
                        rect.y,
                        &input.input[start..end],
                        crate::ui::Style::Selection,
                        rect.right().saturating_sub(reserved),
                    );
                }
            }
            frame.grid.put_text_clipped(
                rect.right().saturating_sub(chrome::width(help) + 1),
                rect.y,
                help,
                st(GitRole::Muted, surface),
                rect.right(),
            );
            let shape = if block {
                crossterm::cursor::SetCursorStyle::SteadyBlock
            } else {
                crossterm::cursor::SetCursorStyle::SteadyBar
            };
            frame.cursor = Some((cursor, rect.y, shape));
            return;
        }
    }
    let grid = &mut frame.grid;
    if app.form.is_some() {
        let commit = app
            .form
            .as_ref()
            .is_some_and(|form| matches!(form.action, super::FormAction::Commit));
        // A modal screen's box: Esc leaves Insert mode first, then closes the form.
        let typing = app.input_mode() == Some(super::modal::InputMode::Insert);
        let visual = app.input_mode() == Some(super::modal::InputMode::Visual);
        // The sheet prints its own keys (Enter or Ctrl+Enter, Tab, Esc, and a Visual
        // selection's verbs); this bar keeps the mode chip and what the sheet does not say,
        // so no key is printed twice.
        let mut hints: Vec<(&str, &str)> = Vec::new();
        if commit && !visual && !sheet::commit_keys_beside_toggles(rect) {
            // Only show the toggles' keys here when they do not fit beside the boxes.
            hints.push(("M-s M-a M-n", "sign-off · amend · skip hooks"));
        }
        if typing && !commit {
            // Other forms still need to explain how to leave Insert mode.
            hints.push(("Esc", "normal mode"));
        }
        // A refusal answering this form stays visible beside it.
        let message = (!app.busy && !app.notice.is_empty()).then_some((app.notice.as_str(), true));
        chrome::footer(grid, rect, &hints, "", message);
        return;
    }
    let message = if app.busy {
        Some((app.busy_line(), false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), looks_like_error(&app.notice)))
    } else {
        None
    };
    let staged = app.preview_staged();
    let marked = app.file_selection_context() && !app.file_selection.marked.is_empty();
    let hints: Vec<(&str, &str)> = if app.focus == 4 && app.preview_anchor.is_some() {
        vec![
            (
                "a",
                if staged {
                    "unstage range"
                } else {
                    "stage range"
                },
            ),
            ("d", "discard"),
            ("Esc", "cancel"),
        ]
    } else if app.focus == 4 && app.last_list == 0 && app.preview.patch {
        vec![
            if staged {
                ("u", "unstage hunk")
            } else {
                ("s", "stage hunk")
            },
            // A verb on every hint.
            ("a", if staged { "unstage line" } else { "stage line" }),
            ("V", "range"),
            ("d", "discard"),
            ("|", "split"),
            ("e", "editor"),
        ]
    } else if app.focus == 4 && app.last_list == 2 {
        // The commit reader: a commit's keys, not the worktree's.
        vec![
            ("m", "message"),
            ("y", "copy"),
            ("o", "open on forge"),
            ("e", "editor"),
            ("/", "find"),
            ("Esc", "back"),
        ]
    } else if app.commit_open {
        // The commit's file list: nothing here stages, unstages or discards.
        vec![
            ("Enter", "read"),
            ("m", "message"),
            ("y", "copy"),
            ("o", "open on forge"),
            ("/", "filter"),
            ("Esc", "back"),
        ]
    } else if app.focus == 4 {
        // `e` opens the line under the cursor in the editor.
        vec![
            ("/", "find"),
            ("e", "editor"),
            ("|", "split"),
            ("Esc", "back"),
        ]
    } else if marked {
        // `x` first: beside a long refusal the row ends at the first hint that does not
        // fit, and marking is what is left to do then.
        vec![
            ("x", "mark"),
            ("s", "stage marked"),
            ("u", "unstage"),
            ("V", "range"),
            ("Esc", "clear"),
        ]
    } else {
        match (app.focus, app.files_view, app.subview, app.history_view) {
            (0, 1, ..) => vec![("Enter", "open"), ("n", "new worktree"), ("d", "remove")],
            (0, 2, ..) => vec![
                ("s", "pointer"),
                ("Enter", "open"),
                ("u", "update"),
                ("-", "toggle"),
            ],
            // Commit before lazygit's `a` (stage or unstage all): a narrow footer keeps the
            // first ones. `/` is on `?`.
            (0, ..) => vec![
                ("s", "stage"),
                ("u", "unstage"),
                ("c", "commit"),
                ("a", "all"),
                ("v", "lines"),
                ("d", "discard"),
            ],
            (1, _, 2, _) => vec![("Enter", "open"), ("f", "fetch"), ("/", "find")],
            (1, _, 3, _) => vec![
                ("Enter", "inspect"),
                ("d", "delete"),
                ("P", "push"),
                ("/", "find"),
            ],
            (1, ..) => vec![
                ("Enter", "checkout"),
                ("n", "new"),
                // `o` (the branch's pull request, or the sheet that opens one) is in
                // the help sheet: the footer holds five actions.
                ("m", "merge"),
                ("R", "rebase"),
                ("d", "delete"),
            ],
            (2, _, _, 1) => vec![("n", "rescue branch"), ("o", "detach"), ("Enter", "files")],
            (2, ..) => vec![
                ("Enter", "files"),
                ("W", "reword"),
                ("s", "squash"),
                ("d", "drop"),
                ("J K", "move"),
            ],
            _ => vec![("a", "apply"), ("p", "pop"), ("d", "drop"), ("n", "save")],
        }
    };
    // The keys in force: the lazygit preset shows its own key for each action.
    let shown = app.github.key_table().show_all(app.key_panel(), &hints);
    // On a conflict the footer says its pick keys (`conflicts.rs`), already in force.
    let hints: Vec<(&str, &str)> = match app.conflict_hints() {
        Some(conflict) => conflict,
        None => shown
            .iter()
            .map(|(keys, label)| (keys.as_str(), *label))
            .collect(),
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
    [
        "error", "fatal", "cannot", "failed", "refuse", "invalid", "not ", "nothing",
    ]
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
        assert_eq!(
            fit_path("a/long_file_name.py", 8),
            (String::new(), "long_fi…".into())
        );
    }
}
