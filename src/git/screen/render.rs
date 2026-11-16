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
                | super::FormAction::Run(_)
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

fn heading_style(focused: bool) -> Style {
    if focused {
        Style::GitControl
    } else {
        Style::GitHeading
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
            "/  Filter",
            "H/L  Branches / Remotes / Tags",
            "Tab / Ctrl+h/l  Sidebar / preview focus",
            "r  Refresh",
            "Esc / q  Clear filter / close",
        ]
    } else if page == 7 {
        vec![
            "d  Confirm deletion of selected LOCAL tag",
            "Safety  No remote deletion, push or checkout",
            "Enter  Inspect selected tag",
            "/  Filter",
            "H/L  Branches / Remotes / Tags",
            "Tab / Ctrl+h/l  Sidebar / preview focus",
            "r  Refresh",
            "Esc / q  Clear filter / close",
        ]
    } else if page == 5 {
        vec![
            "n  Create rescue branch at selected commit",
            "o  Confirm detached checkout of selected commit",
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
            "|  Toggle optional split view",
            "e  Open this line in the editor",
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
            "s u - / x / V  List: stage, unstage, toggle set / mark / range",
            "s u - / a / V  Diff: hunk / exact line / range",
            "d/D discard  Unstaged hunk/line; V range; confirm",
            "v / i / s  All files / index · Split / unified",
            "c/P/C · Enter/m  Commit/push/merge · open/message",
            "n / d  New branch, worktree, stash / remove",
            "s - / u  Submodules: pointer / init-update",
            "f · a/p/d  Fetch · stash apply/pop/drop",
        ]
    } else if page == 0 {
        vec![
            "[ / ]  Switch page",
            "1–4 / Tab  Select pane",
            "Ctrl+h/j/k/l  Focus panes",
            "/  List: filter; preview: find literal source",
            "n/N / Esc (preview)  Next/previous match / clear find",
            "s u - / c / P  Stage, unstage, toggle / commit / push",
            "A/W/F  Amend / reword HEAD / fixup selected commit",
            "m / R (branches)  Confirm merge / rebase onto",
            "C / S / X  Continue / skip rebase commit / abort",
            "x / V (Changes list)  Mark file / visible range",
            "j/k (range)  Extend · V finish · Esc clear marks",
            "s u - (list)  Stage, unstage, toggle marked set, else file / directory",
            "*  Marked file / directory with marked descendants",
            "s u - (diff)  Stage / unstage / toggle current hunk",
            "a (diff)  Stage / unstage exact line or range",
            "e (diff)  Open this line in the editor",
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
            "|  Split / unified preview",
            "n  New branch / worktree / stash in its pane",
            "d (Worktrees)  Remove a clean, unlocked worktree",
            "s - / u (Submodules)  Stage or toggle pointer / init-update",
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
    let super::FormAction::Operation {
        action, operation, ..
    } = &form.action
    else {
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
    backdrop(frame, x, y, w, h);
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
    // The button names the operation: `Continue rebase`, `Abort cherry-pick`, `Skip commit`.
    let label = super::conflicts::action_label(*action, operation);
    let blocked = form
        .fields
        .iter()
        .any(|field| field.label == "Unrelated changes");
    let label = if blocked {
        format!("Retry {}", label.to_lowercase())
    } else {
        label
    };
    let cancel_selected = form.selected == form.fields.len() + 1;
    super::shell::sheet::button_pair(
        frame,
        x + 2,
        y + h - 3,
        right,
        [label.as_str(), "Cancel"],
        Some(usize::from(cancel_selected)),
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
    backdrop(frame, x, y, w, h);
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
    let after = super::shell::sheet::button_pair(
        frame,
        x + 2,
        row,
        x + w - 2,
        ["Open file", "Cancel"],
        form.selected.checked_sub(1),
    );
    super::shell::sheet::button_hints(frame, after, row, x + w - 2, "Ctrl+Enter open · Esc cancel");
    if form.selected == 0 {
        frame.cursor = Some((
            x + 4,
            y + 1 + (selected - first) as u16,
            SetCursorStyle::SteadyBlock,
        ));
    }
    true
}

/// What is behind a dialog recedes to the border colour, as behind every sheet: the box
/// is opaque and the panes stay in view, not blanked. The top bar and footer stay live.
fn backdrop(frame: &mut Frame, x: u16, y: u16, w: u16, h: u16) {
    let (width, height) = (frame.grid.width(), frame.grid.height());
    super::shell::chrome::scrim_within(
        &mut frame.grid,
        super::shell::layout::Rect::new(x, y, w, h),
        super::shell::layout::Rect::new(0, 1, width, height.saturating_sub(2)),
    );
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
        super::FormAction::Run(op) => (op.button(), "[ Cancel ]"),
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
    // The one button pair every sheet draws; the labels lose their plain-text brackets,
    // which the pair adds back.
    let unbracket = |label: &'static str| label.trim_start_matches("[ ").trim_end_matches(" ]");
    let after = super::shell::sheet::button_pair(
        frame,
        x + 2,
        row,
        x + w - 2,
        [unbracket(submit), unbracket(cancel)],
        form.selected.checked_sub(count).filter(|index| *index < 2),
    );
    super::shell::sheet::button_hints(
        frame,
        after,
        row,
        x + w - 2,
        if matches!(form.action, super::FormAction::SourceSearch) {
            "Enter find · Esc cancel"
        } else if matches!(
            form.action,
            super::FormAction::Discard(_) | super::FormAction::DiscardFiles(_)
        ) {
            "Enter discard · Esc cancel"
        } else {
            "Ctrl+Enter confirm · Tab next · Esc cancel"
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
                        // The focused button is the shared filled chip.
                        assert_eq!(
                            frame.grid.get(cx, cy).unwrap().style,
                            super::super::shell::chrome::bold(
                                crate::ui::GitRole::OnFocus,
                                crate::ui::GitSurface::ChipActive
                            )
                        );
                    }
                    // Behind the dialog the screen recedes to the border colour, its text
                    // kept; a one-cell margin is cleared; the top bar and footer stay.
                    let dimmed = super::super::shell::chrome::st(
                        crate::ui::GitRole::Border,
                        crate::ui::GitSurface::Canvas,
                    );
                    for row in 0..height {
                        for col in 0..width {
                            if !(x..x + w).contains(&col) || !(y..y + h).contains(&row) {
                                let cell = frame.grid.get(col, row).unwrap();
                                let margin =
                                    col + 1 >= x && col <= x + w && row + 1 >= y && row <= y + h;
                                if row == 0 || row + 1 == height {
                                    assert_eq!(cell.symbol, ".");
                                    assert_eq!(cell.style, Style::Warning);
                                } else {
                                    assert_eq!(cell.symbol, if margin { " " } else { "." });
                                    assert_eq!(cell.style, dimmed);
                                }
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
}
