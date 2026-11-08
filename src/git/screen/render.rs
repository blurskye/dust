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
    let style = Style::Border;
    for col in x..x + w {
        grid.set_cell(col, y, "─", style);
        grid.set_cell(col, y + h - 1, "─", style);
    }
    for row in y..y + h {
        grid.set_cell(x, row, "│", style);
        grid.set_cell(x + w - 1, row, "│", style);
    }
    for (col, row, text) in [
        (x, y, "╭"),
        (x + w - 1, y, "╮"),
        (x, y + h - 1, "╰"),
        (x + w - 1, y + h - 1, "╯"),
    ] {
        grid.set_cell(col, row, text, style);
    }
    grid.put_text_clipped(
        x + 2,
        y,
        title,
        if focused {
            Style::GitSelected(None)
        } else {
            Style::Muted
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
    navigation(&mut frame, 0, width, &app.root.display().to_string());
    let left = (width / 3).clamp(26, 40);
    let body = height - 4 - if app.tutor.is_some() { 4 } else { 0 };
    let sizes = [
        body * 3 / 10,
        body / 4,
        body / 4,
        body - (body * 3 / 10 + body / 4 + body / 4),
    ];
    let titles = [
        format!(
            "1 {} · {}",
            if app.all_files {
                "All files"
            } else {
                "Changes"
            },
            app.branch
        ),
        format!(
            "2 {}",
            ["Branches", "Worktrees", "Remotes", "Tags"][app.subview]
        ),
        format!(
            "3 Commits · {}",
            app.history_target
                .as_ref()
                .map(|(root, reference)| if reference == "HEAD" {
                    root.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    reference.to_string_lossy().into_owned()
                })
                .unwrap_or_else(|| app.branch.clone())
        ),
        "4 Stashes".into(),
    ];
    let mut top = 1;
    for pane in 0..4 {
        let h = sizes[pane];
        border(
            &mut frame.grid,
            0,
            top,
            left,
            h,
            &titles[pane],
            app.focus == pane,
        );
        let rows = app.rows(pane);
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
            let style = if index == selected {
                Style::GitSelected(None)
            } else if row.directory {
                Style::Accent
            } else {
                Style::Default
            };
            if index == selected {
                for x in 1..left - 1 {
                    frame.grid.set_cell(x, y, " ", style);
                }
            }
            frame
                .grid
                .put_text_clipped(2, y, &row.label, style, left - 1);
            if index == selected && app.focus == pane {
                frame.cursor = Some((2, y, SetCursorStyle::SteadyBlock));
            }
        }
        if rows.is_empty() {
            frame.grid.put_text_clipped(
                2,
                top + 1,
                if pane == 0 && app.refreshing {
                    "Loading…"
                } else {
                    "—"
                },
                Style::Muted,
                left - 1,
            );
            if app.focus == pane {
                frame.cursor = Some((2, top + 1, SetCursorStyle::SteadyBlock));
            }
        }
        top += h;
    }
    let title = match app.last_list {
        0 => {
            if app.all_files {
                "Source"
            } else if app.staged {
                "Index"
            } else {
                "Working tree"
            }
        }
        1 => ["Branch log", "Worktree log", "Remote URLs", "Tag log"][app.subview],
        2 => "Commit",
        _ => "Stash",
    };
    border(
        &mut frame.grid,
        left,
        1,
        width - left,
        body,
        title,
        app.focus == 4,
    );
    let split = app.split_diff && app.preview.patch && width - left >= 96;
    app.preview_rows = if split {
        app.preview.split.len()
    } else {
        app.preview.rows.len()
    };
    app.preview_line = app.preview_line.min(app.preview_rows.saturating_sub(1));
    let first_line = if split { 3 } else { 2 };
    let visible = body.saturating_sub(first_line) as usize;
    let offset = app.preview_line.saturating_sub(visible.saturating_sub(1));
    let number_width = app.preview.number_width();
    if split {
        let middle = left + (width - left) / 2;
        frame
            .grid
            .put_text_clipped(left + 2, 2, "BEFORE", Style::Muted, middle);
        frame
            .grid
            .put_text_clipped(middle + 2, 2, "AFTER", Style::Muted, width - 1);
        for (i, pair) in app
            .preview
            .split
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
        {
            let y = first_line + (i - offset) as u16;
            if let Some(index) = pair.heading {
                paint_source(
                    &mut frame.grid,
                    &app.preview.rows[index],
                    left + 2,
                    y,
                    width - 1,
                    app.preview_column,
                    None,
                );
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
                        Style::Muted,
                        right,
                    );
                    let x = frame.grid.put_text_clipped(
                        x,
                        y,
                        &row.marker.unwrap_or(' ').to_string(),
                        row.style,
                        right,
                    );
                    paint_source(
                        &mut frame.grid,
                        row,
                        x,
                        y,
                        right,
                        app.preview_column,
                        match row.marker {
                            Some('+') => Some(true),
                            Some('-') => Some(false),
                            _ => None,
                        },
                    );
                }
            }
        }
    } else {
        for (i, row) in app
            .preview
            .rows
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
        {
            let y = 2 + (i - offset) as u16;
            let old = row.old.map(|number| number.to_string()).unwrap_or_default();
            let new = row.new.map(|number| number.to_string()).unwrap_or_default();
            let mut x = left + 2;
            if row.old.is_some() || row.new.is_some() {
                let numbers = if app.preview.patch {
                    format!("{old:>number_width$} {new:>number_width$} ")
                } else {
                    format!("{new:>number_width$} │ ")
                };
                x = frame
                    .grid
                    .put_text_clipped(x, y, &numbers, Style::Muted, width - 1);
            }
            if let Some(marker) = row.marker {
                x = frame
                    .grid
                    .put_text_clipped(x, y, &marker.to_string(), row.style, width - 1);
            }
            paint_source(
                &mut frame.grid,
                row,
                x,
                y,
                width - 1,
                app.preview_column,
                None,
            );
        }
    }
    if app.focus == 4 {
        frame.cursor = Some((
            left + 2,
            first_line
                + (app
                    .preview_line
                    .saturating_sub(offset)
                    .min(visible.saturating_sub(1))) as u16,
            SetCursorStyle::SteadyBlock,
        ));
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
    frame.grid.put_text_clipped(
        1,
        height - 3,
        if app.busy {
            "Git operation running…"
        } else {
            &app.notice
        },
        Style::Warning,
        width - 1,
    );
    frame.grid.put_text_clipped(
        1,
        height - 2,
        if app.focus == 4 {
            "Preview · j/k scroll · h/l horizontal · s split/unified · Ctrl+h back · q close"
        } else {
            "[ / ] pages · 1–4 panes · Ctrl+h/j/k/l focus · Space stage · c commit · ? keys · q close"
        },
        Style::Muted,
        width - 1,
    );
    if let Some(form) = &app.form {
        render_form(&mut frame, form, width, height);
    }
    frame
}

pub(super) fn paint_source(
    grid: &mut CellGrid,
    row: &super::preview::PreviewRow,
    mut x: u16,
    y: u16,
    right: u16,
    offset: usize,
    diff: Option<bool>,
) {
    if let Some(added) = diff {
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
            let style = diff.map_or_else(
                || role.map(Style::Syntax).unwrap_or(row.style),
                |added| Style::GitDiff {
                    added,
                    syntax: role,
                },
            );
            let next = if grapheme == "\t" || column < offset {
                grid.put_text_clipped(x, y, &" ".repeat(end - column.max(offset)), style, right)
            } else {
                grid.put_text_clipped(x, y, grapheme, style, right)
            };
            if next == x {
                break;
            }
            x = next;
        }
        column = end;
    }
}

pub(super) fn navigation(frame: &mut Frame, page: usize, width: u16, repository: &str) {
    for x in 0..width {
        frame.grid.set_cell(x, 0, " ", Style::Status);
    }
    let mut x = frame
        .grid
        .put_text_clipped(1, 0, "Dusty Git  ", Style::StatusAccent, width);
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
            width,
        );
    }
    frame
        .grid
        .put_text_clipped(x + 1, 0, repository, Style::StatusMuted, width);
}

pub(super) fn render_form(frame: &mut Frame, form: &super::Form, width: u16, height: u16) {
    let w = width.saturating_sub(6).min(90);
    let total = form
        .fields
        .iter()
        .map(|field| if field.multiline { 6 } else { 3 })
        .sum::<u16>();
    let h = (total + 5).min(height.saturating_sub(2));
    let x = (width - w) / 2;
    let y = (height - h) / 2;
    for row in y..y + h {
        for col in x..x + w {
            frame.grid.set_cell(col, row, " ", Style::Default);
        }
    }
    border(&mut frame.grid, x, y, w, h, &form.title, true);
    let chosen_bottom = form
        .fields
        .iter()
        .take((form.selected + 1).min(form.fields.len()))
        .map(|field| if field.multiline { 6 } else { 3 })
        .sum::<u16>();
    let scroll = chosen_bottom.saturating_sub(h.saturating_sub(5));
    let mut at = 0u16;
    for (i, field) in form.fields.iter().enumerate() {
        let size = if field.multiline { 6 } else { 3 };
        if at < scroll || at + size > scroll + h.saturating_sub(4) {
            at += size;
            continue;
        }
        let row = y + 1 + at - scroll;
        at += size;
        let label = format!(
            "{}{}{}",
            field.label,
            if field.required { " *" } else { "" },
            if field.choices.is_empty() {
                ""
            } else {
                " · ↑/↓ choose"
            }
        );
        frame
            .grid
            .put_text_clipped(x + 2, row, &label, Style::Muted, x + w - 2);
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
                    Style::Selection
                } else {
                    Style::Default
                },
                x + w - 2,
            );
        }
        if form.selected == i {
            frame.cursor = Some((
                x + 2 + column as u16,
                row + 1 + (cursor_line - first) as u16,
                SetCursorStyle::BlinkingBar,
            ));
        }
    }
    let row = y + h - 2;
    let count = form.fields.len();
    frame.grid.put_text(
        x + 2,
        row,
        "[ Submit ]",
        if form.selected == count {
            Style::Selection
        } else {
            Style::Accent
        },
    );
    frame.grid.put_text(
        x + 15,
        row,
        "[ Cancel ]",
        if form.selected == count + 1 {
            Style::Selection
        } else {
            Style::Default
        },
    );
    frame.grid.put_text_clipped(
        x + 28,
        row,
        "Tab · Ctrl+Enter · Esc",
        Style::Muted,
        x + w - 2,
    );
    if form.selected >= count {
        frame.cursor = Some((
            x + if form.selected == count { 3 } else { 16 },
            row,
            SetCursorStyle::SteadyBlock,
        ));
    }
}
