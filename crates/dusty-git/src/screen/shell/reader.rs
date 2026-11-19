//! The reader (pane 0): file and hunk bands, two-number gutters, full-width
//! added/removed bands with syntax kept on top.
use super::super::GitScreen;
use super::super::conflicts;
use super::super::preview::PreviewRow;
use super::chrome::{self, Span, bold, st};
use super::glyphs::{Glyph, glyph};
use super::layout::Rect;
use super::syntax;
use crate::ui::{CellGrid, Frame, GitChip, GitRole, GitSurface, Style};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn paint(app: &mut GitScreen, frame: &mut Frame, rect: Rect) {
    if rect.w < 12 || rect.h < 3 {
        return;
    }
    let focused = app.focus == 4;
    let mode = app.icon_mode;
    let changes = app.last_list == 0 && app.files_view == 0 && !app.all_files;
    let selected = app.selected_row();
    let path = selected.as_ref().map(|row| row.value.display().to_string());
    let pinned = app.pinned_preview_heading();
    let split = app.split_diff && app.preview.patch && rect.w >= 100;
    app.preview_split = split;
    app.normalize_preview_cursor();
    let active_hunk = if focused
        && changes
        && app.preview.patch
        && app.preview_anchor.is_none()
        && !app.jobs.preview_loading()
    {
        app.preview_cursor
            .and_then(|cursor| app.preview.hunk_rows(cursor))
    } else {
        None
    };
    let hunks = app
        .preview
        .rows
        .iter()
        .filter(|row| row.marker.is_none() && row.text.starts_with("@@"))
        .count();

    // Title on the border.
    let mut title: Vec<Span> = Vec::new();
    let pane_surface = GitSurface::Pane;
    match app.last_list {
        0 if app.files_view == 1 => {
            title.push(("Worktree".into(), bold(GitRole::Text, pane_surface)))
        }
        0 if app.files_view == 2 => {
            title.push(("Submodule".into(), bold(GitRole::Text, pane_surface)))
        }
        0 => {
            if let Some(row) = &selected {
                let (icon, group) = super::glyphs::file(&row.value, mode);
                title.push((format!("{icon} "), syntax(group, pane_surface)));
                title.push((
                    path.clone().unwrap_or_default(),
                    bold(GitRole::Text, pane_surface),
                ));
            } else {
                title.push(("Changes".into(), bold(GitRole::Text, pane_surface)));
            }
        }
        1 => {
            title.push((
                format!("{} ", glyph(Glyph::Branch, mode)),
                st(GitRole::Ref, pane_surface),
            ));
            title.push((
                path.clone().unwrap_or_else(|| "Branch".into()),
                bold(GitRole::Text, pane_surface),
            ));
        }
        2 => {
            // The commit's hash and author live once, in the header block below; the
            // title names the file shown (R15-06).
            if let Some(file) = app
                .commit
                .as_ref()
                .and_then(|commit| commit.files.get(commit.selected))
            {
                let (icon, group) = super::glyphs::file(&file.path, mode);
                title.push((format!("{icon} "), syntax(group, pane_surface)));
                title.push((
                    file.path.display().to_string(),
                    bold(GitRole::Text, pane_surface),
                ));
            } else if app.commit.is_some() {
                title.push((
                    format!("{} ", glyph(Glyph::Commit, mode)),
                    st(GitRole::Hash, pane_surface),
                ));
                title.push(("Commit".into(), bold(GitRole::Text, pane_surface)));
            } else {
                title.push(("Commit".into(), bold(GitRole::Text, pane_surface)));
            }
        }
        _ => {
            title.push((
                format!("{} ", glyph(Glyph::Stash, mode)),
                st(GitRole::Muted, pane_surface),
            ));
            title.push((
                path.clone().unwrap_or_else(|| "Stash".into()),
                bold(GitRole::Text, pane_surface),
            ));
        }
    }
    let right_label = if app.last_list == 1
        && let Some(context) = app
            .ref_preview
            .as_ref()
            .filter(|context| !(context.selected_is_current && context.head_at_tip))
    {
        // How the selected ref relates to HEAD: `HEAD→main = feature/x`.
        super::super::render::ref_preview_heading(
            context,
            usize::from(rect.w / 2).saturating_sub(2),
            rect.w < 100,
        )
    } else if let Some(range) = &active_hunk {
        let index = app.preview.rows[..=range.start]
            .iter()
            .filter(|row| row.marker.is_none() && row.text.starts_with("@@"))
            .count();
        format!("Hunk {index} of {hunks}")
    } else if app.jobs.preview_loading() || app.refreshing {
        "updating…".into()
    } else {
        String::new()
    };
    chrome::pane(&mut frame.grid, rect, &title, &right_label, focused);
    let inner = rect.inner();
    let mut y = inner.y;

    if let Some((tone, text)) = &app.receipt {
        chrome::receipt(
            &mut frame.grid,
            Rect::new(inner.x, y, inner.w, 1),
            *tone,
            text,
            mode,
        );
        y += 1;
    }
    if changes && selected.is_some() {
        let staged = app.preview_staged();
        chrome::sub_tabs(
            &mut frame.grid,
            inner.x,
            y,
            inner.right(),
            &["Working tree".into(), "Staged".into()],
            usize::from(staged),
            focused,
        );
        y += 1;
    }
    if let Some(commit) = &app.commit {
        // Subject, then identity: hash · refs · author · date · files.
        let surface = GitSurface::FileBand;
        chrome::fill(
            &mut frame.grid,
            Rect::new(inner.x, y, inner.w, 2),
            st(GitRole::Text, surface),
        );
        let subject = commit.message.lines().next().unwrap_or_default();
        frame.grid.put_text_clipped(
            inner.x + 1,
            y,
            &chrome::ellipsize(subject, usize::from(inner.w.saturating_sub(2))),
            bold(GitRole::Text, surface),
            inner.right(),
        );
        let mut x = frame.grid.put_text_clipped(
            inner.x + 1,
            y + 1,
            &commit.id[..7],
            st(GitRole::Hash, surface),
            inner.right(),
        ) + 1;
        let meta: Vec<Span> = vec![
            (commit.author.clone(), st(GitRole::Author, surface)),
            (" · ".into(), st(GitRole::Muted, surface)),
            // The Git tab's one date grammar (`2h`, `Nov 12`), as History writes it.
            (
                chrome::when(&commit.date, crate::forge::records::now()),
                st(GitRole::Muted, surface),
            ),
            (
                format!(
                    " · {} file{}",
                    commit.files.len(),
                    if commit.files.len() == 1 { "" } else { "s" }
                ),
                st(GitRole::Muted, surface),
            ),
        ];
        // Chips take only what author · date · files leave free.
        let meta_width = meta
            .iter()
            .map(|(text, _)| chrome::width(text))
            .sum::<u16>();
        let chips_right = inner.right().saturating_sub(meta_width + 1);
        if !commit.decorations.is_empty() {
            for (index, reference) in commit.decorations.split(", ").take(3).enumerate() {
                let (text, hue) = match reference.strip_prefix("tag: ") {
                    Some(tag) => (format!("{} {tag}", glyph(Glyph::Tag, mode)), GitChip::Tag),
                    None => (
                        reference.trim_start_matches("HEAD -> ").to_owned(),
                        GitChip::Ref,
                    ),
                };
                let room = chips_right.saturating_sub(x + 3);
                let text = if chrome::width(&text) <= room {
                    text
                } else if index == 0 && room >= 6 {
                    chrome::ellipsize(&text, usize::from(room))
                } else {
                    break;
                };
                x = chrome::chip(&mut frame.grid, x, y + 1, chips_right, &text, hue) + 1;
            }
        }
        chrome::put_spans(&mut frame.grid, x, y + 1, &meta, inner.right());
        y += 2;
    }
    if let Some(index) = pinned {
        file_band(
            app,
            &mut frame.grid,
            &app.preview.rows[index],
            Rect::new(inner.x, y, inner.w, 1),
            changes,
            hunks,
        );
        y += 1;
    }
    if split {
        let middle = inner.x + inner.w / 2;
        let surface = GitSurface::Pane;
        frame.grid.put_text_clipped(
            inner.x + 1,
            y,
            "Before",
            bold(GitRole::Removed, surface),
            middle,
        );
        frame.grid.put_text_clipped(
            middle + 2,
            y,
            "After",
            bold(GitRole::Added, surface),
            inner.right(),
        );
        y += 1;
    }

    let body = Rect::new(inner.x, y, inner.w, inner.bottom().saturating_sub(y));
    let visible = usize::from(body.h);
    app.preview_rows = (if split {
        app.preview.split.len()
    } else {
        app.preview.rows.len()
    })
    .saturating_sub(usize::from(pinned.is_some()));
    app.preview_visible = visible;
    app.preview_offset = app
        .preview_offset
        .min(app.preview_rows.saturating_sub(visible));
    if let Some(cursor) = app
        .preview_cursor
        .and_then(|index| app.preview.display_position(index, split, pinned))
    {
        if cursor < app.preview_offset {
            app.preview_offset = cursor;
        } else if cursor >= app.preview_offset + visible {
            app.preview_offset = cursor + 1 - visible.max(1);
        }
    }
    let offset = app.preview_offset;
    let number_width = app.preview.number_width().max(3);
    let gutter = Gutter::new(body.w, number_width, app.preview.patch, split);
    let code_width = usize::from(body.w.saturating_sub(gutter.width() + 2));
    app.reveal_source_match(code_width.saturating_sub(1));
    let pan = app.preview_column;
    let range = app
        .preview_anchor
        .zip(app.preview_cursor)
        .map(|(anchor, cursor)| anchor.min(cursor)..=anchor.max(cursor));

    if split {
        let middle = body.x + body.w / 2;
        let pairs = app
            .preview
            .split
            .iter()
            .filter(|pair| pinned.is_none() || pair.heading != pinned)
            .skip(offset)
            .take(visible)
            .map(|pair| (pair.heading, pair.old, pair.new))
            .collect::<Vec<_>>();
        for (line, (heading, old, new)) in pairs.into_iter().enumerate() {
            let y = body.y + line as u16;
            if let Some(index) = heading {
                let cursor_x = special_row(
                    app,
                    &mut frame.grid,
                    index,
                    Rect::new(body.x, y, body.w, 1),
                    changes,
                    hunks,
                    active_hunk.as_ref(),
                );
                if focused && app.preview_cursor == Some(index) {
                    frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
                }
                continue;
            }
            for (index, side, old_side) in [
                (old, Rect::new(body.x, y, middle - body.x, 1), true),
                (
                    new,
                    Rect::new(middle + 1, y, body.right() - middle - 1, 1),
                    false,
                ),
            ] {
                if let Some(index) = index {
                    let cursor_x = code_row(
                        app,
                        &mut frame.grid,
                        index,
                        side,
                        &gutter,
                        Some(old_side),
                        pan,
                        range.as_ref(),
                    );
                    if focused && app.preview_cursor == Some(index) {
                        frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
                    }
                }
            }
            frame
                .grid
                .set_cell(middle, y, "│", st(GitRole::Border, GitSurface::Pane));
        }
    } else {
        let indices = (0..app.preview.rows.len())
            .filter(|index| Some(*index) != pinned)
            .skip(offset)
            .take(visible)
            .collect::<Vec<_>>();
        for (line, index) in indices.into_iter().enumerate() {
            let y = body.y + line as u16;
            let row_rect = Rect::new(body.x, y, body.w, 1);
            let cursor_x = if let Some(entry) = app.ref_log.get(index).filter(|_| {
                app.ref_preview_target.is_some() && app.ref_log.len() == app.preview.rows.len()
            }) {
                ref_log_row(app, &mut frame.grid, index, entry, row_rect)
            } else if app.preview.rows[index].marker.is_some()
                || app.preview.rows[index].new.is_some()
            {
                code_row(
                    app,
                    &mut frame.grid,
                    index,
                    row_rect,
                    &gutter,
                    None,
                    pan,
                    range.as_ref(),
                )
            } else {
                special_row(
                    app,
                    &mut frame.grid,
                    index,
                    row_rect,
                    changes,
                    hunks,
                    active_hunk.as_ref(),
                )
            };
            if focused && app.preview_cursor == Some(index) {
                frame.cursor = Some((cursor_x, y, SetCursorStyle::SteadyBlock));
            }
        }
    }
    if app.preview_rows > visible && visible > 0 {
        let label = format!(
            " {}–{}/{} ",
            offset + 1,
            (offset + visible).min(app.preview_rows),
            app.preview_rows
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
    if focused && frame.cursor.is_none() {
        frame.cursor = Some((body.x + 1, body.y, SetCursorStyle::SteadyBlock));
    }
}

/// The file band: glyph, bold path, muted layer context, `+n −m` on the right.
fn file_band(
    app: &GitScreen,
    grid: &mut CellGrid,
    row: &PreviewRow,
    rect: Rect,
    changes: bool,
    hunks: usize,
) {
    let surface = GitSurface::FileBand;
    chrome::fill(grid, rect, st(GitRole::Text, surface));
    let (name, counts) = match row.text.rsplit_once("   ") {
        Some((name, counts)) => (name.to_owned(), counts.to_owned()),
        None => (row.text.clone(), String::new()),
    };
    let mode = app.icon_mode;
    let mut x = rect.x + 1;
    if let Some(path) = &row.path {
        let (icon, group) = super::glyphs::file(path, mode);
        x = grid.put_text_clipped(
            x,
            rect.y,
            &format!("{icon} "),
            syntax(group, surface),
            rect.right(),
        );
    }
    let mut context = String::new();
    // A file with edits in both layers: the band's stat is the shown layer's, the list's
    // is both together, so the band names its layer (`unstaged +3 −1`).
    let mut layer = "";
    if changes {
        let staged = app.preview_staged();
        context.push_str(if staged { " · Staged" } else { " · Unstaged" });
        context.push_str(&format!(
            " · {hunks} hunk{}",
            if hunks == 1 { "" } else { "s" }
        ));
        if let Some(entry) = app.selected_row().and_then(|row| {
            super::super::memo::entry(&app.snapshot.as_ref()?.entries, &row.value).cloned()
        }) && entry.sources.staged
            && entry.sources.unstaged
        {
            context.push_str(if staged {
                " · Working tree differs"
            } else {
                " · Index has staged edits"
            });
            layer = if staged { "staged " } else { "unstaged " };
        }
    }
    let (added, removed) = counts
        .split_once(' ')
        .map_or((String::new(), String::new()), |(a, r)| {
            (a.to_owned(), r.to_owned())
        });
    let counts_width = chrome::width(&added) + chrome::width(&removed) + 2;
    // The layer word is a label for the stat; the context names the layer too, so on a
    // narrow band the word gives way before the context is cut.
    let fits = |layer: &str| {
        let right = rect
            .right()
            .saturating_sub(counts_width + chrome::width(layer));
        x + chrome::width(&name) + chrome::width(&context) < right
    };
    let layer = if added.is_empty() || !fits(layer) {
        ""
    } else {
        layer
    };
    let right = rect
        .right()
        .saturating_sub(counts_width + chrome::width(layer));
    x = grid.put_text_clipped(
        x,
        rect.y,
        &chrome::ellipsize(&name, usize::from(right.saturating_sub(x + 1))),
        bold(GitRole::Text, surface),
        right,
    );
    let room = usize::from(right.saturating_sub(x + 1));
    grid.put_text_clipped(
        x,
        rect.y,
        &chrome::ellipsize(&context, room),
        st(GitRole::Muted, surface),
        right,
    );
    if !added.is_empty() {
        let at = grid.put_text_clipped(
            right,
            rect.y,
            layer,
            st(GitRole::Muted, surface),
            rect.right(),
        );
        let at = grid.put_text_clipped(
            at,
            rect.y,
            &added,
            st(GitRole::Added, surface),
            rect.right(),
        );
        grid.put_text_clipped(
            at + 1,
            rect.y,
            &removed,
            st(GitRole::Removed, surface),
            rect.right(),
        );
    }
}

/// A branch's commit as History draws it: the hash in its hue, the subject, and the author
/// and date at the right when the whole subject fits beside them (the subject comes first).
/// The ref's relation to HEAD is the pane's heading, so rows name no refs. The row's text is
/// `hash  subject`, so find and the cursor still work.
fn ref_log_row(
    app: &GitScreen,
    grid: &mut CellGrid,
    index: usize,
    entry: &super::super::commit::RefLogEntry,
    rect: Rect,
) -> u16 {
    let surface = GitSurface::Pane;
    let right = rect.right();
    let text = &app.preview.rows[index].text;
    let end = (rect.x + 1).saturating_add(chrome::width(text));
    if let Some((author, date)) = &entry.attribution {
        let tail = format!(
            "{author} · {}",
            chrome::when(date, crate::forge::records::now())
        );
        let at = right.saturating_sub(chrome::width(&tail) + 1);
        if at >= end + 3 {
            grid.put_text_clipped(at, rect.y, &tail, st(GitRole::Muted, surface), right);
        }
    }
    let cursor_x = paint_code(
        app,
        grid,
        index,
        rect.x + 1,
        rect.y,
        right,
        0,
        surface,
        st(GitRole::Text, surface),
    );
    // The hash in History's hue, unless find has marked it.
    if app.preview.search.row_matches(index).is_empty() {
        let hash = &text[..text.len().min(7)];
        grid.put_text_clipped(rect.x + 1, rect.y, hash, st(GitRole::Hash, surface), right);
    }
    cursor_x
}

/// Rows without code: file bands, hunk bands and messages.
fn special_row(
    app: &GitScreen,
    grid: &mut CellGrid,
    index: usize,
    rect: Rect,
    changes: bool,
    hunks: usize,
    active: Option<&std::ops::Range<usize>>,
) -> u16 {
    let row = &app.preview.rows[index];
    if row.style == Style::GitHeading {
        file_band(app, grid, row, rect, changes, hunks);
        return rect.x + 1;
    }
    if row.text.starts_with("@@") {
        let surface = GitSurface::HunkBand;
        chrome::fill(grid, rect, st(GitRole::Text, surface));
        let bold_hunk = active.is_some_and(|range| range.start == index);
        let (range, context) = match row.text[2..].find("@@") {
            Some(at) => row.text.split_at(at + 4),
            None => (row.text.as_str(), ""),
        };
        let x = grid.put_text_clipped(
            rect.x + 1,
            rect.y,
            range,
            if bold_hunk {
                bold(GitRole::Focus, surface)
            } else {
                st(GitRole::Focus, surface)
            },
            rect.right(),
        );
        grid.put_text_clipped(
            x,
            rect.y,
            context,
            st(GitRole::Muted, surface),
            rect.right(),
        );
        return rect.x + 1;
    }
    let surface = GitSurface::Pane;
    let style = match row.style {
        Style::Accent => bold(GitRole::Focus, surface),
        Style::Muted => st(GitRole::Muted, surface),
        Style::GitSignAdded => st(GitRole::Added, surface),
        Style::GitSignDeleted => st(GitRole::Removed, surface),
        _ => st(GitRole::Text, surface),
    };
    paint_code(
        app,
        grid,
        index,
        rect.x + 1,
        rect.y,
        rect.right(),
        0,
        surface,
        style,
    )
}

#[derive(Clone, Copy)]
struct Gutter {
    number_width: u16,
    patch: bool,
    /// One number column (narrow readers and split halves).
    single: bool,
}

impl Gutter {
    fn new(width: u16, number_width: usize, patch: bool, split: bool) -> Self {
        Self {
            number_width: number_width as u16,
            patch,
            single: split || width < 70 || !patch,
        }
    }
    /// Cells before the code, including the range edge column: edge, numbers, ` │ `,
    /// the `+`/`−` marker and a space. Every row of a reader has the same gutter, so code
    /// starts in one column whether a row is context, added or removed.
    fn width(&self) -> u16 {
        let numbers = if self.single {
            self.number_width
        } else {
            self.number_width * 2 + 1
        };
        if self.patch {
            1 + numbers + 3 + 2
        } else {
            1 + numbers + 3
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn code_row(
    app: &GitScreen,
    grid: &mut CellGrid,
    index: usize,
    rect: Rect,
    gutter: &Gutter,
    side: Option<bool>,
    pan: usize,
    range: Option<&std::ops::RangeInclusive<usize>>,
) -> u16 {
    let row = &app.preview.rows[index];
    let in_range = range.is_some_and(|range| range.contains(&index)) && row.marker.is_some();
    let is_cursor = app.focus == 4 && app.preview_cursor == Some(index) && app.preview.patch;
    // A conflicted file's sides (`conflicts.rs`): ours on the added band, theirs on the
    // hunk band, the base and Git's marker lines on the pane.
    let conflict = !app.preview.patch;
    let band = match row.marker {
        Some('+') => GitSurface::AddedBand,
        Some('-') => GitSurface::RemovedBand,
        Some(conflicts::OURS) if conflict => GitSurface::AddedBand,
        Some(conflicts::THEIRS) if conflict => GitSurface::HunkBand,
        _ => GitSurface::Pane,
    };
    let surface = if in_range || is_cursor {
        GitSurface::Cursor
    } else {
        band
    };
    chrome::fill(grid, rect, st(GitRole::Text, surface));
    if in_range {
        grid.set_cell(
            rect.x,
            rect.y,
            glyph(Glyph::RangeEdge, app.icon_mode),
            bold(GitRole::Focus, surface),
        );
    }
    let number_role = match row.marker {
        Some('+') => GitRole::Added,
        Some('-') => GitRole::Removed,
        Some(conflicts::OURS) if conflict => GitRole::Added,
        Some(conflicts::THEIRS) if conflict => GitRole::Focus,
        Some(conflicts::MARKER_ROW) if conflict => GitRole::Conflict,
        _ => GitRole::Muted,
    };
    let width = usize::from(gutter.number_width);
    let numbers = if gutter.single {
        let number = match side {
            Some(true) => row.old,
            Some(false) => row.new,
            None => row.new.or(row.old),
        };
        format!(
            "{:>width$}",
            number.map(|n| n.to_string()).unwrap_or_default()
        )
    } else {
        format!(
            "{:>width$} {:>width$}",
            row.old.map(|n| n.to_string()).unwrap_or_default(),
            row.new.map(|n| n.to_string()).unwrap_or_default(),
        )
    };
    let mut x = grid.put_text_clipped(
        rect.x + 1,
        rect.y,
        &numbers,
        st(number_role, surface),
        rect.right(),
    );
    if gutter.patch {
        // `old new │ ± code`: a rule ends the numbers on every row, and the marker column
        // after it is blank on context rows, so the code axis is visible (r16 git-tab-04).
        x = grid.put_text_clipped(x, rect.y, " │ ", st(GitRole::Border, surface), rect.right());
        let sign = match row.marker {
            Some('+') => "+",
            Some('-') => "−",
            _ => " ",
        };
        x = grid.put_text_clipped(x, rect.y, sign, bold(number_role, surface), rect.right());
        x += 1;
    } else {
        x = grid.put_text_clipped(x, rect.y, " │ ", st(GitRole::Border, surface), rect.right());
    }
    let plain = match row.marker {
        Some(conflicts::MARKER_ROW) if conflict => bold(GitRole::Conflict, surface),
        Some(conflicts::BASE) if conflict => st(GitRole::Muted, surface),
        _ => st(GitRole::Text, surface),
    };
    paint_code(
        app,
        grid,
        index,
        x,
        rect.y,
        rect.right(),
        pan,
        surface,
        plain,
    )
}

/// Code with syntax spans on `surface`, search matches, panning marks; returns the
/// column for the native cursor.
#[allow(clippy::too_many_arguments)]
fn paint_code(
    app: &GitScreen,
    grid: &mut CellGrid,
    index: usize,
    mut x: u16,
    y: u16,
    right: u16,
    offset: usize,
    surface: GitSurface,
    plain: Style,
) -> u16 {
    let row = &app.preview.rows[index];
    let matches = app.preview.search.row_matches(index);
    let cursor_byte = app
        .preview
        .cursor_byte
        .filter(|(row, _)| *row == index && app.preview_cursor == Some(index))
        .map(|(_, byte)| byte);
    let mode = app.icon_mode;
    if x >= right {
        return right.saturating_sub(1);
    }
    if offset > 0 && !row.text.is_empty() {
        grid.set_cell(
            x,
            y,
            glyph(Glyph::ClipLeft, mode),
            st(GitRole::Muted, surface),
        );
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
            let group = row
                .spans
                .iter()
                .find(|(range, _)| range.contains(&at))
                .map(|(_, group)| *group);
            let candidate = matches.partition_point(|found| found.bytes.end <= at);
            let matched = matches
                .get(candidate)
                .is_some_and(|found| found.bytes.start < at + grapheme.len());
            let style = if matched {
                Style::Search
            } else {
                group.map_or(plain, |group| syntax(group, surface))
            };
            let next = if grapheme == "\t" || column < offset {
                grid.put_text_clipped(x, y, &" ".repeat(end - column.max(offset)), style, right)
            } else {
                grid.put_text_clipped(x, y, grapheme, style, right)
            };
            if next == x {
                grid.set_cell(
                    right - 1,
                    y,
                    glyph(Glyph::ClipRight, mode),
                    st(GitRole::Muted, surface),
                );
                break;
            }
            if cursor_byte.is_some_and(|byte| at <= byte && byte < at + grapheme.len())
                || (cursor_byte.is_none()
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
