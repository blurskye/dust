//! Sheets (DESIGN.md §2.7, §3.4): forms, confirmations and the commit composer as
//! centred overlays with an accent border, labelled inputs and at most three buttons.
use super::chrome::{self, Span, bold, st};
use super::glyphs::{Glyph, glyph};
use super::layout::{Layout, Rect};
use super::super::{Field, Form, FormAction, GitScreen};
use crate::ui::{Frame, GitChip, GitRole, GitSurface};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SURFACE: GitSurface = GitSurface::Sheet;

/// Paints the open form, if this module draws it. Returns false for the forms that
/// keep their old painters (operation sheets and the changed-file picker).
pub(in crate::git::screen) fn paint(app: &GitScreen, frame: &mut Frame, lay: &Layout) -> bool {
    let Some(form) = &app.form else {
        return true;
    };
    match &form.action {
        FormAction::Operation { .. } | FormAction::Github(super::super::github::Action::Files(_)) => {
            false
        }
        FormAction::Commit => {
            commit(app, frame, form, lay);
            true
        }
        _ => {
            generic(app, frame, form, lay);
            true
        }
    }
}

/// The accent frame every sheet shares; returns its interior.
fn frame_box(frame: &mut Frame, rect: Rect, title: &[Span], right: &str) -> Rect {
    let grid = &mut frame.grid;
    chrome::fill(grid, rect, st(GitRole::Text, SURFACE));
    let edge = st(GitRole::Focus, SURFACE);
    let (right_x, bottom) = (rect.right() - 1, rect.bottom() - 1);
    for x in rect.x + 1..right_x {
        grid.set_cell(x, rect.y, "─", edge);
        grid.set_cell(x, bottom, "─", edge);
    }
    for y in rect.y + 1..bottom {
        grid.set_cell(rect.x, y, "│", edge);
        grid.set_cell(right_x, y, "│", edge);
    }
    grid.set_cell(rect.x, rect.y, "╭", edge);
    grid.set_cell(right_x, rect.y, "╮", edge);
    grid.set_cell(rect.x, bottom, "╰", edge);
    grid.set_cell(right_x, bottom, "╯", edge);
    let mut spans = vec![(" ".to_owned(), edge)];
    spans.extend_from_slice(title);
    spans.push((" ".into(), edge));
    chrome::put_spans(grid, rect.x + 2, rect.y, &spans, right_x.saturating_sub(1));
    if !right.is_empty() {
        let label = format!(" {right} ");
        let at = right_x.saturating_sub(chrome::width(&label) + 1);
        grid.put_text_clipped(at, rect.y, &label, st(GitRole::Muted, SURFACE), right_x);
    }
    Rect::new(rect.x + 2, rect.y + 1, rect.w.saturating_sub(4), rect.h.saturating_sub(2))
}

fn title_spans(app: &GitScreen, glyph_kind: Glyph, title: &str) -> Vec<Span> {
    vec![
        (
            format!("{} ", glyph(glyph_kind, app.icon_mode)),
            st(GitRole::Focus, SURFACE),
        ),
        (title.to_owned(), bold(GitRole::Text, SURFACE)),
    ]
}

/// One labelled input. `rows` is the input height (1 for single-line fields).
pub(in crate::git::screen) fn input(frame: &mut Frame, field: &Field, rect: Rect, focused: bool) {
    let surface = if focused { GitSurface::CursorInactive } else { GitSurface::Canvas };
    chrome::fill(&mut frame.grid, rect, st(GitRole::Text, surface));
    if rect.x > 0 {
        let edge = if focused {
            bold(GitRole::Focus, SURFACE)
        } else {
            st(GitRole::Border, SURFACE)
        };
        for y in rect.y..rect.bottom() {
            frame.grid.set_cell(rect.x - 1, y, "▎", edge);
        }
    }
    let text = &field.input.input;
    let cursor = field.input.cursor.min(text.len());
    let cursor_line = text[..cursor].bytes().filter(|&b| b == b'\n').count();
    let lines = text.split('\n').collect::<Vec<_>>();
    let visible = usize::from(rect.h.max(1));
    let first = cursor_line.saturating_sub(visible - 1);
    let before = text[..cursor].rsplit('\n').next().unwrap_or_default();
    // Scroll the cursor line horizontally so the cursor is always on screen.
    let room = usize::from(rect.w.saturating_sub(2));
    let mut start = before.len();
    let mut column = 0;
    for (at, grapheme) in before.grapheme_indices(true).rev() {
        let size = grapheme.width().max(1);
        if column + size > room {
            break;
        }
        start = at;
        column += size;
    }
    if text.is_empty() && !field.read_only {
        let hint = match field.label.as_str() {
            "Subject" => "Summarise the change",
            "Body" => "Why this change, in a sentence or two (optional)",
            _ => "",
        };
        frame.grid.put_text_clipped(
            rect.x + 2,
            rect.y,
            hint,
            st(GitRole::Muted, surface),
            rect.right(),
        );
    }
    for (offset, line) in lines.iter().enumerate().skip(first).take(visible) {
        let shown = if offset == cursor_line { &line[start..] } else { line };
        frame.grid.put_text_clipped(
            rect.x + 1,
            rect.y + (offset - first) as u16,
            shown,
            st(if field.read_only { GitRole::Muted } else { GitRole::Text }, surface),
            rect.right().saturating_sub(1),
        );
    }
    if focused {
        frame.cursor = Some((
            rect.x + 1 + column as u16,
            rect.y + (cursor_line - first) as u16,
            if field.read_only {
                SetCursorStyle::SteadyBlock
            } else {
                SetCursorStyle::BlinkingBar
            },
        ));
    }
}

/// The options of a closed choice on one row, the chosen one filled; a long set stacks one
/// option per row so none is clipped.
fn radio(frame: &mut Frame, field: &Field, x: u16, y: u16, right: u16, focused: bool) {
    if stacked(field) {
        let chosen = field.choices.iter().position(|choice| *choice == field.input.input).unwrap_or(0);
        let first = chosen.saturating_sub(STACKED - 1).min(field.choices.len().saturating_sub(STACKED));
        for (row, (index, choice)) in field.choices.iter().enumerate().skip(first).take(STACKED).enumerate() {
            let y = y + row as u16;
            let more = (row == 0 && first > 0) || (row == STACKED - 1 && first + STACKED < field.choices.len());
            let label = chrome::ellipsize(choice, usize::from(right.saturating_sub(x + 6)));
            if index == chosen {
                let style = if focused {
                    bold(GitRole::OnFocus, GitSurface::ChipActive)
                } else {
                    bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus))
                };
                frame.grid.put_text_clipped(x, y, &format!(" ● {label} "), style, right);
                if focused {
                    frame.cursor = Some((x + 1, y, SetCursorStyle::SteadyBlock));
                }
            } else {
                let next = frame.grid.put_text_clipped(x, y, &format!(" ○ {label} "), st(GitRole::Text, SURFACE), right);
                if more {
                    frame.grid.put_text_clipped(next + 1, y, "⋯", st(GitRole::Muted, SURFACE), right);
                }
            }
        }
        return;
    }
    let mut at = x;
    for choice in &field.choices {
        let chosen = *choice == field.input.input;
        let (text, style) = if chosen {
            (
                format!(" ● {choice} "),
                if focused {
                    bold(GitRole::OnFocus, GitSurface::ChipActive)
                } else {
                    bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus))
                },
            )
        } else {
            (format!(" ○ {choice} "), st(GitRole::Text, SURFACE))
        };
        let start = at;
        at = frame.grid.put_text_clipped(at, y, &text, style, right) + 1;
        if chosen && focused {
            frame.cursor = Some((start + 1, y, SetCursorStyle::SteadyBlock));
        }
    }
}

/// `[ primary ]  [ Cancel ]   hints`, on one row.
fn buttons(frame: &mut Frame, form: &Form, x: u16, y: u16, right: u16, primary: &str, hints: &str) {
    let count = form.fields.len();
    let mut at = x;
    for (index, label) in [primary, "Cancel"].into_iter().enumerate() {
        let selected = form.selected == count + index;
        let text = format!(" {label} ");
        let style = if selected {
            bold(GitRole::OnFocus, GitSurface::ChipActive)
        } else if index == 0 {
            bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus))
        } else {
            st(GitRole::Text, GitSurface::Chip(GitChip::Muted))
        };
        let next = frame.grid.put_text_clipped(at, y, &text, style, right);
        if selected {
            frame.cursor = Some((at + 1, y, SetCursorStyle::SteadyBlock));
        }
        at = next + 2;
    }
    // Whole hints only: one that does not fit is left out, never cut mid-word.
    let room = right.saturating_sub(at + 1);
    let mut spans = Vec::new();
    for (index, part) in hints.split(" · ").enumerate() {
        let mut hint: Vec<Span> = Vec::new();
        if index > 0 {
            hint.push((" · ".into(), st(GitRole::Muted, SURFACE)));
        }
        match part.split_once(' ') {
            Some((key, what)) => {
                hint.push((key.to_owned(), bold(GitRole::Focus, SURFACE)));
                hint.push((format!(" {what}"), st(GitRole::Muted, SURFACE)));
            }
            None => hint.push((part.to_owned(), st(GitRole::Muted, SURFACE))),
        }
        if chrome::spans_width(&spans) + chrome::spans_width(&hint) > room {
            break;
        }
        spans.extend(hint);
    }
    chrome::put_spans(&mut frame.grid, at + 1, y, &spans, right);
}

fn primary_label(form: &Form) -> &'static str {
    use super::super::github::Action;
    use super::super::refs::Action as Ref;
    match &form.action {
        FormAction::Github(Action::CreatePullRequest) => "Review →",
        FormAction::Github(Action::ConfirmPullRequest(request)) if request.push.is_some() => {
            "Push + create PR"
        }
        FormAction::Github(Action::ConfirmPullRequest(_)) => "Create PR",
        FormAction::Github(Action::SaveView(..)) => "Save view",
        FormAction::Rewrite(plan) => match plan.kind {
            crate::git::rewrite::Kind::Amend => "Amend",
            crate::git::rewrite::Kind::Reword => "Reword",
            crate::git::rewrite::Kind::Fixup(_) => "Fixup",
        },
        FormAction::SourceSearch => "Find",
        FormAction::Discard(_) | FormAction::DiscardFiles(_) => "Discard",
        FormAction::Reference(Ref::MergeBranch { .. }) => "Merge",
        FormAction::Reference(Ref::RebaseBranch { .. }) => "Rebase",
        FormAction::Stash => "Stash",
        FormAction::Branch(_) => "Create",
        _ => "Confirm",
    }
}

/// Most options a stacked choice shows at once; the chosen one is kept in view.
const STACKED: usize = 7;

/// Whether a closed choice is too long for one row and reads as a list instead.
fn stacked(field: &Field) -> bool {
    !field.custom_choices
        && field.choices.iter().map(|choice| usize::from(chrome::width(choice)) + 5).sum::<usize>() > 52
}

fn field_rows(field: &Field) -> u16 {
    if field.read_only {
        1
    } else if !field.choices.is_empty() && stacked(field) {
        1 + field.choices.len().min(STACKED) as u16
    } else if field.multiline {
        // Label and four input rows.
        5
    } else {
        2
    }
}

fn generic(app: &GitScreen, frame: &mut Frame, form: &Form, lay: &Layout) {
    let body = lay.body;
    let content = form
        .fields
        .iter()
        .map(|field| field_rows(field) + u16::from(!field.read_only))
        .sum::<u16>();
    let status = status_line(app, form);
    let w = (body.w * 6 / 10).clamp(56.min(body.w), 110.min(body.w));
    // Top padding, fields, an optional status row, the button row, borders.
    let h = (content + 1 + u16::from(status.is_some()) + 1 + 2).min(body.h.saturating_sub(2)).max(5);
    let rect = Rect::new(
        body.x + (body.w - w) / 2,
        body.y + body.h.saturating_sub(h) / 2,
        w,
        h,
    );
    let position = if form.fields.len() > 1 {
        format!(
            "{}/{}",
            (form.selected + 1).min(form.fields.len()),
            form.fields.len()
        )
    } else {
        String::new()
    };
    let glyph_kind = match &form.action {
        FormAction::Discard(_) | FormAction::DiscardFiles(_) => Glyph::Warning,
        FormAction::Stash => Glyph::Stash,
        FormAction::Branch(_) | FormAction::Reference(_) => Glyph::Branch,
        FormAction::SourceSearch => Glyph::Search,
        _ => Glyph::Commit,
    };
    let inner = frame_box(frame, rect, &title_spans(app, glyph_kind, &form.title), &position);
    let fields_bottom = inner.bottom().saturating_sub(1 + u16::from(status.is_some()));
    // Scroll so the selected field is fully visible.
    let selected_bottom = form
        .fields
        .iter()
        .take((form.selected + 1).min(form.fields.len()))
        .map(|field| field_rows(field) + u16::from(!field.read_only))
        .sum::<u16>();
    let room = fields_bottom.saturating_sub(inner.y + 1);
    let scroll = selected_bottom.saturating_sub(room);
    let mut at = 0u16;
    for (index, field) in form.fields.iter().enumerate() {
        let rows = field_rows(field) + u16::from(!field.read_only);
        let top = at;
        at += rows;
        if top < scroll || top + rows - scroll > room {
            continue;
        }
        let y = inner.y + 1 + top - scroll;
        let focused = form.selected == index;
        if field.read_only {
            let x = frame.grid.put_text_clipped(
                inner.x + 1,
                y,
                &format!("{}  ", field.label),
                st(GitRole::Muted, SURFACE),
                inner.right(),
            );
            let value = field.input.input.lines().next().unwrap_or_default();
            frame.grid.put_text_clipped(
                x,
                y,
                &chrome::ellipsize(value, usize::from(inner.right().saturating_sub(x))),
                if field.label == "Target" {
                    bold(GitRole::Ref, SURFACE)
                } else {
                    st(GitRole::Text, SURFACE)
                },
                inner.right(),
            );
            continue;
        }
        let mut label: Vec<Span> = vec![(
            field.label.clone(),
            if focused {
                bold(GitRole::Focus, SURFACE)
            } else {
                st(GitRole::Text, SURFACE)
            },
        )];
        if field.required {
            label.push((" *".into(), st(GitRole::Removed, SURFACE)));
        }
        if !field.choices.is_empty() {
            label.push((
                if field.custom_choices {
                    "  ↑/↓ choose or type"
                } else {
                    "  ↑/↓ choose"
                }
                .into(),
                st(GitRole::Muted, SURFACE),
            ));
        }
        chrome::put_spans(&mut frame.grid, inner.x + 1, y, &label, inner.right());
        let rows = field_rows(field) - 1;
        if !field.choices.is_empty() && !field.custom_choices {
            // A closed set reads as radio options: `● Completed  ○ Not planned`.
            radio(frame, field, inner.x + 1, y + 1, inner.right(), focused);
            continue;
        }
        input(
            frame,
            field,
            Rect::new(inner.x + 1, y + 1, inner.w.saturating_sub(1), rows),
            focused,
        );
    }
    if let Some((text, error)) = status {
        frame.grid.put_text_clipped(
            inner.x + 1,
            inner.bottom() - 2,
            &chrome::ellipsize(&text, usize::from(inner.w.saturating_sub(1))),
            st(if error { GitRole::Removed } else { GitRole::Modified }, SURFACE),
            inner.right(),
        );
    }
    let hints = match &form.action {
        // The forge pages' footer carries a sheet's keys (one place for them).
        FormAction::Github(_) => "",
        FormAction::SourceSearch => "Enter find · Esc cancel",
        FormAction::Discard(_) | FormAction::DiscardFiles(_) => "Enter discard · Esc cancel",
        _ if form.fields.iter().any(|field| field.multiline) => {
            "Ctrl+Enter confirm · Esc cancel · Tab next"
        }
        _ => "Enter confirm · Esc cancel · Tab next",
    };
    buttons(
        frame,
        form,
        inner.x + 1,
        inner.bottom() - 1,
        inner.right(),
        primary_label(form),
        hints,
    );
}

/// Worker state or a Git refusal kept inside sheets that can cover the footer.
fn status_line(app: &GitScreen, form: &Form) -> Option<(String, bool)> {
    let wants = matches!(
        &form.action,
        FormAction::Branch(Some(_))
            | FormAction::CheckoutDetached(_)
            | FormAction::DetachedWorktree(_)
            | FormAction::Reference(_)
            | FormAction::Discard(_)
            | FormAction::DiscardFiles(_)
            | FormAction::Rewrite(_)
    );
    // Any other sheet opens with the notice cleared, so what it shows now is the
    // answer to this sheet: a host or Git refusal.
    if !wants {
        let line = app.notice.lines().next().unwrap_or_default();
        return (!line.is_empty() && !app.busy).then(|| (line.to_owned(), true));
    }
    if app.reflog_submitting {
        return Some(("Running Git…".into(), false));
    }
    if matches!(&form.action, FormAction::CheckoutDetached(_))
        && app.notice.contains("would be overwritten")
    {
        return Some(("Git refused checkout: changes would be overwritten".into(), true));
    }
    let line = app.notice.lines().next().unwrap_or_default();
    (!line.is_empty()).then(|| (line.to_owned(), super::looks_like_error(line)))
}

/// G-COMMIT: subject and body on the left, what is staged on the right; covers the
/// reader so the Changes pane stays in view.
fn commit(app: &GitScreen, frame: &mut Frame, form: &Form, lay: &Layout) {
    let area = if lay.reader.w >= 60 { lay.reader } else { lay.body };
    let rect = Rect::new(
        area.x + 1,
        area.y + 1,
        area.w.saturating_sub(2),
        area.h.saturating_sub(2).min(32).max(12.min(area.h)),
    );
    let branch = app.head_label().to_owned();
    let title = if app.commit_options[1] {
        format!("Amend HEAD on {branch}")
    } else {
        format!("Commit to {branch}")
    };
    let subject = form.fields.first().map(|field| field.input.input.as_str()).unwrap_or("");
    let length = subject.width();
    let inner = frame_box(frame, rect, &title_spans(app, Glyph::Commit, &title), "");
    let split = inner.w >= 80;
    let left_w = if split { (inner.w * 55 / 100).min(76) } else { inner.w };
    let left = Rect::new(inner.x, inner.y, left_w, inner.h);

    // Subject: label, length counter, one input row.
    let focused = |index: usize| form.selected == index;
    let mut y = left.y + 1;
    if let Some(field) = form.fields.first() {
        chrome::put_spans(
            &mut frame.grid,
            left.x + 1,
            y,
            &[(
                "Subject".into(),
                if focused(0) {
                    bold(GitRole::Focus, SURFACE)
                } else {
                    st(GitRole::Text, SURFACE)
                },
            )],
            left.right(),
        );
        let counter = format!("{length}/72");
        let role = if length > 72 {
            GitRole::Removed
        } else if length > 50 {
            GitRole::Modified
        } else {
            GitRole::Muted
        };
        frame.grid.put_text_clipped(
            left.right().saturating_sub(chrome::width(&counter) + 1),
            y,
            &counter,
            st(role, SURFACE),
            left.right(),
        );
        input(
            frame,
            field,
            Rect::new(left.x + 1, y + 1, left.w.saturating_sub(2), 1),
            focused(0),
        );
        y += 3;
    }
    if let Some(field) = form.fields.get(1) {
        chrome::put_spans(
            &mut frame.grid,
            left.x + 1,
            y,
            &[
                (
                    "Body".into(),
                    if focused(1) {
                        bold(GitRole::Focus, SURFACE)
                    } else {
                        st(GitRole::Text, SURFACE)
                    },
                ),
                ("  optional".into(), st(GitRole::Muted, SURFACE)),
            ],
            left.right(),
        );
        let rows = left.bottom().saturating_sub(y + 5).max(1);
        input(
            frame,
            field,
            Rect::new(left.x + 1, y + 1, left.w.saturating_sub(2), rows),
            focused(1),
        );
    }
    // Toggles: sign-off, amend, skip hooks.
    let ascii = app.icon_mode == crate::config::ExplorerIconMode::Ascii;
    // Key hints go first when the row is short: `☐ Sign-off M-s` → `☐ Sign-off`.
    let with_keys = left.w >= 58;
    let mut toggles: Vec<Span> = Vec::new();
    for ((label, key), on) in [("Sign-off", "M-s"), ("Amend", "M-a"), ("Skip hooks", "M-n")]
        .into_iter()
        .zip(app.commit_options)
    {
        let box_glyph = match (ascii, on) {
            (true, true) => "[x]",
            (true, false) => "[ ]",
            (false, true) => "☑",
            (false, false) => "☐",
        };
        toggles.push((
            format!("{box_glyph} "),
            if on { bold(GitRole::Focus, SURFACE) } else { st(GitRole::Muted, SURFACE) },
        ));
        toggles.push((
            label.into(),
            if on { bold(GitRole::Text, SURFACE) } else { st(GitRole::Text, SURFACE) },
        ));
        toggles.push((
            if with_keys { format!(" {key}   ") } else { "   ".into() },
            st(GitRole::Muted, SURFACE),
        ));
    }
    chrome::put_spans(&mut frame.grid, left.x + 1, inner.bottom() - 3, &toggles, left.right());
    // A refusal answering Ctrl+Enter sits between the toggles and the buttons.
    if let Some((line, _)) = status_line(app, form) {
        frame.grid.put_text_clipped(
            left.x + 1,
            inner.bottom() - 2,
            &chrome::ellipsize(&format!("✕ {line}"), usize::from(left.w.saturating_sub(2))),
            bold(GitRole::Removed, SURFACE),
            left.right(),
        );
    }
    let amend = app.commit_options[1];
    buttons(
        frame,
        form,
        left.x + 1,
        inner.bottom() - 1,
        inner.right(),
        if amend { "Amend" } else { "Commit" },
        if amend {
            "Ctrl+Enter amend HEAD · Tab next · Esc back"
        } else {
            "Ctrl+Enter commit · Tab next · Esc back"
        },
    );

    if !split {
        return;
    }
    // The staged side: what this commit will contain.
    let divider_x = left.right();
    for row in inner.y..inner.bottom().saturating_sub(1) {
        frame
            .grid
            .set_cell(divider_x, row, "│", st(GitRole::Border, SURFACE));
    }
    let right = Rect::new(
        divider_x + 2,
        inner.y,
        inner.right().saturating_sub(divider_x + 2),
        inner.h.saturating_sub(1),
    );
    let staged = app
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .entries
                .iter()
                .filter(|entry| entry.sources.staged)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mode = app.icon_mode;
    chrome::put_spans(
        &mut frame.grid,
        right.x,
        right.y + 1,
        &[
            (
                format!("{} ", glyph(Glyph::ChangesPane, mode)),
                st(GitRole::Added, SURFACE),
            ),
            ("Staged".into(), bold(GitRole::Text, SURFACE)),
            (format!(" {}", staged.len()), st(GitRole::Muted, SURFACE)),
        ],
        right.right(),
    );
    let (mut added, mut removed) = (0u32, 0u32);
    let list_bottom = (right.y + 3 + 6).min(right.bottom().saturating_sub(2));
    let mut y = right.y + 3;
    for (index, entry) in staged.iter().enumerate() {
        let counts = app.staged_numstat.get(&entry.relative_path).copied().flatten();
        if let Some((a, r)) = counts {
            added += a;
            removed += r;
        }
        if y >= list_bottom {
            if index < staged.len() {
                frame.grid.put_text_clipped(
                    right.x,
                    y,
                    &format!("+{} more", staged.len() - index),
                    st(GitRole::Muted, SURFACE),
                    right.right(),
                );
            }
            // Keep summing the totals for the summary row.
            continue;
        }
        let code = entry.change_code();
        let letter = code.chars().next().unwrap_or(' ');
        frame.grid.put_text_clipped(
            right.x,
            y,
            &letter.to_string(),
            st(GitRole::Added, SURFACE),
            right.right(),
        );
        let (icon, group) = super::glyphs::file(&entry.relative_path, mode);
        frame
            .grid
            .put_text_clipped(right.x + 2, y, icon, super::syntax(group, SURFACE), right.right());
        let stats: Vec<Span> = match counts {
            Some((a, r)) => vec![
                (format!("+{a}"), st(GitRole::Added, SURFACE)),
                (" ".into(), st(GitRole::Text, SURFACE)),
                (format!("−{r}"), st(GitRole::Removed, SURFACE)),
            ],
            None => vec![],
        };
        let stats_x = right.right().saturating_sub(chrome::spans_width(&stats));
        chrome::put_spans(&mut frame.grid, stats_x, y, &stats, right.right());
        let path = entry.relative_path.display().to_string();
        let (dir, name) = super::fit_path(&path, usize::from(stats_x.saturating_sub(right.x + 5)));
        let x = frame
            .grid
            .put_text_clipped(right.x + 4, y, &dir, st(GitRole::Muted, SURFACE), stats_x);
        frame
            .grid
            .put_text_clipped(x, y, &name, st(GitRole::Added, SURFACE), stats_x);
        y += 1;
    }
    patch(app, frame, Rect::new(right.x, y + 1, right.w, right.bottom().saturating_sub(y + 3)));
    if staged.is_empty() {
        frame.grid.put_text_clipped(
            right.x,
            right.y + 3,
            "Nothing staged · Esc, then Space stages a change",
            st(GitRole::Modified, SURFACE),
            right.right(),
        );
    }
    let summary: Vec<Span> = vec![
        (
            format!(
                "{} file{} · ",
                staged.len(),
                if staged.len() == 1 { "" } else { "s" }
            ),
            st(GitRole::Muted, SURFACE),
        ),
        (format!("+{added}"), st(GitRole::Added, SURFACE)),
        (" ".into(), st(GitRole::Muted, SURFACE)),
        (format!("−{removed}"), st(GitRole::Removed, SURFACE)),
    ];
    chrome::put_spans(&mut frame.grid, right.x, right.bottom() - 1, &summary, right.right());
}

/// The staged patch, compact: a file band per file, hunk bands, tinted +/− rows.
fn patch(app: &GitScreen, frame: &mut Frame, rect: Rect) {
    if rect.h < 2 || app.staged_patch.is_empty() {
        return;
    }
    let mode = app.icon_mode;
    for x in rect.x..rect.right() {
        frame.grid.set_cell(x, rect.y, "─", st(GitRole::Border, SURFACE));
    }
    let mut y = rect.y + 1;
    for line in app.staged_patch.lines() {
        if y >= rect.bottom() {
            break;
        }
        let row = Rect::new(rect.x, y, rect.w, 1);
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let path = rest.split(" b/").next().unwrap_or(rest);
            chrome::fill(&mut frame.grid, row, st(GitRole::Text, GitSurface::FileBand));
            let (icon, group) = super::glyphs::file(std::path::Path::new(path), mode);
            let x = frame.grid.put_text_clipped(
                rect.x + 1,
                y,
                &format!("{icon} "),
                super::syntax(group, GitSurface::FileBand),
                rect.right(),
            );
            frame.grid.put_text_clipped(
                x,
                y,
                &chrome::ellipsize_start(path, usize::from(rect.right().saturating_sub(x + 1))),
                bold(GitRole::Text, GitSurface::FileBand),
                rect.right(),
            );
        } else if line.starts_with("@@") {
            chrome::fill(&mut frame.grid, row, st(GitRole::Text, GitSurface::HunkBand));
            let range = line.split("@@").nth(1).map_or(line, str::trim);
            frame.grid.put_text_clipped(
                rect.x + 1,
                y,
                &format!("@@ {range} @@"),
                st(GitRole::Focus, GitSurface::HunkBand),
                rect.right(),
            );
        } else if line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with('\\')
        {
            continue;
        } else {
            let (surface, role, sign) = match line.chars().next() {
                Some('+') => (GitSurface::AddedBand, GitRole::Added, "+"),
                Some('-') => (GitSurface::RemovedBand, GitRole::Removed, "−"),
                _ => (SURFACE, GitRole::Muted, " "),
            };
            chrome::fill(&mut frame.grid, row, st(GitRole::Text, surface));
            frame.grid.put_text_clipped(rect.x + 1, y, sign, bold(role, surface), rect.right());
            let code = line.get(1..).unwrap_or_default().replace('\t', "    ");
            frame.grid.put_text_clipped(
                rect.x + 3,
                y,
                &code,
                st(if role == GitRole::Muted { GitRole::Muted } else { GitRole::Text }, surface),
                rect.right(),
            );
        }
        y += 1;
    }
}
