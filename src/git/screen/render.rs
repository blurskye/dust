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
    navigation(
        &mut frame,
        0,
        width,
        &app.root.file_name().unwrap_or_default().to_string_lossy(),
    );
    let left = (width / 3).clamp(26, 40);
    let body = height - 4 - if app.tutor.is_some() { 4 } else { 0 };
    let mut sizes = [4, 4, 4, 3];
    let wanted = std::array::from_fn::<_, 4, _>(|pane| {
        u16::try_from(app.rows(pane).len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .max(sizes[pane])
    });
    let mut remaining = body.saturating_sub(sizes.iter().sum());
    while remaining > 0 {
        let before = remaining;
        for pane in [0, 2, 1, 3] {
            if remaining > 0 && sizes[pane] < wanted[pane] {
                sizes[pane] += 1;
                remaining -= 1;
            }
        }
        if before == remaining {
            break;
        }
    }
    for y in 1..body + 1 {
        for x in 0..left {
            frame.grid.set_cell(x, y, " ", Style::GitSurface(None));
        }
    }
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
        section(
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
                if app.focus == pane {
                    Style::GitSelected(None)
                } else {
                    Style::GitInactiveSelection(None)
                }
            } else if row.directory {
                Style::Accent
            } else {
                Style::GitSurface(None)
            };
            if index == selected {
                for x in 1..left - 1 {
                    frame.grid.set_cell(x, y, " ", style);
                }
            }
            frame.grid.put_text_clipped(
                2,
                y,
                &ellipsize(&row.label, usize::from(left - 3)),
                style,
                left - 1,
            );
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
                    [
                        "Working tree clean",
                        "No references",
                        "No commits yet",
                        "No stashes · n save",
                    ][pane]
                },
                Style::GitSurface(Some(crate::config::Highlight::COMMENT)),
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
                match row.marker {
                    Some('+') => Some(true),
                    Some('-') => Some(false),
                    _ => None,
                },
            );
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
    footer(
        &mut frame,
        width,
        height,
        if app.focus == 4 {
            &[
                ("j/k", "scroll"),
                ("h/l", "pan"),
                ("s", "split"),
                ("Ctrl+h", "back"),
                ("[ ]", "pages"),
            ]
        } else {
            &[
                ("Tab", "focus"),
                ("Space", "stage"),
                ("c", "commit"),
                ("i", "index"),
                ("[ ]", "pages"),
                ("1–4", "panes"),
                ("Ctrl+h/j/k/l", "focus"),
            ]
        },
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
    if row.style == Style::GitHeading {
        for at in x..right {
            grid.set_cell(at, y, " ", Style::GitHeading);
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
                grid.set_cell(right - 1, y, "›", Style::Muted);
                break;
            }
            x = next;
        }
        column = end;
    }
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

pub(super) fn section(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    title: &str,
    focused: bool,
) {
    for row in y..y + h {
        for col in x..x + w {
            grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    for col in x + 1..x + w - 1 {
        grid.set_cell(col, y + h - 1, "─", Style::Border);
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
    if focused {
        grid.set_cell(x, y, "▎", Style::Accent);
    }
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

pub(super) fn footer(frame: &mut Frame, width: u16, height: u16, hints: &[(&str, &str)]) {
    let y = height - 2;
    for x in 0..width {
        frame.grid.set_cell(x, y, " ", Style::Status);
    }
    let fixed = [("?", "keys"), ("r", "refresh"), ("q", "close")];
    let size = |(key, label): &(&str, &str)| (key.width() + label.width() + 4) as u16;
    let right = width.saturating_sub(fixed.iter().map(size).sum::<u16>() + 1);
    let draw = |frame: &mut Frame, x, key, label| {
        let x = frame
            .grid
            .put_text_clipped(x, y, &format!(" {key} "), Style::GitControl, width);
        frame
            .grid
            .put_text_clipped(x, y, &format!(" {label} "), Style::StatusMuted, width)
    };
    let mut x = 1;
    for &(key, label) in hints {
        if x + size(&(key, label)) <= right {
            x = draw(frame, x, key, label);
        }
    }
    x = right;
    for (key, label) in fixed {
        x = draw(frame, x, key, label);
    }
}

pub(super) fn help(frame: &mut Frame, width: u16, height: u16, page: usize) {
    if width < 40 || height < 18 {
        return;
    }
    let mut rows = if page == 0 {
        vec![
            "[ / ]  Switch page",
            "1–4 / Tab  Select pane",
            "Ctrl+h/j/k/l  Focus panes",
            "j/k or ↑/↓  Move selection",
            "h/l  Tree / horizontal scroll",
            "H/L  Branches / worktrees / remotes / tags",
            "v / i  All files / index",
            "Space / c / P  Stage / commit / push",
            "s  Split / unified preview",
            "n  New worktree / stash",
            "a / p / d  Apply / pop / drop stash",
            "o / t / C  Conflict ours / theirs / continue",
            ":tutor git  Disposable Git lessons",
            "Enter  Open selection",
        ]
    } else {
        vec![
            "[ / ]  Switch page",
            "Ctrl+h/l  List / details",
            "j/k or ↑/↓  Move selection",
            "Tab / Shift+Tab  Next / previous control",
            "Enter  Open / expand",
            "h/l or ←/→  Horizontal scroll",
            "PgUp/PgDn  Scroll details",
            "Home / End  First / last detail line",
            "r  Refresh",
        ]
    };
    if page == 1 || page == 2 {
        rows.extend([
            "/  Filter list",
            "n  New issue / pull request",
            "c / e  Comment / edit",
        ]);
    }
    if page == 2 {
        rows.push("H/L  Pull request tabs");
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
    let mut x = frame
        .grid
        .put_text_clipped(1, 0, "Dusty Git  ", Style::GitControl, width);
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
    frame.grid.put_text_clipped(
        x + 1,
        0,
        &ellipsize(repository, usize::from(width.saturating_sub(x + 2))),
        Style::StatusMuted,
        width,
    );
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
            frame.grid.set_cell(col, row, " ", Style::GitSurface(None));
        }
    }
    border(&mut frame.grid, x, y, w, h, &form.title, true);
    if !form.fields.is_empty() {
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
        "Tab · Ctrl+Enter · Esc",
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
