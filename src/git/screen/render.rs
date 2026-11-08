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
        title,
        if focused {
            Style::Accent
        } else {
            Style::Default
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
    let left = (width * 2 / 5).max(26);
    let body = height - 4;
    let sizes = [
        body * 3 / 10,
        body / 4,
        body / 4,
        body - (body * 3 / 10 + body / 4 + body / 4),
    ];
    let titles = [
        format!("1 Changes · {}", app.branch),
        format!(
            "2 {}",
            ["Branches", "Worktrees", "Remotes", "Tags"][app.subview]
        ),
        "3 Commits".into(),
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
            let style = if index == selected && app.focus == pane {
                Style::Selection
            } else if row.directory {
                Style::Accent
            } else {
                Style::Default
            };
            if style == Style::Selection {
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
            if app.staged {
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
    let visible = body.saturating_sub(2) as usize;
    let offset = app.preview_line.saturating_sub(visible.saturating_sub(1));
    let number_width = app.preview.number_width();
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
        let mut column = 0;
        for (at, grapheme) in row.text.grapheme_indices(true) {
            let size = if grapheme == "\t" {
                4 - column % 4
            } else {
                UnicodeWidthStr::width(grapheme).max(1)
            };
            let end = column + size;
            if end > app.preview_column {
                let style = row
                    .spans
                    .iter()
                    .find(|(range, _)| range.start <= at && at < range.end)
                    .map(|(_, role)| Style::Syntax(*role))
                    .unwrap_or(row.style);
                let next = if grapheme == "\t" || column < app.preview_column {
                    frame.grid.put_text_clipped(
                        x,
                        y,
                        &" ".repeat(end - column.max(app.preview_column)),
                        style,
                        width - 1,
                    )
                } else {
                    frame
                        .grid
                        .put_text_clipped(x, y, grapheme, style, width - 1)
                };
                if next == x {
                    break;
                }
                x = next;
            }
            column = end;
        }
    }
    if app.focus == 4 {
        frame.cursor = Some((
            left + 2,
            2 + (app
                .preview_line
                .saturating_sub(offset)
                .min(visible.saturating_sub(1))) as u16,
            SetCursorStyle::SteadyBlock,
        ));
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
            "Preview · j/k scroll · h/l horizontal · Ctrl+h back · ? keys · q close"
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

pub(super) fn navigation(frame: &mut Frame, page: usize, width: u16, repository: &str) {
    let mut x = frame
        .grid
        .put_text_clipped(1, 0, "Dusty Git  ", Style::Accent, width);
    for (index, label) in ["Git", "Issues", "Pull requests", "Actions"]
        .iter()
        .enumerate()
    {
        x = frame.grid.put_text_clipped(
            x,
            0,
            &format!("{label}  "),
            if index == page {
                Style::Selection
            } else {
                Style::Muted
            },
            width,
        );
    }
    frame
        .grid
        .put_text_clipped(x, 0, repository, Style::Muted, width);
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
