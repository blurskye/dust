//! F-PRFILES / F-PRSPLIT (DESIGN.md §3.28, §3.29): the changed-file outline beside the
//! selected file's diff, unified or split (`u`). Rows keep their inline-comment actions, so
//! `c` comments on the line under the cursor in either layout.
use super::super::preview::{Preview, PreviewRow};
use super::super::shell::glyphs::{Glyph, glyph};
use super::prose::{Rich, muted, role, strong};
use super::lines::Line;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::records::DetailRecord;
use crate::ui::{GitRole, GitSurface, Style};
use std::path::{Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Split needs room for two readable halves.
pub(super) const SPLIT_WIDTH: usize = 96;

/// The diff's own width once the outline takes its share of the reader (as `paint` lays it).
pub(super) fn diff_width(workspace: &Workspace, width: usize) -> usize {
    if width + 5 >= 76 && width >= 72 && !workspace.reading_position.narrow_pr {
        width.saturating_sub((width * 22 / 100).clamp(24, 44) + 2)
    } else {
        width
    }
}

pub(super) fn rows(
    workspace: &Workspace,
    id: u64,
    item: &Value,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    width: usize,
    mode: ExplorerIconMode,
) {
    let wide = width + 5 >= 76 && !workspace.reading_position.narrow_pr;
    let selected = workspace.selected_file(id, item).to_owned();
    if wide {
        outline(workspace, id, record, &selected, rows, mode);
    }
    let Some((index, file)) = record.files.iter().enumerate().find(|(_, file)| file.path == selected) else {
        rows.push(muted("No changed files"));
        return;
    };
    // The file heading: pinned above the diff, Enter picks another file.
    let viewed = workspace.file_viewed(id, &selected);
    let mut heading = Rich::new()
        .push(format!("{} ", glyph(Glyph::File, mode)), role(GitRole::Focus))
        .push(selected.clone(), strong(GitRole::Text))
        .push("  ", role(GitRole::Text))
        .push(format!("+{}", file.additions), role(GitRole::Added))
        .push(format!(" −{}", file.deletions), role(GitRole::Removed))
        .sep()
        .push(format!("{}/{}", index + 1, record.files.len()), role(GitRole::Muted));
    let diff = diff_width(workspace, width);
    let split_view = workspace.split_diff && diff >= SPLIT_WIDTH;
    if split_view {
        heading = heading.sep().push("split", role(GitRole::Muted));
    } else if workspace.split_diff {
        // The preference stays split; this width shows unified until there is room.
        // (the footer says why).
        heading = heading.sep().push("unified", role(GitRole::Modified));
    }
    let mut heading = heading.line();
    heading.action = Some(Action::Files(id));
    heading.band = Some(GitSurface::FileBand);
    rows.push(heading);
    match viewed {
        Some(viewed) => {
            let mut row = if viewed {
                Rich::new()
                    .push(format!("{} ", glyph(Glyph::Viewed, mode)), role(GitRole::Added))
                    .push("Viewed", role(GitRole::Added))
                    .sep()
                    .push("v unmark", role(GitRole::Muted))
                    .line()
            } else {
                Rich::new()
                    .push("○ ", role(GitRole::Muted))
                    .push("Mark viewed", role(GitRole::Link))
                    .sep()
                    .push("v", role(GitRole::Muted))
                    .line()
            };
            row.action = Some(Action::Viewed(id, selected.clone()));
            rows.push(row);
        }
        None => {
            lines::error_or_loading(workspace, &Key::Viewed(id), rows);
        }
    }
    if lines::error_or_loading(workspace, &Key::Diff(id), rows) {
        return;
    }
    let preview = Preview::git(workspace.raw(&Key::Diff(id)).unwrap_or_default(), None);
    let head = record.head_sha.clone();
    let path = Path::new(&selected);
    let diff_start = rows.len();
    if split_view {
        split(&preview, path, id, &head, rows, diff);
    } else {
        unified(&preview, path, id, &head, rows);
    }
    let conversation = if wide {
        width.saturating_sub(((width + 5) / 3).clamp(24, 42))
    } else {
        width
    };
    let mut threads = Vec::new();
    lines::threads(workspace, id, &mut threads, conversation.saturating_sub(6), Some(&selected));
    place_threads(workspace, id, Some(&selected), rows, diff_start, threads);
    rows.push(super::prose::blank());
    let mut review = Rich::new()
        .push("› ", role(GitRole::Focus))
        .push("Review changes", role(GitRole::Link))
        .push("  r", role(GitRole::Muted))
        .line();
    review.action = Some(Action::Review(id));
    rows.push(review);
}

/// The tree of changed files: folders collapse, files carry ± and a viewed mark.
fn outline(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    selected: &str,
    rows: &mut Vec<Line>,
    mode: ExplorerIconMode,
) {
    let collapsed = workspace
        .collapsed
        .iter()
        .filter(|(number, _)| *number == id)
        .map(|(_, path)| PathBuf::from(path))
        .collect();
    let tree = super::super::tree::rows(
        record.files.iter().map(|file| (PathBuf::from(&file.path), String::new())).collect(),
        &collapsed,
    );
    let mut heading = Rich::new()
        .push("Files", strong(GitRole::Text))
        .push(format!("  {}", record.files.len()), role(GitRole::Muted))
        .line();
    heading.outline = true;
    rows.push(heading);
    for entry in tree {
        let path = entry.value.to_string_lossy().into_owned();
        let file = record.files.iter().find(|file| file.path == path);
        let mut row = Rich::new();
        if entry.directory {
            row = row.push(entry.label.clone(), role(GitRole::Muted));
        } else {
            let viewed = workspace.file_viewed(id, &path) == Some(true);
            let name_role = if path == selected { GitRole::Focus } else { GitRole::Text };
            row = row.push(entry.label.clone(), if path == selected { strong(name_role) } else { role(name_role) });
            if let Some(file) = file {
                row = row
                    .push(format!("  +{}", file.additions), role(GitRole::Added))
                    .push(format!(" −{}", file.deletions), role(GitRole::Removed));
            }
            if viewed {
                row = row.push(format!(" {}", glyph(Glyph::Viewed, mode)), role(GitRole::Added));
            }
        }
        let mut line = row.line();
        line.outline = true;
        line.action = Some(if entry.directory {
            Action::Collapse(id, path)
        } else {
            Action::SelectFile(id, path)
        });
        rows.push(line);
    }
}

fn inline(id: u64, path: &Path, source: &PreviewRow, head: &str) -> Option<Action> {
    let line = source.new.or(source.old)?;
    Some(Action::Inline {
        number: id,
        path: path.to_string_lossy().into_owned(),
        line,
        side: if source.new.is_some() { "RIGHT" } else { "LEFT" },
        head: head.to_owned(),
    })
}

fn hunk_row(text: &str) -> Line {
    // `@@ -10,7 +10,9 @@ def retry` — the range muted, the enclosing scope in focus.
    let (range, scope) = match text.match_indices("@@").nth(1) {
        Some((at, _)) => (&text[..at + 2], text[at + 2..].trim()),
        None => (text, ""),
    };
    let mut row = Rich::new().push(range.to_owned(), role(GitRole::Muted));
    if !scope.is_empty() {
        row = row.push(format!("  {scope}"), role(GitRole::Focus));
    }
    let mut row = row.line();
    // Hunk headers are chrome: find searches the code, not `@@` ranges.
    row.band = Some(GitSurface::HunkBand);
    row
}

fn unified(preview: &Preview, path: &Path, id: u64, head: &str, rows: &mut Vec<Line>) {
    let number_width = preview.number_width();
    for source in preview
        .rows
        .iter()
        .filter(|row| row.path.as_deref() == Some(path) && row.style != Style::GitHeading)
    {
        if source.marker.is_none() && source.text.starts_with("@@") {
            rows.push(hunk_row(&source.text));
            continue;
        }
        let mut row = Line::text(&source.text, source.style);
        if source.old.is_some() || source.new.is_some() {
            row.source = Some(find::Source::new(&source.text));
        }
        row.diff = match source.marker {
            Some('+') => Some(true),
            Some('-') => Some(false),
            _ => None,
        };
        row.spans = source.spans.clone();
        if source.marker.is_some() {
            row.gutter = format!(
                "{:>number_width$} {:>number_width$} {} ",
                source.old.map(|v| v.to_string()).unwrap_or_default(),
                source.new.map(|v| v.to_string()).unwrap_or_default(),
                source.marker.unwrap_or(' ')
            );
            row.action = inline(id, path, source, head);
        }
        rows.push(row);
    }
}

/// `text` cut to `room` cells (tabs as four spaces), padded; the byte length kept from
/// the source so syntax spans stay aligned.
fn fit(text: &str, room: usize) -> (String, usize) {
    let mut out = String::new();
    let mut used = 0;
    let mut kept = 0;
    let mut exact = true;
    for (at, grapheme) in text.grapheme_indices(true) {
        let (shown, size) = if grapheme == "\t" {
            exact = false;
            ("    ", 4)
        } else {
            (grapheme, grapheme.width())
        };
        if used + size > room {
            break;
        }
        out.push_str(shown);
        used += size;
        kept = at + grapheme.len();
    }
    out.push_str(&" ".repeat(room - used));
    (out, if exact { kept } else { 0 })
}

/// One side of a split row, `  42 − code…`, starting at byte `offset` of `line`: its
/// styles, syntax spans and band go onto `line`, its text is returned.
fn side(
    source: Option<&PreviewRow>,
    number_width: usize,
    room: usize,
    offset: usize,
    line: &mut Line,
) -> String {
    let Some(source) = source else {
        return " ".repeat(room);
    };
    let number = if source.marker == Some('+') { source.new } else { source.old.or(source.new) };
    let sign = match source.marker.unwrap_or(' ') {
        '-' => '−',
        other => other,
    };
    let digits = format!("{:>number_width$}", number.map(|v| v.to_string()).unwrap_or_default());
    let prefix = format!("{digits} {sign} ");
    let code_room = room.saturating_sub(prefix.width());
    let (code, kept) = fit(&source.text, code_room);
    let start = offset + prefix.len();
    let muted = |role| Style::Git {
        role,
        surface: GitSurface::Pane,
        emphasis: crate::ui::Emphasis::None,
    };
    line.paint.push((offset..offset + digits.len(), muted(GitRole::Muted)));
    let (band, sign_role) = match source.marker {
        Some('+') => (Some(GitSurface::AddedBand), GitRole::Added),
        Some('-') => (Some(GitSurface::RemovedBand), GitRole::Removed),
        _ => (None, GitRole::Muted),
    };
    let sign_at = offset + digits.len() + 1;
    line.paint.push((sign_at..sign_at + sign.len_utf8(), Style::Git {
        role: sign_role,
        surface: GitSurface::Pane,
        emphasis: crate::ui::Emphasis::Bold,
    }));
    for (range, highlight) in &source.spans {
        if range.start < kept {
            line.spans.push((start + range.start..start + range.end.min(kept), *highlight));
        }
    }
    let text = format!("{prefix}{code}");
    if let Some(band) = band {
        line.bands.push((offset..offset + text.len(), band));
        if source.spans.is_empty() {
            line.paint.push((start..offset + text.len(), muted(sign_role)));
        }
    }
    text
}

fn split(preview: &Preview, path: &Path, id: u64, head: &str, rows: &mut Vec<Line>, width: usize) {
    let number_width = preview.number_width();
    let half = width.saturating_sub(1) / 2;
    let in_file = |index: usize| preview.rows.get(index).is_some_and(|row| row.path.as_deref() == Some(path));
    for pair in &preview.split {
        if let Some(heading) = pair.heading {
            if !in_file(heading) {
                continue;
            }
            let source = &preview.rows[heading];
            if source.style == Style::GitHeading {
                continue;
            }
            rows.push(hunk_row(&source.text));
            continue;
        }
        let old = pair.old.filter(|index| in_file(*index)).map(|index| &preview.rows[index]);
        let new = pair.new.filter(|index| in_file(*index)).map(|index| &preview.rows[index]);
        if old.is_none() && new.is_none() {
            continue;
        }
        let mut line = Line::text("", Style::Default);
        line.pan = false;
        let left = side(old, number_width, half, 0, &mut line);
        let divider = "│";
        line.paint.push((left.len()..left.len() + divider.len(), Style::Git {
            role: GitRole::Border,
            surface: GitSurface::Pane,
            emphasis: crate::ui::Emphasis::None,
        }));
        let right = side(new, number_width, width.saturating_sub(half + 1), left.len() + divider.len(), &mut line);
        line.text = format!("{left}{divider}{right}");
        line.source = Some(find::Source::new(&line.text));
        line.action = new
            .filter(|row| row.new.is_some())
            .and_then(|row| inline(id, path, row, head))
            .or_else(|| old.and_then(|row| inline(id, path, row, head)));
        rows.push(line);
    }
}

/// The Activity tab's review threads: the same cards as Files, one after another.
pub(super) fn thread_cards(workspace: &Workspace, id: u64, rows: &mut Vec<Line>, width: usize, path: Option<&str>) {
    let mut threads = Vec::new();
    lines::threads(workspace, id, &mut threads, width.saturating_sub(6), path);
    let start = rows.len();
    place_threads(workspace, id, path, rows, start, threads);
}

/// Review threads sit under the line they discuss, as cards with a rail coloured by state
/// (GitHub's inline conversations). Threads whose line is not in the diff follow it.
fn place_threads(workspace: &Workspace, id: u64, selected: Option<&str>, rows: &mut Vec<Line>, diff_start: usize, threads: Vec<Line>) {
    let anchors = workspace
        .review_threads(id)
        .into_iter()
        .filter(|thread| {
            selected.is_none_or(|selected| {
                array(&thread["comments"], "nodes")
                    .iter()
                    .any(|comment| string(comment, "path") == selected)
            })
        })
        .map(|thread| {
            let first = array(&thread["comments"], "nodes").first();
            let line = first.and_then(|first| first["line"].as_u64().or_else(|| first["originalLine"].as_u64()));
            (line.map(|line| line as u32), thread["isResolved"].as_bool() == Some(true))
        })
        .collect::<Vec<_>>();
    // Split the rendered threads into one card per thread; a loading or error row stays loose.
    let mut cards: Vec<Vec<Line>> = Vec::new();
    let mut loose = Vec::new();
    for row in threads {
        if row.thread_header {
            cards.push(vec![row]);
        } else if let Some(card) = cards.last_mut() {
            card.push(row);
        } else {
            loose.push(row);
        }
    }
    let mut placed: Vec<(usize, usize, Vec<Line>)> = Vec::new();
    let mut trailing = Vec::new();
    for (index, mut card) in cards.into_iter().enumerate() {
        let (line, resolved) = anchors.get(index).copied().unwrap_or((None, false));
        // The blank after a thread stays a plain separator (it keeps the thread's identity
        // for j/k); only the card itself is banded.
        let mut tail = Vec::new();
        while card.last().is_some_and(|row| row.text.is_empty() && row.action.is_none() && row.reply.is_none()) {
            tail.extend(card.pop());
        }
        let rail = Style::Git {
            role: if resolved { GitRole::Muted } else { GitRole::Modified },
            surface: GitSurface::Code,
            emphasis: crate::ui::Emphasis::None,
        };
        let target = line.and_then(|line| {
            rows[diff_start..]
                .iter()
                .rposition(|row| {
                    matches!(&row.action, Some(Action::Inline { line: at, side: "RIGHT", .. }) if *at == line)
                })
                .or_else(|| {
                    rows[diff_start..]
                        .iter()
                        .rposition(|row| matches!(&row.action, Some(Action::Inline { line: at, .. }) if *at == line))
                })
                .map(|at| diff_start + at + 1)
        });
        for row in &mut card {
            row.band = Some(GitSurface::Code);
            row.gutter = "  ▍ ".into();
            row.gutter_style = rail;
            row.pan = false;
        }
        if let Some(header) = card.first_mut() {
            style_header(header, resolved);
        }
        match target {
            Some(at) => {
                card.extend(tail);
                placed.push((at, index, card));
            }
            None => {
                trailing.extend(card);
                trailing.extend(tail);
            }
        }
    }
    // Insert from the bottom so earlier positions stay valid; threads on one line keep
    // their order (the later one goes in first).
    placed.sort_by_key(|(at, index, _)| std::cmp::Reverse((*at, *index)));
    for (at, _, card) in placed {
        rows.splice(at..at, card);
    }
    rows.extend(loose);
    if !trailing.is_empty() && rows.len() > diff_start {
        rows.push(super::prose::blank());
    }
    rows.extend(trailing);
}

/// `○ Unresolved · alex · line 14`: the state in its colour, the author bold.
fn style_header(header: &mut Line, resolved: bool) {
    let mut parts = header.text.match_indices(" · ").map(|(at, _)| at).collect::<Vec<_>>();
    parts.push(header.text.len());
    let state_end = parts[0];
    let tone = if resolved { GitRole::Added } else { GitRole::Modified };
    header.paint.push((0..state_end, Style::Git { role: tone, surface: GitSurface::Code, emphasis: crate::ui::Emphasis::None }));
    if parts.len() > 1 {
        let author = state_end + 3..parts[1];
        header.paint.push((author, Style::Git { role: GitRole::Author, surface: GitSurface::Code, emphasis: crate::ui::Emphasis::Bold }));
    }
    if parts.len() > 2 && !header.text[parts[1] + 3..parts[2]].starts_with("line ") {
        // Activity names the file: `orbit/queue.py:14` reads as a link.
        let file = parts[1] + 3..parts[2];
        header.paint.push((file, Style::Git { role: GitRole::Link, surface: GitSurface::Code, emphasis: crate::ui::Emphasis::None }));
    }
    header.paint.push((0..header.text.len(), Style::Git { role: GitRole::Muted, surface: GitSurface::Code, emphasis: crate::ui::Emphasis::None }));
    // `paint` is searched from the end; keep the specific ranges last.
    header.paint.rotate_right(1);
}
