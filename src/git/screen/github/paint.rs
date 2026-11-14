//! The forge tabs (DESIGN.md §3.26–§3.36): Pull requests and Issues on the shell chrome.
//! A list of two-row records beside a reader with a pinned header (title, state chips,
//! tabs) over Markdown, metadata and timeline rows. Actions paints in `runs`.
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::{self, Mode, Rect};
use super::super::{FormAction, GitScreen};
use super::prose::highlight_style;
use super::lines::Line;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::{CiState, Lifecycle, ReviewState};
use crate::forge::records::{self, DetailRecord, ItemRecord};
use crate::ui::{CellGrid, Emphasis, Frame, GitChip, GitRole, GitSurface, Style};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PANE: GitSurface = GitSurface::Pane;

/// A style moved onto another background; chips and label chips keep their own.
fn on(style: Style, surface: GitSurface) -> Style {
    match style {
        Style::Git {
            role,
            surface: own,
            emphasis,
        } => Style::Git {
            role,
            surface: if matches!(own, GitSurface::Pane) { surface } else { own },
            emphasis,
        },
        Style::GitSyntax { group, surface: own } => Style::GitSyntax {
            group,
            surface: if matches!(own, GitSurface::Pane) { surface } else { own },
        },
        other => other,
    }
}

/// The legacy row styles still used by the Files rows, as Git roles.
fn legacy(style: Style, surface: GitSurface) -> Style {
    let git = |role, emphasis| Style::Git {
        role,
        surface,
        emphasis,
    };
    match style {
        Style::Git { .. } | Style::GitSyntax { .. } => on(style, surface),
        Style::Muted => git(GitRole::Muted, Emphasis::None),
        Style::Warning => git(GitRole::Modified, Emphasis::None),
        Style::Error => git(GitRole::Removed, Emphasis::None),
        Style::GitHeading => git(GitRole::Text, Emphasis::Bold),
        Style::GitSignAdded => git(GitRole::Added, Emphasis::None),
        Style::GitControl => git(GitRole::Focus, Emphasis::None),
        _ => git(GitRole::Text, Emphasis::None),
    }
}

fn lifecycle_mark(lifecycle: Lifecycle, pull: bool, mode: ExplorerIconMode) -> (&'static str, GitRole) {
    match (lifecycle, pull) {
        (Lifecycle::Open, true) => (glyph(Glyph::PullRequest, mode), GitRole::Added),
        (Lifecycle::Draft, _) => (glyph(Glyph::PullRequest, mode), GitRole::Muted),
        (Lifecycle::Merged, _) => (glyph(Glyph::PullMerged, mode), GitRole::Ref),
        (Lifecycle::Closed, true) => (glyph(Glyph::PullRequest, mode), GitRole::Removed),
        (Lifecycle::Open, false) => (glyph(Glyph::Issue, mode), GitRole::Added),
        (Lifecycle::Closed, false) => (glyph(Glyph::IssueClosed, mode), GitRole::Ref),
        (Lifecycle::ClosedNotPlanned, _) => (glyph(Glyph::NotPlanned, mode), GitRole::Muted),
    }
}

/// The reader's filled state chip: `● Open`, `◌ Draft`, `󰘭 Merged`, `✕ Closed`.
fn state_chip(lifecycle: Lifecycle, pull: bool, mode: ExplorerIconMode) -> (String, GitChip, GitRole) {
    match (lifecycle, pull) {
        (Lifecycle::Open, _) => (format!("{} Open", glyph(Glyph::Dot, mode)), GitChip::Added, GitRole::Added),
        (Lifecycle::Draft, _) => (format!("{} Draft", glyph(Glyph::ReviewRequested, mode)), GitChip::Muted, GitRole::Muted),
        (Lifecycle::Merged, _) => (format!("{} Merged", glyph(Glyph::PullMerged, mode)), GitChip::Ref, GitRole::Ref),
        (Lifecycle::Closed, true) => (format!("{} Closed", glyph(Glyph::CheckFailed, mode)), GitChip::Removed, GitRole::Removed),
        (Lifecycle::Closed, false) => (format!("{} Closed", glyph(Glyph::IssueClosed, mode)), GitChip::Ref, GitRole::Ref),
        (Lifecycle::ClosedNotPlanned, _) => (format!("{} Not planned", glyph(Glyph::NotPlanned, mode)), GitChip::Muted, GitRole::Muted),
    }
}

fn label_spans(labels: &[records::LabelChip], room: usize, surface: GitSurface) -> Vec<Span> {
    // Up to two chips, then `+n more`, inside `room` cells.
    let mut spans = Vec::new();
    let mut used = 0;
    for (index, label) in labels.iter().enumerate() {
        let text = format!(" {} ", label.name);
        let rest = labels.len() - index - 1;
        let more = if rest > 0 { format!(" +{rest} more") } else { String::new() };
        if index >= 2 || used + text.width() + 1 + more.width() > room {
            let left = labels.len() - index;
            let tail = format!("+{left} more");
            if used + tail.width() + 1 <= room {
                spans.push((" ".into(), st(GitRole::Text, surface)));
                spans.push((tail, st(GitRole::Muted, surface)));
            }
            break;
        }
        if index > 0 {
            spans.push((" ".into(), st(GitRole::Text, surface)));
            used += 1;
        }
        used += text.width();
        spans.push((text, super::prose::label_style(label)));
    }
    spans
}

fn put(grid: &mut CellGrid, x: u16, y: u16, spans: &[Span], right: u16) -> u16 {
    chrome::put_spans(grid, x, y, spans, right)
}

struct Geometry {
    list: Option<Rect>,
    reader: Option<Rect>,
    footer: Rect,
    mode: Mode,
}

fn geometry(workspace: &Workspace, width: u16, height: u16) -> Geometry {
    let mode = layout::mode(width, height);
    let body = Rect::new(0, 1, width, height.saturating_sub(2));
    let footer = Rect::new(0, height - 1, width, 1);
    let files = workspace.page == 2 && workspace.pr_tab == pr::FILES && workspace.focus == 2;
    if mode == Mode::Narrow {
        return if workspace.focus == 2 {
            Geometry { list: None, reader: Some(body), footer, mode }
        } else {
            Geometry { list: Some(body), reader: None, footer, mode }
        };
    }
    if files {
        // Entering Files hides the list (§3.28).
        return Geometry { list: None, reader: Some(body), footer, mode };
    }
    let percent = if mode == Mode::Wide { 44 } else { 40 };
    let left = (width * percent / 100).max(40);
    Geometry {
        list: Some(Rect::new(0, 1, left, body.h)),
        reader: Some(Rect::new(left, 1, width - left, body.h)),
        footer,
        mode,
    }
}

pub(in crate::git::screen) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if layout::mode(width, height) == Mode::TooSmall {
        let text = format!("Dusty Git needs {}×{}", layout::MIN_WIDTH, layout::MIN_HEIGHT);
        frame.grid.put_text_clipped(
            width.saturating_sub(chrome::width(&text)) / 2,
            height / 2,
            &text,
            bold(GitRole::Modified, GitSurface::Canvas),
            width,
        );
        return frame;
    }
    chrome::fill(&mut frame.grid, Rect::new(0, 0, width, height), st(GitRole::Text, GitSurface::Canvas));
    super::super::shell::top_bar(app, &mut frame.grid, width);
    let filter_form = app
        .form
        .as_ref()
        .is_some_and(|form| matches!(&form.action, FormAction::Github(Action::Filter)));
    // §7.4: the filter form's text drives a local search while the remote one runs.
    app.github.live_input(app.form.as_ref().filter(|_| filter_form).map(|form| {
        (
            form.fields.first().map(|field| field.input.input.trim().to_owned()).unwrap_or_default(),
            form.fields.get(1).map(|field| field.input.input.clone()).unwrap_or_default(),
        )
    }));
    let mode_icons = app.icon_mode;
    let geometry = geometry(&app.github, width, height);
    let reply = app.form.as_ref().filter(|form| {
        matches!(
            &form.action,
            FormAction::Github(Action::Comment(..) | Action::Reply { .. })
        ) && geometry.reader.is_some()
    });
    let reply_rows = reply.map_or(0, |_| 9);
    if let Some(rect) = geometry.list {
        let filter = app.form.as_ref().filter(|_| filter_form);
        list(&mut frame, &mut app.github, rect, filter, mode_icons, geometry.mode);
    }
    if let Some(rect) = geometry.reader {
        reader(&mut frame, &mut app.github, rect, reply_rows, mode_icons);
        if let Some(form) = reply {
            reply_box(&mut frame, &app.github, form, rect, reply_rows);
        }
    }
    // Review and Merge take the reader's place (the whole body when narrow).
    let owned = app.form.as_ref().filter(|form| super::forms::owns(form));
    if let Some(form) = owned {
        let rect = geometry.reader.unwrap_or(Rect::new(0, 1, width, height.saturating_sub(2)));
        super::forms::paint(&mut frame, &app.github, form, rect, mode_icons, &app.notice);
    }
    footer(app, &mut frame, geometry.footer, filter_form);
    super::accounts::paint(&mut frame, &app.github, 0, width, height);
    if let Some(form) = &app.form {
        let inline = filter_form && geometry.list.is_some() || reply.is_some() || owned.is_some();
        if !inline {
            let lay = layout::layout(width, height, false, [false; 4]);
            if !super::super::shell::sheet::paint(app, &mut frame, &lay) {
                super::super::render::render_form(&mut frame, form, width, height);
            }
        }
    }
    frame
}

/// One list row: a record, the live-search divider, or a cached row gone from the forge.
enum Entry<'a> {
    Record(&'a Value),
    Divider(String),
}

fn list(
    frame: &mut Frame,
    workspace: &mut Workspace,
    rect: Rect,
    filter: Option<&super::super::Form>,
    mode: ExplorerIconMode,
    layout_mode: Mode,
) {
    let focused = workspace.focus < 2;
    let pull = workspace.page == 2;
    let terms = workspace.terms();
    let kind = if pull { terms.pr_plural } else { "Issues" };
    let title: Vec<Span> = vec![
        (
            format!("{} ", glyph(if pull { Glyph::PullRequest } else { Glyph::Issue }, mode)),
            st(GitRole::Focus, PANE),
        ),
        (
            if workspace.inbox.enabled {
                format!("{kind} · Inbox · {}", workspace.host)
            } else {
                kind.to_owned()
            },
            bold(GitRole::Text, PANE),
        ),
    ];
    let chip = workspace
        .freshness_chip(&workspace.list_key(), rect.w < 52)
        .unwrap_or_default();
    chrome::pane(&mut frame.grid, rect, &title, &chip, focused);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let mut y = inner.y;
    // Sections row.
    let sections = workspace.sections();
    let active = workspace.active_section(&sections);
    // While a typed query differs from the section's, no section claims the list (§7.4).
    let typed = filter
        .and_then(|form| form.fields.first())
        .map(|field| field.input.input.trim().to_owned());
    let highlighted = active.filter(|_| {
        typed
            .as_deref()
            .is_none_or(|typed| typed == workspace.queries[workspace.page.saturating_sub(1).min(1)].trim())
    });
    let tabs = sections
        .iter()
        .map(|section| match workspace.section_count(section) {
            Some(count) => format!("{} {count}", section.name),
            None => section.name.clone(),
        })
        .collect::<Vec<_>>();
    // A custom query owns a badge at the row's end; the tabs fit in what is left and
    // never run under it. Built-in sections shorten, then fold into `+N more`; saved views
    // scroll as before.
    let custom = highlighted.is_none().then_some(" Custom filter ");
    let badge = custom.map_or(0, |custom| chrome::width(custom) + 1);
    let tabs_right = right.saturating_sub(badge);
    let room = tabs_right.saturating_sub(x);
    let tabs = if active.is_none_or(|active| active < 4) {
        chrome::fit_tabs(&tabs, highlighted.unwrap_or(0), room)
    } else {
        tabs
    };
    // Keep the active section in view when the row overflows.
    let first = highlighted
        .filter(|&active| active < tabs.len())
        .map(|active| {
            let mut first = 0;
            while first < active
                && tabs[first..=active].iter().map(|tab| tab.width() + 3).sum::<usize>() > usize::from(room)
            {
                first += 1;
            }
            first
        })
        .unwrap_or(0);
    let shown = tabs[first..].to_vec();
    let mut at = x;
    if first > 0 {
        at = frame.grid.put_text_clipped(at, y, "… ", st(GitRole::Muted, PANE), tabs_right);
    }
    chrome::sub_tabs(
        &mut frame.grid,
        at,
        y,
        tabs_right,
        &shown,
        highlighted.map_or(usize::MAX, |active| active.saturating_sub(first)),
        focused,
    );
    if let Some(custom) = custom {
        let w = chrome::width(custom);
        if right > x + w {
            frame.grid.put_text_clipped(right - w, y, custom, st(GitRole::Modified, GitSurface::Chip(GitChip::Modified)), right);
        }
    }
    y += 1;
    for column in inner.x..inner.right() {
        frame.grid.set_cell(column, y, "─", st(GitRole::Border, PANE));
    }
    y += 1;
    // The query row: the live filter while typing, else a query beyond the section's.
    let at = workspace.page - 1;
    let query = workspace.queries[at].clone();
    if let Some(form) = filter {
        let mark = format!("{} ", glyph(Glyph::Search, mode));
        let after = frame.grid.put_text_clipped(x, y, &mark, bold(GitRole::Focus, PANE), right);
        if let Some(field) = form.fields.first() {
            super::super::shell::sheet::input(frame, field, Rect::new(after + 1, y, right.saturating_sub(after + 1), 1), true);
        }
        y += 1;
    } else if active.is_none() && (!query.is_empty() || workspace.states[at] != "open") {
        // A custom filter names its state too when it is not the default.
        let state = workspace.states[at].as_str();
        let mut spans: Vec<Span> = vec![(format!("{} ", glyph(Glyph::Search, mode)), st(GitRole::Focus, PANE))];
        if state != "open" {
            let (chip, role) = match state {
                "closed" => (GitChip::Removed, GitRole::Removed),
                "merged" => (GitChip::Ref, GitRole::Ref),
                _ => (GitChip::Muted, GitRole::Muted),
            };
            spans.push((format!(" {state} "), st(role, GitSurface::Chip(chip))));
            spans.push((" ".into(), st(GitRole::Text, PANE)));
        }
        let used = chrome::spans_width(&spans);
        spans.push((chrome::ellipsize(&query, usize::from(right.saturating_sub(x + used))), st(GitRole::Text, PANE)));
        put(&mut frame.grid, x, y, &spans, right);
        y += 1;
    }
    if let Some((text, tone)) = workspace.host_notice(workspace.cache.values().any(Result::is_ok)) {
        let (surface, role) = match tone {
            crate::forge::host_state::Tone::Error => (GitSurface::ReceiptErr, GitRole::Removed),
            crate::forge::host_state::Tone::Warning => (GitSurface::Warning, GitRole::Modified),
            _ => (GitSurface::FileBand, GitRole::Muted),
        };
        chrome::fill(&mut frame.grid, Rect::new(inner.x, y, inner.w, 1), st(GitRole::Text, surface));
        frame.grid.put_text_clipped(x, y, &chrome::ellipsize(&text, usize::from(right - x)), st(role, surface), right);
        y += 1;
    }
    // Records.
    let live = workspace.live_rows();
    let flash = std::mem::take(&mut workspace.fresh.flash);
    let mut entries = Vec::new();
    let rows_owned: Vec<Value>;
    match &live {
        Some(live) => {
            entries.extend(live.local.iter().map(Entry::Record));
            entries.push(Entry::Divider(live.divider.clone()));
            entries.extend(live.remote.iter().map(Entry::Record));
        }
        None => {
            rows_owned = workspace.items().into_iter().cloned().collect();
            entries.extend(rows_owned.iter().map(Entry::Record));
            // `rows_owned` lives to the end of this function.
            let _ = &rows_owned;
        }
    }
    let selected = if live.is_some() {
        usize::MAX
    } else {
        workspace.selected[at]
    };
    if live.is_some() {
        let matching = entries.iter().filter(|entry| matches!(entry, Entry::Record(_))).count();
        let note = format!("{matching} matching {}", if pull { "items" } else { "issues" });
        frame.grid.put_text_clipped(x, y, &note, st(GitRole::Muted, PANE), right);
        y += 1;
    }
    let bottom = inner.bottom().saturating_sub(1);
    // §S47: a narrow layout reads one row per record; a short or slim pane keeps the meta
    // row (author and state stay words, gh-dash's narrow rows do too) but drops the gap.
    let compact = layout_mode == Mode::Narrow || rect.h < 16;
    let record_rows: u16 = if compact {
        1
    } else if rect.h <= 30 || rect.w < 60 {
        2
    } else {
        3
    };
    if entries.is_empty() {
        let key = workspace.list_key();
        let (headline, hint) = match workspace.cache.get(&key) {
            _ if workspace.sign_in_hints().is_some() => ("Sign-in required", "a accounts · the reader says how"),
            Some(Err(_)) => ("This view is unavailable", "R retry · the reader says why"),
            None => ("Loading…", ""),
            Some(Ok(_)) if workspace.pending(&key) => ("Refreshing…", ""),
            Some(Ok(_)) if workspace.inbox.enabled => ("No indexed matches", "[ ] sections · / filter · R reload"),
            Some(Ok(_)) => ("Nothing in this section", "[ ] sections · / filter · n create"),
        };
        let y = y + 1;
        frame.grid.put_text_clipped(x + 1, y, headline, bold(GitRole::Text, PANE), right);
        if !hint.is_empty() {
            frame.grid.put_text_clipped(x + 1, y + 1, hint, st(GitRole::Muted, PANE), right);
        }
        if focused {
            frame.cursor = Some((x + 1, y, SetCursorStyle::SteadyBlock));
        }
        return;
    }
    let room = bottom.saturating_sub(y);
    let visible = usize::from(room / record_rows).max(1);
    let cursor = if selected >= entries.len() { 0 } else { selected };
    let offset = cursor.saturating_sub(visible.saturating_sub(1));
    let now = records::now();
    for (index, entry) in entries.iter().enumerate().skip(offset).take(visible) {
        let top = y + (index - offset) as u16 * record_rows;
        if top >= bottom {
            break;
        }
        let value = match entry {
            Entry::Divider(divider) => {
                let text = format!(" {divider} ");
                let w = chrome::width(&text);
                for column in x..right {
                    frame.grid.set_cell(column, top, "─", st(GitRole::Border, PANE));
                }
                frame.grid.put_text_clipped(x + 2, top, &text, st(GitRole::Muted, PANE), right);
                let _ = w;
                continue;
            }
            Entry::Record(value) => *value,
        };
        let is_selected = index == selected;
        let surface = if is_selected {
            if focused { GitSurface::Cursor } else { GitSurface::CursorInactive }
        } else {
            PANE
        };
        let rows = if compact { 1 } else { 2 };
        if is_selected {
            chrome::fill(&mut frame.grid, Rect::new(inner.x, top, inner.w, rows.min(bottom - top)), st(GitRole::Text, surface));
            frame.grid.put_text_clipped(x, top, "▸", bold(GitRole::Focus, surface), right);
            if focused {
                frame.cursor = Some((inner.x, top, SetCursorStyle::SteadyBar));
            }
        }
        let record = ItemRecord::from_row(value);
        let gone = value["dustyGone"].as_str();
        let flashed = flash.contains(&workspace.row_identity(value));
        let (mark, tone) = if gone.is_some() {
            ("✕", GitRole::Muted)
        } else {
            lifecycle_mark(record.lifecycle, pull, mode)
        };
        // Row 1: glyph, number, title, a state chip when not open.
        let mut spans: Vec<Span> = vec![(format!("{mark} "), st(tone, surface))];
        if workspace.inbox.enabled
            && let Some(repo) = &record.repo
        {
            spans.push((format!("{} ", chrome::ellipsize(repo, 18)), st(GitRole::Muted, surface)));
        }
        spans.push((format!("{}{}", workspace.sigil(pull), record.number), st(GitRole::Muted, surface)));
        spans.push((" ".into(), st(GitRole::Text, surface)));
        let chip = match (record.lifecycle, gone) {
            (_, Some(_)) => None,
            (Lifecycle::Draft, _) => Some(("Draft", GitChip::Muted, GitRole::Muted)),
            (Lifecycle::Merged, _) => Some(("Merged", GitChip::Ref, GitRole::Ref)),
            (Lifecycle::Closed, _) if pull => Some(("Closed", GitChip::Removed, GitRole::Removed)),
            (Lifecycle::Closed, _) => Some(("Closed", GitChip::Ref, GitRole::Ref)),
            (Lifecycle::ClosedNotPlanned, _) => Some(("Not planned", GitChip::Muted, GitRole::Muted)),
            (Lifecycle::Open, _) => None,
        };
        let chip_width = chip.map_or(0, |(text, ..)| chrome::width(text) + 3);
        let x1 = x + 2;
        let before = chrome::spans_width(&spans);
        let mut tail: Vec<Span> = Vec::new();
        if compact && gone.is_none() {
            // One row: review and CI marks and ± after the title.
            if let Some(review) = record.review {
                let (mark, _, role) = pr::review_mark(review, mode);
                tail.push((format!(" {mark}"), st(role, surface)));
            }
            if let Some(ci) = record.ci() {
                let (mark, _, role) = pr::ci_mark(ci, mode);
                tail.push((format!(" {mark}"), st(role, surface)));
            }
            if let Some(add) = record.additions {
                tail.push((format!(" +{add}"), st(GitRole::Added, surface)));
            }
            if !pull && record.comments > 0 {
                tail.push((format!(" {} {}", glyph(Glyph::Comment, mode), record.comments), st(GitRole::Muted, surface)));
            }
        }
        let tail_width = chrome::spans_width(&tail);
        let title_room = usize::from(right.saturating_sub(x1 + before + chip_width + tail_width + 1));
        spans.push((
            chrome::ellipsize(&prose::title(&record.title, record.lifecycle == Lifecycle::Draft), title_room),
            Style::Git {
                role: if gone.is_some() {
                    GitRole::Muted
                } else if flashed {
                    GitRole::Modified
                } else {
                    GitRole::Text
                },
                surface,
                emphasis: Emphasis::Bold,
            },
        ));
        put(&mut frame.grid, x1, top, &spans, right);
        if !tail.is_empty() {
            put(&mut frame.grid, right.saturating_sub(tail_width + chip_width), top, &tail, right);
        }
        if let Some((text, hue, role)) = chip {
            let at = right.saturating_sub(chip_width - 1);
            frame.grid.put_text_clipped(at, top, &format!(" {text} "), st(role, GitSurface::Chip(hue)), right);
        }
        if compact || top + 1 >= bottom {
            continue;
        }
        // Row 2: author · age · review · CI · ± · comments, then label chips.
        let meta_x = x1 + 2;
        // Each fact is whole or absent. A narrow row first takes the short words
        // (`◌ Requested`), then drops the least telling facts (± first, then comments, age,
        // CI) instead of clipping one mid-word; author and review state stay longest.
        let mut facts: Vec<(u8, Vec<Span>, Option<Vec<Span>>)> = Vec::new();
        if let Some(gone) = gone {
            facts.push((0, vec![(format!("{gone} · gone from this view on refresh"), st(GitRole::Muted, surface))], None));
        } else {
            facts.push((0, vec![(record.author.clone(), st(GitRole::Author, surface))], None));
            let when = records::ago(&record.updated, now);
            if !when.is_empty() {
                facts.push((3, vec![(when, st(GitRole::Muted, surface))], None));
            }
            if pull {
                // No review state is no fact: "Not requested" says nothing, and forges
                // without review requests would show it on every row.
                if let Some(review) = record.review.filter(|review| *review != ReviewState::NotRequested) {
                    let (mark, word, role) = pr::review_mark(review, mode);
                    let (word, requested) = match review {
                        ReviewState::Requested => workspace.requested_words(),
                        _ => (word, "Requested"),
                    };
                    let short = match review {
                        ReviewState::Requested => requested,
                        ReviewState::ChangesRequested => "Changes",
                        _ => word,
                    };
                    facts.push((
                        1,
                        vec![(format!("{mark} {word}"), st(role, surface))],
                        Some(vec![(format!("{mark} {short}"), st(role, surface))]),
                    ));
                }
                if let Some(ci) = record.ci() {
                    let (mark, word, role) = pr::ci_mark(ci, mode);
                    facts.push((2, vec![(format!("{mark} {word}"), st(role, surface))], None));
                }
                if let (Some(add), Some(del)) = (record.additions, record.deletions) {
                    facts.push((5, vec![
                        (format!("+{add}"), st(GitRole::Added, surface)),
                        (" ".into(), st(GitRole::Text, surface)),
                        (format!("−{del}"), st(GitRole::Removed, surface)),
                    ], None));
                }
            }
            if record.comments > 0 {
                facts.push((4, vec![(
                    format!(
                        "{} {}{}",
                        glyph(Glyph::Comment, mode),
                        record.comments,
                        if record.comments_capped { "+" } else { "" }
                    ),
                    st(GitRole::Muted, surface),
                )], None));
            }
        }
        let room = usize::from(right.saturating_sub(meta_x));
        let joined = |facts: &[(u8, Vec<Span>, Option<Vec<Span>>)]| {
            let mut meta: Vec<Span> = Vec::new();
            for (index, (_, spans, _)) in facts.iter().enumerate() {
                if index > 0 {
                    meta.push((" · ".into(), st(GitRole::Muted, surface)));
                }
                meta.extend(spans.iter().cloned());
            }
            meta
        };
        let mut meta = joined(&facts);
        if usize::from(chrome::spans_width(&meta)) > room {
            for fact in &mut facts {
                if let Some(short) = fact.2.take() {
                    fact.1 = short;
                }
            }
            meta = joined(&facts);
        }
        while facts.len() > 1 && usize::from(chrome::spans_width(&meta)) > room {
            let drop = facts
                .iter()
                .enumerate()
                .max_by_key(|(_, (rank, _, _))| *rank)
                .map(|(index, _)| index)
                .unwrap_or(facts.len() - 1);
            facts.remove(drop);
            meta = joined(&facts);
        }
        let meta_width = chrome::spans_width(&meta);
        let after = put(&mut frame.grid, meta_x, top + 1, &meta, right);
        if gone.is_none() && !record.labels.is_empty() {
            let room = usize::from(right.saturating_sub(after + 2));
            let labels = label_spans(&prose::chips(&record.labels, record.lifecycle == Lifecycle::Draft), room, surface);
            let w = chrome::spans_width(&labels);
            // Pull requests right-align their labels (a column); issues follow the meta.
            let at = if pull { right.saturating_sub(w) } else { after + 2 };
            if at >= meta_x + meta_width + 1 {
                put(&mut frame.grid, at, top + 1, &labels, right);
            }
        }
        let _ = CiState::Passed;
    }
    // The last row: how much of the view is here.
    let count = entries.iter().filter(|entry| matches!(entry, Entry::Record(_))).count();
    let mut footer: Vec<Span> = vec![(
        if workspace.value(&workspace.list_key()).is_some() {
            format!("Showing {count}{}", if workspace.inbox.enabled { " indexed" } else { "" })
        } else {
            "…".into()
        },
        st(GitRole::Muted, PANE),
    )];
    if workspace.loading_more() {
        footer.push((" · loading more…".into(), st(GitRole::Muted, PANE)));
    } else if workspace.list_may_have_more() {
        if workspace.list_limit() >= LIST_CEILING {
            // The forge stops at 1000 results; a narrower filter is the way on.
            footer.push((" · limit · / refine".into(), st(GitRole::Modified, PANE)));
        } else {
            footer.push((" · ".into(), st(GitRole::Muted, PANE)));
            footer.push(("^N".into(), bold(GitRole::Focus, PANE)));
            footer.push((" more".into(), st(GitRole::Muted, PANE)));
        }
    }
    if entries.len() > visible {
        footer.push((format!(" · {}–{}", offset + 1, (offset + visible).min(entries.len())), st(GitRole::Muted, PANE)));
    }
    if !workspace.inbox.enabled && !workspace.repo.is_empty() {
        // The forge repository the list reads, which may differ from the folder's name;
        // right-aligned when there is room.
        let used: u16 = footer.iter().map(|(text, _)| chrome::width(text)).sum();
        let repo = chrome::width(&workspace.repo);
        if x + used + repo + 3 <= right {
            frame.grid.put_text_clipped(right - repo, bottom, &workspace.repo, st(GitRole::Muted, PANE), right);
        }
    }
    put(&mut frame.grid, x, bottom, &footer, right);
}

/// Tabs of the reader, with counts.
fn reader_tabs(workspace: &Workspace, record: Option<&DetailRecord>) -> (Vec<String>, usize) {
    let id = workspace.number();
    if workspace.page == 1 {
        let discussion = record.map(|record| issue::discussion_count(workspace, id, record)).unwrap_or(0);
        let activity = issue::activity_count(workspace, id);
        let tabs = vec![
            issue::TABS[0].to_owned(),
            // Counted once known, 0 included; the list's comment count stands in meanwhile.
            match record {
                Some(_) => format!("{} {discussion}", issue::TABS[1]),
                None => issue::TABS[1].to_owned(),
            },
            match activity {
                Some(activity) => format!("{} {activity}", issue::TABS[2]),
                None => issue::TABS[2].to_owned(),
            },
        ];
        return (tabs, workspace.issue_tab.min(2));
    }
    let counts = record.map(|record| pr::counts(workspace, id, record)).unwrap_or_default();
    let names = ["Overview", "Commits", workspace.ci_words().0, "Files", "Activity"];
    let mut tabs = names
        .iter()
        .zip(counts)
        .map(|(name, count)| match count {
            Some(count) => format!("{name} {count}"),
            None => (*name).to_owned(),
        })
        .collect::<Vec<_>>();
    if workspace.pr_tab == pr::MERGE {
        tabs.push("Merge".into());
    }
    (tabs, workspace.pr_tab)
}

fn reader(frame: &mut Frame, workspace: &mut Workspace, rect: Rect, reply_rows: u16, mode: ExplorerIconMode) {
    let focused = workspace.focus == 2;
    let pull = workspace.page == 2;
    let id = workspace.number();
    let value = workspace.item().or_else(|| workspace.list_item()).cloned();
    let record = value.as_ref().map(DetailRecord::from_value);
    let kind = if pull { workspace.terms().pr_singular } else { "issue" };
    let mut kind = kind.to_owned();
    if let Some(first) = kind.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let title: Vec<Span> = vec![(kind, bold(GitRole::Text, PANE))];
    chrome::pane(&mut frame.grid, rect, &title, "", focused);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let mut y = inner.y;
    let width = usize::from(right.saturating_sub(x));
    if let Some(record) = &record {
        // Title: glyph, number, bold title.
        let (mark, tone) = lifecycle_mark(record.row.lifecycle, pull, mode);
        let prefix = format!("{mark} {}{} ", workspace.sigil(pull), record.row.number);
        let indent = prefix.width();
        let lines = crate::ui::render::wrap_plain_text(
            &prose::title(&record.row.title, record.row.lifecycle == Lifecycle::Draft),
            width.saturating_sub(indent).max(10),
            crate::config::AmbiguousWidth::Narrow,
        );
        // One row on a wide reader, two on a narrow one (where a title needs them to be
        // readable), so the state chip and the tabs move at most once; the rest is elided.
        let most = if width < 100 { 2 } else { 1 };
        let lines = if lines.len() > most {
            let mut kept = lines[..most - 1].to_vec();
            kept.push(chrome::ellipsize(&lines[most - 1..].join(" "), width.saturating_sub(indent)));
            kept
        } else {
            lines
        };
        for (index, line) in lines.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if index == 0 {
                spans.push((format!("{mark} "), bold(tone, PANE)));
                spans.push((format!("{}{} ", workspace.sigil(pull), record.row.number), st(GitRole::Muted, PANE)));
            } else {
                spans.push((" ".repeat(indent), st(GitRole::Text, PANE)));
            }
            spans.push((line.clone(), bold(GitRole::Text, PANE)));
            put(&mut frame.grid, x, y, &spans, right);
            y += 1;
        }
        // State chips and facts.
        let (text, hue, role) = state_chip(record.row.lifecycle, pull, mode);
        let mut at = frame.grid.put_text_clipped(x, y, &format!(" {text} "), bold(role, GitSurface::Chip(hue)), right);
        let now = records::now();
        // Facts in groups; on a narrow reader whole groups go (assignees, then the author),
        // never half a word.
        let mut groups: Vec<(u8, Vec<Span>)> = Vec::new();
        if pull && !record.base.is_empty() {
            groups.push((
                0,
                vec![
                    (record.base.clone(), st(GitRole::Muted, PANE)),
                    (" ← ".into(), st(GitRole::Muted, PANE)),
                    (record.head.clone(), st(GitRole::Ref, PANE)),
                ],
            ));
        }
        groups.push((2, vec![
            ("by ".into(), st(GitRole::Muted, PANE)),
            (record.row.author.clone(), st(GitRole::Author, PANE)),
        ]));
        let (word, stamp) = if pull {
            ("updated", record.row.updated.as_str())
        } else {
            ("opened", record.created.as_str())
        };
        let when = records::ago(stamp, now);
        if !when.is_empty() {
            groups.push((1, vec![(format!("{word} {when}"), st(GitRole::Muted, PANE))]));
        }
        if !pull && !record.assignees.is_empty() {
            groups.push((3, vec![(format!("{} {}", glyph(Glyph::Person, mode), record.assignees.join(", ")), st(GitRole::Author, PANE))]));
        }
        let room = usize::from(right.saturating_sub(at)).saturating_sub(2);
        let size = |groups: &[(u8, Vec<Span>)]| {
            2 + groups.iter().map(|(_, spans)| spans.iter().map(|(text, _)| text.width()).sum::<usize>()).sum::<usize>()
                + 3 * groups.len().saturating_sub(1)
        };
        while size(&groups) > room && groups.len() > 1 {
            let drop = groups.iter().enumerate().max_by_key(|(_, (rank, _))| *rank).map(|(at, _)| at).unwrap_or(0);
            groups.remove(drop);
        }
        let mut facts: Vec<Span> = vec![("  ".into(), st(GitRole::Text, PANE))];
        for (index, (_, spans)) in groups.into_iter().enumerate() {
            if index > 0 {
                facts.push((" · ".into(), st(GitRole::Muted, PANE)));
            }
            facts.extend(spans);
        }
        at = put(&mut frame.grid, at, y, &facts, right);
        // Detail freshness on the right.
        let key = Key::Item(workspace.page, id);
        let status = match workspace.cache.get(&key) {
            Some(Err(_)) => Some(("details unavailable", GitRole::Modified)),
            None => Some(("loading details…", GitRole::Muted)),
            Some(Ok(_)) if workspace.pending(&key) && !workspace.fresh.implicit.contains(&key) => {
                Some(("⟳ refreshing", GitRole::Muted))
            }
            _ => None,
        };
        if let Some((text, role)) = status {
            let w = chrome::width(text);
            if right > at + w + 2 {
                frame.grid.put_text_clipped(right - w, y, text, st(role, PANE), right);
            }
        }
        y += 1;
    } else if workspace.repo.is_empty() || id == 0 {
        // Nothing selected: the body explains.
    } else {
        frame.grid.put_text_clipped(x, y, &format!("{}{id}", workspace.sigil(pull)), bold(GitRole::Muted, PANE), right);
        y += 2;
    }
    if record.is_some() {
        let (tabs, active) = reader_tabs(workspace, record.as_ref());
        // The Merge tab comes and goes; the row keeps room for it so the other names stay
        // the same on every pull request tab.
        let reserve = if pull && workspace.pr_tab != pr::MERGE { 8 } else { 0 };
        let tabs = chrome::fit_tabs_reserving(&tabs, active, right.saturating_sub(x.saturating_sub(1)), reserve);
        y += 1;
        chrome::sub_tabs(&mut frame.grid, x.saturating_sub(1), y, right, &tabs, active, focused);
        if pull && workspace.pr_tab == pr::FILES {
            let hint = "f choose file · v viewed";
            let w = chrome::width(hint);
            if right > x + w + 60 {
                frame.grid.put_text_clipped(right - w, y, hint, st(GitRole::Muted, PANE), right);
            }
        }
        y += 1;
        for column in inner.x..inner.right() {
            frame.grid.set_cell(column, y, "─", st(GitRole::Border, PANE));
        }
        y += 1;
    }
    let body = Rect::new(x, y, right.saturating_sub(x), inner.bottom().saturating_sub(y + reply_rows));
    body_rows(frame, workspace, body, mode);
}

/// The reader's rows for the selected issue or pull request; find searches the same rows.
pub(super) fn record_rows(workspace: &Workspace, width: usize, mode: ExplorerIconMode) -> Vec<Line> {
    let mut rows = if let Some((host, hints)) = workspace.sign_in_hints() {
        let host = if host.is_empty() { "This forge".to_owned() } else { host };
        let mut rows = vec![
            super::prose::Rich::new()
                .push(&format!("{host} requires sign-in"), super::prose::strong(GitRole::Modified))
                .line(),
            super::prose::blank(),
            super::prose::muted("Anonymous visitors see nothing here,"),
            super::prose::muted("not even public repositories."),
            super::prose::blank(),
            super::prose::Rich::new().push("Sign in with", super::prose::strong(GitRole::Text)).line(),
        ];
        for hint in &hints {
            rows.push(
                super::prose::Rich::new()
                    .push("  $ ", super::prose::role(GitRole::Muted))
                    .push(&crate::forge::cli::redact(hint), super::prose::role(GitRole::Link))
                    .line(),
            );
        }
        rows.push(super::prose::blank());
        rows.push(super::prose::muted("then R to retry · a accounts"));
        rows
    } else if workspace.repo.is_empty() {
        let mut rows = Vec::new();
        lines::error_or_loading(workspace, &Key::Repository, &mut rows);
        rows
    } else if let Some(Err(error)) = workspace.cache.get(&workspace.list_key()) {
        vec![
            super::prose::Rich::new().push("This view is unavailable", super::prose::strong(GitRole::Modified)).line(),
            super::prose::blank(),
            super::prose::muted(error),
            super::prose::muted("R retry · / change the filter"),
        ]
    } else if workspace.page == 1 {
        issue::rows(workspace, width, mode)
    } else {
        pr::rows(workspace, width, mode)
    };
    workspace.mark_refused(&mut rows);
    for row in &mut rows {
        // Diff rows scroll sideways; everything else is wrapped already.
        if row.diff.is_none() && row.gutter.is_empty() && row.source.is_none() && row.paint.is_empty() && row.band.is_none() {
            row.pan = row.pan && row.style != Style::Muted;
        }
    }
    rows
}

fn body_rows(frame: &mut Frame, workspace: &mut Workspace, body: Rect, mode: ExplorerIconMode) {
    let focused = workspace.focus == 2;
    if workspace.detail_find.search.active() {
        workspace.reading = false;
    }
    // Reading shows the text without a row cursor (§3.27); the first key brings it back.
    let cursor_shown = focused && !workspace.reading;
    let (left, content_right) = (body.x, body.right());
    let width = usize::from(body.w);
    workspace.detail_width = width;
    workspace.icons = mode;
    let selected_action = if workspace.page == 1 {
        workspace.detail_rows.get(workspace.detail_cursor).and_then(|row| row.action.clone())
    } else {
        None
    };
    let mut rows = record_rows(workspace, width, mode);
    if workspace.page == 1 {
        let identity = (workspace.host.clone(), workspace.repo.clone(), workspace.list_key(), workspace.number());
        workspace.reading_position.restore(
            identity,
            &workspace.detail_rows,
            &rows,
            &mut workspace.detail_cursor,
            &mut workspace.detail_scroll,
        );
    }
    let scoped_top = if workspace.page != 1 {
        let context = lines::position_context(workspace);
        if workspace.reading_position.context.as_ref() != Some(&context) {
            workspace.detail_scroll = 0;
        }
        workspace.reading_position.restore_scoped(
            context,
            &workspace.detail_rows,
            &rows,
            &mut workspace.detail_cursor,
            workspace.detail_scroll,
        )
    } else {
        None
    };
    if workspace.reveal_file
        && workspace.page == 2
        && workspace.pr_tab == pr::FILES
        && workspace.value(&Key::Diff(workspace.number())).is_some()
    {
        workspace.detail_cursor = rows
            .iter()
            .position(|row| matches!(row.action, Some(Action::Inline { .. })))
            .unwrap_or(0);
        workspace.detail_scroll = 0;
        workspace.detail_rows.clear();
        workspace.reveal_file = false;
    }
    if let Some(action) = selected_action.as_ref()
        && let Some(index) = rows.iter().position(|row| row.action.as_ref() == Some(action))
    {
        workspace.detail_cursor = index;
    }
    workspace.sync_detail_find(&rows);
    workspace.detail_cursor = workspace.detail_cursor.min(rows.len().saturating_sub(1));
    let active_thread = if cursor_shown {
        rows.get(workspace.detail_cursor).and_then(|row| row.reply.as_ref())
    } else {
        None
    };
    let show_quiet = workspace.expanded_details.contains(&(workspace.page, workspace.number()))
        || rows.get(workspace.detail_cursor).is_some_and(|row| row.quiet_review);
    let shown = |row: &Line| {
        (!row.quiet_review || show_quiet) && (!row.thread_action || row.reply.as_ref() == active_thread)
    };
    let split_outline = width >= 72 && rows.iter().any(|row| row.outline);
    let outline_right = if split_outline {
        left + (body.w * 22 / 100).clamp(24, 44)
    } else {
        left
    };
    let mut pinned = Vec::new();
    if workspace.page == 2 && workspace.pr_tab == pr::FILES {
        for (index, row) in rows.iter().enumerate() {
            if matches!(row.action, Some(Action::Files(_) | Action::Viewed(..))) {
                pinned.push(index);
            }
        }
    }
    let start = body.y + pinned.len() as u16 + u16::from(!pinned.is_empty());
    let payload_bottom = body.bottom();
    let visible = usize::from(payload_bottom.saturating_sub(start)).max(1);
    let main = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| (shown(row) && !(split_outline && row.outline) && !pinned.contains(&i)).then_some(i))
        .collect::<Vec<_>>();
    let outline = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| (row.outline && shown(row) && !pinned.contains(&i)).then_some(i))
        .collect::<Vec<_>>();
    if let Some(top) = scoped_top.and_then(|index| main.iter().position(|row| *row == index)) {
        workspace.detail_scroll = top;
    }
    if focused && let Some(position) = main.iter().position(|i| *i == workspace.detail_cursor) {
        let margin = (visible / 3).min(6);
        if position < workspace.detail_scroll {
            workspace.detail_scroll = position;
        } else if position + margin >= workspace.detail_scroll + visible {
            workspace.detail_scroll = (position + margin + 1).saturating_sub(visible);
        }
    }
    workspace.detail_scroll = workspace.detail_scroll.min(main.len().saturating_sub(visible));
    let outline_offset = outline
        .iter()
        .position(|i| *i == workspace.detail_cursor)
        .unwrap_or(0)
        .saturating_sub(visible - 1);
    let diff_left = if split_outline { outline_right + 2 } else { left };
    if !pinned.is_empty() {
        for column in diff_left.saturating_sub(1)..content_right + 1 {
            frame.grid.set_cell(column, start - 1, "─", st(GitRole::Border, PANE));
        }
    }
    if split_outline {
        for y in body.y..payload_bottom {
            frame.grid.set_cell(outline_right, y, "│", st(GitRole::Border, PANE));
        }
    }
    let mut positions = vec![None; rows.len()];
    for (position, &index) in pinned.iter().enumerate() {
        positions[index] = Some((diff_left, content_right, body.y + position as u16));
    }
    if split_outline {
        for (position, &index) in outline.iter().skip(outline_offset).take(visible + pinned.len() + 1).enumerate() {
            positions[index] = Some((left, outline_right - 1, body.y + position as u16));
        }
    }
    for (position, &index) in main.iter().skip(workspace.detail_scroll).take(visible).enumerate() {
        positions[index] = Some((diff_left, content_right, start + position as u16));
    }
    // Reveal a find match horizontally inside panned rows.
    if let Some((x, right, _)) = positions.get(workspace.detail_cursor).copied().flatten()
        && let Some(row) = rows.get(workspace.detail_cursor)
        && row.pan
        && let Some(byte) = workspace.detail_find.cursor_byte(workspace.detail_cursor)
    {
        let available = usize::from(right.saturating_sub(x)).saturating_sub(row.gutter.width() + 1).max(2);
        let (mut column, mut first, mut end) = (0, None, 0);
        let (matches, offset) = workspace.detail_find.row_matches(workspace.detail_cursor);
        let found = matches.iter().find(|found| found.bytes.start == byte + offset);
        let bytes = found.map_or(byte..byte + 1, |found| byte..found.bytes.end.saturating_sub(offset));
        for (at, grapheme) in row.text.grapheme_indices(true) {
            let size = if grapheme == "\t" { 4 - column % 4 } else { grapheme.width().max(1) };
            if at < bytes.end && at + grapheme.len() > bytes.start {
                first.get_or_insert(column);
                end = column + size;
            }
            column += size;
        }
        if let Some(first) = first {
            let reveal_end = end.saturating_add(4).min(column);
            let mut horizontal = workspace.horizontal;
            if reveal_end <= available {
                horizontal = 0;
            } else if first < horizontal {
                horizontal = first;
            }
            if reveal_end > horizontal.saturating_add(available) {
                horizontal = reveal_end.saturating_sub(available).min(first);
            }
            workspace.horizontal = horizontal;
        }
    }
    let selected_file = workspace
        .item()
        .map(|item| workspace.selected_file(workspace.number(), item).to_owned())
        .unwrap_or_default();
    for (i, (row, position)) in rows.iter().zip(positions).enumerate() {
        let Some((x, right, y)) = position else {
            continue;
        };
        let selected = cursor_shown && i == workspace.detail_cursor;
        let active_file = matches!(row.action.as_ref(), Some(Action::SelectFile(_, path)) if *path == selected_file)
            && workspace.pr_tab == pr::FILES;
        let hunk = row.text.starts_with("@@");
        let surface = if selected {
            GitSurface::Cursor
        } else if active_file {
            GitSurface::CursorInactive
        } else if let Some(band) = row.band {
            band
        } else if let Some(added) = row.diff {
            if added { GitSurface::AddedBand } else { GitSurface::RemovedBand }
        } else if hunk {
            GitSurface::HunkBand
        } else if matches!(row.style, Style::GitHeading) && row.action.is_some() {
            GitSurface::FileBand
        } else {
            PANE
        };
        let base = if row.thread_header && row.reply.as_ref() == active_thread {
            st(GitRole::Focus, surface)
        } else if let Some(added) = row.diff {
            st(if added { GitRole::Added } else { GitRole::Removed }, surface)
        } else if row.action.is_some() && row.paint.is_empty() && row.spans.is_empty() {
            // Legacy buttons (`› Review changes`) read as links.
            st(if row.outline { GitRole::Text } else { GitRole::Link }, surface)
        } else {
            legacy(row.style, surface)
        };
        if surface != PANE {
            let from = x.saturating_sub(u16::from(surface == GitSurface::Cursor));
            for at in from..right {
                frame.grid.set_cell(at, y, " ", st(GitRole::Text, surface));
            }
        }
        let row_x = x;
        let gutter_style = match row.gutter_style {
            Style::Muted => st(GitRole::Muted, surface),
            style => legacy(style, surface),
        };
        let gutter_style = if row.diff.is_some() { st(GitRole::Muted, surface) } else { gutter_style };
        let mut x = frame.grid.put_text_clipped(x, y, &row.gutter, gutter_style, right);
        let content_x = x.min(right.saturating_sub(1));
        let mut cursor_x = None;
        let horizontal = if !row.pan || (split_outline && row.outline) || pinned.contains(&i) {
            0
        } else {
            workspace.horizontal
        };
        let cursor_byte = selected.then(|| workspace.detail_find.cursor_byte(i)).flatten();
        let (matches, offset) = workspace.detail_find.row_matches(i);
        let mut column = 0;
        let graphemes = row.text.grapheme_indices(true).collect::<Vec<_>>();
        for (index, (at, g)) in graphemes.iter().enumerate() {
            let (at, g) = (*at, *g);
            let size = if g == "\t" { 4 - column % 4 } else { g.width().max(1) };
            let end = column + size;
            if end > horizontal {
                // A split diff's sides carry their own bands unless the cursor is on the row.
                let cell = if selected || active_file {
                    surface
                } else {
                    row.bands.iter().find(|(range, _)| range.contains(&at)).map_or(surface, |(_, band)| *band)
                };
                let style = if let Some((_, style)) = row.paint.iter().rev().find(|(range, _)| range.contains(&at)) {
                    on(*style, cell)
                } else if let Some((_, highlight)) = row.spans.iter().find(|(range, _)| range.contains(&at)) {
                    highlight_style(*highlight, cell)
                } else {
                    on(base, cell)
                };
                let candidate = matches.partition_point(|found| found.bytes.end <= at + offset);
                let style = if matches.get(candidate).is_some_and(|found| found.bytes.start < at + offset + g.len()) {
                    Style::Search
                } else {
                    style
                };
                let last = index + 1 == graphemes.len();
                let fits = x + size as u16 <= right && (last || x + (size as u16) < right);
                if !fits {
                    frame.grid.set_cell(right.saturating_sub(1), y, if row.pan { "›" } else { "…" }, st(GitRole::Muted, surface));
                    break;
                }
                let next = if g == "\t" || column < horizontal {
                    frame.grid.put_text_clipped(x, y, &" ".repeat(end - column.max(horizontal)), style, right)
                } else {
                    frame.grid.put_text_clipped(x, y, g, style, right)
                };
                if cursor_byte.is_some_and(|byte| at <= byte && byte < at + g.len())
                    || (cursor_byte.is_none() && cursor_x.is_none() && !g.trim().is_empty())
                {
                    cursor_x = Some(x);
                }
                x = next;
            }
            column = end;
        }
        if selected {
            // A mark sits in the gutter beside the row (the band marks the row) and the bar
            // cursor before the first letter; a find match takes it to the match.
            frame.cursor = Some(match cursor_x.filter(|_| cursor_byte.is_some()) {
                Some(at) => (at, y, SetCursorStyle::SteadyBlock),
                None => {
                    let gutter = row_x.saturating_sub(1).max(left.saturating_sub(1));
                    frame.grid.set_cell(gutter, y, "▎", bold(GitRole::Focus, surface));
                    // The terminal's own cursor is a thin bar at the text's first cell.
                    (content_x, y, SetCursorStyle::SteadyBar)
                }
            });
        }
    }
    if cursor_shown && frame.cursor.is_none() {
        frame.cursor = Some((left, body.y, SetCursorStyle::SteadyBlock));
    }
    // A scroll position when the body overflows.
    if main.len() > visible && body.bottom() > body.y {
        // Lines of the reader, not items of the list: said in words so the two counts are
        // never read as one.
        let last = (workspace.detail_scroll + visible).min(main.len());
        let label = format!(
            " lines {}–{last} of {}{} ",
            workspace.detail_scroll + 1,
            main.len(),
            if last < main.len() { " ↓" } else { "" }
        );
        let w = chrome::width(&label);
        let y = body.bottom();
        if y > 0 && content_right > left + w {
            frame.grid.put_text_clipped(content_right + 1 - w, y, &label, st(GitRole::Muted, PANE), content_right + 1);
        }
    }
    if workspace.page == 1 {
        workspace.reading_position.finish(&rows, workspace.detail_cursor, workspace.detail_scroll);
    } else {
        workspace.reading_position.finish_scoped(&rows, workspace.detail_cursor, workspace.detail_scroll, main);
    }
    workspace.detail_rows = rows;
}

/// §3.34: the reply box pinned to the bottom of the reader.
fn reply_box(frame: &mut Frame, workspace: &Workspace, form: &super::super::Form, rect: Rect, rows: u16) {
    let inner = rect.inner();
    let top = inner.bottom().saturating_sub(rows);
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    chrome::fill(&mut frame.grid, Rect::new(inner.x, top, inner.w, rows), st(GitRole::Text, PANE));
    for column in inner.x..inner.right() {
        frame.grid.set_cell(column, top, "─", st(GitRole::Border, PANE));
    }
    let login = crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&workspace.host))
        .and_then(|account| account.login);
    // `Reply to @alex · as maya`: whose thread this answers, and who is writing.
    let target = form.title.split(" · ").next().filter(|first| first.starts_with("Reply to "));
    let label = match (&form.action, login) {
        (FormAction::Github(Action::Reply { .. }), Some(login)) => {
            format!("{} · as {login}", target.unwrap_or("Reply in thread"))
        }
        (FormAction::Github(Action::Reply { .. }), None) => target.unwrap_or("Reply in thread").to_owned(),
        (_, Some(login)) => format!("Comment as {login}"),
        (_, None) => "Comment".to_owned(),
    };
    frame.grid.put_text_clipped(x, top + 1, &label, bold(GitRole::Text, PANE), right);
    let field = &form.fields[0];
    let box_rect = Rect::new(x, top + 2, right.saturating_sub(x), rows.saturating_sub(4));
    // A framed box (╭╮╰╯) around the input.
    let edge = st(GitRole::Focus, PANE);
    let (bx, by, bw, bh) = (box_rect.x, box_rect.y, box_rect.w, box_rect.h + 0);
    if bw > 4 && bh >= 3 {
        for column in bx + 1..bx + bw - 1 {
            frame.grid.set_cell(column, by, "─", edge);
            frame.grid.set_cell(column, by + bh - 1, "─", edge);
        }
        for row in by + 1..by + bh - 1 {
            frame.grid.set_cell(bx, row, "│", edge);
            frame.grid.set_cell(bx + bw - 1, row, "│", edge);
        }
        frame.grid.set_cell(bx, by, "╭", edge);
        frame.grid.set_cell(bx + bw - 1, by, "╮", edge);
        frame.grid.set_cell(bx, by + bh - 1, "╰", edge);
        frame.grid.set_cell(bx + bw - 1, by + bh - 1, "╯", edge);
        let inside = Rect::new(bx + 2, by + 1, bw.saturating_sub(4), bh - 2);
        super::super::shell::sheet::input(frame, field, inside, true);
        // `input` marks its left edge; the frame already does.
        for row in inside.y..inside.bottom() {
            frame.grid.set_cell(inside.x - 1, row, " ", st(GitRole::Text, GitSurface::CursorInactive));
        }
    }
    let hint_y = top + rows - 1;
    // The send and Esc keys are in the footer, once.
    frame.grid.put_text_clipped(x, hint_y, "Markdown supported", st(GitRole::Muted, PANE), right);
}

fn footer(app: &GitScreen, frame: &mut Frame, rect: Rect, filter: bool) {
    let workspace = &app.github;
    let grid = &mut frame.grid;
    if let Some(form) = &app.form {
        let several = form.fields.len() > 1;
        // A sheet prints its own submit, Tab and Esc keys beside its buttons; the footer
        // adds only what the sheet does not say, so no key is listed twice.
        let hints: Vec<(&str, &str)> = if filter {
            vec![("Enter", "search the forge"), ("Tab", "state"), ("Esc", "clear")]
        } else if super::forms::owns(form) {
            // Review and merge: the send key stays in the footer with the field keys.
            let multiline = form.fields.iter().any(|field| field.multiline);
            let merge = matches!(&form.action, FormAction::Github(Action::Merge(..)));
            vec![
                (if multiline { "Ctrl+Enter" } else { "Enter" }, if merge { "merge" } else { "submit review" }),
                ("Esc", "continue editing"),
                ("Tab", "next field"),
                ("Shift+Tab", "previous"),
            ]
        } else if matches!(&form.action, FormAction::Github(Action::Comment(..) | Action::Reply { .. })) {
            // The composer: Esc leaves the text in place for the next `c`.
            vec![("Ctrl+Enter", "submit"), ("Esc", "keep draft")]
        } else {
            // A sheet: its keys live here, not on the sheet.
            let multiline = form.fields.iter().any(|field| field.multiline);
            let mut keys = vec![(if multiline { "Ctrl+Enter" } else { "Enter" }, "confirm"), ("Esc", "cancel")];
            if several {
                keys.extend([("Tab", "next field"), ("Shift+Tab", "previous")]);
            }
            keys
        };
        let hints = hints.as_slice();
        let message = (!app.notice.is_empty()).then(|| (app.notice.as_str(), true));
        // The live filter names what it filters (`Filter issues`) where the query would go.
        let context = if filter { form.title.split(" · ").next().unwrap_or_default() } else { "" };
        chrome::footer(grid, rect, hints, context, message);
        return;
    }
    let find = (workspace.focus == 2 && workspace.detail_find.search.active())
        .then(|| workspace.detail_find.footer_status(workspace.detail_cursor, rect.w));
    let refresh = if workspace.page < 3
        && !workspace.fresh.implicit.contains(&workspace.list_key())
        && workspace.pending(&workspace.list_key())
        && workspace.cache.get(&workspace.list_key()).is_some_and(Result::is_ok)
    {
        if workspace.loading_more() {
            "Loading more… · showing cached results"
        } else {
            "Refreshing view… · showing cached results"
        }
    } else if workspace.page < 3 && workspace.number() != 0 && {
        let item = Key::Item(workspace.page, workspace.number());
        workspace.item().is_some() && workspace.pending(&item) && !workspace.fresh.implicit.contains(&item)
    } {
        "Refreshing details… · showing what was loaded"
    } else {
        ""
    };
    let message = if workspace.writing() {
        Some(("Sending to the forge…", false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), true))
    } else if !workspace.notice.is_empty() {
        Some((workspace.notice.as_str(), workspace.notice_error))
    } else {
        None
    };
    let enter = match workspace.detail_rows.get(workspace.detail_cursor).and_then(|row| row.action.as_ref()) {
        Some(Action::Reply { .. }) => "reply",
        Some(Action::Resolve(_, true)) => "resolve",
        Some(Action::Resolve(_, false)) => "reopen",
        Some(Action::Files(_)) => "choose file",
        Some(Action::Inline { .. }) => "comment",
        Some(Action::Metadata(_, _, name)) => *name,
        Some(Action::PrTab(_)) => "show",
        Some(Action::Merge(..)) => "merge",
        Some(Action::Review(_)) => "review",
        Some(Action::Develop(_)) => "create branch",
        Some(_) => "open",
        None => "",
    };
    let reader = workspace.focus == 2;
    // While reading nothing is selected: the first hint says how to reach the rows.
    let (enter_key, enter) = if reader && workspace.reading { ("j k", "select") } else { ("Enter", enter) };
    let hints: Vec<(&str, &str)> = match (workspace.page, reader) {
        (1, false) => vec![("Enter", "open"), ("C", "comment"), ("e", "edit"), ("x", "close"), ("l", "labels"), ("/", "filter")],
        // The reader's way back stays visible on narrow terminals: Esc comes before edits.
        (1, true) => vec![(enter_key, enter), ("C", "comment"), ("[ ]", "tabs"), ("Esc", "list"), ("e", "edit"), ("x", "close")],
        (_, false) => vec![("Enter", "open"), ("r", "review"), ("c", "checkout"), ("w", "worktree"), ("m", "merge"), ("/", "filter")],
        (_, true) if workspace.pr_tab == pr::FILES => {
            let blocked = workspace.split_diff
                && super::files::diff_width(workspace, workspace.detail_width) < super::files::SPLIT_WIDTH;
            let layout = match (workspace.split_diff, blocked) {
                (true, true) => "unified (split needs 96 cols)",
                (true, false) => "unified",
                (false, _) => "split",
            };
            vec![(enter_key, enter), ("r", "review"), ("u", layout), ("v", "viewed"), ("Esc", "back"), ("m", "merge"), ("f", "files"), ("[ ]", "tabs")]
        }
        (_, true) => vec![(enter_key, enter), ("r", "review"), ("C", "comment"), ("[ ]", "tabs"), ("Esc", "list"), ("m", "merge")],
    };
    let mut hints = hints.into_iter().filter(|(_, label)| !label.is_empty()).collect::<Vec<_>>();
    if workspace.read_only() {
        // Signed out: write keys leave the footer; pressing one still says why. The accounts
        // popover (`a`, from the list) shows how to sign in.
        const WRITES: [&str; 8] = ["review", "comment", "edit", "close", "labels", "viewed", "reply", "resolve"];
        hints.retain(|(_, label)| !WRITES.contains(label));
        if !reader {
            hints.push(("a", "sign in"));
        }
    }
    if find.is_some() {
        // While a find is active its keys lead; the rest follow as room allows.
        hints.splice(0..0, [("n/N", "next match"), ("Esc", "clear find")]);
        hints.retain(|(key, label)| !(*key == "Esc" && *label != "clear find"));
    }
    let context = find.unwrap_or_else(|| {
        if !refresh.is_empty() {
            // The context keeps a third of the row; a narrow one keeps what is happening.
            if rect.w / 3 < chrome::width(refresh) {
                refresh.split(" · ").next().unwrap_or(refresh).to_owned()
            } else {
                refresh.to_owned()
            }
        } else if reader && rect.w < 100 {
            // A narrow reader hides the list, and the room goes to the way back.
            String::new()
        } else {
            let at = workspace.page.saturating_sub(1).min(1);
            // The long qualifier reads as its short form here; the query itself is unchanged.
            workspace.queries[at].replace("review-requested:", "review:")
        }
    });
    chrome::footer(grid, rect, &hints, &context, message);
}
