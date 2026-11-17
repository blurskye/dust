//! Rail panes: Changes, Branches, History and Stashes.
use super::super::GitScreen;
use super::chrome::{self, Span, bold, st};
use super::glyphs::{Glyph, glyph, lane};
use super::layout::Rect;
use super::{fit_path, surface_for, syntax};
use crate::git::GitFileStatus;
use crate::ui::{Emphasis, Frame, GitChip, GitRole, GitSurface, Style};
use crossterm::cursor::SetCursorStyle;

pub(super) fn paint(app: &mut GitScreen, frame: &mut Frame, pane: usize, rect: Rect) {
    if rect.w < 8 || rect.h < 2 {
        return;
    }
    if pane == 0 && app.last_list == 2 && app.commit.is_some() {
        commit_files(app, frame, rect);
        return;
    }
    let operation_modal = app
        .form
        .as_ref()
        .is_some_and(|form| matches!(&form.action, super::super::FormAction::Operation { .. }));
    let focused = app.focus == pane && !operation_modal;
    let mode = app.icon_mode;
    let rows = app.rows(pane);
    let filter = &app.filters[app.slot(pane)];
    let filtering = filter.active() || filter.editing;
    let query = filter.input.input.clone();

    let (title_glyph, title) = match pane {
        0 if app.all_files => (Glyph::ChangesPane, "Files".to_owned()),
        0 if app.files_view == 1 => (Glyph::ChangesPane, "Worktrees".into()),
        0 if app.files_view == 2 => (Glyph::ChangesPane, "Submodules".into()),
        0 => (Glyph::ChangesPane, "Changes".into()),
        1 => (Glyph::Branch, "Branches".into()),
        2 if app.history_view == 1 => (Glyph::HistoryPane, "Reflog".into()),
        2 => (
            Glyph::HistoryPane,
            app.history_target
                .as_ref()
                .filter(|(_, reference)| reference != "HEAD")
                .map_or_else(
                    || "History".into(),
                    |(_, reference)| format!("History · {}", reference.to_string_lossy()),
                ),
        ),
        _ => (Glyph::Stash, "Stashes".into()),
    };
    let loading = pane == 2
        && (app.history_view == 0 && app.history_loading
            || app.history_view == 1 && app.reflog_loading);
    let mut count = if loading {
        "…".to_owned()
    } else {
        match pane {
            0 if app.files_view == 0 && !app.all_files => app
                .snapshot
                .as_ref()
                .map_or_else(|| "…".into(), |snapshot| snapshot.entries.len().to_string()),
            1 if app.subview == 0 => app
                .refs
                .as_ref()
                .map_or(0, |refs| refs.branches.iter().filter(|b| !b.remote).count())
                .to_string(),
            _ => rows.iter().filter(|row| !row.directory).count().to_string(),
        }
    };
    if pane == 0 && app.file_selection_context() && !app.file_selection.marked.is_empty() {
        count = format!("{count} · {} marked", app.file_selection.marked.len());
    } else if pane == 0 && app.files_view == 0 && !app.changes_flat() && !app.all_files {
        count = format!("{count} · tree");
    }
    // The working tree's total, as GitHub's `7 files changed, +21 −5`: `7 · +21 −5`.
    if pane == 0 && app.files_view == 0 && !app.all_files {
        let (added, removed) = app
            .snapshot
            .iter()
            .flat_map(|snapshot| &snapshot.entries)
            .filter_map(|entry| app.numstat.get(&entry.relative_path).copied().flatten())
            .fold((0u32, 0u32), |(a, r), (x, y)| (a + x, r + y));
        if added > 0 || removed > 0 {
            count = format!("{count} · {}", diffstat(added, removed));
        }
    }
    if filtering {
        count = format!(
            "/{query} · {}",
            rows.iter().filter(|row| !row.directory).count()
        );
    }
    let title_spans: Vec<Span> = vec![
        (
            format!("{} ", glyph(title_glyph, mode)),
            st(GitRole::Text, GitSurface::Pane),
        ),
        (title, bold(GitRole::Text, GitSurface::Pane)),
    ];
    chrome::pane(&mut frame.grid, rect, &title_spans, &count, focused);
    // An empty pane collapses to title, one body row and its bottom border: its
    // `No stashes` is body text below, never written on a border.
    if rect.h <= 2 {
        return;
    }
    let inner = rect.inner();
    let mut list = inner;
    let tabs = match pane {
        0 => {
            let worktrees = app.refs.as_ref().map_or(0, |refs| refs.worktrees.len());
            let submodules = app
                .refs
                .as_ref()
                .and_then(|refs| refs.submodules.as_ref().ok())
                .map_or(0, Vec::len);
            (worktrees > 1 || submodules > 0 || app.files_view != 0).then(|| {
                (
                    {
                        // Counts first, then zero counts, then long names give way.
                        let label = |name: &str, count: usize, zero: bool| {
                            if count > 0 || zero {
                                format!("{name} {count}")
                            } else {
                                name.to_owned()
                            }
                        };
                        let set = |trees: &str, subs: &str, zero: bool| {
                            vec![
                                "Changes".to_owned(),
                                label(trees, worktrees, zero),
                                label(subs, submodules, zero),
                            ]
                        };
                        fit_tabs(
                            &[
                                set("Worktrees", "Submodules", true),
                                set("Worktrees", "Submodules", false),
                                set("Worktrees", "Subs", false),
                                set("Trees", "Submodules", false),
                                set("Trees", "Subs", false),
                                vec!["Changes".into(), "Trees".into(), "Subs".into()],
                            ],
                            inner.w,
                        )
                    },
                    app.files_view,
                )
            })
        }
        1 => {
            let remotes = app
                .refs
                .as_ref()
                .map_or(0, |refs| refs.branches.iter().filter(|b| b.remote).count());
            let locals = app
                .refs
                .as_ref()
                .map_or(0, |refs| refs.branches.iter().filter(|b| !b.remote).count());
            Some((
                fit_tabs(
                    &[
                        // Counts on every tab or on none.
                        vec![
                            format!("Local {locals}"),
                            format!("Remotes {remotes}"),
                            format!("Tags {}", app.tags.len()),
                        ],
                        vec!["Local".into(), "Remotes".into(), "Tags".into()],
                    ],
                    inner.w,
                ),
                match app.subview {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                },
            ))
        }
        _ => None,
    };
    if let Some((tabs, active)) = tabs
        && list.h > 1
    {
        chrome::sub_tabs(
            &mut frame.grid,
            inner.x,
            inner.y,
            inner.right(),
            &tabs,
            active,
            focused,
        );
        list.y += 1;
        list.h -= 1;
    }
    let slot = app.slot(pane);
    let selected = app.selected[slot];
    let visible = usize::from(list.h);
    if selected < app.offsets[pane] {
        app.offsets[pane] = selected;
    }
    if visible > 0 && selected >= app.offsets[pane] + visible {
        app.offsets[pane] = selected + 1 - visible;
    }
    let offset = app.offsets[pane];
    if rows.is_empty() {
        let text = if filter.active() {
            "No matches · / edit"
        } else if pane == 0 && app.files_view != 0 && app.refs.is_none()
            || pane == 0 && app.refreshing && app.snapshot.is_none()
            || loading
        {
            "Loading…"
        } else if pane == 0 && app.files_view == 1 {
            "No worktrees"
        } else if pane == 0 && app.files_view == 2 {
            app.refs
                .as_ref()
                .and_then(|refs| refs.submodules.as_ref().err())
                .map_or("No submodules", String::as_str)
        } else if pane == 0 && app.changes_collapsed {
            "l / Enter expand"
        } else if pane == 2 && app.history_view == 1 && app.reflog_error.is_some() {
            "Reflog unavailable"
        } else if pane == 2 && app.history_view == 1 {
            "No reflog entries"
        } else {
            [
                "Working tree clean",
                "No references",
                "No commits yet",
                "No stashes",
            ][pane]
        };
        frame.grid.put_text_clipped(
            list.x + 2,
            list.y,
            text,
            st(GitRole::Muted, GitSurface::Pane),
            list.right(),
        );
        // No cursor on the note: the focused border already says where keys go, and a lone
        // block on "Working tree clean" reads as a glitch (as on the forge tabs' empty lists).
        return;
    }
    let graph_width = if pane == 2 && app.history_view == 0 {
        app.history_graph
            .iter()
            .skip(offset)
            .take(visible)
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0)
            .min(10) as u16
    } else {
        0
    };
    for (index, row) in rows.iter().enumerate().skip(offset).take(visible) {
        let y = list.y + (index - offset) as u16;
        let is_cursor = index == selected;
        let surface = surface_for(is_cursor, focused);
        chrome::fill(
            &mut frame.grid,
            Rect::new(list.x, y, list.w, 1),
            st(GitRole::Text, surface),
        );
        if is_cursor && focused {
            frame.grid.set_cell(
                list.x,
                y,
                glyph(Glyph::Cursor, mode),
                bold(GitRole::Focus, surface),
            );
            frame.cursor = Some((list.x, y, SetCursorStyle::SteadyBar));
        }
        let row_rect = Rect::new(list.x, y, list.w, 1);
        match pane {
            0 if app.files_view == 0 => change_row(app, frame, row, row_rect, surface),
            1 if app.subview == 0 => branch_row(app, frame, row, row_rect, surface),
            1 => {
                let (mark, role) = if app.subview == 3 {
                    (glyph(Glyph::Tag, mode), GitRole::Tag)
                } else {
                    (glyph(Glyph::Remote, mode), GitRole::RemoteRef)
                };
                let x = list.x + 2 + row.indent as u16;
                let x = frame.grid.put_text_clipped(
                    x,
                    y,
                    &format!("{mark} "),
                    st(role, surface),
                    list.right(),
                );
                let label = row.label.trim_start();
                // A remote row reads `origin · 3 branches`; its URL lives in the reader.
                if app.subview == 2
                    && let Some(name) = remote_name(label)
                {
                    let prefix = format!("{name}/");
                    let branches = app.refs.as_ref().map_or(0, |refs| {
                        refs.branches
                            .iter()
                            .filter(|branch| branch.remote && branch.name.starts_with(&prefix))
                            .count()
                    });
                    let x = frame.grid.put_text_clipped(
                        x,
                        y,
                        name,
                        bold(GitRole::RemoteRef, surface),
                        list.right(),
                    );
                    frame.grid.put_text_clipped(
                        x,
                        y,
                        &format!(
                            " · {branches} branch{}",
                            if branches == 1 { "" } else { "es" }
                        ),
                        st(GitRole::Muted, surface),
                        list.right(),
                    );
                    continue;
                }
                frame.grid.put_text_clipped(
                    x,
                    y,
                    &chrome::ellipsize(label, usize::from(list.right().saturating_sub(x + 1))),
                    if row.directory {
                        bold(GitRole::Muted, surface)
                    } else {
                        st(GitRole::Text, surface)
                    },
                    list.right(),
                );
            }
            2 if app.history_view == 0 => {
                history_row(app, frame, index, row_rect, surface, graph_width)
            }
            3 => stash_row(app, frame, row, row_rect, surface),
            _ => {
                frame.grid.put_text_clipped(
                    list.x + 2,
                    y,
                    &chrome::ellipsize(&row.label, usize::from(list.w.saturating_sub(3))),
                    st(GitRole::Text, surface),
                    list.right(),
                );
            }
        }
    }
    // A scroll position on the bottom border, only when the list overflows.
    if rows.len() > visible && visible > 0 {
        let label = format!(
            " {}–{}/{} ",
            offset + 1,
            (offset + visible).min(rows.len()),
            rows.len()
        );
        let at = rect.right().saturating_sub(chrome::width(&label) + 2);
        frame.grid.put_text_clipped(
            at,
            rect.bottom() - 1,
            &label,
            st(GitRole::Muted, GitSurface::Pane),
            rect.right() - 1,
        );
    }
}

fn change_row(
    app: &GitScreen,
    frame: &mut Frame,
    row: &super::super::Row,
    rect: Rect,
    surface: GitSurface,
) {
    let mode = app.icon_mode;
    let grid = &mut frame.grid;
    let y = rect.y;
    let right = rect.right();
    let indent = if app.changes_flat() {
        0
    } else {
        row.indent as u16
    };
    if row.directory {
        let label = row.label.trim_start();
        let (fold, name) = label.split_at(label.find(' ').unwrap_or(0));
        let x = rect.x + 7 + indent;
        let x = grid.put_text_clipped(
            x.saturating_sub(2),
            y,
            &format!(
                "{} ",
                if fold.contains('▸') {
                    glyph(Glyph::FoldClosed, mode)
                } else {
                    glyph(Glyph::FoldOpen, mode)
                }
            ),
            st(GitRole::Muted, surface),
            right,
        );
        let marked = app.file_selection_context()
            && app
                .file_selection
                .marked
                .iter()
                .any(|path| path.starts_with(&row.value));
        // Aggregated `+n −m` and the staged marker of everything below this directory.
        let below = app.snapshot.as_ref().map_or(&[][..], |snapshot| {
            super::super::memo::below(&snapshot.entries, &row.value)
        });
        let staged = below.iter().filter(|entry| entry.sources.staged).count();
        if marked {
            grid.set_cell(
                rect.x + 1,
                y,
                glyph(Glyph::Mark, mode),
                bold(GitRole::Focus, surface),
            );
        } else if staged > 0 {
            let (mark, role) =
                if staged == below.len() && below.iter().all(|entry| !entry.sources.unstaged) {
                    (glyph(Glyph::Mark, mode), GitRole::Added)
                } else {
                    (glyph(Glyph::Partial, mode), GitRole::Modified)
                };
            grid.set_cell(rect.x + 1, y, mark, st(role, surface));
        }
        let (added, removed) = below
            .iter()
            .filter_map(|entry| app.numstat.get(&entry.relative_path).copied().flatten())
            .fold((0, 0), |(a, r), (x, y)| (a + x, r + y));
        let mut name_right = right;
        if rect.w >= 30 && (added > 0 || removed > 0) {
            let spans: Vec<Span> = vec![
                (format!("+{added}"), st(GitRole::Added, surface)),
                (" ".into(), st(GitRole::Text, surface)),
                (format!("−{removed}"), st(GitRole::Removed, surface)),
            ];
            let at = right.saturating_sub(chrome::spans_width(&spans) + 1);
            chrome::put_spans(grid, at, y, &spans, right);
            name_right = at.saturating_sub(1);
        }
        grid.put_text_clipped(
            x,
            y,
            &chrome::ellipsize(name.trim(), usize::from(name_right.saturating_sub(x + 1))),
            bold(GitRole::Muted, surface),
            name_right,
        );
        return;
    }
    let entry = app
        .snapshot
        .as_ref()
        .and_then(|snapshot| super::super::memo::entry(&snapshot.entries, &row.value));
    let module = app
        .refs
        .as_ref()
        .and_then(|refs| refs.submodules.as_ref().ok())
        .and_then(|modules| modules.iter().find(|module| module.path == row.value));
    let code = module
        .map(|module| app.submodule_change_code(module))
        .or_else(|| entry.map(|entry| entry.change_code()))
        .unwrap_or_else(|| "  ".into());
    let conflicted = entry.is_some_and(|entry| entry.status == GitFileStatus::Conflicted);
    let untracked = entry.is_some_and(|entry| entry.status == GitFileStatus::Untracked);
    let staged = entry.is_some_and(|entry| entry.sources.staged);
    let unstaged = entry.is_some_and(|entry| entry.sources.unstaged)
        || module.is_some_and(|module| {
            module.dirty || module.initialized && module.head != module.index
        });
    // Column 1: the multi-mark wins over the partly-staged marker.
    let marked = app.file_selection_context() && app.file_selection.marked.contains(&row.value);
    if marked {
        grid.set_cell(
            rect.x + 1,
            y,
            glyph(Glyph::Mark, mode),
            bold(GitRole::Focus, surface),
        );
    } else if staged && unstaged && !conflicted {
        grid.set_cell(
            rect.x + 1,
            y,
            glyph(Glyph::Partial, mode),
            st(GitRole::Modified, surface),
        );
    }
    // Columns 2–3: X (index) and Y (worktree), exactly `git status --short`.
    let mut letters = code.chars();
    let x_letter = letters.next().unwrap_or(' ');
    let y_letter = letters.next().unwrap_or(' ');
    let paint_letter = |letter: char, index_column: bool| -> Style {
        let role = match letter {
            'U' => return bold(GitRole::Conflict, surface),
            _ if conflicted => return bold(GitRole::Conflict, surface),
            '?' => GitRole::Untracked,
            '!' => GitRole::Muted,
            'D' if !index_column => GitRole::Removed,
            _ if index_column => GitRole::Added,
            _ => GitRole::Modified,
        };
        st(role, surface)
    };
    grid.set_cell(
        rect.x + 2,
        y,
        &x_letter.to_string(),
        paint_letter(x_letter, true),
    );
    grid.set_cell(
        rect.x + 3,
        y,
        &y_letter.to_string(),
        paint_letter(y_letter, false),
    );
    // Columns 5–6: the file-type glyph.
    let (icon, icon_group) = super::glyphs::file(&row.value, mode);
    let icon_x = rect.x + 5 + indent;
    if icon_x < right {
        grid.put_text_clipped(icon_x, y, icon, syntax(icon_group, surface), right);
    }
    // Right 9: `+n −m`.
    let counts = (rect.w >= 30)
        .then(|| app.numstat.get(&row.value))
        .flatten();
    let mut path_right = right;
    if let Some(counts) = counts {
        let spans: Vec<Span> = match counts {
            Some((added, removed)) => vec![
                (format!("+{added}"), st(GitRole::Added, surface)),
                (" ".into(), st(GitRole::Text, surface)),
                (format!("−{removed}"), st(GitRole::Removed, surface)),
            ],
            None => vec![("bin".into(), st(GitRole::Muted, surface))],
        };
        let at = right.saturating_sub(chrome::spans_width(&spans) + 1);
        chrome::put_spans(grid, at, y, &spans, right);
        // The path may use every cell the counts leave free.
        path_right = at.saturating_sub(1);
    }
    // Columns 7..: muted directory, file name in the row's state colour.
    let name_role = if conflicted {
        GitRole::Conflict
    } else if untracked {
        GitRole::Untracked
    } else if staged && unstaged {
        GitRole::Modified
    } else if staged {
        GitRole::Added
    } else {
        GitRole::Text
    };
    let emphasis = if conflicted {
        Emphasis::Bold
    } else {
        Emphasis::None
    };
    let path_x = rect.x + 7 + indent;
    let label = if app.changes_flat() {
        row.value.display().to_string()
    } else {
        row.value
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (dir, name) = fit_path(&label, usize::from(path_right.saturating_sub(path_x + 1)));
    let x = grid.put_text_clipped(path_x, y, &dir, st(GitRole::Muted, surface), path_right);
    grid.put_text_clipped(
        x,
        y,
        &name,
        Style::Git {
            role: name_role,
            surface,
            emphasis,
        },
        path_right,
    );
}

fn branch_row(
    app: &GitScreen,
    frame: &mut Frame,
    row: &super::super::Row,
    rect: Rect,
    surface: GitSurface,
) {
    let mode = app.icon_mode;
    let grid = &mut frame.grid;
    let y = rect.y;
    let right = rect.right();
    let name = row.value.to_string_lossy();
    let entry = app.refs.as_ref().and_then(|refs| {
        refs.branches
            .iter()
            .find(|branch| !branch.remote && branch.name == name)
    });
    let head = entry.is_some_and(|branch| branch.head) || name == app.branch;
    if head {
        grid.set_cell(
            rect.x + 2,
            y,
            glyph(Glyph::CommitHead, mode),
            bold(GitRole::Added, surface),
        );
    }
    grid.set_cell(
        rect.x + 4,
        y,
        glyph(Glyph::Branch, mode),
        st(GitRole::Ref, surface),
    );
    let mut tail: Vec<Span> = Vec::new();
    if let Some(branch) = entry {
        if branch.upstream.is_some() && (branch.ahead > 0 || branch.behind > 0 || head) {
            let quiet = branch.ahead == 0 && branch.behind == 0;
            tail.push((
                format!("{}{}", glyph(Glyph::Ahead, mode), branch.ahead),
                st(
                    if quiet || branch.ahead == 0 {
                        GitRole::Muted
                    } else {
                        GitRole::Added
                    },
                    surface,
                ),
            ));
            tail.push((" ".into(), st(GitRole::Text, surface)));
            tail.push((
                format!("{}{}", glyph(Glyph::Behind, mode), branch.behind),
                st(
                    if quiet || branch.behind == 0 {
                        GitRole::Muted
                    } else {
                        GitRole::Removed
                    },
                    surface,
                ),
            ));
        } else if branch.upstream.is_none() {
            tail.push(("local".into(), st(GitRole::Muted, surface)));
        }
        if branch.checked_out.is_some() && !head {
            tail.push((" ".into(), st(GitRole::Text, surface)));
            tail.push((
                " wt ".into(),
                st(GitRole::Modified, GitSurface::Chip(GitChip::Modified)),
            ));
        }
    }
    let tail_width = chrome::spans_width(&tail);
    let name_right = right.saturating_sub(tail_width + 2);
    grid.put_text_clipped(
        rect.x + 6,
        y,
        &chrome::ellipsize(&name, usize::from(name_right.saturating_sub(rect.x + 6))),
        if head {
            bold(GitRole::Ref, surface)
        } else {
            st(GitRole::Text, surface)
        },
        name_right,
    );
    chrome::put_spans(grid, right.saturating_sub(tail_width + 1), y, &tail, right);
}

fn history_row(
    app: &GitScreen,
    frame: &mut Frame,
    index: usize,
    rect: Rect,
    surface: GitSurface,
    graph_width: u16,
) {
    let mode = app.icon_mode;
    let grid = &mut frame.grid;
    let y = rect.y;
    let right = rect.right();
    let Some((id, subject)) = app.history.get(index) else {
        return;
    };
    let info = app.history_info.get(index);
    let head = info.is_some_and(|info| {
        info.refs
            .iter()
            .any(|reference| reference == "HEAD" || reference.starts_with("HEAD -> "))
    });
    let merge = info.is_some_and(|info| info.parents.len() > 1);
    let mut x = rect.x + 2;
    if let Some(graph) = app.history_graph.get(index) {
        for (cell_index, cell) in graph
            .cells
            .iter()
            .enumerate()
            .take(usize::from(graph_width))
        {
            let at = x + cell_index as u16;
            let Some((symbol, colour)) = *cell else {
                continue;
            };
            let clipped = cell_index + 1 == usize::from(graph_width)
                && graph.cells.len() > usize::from(graph_width);
            let text = if clipped {
                lane('…', mode)
            } else if symbol == super::graph::NODE {
                glyph(
                    if head {
                        Glyph::CommitHead
                    } else if merge {
                        Glyph::CommitMerge
                    } else {
                        Glyph::Commit
                    },
                    mode,
                )
            } else {
                lane(symbol, mode)
            };
            let role = if clipped {
                GitRole::Muted
            } else {
                GitRole::Lane(colour % 5)
            };
            let style = if symbol == super::graph::NODE && head {
                bold(GitRole::Focus, surface)
            } else {
                st(role, surface)
            };
            grid.set_cell(at, y, text, style);
        }
        x += graph_width + 1;
    }
    x = grid.put_text_clipped(
        x,
        y,
        &id[..id.len().min(7)],
        st(GitRole::Hash, surface),
        right,
    );
    x += 1;
    // Ref chips, decided before the age column: a branch tip matters more than a date.
    let chips = info.map_or_else(Vec::new, |info| ref_chips(app, &info.refs));
    // Wide rails get `AK Nov 12` on the right, muted, like the design's age column.
    let mut right = right;
    if rect.w >= 46
        && let Some(info) = info
    {
        let initials = info
            .author
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase();
        let tail = format!(
            "{initials} {}",
            chrome::when(&info.date, crate::forge::records::now())
        );
        let at = right.saturating_sub(chrome::width(&tail) + 1);
        if at > x + 14 {
            grid.put_text_clipped(at, y, &tail, st(GitRole::Muted, surface), right);
            right = at.saturating_sub(1);
        }
    }
    // Ref chips while the subject keeps at least 12 cells.
    for (text, hue) in &chips {
        let chip_width = chrome::width(text).min(14) + 2;
        if x + chip_width + 1 + 12 > right {
            break;
        }
        let text = chrome::ellipsize(text, 14);
        x = chrome::chip(grid, x, y, right, &text, *hue) + 1;
    }
    grid.put_text_clipped(
        x,
        y,
        &chrome::ellipsize(subject, usize::from(right.saturating_sub(x + 1))),
        if head {
            bold(GitRole::Text, surface)
        } else {
            st(GitRole::Text, surface)
        },
        right.saturating_sub(1),
    );
}

/// `%D`'s names as History's chips: tags, branches, and remote branches in their own hue.
/// HEAD alone is the node's glyph, not a chip.
fn ref_chips(app: &GitScreen, refs: &[String]) -> Vec<(String, GitChip)> {
    let mode = app.icon_mode;
    refs.iter()
        .filter_map(|reference| {
            Some(if let Some(tag) = reference.strip_prefix("tag: ") {
                (format!("{} {tag}", glyph(Glyph::Tag, mode)), GitChip::Tag)
            } else if let Some(branch) = reference.strip_prefix("HEAD -> ") {
                (branch.to_owned(), GitChip::Ref)
            } else if reference == "HEAD" {
                return None;
            } else if reference.contains('/')
                && app.remotes.iter().any(|remote| {
                    remote
                        .split_whitespace()
                        .next()
                        .is_some_and(|name| reference.starts_with(&format!("{name}/")))
                })
            {
                (reference.clone(), GitChip::RemoteRef)
            } else {
                (reference.clone(), GitChip::Ref)
            })
        })
        .collect()
}

fn stash_row(
    app: &GitScreen,
    frame: &mut Frame,
    row: &super::super::Row,
    rect: Rect,
    surface: GitSurface,
) {
    let mode = app.icon_mode;
    let grid = &mut frame.grid;
    let y = rect.y;
    let right = rect.right();
    // `stash@{0}: On fix/backoff: WIP reconnect`
    let mut parts = row.label.splitn(3, ": ");
    let reference = parts.next().unwrap_or_default();
    let (branch, message) = match (parts.next(), parts.next()) {
        (Some(on), Some(message)) => (
            on.trim_start_matches("WIP on ")
                .trim_start_matches("On ")
                .to_owned(),
            message.to_owned(),
        ),
        (Some(message), None) => (String::new(), message.to_owned()),
        _ => (String::new(), String::new()),
    };
    let index = reference
        .strip_prefix("stash@")
        .unwrap_or(reference)
        .to_owned();
    let x = grid.put_text_clipped(
        rect.x + 2,
        y,
        &format!("{} ", glyph(Glyph::Stash, mode)),
        st(GitRole::Muted, surface),
        right,
    );
    let x = grid.put_text_clipped(x, y, &index, st(GitRole::Hash, surface), right) + 1;
    // The branch is dropped before the message is cut.
    let room = usize::from(right.saturating_sub(x + 1));
    let branch = if chrome::width(&message) as usize + chrome::width(&branch) as usize + 2 > room {
        String::new()
    } else {
        branch
    };
    let branch_width = if branch.is_empty() {
        0
    } else {
        chrome::width(&branch).min(14) + 1
    };
    let message_right = right.saturating_sub(branch_width + 1);
    grid.put_text_clipped(
        x,
        y,
        &chrome::ellipsize(&message, usize::from(message_right.saturating_sub(x))),
        st(GitRole::Text, surface),
        message_right,
    );
    if !branch.is_empty() {
        grid.put_text_clipped(
            right.saturating_sub(branch_width),
            y,
            &chrome::ellipsize_start(&branch, 14),
            st(GitRole::Ref, surface),
            right,
        );
    }
}

/// G-COMMITFILES in the rail: the files of the commit History selected, as a tree with
/// the same anatomy as Changes minus the XY columns.
fn commit_files(app: &GitScreen, frame: &mut Frame, rect: Rect) {
    let Some(commit) = &app.commit else { return };
    let mode = app.icon_mode;
    let focused = app.focus == 0;
    let filtering = commit.filter.active() || commit.filter.editing;
    let rows = commit.rows();
    let count = if commit.filter.active() {
        format!(
            "/{} · {}/{}",
            commit.filter.input.input,
            rows.iter().filter(|row| !row.directory).count(),
            commit.files.len()
        )
    } else {
        commit.files.len().to_string()
    };
    let title: Vec<Span> = vec![
        (
            format!("{} ", glyph(Glyph::Commit, mode)),
            st(GitRole::Hash, GitSurface::Pane),
        ),
        // The hash lives once, in the reader's commit header (R15-06).
        ("Commit files".into(), bold(GitRole::Text, GitSurface::Pane)),
        (
            if filtering && !commit.filter.active() {
                " /"
            } else {
                ""
            }
            .into(),
            st(GitRole::Focus, GitSurface::Pane),
        ),
    ];
    chrome::pane(&mut frame.grid, rect, &title, &count, focused);
    if rect.h <= 2 {
        return;
    }
    let list = rect.inner();
    let visible = usize::from(list.h).max(1);
    let offset = commit.cursor.saturating_sub(visible - 1);
    if rows.is_empty() {
        let text = if commit.filter.active() {
            "No matching files"
        } else if commit.message == "Loading commit…" {
            "Loading commit…"
        } else {
            "No changed files"
        };
        frame.grid.put_text_clipped(
            list.x + 2,
            list.y,
            text,
            st(GitRole::Muted, GitSurface::Pane),
            list.right(),
        );
        if focused {
            frame.cursor = Some((list.x + 2, list.y, SetCursorStyle::SteadyBlock));
        }
        return;
    }
    for (index, row) in rows.iter().enumerate().skip(offset).take(visible) {
        let y = list.y + (index - offset) as u16;
        let is_cursor = index == commit.cursor;
        let surface = surface_for(is_cursor, focused);
        let right = list.right();
        chrome::fill(
            &mut frame.grid,
            Rect::new(list.x, y, list.w, 1),
            st(GitRole::Text, surface),
        );
        if is_cursor && focused {
            frame.grid.set_cell(
                list.x,
                y,
                glyph(Glyph::Cursor, mode),
                bold(GitRole::Focus, surface),
            );
            frame.cursor = Some((list.x, y, SetCursorStyle::SteadyBar));
        }
        let indent = row.indent as u16;
        let label = row.label.trim_start();
        if row.directory {
            let (fold, name) = label.split_at(label.find(' ').unwrap_or(0));
            let x = frame.grid.put_text_clipped(
                list.x + 2 + indent,
                y,
                &format!(
                    "{} ",
                    if fold.contains('▸') {
                        glyph(Glyph::FoldClosed, mode)
                    } else {
                        glyph(Glyph::FoldOpen, mode)
                    }
                ),
                st(GitRole::Muted, surface),
                right,
            );
            frame.grid.put_text_clipped(
                x,
                y,
                &chrome::ellipsize(name.trim(), usize::from(right.saturating_sub(x + 1))),
                bold(GitRole::Muted, surface),
                right,
            );
            continue;
        }
        let file = commit.file(&row.value);
        let mut path_right = right;
        if let Some(file) = file {
            let spans: Vec<Span> = if file.added == "-" {
                vec![("bin".into(), st(GitRole::Muted, surface))]
            } else {
                vec![
                    (format!("+{}", file.added), st(GitRole::Added, surface)),
                    (" ".into(), st(GitRole::Text, surface)),
                    (format!("−{}", file.removed), st(GitRole::Removed, surface)),
                ]
            };
            let at = right.saturating_sub(chrome::spans_width(&spans) + 1);
            chrome::put_spans(&mut frame.grid, at, y, &spans, right);
            path_right = at.saturating_sub(1);
        }
        let (icon, group) = super::glyphs::file(&row.value, mode);
        let x = list.x + 2 + indent;
        frame
            .grid
            .put_text_clipped(x, y, icon, syntax(group, surface), path_right);
        let name = row
            .value
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        frame.grid.put_text_clipped(
            x + 2,
            y,
            &chrome::ellipsize(&name, usize::from(path_right.saturating_sub(x + 3))),
            if is_cursor {
                bold(GitRole::Text, surface)
            } else {
                st(GitRole::Text, surface)
            },
            path_right,
        );
    }
    if rows.len() > visible {
        let label = format!(
            " {}–{}/{} ",
            offset + 1,
            (offset + visible).min(rows.len()),
            rows.len()
        );
        let at = rect.right().saturating_sub(chrome::width(&label) + 2);
        frame.grid.put_text_clipped(
            at,
            rect.bottom() - 1,
            &label,
            st(GitRole::Muted, GitSurface::Pane),
            rect.right() - 1,
        );
    }
}

/// The first label set whose ` a │ b │ c ` strip fits `room` cells, else the last.
fn fit_tabs(variants: &[Vec<String>], room: u16) -> Vec<String> {
    let strip = |tabs: &Vec<String>| {
        tabs.iter().map(|tab| chrome::width(tab) + 2).sum::<u16>()
            + tabs.len().saturating_sub(1) as u16
    };
    variants
        .iter()
        .find(|tabs| strip(tabs) <= room)
        .or(variants.last())
        .cloned()
        .unwrap_or_default()
}

/// `+21 −5`, without the side that is zero: `+3` for a pure addition.
fn diffstat(added: u32, removed: u32) -> String {
    match (added, removed) {
        (_, 0) => format!("+{added}"),
        (0, _) => format!("−{removed}"),
        _ => format!("+{added} −{removed}"),
    }
}

/// The name of a Remotes row, which reads `name  url`: a URL, an scp-style address or a local
/// path, Windows ones (`D:\repos\origin`) too.
fn remote_name(label: &str) -> Option<&str> {
    label
        .split_once("  ")
        .filter(|(_, url)| url.contains("://") || url.contains(['@', '/', '\\']))
        .map(|(name, _)| name)
}

#[cfg(test)]
mod remote_row_tests {
    #[test]
    fn a_remote_row_is_named_whatever_its_url_looks_like() {
        for url in [
            "https://example.com/team/app.git",
            "git@example.com:team/app.git",
            "/srv/git/app.git",
            "D:\\a\\_temp\\practice\\origin.git",
        ] {
            assert_eq!(
                super::remote_name(&format!("origin  {url}")),
                Some("origin"),
                "{url}"
            );
        }
        assert_eq!(super::remote_name("origin"), None);
    }
}
