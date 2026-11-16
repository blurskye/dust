//! F-PRFILES (DESIGN.md §3.28, §3.29): GitHub's "Files changed". The changed-file tree, each
//! file with its Viewed box and the review threads on it, sits beside the selected file's
//! unified diff, threads under the lines they discuss. Long lines wrap by default (`w` pans
//! them sideways instead): a continuation row has a blank gutter, sits under the code and
//! keeps its line's inline-comment action. A wrapped line is one cursor stop, its rows lit
//! together, so `j`/`k` (and a `V` range) move by source line at every width, as GitHub's do.
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

/// Whether the changed-file tree (and an open commit's commit list) sits beside the diff. It
/// does wherever the list is beside the reader, at every width (the list narrows for it
/// first, `paint::split_list_width`), and when `Space e` hid the list; only a Narrow terminal,
/// whose reader stands alone, picks files with `f` (the heading above the diff) instead.
pub(super) fn outline_beside(workspace: &Workspace, width: usize) -> bool {
    width >= super::paint::OUTLINE_MIN && !workspace.reading_position.narrow_pr
}

/// Each changed file's `+N −M` counted from a unified diff's hunks.
fn hunk_stats(diff: &str) -> std::collections::BTreeMap<String, (u64, u64)> {
    let mut stats = std::collections::BTreeMap::new();
    let mut path: Option<String> = None;
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            path = rest.rsplit_once(" b/").map(|(_, path)| path.to_owned());
            in_hunk = false;
            if let Some(path) = &path {
                stats.entry(path.clone()).or_insert((0, 0));
            }
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk && let Some(path) = &path {
            let entry = stats.entry(path.clone()).or_insert((0, 0));
            match line.as_bytes().first() {
                Some(b'+') => entry.0 += 1,
                Some(b'-') => entry.1 += 1,
                _ => {}
            }
        }
    }
    stats
}

/// The diff's counts once it is here, so the tree, the file heading, `Changes` and the
/// merge box's `Brings` all count the hunks on screen (R16 prs-02); the forge's own numbers
/// until then, or for a file the diff does not carry.
pub(super) fn diff_counted(workspace: &Workspace, id: u64, record: &DetailRecord) -> DetailRecord {
    let mut record = record.clone();
    let Some(diff) = workspace.raw(&Key::Diff(id)) else {
        return record;
    };
    let stats = hunk_stats(diff);
    if stats.is_empty() {
        return record;
    }
    for file in &mut record.files {
        if let Some((additions, deletions)) = stats.get(&file.path) {
            (file.additions, file.deletions) = (*additions, *deletions);
        }
    }
    // The totals too, when the diff holds every file the forge counted.
    let changed = record.row.changed_files.map_or(record.files.len(), |n| n as usize);
    if stats.len() >= changed.max(record.files.len()) {
        record.row.additions = Some(stats.values().map(|stat| stat.0).sum());
        record.row.deletions = Some(stats.values().map(|stat| stat.1).sum());
    }
    record
}

/// The diff's own width once the tree takes its share of the reader (as `paint` lays it).
fn diff_width(workspace: &Workspace, width: usize) -> usize {
    if outline_beside(workspace, width) {
        width.saturating_sub(super::paint::outline_width(width) + 2)
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
    let record = &diff_counted(workspace, id, record);
    let wide = outline_beside(workspace, width);
    let selected = workspace.selected_file(id, item).to_owned();
    if wide {
        outline(workspace, id, record, &selected, rows, super::paint::outline_room(width), mode);
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
        .diffstat(file.additions, file.deletions)
        .sep()
        .push(format!("file {} of {}", index + 1, record.files.len()), role(GitRole::Muted));
    // Pending comments on this file (§3.28, §3.30a): counted only when there are some.
    let waiting = super::drafts::file_count(workspace, id, &selected);
    if waiting > 0 {
        heading = heading.sep().push(format!("{waiting} pending"), role(GitRole::Modified));
    }
    let mut heading = heading.line();
    heading.action = Some(Action::Files(id));
    heading.band = Some(GitSurface::FileBand);
    rows.push(heading);
    match viewed {
        Some(viewed) => {
            // GitHub's Viewed checkbox, the same box the file tree shows.
            let mut row = if viewed {
                Rich::new()
                    .push(format!("{} ", glyph(Glyph::Checked, mode)), role(GitRole::Added))
                    .push("Viewed", role(GitRole::Added))
                    .sep()
                    .push("v unmark", role(GitRole::Muted))
                    .line()
            } else {
                Rich::new()
                    .push(format!("{} ", glyph(Glyph::Unchecked, mode)), role(GitRole::Muted))
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
    let wrap = (!workspace.diff_nowrap).then(|| diff_width(workspace, width));
    let search = &workspace.detail_find.search;
    let finding = if search.active() { search.query.as_str() } else { "" };
    unified(&preview, path, id, &head, rows, wrap, finding);
    let conversation = if wide {
        width.saturating_sub(((width + 5) / 3).clamp(24, 42))
    } else {
        width
    };
    let mut threads = Vec::new();
    lines::threads(workspace, id, &mut threads, conversation.saturating_sub(6), Some(&selected));
    place_threads(workspace, id, Some(&selected), rows, diff_start, threads);
    super::drafts::mark_range(workspace, id, &selected, rows, diff_start);
    super::drafts::place(workspace, id, &selected, rows, diff_start, conversation, mode);
    rows.push(super::prose::blank());
    rows.push(super::drafts::review_row(workspace, id));
}

/// The review threads on `path`, and whether any of them is still open.
fn thread_count(workspace: &Workspace, id: u64, path: &str) -> (usize, bool) {
    let threads = workspace
        .review_threads(id)
        .into_iter()
        .filter(|thread| array(&thread["comments"], "nodes").iter().any(|comment| string(comment, "path") == path))
        .collect::<Vec<_>>();
    (threads.len(), threads.iter().any(|thread| thread["isResolved"].as_bool() != Some(true)))
}

/// The tree of changed files: folders collapse; files carry their Viewed box, their review
/// threads and ±. Each level indents one cell and shows basenames, so the tree reads in its
/// narrowest column (`room` cells): a long name ends in `…`, and ± goes before the name does.
fn outline(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    selected: &str,
    rows: &mut Vec<Line>,
    room: usize,
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
    // No `Files  2` heading: the tab above counts them (r12 prs-10).
    for entry in tree {
        let path = entry.value.to_string_lossy().into_owned();
        let file = record.files.iter().find(|file| file.path == path);
        let mut row = Rich::new();
        // `tree::rows` indents two cells a level; the tree beside the diff indents one.
        let (indent, label) = entry.label.split_at(entry.indent.min(entry.label.len()));
        let indent = " ".repeat(indent.len() / 2);
        if entry.directory {
            let label = super::super::shell::chrome::ellipsize(label, room.saturating_sub(indent.len()));
            row = row.plain(&indent).push(label, role(GitRole::Muted));
        } else {
            // `☐ queue.py ❝ 2  +13 −2`: the Viewed box first, where the eye runs down the
            // tree (none while the forge has not said, or cannot), then the review threads
            // on the file (the open ones in their colour).
            let name_role = if path == selected { GitRole::Focus } else { GitRole::Text };
            row = row.plain(&indent);
            match workspace.file_viewed(id, &path) {
                Some(true) => row = row.push(format!("{} ", glyph(Glyph::Checked, mode)), role(GitRole::Added)),
                Some(false) => row = row.push(format!("{} ", glyph(Glyph::Unchecked, mode)), role(GitRole::Muted)),
                None => {}
            }
            let (threads, open) = thread_count(workspace, id, &path);
            let mut threads = (threads > 0).then(|| (format!(" {} {threads}", glyph(Glyph::Comment, mode)), open));
            let mut stat = file.map(|file| Rich::new().plain("  ").diffstat(file.additions, file.deletions));
            // What does not fit goes whole, never as `+…`: the thread count first, then ±,
            // and only then is the name cut (it keeps at least a few cells).
            let extra = |threads: &Option<(String, bool)>, stat: &Option<Rich>| {
                threads.as_ref().map_or(0, |(text, _)| text.width()) + stat.as_ref().map_or(0, Rich::width)
            };
            let room_left = room.saturating_sub(row.width());
            if label.width() + extra(&threads, &stat) > room_left {
                threads = None;
            }
            if label.width().min(8) + extra(&threads, &stat) > room_left {
                stat = None;
            }
            let name_room = room_left.saturating_sub(extra(&threads, &stat));
            let name = super::super::shell::chrome::ellipsize(label, name_room.max(4));
            row = row.push(name, if path == selected { strong(name_role) } else { role(name_role) });
            if let Some((text, open)) = threads {
                let tone = if open { GitRole::Modified } else { GitRole::Muted };
                row = row.push(text, role(tone));
            }
            if let (Some(_), Some(file)) = (stat, file) {
                row = row.plain("  ").diffstat(file.additions, file.deletions);
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

/// The selected file's rows. With `wrap` (the diff's width) a long line continues on rows
/// with a blank gutter, indented to the line's own indentation; each part keeps the line's
/// diff band, its syntax spans and its inline-comment action, and find reads the parts as
/// one line. Without it a row is the whole line and pans sideways.
/// `finding`: the open find's query, whose matches a wrapped line never splits.
fn unified(preview: &Preview, path: &Path, id: u64, head: &str, rows: &mut Vec<Line>, wrap: Option<usize>, finding: &str) {
    let number_width = preview.number_width();
    for source in preview
        .rows
        .iter()
        .filter(|row| row.path.as_deref() == Some(path) && row.style != Style::GitHeading)
    {
        if source.marker.is_none() && source.text.starts_with("@@") {
            let mut row = hunk_row(&source.text);
            row.pan = wrap.is_none();
            rows.push(row);
            continue;
        }
        let diff = match source.marker {
            Some('+') => Some(true),
            Some('-') => Some(false),
            _ => None,
        };
        let (gutter, action) = if source.marker.is_some() {
            let gutter = format!(
                "{:>number_width$} {:>number_width$} {} ",
                source.old.map(|v| v.to_string()).unwrap_or_default(),
                source.new.map(|v| v.to_string()).unwrap_or_default(),
                source.marker.unwrap_or(' ')
            );
            (gutter, inline(id, path, source, head))
        } else {
            (String::new(), None)
        };
        let parts = match wrap {
            Some(width) if source.marker.is_some() => {
                let room = width.saturating_sub(gutter.width()).max(8);
                let hang = indentation(&source.text).min(room / 2);
                let mut parts = breaks(&source.text, room, room - hang);
                // While a find is open a match reads whole on one row, as in a run's log:
                // a break inside one moves to the match's start.
                let keep = super::runs::matches(&source.text, finding);
                if keep.iter().any(|kept| parts.iter().any(|part| kept.start < part.start && part.start < kept.end)) {
                    parts = super::runs::wrap_points_keeping(&source.text, room - hang, &keep);
                }
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(part, range)| (range, if part == 0 { gutter.clone() } else { " ".repeat(gutter.width() + hang) }))
                    .collect::<Vec<_>>()
            }
            _ => vec![(0..source.text.len(), gutter)],
        };
        let logical: std::sync::Arc<str> = std::sync::Arc::from(source.text.as_str());
        for (range, gutter) in parts {
            let mut row = Line::text(&source.text[range.clone()], source.style);
            if source.old.is_some() || source.new.is_some() {
                row.source = Some(find::Source { text: logical.clone(), offset: range.start });
            }
            row.diff = diff;
            row.spans = source
                .spans
                .iter()
                .filter(|(span, _)| span.start < range.end && span.end > range.start)
                .map(|(span, highlight)| (span.start.max(range.start) - range.start..span.end.min(range.end) - range.start, *highlight))
                .collect();
            row.gutter = gutter;
            row.action = action.clone();
            if wrap.is_some() && source.marker.is_some() {
                row.pan = false;
            }
            rows.push(row);
        }
    }
}

/// Whether `rows[index]` continues the wrapped diff line above it: one cursor stop with its
/// first row, as GitHub selects whole lines (`j`/`k` step over it, the cursor band covers it).
pub(super) fn continues(rows: &[Line], index: usize) -> bool {
    index > 0
        && rows.get(index).is_some_and(|row| {
            matches!(row.action, Some(Action::Inline { .. }))
                && row.source.as_ref().is_some_and(|source| source.offset > 0)
                && rows[index - 1].action == row.action
        })
}

/// The first row of the diff line `index` is on.
pub(super) fn line_start(rows: &[Line], mut index: usize) -> usize {
    while continues(rows, index) {
        index -= 1;
    }
    index
}

/// The last row of the diff line starting at `index`.
pub(super) fn line_end(rows: &[Line], mut index: usize) -> usize {
    while continues(rows, index + 1) {
        index += 1;
    }
    index
}

/// A grapheme's cells at `column` of its row: a tab runs to the next stop of four, as
/// `paint` draws it.
fn cells(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" { 4 - column % 4 } else { grapheme.width() }
}

/// The cells of a line's leading whitespace: its continuation rows hang under its code.
fn indentation(text: &str) -> usize {
    let mut column = 0;
    for grapheme in text.graphemes(true).take_while(|g| g.chars().all(char::is_whitespace)) {
        column += cells(grapheme, column);
    }
    column
}

/// Where a code line breaks into rows of `first` cells, then `rest`: after the last space
/// that fits or beside a wide (CJK) character, else where the room runs out. Every byte
/// lands in exactly one part, so the parts join back into the line.
fn breaks(text: &str, first: usize, rest: usize) -> Vec<std::ops::Range<usize>> {
    let graphemes = text.grapheme_indices(true).collect::<Vec<_>>();
    let mut parts = Vec::new();
    let (mut start, mut index) = (0, 0);
    while index < graphemes.len() {
        let room = (if parts.is_empty() { first } else { rest }).max(1);
        let (mut column, mut cut, mut wide_before, mut text_seen) = (0, None, false, false);
        let mut end = graphemes.len();
        let mut at = index;
        while at < graphemes.len() {
            let (_, grapheme) = graphemes[at];
            let size = cells(grapheme, column);
            let wide = grapheme != "\t" && size >= 2;
            let space = grapheme.chars().all(char::is_whitespace);
            if at > index && (wide || wide_before || space && text_seen) {
                cut = Some(at);
            }
            if column + size > room && at > index {
                end = cut.unwrap_or(at);
                break;
            }
            column += size;
            wide_before = wide;
            text_seen |= !space;
            if space && text_seen && at + 1 < graphemes.len() {
                // After a space: the next word starts the next row.
                cut = Some(at + 1);
            }
            at += 1;
        }
        let byte_end = graphemes.get(end).map_or(text.len(), |(byte, _)| *byte);
        parts.push(start..byte_end);
        start = byte_end;
        index = end;
    }
    if parts.is_empty() {
        parts.push(0..text.len());
    }
    parts
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
            // A suggested change keeps its own − and + bands (§3.30a).
            row.band = Some(row.band.filter(|band| matches!(band, GitSurface::AddedBand | GitSurface::RemovedBand)).unwrap_or(GitSurface::Code));
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

/// `◔ Unresolved · alex · line 14`: the state in its colour, the author bold.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(text: &str, parts: &[std::ops::Range<usize>]) -> String {
        parts.iter().map(|part| &text[part.clone()]).collect()
    }

    #[test]
    fn a_long_line_breaks_after_spaces_and_beside_wide_characters() {
        let text = "let total = first_value + second_value;";
        let parts = breaks(text, 16, 12);
        assert_eq!(joined(text, &parts), text);
        assert_eq!(&text[parts[0].clone()], "let total = ");
        for part in &parts[1..] {
            assert!(text[part.clone()].width() <= 12, "{:?}", &text[part.clone()]);
        }
        // CJK breaks between characters; a word after them moves whole.
        let wide = format!("{}needle end", "界".repeat(7));
        let parts = breaks(&wide, 16, 16);
        assert_eq!(joined(&wide, &parts), wide);
        assert_eq!(&wide[parts[1].clone()], "needle end");
        // No break point: the room decides; tabs take their stop.
        assert_eq!(breaks("abcdefgh", 3, 3), vec![0..3, 3..6, 6..8]);
        assert_eq!(breaks("\tab", 5, 5), vec![0..2, 2..3]);
        assert_eq!(breaks("", 4, 4), vec![0..0]);
    }

    #[test]
    fn wrapped_rows_keep_the_line_band_spans_action_and_find_source() {
        let long = format!("    let value = {};", "item + ".repeat(12));
        let diff = format!(
            "diff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1,2 +1,2 @@\n context\n-old\n+{long}\n"
        );
        let preview = Preview::git(&diff, None);
        let path = Path::new("lib.rs");
        let mut wrapped = Vec::new();
        unified(&preview, path, 9, "abc", &mut wrapped, Some(40), "");
        let mut panned = Vec::new();
        unified(&preview, path, 9, "abc", &mut panned, None, "");
        assert_eq!(panned.iter().filter(|row| row.diff == Some(true)).count(), 1);
        assert!(wrapped.len() > panned.len());
        let line = wrapped.iter().filter(|row| row.diff == Some(true)).collect::<Vec<_>>();
        assert!(line.len() > 1);
        let gutter = line[0].gutter.clone();
        assert!(gutter.contains('2') && gutter.contains('+'), "{gutter:?}");
        let mut text = String::new();
        for (part, row) in line.iter().enumerate() {
            // One source line: the same comment target, band and logical find source.
            assert_eq!(row.action, line[0].action);
            assert!(matches!(&row.action, Some(Action::Inline { line: 2, side: "RIGHT", .. })));
            assert!(!row.pan);
            let source = row.source.as_ref().unwrap();
            assert_eq!(&*source.text, long.as_str());
            assert_eq!(source.offset, text.len());
            text.push_str(&row.text);
            if part > 0 {
                // No line number: a blank gutter, hung under the line's own indentation.
                assert_eq!(row.gutter, " ".repeat(gutter.width() + 4));
            }
            assert!(row.gutter.width() + row.text.width() <= 40, "{:?}", row.text);
            for (span, _) in &row.spans {
                assert!(span.end <= row.text.len());
            }
        }
        assert_eq!(text, long);
        assert!(panned.iter().filter(|row| row.diff == Some(true)).all(|row| row.pan && row.text == long));
        // The wrapped line is one cursor stop: its first row starts it, the rest continue it.
        let first = wrapped.iter().position(|row| row.diff == Some(true)).unwrap();
        let last = first + line.len() - 1;
        assert!(!continues(&wrapped, first) && (first + 1..=last).all(|index| continues(&wrapped, index)));
        assert_eq!((line_start(&wrapped, last), line_end(&wrapped, first)), (first, last));
        // The lines around it are stops of their own.
        assert_eq!(line_end(&wrapped, first - 1), first - 1);
        assert!(!continues(&wrapped, first - 1) && !continues(&wrapped, last + 1));
    }

    #[test]
    fn diff_stats_count_the_hunks() {
        let diff = "diff --git a/orbit/queue.py b/orbit/queue.py\n--- a/orbit/queue.py\n+++ b/orbit/queue.py\n@@ -1,3 +1,5 @@\n ctx\n-old\n+new\n+more\n+most\n--- not a header\ndiff --git a/t.py b/t.py\n--- a/t.py\n+++ b/t.py\n@@ -1 +1 @@\n+x\n";
        let stats = super::hunk_stats(diff);
        assert_eq!(stats["orbit/queue.py"], (3, 2));
        assert_eq!(stats["t.py"], (1, 0));
    }
}
