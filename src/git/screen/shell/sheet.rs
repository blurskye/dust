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
const COMMIT_TOGGLE_KEYS_WIDTH: u16 = 58;

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
    frame_inner(rect)
}

fn frame_inner(rect: Rect) -> Rect {
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

/// A title too long for its sheet ends on a whole word: `Re-run #7 · Exercise the…`,
/// never `regression ch`.
fn cut_title(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.into();
    }
    let cut = chrome::ellipsize(text, cells);
    let kept = cut.trim_end_matches('…');
    if text[kept.len()..].starts_with(' ') {
        return format!("{}…", kept.trim_end_matches([' ', '·']));
    }
    match kept.rfind(' ') {
        Some(at) if at * 2 >= kept.len() => format!("{}…", kept[..at].trim_end_matches([' ', '·'])),
        _ => cut,
    }
}

/// A run sheet's note in `cells` columns: each line wraps on words, so the commit it names
/// (`(425ac6a)`) is never cut. Each row says whether it belongs to the first line.
fn wrap_note(note: &str, cells: usize) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    for (index, line) in note.lines().enumerate() {
        let mut row = String::new();
        for word in line.split(' ') {
            if !row.is_empty() && row.width() + 1 + word.width() > cells {
                rows.push((std::mem::take(&mut row), index == 0));
            }
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(word);
        }
        rows.push((row, index == 0));
    }
    rows
}

/// One labelled input. `rows` is the input height (1 for single-line fields).
pub(in crate::git::screen) fn input(frame: &mut Frame, field: &Field, rect: Rect, focused: bool) {
    let surface = if focused { GitSurface::CursorInactive } else { GitSurface::Canvas };
    chrome::fill(&mut frame.grid, rect, st(GitRole::Text, surface));
    // Only the focused field carries the accent edge (r15 ISS-06).
    if rect.x > 0 && focused {
        for y in rect.y..rect.bottom() {
            frame.grid.set_cell(rect.x - 1, y, "▎", bold(GitRole::Focus, SURFACE));
        }
    }
    let text = &field.input.input;
    let cursor = field.input.cursor.min(text.len());
    let cursor_line = text[..cursor].bytes().filter(|&b| b == b'\n').count();
    let lines = text.split('\n').collect::<Vec<_>>();
    let visible = usize::from(rect.h.max(1));
    let first = viewport(field.top.get(), cursor_line, lines.len(), visible);
    field.top.set(first);
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
            "Body" => "Why this change, in a sentence or two",
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
    let mut line_start = 0;
    for (offset, line) in lines.iter().enumerate() {
        let begin = line_start;
        line_start += line.len() + 1;
        if offset < first || offset >= first + visible {
            continue;
        }
        let skip = if offset == cursor_line { start } else { 0 };
        let shown = &line[skip..];
        let y = rect.y + (offset - first) as u16;
        frame.grid.put_text_clipped(
            rect.x + 1,
            y,
            shown,
            st(if field.read_only { GitRole::Muted } else { GitRole::Text }, surface),
            rect.right().saturating_sub(1),
        );
        // A modal text box's visual selection, clipped to this line.
        if let Some(range) = field.highlight.as_ref() {
            let from = range.start.max(begin + skip).min(begin + line.len());
            let to = range.end.min(begin + line.len());
            let (from, to) = (from - begin, to.max(from) - begin);
            let mut selected = &line[from..to];
            if selected.is_empty() && range.start <= begin + line.len() && range.end > begin + line.len() {
                selected = " ";
            }
            if !selected.is_empty() {
                let x = rect.x + 1 + line[skip..from].width() as u16;
                // The selected span reads as one block in the accent, not a faint tint.
                frame.grid.put_text_clipped(
                    x,
                    y,
                    selected,
                    bold(GitRole::OnFocus, GitSurface::ChipActive),
                    rect.right().saturating_sub(1),
                );
            }
        }
    }
    if focused {
        frame.cursor = Some((
            rect.x + 1 + column as u16,
            rect.y + (cursor_line - first) as u16,
            if field.read_only || field.block_cursor {
                SetCursorStyle::SteadyBlock
            } else {
                SetCursorStyle::BlinkingBar
            },
        ));
    }
}

/// A text box's first shown line: where it was (`top`), moved only as far as keeps the cursor
/// line in view, as an editor scrolls, typing or on a motion; never past the last line.
fn viewport(top: usize, cursor_line: usize, lines: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    let top = if cursor_line < top {
        cursor_line
    } else if cursor_line >= top + visible {
        cursor_line + 1 - visible
    } else {
        top
    };
    top.min(lines.saturating_sub(visible))
}

/// A stacked option's description, muted after its label: `● Completed  Done, closed, …`.
fn describe(frame: &mut Frame, choice: &str, x: u16, y: u16, right: u16) {
    if let Some(line) = described(choice) {
        let line = chrome::ellipsize(line, usize::from(right.saturating_sub(x)));
        frame.grid.put_text_clipped(x, y, &line, st(GitRole::Muted, SURFACE), right);
    }
}

/// The options of a closed choice on one row, the chosen one filled; a long set stacks one
/// option per row so none is clipped.
fn radio(frame: &mut Frame, field: &Field, x: u16, y: u16, right: u16, focused: bool) {
    if stacked(field) {
        let chosen = field.choices.iter().position(|choice| *choice == field.input.input).unwrap_or(0);
        let first = chosen.saturating_sub(STACKED - 1).min(field.choices.len().saturating_sub(STACKED));
        // Descriptions line up in one column after the longest option.
        let column = x + 5 + field.choices.iter().map(|choice| chrome::width(choice)).max().unwrap_or(0);
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
                let next = frame.grid.put_text_clipped(x, y, &format!(" ● {label} "), style, right);
                if focused {
                    frame.cursor = Some((x + 1, y, SetCursorStyle::SteadyBlock));
                }
                describe(frame, choice, column.max(next + 1), y, right);
            } else {
                let next = frame.grid.put_text_clipped(x, y, &format!(" ○ {label} "), st(GitRole::Text, SURFACE), right);
                if more {
                    frame.grid.put_text_clipped(next + 1, y, "⋯", st(GitRole::Muted, SURFACE), right);
                } else {
                    describe(frame, choice, column.max(next + 1), y, right);
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
    // `Cancel` beside `Cancel run` would read as the same verb.
    let secondary = if matches!(&form.action, FormAction::Github(super::super::github::Action::Cancel(_))) {
        "Keep running"
    } else {
        "Cancel"
    };
    let selected = form.selected.checked_sub(count).filter(|index| *index < 2);
    let at = button_pair(frame, x, y, right, [primary, secondary], selected);
    button_hints(frame, at, y, right, hints);
}

/// The one button pair every sheet and dialog draws (R16-issues-07): the primary a filled
/// accent chip, the secondary a muted chip, the focused one filled and marked `›`. Each is
/// bracketed so it reads as a button in plain text too (r15 ISS-08). Returns the column
/// after the pair.
pub(in crate::git::screen) fn button_pair(
    frame: &mut Frame,
    x: u16,
    y: u16,
    right: u16,
    labels: [&str; 2],
    selected: Option<usize>,
) -> u16 {
    let mut at = x;
    for (index, label) in labels.into_iter().enumerate() {
        let chosen = selected == Some(index);
        let style = if chosen {
            bold(GitRole::OnFocus, GitSurface::ChipActive)
        } else if index == 0 {
            bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus))
        } else {
            st(GitRole::Text, GitSurface::Chip(GitChip::Muted))
        };
        let edge = if chosen { bold(GitRole::Focus, SURFACE) } else { st(GitRole::Border, SURFACE) };
        let start = frame.grid.put_text_clipped(at, y, if chosen { "›" } else { " " }, edge, right);
        let start = frame.grid.put_text_clipped(start, y, "[", edge, right);
        let next = frame.grid.put_text_clipped(start, y, &format!(" {label} "), style, right);
        let next = frame.grid.put_text_clipped(next, y, "]", edge, right);
        if chosen {
            frame.cursor = Some((start + 1, y, SetCursorStyle::SteadyBlock));
        }
        at = next + 1;
    }
    at
}

/// `Enter confirm · Esc cancel` after a button pair, keys in the accent.
pub(in crate::git::screen) fn button_hints(frame: &mut Frame, at: u16, y: u16, right: u16, hints: &str) {
    // Whole hints only, never cut mid-word. The first part is the confirm key and stays;
    // middle parts go first, then the words (the buttons beside them already say them),
    // then the last key.
    let room = usize::from(right.saturating_sub(at + 1));
    let mut parts: Vec<(&str, Option<&str>)> = hints
        .split(" · ")
        .map(|part| match part.split_once(' ') {
            Some((key, what)) => (key, Some(what)),
            None => (part, None),
        })
        .collect();
    let width = |parts: &[(&str, Option<&str>)]| {
        parts.iter().map(|(key, what)| key.width() + what.map_or(0, |what| what.width() + 1)).sum::<usize>()
            + parts.len().saturating_sub(1) * 3
    };
    while parts.len() > 2 && width(&parts) > room {
        parts.remove(1);
    }
    if width(&parts) > room {
        for part in &mut parts {
            part.1 = None;
        }
    }
    while !parts.is_empty() && width(&parts) > room {
        parts.pop();
    }
    let mut spans = Vec::new();
    for (index, (key, what)) in parts.into_iter().enumerate() {
        if index > 0 {
            spans.push((" · ".into(), st(GitRole::Muted, SURFACE)));
        }
        spans.push((key.to_owned(), bold(GitRole::Focus, SURFACE)));
        if let Some(what) = what {
            spans.push((format!(" {what}"), st(GitRole::Muted, SURFACE)));
        }
    }
    chrome::put_spans(&mut frame.grid, at + 1, y, &spans, right);
}

fn primary_label(app: &GitScreen, form: &Form) -> &'static str {
    use super::super::github::Action;
    use super::super::refs::Action as Ref;
    let pipeline = app.github.terms().run_noun == "pipeline";
    match &form.action {
        // The button says the verb the footer says (§3.40).
        FormAction::Github(Action::Cancel(_)) if pipeline => "Cancel pipeline",
        FormAction::Github(Action::Cancel(_)) => "Cancel run",
        FormAction::Github(Action::Rerun(_)) => "Re-run jobs",
        FormAction::Github(Action::Dispatch(_)) => "Run workflow",
        FormAction::Github(Action::Approve(_)) => "Approve",
        FormAction::Github(Action::Download(..)) => "Download",
        FormAction::Github(Action::SaveLog(_)) => "Save",
        FormAction::Github(Action::CreatePullRequest) => "Review →",
        FormAction::Github(Action::CreatePullRequestWeb | Action::Browser(..) | Action::Web(..)) => "Open in browser",
        FormAction::Github(Action::InlineRange { .. }) => "Add review comment",
        FormAction::Github(Action::Draft(..)) => "Save",
        FormAction::Github(Action::ConfirmPullRequest(request)) if request.push.is_some() => {
            "Push + create PR"
        }
        FormAction::Github(Action::ConfirmPullRequest(_)) => "Create PR",
        FormAction::Github(Action::SaveView(..)) => "Save view",
        // The verb the title asks about, not a bare `Confirm` (r11 ISS-07).
        FormAction::Github(Action::State(_, _, true)) => "Close",
        FormAction::Github(Action::State(_, _, false)) => "Reopen",
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
        FormAction::Run(op) => op.verb(),
        FormAction::OperationMenu => "Choose",
        _ => "Confirm",
    }
}

/// Most options a stacked choice shows at once; the chosen one is kept in view.
const STACKED: usize = 7;

/// Whether a closed choice is too long for one row and reads as a list instead. A choice
/// whose options carry a description (GitHub's close reasons) stacks too, one per row with
/// its line beside it, as GitHub's close menu does.
fn stacked(field: &Field) -> bool {
    !field.custom_choices
        && (field.choices.iter().map(|choice| usize::from(chrome::width(choice)) + 5).sum::<usize>() > 52
            || field.choices.iter().any(|choice| described(choice).is_some()))
}

/// The one-line description GitHub's close menu gives each reason.
fn described(choice: &str) -> Option<&'static str> {
    match choice {
        "Completed" => Some("Done, closed, fixed, resolved"),
        "Not planned" => Some("Won't fix, can't repro, duplicate, stale"),
        _ => None,
    }
}

/// A toggle's two values. A toggle is a closed choice of exactly these, drawn as one
/// `☐ Open as draft` row instead of a label over radio options.
const ON: &str = "on";
const OFF: &str = "off";

/// A one-row checkbox field: `label` is what it does when checked.
pub(in crate::git::screen) fn toggle(label: &str, on: bool) -> Field {
    let mut field = Field::new(label, if on { ON } else { OFF }.into());
    field.choices = vec![ON.into(), OFF.into()];
    field
}

pub(in crate::git::screen) fn is_toggle(field: &Field) -> bool {
    !field.custom_choices && field.choices.len() == 2 && field.choices[0] == ON && field.choices[1] == OFF
}

pub(in crate::git::screen) fn toggled(field: &Field) -> bool {
    field.input.input == ON
}

/// Space, Up or Down flips a toggle; any other typing leaves it alone. True when the key
/// was the toggle's.
pub(in crate::git::screen) fn toggle_key(field: &mut Field, code: &crate::editor::input::KeyCode) -> bool {
    use crate::editor::input::KeyCode;
    if !is_toggle(field) {
        return false;
    }
    match code {
        KeyCode::Char(' ') | KeyCode::Up | KeyCode::Down => {
            field.input.input = if toggled(field) { OFF } else { ON }.into();
            field.input.cursor = field.input.input.len();
            true
        }
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => true,
        _ => false,
    }
}

/// Each field's rows on the sheet, its gap included. Toggles stack without gaps; the last
/// of a run keeps one before what follows.
fn heights(fields: &[Field]) -> Vec<u16> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if is_toggle(field) {
                1 + u16::from(fields.get(index + 1).is_none_or(|next| !is_toggle(next)))
            } else {
                field_rows(field) + u16::from(!field.read_only)
            }
        })
        .collect()
}

/// `☑ Push 2 commits first`: the box in the accent when checked, the row a chip when focused.
fn checkbox(app: &GitScreen, frame: &mut Frame, field: &Field, x: u16, y: u16, right: u16, focused: bool) {
    let mark = glyph(if toggled(field) { Glyph::Checked } else { Glyph::Unchecked }, app.icon_mode);
    if focused {
        frame.grid.put_text_clipped(x, y, &format!(" {mark} {} ", field.label), bold(GitRole::OnFocus, GitSurface::ChipActive), right);
        frame.cursor = Some((x + 1, y, SetCursorStyle::SteadyBlock));
        return;
    }
    let spans: Vec<Span> = vec![
        (format!(" {mark} "), st(if toggled(field) { GitRole::Focus } else { GitRole::Muted }, SURFACE)),
        (field.label.clone(), st(GitRole::Text, SURFACE)),
    ];
    chrome::put_spans(&mut frame.grid, x, y, &spans, right);
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
    let mut heights = heights(&form.fields);
    let content = heights.iter().sum::<u16>();
    let status = status_line(app, form);
    // A run sheet says what its confirm does first (`Running jobs stop…`), then a gap.
    let note = match &form.action {
        FormAction::Github(action) => app.github.sheet_note(action, &form.fields),
        _ => None,
    };
    let w = (body.w * 6 / 10).clamp(56.min(body.w), 110.min(body.w));
    // A forge confirmation with only choices to make (`Close issue #19?`) is a short
    // question: about 60 columns, not a 110-column bar across both panes (r11 ISS-08).
    // Closing an issue adds an optional comment; the dialog stays a short question.
    let short = matches!(&form.action, FormAction::Github(super::super::github::Action::State(..)));
    let w = if short { w.min(72) } else { w };
    // The note wraps in the interior less its one-column pad (a rerun's second line says why
    // failed-only is unavailable), then a gap.
    let note = note.map(|note| wrap_note(&note, usize::from(w.saturating_sub(6)))).unwrap_or_default();
    let note_rows = if note.is_empty() { 0 } else { note.len() as u16 + 1 };
    let content = content + note_rows;
    // Top padding, fields, an optional status row, the button row, borders.
    let wanted = content + 1 + u16::from(status.is_some()) + 1 + 2;
    let h = wanted.min(body.h.saturating_sub(2)).max(5);
    // One placement rule at every width: centred on the screen (r11 ISS-04).
    let x = body.x + (body.w - w) / 2;
    let rect = Rect::new(x, body.y + body.h.saturating_sub(h) / 2, w, h);
    // The `Field 1/2` pager shows only when some field is out of view: a sheet whose
    // fields all show reads as GitHub's dialogs do, without a counter (R16-issues-06).
    let position = if form.fields.len() > 1 && note.is_empty() && wanted > h {
        format!(
            "Field {}/{}",
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
    // One rule for every overlay (R16-issues-01, git-tab-02, notif-actions-07): the box is
    // opaque and what is behind it recedes to the border colour, never blanked, so the run,
    // issue or pane the sheet is about stays in view. The forge reader's find prompt leaves
    // the text it searches as it is.
    if !(matches!(&form.action, FormAction::SourceSearch) && app.page != 0) {
        chrome::scrim_within(&mut frame.grid, rect, body);
    }
    // A run sheet's title names the run; a glyph before it would read as its status (A2-10).
    use super::super::github::Action;
    let run_sheet = matches!(
        &form.action,
        FormAction::Github(
            Action::Cancel(_) | Action::Rerun(_) | Action::Approve(_) | Action::Dispatch(_) | Action::Download(..) | Action::SaveLog(_)
        )
    );
    // The frame's corner, a space either side of the title, and the position label.
    let glyph_cells = if run_sheet { 0 } else { usize::from(chrome::width(glyph(glyph_kind, app.icon_mode))) + 1 };
    let position_cells = if position.is_empty() { 0 } else { usize::from(chrome::width(&position)) + 3 };
    let title = cut_title(&form.title, usize::from(w.saturating_sub(6)).saturating_sub(glyph_cells + position_cells));
    let spans = if run_sheet { vec![(title, bold(GitRole::Text, SURFACE))] } else { title_spans(app, glyph_kind, &title) };
    let inner = frame_box(frame, rect, &spans, &position);
    let fields_bottom = inner.bottom().saturating_sub(1 + u16::from(status.is_some()));

    for (row, (line, first)) in note.iter().enumerate() {
        frame.grid.put_text_clipped(
            inner.x + 1,
            inner.y + 1 + row as u16,
            &chrome::ellipsize(line, usize::from(inner.w.saturating_sub(2))),
            st(if *first { GitRole::Text } else { GitRole::Muted }, SURFACE),
            inner.right(),
        );
    }
    let room = fields_bottom.saturating_sub(inner.y + 1 + note_rows);
    // On a short screen a description gives up rows (down to two) before a field below it
    // drops out of view: every field of the Create sheet shows at 100x30.
    let mut over = heights.iter().sum::<u16>().saturating_sub(room);
    for (index, field) in form.fields.iter().enumerate() {
        if over == 0 {
            break;
        }
        if field.multiline && !field.read_only && field.choices.is_empty() {
            let give = over.min(heights[index].saturating_sub(4));
            heights[index] -= give;
            over -= give;
        }
    }
    // Scroll so the selected field is fully visible.
    let selected_bottom = heights.iter().take((form.selected + 1).min(form.fields.len())).sum::<u16>();
    let scroll = selected_bottom.saturating_sub(room);
    let mut at = 0u16;
    for (index, field) in form.fields.iter().enumerate() {
        let rows = heights[index];
        let top = at;
        at += rows;
        if top < scroll || top + rows - scroll > room {
            continue;
        }
        let y = inner.y + 1 + note_rows + top - scroll;
        let focused = form.selected == index;
        if is_toggle(field) {
            checkbox(app, frame, field, inner.x, y, inner.right(), focused);
            continue;
        }
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
        // The choice keys show on the focused field only, not repeated down the sheet.
        if !field.choices.is_empty() && focused {
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
        let rows = if field.multiline { heights[index].saturating_sub(2) } else { field_rows(field) - 1 };
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
            "Ctrl+Enter confirm · Tab next · Esc cancel"
        }
        _ => "Enter confirm · Tab next · Esc cancel",
    };
    buttons(
        frame,
        form,
        inner.x + 1,
        inner.bottom() - 1,
        inner.right(),
        primary_label(app, form),
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

fn commit_rect(lay: &Layout) -> Rect {
    let area = if lay.reader.w >= 60 { lay.reader } else { lay.body };
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.w.saturating_sub(2),
        area.h.saturating_sub(2).min(32).max(12.min(area.h)),
    )
}

fn commit_left(inner: Rect) -> Rect {
    let width = if inner.w >= 80 { (inner.w * 55 / 100).min(76) } else { inner.w };
    Rect::new(inner.x, inner.y, width, inner.h)
}

/// Whether the commit sheet has room for the keys beside its checkboxes.
pub(in crate::git::screen) fn commit_keys_beside_toggles(screen: Rect) -> bool {
    // A footer rectangle ends at the screen's bottom, just like a full-screen one.
    let lay = super::layout::layout(screen.w, screen.bottom(), false, [false; 4]);
    commit_left(frame_inner(commit_rect(&lay))).w >= COMMIT_TOGGLE_KEYS_WIDTH
}

/// G-COMMIT: subject and body on the left, what is staged on the right; covers the
/// reader so the Changes pane stays in view.
fn commit(app: &GitScreen, frame: &mut Frame, form: &Form, lay: &Layout) {
    let rect = commit_rect(lay);
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
    let left = commit_left(inner);

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
        let counter = match selected_span(form) {
            Some(span) => format!("{span} · {length}/72"),
            None => format!("{length}/72"),
        };
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
        if focused(1)
            && let Some(span) = selected_span(form)
        {
            frame.grid.put_text_clipped(
                left.right().saturating_sub(chrome::width(&span) + 1),
                y,
                &span,
                st(GitRole::Focus, SURFACE),
                left.right(),
            );
        }
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
    let with_keys = left.w >= COMMIT_TOGGLE_KEYS_WIDTH;
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
    let typing = app.input_mode() == Some(super::super::modal::InputMode::Insert);
    let visual = selected_span(form).is_some();
    buttons(
        frame,
        form,
        left.x + 1,
        inner.bottom() - 1,
        inner.right(),
        if amend { "Amend" } else { "Commit" },
        if visual {
            "y yank · d delete · c change · Esc normal"
        } else if amend && typing {
            "Ctrl+Enter amend HEAD · Tab next · Esc normal"
        } else if amend {
            "Ctrl+Enter amend HEAD · Tab next · Esc back"
        } else if typing {
            "Ctrl+Enter commit · Tab next · Esc normal"
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
            "Nothing staged · Esc, then s stages a change",
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
            // Code wraps, as the reader's diff does: a long line continues on the next rows
            // under its sign, never cut mid-token.
            let code = line.get(1..).unwrap_or_default().replace('\t', "    ");
            let style = st(if role == GitRole::Muted { GitRole::Muted } else { GitRole::Text }, surface);
            for (index, piece) in wrap_code(&code, usize::from(rect.w.saturating_sub(4)).max(1)).iter().enumerate() {
                if y >= rect.bottom() {
                    break;
                }
                let row = Rect::new(rect.x, y, rect.w, 1);
                chrome::fill(&mut frame.grid, row, st(GitRole::Text, surface));
                if index == 0 {
                    frame.grid.put_text_clipped(rect.x + 1, y, sign, bold(role, surface), rect.right());
                }
                frame.grid.put_text_clipped(rect.x + 3, y, piece, style, rect.right());
                y += 1;
            }
            continue;
        }
        y += 1;
    }
}

/// The focused box's Visual selection, as the commit box's range indicator: `3 selected`,
/// or `2 lines` for a selection across lines.
fn selected_span(form: &Form) -> Option<String> {
    let field = form.fields.get(form.selected)?;
    let range = field.highlight.clone()?;
    let text = field.input.input.get(range)?;
    let lines = text.split('\n').count();
    Some(if lines > 1 {
        format!("{lines} lines")
    } else {
        format!("{} selected", text.chars().count())
    })
}

/// `code` in rows of at most `cells` columns, cut between graphemes; an empty line is one row.
fn wrap_code(code: &str, cells: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for grapheme in code.graphemes(true) {
        let size = grapheme.width();
        if used + size > cells && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("one row").push_str(grapheme);
        used += size;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_hints_keep_the_confirm_key() {
        let hints = "Ctrl+Enter commit · Tab next · Esc normal";
        for (room, expected) in [
            (41, hints),
            (30, "Ctrl+Enter commit · Esc normal"),
            (29, "Ctrl+Enter · Esc"),
            (16, "Ctrl+Enter · Esc"),
            (15, "Ctrl+Enter"),
            (10, "Ctrl+Enter"),
            (9, ""),
        ] {
            let mut frame = Frame {
                grid: crate::ui::CellGrid::new(room + 1, 1),
                cursor: None,
            };
            button_hints(&mut frame, 0, 0, room + 1, hints);
            assert_eq!(frame.grid.row_text(0).trim(), expected, "room={room}");
        }
    }

    #[test]
    fn staged_patch_code_wraps_instead_of_clipping() {
        assert_eq!(wrap_code("let modes = [\"NORMAL\"];", 10), ["let modes ", "= [\"NORMAL", "\"];"]);
        assert_eq!(wrap_code("", 10), [""]);
        assert_eq!(wrap_code("中文中文", 4), ["中文", "中文"]);
    }

    /// A2-01: a long run title ends on a word with `…`; the note wraps and keeps the commit.
    #[test]
    fn run_sheet_titles_end_on_a_word_and_notes_wrap() {
        let title = "Re-run #7 · Exercise the retry-limit regression check";
        assert_eq!(cut_title(title, 60), title);
        let cut = cut_title(title, 51);
        assert_eq!(cut, "Re-run #7 · Exercise the retry-limit regression…");
        assert!(cut.width() <= 51);
        assert_eq!(cut_title("Cancel run #12 · Fix it?", 16), "Cancel run #12…");
        let note = "Reruns 1 failed job (Python tests) on the same commit (425ac6a).\nForgejo reruns whole runs only.";
        let rows = wrap_note(note, 54);
        assert!(rows.iter().all(|(row, _)| row.width() <= 54), "{rows:?}");
        assert!(rows.iter().any(|(row, first)| *first && row.ends_with("(425ac6a).")), "{rows:?}");
        assert_eq!(rows.last().map(|(row, first)| (row.as_str(), *first)), Some(("Forgejo reruns whole runs only.", false)));
    }

    /// A text box scrolls only as far as the cursor needs, typing down or moving back up.
    #[test]
    fn a_text_box_keeps_its_scroll_until_the_cursor_leaves_the_view() {
        // Typing past the fourth line scrolls one line at a time.
        assert_eq!(viewport(0, 3, 4, 4), 0);
        assert_eq!(viewport(0, 4, 5, 4), 1);
        assert_eq!(viewport(1, 6, 7, 4), 3);
        // `k` within the view keeps it; past its top it follows the cursor up; `gg` goes home.
        assert_eq!(viewport(3, 4, 7, 4), 3);
        assert_eq!(viewport(3, 2, 7, 4), 2);
        assert_eq!(viewport(3, 0, 7, 4), 0);
        // `G` shows the end; deleting lines never leaves the box scrolled past them.
        assert_eq!(viewport(0, 9, 10, 4), 6);
        assert_eq!(viewport(6, 2, 3, 4), 0);
        assert_eq!(viewport(5, 5, 6, 0), 5);
    }
}
