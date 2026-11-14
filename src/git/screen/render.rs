use super::GitScreen;
use crate::ui::{CellGrid, Frame, Style};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn border(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    title: &str,
    focused: bool,
) {
    if w < 2 || h < 2 {
        return;
    }
    let style = if focused {
        Style::Accent
    } else {
        Style::Border
    };
    for col in x..x + w {
        grid.set_cell(col, y, "─", style);
        grid.set_cell(col, y + h - 1, "─", style);
    }
    for row in y..y + h {
        grid.set_cell(x, row, "│", style);
        grid.set_cell(x + w - 1, row, "│", style);
    }
    for (col, row, text) in [
        (x, y, "┌"),
        (x + w - 1, y, "┐"),
        (x, y + h - 1, "└"),
        (x + w - 1, y + h - 1, "┘"),
    ] {
        grid.set_cell(col, row, text, style);
    }
    grid.put_text_clipped(
        x + 2,
        y,
        &ellipsize(title, usize::from(w.saturating_sub(4))),
        if focused {
            Style::GitControl
        } else {
            Style::GitHeading
        },
        x + w - 2,
    );
}

pub(super) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if width < 64 || height < 20 {
        frame.grid.put_text_clipped(
            0,
            0,
            "Dusty Git needs at least 64 columns × 20 rows",
            Style::Warning,
            width,
        );
        return frame;
    }
    let operation_modal = app
        .form
        .as_ref()
        .is_some_and(|form| matches!(&form.action, super::FormAction::Operation { .. }));
    navigation(&mut frame, 0, width, &app.project);
    let left = (width * 3 / 10).clamp(26, 52);
    let body = height - 4 - if app.tutor.is_some() { 4 } else { 0 };
    // Pane rectangles depend only on terminal height, never on their contents.
    let extra = body.saturating_sub(15);
    let mut sizes = [4, 3, 3, 0].map(|weight| 3 + extra * weight / 10);
    if body >= 16 {
        sizes[0] = sizes[0].max(4);
    }
    sizes[1] += body.saturating_sub(3 + sizes.iter().sum::<u16>());
    for y in 1..body + 1 {
        for x in 0..left {
            frame.grid.set_cell(x, y, " ", Style::GitSurface(None));
        }
    }
    for x in 0..width {
        frame.grid.set_cell(x, 1, "─", Style::Border);
    }
    let other_worktree = app.refs.as_ref().is_some_and(|refs| {
        refs.branches
            .iter()
            .any(|branch| branch.name == app.branch && branch.checked_out.is_some())
    });
    let head_right = if other_worktree { left - 6 } else { left - 1 };
    // The row states what this checkout stands on: its branch, a paused replay or
    // a detach, with real divergence counted on the right. The project name is
    // already the window's own title, so it is not repeated here.
    let (ahead, behind) = app.divergence();
    let marks = format!(
        "{}{}",
        if ahead == 0 {
            String::new()
        } else {
            format!("↑{ahead} ")
        },
        if behind == 0 {
            String::new()
        } else {
            format!("↓{behind}")
        }
    );
    let marks_right = head_right.saturating_sub(marks.width() as u16);
    frame.grid.put_text_clipped(
        2,
        2,
        &ellipsize(app.head_label(), usize::from(marks_right.saturating_sub(3))),
        Style::GitHeading,
        marks_right,
    );
    if !marks.is_empty() {
        frame
            .grid
            .put_text_clipped(marks_right, 2, &marks, Style::GitSignAdded, head_right);
    }
    if other_worktree {
        frame
            .grid
            .put_text_clipped(left - 5, 2, " wt ", Style::Warning, left - 1);
    }
    let files_filtered = app.files_view == 0
        && (app.filters[app.slot(0)].active() || app.filters[app.slot(0)].editing);
    // The active tab's chip carries the filter mark itself, once.
    let file_label = format!(
        "{} {}",
        if app.changes_collapsed { "▸" } else { "▾" },
        if app.all_files { "Files" } else { "Changes" },
    );
    let files_tabs = [
        file_label.as_str(),
        if left < 30 { "WT" } else { "Worktrees" },
        if left < 42 { "Submod" } else { "Submodules" },
    ];
    let refs_tabs = ["Branches", "Remotes", "Tags"];
    let history_tabs = ["Commits", "Reflog"];
    // A section's title is the label the sidebar needs when it cannot draw tabs:
    // the reference a history list is pinned to, or the plain pane name.
    let titles = [
        files_tabs.join(" "),
        refs_tabs.join(" "),
        app.history_target
            .as_ref()
            .filter(|(_, reference)| reference != "HEAD")
            .map(|(_, reference)| format!("Commits · {}", reference.to_string_lossy()))
            .unwrap_or_else(|| "Commits Reflog".into()),
        "Stashes".into(),
    ];
    let mut top = 4;
    for pane in 0..4 {
        let h = sizes[pane];
        if pane == 0 && app.last_list == 2 && app.commit.is_some() {
            super::commit::files(&mut frame, app, left, top, h);
            if let Some(commit) = &app.commit {
                commit_files_heading(&mut frame.grid, commit, left, top, app.focus == 0);
            }
            top += h;
            continue;
        }
        let filter = &app.filters[app.slot(pane)];
        let title = if (filter.active() || filter.editing) && !(pane == 0 && files_filtered) {
            format!("{} /", titles[pane])
        } else {
            titles[pane].clone()
        };
        section(&mut frame.grid, 0, top, left, h, &title, app.focus == pane);
        let focused = app.focus == pane && !operation_modal;
        let rows = app.rows(pane);
        let count = if filter.active() && pane == 0 {
            rows.iter().filter(|row| !row.directory).count()
        } else if pane == 0 && app.files_view == 0 && app.all_files {
            app.files.len()
        } else if pane == 0 && app.files_view == 0 {
            app.snapshot
                .as_ref()
                .map(|snapshot| snapshot.entries.len())
                .unwrap_or(0)
        } else {
            app.rows(pane).len()
        };
        let count = if pane == 2
            && (app.history_view == 0 && app.history_loading
                || app.history_view == 1 && app.reflog_loading)
        {
            "…".into()
        } else {
            count.to_string()
        };
        // The count owns the right end of the heading row, so tabs and context
        // both stop before it instead of drawing over the one live number. It
        // earns those cells only while the names beside it keep real room, and it
        // is painted last so the number always survives.
        let count_x = left - 2 - count.len() as u16;
        let count_drawn = title.len() + count.len() + 5 <= usize::from(left);
        let heading_right = if count_drawn { count_x } else { left - 2 };
        let active_tab = match pane {
            0 => Some((files_tabs.as_slice(), app.files_view)),
            1 => Some((
                refs_tabs.as_slice(),
                if app.subview == 0 { 0 } else { app.subview - 1 },
            )),
            2 => Some((history_tabs.as_slice(), app.history_view)),
            _ => None,
        };
        if let Some((tabs, selected)) = active_tab {
            // Tabs are the section's map, not two of its words: every view keeps
            // its own chip, the active one filled, the rest quiet.
            // A heading is one quiet rail: chips sit on the pane's own surface, so
            // the space between two of them is never a leftover rule.
            for col in 2..left - 2 {
                frame
                    .grid
                    .set_cell(col, top, " ", surface(app.focus == pane, None));
            }
            let filtered_here = if pane == 0 {
                files_filtered
            } else {
                filter.active() || filter.editing
            };
            // The row reads exactly like the joined label it replaces; only the
            // colouring differs per view, so nothing about the text moves.
            let mut labels = tabs
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            if filtered_here {
                labels[selected].push_str(" /");
            }
            let span = labels.iter().map(|label| label.width()).sum::<usize>();
            let mut x = 2;
            if span + 2 <= usize::from(heading_right.saturating_sub(2)) {
                for (index, label) in labels.iter().enumerate() {
                    if index > 0 {
                        x = frame.grid.put_text_clipped(
                            x,
                            top,
                            " ",
                            surface(app.focus == pane, None),
                            heading_right,
                        );
                    }
                    x = frame.grid.put_text_clipped(
                        x,
                        top,
                        label,
                        Style::GitTab(index == selected),
                        heading_right,
                    );
                }
            } else {
                // Too tight for separate chips: keep every view named in one
                // line and mark the active one in place, never a short form.
                let label = ellipsize(&title, usize::from(heading_right.saturating_sub(3)));
                x = frame
                    .grid
                    .put_text_clipped(2, top, &label, Style::GitHeading, heading_right);
                let before = 2 + labels[..selected]
                    .iter()
                    .map(|label| label.width() as u16 + 1)
                    .sum::<u16>();
                let active = labels[selected].as_str();
                if before + active.width() as u16 <= heading_right {
                    frame.grid.put_text_clipped(
                        before,
                        top,
                        active,
                        Style::GitTab(true),
                        heading_right,
                    );
                    x = x.max(before + active.width() as u16);
                }
            }
            // History names the reference it is pinned to; the branch is already
            // the row above, so the branches list does not repeat it.
            let context = if pane == 2 {
                app.history_target
                    .as_ref()
                    .filter(|(_, reference)| reference != "HEAD")
                    .map(|(_, reference)| reference.to_string_lossy().into_owned())
            } else {
                None
            };
            if let Some(context) = context {
                let room = usize::from(heading_right.saturating_sub(x + 2));
                if room >= 8 {
                    let text = ellipsize(&context, room);
                    let at = heading_right.saturating_sub(1 + text.width() as u16).max(x);
                    frame
                        .grid
                        .put_text_clipped(at, top, &text, Style::Muted, heading_right);
                } else if x <= 2 {
                    // No room beside the tabs: where HEAD is outranks which tab is
                    // open, so the context becomes the heading itself.
                    let text = ellipsize(&context, usize::from(heading_right.saturating_sub(2)));
                    frame
                        .grid
                        .put_text_clipped(2, top, &text, Style::GitHeading, heading_right);
                }
            }
        }
        if count_drawn {
            frame.grid.put_text_clipped(
                count_x,
                top,
                &count,
                surface(focused, Some(crate::config::Highlight::COMMENT)),
                left - 1,
            );
        }
        let slot = app.slot(pane);
        let selected = app.selected[slot];
        let visible = h.saturating_sub(2) as usize;
        if selected < app.offsets[pane] {
            app.offsets[pane] = selected;
        }
        if selected >= app.offsets[pane] + visible {
            app.offsets[pane] = selected.saturating_sub(visible.saturating_sub(1));
        }
        for (index, row) in rows
            .iter()
            .enumerate()
            .skip(app.offsets[pane])
            .take(visible)
        {
            let y = top + 1 + (index - app.offsets[pane]) as u16;
            let selection = (index == selected).then_some(focused);
            let entry = (pane == 0 && matches!(app.files_view, 0 | 2) && !row.directory)
                .then(|| {
                    app.snapshot
                        .as_ref()?
                        .entries
                        .iter()
                        .find(|entry| entry.relative_path == row.value)
                })
                .flatten();
            let submodule = (pane == 0 && matches!(app.files_view, 0 | 2) && !row.directory)
                .then(|| {
                    app.refs
                        .as_ref()?
                        .submodules
                        .as_ref()
                        .ok()?
                        .iter()
                        .find(|module| module.path == row.value)
                })
                .flatten();
            let child_unstaged = submodule.is_some_and(|module| {
                module.dirty || module.initialized && module.head != module.index
            });
            let file_status = (pane == 0 && matches!(app.files_view, 0 | 2) && !row.directory)
                .then(|| {
                    let code = submodule
                        .map(|module| app.submodule_change_code(module))
                        .or_else(|| entry.map(|entry| entry.change_code()))
                        .unwrap_or_else(|| "  ".into());
                    let staged = entry.is_some_and(|entry| entry.sources.staged);
                    let unstaged =
                        child_unstaged || entry.is_some_and(|entry| entry.sources.unstaged);
                    let marker = match (staged, unstaged) {
                        _ if app.file_selection_context()
                            && app.file_selection.marked.contains(&row.value) =>
                        {
                            '*'
                        }
                        (true, true) => '◐',
                        (true, false) => '✓',
                        _ if submodule.is_some_and(|module| !module.initialized) => '−',
                        _ => ' ',
                    };
                    let label = if app.files_view == 2 {
                        row.label.chars().skip(5).collect::<String>()
                    } else {
                        row.value
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    };
                    (marker, code, label)
                });
            let style = if file_status.is_some() {
                Style::GitFile {
                    staged: entry
                        .map(|entry| {
                            entry.sources.staged && !entry.sources.unstaged && !child_unstaged
                        })
                        .or_else(|| child_unstaged.then_some(false)),
                    selection,
                    focused,
                }
            } else if index == selected {
                if focused {
                    Style::GitSelected(None)
                } else {
                    Style::GitInactiveSelection(None)
                }
            } else {
                surface(
                    focused,
                    row.directory.then_some(crate::config::Highlight::COMMENT),
                )
            };
            if index == selected {
                for x in 1..left - 1 {
                    frame.grid.set_cell(x, y, " ", style);
                }
            }
            if index == selected {
                frame.grid.set_cell(
                    0,
                    y,
                    "▎",
                    if app.focus == pane {
                        Style::Accent
                    } else {
                        Style::Border
                    },
                );
            }
            let worktree = pane == 1
                && app.subview == 0
                && app.refs.as_ref().is_some_and(|refs| {
                    refs.branches.iter().any(|branch| {
                        std::path::Path::new(&branch.name) == row.value
                            && branch.checked_out.is_some()
                    })
                });
            let row_right = if worktree { left - 6 } else { left - 1 };
            // Marker, X, Y and filename have fixed cells in both Changes and
            // Submodules. Never trim XY or repaint it over a preformatted label.
            let icon = file_status
                .as_ref()
                .map(|_| file_glyph(&row.value, app.icon_mode));
            let marked_descendants = if pane == 0 && row.directory && app.file_selection_context() {
                app.file_selection
                    .marked
                    .iter()
                    .filter(|path| path.starts_with(&row.value))
                    .count()
            } else {
                0
            };
            let label = file_status.as_ref().map_or_else(
                || {
                    if marked_descendants == 0 {
                        row.label.clone()
                    } else {
                        format!("{} [{marked_descendants}*]", row.label)
                    }
                },
                |(marker, code, label)| {
                    format!(
                        "{}{marker} {code} {} {label}",
                        " ".repeat(row.indent),
                        icon.unwrap().0
                    )
                },
            );
            frame.grid.put_text_clipped(
                2,
                y,
                &ellipsize(&label, usize::from(row_right - 2)),
                style,
                row_right,
            );
            if let Some((glyph, role)) = icon {
                let icon_x = 7 + row.indent as u16;
                if icon_x + glyph.width() as u16 <= row_right {
                    frame.grid.put_text_clipped(
                        icon_x,
                        y,
                        glyph,
                        match selection {
                            Some(true) => Style::GitSelected(Some(role)),
                            Some(false) => Style::GitInactiveSelection(Some(role)),
                            None => surface(focused, Some(role)),
                        },
                        row_right,
                    );
                }
            }
            if worktree {
                frame
                    .grid
                    .put_text_clipped(left - 4, y, "wt", Style::Warning, left - 1);
            }
            if pane == 2 {
                frame.grid.put_text_clipped(
                    2,
                    y,
                    row.label.split(' ').next().unwrap_or(""),
                    Style::GitHash(selection, focused),
                    left - 1,
                );
            } else if pane == 1 && app.subview == 0 {
                let current = row.value == std::path::Path::new(&app.branch);
                frame.grid.put_text_clipped(
                    2,
                    y,
                    &ellipsize(&row.label, usize::from(row_right - 2)),
                    Style::GitBranch {
                        current,
                        selection,
                        focused,
                    },
                    row_right,
                );
            } else if let Some((marker, code, _)) = &file_status {
                // Color X independently from Y, including a dirty submodule whose
                // commit pointer is already staged. Bound overlays to this pane.
                for (column, code) in code.chars().take(2).enumerate() {
                    let color = if matches!(code, 'D' | 'U') {
                        Style::GitDetail {
                            added: Some(false),
                            selection,
                            focused,
                        }
                    } else if column == 0 && code != ' ' && code != '?' {
                        Style::GitDetail {
                            added: Some(true),
                            selection,
                            focused,
                        }
                    } else {
                        style
                    };
                    let x = 4 + row.indent as u16 + column as u16;
                    if x < row_right {
                        frame.grid.set_cell(x, y, &code.to_string(), color);
                    }
                }
                let x = 2 + row.indent as u16;
                if matches!(*marker, '✓' | '*') && x < row_right {
                    frame.grid.set_cell(
                        x,
                        y,
                        &marker.to_string(),
                        if *marker == '*' {
                            Style::GitControl
                        } else {
                            Style::GitDetail {
                                added: Some(true),
                                selection,
                                focused,
                            }
                        },
                    );
                }
            } else if marked_descendants != 0 {
                // The space after the fold arrow is reserved; keep the arrow,
                // filename, status columns and native cursor exactly where they were.
                let x = 3 + row.indent as u16;
                if x < row_right {
                    frame.grid.set_cell(x, y, "*", Style::GitControl);
                }
            }
            if index == selected && focused {
                frame.cursor = Some((
                    1 + row.indent.min(usize::from(left - 3)) as u16,
                    y,
                    SetCursorStyle::SteadyBar,
                ));
            }
        }
        if rows.is_empty() {
            frame.grid.put_text_clipped(
                2,
                top + 1,
                if filter.active() {
                    "No matches · / edit"
                } else if pane == 0 && app.files_view != 0 && app.refs.is_none() {
                    "Loading…"
                } else if pane == 0 && app.files_view == 1 {
                    "No worktrees"
                } else if pane == 0 && app.files_view == 2 {
                    app.refs
                        .as_ref()
                        .and_then(|refs| refs.submodules.as_ref().err())
                        .map(String::as_str)
                        .unwrap_or("No submodules")
                } else if pane == 0 && app.changes_collapsed {
                    "l / Enter expand"
                } else if pane == 0 && app.refreshing
                    || pane == 2
                        && (app.history_view == 0 && app.history_loading
                            || app.history_view == 1 && app.reflog_loading)
                {
                    "Loading…"
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
                },
                surface(focused, Some(crate::config::Highlight::COMMENT)),
                left - 1,
            );
            if app.focus == pane {
                frame.cursor = Some((2, top + 1, SetCursorStyle::SteadyBlock));
            }
        }
        viewport(
            &mut frame,
            left - 2,
            top + h - 1,
            app.offsets[pane],
            visible,
            rows.len(),
        );
        top += h;
    }
    let title = match app.last_list {
        0 => {
            if app.files_view == 1 {
                "Worktrees"
            } else if app.files_view == 2 {
                "Submodules"
            } else if app.all_files {
                "Source"
            } else if app.preview_staged() {
                "Index"
            } else {
                "Changes"
            }
        }
        1 => ["Branches", "Worktrees", "Remotes", "Tags"][app.subview],
        2 => "Commit",
        _ => "Stash",
    };
    let mixed_title = if app.last_list == 0
        && app.files_view == 0
        && !app.all_files
        && !app.refreshing
        && !app.jobs.preview_loading()
        && selected_status(app).is_some_and(|entry| {
            entry.status != super::GitFileStatus::Conflicted
                && entry.sources.staged
                && entry.sources.unstaged
        }) {
        Some(if app.preview_staged() {
            "Index · unstaged remains"
        } else {
            "Changes · staged edits"
        })
    } else {
        None
    };
    panel(
        &mut frame.grid,
        left,
        1,
        width - left,
        body,
        if app.commit.is_some() {
            ""
        } else {
            mixed_title.unwrap_or(title)
        },
        app.focus == 4,
    );
    if app.last_list == 1
        && let Some(context) = &app.ref_preview
    {
        paint_heading(
            &mut frame.grid,
            left + 2,
            1,
            width - 1,
            &ref_preview_heading(context, usize::from(width - left - 4), width < 100),
            Style::Muted,
        );
    }
    let split = app.split_diff && app.preview.patch && width - left >= 96;
    let pinned_heading = app.pinned_preview_heading();
    app.preview_split = split;
    app.normalize_preview_cursor();
    let active_hunk = if app.focus == 4
        && app.last_list == 0
        && app.files_view == 0
        && !app.all_files
        && app.preview.patch
        && app.preview_anchor.is_none()
        && !app.jobs.preview_loading()
    {
        app.preview_cursor
            .and_then(|cursor| app.preview.hunk_rows(cursor))
    } else {
        None
    };
    app.preview_rows = (if split {
        app.preview.split.len()
    } else {
        app.preview.rows.len()
    })
    .saturating_sub(usize::from(pinned_heading.is_some()));
    let mut first_line = 2;
    if let Some(commit) = &app.commit {
        let reflog_title = (app.history_view == 1)
            .then(|| app.selected_row().map(|row| row.label))
            .flatten();
        let lines = crate::ui::render::wrap_plain_text(
            reflog_title
                .as_deref()
                .unwrap_or_else(|| commit.message.lines().next().unwrap_or_default()),
            usize::from(width - left - 4),
            crate::config::AmbiguousWidth::Narrow,
        );
        for (index, line) in lines.iter().take(2).enumerate() {
            let text = if index == 1 && lines.len() > 2 {
                format!(
                    "{}  [m: more]",
                    ellipsize(
                        &format!("{line} …"),
                        usize::from(width - left - 4).saturating_sub(11)
                    )
                )
            } else {
                line.clone()
            };
            frame
                .grid
                .put_text_clipped(left + 2, first_line, &text, Style::GitHeading, width - 2);
            first_line += 1;
        }
        let meta = commit_metadata(commit, usize::from(width - left - 4));
        frame.grid.put_text_clipped(
            left + 2,
            first_line,
            &ellipsize(&meta, usize::from(width - left - 4)),
            Style::Muted,
            width - 2,
        );
        // The real commit identity is the anchor; dates remain secondary.
        frame.grid.put_text_clipped(
            left + 2,
            first_line,
            &commit.id[..7],
            Style::Accent,
            width - 2,
        );
        if let Some(reference) = meta
            .strip_prefix(&format!("{} · ", &commit.id[..7]))
            .and_then(|rest| rest.strip_prefix("HEAD→"))
            .map(|rest| rest.split(" · ").next().unwrap_or(rest))
        {
            frame.grid.put_text_clipped(
                left + 12,
                first_line,
                &format!("HEAD→{reference}"),
                Style::GitSignAdded,
                width - 2,
            );
        }
        first_line += 1;
        for x in left + 1..width {
            frame.grid.set_cell(x, first_line, "─", Style::Border);
        }
        first_line += 1;
        if let Some(file) = commit.files.get(commit.selected) {
            let counts = if file.added == "-" {
                "binary".into()
            } else {
                format!("+{} −{}", file.added, file.removed)
            };
            let stats = format!(
                " {counts} · {}/{} ",
                commit.selected + 1,
                commit.files.len()
            );
            let stats_x = (width - 2)
                .saturating_sub(stats.width() as u16)
                .max(left + 2);
            for x in left + 2..width - 2 {
                frame.grid.set_cell(x, first_line, " ", Style::GitHeading);
            }
            frame.grid.put_text_clipped(
                left + 2,
                first_line,
                &ellipsize(
                    &file.path.display().to_string(),
                    usize::from(stats_x.saturating_sub(left + 3)),
                ),
                Style::GitHeading,
                stats_x,
            );
            frame
                .grid
                .put_text_clipped(stats_x, first_line, &stats, Style::GitHeading, width - 2);
            first_line += 1;
        }
    } else {
        let summary = if app.last_list == 0 && app.files_view != 0 {
            app.selected_row()
                .map(|row| row.value.display().to_string())
                .unwrap_or_default()
        } else if app.last_list == 0 {
            changes_summary(app)
        } else {
            app.selected_row()
                .map(|row| row.value.display().to_string())
                .unwrap_or_default()
        };
        let summary = if app.jobs.preview_loading() || app.refreshing {
            format!("{summary} · updating…")
        } else {
            summary
        };
        frame.grid.put_text_clipped(
            left + 2,
            3,
            &ellipsize(&summary, usize::from(width - left - 4)),
            Style::Muted,
            width - 1,
        );
        first_line = 4;
        if let Some(index) = pinned_heading {
            for x in left + 1..width {
                frame.grid.set_cell(x, first_line, "─", Style::Border);
            }
            first_line += 1;
            paint_source(
                &mut frame.grid,
                &app.preview.rows[index],
                left + 2,
                first_line,
                width - 1,
                0,
                SourceMark::default(),
            );
            first_line += 1;
        }
    }
    if split {
        first_line += 1;
    }
    let visible = body.saturating_sub(first_line) as usize;
    app.preview_visible = visible;
    app.preview_offset = app
        .preview_offset
        .min(app.preview_rows.saturating_sub(visible));
    if let Some(cursor) = app
        .preview_cursor
        .and_then(|index| app.preview.display_position(index, split, pinned_heading))
    {
        if cursor < app.preview_offset {
            app.preview_offset = cursor;
        } else if cursor >= app.preview_offset + visible {
            app.preview_offset = cursor.saturating_sub(visible.saturating_sub(1));
        }
    }
    let offset = app.preview_offset;
    let number_width = app.preview.number_width();
    let source_layout = SourceLayout::new(width, left, number_width, app.preview.patch);
    let code_width = if split {
        usize::from((width - left) / 2).saturating_sub(number_width + 7)
    } else {
        app.preview_cursor
            .and_then(|index| app.preview.rows.get(index))
            .map_or(0, |row| source_layout.code_width(row))
    };
    // Leave one cell for the right-truncation mark: it must not overwrite a
    // one-grapheme match (and its real cursor) at the viewport's right edge.
    app.reveal_source_match(code_width.saturating_sub(1));
    if split {
        let middle = left + (width - left) / 2;
        frame
            .grid
            .put_text_clipped(left + 2, first_line - 1, "BEFORE", Style::Muted, middle);
        frame
            .grid
            .put_text_clipped(middle + 2, first_line - 1, "AFTER", Style::Muted, width - 1);
        for (i, pair) in app
            .preview
            .split
            .iter()
            .filter(|pair| pinned_heading.is_none() || pair.heading != pinned_heading)
            .enumerate()
            .skip(offset)
            .take(visible)
        {
            let y = first_line + (i - offset) as u16;
            if let Some(index) = pair.heading {
                let cursor_x = paint_source(
                    &mut frame.grid,
                    &app.preview.rows[index],
                    left + 2,
                    y,
                    width - 1,
                    if app.commit.is_some() {
                        0
                    } else {
                        app.preview_column
                    },
                    SourceMark {
                        hunk_heading: active_hunk
                            .as_ref()
                            .is_some_and(|range| range.start == index),
                        ..SourceMark::default()
                    },
                );
                if app.focus == 4 && app.preview_cursor == Some(index) {
                    frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
                }
                continue;
            }
            frame.grid.set_cell(middle, y, "│", Style::Border);
            for (index, x, right, old) in [
                (pair.old, left + 2, middle, true),
                (pair.new, middle + 2, width - 1, false),
            ] {
                if let Some(index) = index {
                    let row = &app.preview.rows[index];
                    let number = if old { row.old } else { row.new }
                        .map(|n| n.to_string())
                        .unwrap_or_default();
                    let x = frame.grid.put_text_clipped(
                        x,
                        y,
                        &format!("{number:>number_width$} │ "),
                        number_style(
                            row,
                            active_hunk
                                .as_ref()
                                .is_some_and(|range| range.contains(&index)),
                        ),
                        right,
                    );
                    let x = frame.grid.put_text_clipped(
                        x,
                        y,
                        &row.marker.unwrap_or(' ').to_string(),
                        row.style,
                        right,
                    );
                    let cursor_x = paint_source(
                        &mut frame.grid,
                        row,
                        x,
                        y,
                        right,
                        app.preview_column,
                        SourceMark::new(app, index),
                    );
                    if app.focus == 4 && app.preview_cursor == Some(index) {
                        frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
                    }
                }
            }
        }
    } else {
        for (i, (index, row)) in app
            .preview
            .rows
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != pinned_heading)
            .enumerate()
            .skip(offset)
            .take(visible)
        {
            let y = first_line + (i - offset) as u16;
            let x = source_layout.paint_gutter(
                &mut frame.grid,
                row,
                y,
                active_hunk
                    .as_ref()
                    .is_some_and(|range| range.contains(&index)),
            );
            let cursor_x = paint_source(
                &mut frame.grid,
                row,
                x,
                y,
                source_layout.right,
                if app.commit.is_some() && row.marker.is_none() {
                    0
                } else {
                    app.preview_column
                },
                SourceMark {
                    hunk_heading: active_hunk
                        .as_ref()
                        .is_some_and(|range| range.start == index),
                    ..SourceMark::new(app, index)
                },
            );
            if app
                .preview_anchor
                .zip(app.preview_cursor)
                .is_some_and(|(anchor, cursor)| {
                    index >= anchor.min(cursor)
                        && index <= anchor.max(cursor)
                        && row.marker.is_some()
                })
            {
                // In compact mode the gutter starts beside the existing divider.
                // Mark the range on that divider, never over its line number.
                let edge = if source_layout.compact {
                    left
                } else {
                    left + 1
                };
                frame.grid.set_cell(edge, y, "▎", Style::Accent);
            }
            if app.focus == 4 && app.preview_cursor == Some(index) {
                frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
            }
        }
    }
    viewport(
        &mut frame,
        width - 2,
        body,
        offset,
        visible,
        app.preview_rows,
    );
    if app.focus == 4 && frame.cursor.is_none() {
        frame.cursor = Some((left + 2, first_line, SetCursorStyle::SteadyBlock));
    }
    if let Some(tutor) = &app.tutor {
        frame
            .grid
            .put_text_clipped(1, height - 7, &tutor.title(), Style::Accent, width - 2);
        let lines = crate::ui::render::wrap_plain_text(
            tutor.instruction(),
            usize::from(width - 2),
            crate::config::AmbiguousWidth::Narrow,
        );
        for (index, line) in lines.iter().take(2).enumerate() {
            frame.grid.put_text_clipped(
                1,
                height - 6 + index as u16,
                line,
                Style::Default,
                width - 2,
            );
        }
        frame.grid.put_text_clipped(
            1,
            height - 4,
            "Disposable repository · :tutor quit restores your workspace",
            Style::Muted,
            width - 2,
        );
    }
    let selection_summary = if app.file_selection_context() && !app.file_selection.marked.is_empty()
    {
        app.file_selection_summary()
    } else {
        String::new()
    };
    let operation_summary = app.operation_bar.as_ref().map_or_else(String::new, |bar| {
        if let Some(replay) = &app.paused_replay
            && bar.operation == "rebase"
        {
            let phase = if width < 100 {
                let count = bar.blockers.len();
                format!(
                    "rebase: {count} conflict{}",
                    if count == 1 { "" } else { "s" }
                )
            } else {
                bar.phase.clone()
            };
            format!(
                "{phase} · replay {} {}",
                &replay.id.to_string()[..7],
                replay.subject
            )
        } else {
            bar.phase.clone()
        }
    });
    if !operation_modal {
        frame.grid.put_text_clipped(
            1,
            height - 3,
            if app.busy {
                "Git operation running…"
            } else if app.notice.is_empty() && selection_summary.is_empty() {
                &operation_summary
            } else if app.notice.is_empty() {
                &selection_summary
            } else {
                &app.notice
            },
            Style::Warning,
            width - 1,
        );
    }
    let commit_hints = [
        ("j/k", if app.focus == 4 { "scroll" } else { "files" }),
        ("/", if app.focus == 4 { "find" } else { "filter" }),
        ("Esc", "back"),
        ("Tab", "focus"),
        ("J/K", "files"),
        ("m", "message"),
        ("A/W/F", "amend/reword/fixup"),
        ("h/l", if app.focus == 4 { "pan" } else { "fold" }),
        ("s", "split"),
    ];
    let mut range_hints = vec![
        (
            "a",
            if app.preview_staged() {
                "unstage range"
            } else {
                "stage range"
            },
        ),
        ("Esc", "cancel"),
        ("j/k", "extend"),
        ("V", "range"),
        ("i", "layer"),
        ("/", "find"),
        ("s", "split"),
    ];
    if !app.preview_staged() {
        range_hints.insert(1, ("d/D", "discard"));
    }
    let staged_hints = [
        ("Space", if width < 90 { "hunk" } else { "unstage hunk" }),
        ("a", if width < 90 { "line" } else { "unstage line" }),
        ("V", "range"),
        ("i", "working tree"),
        ("/", "find"),
        ("j/k", "code"),
        ("s", "split"),
    ];
    let working_hints = [
        ("Space", if width < 90 { "hunk" } else { "stage hunk" }),
        ("a", if width < 90 { "line" } else { "stage line" }),
        ("d/D", "discard"),
        ("V", "range"),
        ("i", "layer"),
        ("/", "find"),
        ("j/k", "code"),
        ("s", "split"),
        ("Esc", "back"),
    ];
    let file_action = file_stage_hint(app);
    let all_file_hints = [
        ("v", "changes"),
        ("Space", file_action),
        ("/", "filter"),
        ("H/L", "tabs"),
        ("j/k", "move"),
        ("h/l", "fold"),
        ("i", "index"),
        ("Tab", "focus"),
    ];
    let file_hints = [
        ("Space", file_action),
        ("c", "commit"),
        ("i", "index"),
        ("/", "filter"),
        ("x", "mark"),
        ("V", "range"),
    ];
    let marked_action = format!("toggle {} marked", app.file_selection.marked.len());
    let marked_hints = [
        ("Space", marked_action.as_str()),
        ("x", "mark"),
        (
            "V",
            if app.file_selection.active() {
                "finish"
            } else {
                "range"
            },
        ),
        ("Esc", "clear"),
        (
            "j/k",
            if app.file_selection.active() {
                "extend"
            } else {
                "move"
            },
        ),
        ("/", "filter"),
        ("h/l", "fold"),
        ("H/L", "tabs"),
        ("Tab", "focus"),
    ];
    let mut operation_hints = Vec::new();
    if let Some(bar) = &app.operation_bar {
        if bar.offers(super::GitOperationAction::Continue) {
            operation_hints.push(("C", "continue"));
        }
        if bar.offers(super::GitOperationAction::Skip) {
            operation_hints.push(("S", "skip commit"));
        }
        operation_hints.push(("X", "abort"));
        if app.last_list == 0 && app.files_view == 0 {
            operation_hints.push(("o/t", "choose side"));
            operation_hints.push((
                "Space",
                if app.focus == 4 {
                    "stage hunk"
                } else if !app.file_selection.marked.is_empty() {
                    "toggle marked"
                } else {
                    "stage file"
                },
            ));
        } else {
            operation_hints.push(("1", "files"));
        }
        operation_hints.push(("Tab", "focus"));
    }
    if !operation_modal {
        footer(
            &mut frame,
            width,
            height,
            if !operation_hints.is_empty() {
                &operation_hints
            } else if app.commit_open || (app.focus == 4 && app.last_list == 2) {
                &commit_hints
            } else if app.focus == 2 && app.history_view == 1 {
                &[
                    ("n", "rescue branch"),
                    ("Space", "detach"),
                    ("w", "worktree"),
                    ("Enter", "files"),
                    ("H/L", "tabs"),
                    ("/", "filter"),
                    ("c", "commit"),
                    ("m", "message"),
                ]
            } else if app.last_list == 2 {
                &[
                    ("Enter", "files"),
                    ("m", "message"),
                    ("F", "fixup"),
                    ("A", "amend"),
                    ("W", "reword"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                    ("j/k", "move"),
                    ("h/l", "pan"),
                    ("Esc", "changes"),
                ]
            } else if app.focus == 4 && app.preview_anchor.is_some() {
                &range_hints
            } else if app.focus == 4
                && app.last_list == 0
                && app.preview.patch
                && app.preview_staged()
            {
                &staged_hints
            } else if app.focus == 4 && app.last_list == 0 && app.preview.patch {
                &working_hints
            } else if app.focus == 4 {
                &[
                    ("/", "find"),
                    ("j/k", "scroll"),
                    ("h/l", "pan"),
                    ("s", "split"),
                    ("Ctrl+h", "back"),
                    ("[ ]", "pages"),
                ]
            } else if app.last_list == 0 && app.files_view == 1 {
                &[
                    ("Enter", "open"),
                    ("H/L", "tabs"),
                    ("n", "create"),
                    ("d", "remove"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                ]
            } else if app.last_list == 0 && app.files_view == 2 {
                &[
                    ("Space", "pointer"),
                    ("Enter", "open"),
                    ("H/L", "tabs"),
                    ("u", "update"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                ]
            } else if app.focus == 0
                && app.file_selection_context()
                && !app.file_selection.marked.is_empty()
            {
                &marked_hints
            } else if app.last_list == 0 && app.all_files {
                &all_file_hints
            } else if app.last_list == 0 {
                &file_hints
            } else if app.last_list == 1 && app.subview == 2 {
                &[
                    ("H/L", "tabs"),
                    ("Enter", "open / track"),
                    ("f", "fetch"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                ]
            } else if app.last_list == 1 && app.subview == 3 {
                &[
                    ("d", "delete local"),
                    ("Enter", "inspect"),
                    ("H/L", "tabs"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                ]
            } else if app.last_list == 1 {
                &[
                    ("m", "merge"),
                    ("R", "rebase onto"),
                    ("e", "rename"),
                    ("u", "upstream"),
                    ("d", "delete"),
                    ("Enter", "checkout"),
                    ("n", "branch"),
                    ("H/L", "tabs"),
                    ("/", "filter"),
                    ("Tab", "focus"),
                ]
            } else {
                &[
                    ("/", "filter"),
                    ("j/k", "move"),
                    ("n", "save"),
                    ("a", "apply"),
                    ("p", "pop"),
                    ("d", "drop"),
                    ("Tab", "focus"),
                ]
            },
            app.commit_open,
        );
    }
    app.render_source_search(&mut frame, width, height);
    app.render_filter(&mut frame, width, height);
    forms(app, &mut frame, width, height);
    frame
}

/// Forms and the operation sheet over any Git page.
pub(super) fn forms(app: &GitScreen, frame: &mut Frame, width: u16, height: u16) {
    let operation_modal = app
        .form
        .as_ref()
        .is_some_and(|form| matches!(&form.action, super::FormAction::Operation { .. }));
    if let Some(form) = &app.form {
        if operation_modal {
            let status = if app.reflog_submitting || app.busy {
                "Running Git…"
            } else {
                &app.notice
            };
            render_operation_form(frame, form, width, height, status);
        } else {
            render_form(frame, form, width, height);
        }
        if matches!(
            &form.action,
            super::FormAction::Branch(Some(_))
                | super::FormAction::CheckoutDetached(_)
                | super::FormAction::DetachedWorktree(_)
                | super::FormAction::Reference(_)
                | super::FormAction::Discard(_)
                | super::FormAction::DiscardFiles(_)
                | super::FormAction::Rewrite(_)
        ) {
            // The minimum-size modal can cover the ordinary notice row. Keep the
            // worker state or Git refusal inside it, without replacing user input.
            let w = width.saturating_sub(6).min(90);
            let h = form_height(form, height);
            let x = (width - w) / 2;
            let y = (height - h) / 2 + h - 3;
            let status = if app.reflog_submitting {
                "Running Git…"
            } else if matches!(&form.action, super::FormAction::CheckoutDetached(_))
                && app.notice.contains("would be overwritten")
            {
                "Git refused checkout: changes would be overwritten"
            } else {
                app.notice.lines().next().unwrap_or_default()
            };
            frame
                .grid
                .put_text_clipped(x + 2, y, status, Style::Warning, x + w - 2);
        }
    }
}

// Header-only display budgeting: prepared log rows never change on resize.
pub(super) fn ref_preview_heading(
    context: &super::RefPreviewContext,
    available: usize,
    compact: bool,
) -> String {
    let head = context
        .current
        .as_ref()
        .map_or_else(|| "HEAD".into(), |name| format!("HEAD→{name}"));
    if context.selected_is_current && context.head_at_tip {
        return ellipsize(&head, available);
    }
    if !context.head_at_tip {
        let tail = " · other tip";
        return format!(
            "{}{tail}",
            ellipsize(&head, available.saturating_sub(tail.width()))
        );
    }
    let selected = if context.selected.is_empty() {
        format!("tip {}", &context.tip.to_string()[..7])
    } else {
        context.selected.clone()
    };
    let selected = if compact || head.width() + selected.width() + 3 > available {
        "selected".to_owned()
    } else {
        selected
    };
    let tail = format!(" = {selected}");
    format!(
        "{}{tail}",
        ellipsize(&head, available.saturating_sub(tail.width()))
    )
}

fn selected_status(app: &GitScreen) -> Option<&crate::git::GitStatusEntry> {
    let row = app.selected_row()?;
    app.snapshot
        .as_ref()?
        .entries
        .iter()
        .find(|entry| entry.relative_path == row.value)
}

// Counts describe the captured status snapshot, never another layer's cached patch.
fn changes_summary(app: &GitScreen) -> String {
    let Some(snapshot) = &app.snapshot else {
        return "Loading changes…".into();
    };
    let suffix = if snapshot.truncated { "+" } else { "" };
    let repo = format!("{}{suffix}", snapshot.entries.len());
    let mut summary = if app.all_files {
        format!("source · repo {repo}")
    } else {
        let staged = app.preview_staged();
        let count = snapshot
            .entries
            .iter()
            .filter(|entry| {
                if staged {
                    entry.sources.staged
                } else {
                    entry.sources.unstaged
                }
            })
            .count();
        let layer = if staged { "staged" } else { "working tree" };
        format!("{count}{suffix} {layer} · repo {repo}")
    };
    if app.filters[app.slot(0)].active() {
        let shown = app.rows(0).iter().filter(|row| !row.directory).count();
        summary.push_str(&format!(" · filter {shown}{suffix}"));
    }
    summary
}

// Mirror only the known single-file status rule used by stage(). Directory,
// unloaded and submodule cases retain an honest non-specific action label.
fn file_stage_hint(app: &GitScreen) -> &'static str {
    if app.refreshing || app.busy {
        return "stage/unstage";
    }
    let Some(row) = app.selected_row().filter(|row| !row.directory) else {
        return "stage/unstage";
    };
    if app
        .refs
        .as_ref()
        .and_then(|refs| refs.submodules.as_ref().ok())
        .is_some_and(|modules| modules.iter().any(|module| module.path == row.value))
    {
        return "pointer";
    }
    match selected_status(app) {
        Some(entry) if entry.sources.staged && !entry.sources.unstaged => "unstage file",
        Some(_) => "stage file",
        None => "stage/unstage",
    }
}

fn number_style(row: &super::preview::PreviewRow, active_hunk: bool) -> Style {
    if active_hunk {
        Style::GitControl
    } else if matches!(row.marker, Some('+' | '-')) {
        Style::GitDetail {
            added: Some(row.marker == Some('+')),
            selection: None,
            focused: false,
        }
    } else {
        Style::Muted
    }
}

// One geometry for painting unified gutters and revealing a byte-level cursor.
// Compact patches show the applicable number (old for removals, new otherwise);
// signs retain the old/new identity. Wide patches keep both columns unchanged.
struct SourceLayout {
    x: u16,
    right: u16,
    number_width: usize,
    patch: bool,
    compact: bool,
}

impl SourceLayout {
    fn new(width: u16, left: u16, number_width: usize, patch: bool) -> Self {
        let compact = width < 80;
        Self {
            x: left + if compact { 1 } else { 2 },
            right: width - if compact { 0 } else { 1 },
            number_width,
            patch,
            compact,
        }
    }

    fn numbers(&self, row: &super::preview::PreviewRow) -> String {
        if row.old.is_none() && row.new.is_none() {
            return String::new();
        }
        let width = self.number_width;
        if self.patch && self.compact {
            let number = row
                .new
                .or(row.old)
                .map(|n| n.to_string())
                .unwrap_or_default();
            // The sign is also the separator on context rows. Keep all digits,
            // but do not spend another cell between the number and that sign.
            format!("{number:>width$}")
        } else if self.patch {
            let old = row.old.map(|n| n.to_string()).unwrap_or_default();
            let new = row.new.map(|n| n.to_string()).unwrap_or_default();
            format!("{old:>width$} {new:>width$} ")
        } else {
            let new = row.new.map(|n| n.to_string()).unwrap_or_default();
            format!("{new:>width$} │ ")
        }
    }

    fn code_width(&self, row: &super::preview::PreviewRow) -> usize {
        usize::from(self.right.saturating_sub(self.x))
            .saturating_sub(self.numbers(row).width() + usize::from(row.marker.is_some()))
    }

    fn paint_gutter(
        &self,
        grid: &mut CellGrid,
        row: &super::preview::PreviewRow,
        y: u16,
        active_hunk: bool,
    ) -> u16 {
        let x = grid.put_text_clipped(
            self.x,
            y,
            &self.numbers(row),
            number_style(row, active_hunk),
            self.right,
        );
        if let Some(marker) = row.marker {
            grid.put_text_clipped(x, y, &marker.to_string(), row.style, self.right)
        } else {
            x
        }
    }
}

fn commit_metadata(commit: &super::commit::Commit, available: usize) -> String {
    let refs = if commit.decorations.is_empty() {
        String::new()
    } else {
        format!("{} · ", commit.decorations)
    };
    let full = format!(
        "{} · {refs}{} · {} · {} files",
        &commit.id[..7],
        commit.author,
        commit.date,
        commit.files.len()
    );
    if full.width() <= available {
        return full;
    }
    let prefix = format!("{} · ", &commit.id[..7]);
    let suffix = format!(" · {} · {} files", commit.date, commit.files.len());
    let room = available.saturating_sub(prefix.width() + suffix.width());
    // Narrow headers retain their existing ref-first identity. Once there is
    // room for attribution, secondary refs must not displace author and date.
    if room < 15 {
        return ellipsize(&full, available);
    }
    let relevant = commit.decorations.split(", ").next().unwrap_or_default();
    let refs = if relevant.is_empty() {
        String::new()
    } else {
        let budget = room
            .saturating_sub(commit.author.width().min(21) + 3)
            .clamp(9, room - 4);
        format!("{} · ", ellipsize(relevant, budget))
    };
    let author = ellipsize(&commit.author, room.saturating_sub(refs.width()));
    format!("{prefix}{refs}{author}{suffix}")
}

// commit::files owns the list, selection, count and cursor. Recompose only its
// heading here so historical files cannot be mistaken for worktree Changes.
fn commit_files_heading(
    grid: &mut CellGrid,
    commit: &super::commit::Commit,
    left: u16,
    top: u16,
    focused: bool,
) {
    let count = if commit.filter.active() {
        format!(
            "{}/{}",
            commit.rows().iter().filter(|row| !row.directory).count(),
            commit.files.len()
        )
    } else {
        commit.files.len().to_string()
    };
    let right = (left - 3).saturating_sub(count.len() as u16);
    for x in 2..=right {
        grid.set_cell(x, top, " ", surface(focused, None));
    }
    let separator = if right.saturating_sub(2) >= 22 {
        " · "
    } else {
        " "
    };
    let title = format!(
        "Commit {}{separator}files{}",
        &commit.id[..7],
        if commit.filter.active() || commit.filter.editing {
            " /"
        } else {
            ""
        }
    );
    grid.put_text_clipped(
        2,
        top,
        &ellipsize(&title, usize::from(right.saturating_sub(2))),
        heading_style(focused),
        right,
    );
}

fn heading_style(focused: bool) -> Style {
    if focused {
        Style::GitControl
    } else {
        Style::GitHeading
    }
}

#[derive(Default)]
pub(super) struct SourceMark<'a> {
    diff: Option<bool>,
    matches: &'a [super::search::SourceMatch],
    cursor: Option<usize>,
    hunk_heading: bool,
}
impl<'a> SourceMark<'a> {
    fn new(app: &'a GitScreen, index: usize) -> Self {
        Self {
            hunk_heading: false,
            diff: match app.preview.rows[index].marker {
                Some('+') => Some(true),
                Some('-') => Some(false),
                _ => None,
            },
            matches: app.preview.search.row_matches(index),
            cursor: app
                .preview
                .cursor_byte
                .filter(|(row, _)| *row == index && app.preview_cursor == Some(index))
                .map(|(_, byte)| byte),
        }
    }
}

pub(super) fn paint_source(
    grid: &mut CellGrid,
    row: &super::preview::PreviewRow,
    mut x: u16,
    y: u16,
    right: u16,
    offset: usize,
    mark: SourceMark<'_>,
) -> u16 {
    if let Some(added) = mark.diff {
        for at in x..right {
            grid.set_cell(
                at,
                y,
                " ",
                Style::GitDiff {
                    added,
                    syntax: None,
                },
            );
        }
    }
    let row_style = if mark.hunk_heading {
        Style::GitControl
    } else {
        row.style
    };
    if matches!(row_style, Style::GitHeading | Style::GitControl) {
        for at in x..right {
            grid.set_cell(at, y, " ", row_style);
        }
    }
    if offset > 0 && !row.text.is_empty() && x < right {
        grid.set_cell(x, y, "‹", Style::Muted);
        x += 1;
    }
    let content_x = x.min(right - 1);
    let mut cursor_x = None;
    let mut column = 0;
    for (at, grapheme) in row.text.grapheme_indices(true) {
        let size = if grapheme == "\t" {
            4 - column % 4
        } else {
            grapheme.width().max(1)
        };
        let end = column + size;
        if end > offset {
            let role = row
                .spans
                .iter()
                .find(|(range, _)| range.contains(&at))
                .map(|(_, role)| *role);
            // A panned line can contain thousands of earlier occurrences. Look
            // up the first possible overlap instead of rescanning them per cell.
            let candidate = mark.matches.partition_point(|found| found.bytes.end <= at);
            let matched = mark
                .matches
                .get(candidate)
                .is_some_and(|found| found.bytes.start < at + grapheme.len());
            let style = if matched {
                Style::Search
            } else {
                mark.diff.map_or_else(
                    || role.map(Style::Syntax).unwrap_or(row_style),
                    |added| Style::GitDiff {
                        added,
                        syntax: role,
                    },
                )
            };
            let next = if grapheme == "\t" || column < offset {
                grid.put_text_clipped(x, y, &" ".repeat(end - column.max(offset)), style, right)
            } else {
                grid.put_text_clipped(x, y, grapheme, style, right)
            };
            if next == x {
                grid.set_cell(right - 1, y, "›", Style::Muted);
                break;
            }
            if mark
                .cursor
                .is_some_and(|byte| at <= byte && byte < at + grapheme.len())
                || (mark.cursor.is_none()
                    && cursor_x.is_none()
                    && column >= offset
                    && !grapheme.trim().is_empty())
            {
                cursor_x = Some(x);
            }
            x = next;
        }
        column = end;
    }
    cursor_x.unwrap_or(content_x)
}

fn file_glyph(
    path: &std::path::Path,
    mode: crate::config::ExplorerIconMode,
) -> (&'static str, crate::config::Highlight) {
    use crate::config::{ExplorerIconMode, Highlight};
    use crate::files::ExplorerIconColor;
    let icon = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(crate::files::file_icon);
    let role = match icon.map(|icon| icon.color) {
        Some(ExplorerIconColor::Azure | ExplorerIconColor::Blue | ExplorerIconColor::Cyan) => {
            Highlight::FUNCTION
        }
        Some(ExplorerIconColor::Green) => Highlight::STRING,
        Some(ExplorerIconColor::Orange | ExplorerIconColor::Yellow) => Highlight::NUMBER,
        Some(ExplorerIconColor::Purple | ExplorerIconColor::Red) => Highlight::KEYWORD,
        _ => Highlight::TYPE,
    };
    let glyph = match mode {
        ExplorerIconMode::NerdFont => icon.map_or("", |icon| icon.glyph),
        ExplorerIconMode::Unicode => "◇",
        ExplorerIconMode::Ascii => "-",
    };
    (glyph, role)
}

pub(super) fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.into();
    }
    let mut result = String::new();
    let mut columns = 0;
    for g in text.graphemes(true) {
        columns += g.width().max(1);
        if columns >= width {
            break;
        }
        result.push_str(g);
    }
    result.push('…');
    result
}

// Replacing a heading must erase its old tail without repainting adjacent rows
// or borrowing cells from the native source/cursor field.
fn paint_heading(grid: &mut CellGrid, x: u16, y: u16, right: u16, text: &str, style: Style) {
    for col in x..right {
        grid.set_cell(col, y, " ", Style::Default);
    }
    grid.put_text_clipped(x, y, text, style, right);
}

pub(super) fn panel(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    title: &str,
    focused: bool,
) {
    if w < 4 || h < 2 {
        return;
    }
    let edge = if focused {
        Style::Accent
    } else {
        Style::Border
    };
    for row in y..y + h {
        grid.set_cell(x, row, "│", edge);
        grid.set_cell(x + w - 1, row, "│", edge);
    }
    // The bottom row was already reserved for the viewport receipt.
    for col in x + 1..x + w - 1 {
        grid.set_cell(col, y + h - 1, "─", edge);
    }
    grid.set_cell(x, y + h - 1, "└", edge);
    grid.set_cell(x + w - 1, y + h - 1, "┘", edge);
    if !title.is_empty() {
        for col in x + 1..x + w - 1 {
            grid.set_cell(col, y, "─", edge);
        }
        grid.set_cell(x, y, "┌", edge);
        grid.set_cell(x + w - 1, y, "┐", edge);
        paint_heading(
            grid,
            x + 2,
            y + 1,
            x + w - 1,
            &ellipsize(title, usize::from(w.saturating_sub(4))),
            if focused {
                Style::GitControl
            } else {
                Style::Default
            },
        );
    }
}

pub(super) fn surface(focused: bool, syntax: Option<crate::config::Highlight>) -> Style {
    if focused {
        Style::GitFocusedSurface(syntax)
    } else {
        Style::GitSurface(syntax)
    }
}

pub(super) fn section(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    title: &str,
    focused: bool,
) {
    if w < 4 || h < 2 {
        return;
    }
    for row in y..y + h {
        for col in x..x + w {
            grid.set_cell(col, row, " ", surface(focused, None));
        }
    }
    // The existing margins become the enclosure; content and cursor cells stay put.
    border(grid, x, y, w, h, title, focused);
    grid.put_text_clipped(
        x + 2,
        y,
        &ellipsize(title, usize::from(w.saturating_sub(4))),
        heading_style(focused),
        x + w - 2,
    );
}

pub(super) fn viewport(
    frame: &mut Frame,
    right: u16,
    y: u16,
    offset: usize,
    visible: usize,
    count: usize,
) {
    if count <= visible {
        return;
    }
    let label = format!(
        " {}–{} / {} ",
        offset + 1,
        (offset + visible).min(count),
        count
    );
    frame.grid.put_text_clipped(
        right.saturating_sub(label.width() as u16),
        y,
        &label,
        Style::Muted,
        right,
    );
}

pub(super) fn footer(
    frame: &mut Frame,
    width: u16,
    height: u16,
    hints: &[(&str, &str)],
    back: bool,
) {
    let y = height - 2;
    // Keep the established footer row and clear its former overflow row.
    for row in y..height {
        for x in 0..width {
            frame.grid.set_cell(x, row, " ", Style::Status);
        }
    }
    let escape = if back {
        Some(("Esc", "back"))
    } else {
        hints.iter().find(|(key, _)| *key == "Esc").copied()
    };
    let mut fixed = vec![("?", "keys")];
    if let Some(escape) = escape {
        fixed.push(escape);
    } else {
        fixed.extend([("r", "refresh"), ("q", "close")]);
    }
    let size = |(key, label): &(&str, &str)| (key.width() + label.width() + 2) as u16;
    let right = width.saturating_sub(fixed.iter().map(size).sum::<u16>() + 1);
    let draw = |frame: &mut Frame, x: u16, row: u16, key: &str, label: &str| {
        let x = frame
            .grid
            .put_text_clipped(x, row, key, Style::GitControl, width);
        frame
            .grid
            .put_text_clipped(x, row, &format!(" {label} "), Style::StatusMuted, width)
    };
    let mut x = 1;
    // Actions belong here; movement and the complete command set live in ? keys.
    // Never turn a wide window into a wall of shortcuts or spill into a second row.
    for &(key, label) in hints
        .iter()
        .filter(|(key, label)| {
            *key != "Esc"
                && !matches!(
                    *label,
                    "move" | "scroll" | "focus" | "pages" | "panes" | "tabs" | "code"
                )
        })
        .take(5)
    {
        if x + size(&(key, label)) > right {
            break;
        }
        x = draw(frame, x, y, key, label);
    }
    x = right;
    for &(key, label) in &fixed {
        x = draw(frame, x, y, key, label);
    }
}

pub(super) fn help(frame: &mut Frame, width: u16, height: u16, page: usize) {
    if width < 40 || height < 18 {
        return;
    }
    let mut rows = if page == 6 {
        vec![
            "m  Confirm merge of selected branch into current branch",
            "R  Confirm rebase of current branch onto selected tip",
            "e  Rename selected local branch; no overwrite",
            "u  Choose selected branch's upstream (Up/Down)",
            "d  Confirm normal local branch deletion",
            "Safety  No force; refuse unmerged / checked out",
            "Upstream  Local config only; no push / fetch",
            "Enter / n  Checkout / create local branch",
            "C / S / X  Continue / skip rebase commit / abort",
            "/  Filter; actions use the selected identity",
            "H/L  Branches / Remotes / Tags",
            "Tab / Ctrl+h/l  Sidebar / preview focus",
            "r  Refresh real references",
            "Esc / q  Clear filter / close",
        ]
    } else if page == 7 {
        vec![
            "d  Confirm deletion of selected LOCAL tag",
            "Safety  No remote deletion, push or checkout",
            "Enter  Inspect selected tag",
            "/  Filter; actions use the selected identity",
            "H/L  Branches / Remotes / Tags",
            "Tab / Ctrl+h/l  Sidebar / preview focus",
            "r  Refresh real references",
            "Esc / q  Clear filter / close",
        ]
    } else if page == 5 {
        vec![
            "n  Create rescue branch at selected commit",
            "Space  Confirm detached checkout of selected commit",
            "w  Create detached worktree at selected commit",
            "Enter  Browse historical files in Changes",
            "c  Commit currently staged changes",
            "m  Full selected commit message",
            "H/L  Commits / Reflog tabs",
            "/  Events: filter; preview: find · n/N matches",
            "j/k or ↑/↓  Select reflog event",
            "r  Refresh; preserve selected event",
            "Tab / Ctrl+h/l  Sidebar / diff focus",
            "Esc / q  Changes / close",
            "Recovery  Uses the full commit ID; no moving HEAD@{n}",
            "Checkout  Git refuses conflicts; no auto-stash",
        ]
    } else if page == 4 {
        vec![
            "/  Files: filter; preview: find literal source",
            "n/N  Next / previous source match (preview)",
            "Tab / Ctrl+h/l  Files / diff",
            "1–4  Select sidebar pane",
            "j/k or ↑/↓  Select file / scroll diff",
            "J/K  Next / previous file",
            "h/l or ←/→  Fold tree / pan diff",
            "PgUp/PgDn  Scroll diff",
            "Home / End  First / last diff line",
            "s  Toggle optional split view",
            "m  Full commit message",
            "r  Refresh commit",
            "Esc / q  Return to history",
        ]
    } else if page == 0 && height < 31 {
        // Every row is reachable at the minimum 64×20 size. The full list below
        // used to silently cut off submodule, remote and stash actions.
        vec![
            "1–4 / Tab  Panes · Ctrl+h/j/k/l focus",
            "H/L · [ / ]  Pane tabs / pages",
            "A/W/F  Amend / reword HEAD / fixup commit",
            "/ / Enter  List filter / source find; apply",
            "j/k · Home/End  Code move/edges · n/N source matches",
            "h/l  Fold tree / pan · PgUp/PgDn scroll",
            "Space / x / V  List: toggle set / mark / range",
            "Space / a / V  Diff: hunk / exact line / range",
            "d/D discard  Unstaged hunk/line; V range; confirm",
            "v / i / s  All files / index · Split / unified",
            "c/P/C · Enter/m  Commit/push/merge · open/message",
            "n / d  New branch, worktree, stash / remove",
            "Space / u  Submodules: pointer / init-update",
            "f · a/p/d  Fetch · stash apply/pop/drop",
        ]
    } else if page == 0 {
        vec![
            "[ / ]  Switch page",
            "1–4 / Tab  Select pane",
            "Ctrl+h/j/k/l  Focus panes",
            "/  List: filter; preview: find literal source",
            "n/N / Esc (preview)  Next/previous match / clear find",
            "Space / c / P  Stage or unstage / commit / push",
            "A/W/F  Amend / reword HEAD / fixup selected commit",
            "m / R (branches)  Confirm merge / rebase onto",
            "C / S / X  Continue / skip rebase commit / abort",
            "x / V (Changes list)  Mark file / visible range",
            "j/k (range)  Extend · V finish · Esc clear marks",
            "Space (list)  Toggle marked set, else file / directory",
            "*  Marked file / directory with marked descendants",
            "Space (diff)  Stage / unstage current hunk",
            "a (diff)  Stage / unstage exact line or range",
            "V / Esc (diff)  Mark / cancel range (unified)",
            "d / D (code)  Confirm discard: unstaged hunk / exact line",
            "V + d/D (code)  Discard only range within one hunk; Esc cancels",
            "d / D (Changes)  Confirm discard: file / marked set; worktree only",
            "j/k or ↑/↓  Move selection / native code cursor",
            "Home/End · PgUp/PgDn  Code edges / scroll",
            "h/l  Tree / horizontal scroll",
            "H/L  Switch tabs in the focused sidebar pane",
            "1  Changes / Worktrees / Submodules",
            "2  Local branches / Remotes / Tags",
            "3  Commits / Reflog",
            "v / i  All files / index",
            "s  Split / unified preview",
            "n  New branch / worktree / stash in its pane",
            "d (Worktrees)  Remove a clean, unlocked worktree",
            "Space / u (Submodules)  Toggle pointer / init-update",
            "f (Remotes)  Fetch after confirmation",
            "a / p / d  Apply / pop / drop stash",
            "o / t / C  Conflict ours / theirs / continue",
            ":tutor git  Disposable Git lessons",
            "Enter  Open selection",
            "Esc  Commit files → history → changes",
        ]
    } else {
        vec![
            "[ / ]  Switch page",
            "Ctrl+h/l  List / details",
            "j/k · Home/End  Move / first / last",
            "Tab/Shift+Tab · Enter  Controls / open",
            "h/l · PgUp/PgDn  Pan / scroll",
            "Ctrl+f · n/N  Find / next / previous match",
            "r  Refresh",
        ]
    };
    if page == 1 || page == 2 {
        rows.extend([
            "i  Repository / inbox",
            "/  Filter list",
            "n  New issue / pull request",
            "c / e  Comment / edit",
        ]);
    }
    if page == 1 {
        rows.extend([
            "v / S / D  Views / save / delete locally",
            ", / .  Previous / next saved view",
        ]);
    }
    if page == 2 {
        rows.extend([
            "H/L  Pull request tabs",
            "f  Choose changed file",
            "v/S/D · ,/.  Views/save/delete · previous/next",
        ]);
    }
    if page == 3 {
        rows.push("Ctrl+j/k  Run history / workflows");
    }
    let w = width.saturating_sub(6).min(78);
    let h = (rows.len() as u16 + 4).min(height - 2);
    let x = (width - w) / 2;
    let y = (height - h) / 2;
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    border(&mut frame.grid, x, y, w, h, "Keyboard", true);
    for (i, row) in rows.iter().take(usize::from(h - 4)).enumerate() {
        let (key, label) = row.split_once("  ").unwrap();
        let at = frame.grid.put_text_clipped(
            x + 2,
            y + 2 + i as u16,
            &format!("{key:<16} "),
            Style::GitControl,
            x + w - 2,
        );
        frame.grid.put_text_clipped(
            at,
            y + 2 + i as u16,
            &ellipsize(label, usize::from((x + w - 2).saturating_sub(at))),
            Style::GitSurface(None),
            x + w - 2,
        );
    }
    frame
        .grid
        .put_text_clipped(x + 2, y + h - 2, "Esc / ?  back", Style::Muted, x + w - 2);
    frame.cursor = Some((x + 2, y + 2, SetCursorStyle::SteadyBlock));
}

pub(super) fn navigation(frame: &mut Frame, page: usize, width: u16, repository: &str) {
    for x in 0..width {
        frame.grid.set_cell(x, 0, " ", Style::Status);
    }
    let right = width.saturating_sub(9);
    let name_width = usize::from(width.saturating_sub(50)).min(24);
    let name = ellipsize(repository, name_width);
    let mut x = frame
        .grid
        .put_text_clipped(1, 0, &format!("{name} │ "), Style::GitHeading, right);
    for (index, label) in ["Git", "Issues", "Pull requests", "Actions"]
        .iter()
        .enumerate()
    {
        x = frame.grid.put_text_clipped(
            x,
            0,
            &format!(" {label} "),
            if index == page {
                Style::GitSelected(None)
            } else {
                Style::StatusMuted
            },
            right,
        );
    }
    // Git already keeps help in its footer; GitHub pages own their own legends.
    if page != 0 {
        frame
            .grid
            .put_text_clipped(right + 1, 0, "? keys", Style::StatusMuted, width);
    }
}

// Operation fields are static context, not inputs. Keep context, notices and the
// two-button navigation in one layout so errors cannot overwrite the controls.
fn operation_lines(text: &str, width: usize, limit: usize) -> Vec<String> {
    let mut lines =
        crate::ui::render::wrap_plain_text(text, width, crate::config::AmbiguousWidth::Narrow);
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(&format!("{last} …"), width);
        }
    }
    for line in &mut lines {
        *line = ellipsize(line, width);
    }
    if lines.is_empty() && limit > 0 {
        lines.push(String::new());
    }
    lines
}

fn render_operation_form(
    frame: &mut Frame,
    form: &super::Form,
    width: u16,
    height: u16,
    status: &str,
) {
    let super::FormAction::Operation { action, .. } = &form.action else {
        return;
    };
    frame.cursor = None;
    let w = width.saturating_sub(6).min(78);
    let max_h = height.saturating_sub(2);
    if w < 16 || max_h < 5 {
        return;
    }
    let content_width = usize::from(w - 4);
    let label_width = form
        .fields
        .iter()
        .map(|field| field.label.width())
        .max()
        .unwrap_or(0)
        .min(content_width / 2);
    let value_width = content_width.saturating_sub(label_width + 2).max(1);
    let status_lines = if status.is_empty() {
        Vec::new()
    } else {
        operation_lines(status, content_width, 2.min(usize::from(max_h - 5)))
    };
    let mut remaining = usize::from(max_h - 5).saturating_sub(status_lines.len());
    let mut context = Vec::new();
    for (index, field) in form.fields.iter().enumerate() {
        if remaining == 0 {
            break;
        }
        // Reserve a line for each remaining context before wrapping this one.
        let reserve = form
            .fields
            .len()
            .saturating_sub(index + 1)
            .min(remaining - 1);
        let lines = operation_lines(&field.input.input, value_width, 2.min(remaining - reserve));
        remaining -= lines.len();
        context.push((ellipsize(&field.label, label_width), lines));
    }
    let context_height = context.iter().map(|(_, lines)| lines.len()).sum::<usize>();
    let h = (context_height + status_lines.len() + 5) as u16;
    let x = (width - w) / 2;
    let y = (height - h) / 2;
    let right = x + w - 2;
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    border(&mut frame.grid, x, y, w, h, &form.title, true);
    let mut row = y + 1;
    for (label, lines) in context {
        frame.grid.put_text_clipped(
            x + 2,
            row,
            &format!("{label}:"),
            Style::GitDetail {
                added: None,
                selection: None,
                focused: false,
            },
            right,
        );
        for line in lines {
            frame.grid.put_text_clipped(
                x + 4 + label_width as u16,
                row,
                &line,
                Style::GitSurface(None),
                right,
            );
            row += 1;
        }
    }
    for line in status_lines {
        frame
            .grid
            .put_text_clipped(x + 2, row, &line, Style::Warning, right);
        row += 1;
    }
    let label = match action {
        super::GitOperationAction::Continue => "Continue",
        super::GitOperationAction::Abort => "Abort",
        super::GitOperationAction::Skip => "Skip commit",
        super::GitOperationAction::Resolve => "Resolve",
    };
    let blocked = form
        .fields
        .iter()
        .any(|field| field.label == "Unrelated changes");
    let label = if blocked {
        format!("Retry {}", label.to_lowercase())
    } else {
        label.to_owned()
    };
    let cancel_selected = form.selected == form.fields.len() + 1;
    let submit = format!("{}[ {label} ]", if cancel_selected { "  " } else { "› " });
    let cancel = if cancel_selected {
        "› [ Cancel ]"
    } else {
        "  [ Cancel ]"
    };
    let buttons_y = y + h - 3;
    let cancel_x = x + 4 + submit.width() as u16;
    frame.grid.put_text_clipped(
        x + 2,
        buttons_y,
        &submit,
        if cancel_selected {
            Style::GitSurface(None)
        } else {
            Style::GitSelected(None)
        },
        right,
    );
    frame.grid.put_text_clipped(
        cancel_x,
        buttons_y,
        cancel,
        if cancel_selected {
            Style::GitSelected(None)
        } else {
            Style::GitSurface(None)
        },
        right,
    );
    let enter = if cancel_selected {
        "cancel".into()
    } else {
        label.to_lowercase()
    };
    frame.grid.put_text_clipped(
        x + 2,
        y + h - 2,
        &format!("Enter {enter} · Tab switch · Esc cancel"),
        Style::GitDetail {
            added: None,
            selection: None,
            focused: false,
        },
        right,
    );
    frame.cursor = Some((
        if cancel_selected { cancel_x } else { x + 2 },
        buttons_y,
        SetCursorStyle::SteadyBlock,
    ));
}

fn render_changed_file_picker(
    frame: &mut Frame,
    form: &super::Form,
    width: u16,
    height: u16,
) -> bool {
    if !matches!(
        form.action,
        super::FormAction::Github(super::github::Action::Files(_))
    ) || width < 24
        || height < 8
    {
        return false;
    }
    let [field] = form.fields.as_slice() else {
        return false;
    };
    // An obsolete draft must remain visible to the ordinary validation path,
    // never silently become the first available file.
    let Some(selected) = field
        .choices
        .iter()
        .position(|path| path == &field.input.input)
    else {
        return false;
    };
    let w = width.saturating_sub(6).min(90);
    let visible = field.choices.len().min(usize::from(height - 6)).max(1);
    let h = visible as u16 + 4;
    let x = (width - w) / 2;
    let y = (height - h) / 2;
    let first = selected.saturating_sub(visible - 1);
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    border(&mut frame.grid, x, y, w, h, "Choose changed file", true);
    let position = format!(" {}/{} ", selected + 1, field.choices.len());
    frame.grid.put_text_clipped(
        x + w - position.width() as u16 - 2,
        y,
        &position,
        Style::Muted,
        x + w - 1,
    );
    for (offset, (index, path)) in field
        .choices
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .enumerate()
    {
        let row = y + 1 + offset as u16;
        let active = index == selected;
        let style = if active && form.selected == 0 {
            Style::GitSelected(None)
        } else {
            Style::GitSurface(None)
        };
        for col in x + 2..x + w - 2 {
            frame.grid.set_cell(col, row, " ", style);
        }
        frame
            .grid
            .put_text_clipped(x + 2, row, if active { "›" } else { " " }, style, x + w - 2);
        // Display escaping and tail fitting never change the submitted path.
        let path: String = path
            .chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect();
        let available = usize::from(w - 6);
        let label = if path.width() <= available {
            path
        } else {
            let mut room = available.saturating_sub(1);
            let mut start = path.len();
            for (at, grapheme) in path.grapheme_indices(true).rev() {
                let size = grapheme.width().max(1);
                if size > room {
                    break;
                }
                start = at;
                room -= size;
            }
            format!("…{}", &path[start..])
        };
        frame
            .grid
            .put_text_clipped(x + 4, row, &label, style, x + w - 2);
    }
    let row = y + h - 2;
    let submit = "[ Open file ]";
    let cancel = "[ Cancel ]";
    let cancel_x = x + 4 + submit.width() as u16;
    for (at, text, index) in [(x + 2, submit, 1), (cancel_x, cancel, 2)] {
        frame.grid.put_text_clipped(
            at,
            row,
            text,
            if form.selected == index {
                Style::Selection
            } else {
                Style::Accent
            },
            x + w - 2,
        );
    }
    frame.grid.put_text_clipped(
        cancel_x + cancel.width() as u16 + 2,
        row,
        "Ctrl+Enter open · Esc cancel",
        Style::Muted,
        x + w - 2,
    );
    frame.cursor = Some(if form.selected == 0 {
        (
            x + 4,
            y + 1 + (selected - first) as u16,
            SetCursorStyle::SteadyBlock,
        )
    } else {
        (
            if form.selected == 1 {
                x + 4
            } else {
                cancel_x + 2
            },
            row,
            SetCursorStyle::SteadyBlock,
        )
    });
    true
}

fn field_height(form: &super::Form, field: &super::Field) -> u16 {
    if matches!(form.action, super::FormAction::Rewrite(_)) && field.read_only {
        1
    } else if field.multiline {
        6
    } else {
        3
    }
}

fn form_height(form: &super::Form, height: u16) -> u16 {
    (form
        .fields
        .iter()
        .map(|field| field_height(form, field))
        .sum::<u16>()
        + 5)
    .min(height.saturating_sub(2))
}

pub(super) fn render_form(frame: &mut Frame, form: &super::Form, width: u16, height: u16) {
    if matches!(&form.action, super::FormAction::Operation { .. }) {
        render_operation_form(frame, form, width, height, "");
        return;
    }
    if render_changed_file_picker(frame, form, width, height) {
        return;
    }
    let w = width.saturating_sub(6).min(90);
    let compact_context = matches!(form.action, super::FormAction::Rewrite(_));
    let h = form_height(form, height);
    let x = (width - w) / 2;
    let y = (height - h) / 2;
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    border(&mut frame.grid, x, y, w, h, &form.title, true);
    if !form.fields.is_empty() && !compact_context {
        let position = format!(
            " {}/{} ",
            (form.selected + 1).min(form.fields.len()),
            form.fields.len()
        );
        frame.grid.put_text_clipped(
            x + w - position.width() as u16 - 2,
            y,
            &position,
            Style::Muted,
            x + w - 1,
        );
    }
    let chosen_bottom = form
        .fields
        .iter()
        .take((form.selected + 1).min(form.fields.len()))
        .map(|field| field_height(form, field))
        .sum::<u16>();
    let scroll = chosen_bottom.saturating_sub(h.saturating_sub(5));
    let mut at = 0u16;
    for (i, field) in form.fields.iter().enumerate() {
        let size = field_height(form, field);
        if at < scroll || at + size > scroll + h.saturating_sub(4) {
            at += size;
            continue;
        }
        let row = y + 1 + at - scroll;
        at += size;
        if compact_context && field.read_only {
            let value = if field.label == "Target" {
                field.input.input.chars().take(12).collect::<String>()
            } else {
                field.input.input.clone()
            };
            let at = frame.grid.put_text_clipped(
                x + 2,
                row,
                &format!("{}  ", field.label),
                Style::Muted,
                x + w - 2,
            );
            frame.grid.put_text_clipped(
                at,
                row,
                &ellipsize(&value, usize::from((x + w - 2).saturating_sub(at))),
                if field.label == "Target" {
                    Style::Accent
                } else {
                    Style::GitHeading
                },
                x + w - 2,
            );
            continue;
        }
        let label = format!(
            "{}{}{}",
            field.label,
            if field.required && !compact_context {
                " *"
            } else {
                ""
            },
            if field.read_only {
                " · read only"
            } else if field.choices.is_empty() {
                ""
            } else if field.custom_choices {
                " · ↑/↓ choose or type"
            } else {
                " · ↑/↓ choose"
            }
        );
        frame.grid.put_text_clipped(
            x + 2,
            row,
            &label,
            if form.selected == i {
                Style::Accent
            } else {
                Style::GitSurface(Some(crate::config::Highlight::COMMENT))
            },
            x + w - 2,
        );
        for input_y in row + 1..row + size - 1 {
            for input_x in x + 2..x + w - 2 {
                frame.grid.set_cell(
                    input_x,
                    input_y,
                    " ",
                    if form.selected == i {
                        Style::GitSelected(None)
                    } else {
                        Style::GitSurface(None)
                    },
                );
            }
        }
        let input = &field.input;
        let cursor_line = input.input[..input.cursor]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let lines = input.input.split('\n').collect::<Vec<_>>();
        let visible = (size - 2) as usize;
        let first = cursor_line.saturating_sub(visible.saturating_sub(1));
        let before = input.input[..input.cursor]
            .rsplit('\n')
            .next()
            .unwrap_or_default();
        let max = w.saturating_sub(5) as usize;
        let mut start = before.len();
        let mut column = 0;
        for (at, g) in before.grapheme_indices(true).rev() {
            let size = UnicodeWidthStr::width(g).max(1);
            if column + size >= max {
                break;
            }
            start = at;
            column += size;
        }
        for (offset, line) in lines.iter().enumerate().skip(first).take(visible) {
            let text = if offset == cursor_line {
                &line[start..]
            } else {
                line
            };
            frame.grid.put_text_clipped(
                x + 2,
                row + 1 + (offset - first) as u16,
                text,
                if form.selected == i {
                    Style::GitSelected(None)
                } else {
                    Style::GitSurface(None)
                },
                x + w - 2,
            );
        }
        if form.selected == i {
            frame.cursor = Some((
                x + 2 + column as u16,
                row + 1 + (cursor_line - first) as u16,
                if field.read_only {
                    SetCursorStyle::SteadyBlock
                } else {
                    SetCursorStyle::BlinkingBar
                },
            ));
        }
    }
    let row = y + h - 2;
    let count = form.fields.len();
    let (submit, cancel) = match &form.action {
        super::FormAction::Github(super::github::Action::CreatePullRequest) => {
            ("[ Review → ]", "[ Cancel ]")
        }
        super::FormAction::Github(super::github::Action::ConfirmPullRequest(request)) => (
            if request.push.is_some() {
                "[ Push + create PR ]"
            } else {
                "[ Create PR ]"
            },
            "[ Back ]",
        ),
        super::FormAction::Rewrite(plan) => (
            match plan.kind {
                crate::git::rewrite::Kind::Amend => "[ Amend ]",
                crate::git::rewrite::Kind::Reword => "[ Reword ]",
                crate::git::rewrite::Kind::Fixup(_) => "[ Fixup ]",
            },
            "[ Cancel ]",
        ),
        super::FormAction::SourceSearch => ("[ Find ]", "[ Cancel ]"),
        super::FormAction::Discard(_) | super::FormAction::DiscardFiles(_) => {
            ("[ Discard ]", "[ Cancel ]")
        }
        super::FormAction::Reference(super::refs::Action::MergeBranch { .. }) => {
            ("[ Merge ]", "[ Cancel ]")
        }
        super::FormAction::Reference(super::refs::Action::RebaseBranch { .. }) => {
            ("[ Rebase ]", "[ Cancel ]")
        }
        _ => ("[ Submit ]", "[ Cancel ]"),
    };
    let cancel_x = x + 4 + submit.width() as u16;
    frame.grid.put_text(
        x + 2,
        row,
        submit,
        if form.selected == count {
            Style::Selection
        } else {
            Style::Accent
        },
    );
    frame.grid.put_text(
        cancel_x,
        row,
        cancel,
        if form.selected == count + 1 {
            Style::Selection
        } else {
            Style::Default
        },
    );
    frame.grid.put_text_clipped(
        cancel_x + cancel.width() as u16 + 2,
        row,
        if matches!(form.action, super::FormAction::SourceSearch) {
            "Enter find · Esc cancel"
        } else if matches!(
            form.action,
            super::FormAction::Discard(_) | super::FormAction::DiscardFiles(_)
        ) {
            "Enter discard · Esc cancel"
        } else {
            "Tab · Ctrl+Enter · Esc"
        },
        Style::Muted,
        x + w - 2,
    );
    if form.selected >= count {
        frame.cursor = Some((
            if form.selected == count {
                x + 3
            } else {
                cancel_x + 1
            },
            row,
            SetCursorStyle::SteadyBlock,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::preview::PreviewRow;
    use super::*;

    fn row(old: Option<u32>, new: Option<u32>, marker: Option<char>) -> PreviewRow {
        PreviewRow {
            text: "code".into(),
            style: Style::Default,
            spans: Vec::new(),
            old,
            new,
            marker,
            path: None,
        }
    }

    fn changed_file_form(paths: Vec<String>, selected: usize) -> super::super::Form {
        let mut field = super::super::Field::new("File", paths[selected].clone());
        field.choices = paths;
        super::super::Form {
            title: "Choose changed file · example/private-repository".into(),
            action: super::super::FormAction::Github(super::super::github::Action::Files(3)),
            fields: vec![field],
            selected: 0,
        }
    }

    #[test]
    fn changed_file_picker_shows_inventory_actual_choice_and_native_cursor_without_mutation() {
        let paths = [
            "src/git/screen/render.rs",
            "src/git/screen/tree.rs",
            "tests/layout.rs",
        ]
        .map(str::to_owned)
        .to_vec();
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            for selected in 0..paths.len() {
                let mut form = changed_file_form(paths.clone(), selected);
                for focus in 0..3 {
                    form.selected = focus;
                    let mut frame = Frame {
                        grid: CellGrid::new(width, height),
                        cursor: None,
                    };
                    for y in 0..height {
                        for x in 0..width {
                            frame.grid.set_cell(x, y, ".", Style::Warning);
                        }
                    }
                    render_form(&mut frame, &form, width, height);
                    let shown = (0..height)
                        .map(|y| frame.grid.row_text(y))
                        .collect::<Vec<_>>()
                        .join("\n");
                    for path in &paths {
                        assert!(shown.contains(path), "{shown}");
                    }
                    assert!(shown.contains(&format!(" {}/3 ", selected + 1)), "{shown}");
                    assert!(
                        shown.contains("[ Open file ]") && shown.contains("[ Cancel ]"),
                        "{shown}"
                    );
                    assert!(
                        !shown.contains("Submit")
                            && !shown.contains("1/1")
                            && !shown.contains("private-repository")
                    );
                    let w = (width - 6).min(90);
                    let x = (width - w) / 2;
                    let h = 7;
                    let y = (height - h) / 2;
                    let (cx, cy, _) = frame.cursor.unwrap();
                    if focus == 0 {
                        assert_eq!((cx, cy), (x + 4, y + 1 + selected as u16));
                        assert_eq!(
                            frame.grid.get(cx, cy).unwrap().symbol,
                            &paths[selected][..1]
                        );
                        assert_eq!(
                            frame.grid.get(cx, cy).unwrap().style,
                            Style::GitSelected(None)
                        );
                    } else {
                        assert_eq!(cy, y + h - 2);
                        assert_eq!(frame.grid.get(cx, cy).unwrap().style, Style::Selection);
                    }
                    for row in 0..height {
                        for col in 0..width {
                            if !(x..x + w).contains(&col) || !(y..y + h).contains(&row) {
                                let cell = frame.grid.get(col, row).unwrap();
                                assert_eq!(cell.symbol, ".");
                                assert_eq!(cell.style, Style::Warning);
                            }
                        }
                    }
                    assert_eq!(form.fields[0].choices, paths);
                    assert_eq!(form.fields[0].input.input, paths[selected]);
                    assert_eq!(form.fields[0].input.cursor, paths[selected].len());
                }
            }
        }
    }

    #[test]
    fn changed_file_picker_scrolls_escapes_and_keeps_grapheme_tails_without_retargeting_stale_values()
     {
        let paths: Vec<_> = (0..40)
            .map(|i| {
                format!(
                    "{}/path\\t/{i:02}-界-e\u{301}-👩‍💻.rs",
                    "long-parent/".repeat(12)
                )
            })
            .collect();
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            for selected in [0, 20, 39] {
                let mut form = changed_file_form(paths.clone(), selected);
                let mut frame = Frame {
                    grid: CellGrid::new(width, height),
                    cursor: None,
                };
                assert!(render_changed_file_picker(&mut frame, &form, width, height));
                let (x, y, _) = frame.cursor.unwrap();
                let row = frame.grid.row_text(y);
                assert!(
                    row.contains(&format!("{selected:02}-界-e\u{301}-👩‍💻.rs")),
                    "{row}"
                );
                assert_eq!(frame.grid.get(x, y).unwrap().symbol, "…");
                assert!(!frame.grid.get(x, y).unwrap().continuation);
                assert_eq!(form.fields[0].input.input, paths[selected]);
                assert_eq!(form.fields[0].choices, paths);
                form.fields[0].input.input = "obsolete/removed.rs".into();
                form.fields[0].input.cursor = form.fields[0].input.input.len();
                assert!(!render_changed_file_picker(
                    &mut frame, &form, width, height
                ));
                render_form(&mut frame, &form, width, height);
                let shown = (0..height)
                    .map(|y| frame.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    shown.contains("obsolete/removed.rs") && shown.contains("Submit"),
                    "{shown}"
                );
                assert_eq!(form.fields[0].input.input, "obsolete/removed.rs");
            }
            let form = changed_file_form(vec!["dir/line\nbreak\t界.rs".into()], 0);
            let mut frame = Frame {
                grid: CellGrid::new(width, height),
                cursor: None,
            };
            render_form(&mut frame, &form, width, height);
            let shown = (0..height)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(shown.contains("dir/line\\nbreak\\t界.rs"), "{shown}");
            assert_eq!(form.fields[0].input.input, "dir/line\nbreak\t界.rs");
        }
        let mut other = changed_file_form(vec!["one.rs".into()], 0);
        other.action = super::super::FormAction::Commit;
        let mut frame = Frame {
            grid: CellGrid::new(100, 30),
            cursor: None,
        };
        assert!(!render_changed_file_picker(&mut frame, &other, 100, 30));
    }

    #[test]
    fn compact_patch_numbers_keep_the_applicable_line_and_explicit_sign() {
        let compact = SourceLayout::new(64, 26, 2, true);
        let wide = SourceLayout::new(80, 26, 2, true);
        for (line, compact_numbers, wide_numbers, sign) in [
            (row(Some(12), None, Some('-')), "12", "12    ", "-"),
            (row(None, Some(13), Some('+')), "13", "   13 ", "+"),
            (row(Some(14), Some(15), Some(' ')), "15", "14 15 ", " "),
        ] {
            assert_eq!(compact.numbers(&line), compact_numbers);
            assert_eq!(wide.numbers(&line), wide_numbers);
            let mut grid = CellGrid::new(64, 1);
            let x = compact.paint_gutter(&mut grid, &line, 0, false);
            assert_eq!(x, 30); // divider26 + inset1 + two digits + sign
            assert_eq!(grid.get(x - 1, 0).unwrap().symbol, sign);
        }
        assert!(compact.numbers(&row(None, None, None)).is_empty());
    }

    #[test]
    fn one_compact_patch_cell_restores_a_33_cell_body_without_moving_the_divider() {
        let layout = SourceLayout::new(64, 26, 3, true);
        for (text, overflow) in [
            ("pub fn value_009() -> usize { 9 }", false),
            ("pub fn value_010() -> usize { 10 }", true),
        ] {
            let mut line = row(None, Some(139), Some('+'));
            line.text = text.into();
            let mut grid = CellGrid::new(64, 1);
            grid.set_cell(26, 0, "│", Style::Border);
            let x = layout.paint_gutter(&mut grid, &line, 0, false);
            assert_eq!(layout.numbers(&line), "139");
            assert_eq!(x, 31);
            assert_eq!(layout.code_width(&line), 33);
            let cursor = paint_source(
                &mut grid,
                &line,
                x,
                0,
                layout.right,
                0,
                SourceMark {
                    cursor: Some(0),
                    ..SourceMark::default()
                },
            );
            assert_eq!(cursor, x);
            assert_eq!(grid.get(cursor, 0).unwrap().symbol, "p");
            assert_eq!(grid.get(26, 0).unwrap().symbol, "│");
            assert_eq!(grid.get(30, 0).unwrap().symbol, "+");
            assert_eq!(
                grid.get(63, 0).unwrap().symbol,
                if overflow { "›" } else { "}" }
            );
        }
    }

    #[test]
    fn gutter_paint_and_match_reveal_use_identical_width_at_the_80_column_boundary() {
        for width in [64, 79, 80, 100, 190] {
            let left = (width * 3 / 10).clamp(26, 52);
            for patch in [false, true] {
                for number_width in [1, 2, 3, 4] {
                    let number = 10_u32.pow(number_width as u32 - 1);
                    let line = if patch {
                        row(Some(number), Some(number), Some(' '))
                    } else {
                        row(None, Some(number), None)
                    };
                    let layout = SourceLayout::new(width, left, number_width, patch);
                    let expected_numbers = if patch && width < 80 {
                        number.to_string()
                    } else if patch {
                        format!("{number} {number} ")
                    } else {
                        format!("{number} │ ")
                    };
                    assert_eq!(layout.numbers(&line), expected_numbers);
                    if patch && width < 80 {
                        let shorter = row(Some(1), Some(1), Some(' '));
                        assert_eq!(layout.numbers(&shorter), format!("{:>number_width$}", 1));
                    }
                    let mut grid = CellGrid::new(width, 1);
                    let code_x = layout.paint_gutter(&mut grid, &line, 0, false);
                    let painted_numbers = grid
                        .row_text(0)
                        .chars()
                        .skip(usize::from(layout.x))
                        .take(expected_numbers.chars().count())
                        .collect::<String>();
                    assert_eq!(painted_numbers, expected_numbers);
                    if patch {
                        assert_eq!(grid.get(code_x - 1, 0).unwrap().symbol, " ");
                    }
                    assert_eq!(layout.code_width(&line), usize::from(layout.right - code_x));
                    assert_eq!(layout.compact, width < 80);
                    let original = SourceLayout {
                        x: left + 2,
                        right: width - 1,
                        compact: false,
                        ..SourceLayout::new(width, left, number_width, patch)
                    };
                    let gain = if width < 80 {
                        if patch { number_width + 4 } else { 2 }
                    } else {
                        0
                    };
                    assert_eq!(layout.code_width(&line), original.code_width(&line) + gain);
                }
            }
        }
    }

    #[test]
    fn compact_gutter_does_not_change_grapheme_cursor_search_or_pan_mapping() {
        for width in [64, 79, 80, 100, 190] {
            let left = (width * 3 / 10).clamp(26, 52);
            let layout = SourceLayout::new(width, left, 2, true);
            let mut line = row(None, Some(13), Some('+'));
            line.text = format!("\t  界 e\u{301} 👩‍💻 needle_12 {}", "x".repeat(200));
            for (needle, grapheme) in [
                ("界", "界"),
                ("\u{301}", "e\u{301}"),
                ("💻", "👩‍💻"),
                ("needle", "n"),
            ] {
                let byte = line.text.find(needle).unwrap();
                let matches = [super::super::search::SourceMatch {
                    row: 0,
                    bytes: byte..byte + needle.len(),
                }];
                for offset in [0, 6] {
                    let mut grid = CellGrid::new(width, 1);
                    let x = layout.paint_gutter(&mut grid, &line, 0, false);
                    let cursor_x = paint_source(
                        &mut grid,
                        &line,
                        x,
                        0,
                        layout.right,
                        offset,
                        SourceMark {
                            diff: Some(true),
                            matches: &matches,
                            cursor: Some(byte),
                            ..SourceMark::default()
                        },
                    );
                    let cell = grid.get(cursor_x, 0).unwrap();
                    assert_eq!(cell.symbol, grapheme);
                    assert_eq!(cell.style, Style::Search);
                    assert!(!cell.continuation);
                    assert_eq!(grid.get(layout.right - 1, 0).unwrap().symbol, "›");
                    if offset != 0 {
                        assert_eq!(grid.get(x, 0).unwrap().symbol, "‹");
                    }
                }
            }
            // A manually panned distant target retains the same real byte cursor.
            line.text = format!("{}needle{}", "x".repeat(240), "y".repeat(200));
            let mut grid = CellGrid::new(width, 1);
            let x = layout.paint_gutter(&mut grid, &line, 0, false);
            let cursor_x = paint_source(
                &mut grid,
                &line,
                x,
                0,
                layout.right,
                235,
                SourceMark {
                    cursor: Some(240),
                    ..SourceMark::default()
                },
            );
            assert_eq!(cursor_x, x + 6); // left overflow marker + five source cells
            assert_eq!(grid.get(cursor_x, 0).unwrap().symbol, "n");
        }
    }

    #[test]
    fn historical_heading_prioritizes_commit_identity_without_moving_count_or_rows() {
        let mut commit = super::super::commit::Commit::parse(
            concat!(
                "0123456789012345678901234567890123456789\0",
                "Author\0",
                "2026-11-09\0",
                "\0",
                "Subject\0",
                "1\t0\tfile.rs\0",
            ),
            None,
        )
        .unwrap();
        for left in [26, 30, 52] {
            for focused in [false, true] {
                for filtered in [false, true] {
                    commit.filter.input.input = if filtered { "missing" } else { "" }.into();
                    let count = if filtered { "0/1" } else { "1" };
                    let mut grid = CellGrid::new(left + 30, 12);
                    section(&mut grid, 0, 4, left, 4, "Changes · 0123456", focused);
                    let count_x = left - 2 - count.len() as u16;
                    grid.put_text_clipped(count_x, 4, count, Style::Muted, left - 1);
                    grid.put_text_clipped(
                        2,
                        5,
                        "selected file",
                        Style::GitSelected(None),
                        left - 1,
                    );
                    let before = grid.clone();
                    commit_files_heading(&mut grid, &commit, left, 4, focused);
                    let text = grid.row_text(4);
                    assert!(text.contains("Commit 0123456"), "{text}");
                    assert!(!text.contains("Changes"));
                    if !filtered {
                        assert!(text.contains("files"), "{text}");
                    }
                    assert_eq!(grid.get(2, 4).unwrap().style, heading_style(focused));
                    for y in 0..12 {
                        for x in 0..left + 30 {
                            if y != 4 || x < 2 || x >= count_x {
                                assert_eq!(grid.get(x, y), before.get(x, y));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn commit_metadata_puts_shared_tip_refs_before_author_and_date() {
        let mut commit = super::super::commit::Commit::parse(
            concat!(
                "0123456789012345678901234567890123456789\0",
                "Author\0",
                "2026-11-09\0",
                "\0",
                "Subject\0",
                "1\t0\tfile.rs\0",
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            commit_metadata(&commit, 66),
            "0123456 · Author · 2026-11-09 · 1 files"
        );
        commit.decorations = "HEAD→main, feature/source-search".into();
        let wide = commit_metadata(&commit, 134);
        assert!(wide.starts_with("0123456 · HEAD→main, feature/source-search · Author"));
        commit.author = "Generated Recovery QA".into();
        let medium = commit_metadata(&commit, 66);
        assert_eq!(
            medium,
            "0123456 · HEAD→main · Generated Recovery QA · 2026-11-09 · 1 files"
        );
        assert!(commit_metadata(&commit, 134).contains("feature/source-search"));
        assert!(commit_metadata(&commit, 34).contains("HEAD→main"));
        for width in [0, 1, 34, 50, 66, 134] {
            let meta = commit_metadata(&commit, width);
            assert!(meta.width() <= width, "{width}: {meta}");
            assert!(
                !meta.contains('\n'),
                "metadata must stay on its existing row"
            );
        }
        commit.author = "Very long 界 e\u{301} 👩‍💻 attribution which cannot all fit".into();
        commit.decorations = "HEAD→very-long-current-branch, feature/source-search".into();
        let medium = commit_metadata(&commit, 66);
        assert!(medium.contains("HEAD→"));
        assert!(medium.contains("Very long 界 e\u{301} 👩‍💻"));
        assert!(medium.ends_with(" · 2026-11-09 · 1 files"));
        assert!(medium.width() <= 66);
        commit.decorations.clear();
        let medium = commit_metadata(&commit, 66);
        assert!(medium.contains("Very long 界 e\u{301} 👩‍💻"));
        assert!(medium.ends_with(" · 2026-11-09 · 1 files"));
        assert!(medium.width() <= 66);
    }

    #[test]
    fn short_ref_headings_erase_longer_titles_without_moving_divider_body_or_cursor() {
        let context = super::super::RefPreviewContext {
            selected: "a".into(),
            current: Some("a".into()),
            head_at_tip: true,
            selected_is_current: true,
            tip: git2::Oid::from_str("0123456789012345678901234567890123456789").unwrap(),
        };
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            let left = (width * 3 / 10).clamp(26, 52);
            let mut frame = Frame {
                grid: CellGrid::new(width, height),
                cursor: Some((left + 9, 4, SetCursorStyle::SteadyBlock)),
            };
            panel(
                &mut frame.grid,
                left,
                1,
                width - left,
                height - 4,
                "Branches",
                true,
            );
            paint_heading(
                &mut frame.grid,
                left + 2,
                1,
                width - 1,
                "Branches",
                Style::Muted,
            );
            frame.grid.put_text_clipped(
                left + 2,
                4,
                "0123456  Short branch subject",
                Style::Default,
                width - 1,
            );
            let before = frame.grid.clone();
            let cursor = frame.cursor;
            let heading = ref_preview_heading(&context, usize::from(width - left - 4), width < 100);
            assert_eq!(heading, "HEAD→a");
            paint_heading(
                &mut frame.grid,
                left + 2,
                1,
                width - 1,
                &heading,
                Style::Muted,
            );
            let replaced = (left + 2..width - 1)
                .map(|x| frame.grid.get(x, 1).unwrap().symbol.as_str())
                .collect::<String>();
            assert_eq!(replaced.trim_end(), "HEAD→a");
            // Redrawing the enclosure restores its top border and replaces the
            // title on row two; specialized headings are painted after it.
            panel(
                &mut frame.grid,
                left,
                1,
                width - left,
                height - 4,
                &heading,
                true,
            );
            assert!((left + 2..width - 1).all(|x| frame.grid.get(x, 1).unwrap().symbol == "─"));
            paint_heading(
                &mut frame.grid,
                left + 2,
                1,
                width - 1,
                &heading,
                Style::Muted,
            );
            for y in [1, 2] {
                let text = (left + 2..width - 1)
                    .map(|x| frame.grid.get(x, y).unwrap().symbol.as_str())
                    .collect::<String>();
                assert_eq!(text.trim_end(), "HEAD→a", "{width}x{height}: {text}");
            }
            for y in 0..height {
                for x in 0..width {
                    if !([1, 2].contains(&y) && x >= left + 2 && x < width - 1) {
                        assert_eq!(frame.grid.get(x, y), before.get(x, y));
                    }
                }
            }
            assert_eq!(frame.cursor, cursor);
        }
    }

    #[test]
    fn focus_changes_styles_not_pane_rectangles_or_header_padding() {
        let mut inactive = CellGrid::new(100, 30);
        section(&mut inactive, 0, 4, 30, 6, "Branches Remotes Tags", false);
        panel(&mut inactive, 30, 1, 70, 26, "Changes", false);
        let mut active = CellGrid::new(100, 30);
        section(&mut active, 0, 4, 30, 6, "Branches Remotes Tags", true);
        panel(&mut active, 30, 1, 70, 26, "Changes", true);
        assert_eq!(inactive.get(2, 4).unwrap().style, Style::GitHeading);
        assert_eq!(active.get(2, 4).unwrap().style, Style::GitControl);
        assert_eq!(active.get(0, 4).unwrap().symbol, "┌");
        for (x, y) in [(1, 4), (29, 4), (29, 5), (1, 9), (99, 2)] {
            assert_eq!(inactive.get(x, y).unwrap().style, Style::Border);
            assert_eq!(active.get(x, y).unwrap().style, Style::Accent);
            assert_eq!(
                inactive.get(x, y).unwrap().symbol,
                active.get(x, y).unwrap().symbol
            );
        }
        for y in 2..26 {
            assert_eq!(inactive.get(30, y).unwrap().symbol, "│");
            assert_eq!(active.get(30, y).unwrap().symbol, "│");
            assert_eq!(inactive.get(30, y).unwrap().style, Style::Border);
            assert_eq!(active.get(30, y).unwrap().style, Style::Accent);
        }
        for (x, y) in [(1, 5), (28, 8)] {
            assert_eq!(inactive.get(x, y).unwrap().style, Style::GitSurface(None));
            assert_eq!(
                active.get(x, y).unwrap().style,
                Style::GitFocusedSurface(None)
            );
        }
    }

    #[test]
    fn github_help_keeps_reading_and_hidden_management_routes_visible() {
        for (width, height) in [(64, 20), (100, 30), (190, 48)] {
            for page in [1, 2] {
                let mut frame = Frame {
                    grid: CellGrid::new(width, height),
                    cursor: None,
                };
                help(&mut frame, width, height, page);
                let text = (0..height)
                    .map(|y| frame.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n");
                for expected in [
                    "Ctrl+f",
                    "n/N",
                    "Home/End",
                    "Comment / edit",
                    "Repository / inbox",
                    "Esc / ?  back",
                ] {
                    assert!(
                        text.contains(expected),
                        "{width}x{height} page {page}: missing {expected}\n{text}"
                    );
                }
                let expected = if page == 1 {
                    [
                        "v / S / D",
                        "Views / save / delete locally",
                        "Previous / next saved view",
                    ]
                } else {
                    ["H/L", "Pull request tabs", "Choose changed file"]
                };
                for expected in expected {
                    assert!(
                        text.contains(expected),
                        "{width}x{height} page {page}: missing {expected}\n{text}"
                    );
                }
            }
        }
    }

    #[test]
    fn file_kind_glyphs_honor_font_mode_without_changing_status_columns() {
        use crate::config::ExplorerIconMode;
        for name in ["src/main.rs", "orbit/queue.py", "README.md", "unknown.xyz"] {
            let path = std::path::Path::new(name);
            let (nerd, role) = file_glyph(path, ExplorerIconMode::NerdFont);
            assert_eq!(nerd.width(), 1, "{name}");
            assert_eq!(file_glyph(path, ExplorerIconMode::Unicode), ("◇", role));
            assert_eq!(file_glyph(path, ExplorerIconMode::Ascii), ("-", role));
            assert!(file_glyph(path, ExplorerIconMode::Ascii).0.is_ascii());
        }
    }

    #[test]
    fn footer_stays_one_line_and_prioritizes_actions_over_navigation() {
        for width in [64, 100, 190] {
            let mut frame = Frame {
                grid: CellGrid::new(width, 20),
                cursor: None,
            };
            footer(
                &mut frame,
                width,
                20,
                &[
                    ("Space", "stage file"),
                    ("c", "commit"),
                    ("j/k", "move"),
                    ("Tab", "focus"),
                    ("i", "index"),
                    ("/", "filter"),
                    ("x", "mark"),
                    ("V", "range"),
                ],
                false,
            );
            let row = frame.grid.row_text(18);
            assert!(
                row.contains("stage file") && row.contains("commit"),
                "{width}: {row}"
            );
            assert!(row.contains("? keys") && row.contains("refresh") && row.contains("close"));
            assert!(!row.contains("j/k") && !row.contains("Tab") && !row.contains("range"));
            assert!(frame.grid.row_text(19).trim().is_empty());
        }
    }

    #[test]
    fn git_help_is_in_the_footer_once_but_github_keeps_navigation_help() {
        for width in [64, 100, 190] {
            let mut frame = Frame {
                grid: CellGrid::new(width, 20),
                cursor: None,
            };
            navigation(&mut frame, 0, width, "repository");
            footer(&mut frame, width, 20, &[("/", "find")], false);
            let text = (0..20)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches("keys").count(), 1);
            assert!(!frame.grid.row_text(0).contains("keys"));
            navigation(&mut frame, 1, width, "repository");
            assert!(frame.grid.row_text(0).contains("? keys"));
        }
    }
}
