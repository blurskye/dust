//! The forge tabs: Pull requests and Issues on the shell chrome.
//! A list of two-row records beside a reader with a pinned header (title, state chips,
//! tabs) over Markdown, metadata and timeline rows. Actions paints in `runs`.
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::{self, Mode, Rect};
use super::super::{FormAction, GitScreen};
use super::lines::Line;
use super::prose::highlight_style;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::Lifecycle;
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
            surface: if matches!(own, GitSurface::Pane) {
                surface
            } else {
                own
            },
            emphasis,
        },
        Style::GitSyntax {
            group,
            surface: own,
        } => Style::GitSyntax {
            group,
            surface: if matches!(own, GitSurface::Pane) {
                surface
            } else {
                own
            },
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

fn lifecycle_mark(
    lifecycle: Lifecycle,
    pull: bool,
    mode: ExplorerIconMode,
) -> (&'static str, GitRole) {
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
fn state_chip(
    lifecycle: Lifecycle,
    pull: bool,
    mode: ExplorerIconMode,
) -> (String, GitChip, GitRole) {
    match (lifecycle, pull) {
        (Lifecycle::Open, _) => (
            format!("{} Open", glyph(Glyph::Dot, mode)),
            GitChip::Added,
            GitRole::Added,
        ),
        (Lifecycle::Draft, _) => (
            format!("{} Draft", glyph(Glyph::ReviewRequested, mode)),
            GitChip::Muted,
            GitRole::Muted,
        ),
        (Lifecycle::Merged, _) => (
            format!("{} Merged", glyph(Glyph::PullMerged, mode)),
            GitChip::Ref,
            GitRole::Ref,
        ),
        (Lifecycle::Closed, true) => (
            format!("{} Closed", glyph(Glyph::CheckFailed, mode)),
            GitChip::Removed,
            GitRole::Removed,
        ),
        (Lifecycle::Closed, false) => (
            format!("{} Closed", glyph(Glyph::IssueClosed, mode)),
            GitChip::Ref,
            GitRole::Ref,
        ),
        (Lifecycle::ClosedNotPlanned, _) => (
            format!("{} Not planned", glyph(Glyph::NotPlanned, mode)),
            GitChip::Muted,
            GitRole::Muted,
        ),
    }
}

fn label_spans(labels: &[records::LabelChip], room: usize, surface: GitSurface) -> Vec<Span> {
    // One rule on every row and at every width: up to two chips, each whole, then
    // `+N` for the rest. A row whose first chip and its `+N` do not fit shows no labels
    // (the reader lists them); a chip is never cut nor swapped for a count.
    let mut spans = Vec::new();
    let mut used = 0;
    for (index, label) in labels.iter().enumerate() {
        let text = format!(" {} ", label.name);
        let rest = labels.len() - index - 1;
        let tail = if rest > 0 {
            format!(" +{rest}")
        } else {
            String::new()
        };
        let gap = usize::from(index > 0);
        if index >= 2 || used + gap + text.width() + tail.width() > room {
            if index > 0 {
                // The previous chip kept room for exactly this tail.
                spans.push((
                    format!(" +{}", labels.len() - index),
                    st(GitRole::Muted, surface),
                ));
            }
            break;
        }
        if index > 0 {
            spans.push((" ".into(), st(GitRole::Text, surface)));
        }
        used += gap + text.width();
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
    let footer = Rect::new(0, height.saturating_sub(1), width, 1);
    // The list stays beside every reader tab, Files included, until `toggle_sidebar`
    // (`Space e`, `keymap.rs`) hides it: then the reader takes the width while it has focus,
    // and the list comes back when focus returns to it.
    let hidden =
        workspace.sidebar_hidden() && workspace.focus == 2 && matches!(workspace.page, 1 | 2);
    if mode == Mode::Narrow {
        return if workspace.focus == 2 {
            Geometry {
                list: None,
                reader: Some(body),
                footer,
                mode,
            }
        } else {
            Geometry {
                list: Some(body),
                reader: None,
                footer,
                mode,
            }
        };
    }
    if hidden {
        return Geometry {
            list: None,
            reader: Some(body),
            footer,
            mode,
        };
    }
    let left = split_list_width(workspace, width, mode);
    Geometry {
        list: Some(Rect::new(0, 1, left, body.h)),
        reader: Some(Rect::new(left, 1, width - left, body.h)),
        footer,
        mode,
    }
}

/// The list's share of a split body, by page, so each page's tab rows keep their names,
/// counts and gutters at 100 columns:
/// - pull requests: at least 40 for `Review 3 │ Mine 1 │ Drafts 1 │ All 8` and a row's
///   `maya · 9m · ◌ Review requested`; at 100 the reader's four tab names fit beside it
///   (`Pipelines` included) and its counts drop all at once (r11: never a bare icon);
/// - issues: the list keeps 43 for `All 17 │ Bugs 1 │ Features 4 │ Other 12` and a blank
///   before the border;
/// - notifications: the list keeps 45 for `All 6 │ Mentions 1 │ Review 2 │ Checks 1` and
///   a blank before the border.
///
/// [`list_width`] for this workspace: a pull request page gives the reader back a column or
/// two when its four tab names would otherwise meet the border (GitLab's `Pipelines` at 100,
/// r11 GL-01), so every reader tab row keeps a gutter before the frame.
/// Files changed keeps its tree beside the diff at every width (only
/// `Space e` hides a pane): below ~110 columns the list gives the reader a few columns back
/// for it, down to 36, rather than the tree folding away.
fn split_list_width(workspace: &Workspace, width: u16, mode: Mode) -> u16 {
    let mut left = list_width(width, mode, workspace.page);
    if workspace.page == 2 {
        let names = [
            "Conversation",
            "Commits",
            workspace.ci_words().0,
            "Files changed",
        ]
        .map(String::from);
        while left > 38 && !chrome::tabs_fit(&names, width.saturating_sub(left + 2)) {
            left -= 1;
        }
        let tree = workspace.pr_tab == pr::FILES;
        while tree && left > 36 && width.saturating_sub(left) < 64 {
            left -= 1;
        }
    }
    left
}

/// The narrowest reader body an outline (the changed-file tree) sits beside: its column and a 32-cell pane.
pub(super) const OUTLINE_MIN: usize = 58;

/// The outline column's width beside a reader body `width` cells wide, its rule excluded.
pub(super) fn outline_width(width: usize) -> usize {
    (width * 22 / 100).clamp(24, 44)
}

/// The widest an outline grows to fit its rows: two fifths of the reader, leaving the pane
/// beside it 34 cells, never narrower than [`outline_width`].
pub(super) fn outline_most(width: usize) -> usize {
    (width * 2 / 5)
        .min(width.saturating_sub(34))
        .max(outline_width(width))
}

/// The outline column sized to its rows (the checks list, the file tree): its widest row
/// and the blank before the rule, within [`outline_width`] and [`outline_most`].
pub(super) fn outline_fit(width: usize, rows: &[Line]) -> usize {
    rows.iter()
        .filter(|row| row.outline)
        .map(|row| row.text.trim_end().width() + 1)
        .max()
        .unwrap_or(0)
        .clamp(outline_width(width), outline_most(width))
}

/// The cells an outline row's text may take: its column less the blank before the rule.
pub(super) fn outline_room(width: usize) -> usize {
    outline_width(width) - 1
}

pub(super) fn list_width(width: u16, mode: Mode, page: usize) -> u16 {
    let percent = if mode == Mode::Wide { 44 } else { 40 };
    let share = width * percent / 100;
    match page {
        2 => share.max(40),
        notifications::PAGE => share.max(45),
        _ => share.max(43),
    }
}

pub(in crate::screen) fn frame(app: &mut GitScreen, width: u16, height: u16) -> Frame {
    let mut frame = Frame {
        grid: CellGrid::new(width, height),
        cursor: None,
    };
    if layout::mode(width, height) == Mode::TooSmall {
        let text = format!(
            "Dusty Git needs {}×{}",
            layout::MIN_WIDTH,
            layout::MIN_HEIGHT
        );
        frame.grid.put_text_clipped(
            width.saturating_sub(chrome::width(&text)) / 2,
            height / 2,
            &text,
            bold(GitRole::Modified, GitSurface::Canvas),
            width,
        );
        return frame;
    }
    chrome::fill(
        &mut frame.grid,
        Rect::new(0, 0, width, height),
        st(GitRole::Text, GitSurface::Canvas),
    );
    super::super::shell::top_bar(app, &mut frame.grid, width);
    let filter_form = app
        .form
        .as_ref()
        .is_some_and(|form| matches!(&form.action, FormAction::Github(Action::Filter)));
    // The filter form's text drives a local search while the remote one runs.
    app.github
        .live_input(app.form.as_ref().filter(|_| filter_form).map(|form| {
            (
                form.fields
                    .first()
                    .map(|field| field.input.input.trim().to_owned())
                    .unwrap_or_default(),
                form.fields
                    .get(1)
                    .map(|field| field.input.input.clone())
                    .unwrap_or_default(),
            )
        }));
    let mode_icons = app.icon_mode;
    let live = app.github.live_rows();
    app.github.live_pick = live
        .as_ref()
        .and_then(|live| live.local.first().or(live.remote.first()))
        .cloned();
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
        list(
            &mut frame,
            &mut app.github,
            rect,
            filter,
            mode_icons,
            geometry.mode,
            live.as_ref(),
        );
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
        let rect = geometry
            .reader
            .unwrap_or(Rect::new(0, 1, width, height.saturating_sub(2)));
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
    live: Option<&super::fresh::LiveRows>,
) {
    let focused = workspace.focus < 2;
    let pull = workspace.page == 2;
    let terms = workspace.terms();
    let kind = if pull { terms.pr_plural } else { "Issues" };
    let title: Vec<Span> = vec![
        (
            format!(
                "{} ",
                glyph(
                    if pull {
                        Glyph::PullRequest
                    } else {
                        Glyph::Issue
                    },
                    mode
                )
            ),
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
        .freshness_chip(&workspace.list_key(), false)
        // Whole beside the whole title, or not at all (r11: never `now` for `updated just now`).
        .filter(|chip| chrome::spans_width(&title) + chrome::width(chip) + 6 <= rect.w)
        .unwrap_or_default();
    chrome::pane(&mut frame.grid, rect, &title, &chip, focused);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let mut y = inner.y;
    // Sections row.
    let sections = workspace.sections();
    let active = workspace.active_section(&sections);
    // While a typed query differs from the section's, no section claims the list.
    let typed = filter
        .and_then(|form| form.fields.first())
        .map(|field| field.input.input.trim().to_owned());
    let highlighted = active.filter(|_| {
        typed.as_deref().is_none_or(|typed| {
            typed == workspace.queries[workspace.page.saturating_sub(1).min(1)].trim()
        })
    });
    let tabs = sections
        .iter()
        .zip(workspace.section_counts(&sections))
        // `Other`, never `Other 0`: a section is a slice of the list, not a tab.
        .map(|(section, count)| counted(&section.name, count.filter(|count| *count > 0)))
        .collect::<Vec<_>>();
    // A custom query (no section claims the list) says so at the search row's end; the
    // section names keep the whole row (r7 N-03: a filter never squeezes `Features` out).
    // While the filter is typed its input row is the marker.
    let custom = highlighted
        .is_none()
        .then_some(" Custom filter ")
        .filter(|_| filter.is_none());
    // The first tab's pad is the gutter after the border, so its name lines up with the
    // rows below (as in the reader and the inbox). The blank after the last tab may run up
    // to the border, as in the reader's tab row (r10 R9-01: the counts keep that column at 100).
    let tabs_right = inner.right();
    let tabs_x = x.saturating_sub(1);
    let room = tabs_right.saturating_sub(tabs_x);
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
            // The same measure as the fit above; `… ` takes two columns once it shows.
            while first < active
                && !chrome::tabs_fit(
                    &tabs[first..=active],
                    room.saturating_sub(if first > 0 { 2 } else { 0 }),
                )
            {
                first += 1;
            }
            first
        })
        .unwrap_or(0);
    let shown = tabs[first..].to_vec();
    let mut at = tabs_x;
    if first > 0 {
        at = frame
            .grid
            .put_text_clipped(at, y, "… ", st(GitRole::Muted, PANE), tabs_right);
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
    y += 1;
    for column in inner.x..inner.right() {
        frame
            .grid
            .set_cell(column, y, "─", st(GitRole::Border, PANE));
    }
    y += 1;
    // GitHub's search box: the whole query the list reads (`is:pr is:open
    // review-requested:@me`), or the filter being typed.
    let at = workspace.page - 1;
    if let Some(form) = filter {
        let mark = format!("{} ", glyph(Glyph::Search, mode));
        let after = frame
            .grid
            .put_text_clipped(x, y, &mark, bold(GitRole::Focus, PANE), right);
        if let Some(field) = form.fields.first() {
            super::super::shell::sheet::input(
                frame,
                field,
                Rect::new(after + 1, y, right.saturating_sub(after + 1), 1),
                true,
            );
        }
    } else {
        let mark: Span = (
            format!("{} ", glyph(Glyph::Search, mode)),
            st(GitRole::Muted, PANE),
        );
        let used = chrome::width(&mark.0);
        // The badge only where the whole query fits beside it: the query is what matters.
        let query = workspace.search_query();
        // GitHub's `New issue` button stays at the search row's end whatever the list holds
        // (R16 issues-02), where the whole query still fits beside it; `n` opens it.
        const NEW_ISSUE: &str = " New issue ";
        let button = chrome::width(NEW_ISSUE);
        let right = if workspace.page == 1 && x + used + chrome::width(&query) + 2 + button <= right
        {
            frame.grid.put_text_clipped(
                right - button,
                y,
                NEW_ISSUE,
                bold(GitRole::Added, GitSurface::Chip(GitChip::Added)),
                right,
            );
            right - button - 1
        } else {
            right
        };
        let chip = custom.map_or(0, chrome::width);
        let query_right = if chip > 0 && x + used + chrome::width(&query) + 1 + chip <= right {
            right - chip - 1
        } else {
            right
        };
        let mut spans = vec![mark];
        spans.extend(query_spans(
            &query,
            usize::from(query_right.saturating_sub(x + used)),
        ));
        put(&mut frame.grid, x, y, &spans, query_right);
        if let Some(custom) = custom.filter(|_| query_right < right) {
            frame.grid.put_text_clipped(
                right - chip,
                y,
                custom,
                st(GitRole::Modified, GitSurface::Chip(GitChip::Modified)),
                right,
            );
        }
    }
    y += 1;
    // GitHub's `Open · Closed` toggle (`s`) and its filter menus (`F`).
    state_row(frame, workspace, x, y, right, mode);
    y += 1;
    // Records.
    let flash = std::mem::take(&mut workspace.fresh.flash);
    let mut entries = Vec::new();
    let rows_owned: Vec<Value>;
    match live {
        Some(live) => {
            entries.extend(live.local.iter().map(Entry::Record));
            // The forge's divider shows while it searches and when it adds results.
            if !live.divider.is_empty() {
                entries.push(Entry::Divider(live.divider.clone()));
            }
            entries.extend(live.remote.iter().map(Entry::Record));
        }
        None => {
            rows_owned = workspace.items().into_iter().cloned().collect();
            entries.extend(rows_owned.iter().map(Entry::Record));
            // `rows_owned` lives to the end of this function.
            let _ = &rows_owned;
        }
    }
    let key = workspace.list_key();
    // An empty list explains its own error; keep the notice if it can only say Loading….
    let empty_explains_error = workspace.sign_in_hints().is_some()
        || matches!(workspace.cache.get(&key), Some(Err(_)))
        || workspace.blocked(&key).is_some();
    if (!entries.is_empty() || !empty_explains_error)
        && let Some((text, tone)) =
            workspace.host_notice(workspace.cache.values().any(Result::is_ok))
    {
        let (surface, role) = match tone {
            crate::forge::host_state::Tone::Error => (GitSurface::ReceiptErr, GitRole::Removed),
            crate::forge::host_state::Tone::Warning => (GitSurface::Warning, GitRole::Modified),
            _ => (GitSurface::FileBand, GitRole::Muted),
        };
        chrome::fill(
            &mut frame.grid,
            Rect::new(inner.x, y, inner.w, 1),
            st(GitRole::Text, surface),
        );
        frame.grid.put_text_clipped(
            x,
            y,
            &chrome::ellipsize(&text, usize::from(right - x)),
            st(role, surface),
            right,
        );
        y += 1;
    }
    let selected = if live.is_some() {
        entries
            .iter()
            .position(|entry| matches!(entry, Entry::Record(_)))
            .unwrap_or(usize::MAX)
    } else {
        visible_index(entries.len(), workspace.selected[at]).unwrap_or(usize::MAX)
    };
    // The match count is the footer's `Showing N`; no line above the rows repeats it.
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
        let blocked = workspace.blocked(&key);
        let explained;
        let failed = workspace.sign_in_hints().is_some()
            || matches!(workspace.cache.get(&key), Some(Err(_)))
            || (!workspace.cache.contains_key(&key) && blocked.is_some());
        let (headline, hint) = match workspace.cache.get(&key) {
            // One empty state, in the reader: beside it the list holds only its
            // count, for a failure as for no matches, so the problem is said once.
            _ if layout_mode != Mode::Narrow
                && (failed
                    || matches!(workspace.cache.get(&key), Some(Ok(_)) if !workspace.pending(&key) && !workspace.inbox.enabled)) =>
            {
                if !failed {
                    let footer: Vec<Span> = vec![("Showing 0".into(), st(GitRole::Muted, PANE))];
                    put(&mut frame.grid, x, bottom, &footer, right);
                }
                // No cursor: alone in a blank pane it reads as a glitch, and the border
                // already says the list has focus.
                return;
            }
            _ if workspace.sign_in_hints().is_some() => ("Sign-in required", "@ sign in"),
            // The reason in words when it fits the pane whole, else the short form.
            Some(Err(error)) => {
                explained = workspace.explain(error);
                let room = usize::from(right.saturating_sub(x + 1));
                if explained.headline.width() <= room && explained.hint.width() <= room {
                    (explained.headline.as_str(), explained.hint.as_str())
                } else {
                    ("Couldn't load this view", "R retry")
                }
            }
            None => blocked.unwrap_or(("Loading…", "")),
            Some(Ok(_)) if workspace.pending(&key) => ("Refreshing…", ""),
            Some(Ok(_)) if workspace.inbox.enabled => ("No matches", "/ filter · R reload"),
            // Alone (Narrow), the list says what the reader would.
            Some(Ok(_)) => (
                if workspace.page == 1 {
                    "No issues match this view"
                } else {
                    "Nothing matches this view"
                },
                if workspace.page == 1 {
                    "n New issue · / filter"
                } else {
                    "/ filter"
                },
            ),
        };
        let y = y + 1;
        frame
            .grid
            .put_text_clipped(x + 1, y, headline, bold(GitRole::Text, PANE), right);
        if !hint.is_empty() {
            frame
                .grid
                .put_text_clipped(x + 1, y + 1, hint, st(GitRole::Muted, PANE), right);
        }
        if focused {
            frame.cursor = Some((x + 1, y, SetCursorStyle::SteadyBlock));
        }
        return;
    }
    let room = bottom.saturating_sub(y);
    // A blank between records only while every record still fits with it: a taller pane
    // never shows fewer records than a shorter one.
    let record_rows = if record_rows == 3 && entries.len() * 3 > usize::from(room) {
        2
    } else {
        record_rows
    };
    let visible = usize::from(room / record_rows).max(1);
    let cursor = if selected >= entries.len() {
        0
    } else {
        selected
    };
    let offset = cursor.saturating_sub(visible.saturating_sub(1));
    let now = records::now();
    // GitHub's row: the title row ends in fixed columns, the CI mark (pull
    // requests), the linked pull requests or issues, the comment count, so each column's
    // marks line up down the list and every title clips at one column.
    let shown_records = entries
        .iter()
        .skip(offset)
        .take(visible)
        .filter_map(|entry| match entry {
            Entry::Record(value) if !value["dustyGone"].is_string() => {
                let mut record = ItemRecord::from_row(value);
                workspace.fresh.rows.overlay(&mut record);
                Some(record)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if pull {
        // The rows on screen are the ones whose check marks are asked for (`row_checks`).
        workspace
            .fresh
            .rows
            .show(shown_records.iter().map(|record| record.number).collect());
    }
    let linked_cell = shown_records
        .iter()
        .filter(|record| record.linked > 0)
        .map(|record| {
            glyph(
                if pull {
                    Glyph::Issue
                } else {
                    Glyph::PullRequest
                },
                mode,
            )
            .width()
                + 1
                + record.linked.to_string().len()
        })
        .max()
        .unwrap_or(0);
    let comment_cell = shown_records
        .iter()
        .filter(|record| record.comments > 0)
        .map(|record| {
            glyph(Glyph::Comment, mode).width()
                + 1
                + record.comments.to_string().len()
                + usize::from(record.comments_capped)
        })
        .max()
        .unwrap_or(0);
    // The subtitle's rung for the whole list: the fullest form every shown row fits in.
    let meta_room = usize::from(right.saturating_sub(x + 4));
    let rung = (0..META_RUNGS)
        .find(|rung| {
            shown_records.iter().all(|record| {
                let ladder = meta_ladder(workspace, record, pull, now, mode, PANE);
                usize::from(chrome::spans_width(&ladder[*rung])) <= meta_room
            })
        })
        .unwrap_or(META_RUNGS - 1);
    for (index, entry) in entries.iter().enumerate().skip(offset).take(visible) {
        let top = y + (index - offset) as u16 * record_rows;
        if top >= bottom {
            break;
        }
        let value = match entry {
            Entry::Divider(divider) => {
                let text = format!(" {divider} ");
                for column in x..right {
                    frame
                        .grid
                        .set_cell(column, top, "─", st(GitRole::Border, PANE));
                }
                frame
                    .grid
                    .put_text_clipped(x + 2, top, &text, st(GitRole::Muted, PANE), right);
                continue;
            }
            Entry::Record(value) => *value,
        };
        let is_selected = index == selected;
        let surface = if is_selected {
            if focused {
                GitSurface::Cursor
            } else {
                GitSurface::CursorInactive
            }
        } else {
            PANE
        };
        let rows = if compact { 1 } else { 2 };
        if is_selected {
            chrome::fill(
                &mut frame.grid,
                Rect::new(inner.x, top, inner.w, rows.min(bottom - top)),
                st(GitRole::Text, surface),
            );
            // One selection idiom in every list: the accent edge in the gutter
            // on each of the record's rows, as in Notifications, the runs and the reader.
            chrome::selection_edge(
                &mut frame.grid,
                inner.x,
                top..top + rows.min(bottom - top),
                focused,
                surface,
                mode,
            );
            if focused {
                frame.cursor = Some((inner.x, top, SetCursorStyle::SteadyBar));
            }
        }
        let mut record = ItemRecord::from_row(value);
        workspace.fresh.rows.overlay(&mut record);
        let gone = value["dustyGone"].as_str();
        let flashed = flash.contains(&workspace.row_identity(value));
        let (mark, tone) = if gone.is_some() {
            ("✕", GitRole::Muted)
        } else {
            lifecycle_mark(record.lifecycle, pull, mode)
        };
        let chip = match (record.lifecycle, gone) {
            (_, Some(_)) => None,
            (Lifecycle::Draft, _) => Some(("Draft", GitChip::Muted, GitRole::Muted)),
            (Lifecycle::Merged, _) => Some(("Merged", GitChip::Ref, GitRole::Ref)),
            (Lifecycle::Closed, _) if pull => Some(("Closed", GitChip::Removed, GitRole::Removed)),
            (Lifecycle::Closed, _) => Some(("Closed", GitChip::Ref, GitRole::Ref)),
            (Lifecycle::ClosedNotPlanned, _) => {
                Some(("Not planned", GitChip::Muted, GitRole::Muted))
            }
            (Lifecycle::Open, _) => None,
        };
        let title_style = Style::Git {
            role: if gone.is_some() {
                GitRole::Muted
            } else if flashed {
                GitRole::Modified
            } else {
                GitRole::Text
            },
            surface,
            emphasis: Emphasis::Bold,
        };
        let x1 = x + 2;
        if compact {
            // One row (Narrow): glyph, number, title, then the review and CI marks and the
            // comment count; the state chip at the end.
            let mut spans: Vec<Span> = vec![(format!("{mark} "), st(tone, surface))];
            if workspace.inbox.enabled
                && let Some(repo) = &record.repo
            {
                spans.push((
                    format!("{} ", chrome::ellipsize(repo, 18)),
                    st(GitRole::Muted, surface),
                ));
            }
            spans.push((
                format!("{}{} ", workspace.sigil(pull), record.number),
                st(GitRole::Muted, surface),
            ));
            let chip_width = chip.map_or(0, |(text, ..)| chrome::width(text) + 3);
            let mut tail: Vec<Span> = Vec::new();
            if gone.is_none() {
                if let Some(review) = record
                    .review_fact(None)
                    .map(|(state, _)| state)
                    .or(record.review)
                {
                    let (mark, _, role) = pr::review_mark(review, mode);
                    tail.push((format!(" {mark}"), st(role, surface)));
                }
                if let Some(ci) = record.ci() {
                    let (mark, _, role) = pr::ci_mark(ci, mode);
                    tail.push((format!(" {mark}"), st(role, surface)));
                }
                if record.comments > 0 {
                    tail.push((
                        format!(" {} {}", glyph(Glyph::Comment, mode), record.comments),
                        st(GitRole::Muted, surface),
                    ));
                }
            }
            let before = chrome::spans_width(&spans);
            let tail_width = chrome::spans_width(&tail);
            let title_room =
                usize::from(right.saturating_sub(x1 + before + chip_width + tail_width + 1));
            spans.push((
                chrome::ellipsize(
                    &prose::title(&record.title, record.lifecycle == Lifecycle::Draft),
                    title_room,
                ),
                title_style,
            ));
            put(&mut frame.grid, x1, top, &spans, right);
            if !tail.is_empty() {
                put(
                    &mut frame.grid,
                    right.saturating_sub(tail_width + chip_width),
                    top,
                    &tail,
                    right,
                );
            }
            if let Some((text, hue, role)) = chip {
                let at = right.saturating_sub(chip_width - 1);
                frame.grid.put_text_clipped(
                    at,
                    top,
                    &format!(" {text} "),
                    st(role, GitSurface::Chip(hue)),
                    right,
                );
            }
            continue;
        }
        // Row 1: the state glyph, the title, its label chips, then the fixed columns.
        let mut columns: Vec<Span> = Vec::new();
        if gone.is_none() {
            if pull {
                // The CI state never drops: a failing check shows at every width.
                columns.push(match record.ci() {
                    Some(ci) => {
                        let (mark, _, role) = pr::ci_mark(ci, mode);
                        (format!(" {mark}"), st(role, surface))
                    }
                    None => ("  ".into(), st(GitRole::Muted, surface)),
                });
            }
            // An issue row with no counts gives the gutter to its title and label chips
            //; a pull request's keeps it so its CI marks line up.
            let counted = pull || record.linked > 0 || record.comments > 0;
            if linked_cell > 0 && counted {
                let text = if record.linked > 0 {
                    format!(
                        "{} {}",
                        glyph(
                            if pull {
                                Glyph::Issue
                            } else {
                                Glyph::PullRequest
                            },
                            mode
                        ),
                        record.linked
                    )
                } else {
                    String::new()
                };
                columns.push((
                    format!("  {text:<linked_cell$}"),
                    st(GitRole::Muted, surface),
                ));
            }
            if comment_cell > 0 && counted {
                let text = if record.comments > 0 {
                    format!(
                        "{} {}{}",
                        glyph(Glyph::Comment, mode),
                        record.comments,
                        if record.comments_capped { "+" } else { "" }
                    )
                } else {
                    String::new()
                };
                columns.push((
                    format!("  {text:>comment_cell$}"),
                    st(GitRole::Muted, surface),
                ));
            }
        }
        let columns_width = chrome::spans_width(&columns);
        let mut spans: Vec<Span> = vec![(format!("{mark} "), st(tone, surface))];
        let before = chrome::spans_width(&spans);
        let room = usize::from(right.saturating_sub(x1 + before + columns_width + 1));
        let title = prose::title(&record.title, record.lifecycle == Lifecycle::Draft);
        let chips = if gone.is_none() {
            prose::chips(&record.labels, record.lifecycle == Lifecycle::Draft)
        } else {
            Vec::new()
        };
        // The title keeps at least three fifths of its room; the label chips follow it in what
        // is left, whole or not at all, as on github.com.
        let title_width = title.width();
        let labels = if chips.is_empty() || title_width + 2 >= room {
            Vec::new()
        } else {
            let keep = title_width.min(room * 3 / 5);
            label_spans(&chips, room.saturating_sub(keep + 1), surface)
        };
        let labels_width = usize::from(chrome::spans_width(&labels));
        let title_room = if labels_width > 0 {
            room.saturating_sub(labels_width + 1)
        } else {
            room
        };
        spans.push((chrome::ellipsize(&title, title_room), title_style));
        let after = put(&mut frame.grid, x1, top, &spans, right);
        if labels_width > 0 {
            put(&mut frame.grid, after + 1, top, &labels, right);
        }
        if columns_width > 0 {
            put(
                &mut frame.grid,
                right.saturating_sub(columns_width),
                top,
                &columns,
                right,
            );
        }
        if top + 1 >= bottom {
            continue;
        }
        // Row 2: `#13 opened 3h ago by maya · ◌ Review requested`, the state chip at its end.
        let meta_x = x1 + 2;
        let chip_width = chip.map_or(0, |(text, ..)| chrome::width(text) + 2);
        let room_beside_chip = usize::from(
            right.saturating_sub(meta_x + if chip_width > 0 { chip_width + 1 } else { 0 }),
        );
        let sigil = (
            format!(
                "{}{}{}",
                record
                    .repo
                    .as_deref()
                    .filter(|_| workspace.inbox.enabled)
                    .unwrap_or(""),
                workspace.sigil(pull),
                record.number
            ),
            st(GitRole::Muted, surface),
        );
        let meta: Vec<Span> = if let Some(gone) = gone {
            vec![
                sigil,
                (
                    format!(" · {gone} · gone from this view on refresh"),
                    st(GitRole::Muted, surface),
                ),
            ]
        } else {
            // One grammar down the list: every row takes the list's rung.
            meta_ladder(workspace, &record, pull, now, mode, surface).swap_remove(rung)
        };
        let meta_width = usize::from(chrome::spans_width(&meta));
        let chip = chip.filter(|_| meta_width <= room_beside_chip);
        let state_width = if chip.is_some() { chip_width } else { 0 };
        let meta_right = if state_width > 0 {
            right.saturating_sub(state_width + 1)
        } else {
            right
        };
        let meta = fit_meta(meta, usize::from(meta_right.saturating_sub(meta_x)));
        put(&mut frame.grid, meta_x, top + 1, &meta, meta_right);
        if let Some((text, hue, role)) = chip {
            let at = right.saturating_sub(state_width);
            frame.grid.put_text_clipped(
                at,
                top + 1,
                &format!(" {text} "),
                st(role, GitSurface::Chip(hue)),
                right,
            );
        }
    }
    // The last row: how much of the view is here.
    let count = entries
        .iter()
        .filter(|entry| matches!(entry, Entry::Record(_)))
        .count();
    let mut footer: Vec<Span> = vec![(
        if workspace.value(&workspace.list_key()).is_some() {
            format!(
                "Showing {count}{}",
                if workspace.inbox.enabled {
                    " indexed"
                } else {
                    ""
                }
            )
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
        footer.push((
            format!(
                " · {}–{}",
                offset + 1,
                (offset + visible).min(entries.len())
            ),
            st(GitRole::Muted, PANE),
        ));
    }
    if !workspace.inbox.enabled && !workspace.repo.is_empty() {
        // The forge repository the list reads, which may differ from the folder's name;
        // right-aligned when there is room.
        let used: u16 = footer.iter().map(|(text, _)| chrome::width(text)).sum();
        let repo = chrome::width(&workspace.repo);
        if x + used + repo + 3 <= right {
            frame.grid.put_text_clipped(
                right - repo,
                bottom,
                &workspace.repo,
                st(GitRole::Muted, PANE),
                right,
            );
        }
    }
    put(&mut frame.grid, x, bottom, &footer, right);
}

/// A list row's subtitle in `room` cells, shortened by whole facts and never cut mid-word:
/// the review's trailing count (` · 1 pending`) goes first, then the verdict keeps only its
/// mark (`◔`), then the verdict goes; only a subtitle still too wide ends in `…`.
fn fit_meta(mut meta: Vec<Span>, room: usize) -> Vec<Span> {
    let wide = |meta: &[Span]| usize::from(chrome::spans_width(meta)) > room;
    // The number, the separator and the author: the subtitle's core (`meta_ladder`).
    let review = meta.iter().position(|(text, _)| text == " · ");
    while wide(&meta)
        && let Some(at) = review
        && meta.len() > at + 1
    {
        let last = meta.len() - 1;
        let mark = meta[last]
            .0
            .split_once(' ')
            .map(|(mark, _)| mark.to_owned());
        match mark {
            // The verdict (`✓ 1 approval`), the first span after the separator: its mark alone.
            Some(mark) if last == at + 1 && !mark.is_empty() && !meta[last].0.starts_with(' ') => {
                meta[last].0 = mark
            }
            _ => {
                meta.pop();
                if meta.len() == at + 1 {
                    meta.pop();
                }
            }
        }
    }
    if wide(&meta) {
        // Fill what fits, then one `…` in the last span's style.
        let mut used = 0;
        let mut fitted = Vec::new();
        for (text, style) in meta {
            let width = usize::from(chrome::width(&text));
            if used + width < room {
                used += width;
                fitted.push((text, style));
            } else {
                fitted.push((chrome::ellipsize(&text, room - used), style));
                break;
            }
        }
        return fitted;
    }
    meta
}

/// The rungs of a list row's subtitle, fullest first; the list drops facts in this order as
/// it narrows: the age with its `opened`, then `by`. The number, the
/// author and the review state stay on every rung.
const META_RUNGS: usize = 3;

fn meta_ladder(
    workspace: &Workspace,
    record: &ItemRecord,
    pull: bool,
    now: i64,
    mode: ExplorerIconMode,
    surface: GitSurface,
) -> Vec<Vec<Span>> {
    let sigil = (
        format!(
            "{}{}{}",
            record
                .repo
                .as_deref()
                .filter(|_| workspace.inbox.enabled)
                .unwrap_or(""),
            workspace.sigil(pull),
            record.number
        ),
        st(GitRole::Muted, surface),
    );
    // When it was opened, or for a closed or merged item when it was closed.
    let (verb, stamp) = match record.lifecycle {
        Lifecycle::Merged => ("was merged", record.updated.as_str()),
        Lifecycle::Closed | Lifecycle::ClosedNotPlanned => ("was closed", record.updated.as_str()),
        _ if !record.created.is_empty() => ("opened", record.created.as_str()),
        _ => ("updated", record.updated.as_str()),
    };
    let when = fresh::relative(stamp, now);
    let review = if pull {
        workspace.review_spans(record, mode, surface)
    } else {
        None
    };
    let author = (record.author.clone(), st(GitRole::Author, surface));
    let muted = |text: &str| (text.to_owned(), st(GitRole::Muted, surface));
    let open = !matches!(
        record.lifecycle,
        Lifecycle::Merged | Lifecycle::Closed | Lifecycle::ClosedNotPlanned
    );
    let ladder: [Vec<Span>; META_RUNGS] = [
        if when.is_empty() {
            vec![sigil.clone(), muted(" by "), author.clone()]
        } else if open && verb == "opened" {
            vec![
                sigil.clone(),
                muted(&format!(" {verb} {when} by ")),
                author.clone(),
            ]
        } else if verb == "updated" {
            // The author opened it; someone else may have updated it (R16 gitlab-03).
            vec![
                sigil.clone(),
                muted(" by "),
                author.clone(),
                muted(&format!(" · updated {when}")),
            ]
        } else {
            vec![
                sigil.clone(),
                muted(" by "),
                author.clone(),
                muted(&format!(" {verb} {when}")),
            ]
        },
        vec![sigil.clone(), muted(" by "), author.clone()],
        vec![sigil.clone(), muted(" "), author],
    ];
    ladder
        .into_iter()
        .map(|mut spans| {
            if let Some(review) = &review {
                spans.push((" · ".into(), st(GitRole::Muted, surface)));
                spans.extend(review.iter().cloned());
            }
            spans
        })
        .collect()
}

/// The search row's query: qualifier names muted, their values and free words in text, cut
/// with `…` when the row is short.
fn query_spans(query: &str, room: usize) -> Vec<Span> {
    let query = chrome::ellipsize(query, room);
    let mut spans: Vec<Span> = Vec::new();
    for (index, word) in query.split(' ').enumerate() {
        if index > 0 {
            spans.push((" ".into(), st(GitRole::Text, PANE)));
        }
        match word.split_once(':') {
            Some((name, value)) if !name.is_empty() => {
                spans.push((format!("{name}:"), st(GitRole::Muted, PANE)));
                spans.push((value.to_owned(), st(GitRole::Text, PANE)));
            }
            _ => spans.push((word.to_owned(), st(GitRole::Text, PANE))),
        }
    }
    spans
}

/// The filter menus GitHub puts above its lists, in its order.
pub(super) const FACETS: [(&str, &str); 6] = [
    ("Author", "author"),
    ("Label", "label"),
    ("Projects", "project"),
    ("Milestones", "milestone"),
    ("Assignee", "assignee"),
    ("Sort", "sort"),
];

/// `● 8 Open  ✓ 30 Closed` on the left, the active one bright; the filter menus on the right
/// (`Author ▾  Label ▾ …`), each one in use in the accent, or `Filters ▾` when they do not all
/// fit. A count shows once that state's list was read (one list read per visit).
fn state_row(
    frame: &mut Frame,
    workspace: &Workspace,
    x: u16,
    y: u16,
    right: u16,
    mode: ExplorerIconMode,
) {
    let pull = workspace.page == 2;
    let at = workspace.page.saturating_sub(1).min(1);
    let state = workspace.states[at].as_str();
    // The query's own `is:open`/`is:closed` says which state shows when the state is `all`.
    let query = workspace.queries[at].split_whitespace().collect::<Vec<_>>();
    let (open_on, closed_on) = match state {
        "open" => (true, false),
        "closed" | "merged" => (false, true),
        _ => (
            query.contains(&"is:open"),
            query
                .iter()
                .any(|word| matches!(*word, "is:closed" | "is:merged")),
        ),
    };
    let (open_mark, _) = lifecycle_mark(Lifecycle::Open, pull, mode);
    let mut spans: Vec<Span> = Vec::new();
    // GitLab lists merge requests as Open, Merged and Closed (R16 gitlab-05); GitHub's
    // Closed holds the merged ones.
    let mut states = vec![("Open", open_mark, open_on)];
    if pull && workspace.terms().pr_singular.contains("merge") {
        states.push(("Merged", glyph(Glyph::PullMerged, mode), state == "merged"));
        states.push(("Closed", glyph(Glyph::IssueClosed, mode), state == "closed"));
    } else {
        states.push(("Closed", glyph(Glyph::IssueClosed, mode), closed_on));
    }
    for (index, (name, mark, on)) in states.into_iter().enumerate() {
        if index > 0 {
            spans.push(("  ".into(), st(GitRole::Text, PANE)));
        }
        let count = workspace
            .state_count(&name.to_ascii_lowercase())
            .map(|count| format!("{count} "))
            .unwrap_or_default();
        if on {
            // The active state is a filled chip, as the active section is.
            let chip = bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus));
            spans.push((format!(" {mark} {count}{name} "), chip));
        } else {
            spans.push((format!("{mark} "), st(GitRole::Muted, PANE)));
            spans.push((format!("{count}{name}"), st(GitRole::Muted, PANE)));
        }
    }
    let after = put(&mut frame.grid, x, y, &spans, right);
    let used = workspace.facets_in_use();
    let menu = |name: &str, on: bool| -> Vec<Span> {
        vec![(
            format!("{name} ▾"),
            if on {
                st(GitRole::Focus, PANE)
            } else {
                st(GitRole::Muted, PANE)
            },
        )]
    };
    let mut menus: Vec<Span> = Vec::new();
    for (index, (name, qualifier)) in FACETS.iter().enumerate() {
        if index > 0 {
            menus.push(("  ".into(), st(GitRole::Text, PANE)));
        }
        menus.extend(menu(name, used.contains(qualifier)));
    }
    let menus = if after + 2 + chrome::spans_width(&menus) <= right {
        menus
    } else {
        menu("Filters", !used.is_empty())
    };
    let w = chrome::spans_width(&menus);
    if after + 2 + w <= right {
        put(&mut frame.grid, right - w, y, &menus, right);
    }
}

/// `Checks 3`, `Checks 0`, or the bare name. One rule on every forge, as GitHub's tabs
/// (R16 prs-06, gitea-01): a count shows once it is known, zero included, so a tab never
/// loses its number between the list and the reader; only a count not yet loaded is the
/// name alone.
fn counted(name: &str, count: Option<usize>) -> String {
    match count {
        Some(count) => format!("{name} {count}"),
        None => name.to_owned(),
    }
}

/// GitHub's tabs of the pull request reader, with counts. An issue has none: its
/// Conversation is the whole page.
fn reader_tabs(
    workspace: &Workspace,
    record: Option<&DetailRecord>,
    with_counts: bool,
) -> (Vec<String>, usize) {
    let id = workspace.number();
    let counted = |name: &str, count: Option<usize>| counted(name, count.filter(|_| with_counts));
    let counts = record
        .map(|record| pr::counts(workspace, id, record))
        .unwrap_or_default();
    let names = [
        "Conversation",
        "Commits",
        workspace.ci_words().0,
        "Files changed",
    ];
    let tabs = names
        .iter()
        .zip(counts)
        .map(|(name, count)| counted(name, count))
        .collect::<Vec<_>>();
    (tabs, workspace.pr_tab.min(pr::CYCLE - 1))
}

fn reader(
    frame: &mut Frame,
    workspace: &mut Workspace,
    rect: Rect,
    reply_rows: u16,
    mode: ExplorerIconMode,
) {
    let focused = workspace.focus == 2;
    let pull = workspace.page == 2;
    let id = workspace.number();
    let value = workspace.item().or_else(|| workspace.list_item()).cloned();
    let record = value.as_ref().map(DetailRecord::from_value);
    let kind = if pull {
        workspace.terms().pr_singular
    } else {
        "issue"
    };
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
        // At most two rows at every width, so the state chip and the tabs move at most once;
        // the rest is cut at a word with one `…` (r11 gitea-01: never mid-word, never a
        // doubled space where the wrap joined).
        let most = 2;
        let mut lines = lines
            .into_iter()
            .map(|line| line.trim_end().to_owned())
            .collect::<Vec<_>>();
        if lines.len() > most {
            let rest = lines[most - 1..]
                .iter()
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" ");
            lines.truncate(most - 1);
            lines.push(word_cut(&rest, width.saturating_sub(indent)));
        }
        for (index, line) in lines.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if index == 0 {
                spans.push((format!("{mark} "), bold(tone, PANE)));
                spans.push((
                    format!("{}{} ", workspace.sigil(pull), record.row.number),
                    st(GitRole::Muted, PANE),
                ));
            } else {
                spans.push((" ".repeat(indent), st(GitRole::Text, PANE)));
            }
            spans.push((line.clone(), bold(GitRole::Text, PANE)));
            put(&mut frame.grid, x, y, &spans, right);
            y += 1;
        }
        // State chips and facts.
        let (text, hue, role) = state_chip(record.row.lifecycle, pull, mode);
        let mut at = frame.grid.put_text_clipped(
            x,
            y,
            &format!(" {text} "),
            bold(role, GitSurface::Chip(hue)),
            right,
        );
        let now = records::now();
        // GitHub's sentence under the title: `maya wants to merge 2 commits into main
        // from fix/backoff`, GitLab's `requested to merge`, an issue's `opened this issue`.
        // A narrow reader takes the first shorter form that fits, never half a word.
        let room = usize::from(right.saturating_sub(at)).saturating_sub(2);
        let muted = |text: &str| (text.to_owned(), st(GitRole::Muted, PANE));
        let author = (record.row.author.clone(), st(GitRole::Author, PANE));
        let mut forms: Vec<Vec<Span>> = Vec::new();
        if pull {
            let commits = record.commits.len();
            let count = if commits > 0 {
                format!("{commits} commit{} ", if commits == 1 { "" } else { "s" })
            } else {
                String::new()
            };
            let (base, head) = (record.base.clone(), record.head.clone());
            let merged = record.row.lifecycle == Lifecycle::Merged;
            let gitlab = workspace.terms().pr_singular.contains("merge");
            let phrase = |count: &str, head: &str| {
                let mut sentence = vec![author.clone()];
                if !base.is_empty() && !head.is_empty() {
                    if gitlab {
                        sentence.push(muted(if merged {
                            " merged "
                        } else {
                            " requested to merge "
                        }));
                        sentence.push((head.to_owned(), st(GitRole::Ref, PANE)));
                        sentence.push(muted(" into "));
                        sentence.push((base.clone(), st(GitRole::Ref, PANE)));
                    } else {
                        sentence.push(muted(if merged {
                            " merged "
                        } else {
                            " wants to merge "
                        }));
                        if !count.is_empty() {
                            sentence.push((count.to_owned(), st(GitRole::Text, PANE)));
                        }
                        sentence.push(muted("into "));
                        sentence.push((base.clone(), st(GitRole::Ref, PANE)));
                        sentence.push(muted(" from "));
                        sentence.push((head.to_owned(), st(GitRole::Ref, PANE)));
                    }
                }
                sentence
            };
            let sentence = phrase(&count, &head);
            // `updated` is the newest thing on the page: the forge's stamp, or a later comment,
            // review, event or commit the timeline shows (R16 prs-07).
            let timeline = workspace
                .value(&Key::Timeline(id))
                .map(records::timeline)
                .unwrap_or_default();
            let newest = std::iter::once(record.row.updated.as_str())
                .chain(timeline.iter().map(records::TimelineEntry::at))
                .chain(
                    record
                        .reviews
                        .iter()
                        .map(|review| review.submitted.as_str()),
                )
                .chain(
                    record
                        .commits
                        .iter()
                        .map(|commit| commit.committed.as_str()),
                )
                .filter_map(|stamp| records::epoch(stamp).map(|at| (at, stamp)))
                .max_by_key(|(at, _)| *at)
                .map_or(record.row.updated.as_str(), |(_, stamp)| stamp);
            let when = fresh::relative(newest, now);
            if !when.is_empty() {
                let mut dated = sentence.clone();
                dated.push(muted(&format!(" · updated {when}")));
                forms.push(dated);
            }
            forms.push(sentence);
            // GitHub's sentence at every width (R16 gitea-04): a narrower reader drops the
            // commit count, then cuts a long head branch from its start, before it falls
            // back to the bare branches.
            if !base.is_empty() && !head.is_empty() {
                let cells =
                    |spans: &[Span]| 2 + spans.iter().map(|(text, _)| text.width()).sum::<usize>();
                if !count.is_empty() {
                    forms.push(phrase("", &head));
                }
                for count in [count.as_str(), ""] {
                    let keep = head
                        .width()
                        .saturating_sub(cells(&phrase(count, &head)).saturating_sub(room));
                    if (8..head.width()).contains(&keep) {
                        forms.push(phrase(count, &chrome::ellipsize_start(&head, keep)));
                    }
                }
            }
            if !base.is_empty() && !head.is_empty() {
                let short = vec![
                    author.clone(),
                    muted(" · "),
                    (base.clone(), st(GitRole::Muted, PANE)),
                    muted(" ← "),
                    (head.clone(), st(GitRole::Ref, PANE)),
                ];
                forms.push(short);
                // Last: the branches alone, a long head cut from its start.
                let keep = room.saturating_sub(base.width() + 3).max(8);
                forms.push(vec![
                    (base.clone(), st(GitRole::Muted, PANE)),
                    muted(" ← "),
                    (chrome::ellipsize_start(&head, keep), st(GitRole::Ref, PANE)),
                ]);
            }
            forms.push(vec![author.clone()]);
        } else {
            let when = fresh::relative(&record.created, now);
            let comments = issue::discussion_count(workspace, id, record);
            let opened = if when.is_empty() {
                " opened this issue".to_owned()
            } else {
                format!(" opened this issue {when}")
            };
            let mut sentence = vec![author.clone(), muted(&opened)];
            if comments > 0 {
                let mut counted = sentence.clone();
                counted.push(muted(&format!(
                    " · {comments} comment{}",
                    if comments == 1 { "" } else { "s" }
                )));
                forms.push(counted);
            }
            forms.push(std::mem::take(&mut sentence));
            if !when.is_empty() {
                forms.push(vec![author.clone(), muted(&format!(" · {when}"))]);
            }
            forms.push(vec![author.clone()]);
        }
        let size = |spans: &[Span]| 2 + spans.iter().map(|(text, _)| text.width()).sum::<usize>();
        let chosen = forms
            .iter()
            .position(|form| size(form) <= room)
            .unwrap_or(forms.len() - 1);
        let mut facts: Vec<Span> = vec![("  ".into(), st(GitRole::Text, PANE))];
        facts.extend(forms.swap_remove(chosen));
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
                frame
                    .grid
                    .put_text_clipped(right - w, y, text, st(role, PANE), right);
            }
        }
        y += 1;
    } else if workspace.repo.is_empty() || id == 0 {
        // Nothing selected: the body explains.
    } else {
        frame.grid.put_text_clipped(
            x,
            y,
            &format!("{}{id}", workspace.sigil(pull)),
            bold(GitRole::Muted, PANE),
            right,
        );
        y += 2;
    }
    if record.is_some() && !pull {
        // An issue is one page: a rule under the header, no tabs.
        for column in inner.x..inner.right() {
            frame
                .grid
                .set_cell(column, y, "─", st(GitRole::Border, PANE));
        }
        y += 1;
    } else if record.is_some() {
        // One terminal width decides the counts for every reader and every tab (r11 prs-05,
        // ISS-11): below 110 columns no reader shows them; from there they show if they fit
        // the split reader, so a reader whose list is hidden (`Space e`) keeps or drops
        // them exactly as it does beside the list.
        let terminal = frame.grid.width();
        let split = terminal
            .saturating_sub(split_list_width(
                workspace,
                terminal,
                layout::mode(terminal, frame.grid.height()),
            ))
            .saturating_sub(2);
        let (counted_tabs, active) = reader_tabs(workspace, record.as_ref(), terminal >= 110);
        let tabs = if chrome::tabs_fit(&counted_tabs, inner.w.min(split)) {
            counted_tabs
        } else {
            reader_tabs(workspace, record.as_ref(), false).0
        };
        // The tab row spans the whole interior: the first tab's pad is the gutter after the
        // border, and the blank after the last tab may run up to the border.
        // Every tab keeps its name at every width (r11: a bare `󰘭` for Merge read as no
        // label at all); under pressure the counts drop, all at once.
        let tabs = chrome::fit_tabs(&tabs, active, inner.w);
        y += 1;
        chrome::sub_tabs(
            &mut frame.grid,
            inner.x,
            y,
            inner.right(),
            &tabs,
            active,
            focused,
        );
        y += 1;
        for column in inner.x..inner.right() {
            frame
                .grid
                .set_cell(column, y, "─", st(GitRole::Border, PANE));
        }
        y += 1;
    }
    let body = Rect::new(
        x,
        y,
        right.saturating_sub(x),
        inner.bottom().saturating_sub(y + reply_rows),
    );
    body_rows(frame, workspace, body, mode);
}

/// The reader's rows for the selected issue or pull request; find searches the same rows.
pub(super) fn record_rows(
    workspace: &Workspace,
    width: usize,
    mode: ExplorerIconMode,
) -> Vec<Line> {
    let mut rows = if let Some((host, hints)) = workspace.sign_in_hints() {
        let host = if host.is_empty() {
            "This forge".to_owned()
        } else {
            host
        };
        let mut rows = vec![
            super::prose::Rich::new()
                .push(
                    format!("{host} requires sign-in"),
                    super::prose::strong(GitRole::Modified),
                )
                .line(),
            super::prose::blank(),
            super::prose::muted("Anonymous visitors see nothing here,"),
            super::prose::muted("not even public repositories."),
            super::prose::blank(),
            super::prose::Rich::new()
                .push("Sign in with", super::prose::strong(GitRole::Text))
                .line(),
        ];
        for hint in &hints {
            rows.push(
                super::prose::Rich::new()
                    .push("  $ ", super::prose::role(GitRole::Muted))
                    .push(
                        crate::forge::cli::redact(hint),
                        super::prose::role(GitRole::Link),
                    )
                    .line(),
            );
        }
        if let Some(
            crate::forge::provider::ProviderKind::Gitea
            | crate::forge::provider::ProviderKind::Forgejo,
        ) = workspace.forge_kind()
        {
            rows.push(super::prose::muted(&format!(
                "or make a token at https://{host}/user/settings/applications"
            )));
        }
        rows.push(super::prose::blank());
        rows.push(super::prose::muted("then R to retry · @ accounts"));
        rows
    } else if workspace.repo.is_empty() {
        let mut rows = Vec::new();
        lines::error_or_loading(workspace, &Key::Repository, &mut rows);
        rows
    } else if let Some(Err(error)) = workspace.cache.get(&workspace.list_key()) {
        // In words with one key: never the forge's own text.
        let words = workspace.explain(error);
        vec![
            super::prose::Rich::new()
                .push(&words.headline, super::prose::strong(GitRole::Modified))
                .line(),
            super::prose::blank(),
            super::prose::muted(&words.hint),
        ]
    } else if workspace.live_filter() && workspace.number() == 0 {
        let what = if workspace.page == 1 {
            "issues"
        } else {
            workspace.terms().pr_plural
        };
        vec![super::prose::muted(&format!(
            "No {} match this filter",
            what.to_ascii_lowercase()
        ))]
    } else if workspace.page == 1 {
        issue::rows(workspace, width, mode)
    } else {
        pr::rows(workspace, width, mode)
    };
    workspace.mark_refused(&mut rows);
    for row in &mut rows {
        // Diff rows scroll sideways; everything else is wrapped already.
        if row.diff.is_none()
            && row.gutter.is_empty()
            && row.source.is_none()
            && row.paint.is_empty()
            && row.band.is_none()
        {
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
    // Reading shows the text without a row cursor; the first key brings it back.
    let cursor_shown = focused && !workspace.reading;
    let (left, content_right) = (body.x, body.right());
    let width = usize::from(body.w);
    workspace.detail_width = width;
    workspace.icons = mode;
    // The row under the cursor, by its action and which occurrence of it it is.
    let selected_action = if workspace.page == 1 {
        let at = workspace.detail_cursor;
        workspace
            .detail_rows
            .get(at)
            .and_then(|row| row.action.clone())
            .map(|action| {
                (
                    action,
                    super::lines::action_ordinal(&workspace.detail_rows, at),
                    at,
                )
            })
    } else {
        None
    };
    // Checks: the job under the cursor is the one shown beside the list.
    workspace.follow_check();
    // A Narrow terminal shows the reader without the list: there the file tree and an open
    // commit's list fold into the reader (`f` picks a file), as the list itself has.
    let terminal = layout::mode(frame.grid.width(), frame.grid.height());
    workspace.reading_position.narrow_pr = terminal == Mode::Narrow;
    // Commits beside their list open on the newest, as Files changed opens on its first file.
    if workspace.page == 2
        && workspace.pr_tab == pr::COMMITS
        && workspace.selected_commit.is_none()
        && super::files::outline_beside(workspace, width)
        && let Some(newest) = workspace
            .value(&Key::Item(2, workspace.number()))
            .and_then(|value| {
                DetailRecord::from_value(value)
                    .commits
                    .last()
                    .map(|commit| commit.sha.clone())
            })
    {
        workspace.selected_commit = Some(newest);
        workspace.ensure(false);
    }
    let rows = record_rows(workspace, width, mode);
    if workspace.page == 1 {
        let identity = (
            workspace.host.clone(),
            workspace.repo.clone(),
            workspace.list_key(),
            workspace.number(),
        );
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
    // Checks opens on its selected job, once the pull request is read.
    if workspace.reveal_file
        && workspace.page == 2
        && workspace.pr_tab == pr::CHECKS
        && let Some(index) = workspace.selected_check_row(&rows)
    {
        workspace.detail_cursor = index;
        workspace.detail_scroll = 0;
        workspace.reveal_file = false;
    }
    if let Some((action, ordinal, near)) = selected_action.as_ref()
        && let Some(index) = super::lines::action_row(&rows, action, *ordinal, *near)
    {
        workspace.detail_cursor = index;
    }
    // Commits opens on its first commit, not the day heading above it.
    if workspace.page == 2
        && workspace.pr_tab == super::pr::COMMITS
        && workspace.detail_cursor == 0
        && rows.first().is_some_and(|row| row.action.is_none())
        && let Some(index) = workspace
            .selected_commit
            .clone()
            .and_then(|sha| {
                let open = Some(Action::Commit(sha));
                rows.iter().position(|row| row.action == open)
            })
            .or_else(|| rows.iter().position(|row| row.action.is_some()))
    {
        workspace.detail_cursor = index;
    }
    if workspace.reveal_merge
        && let Some(index) = super::conversation::merge_stop(&rows)
    {
        workspace.detail_cursor = index;
        workspace.reveal_merge = false;
    }
    workspace.sync_detail_find(&rows);
    // A wrapped diff line is one cursor stop at its first row, except where find put the
    // cursor on the row its match is in.
    let cursor = workspace.detail_cursor.min(rows.len().saturating_sub(1));
    workspace.detail_cursor = if workspace.detail_find.search.active() {
        cursor
    } else {
        super::files::line_start(&rows, cursor)
    };
    // The cursor band covers every row of a wrapped diff line.
    let cursor_start = super::files::line_start(&rows, workspace.detail_cursor);
    let cursor_end = super::files::line_end(&rows, workspace.detail_cursor);
    let active_thread = if cursor_shown {
        rows.get(workspace.detail_cursor)
            .and_then(|row| row.reply.as_ref())
    } else {
        None
    };
    let show_quiet = rows
        .get(workspace.detail_cursor)
        .is_some_and(|row| row.quiet_review);
    let shown = |row: &Line| {
        (!row.quiet_review || show_quiet)
            && (!row.thread_action || row.reply.as_ref() == active_thread)
    };
    let split_outline = width >= OUTLINE_MIN && rows.iter().any(|row| row.outline);
    let outline_right = if split_outline {
        left + outline_fit(width, &rows) as u16
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
    workspace.detail_visible = visible;
    // The Conversation's metadata sidebar stays beside the timeline, still while
    // the timeline scrolls, as GitHub's does.
    let sidebar = rows.iter().any(|row| row.side);
    let side_left = content_right.saturating_sub(super::conversation::sidebar_width(width) as u16);
    let content_right = if sidebar {
        side_left.saturating_sub(3)
    } else {
        content_right
    };
    let main = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            (shown(row) && !row.side && !(split_outline && row.outline) && !pinned.contains(&i))
                .then_some(i)
        })
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
        // A Conversation card the cursor steps onto comes in from its top, so `j` reads a
        // long card from its header.
        let card = super::conversation::showing(workspace) && rows[workspace.detail_cursor].card;
        if position < workspace.detail_scroll {
            workspace.detail_scroll = position.saturating_sub(usize::from(card));
        } else if card && position + margin >= workspace.detail_scroll + visible {
            workspace.detail_scroll = position.saturating_sub(1);
        } else if position + margin >= workspace.detail_scroll + visible {
            workspace.detail_scroll = (position + margin + 1).saturating_sub(visible);
        }
    }
    if std::mem::take(&mut workspace.reveal_top)
        && let Some(position) = main.iter().position(|i| *i == workspace.detail_cursor)
    {
        // A thread `n` jumped to starts the view, one row of context above it.
        workspace.detail_scroll = position.saturating_sub(1);
    }
    workspace.detail_scroll = workspace
        .detail_scroll
        .min(main.len().saturating_sub(visible));
    let outline_offset = outline
        .iter()
        .position(|i| *i == workspace.detail_cursor)
        .unwrap_or(0)
        .saturating_sub(visible - 1);
    let diff_left = if split_outline {
        outline_right + 2
    } else {
        left
    };
    if !pinned.is_empty() {
        for column in diff_left.saturating_sub(1)..content_right + 1 {
            frame
                .grid
                .set_cell(column, start - 1, "─", st(GitRole::Border, PANE));
        }
    }
    if split_outline {
        for y in body.y..payload_bottom {
            frame
                .grid
                .set_cell(outline_right, y, "│", st(GitRole::Border, PANE));
        }
    }
    let mut positions = vec![None; rows.len()];
    for (position, &index) in pinned.iter().enumerate() {
        positions[index] = Some((diff_left, content_right, body.y + position as u16));
    }
    if split_outline {
        for (position, &index) in outline
            .iter()
            .skip(outline_offset)
            .take(visible + pinned.len() + 1)
            .enumerate()
        {
            positions[index] = Some((left, outline_right - 1, body.y + position as u16));
        }
    }
    for (position, &index) in main
        .iter()
        .skip(workspace.detail_scroll)
        .take(visible)
        .enumerate()
    {
        positions[index] = Some((diff_left, content_right, start + position as u16));
    }
    if sidebar {
        let side = rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.side.then_some(i));
        for (position, index) in side.take(visible).enumerate() {
            positions[index] = Some((side_left, body.right(), start + position as u16));
        }
    }
    // Reveal a find match horizontally inside panned rows.
    if let Some((x, right, _)) = positions.get(workspace.detail_cursor).copied().flatten()
        && let Some(row) = rows.get(workspace.detail_cursor)
        && row.pan
        && let Some(byte) = workspace.detail_find.cursor_byte(workspace.detail_cursor)
    {
        let available = usize::from(right.saturating_sub(x))
            .saturating_sub(row.gutter.width() + 1)
            .max(2);
        let (mut column, mut first, mut end) = (0, None, 0);
        let (matches, offset) = workspace.detail_find.row_matches(workspace.detail_cursor);
        let found = matches
            .iter()
            .find(|found| found.bytes.start == byte + offset);
        let bytes = found.map_or(byte..byte + 1, |found| {
            byte..found.bytes.end.saturating_sub(offset)
        });
        for (at, grapheme) in row.text.grapheme_indices(true) {
            let size = if grapheme == "\t" {
                4 - column % 4
            } else {
                grapheme.width().max(1)
            };
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
    // The focused row runs past the pane (or is panned): the border says how to see the rest,
    // where the inline `wide · h l scroll →` used to sit in the prose (R16).
    let mut wide_focus = false;
    for (i, (row, position)) in rows.iter().zip(positions).enumerate() {
        let Some((x, right, y)) = position else {
            continue;
        };
        if cursor_shown
            && i == workspace.detail_cursor
            && row.pan
            && !(split_outline && row.outline)
            && !pinned.contains(&i)
        {
            let room = usize::from(right.saturating_sub(x));
            wide_focus = workspace.horizontal > 0 || row.gutter.width() + row.text.width() > room;
        }
        let selected = cursor_shown && (cursor_start..=cursor_end).contains(&i);
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
            if added {
                GitSurface::AddedBand
            } else {
                GitSurface::RemovedBand
            }
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
            st(
                if added {
                    GitRole::Added
                } else {
                    GitRole::Removed
                },
                surface,
            )
        } else if row.action.is_some() && row.paint.is_empty() && row.spans.is_empty() {
            // Legacy buttons (`› Review changes`) read as links.
            st(
                if row.outline {
                    GitRole::Text
                } else {
                    GitRole::Link
                },
                surface,
            )
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
        let gutter_style = if row.diff.is_some() {
            st(GitRole::Muted, surface)
        } else {
            gutter_style
        };
        let mut x = frame
            .grid
            .put_text_clipped(x, y, &row.gutter, gutter_style, right);
        let content_x = x.min(right.saturating_sub(1));
        let mut cursor_x = None;
        let horizontal = if !row.pan || (split_outline && row.outline) || pinned.contains(&i) {
            0
        } else {
            workspace.horizontal
        };
        let cursor_byte = (selected && i == workspace.detail_cursor)
            .then(|| workspace.detail_find.cursor_byte(i))
            .flatten();
        let (matches, offset) = workspace.detail_find.row_matches(i);
        let mut column = 0;
        let graphemes = row.text.grapheme_indices(true).collect::<Vec<_>>();
        for (index, (at, g)) in graphemes.iter().enumerate() {
            let (at, g) = (*at, *g);
            let size = if g == "\t" {
                4 - column % 4
            } else {
                g.width().max(1)
            };
            let end = column + size;
            if end > horizontal {
                // Byte-range bands (a row's own backgrounds) unless the cursor is on the row.
                let cell = if selected || active_file {
                    surface
                } else {
                    row.bands
                        .iter()
                        .find(|(range, _)| range.contains(&at))
                        .map_or(surface, |(_, band)| *band)
                };
                let style = if let Some((_, style)) = row
                    .paint
                    .iter()
                    .rev()
                    .find(|(range, _)| range.contains(&at))
                {
                    on(*style, cell)
                } else if let Some((_, highlight)) =
                    row.spans.iter().find(|(range, _)| range.contains(&at))
                {
                    highlight_style(*highlight, cell)
                } else {
                    on(base, cell)
                };
                let candidate = matches.partition_point(|found| found.bytes.end <= at + offset);
                let style = if matches
                    .get(candidate)
                    .is_some_and(|found| found.bytes.start < at + offset + g.len())
                {
                    Style::Search
                } else {
                    style
                };
                let last = index + 1 == graphemes.len();
                // A panned row's `›` keeps a blank before it, so it never reads as part of
                // the text; a wrapped row's `…` ends the text.
                let marker = if row.pan { 2 } else { 1 };
                // A row whose whole rest fits ends in its own text, with no marker.
                let rest_fits = || {
                    let rest: usize = graphemes[index..]
                        .iter()
                        .map(|(_, g)| if *g == "\t" { 4 } else { g.width().max(1) })
                        .sum();
                    usize::from(x) + rest <= usize::from(right)
                };
                let fits = x + size as u16 <= right
                    && (last || x + (size as u16) + marker <= right || rest_fits());
                if !fits {
                    if row.pan {
                        frame.grid.set_cell(
                            right.saturating_sub(2),
                            y,
                            " ",
                            st(GitRole::Muted, surface),
                        );
                    }
                    frame.grid.set_cell(
                        right.saturating_sub(1),
                        y,
                        if row.pan { "›" } else { "…" },
                        st(GitRole::Muted, surface),
                    );
                    break;
                }
                let next = if g == "\t" || column < horizontal {
                    frame.grid.put_text_clipped(
                        x,
                        y,
                        &" ".repeat(end - column.max(horizontal)),
                        style,
                        right,
                    )
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
            // Each row of a wrapped line carries the mark; the cursor sits on the cursor's row.
            match cursor_x.filter(|_| cursor_byte.is_some()) {
                Some(at) => frame.cursor = Some((at, y, SetCursorStyle::SteadyBlock)),
                None => {
                    let gutter = row_x.saturating_sub(1).max(left.saturating_sub(1));
                    frame
                        .grid
                        .set_cell(gutter, y, "▎", bold(GitRole::Focus, surface));
                    // The terminal's own cursor is a thin bar at the text's first cell.
                    if i == workspace.detail_cursor {
                        frame.cursor = Some((content_x, y, SetCursorStyle::SteadyBar));
                    }
                }
            }
        }
    }
    if cursor_shown && frame.cursor.is_none() {
        frame.cursor = Some((left, body.y, SetCursorStyle::SteadyBlock));
    }
    // A scroll position when the body overflows, and the sideways keys when the focused row
    // does.
    let tall = main.len() > visible;
    if (tall || wide_focus) && body.bottom() > body.y {
        let pan = match (wide_focus, workspace.horizontal > 0) {
            (false, _) => "",
            (true, false) => "wide · h l ›",
            (true, true) => "‹ h l ›",
        };
        // Lines of the reader, not items of the list: said in words so the two counts are
        // never read as one.
        let last = (workspace.detail_scroll + visible).min(main.len());
        let lines = tall.then(|| {
            format!(
                "lines {}–{last} of {}{}",
                workspace.detail_scroll + 1,
                main.len(),
                if last < main.len() { " ↓" } else { "" }
            )
        });
        let label = match (lines, pan.is_empty()) {
            (Some(lines), true) => format!(" {lines} "),
            (Some(lines), false) => format!(" {pan} · {lines} "),
            (None, _) => format!(" {pan} "),
        };
        let w = chrome::width(&label);
        let y = body.bottom();
        if y > 0 && content_right > left + w {
            frame.grid.put_text_clipped(
                content_right + 1 - w,
                y,
                &label,
                st(GitRole::Muted, PANE),
                content_right + 1,
            );
        }
    }
    if workspace.page == 1 {
        workspace
            .reading_position
            .finish(&rows, workspace.detail_cursor, workspace.detail_scroll);
    } else {
        workspace.reading_position.finish_scoped(
            &rows,
            workspace.detail_cursor,
            workspace.detail_scroll,
            main,
        );
    }
    workspace.detail_rows = rows;
}

/// The reply box pinned to the bottom of the reader.
fn reply_box(
    frame: &mut Frame,
    workspace: &Workspace,
    form: &super::super::Form,
    rect: Rect,
    rows: u16,
) {
    let inner = rect.inner();
    let top = inner.bottom().saturating_sub(rows);
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    chrome::fill(
        &mut frame.grid,
        Rect::new(inner.x, top, inner.w, rows),
        st(GitRole::Text, PANE),
    );
    for column in inner.x..inner.right() {
        frame
            .grid
            .set_cell(column, top, "─", st(GitRole::Border, PANE));
    }
    let login =
        crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&workspace.host))
            .and_then(|account| account.login);
    // `Reply to @alex · as maya`: whose thread this answers, and who is writing.
    let target = form
        .title
        .split(" · ")
        .next()
        .filter(|first| first.starts_with("Reply to "));
    let label = match (&form.action, login) {
        (FormAction::Github(Action::Reply { .. }), Some(login)) => {
            format!("{} · as {login}", target.unwrap_or("Reply in thread"))
        }
        (FormAction::Github(Action::Reply { .. }), None) => {
            target.unwrap_or("Reply in thread").to_owned()
        }
        (_, Some(login)) => format!("Comment as {login}"),
        (_, None) => "Comment".to_owned(),
    };
    frame
        .grid
        .put_text_clipped(x, top + 1, &label, bold(GitRole::Text, PANE), right);
    let field = &form.fields[0];
    let box_rect = Rect::new(x, top + 2, right.saturating_sub(x), rows.saturating_sub(4));
    // A framed box (╭╮╰╯) around the input.
    let edge = st(GitRole::Focus, PANE);
    let (bx, by, bw, bh) = (box_rect.x, box_rect.y, box_rect.w, box_rect.h);
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
            frame.grid.set_cell(
                inside.x - 1,
                row,
                " ",
                st(GitRole::Text, GitSurface::CursorInactive),
            );
        }
    }
    let hint_y = top + rows - 1;
    // The send and Esc keys are in the footer, once.
    frame.grid.put_text_clipped(
        x,
        hint_y,
        "Markdown supported",
        st(GitRole::Muted, PANE),
        right,
    );
}

fn footer(app: &GitScreen, frame: &mut Frame, rect: Rect, filter: bool) {
    let workspace = &app.github;
    let grid = &mut frame.grid;
    if let Some(form) = &app.form {
        let several = form.fields.len() > 1;
        // A sheet prints its own submit, Tab and Esc keys beside its buttons; the footer
        // adds only what the sheet does not say, so no key is listed twice.
        let hints: Vec<(&str, &str)> = if filter {
            // The list narrows (and the forge is searched) as the query is typed; Enter
            // keeps it as the list's filter.
            vec![
                ("Enter", "apply filter"),
                ("Tab", "state"),
                ("Esc", "clear"),
            ]
        } else if super::forms::owns(form) {
            // Review and merge: the send key stays in the footer with the field keys.
            let multiline = form.fields.iter().any(|field| field.multiline);
            let merge = matches!(&form.action, FormAction::Github(Action::Merge(..)));
            vec![
                (
                    if multiline { "Ctrl+Enter" } else { "Enter" },
                    if merge { "merge" } else { "submit review" },
                ),
                ("Esc", "continue editing"),
                ("Tab", "next field"),
                ("Shift+Tab", "previous"),
            ]
        } else if matches!(
            &form.action,
            FormAction::Github(Action::Comment(..) | Action::Reply { .. })
        ) {
            // The composer: Esc leaves the text in place for the next `c`.
            vec![("Ctrl+Enter", "submit"), ("Esc", "keep draft")]
        } else {
            // A sheet: its keys live here, not on the sheet.
            let multiline = form.fields.iter().any(|field| field.multiline);
            // The submit key says what the sheet's button says (`Review →`: review).
            let verb = match &form.action {
                FormAction::Github(action) => super::create::submit_verb(action),
                _ => None,
            };
            let mut keys = vec![
                (
                    if multiline { "Ctrl+Enter" } else { "Enter" },
                    verb.unwrap_or("confirm"),
                ),
                ("Esc", "cancel"),
            ];
            // A focused choice row (picker or radio) says `↑/↓ choose`; the footer has the
            // same key, and only while that row has focus: a text field takes
            // ↑/↓ as its own.
            // A focused checkbox flips with Space: it is no list to choose from.
            match form.fields.get(form.selected) {
                Some(field) if super::super::shell::sheet::is_toggle(field) => {
                    keys.push(("Space", "toggle"))
                }
                Some(field) if !field.multiline && !field.choices.is_empty() => {
                    keys.push(("↑/↓", "choose"))
                }
                _ => {}
            }
            if several {
                keys.extend([("Tab", "next field"), ("Shift+Tab", "previous")]);
            }
            keys
        };
        let hints = hints.as_slice();
        let message = (!app.notice.is_empty()).then_some((app.notice.as_str(), true));
        // The live filter names what it filters (`Filter issues`) where the query would go.
        let context = if filter {
            form.title.split(" · ").next().unwrap_or_default()
        } else {
            ""
        };
        chrome::footer(grid, rect, hints, context, message);
        return;
    }
    let find = (workspace.focus == 2 && workspace.detail_find.search.active()).then(|| {
        workspace
            .detail_find
            .footer_status(workspace.detail_cursor, rect.w)
    });
    let refresh = if workspace.page < 3
        && !workspace.fresh.implicit.contains(&workspace.list_key())
        && workspace.pending(&workspace.list_key())
        && workspace
            .cache
            .get(&workspace.list_key())
            .is_some_and(Result::is_ok)
    {
        if workspace.loading_more() {
            "Loading more…"
        } else {
            "Refreshing…"
        }
    } else if workspace.page < 3 && workspace.number() != 0 && {
        let item = Key::Item(workspace.page, workspace.number());
        workspace.item().is_some()
            && workspace.pending(&item)
            && !workspace.fresh.implicit.contains(&item)
    } {
        "Refreshing details… · showing what was loaded"
    } else {
        ""
    };
    let message = if workspace.writing() {
        Some(("Sending…", false))
    } else if !app.notice.is_empty() {
        Some((app.notice.as_str(), true))
    } else if !workspace.notice.is_empty() {
        Some((workspace.notice.as_str(), workspace.notice_error))
    } else {
        None
    };
    // The shared key table (`keymap.rs`): `Space e` hides the list; a pressed leader shows
    // the keys that finish its chord instead of the page's keys.
    let keys = workspace.key_table();
    let sidebar = keys
        .keys(super::keymap::Named::ToggleSidebar)
        .unwrap_or_default();
    let forge_page = matches!(workspace.page, 1 | 2);
    if forge_page && workspace.leader_pending() {
        // Each chord whole (`Space e sidebar`), as Actions shows it, never its tail beside a
        // lone `Space …` at the far edge (R16 gitea-03, gitlab-02, notif-06).
        let chords = keys
            .after_leader()
            .into_iter()
            .map(|(key, label)| (format!("{} {key}", keys.leader_name()), label))
            .collect::<Vec<_>>();
        let mut hints = chords
            .iter()
            .map(|(key, label)| (key.as_str(), *label))
            .collect::<Vec<_>>();
        hints.push(("Esc", "cancel"));
        chrome::footer(grid, rect, &hints, "", message);
        return;
    }
    let enter = match workspace
        .detail_rows
        .get(workspace.detail_cursor)
        .and_then(|row| row.action.as_ref())
    {
        Some(Action::Reply { .. }) => "reply",
        Some(Action::Resolve(_, true)) => "resolve",
        Some(Action::Resolve(_, false)) => "unresolve",
        Some(Action::Files(_)) => "choose file",
        Some(Action::Collapse(..)) => "fold",
        Some(Action::Inline { .. }) => "comment",
        Some(Action::Metadata(_, _, name)) => *name,
        Some(Action::PrTab(_)) => "show",
        Some(Action::Merge(..)) => "merge",
        Some(Action::MergeMethod(_)) => "change",
        Some(Action::Review(_)) => "review",
        Some(Action::CommitSuggestion(..)) => "commit",
        Some(Action::Develop(_)) => "create branch",
        Some(Action::Run(_)) if workspace.terms().run_noun == "pipeline" => "open pipeline",
        Some(Action::Run(_)) => "open run",
        Some(_) => "open",
        None => "",
    };
    let reader = workspace.focus == 2;
    // While reading nothing is selected: the first hint says how to reach the rows.
    let (enter_key, enter) = if reader && workspace.reading {
        ("j k", "select")
    } else {
        ("Enter", enter)
    };
    // An empty list offers what can fill it, nothing that needs a row.
    let empty = forge_page && workspace.number() == 0;
    // A list that could not load offers only what can fix it; the reader says why.
    let failed = matches!(workspace.page, 1 | 2)
        && !workspace.inbox.enabled
        && workspace.items().is_empty()
        && {
            let key = workspace.list_key();
            workspace.sign_in_hints().is_some()
                || matches!(workspace.cache.get(&key), Some(Err(_)))
                || (!workspace.cache.contains_key(&key) && workspace.blocked(&key).is_some())
        };
    // `@` says what the reader's hint says it does.
    let at = match workspace.cache.get(&workspace.list_key()) {
        _ if workspace.read_only() => "",
        _ if workspace.sign_in_hints().is_some() => "sign in",
        Some(Err(error)) if workspace.explain(error).hint.contains("@ sign in") => "sign in",
        // Before the list, the reader explains the repository's error (record_rows).
        None if workspace.repo.is_empty()
            && matches!(workspace.cache.get(&Key::Repository), Some(Err(error)) if workspace.explain(error).hint.contains("@ sign in")) =>
        {
            "sign in"
        }
        None if workspace
            .blocked(&workspace.list_key())
            .is_some_and(|(_, hint)| hint.contains("@ sign in")) =>
        {
            "sign in"
        }
        _ => "accounts",
    };
    let new_item = if workspace.page == 1 { "New issue" } else { "" };
    // A closed issue offers `X reopen`, never `x close` again (R16 gitea-08).
    let closed = workspace
        .item()
        .or_else(|| workspace.list_item())
        .is_some_and(|value| {
            matches!(
                ItemRecord::from_row(value).lifecycle,
                Lifecycle::Closed | Lifecycle::Merged
            )
        });
    let close = if closed {
        ("X", "reopen")
    } else {
        ("x", "close")
    };
    let hints: Vec<(&str, &str)> = match (workspace.page, reader) {
        // Signed out, the read-only rule below adds `@ sign in`.
        (_, _) if failed && empty => vec![("R", "retry"), ("@", at)],
        (_, true) if empty => vec![("Esc", "list"), ("n", new_item), ("/", "filter")],
        (_, false) if empty => vec![("n", new_item), ("/", "filter"), ("[ ]", "sections")],
        // `n New issue` in every Issues list footer, rows or none (R16 issues-02).
        (1, false) => vec![
            ("Enter", "open"),
            ("n", "New issue"),
            ("C", "comment"),
            close,
            ("l", "labels"),
            ("/", "filter"),
        ],
        // The reader's way back stays visible on narrow terminals: Esc comes before edits.
        // The Conversation: cards, folds and the thread keys.
        // An open issue keeps its triage keys (R16 issues-09): close, labels, assignees,
        // milestone, as the list and the help sheet offer them; the row's width decides.
        (1, true) => vec![
            ("j/k", "cards"),
            ("C", "comment"),
            ("Esc", "list"),
            close,
            ("l", "labels"),
            ("a", "assignees"),
            ("m", "milestone"),
            ("Enter", "fold"),
            ("e", "edit"),
            ("o", "open"),
        ],
        (_, false) => vec![
            ("Enter", "open"),
            ("r", "review"),
            ("c", "checkout"),
            ("w", "worktree"),
            ("m", "merge"),
            ("/", "filter"),
        ],
        (_, true) if workspace.pr_tab == pr::FILES => {
            // Only the keys that apply to what is shown: `w` once there is a diff (named for
            // what it does next), `v` once the forge has said whether the file is viewed, `f`
            // once there is a file to pick. `[ ] tabs` on every reader tab, and
            // `f`/`v` here only, not again above the diff.
            let shown = |wanted: fn(&Action) -> bool| {
                workspace
                    .detail_rows
                    .iter()
                    .any(|row| row.action.as_ref().is_some_and(wanted))
            };
            let wrap = match (
                shown(|action| matches!(action, Action::Inline { .. })),
                workspace.diff_nowrap,
            ) {
                (false, _) => "",
                (true, true) => "wrap",
                (true, false) => "no wrap",
            };
            let viewed = if shown(|action| matches!(action, Action::Viewed(..))) {
                "viewed"
            } else {
                ""
            };
            let files = if shown(|action| matches!(action, Action::Files(_))) {
                "files"
            } else {
                ""
            };
            // Esc drops an open range first (`drafts.rs`), then goes to the list.
            // While a range is open its own keys lead: extend it, comment on it.
            if workspace.review.anchor.is_some() {
                vec![
                    ("j/k", "extend"),
                    ("C", "comment"),
                    ("Esc", "cancel range"),
                    ("r", "review"),
                    ("w", wrap),
                    ("[ ]", "tabs"),
                ]
            } else {
                // `e` opens the line under the cursor in the editor once there is a diff.
                let editor = if wrap.is_empty() { "" } else { "editor" };
                vec![
                    (enter_key, enter),
                    ("[ ]", "tabs"),
                    ("r", "review"),
                    ("w", wrap),
                    ("v", viewed),
                    ("f", files),
                    ("e", editor),
                    ("Esc", "list"),
                    ("m", "merge"),
                ]
            }
        }
        // On the Merge tab `m` is where you are; it is not offered again.
        // Checks: Enter (or `o`) on a job opens its run; on a step it shows or hides the
        // step's log; elsewhere it does nothing, and the footer offers it nowhere else.
        (_, true) if workspace.pr_tab == pr::CHECKS => {
            let (key, label) = if workspace.reading {
                (enter_key, enter)
            } else {
                ("Enter", workspace.check_enter())
            };
            vec![
                (key, label),
                ("[ ]", "tabs"),
                ("r", "review"),
                ("C", "comment"),
                ("Esc", "list"),
                ("m", "merge"),
            ]
        }
        // In the merge card its own keys lead: Enter merges, Tab changes the
        // method. `[ ] tabs` comes early on every tab, so a short footer keeps it.
        (_, true)
            if workspace.pr_tab == pr::CONVERSATION
                && !workspace.reading
                && super::conversation::in_merge_card(
                    &workspace.detail_rows,
                    workspace.detail_cursor,
                ) =>
        {
            let on_control = matches!(
                workspace
                    .detail_rows
                    .get(workspace.detail_cursor)
                    .and_then(|row| row.action.as_ref()),
                Some(Action::Merge(..) | Action::MergeMethod(_))
            );
            let method =
                if on_control && workspace.merge_methods().len() > 1 && !workspace.read_only() {
                    "method"
                } else {
                    ""
                };
            // While a gate blocks it there is no merge to offer: Enter goes to the first
            // reason instead (R16 prs-04).
            let mergeable = workspace
                .detail_rows
                .iter()
                .any(|row| matches!(row.action, Some(Action::Merge(..))));
            let enter = match workspace
                .detail_rows
                .get(workspace.detail_cursor)
                .and_then(|row| row.action.as_ref())
            {
                Some(Action::MergeMethod(_)) => "change",
                Some(Action::Merge(..)) => "merge",
                None if mergeable => "merge",
                // The same rows Enter jumps to (conversation.rs): a numbered reason.
                None if workspace
                    .detail_rows
                    .iter()
                    .skip_while(|row| !row.merge_box)
                    .any(|row| {
                        row.text.starts_with("  1. ") || row.text.starts_with("    1. ")
                    }) =>
                {
                    "see why"
                }
                None => "",
                Some(_) => enter,
            };
            vec![
                ("Enter", enter),
                ("Tab", method),
                ("[ ]", "tabs"),
                ("C", "comment"),
                ("r", "review"),
                ("Esc", "list"),
            ]
        }
        (_, true) if workspace.pr_tab == pr::CONVERSATION => {
            let open = workspace.detail_rows.iter().any(|row| row.unresolved);
            vec![
                ("j/k", "cards"),
                ("Enter", "fold"),
                ("C", "comment"),
                ("[ ]", "tabs"),
                ("n/N", if open { "threads" } else { "" }),
                ("r", "review"),
                ("m", "merge"),
                ("Esc", "list"),
            ]
        }
        (_, true) => vec![
            (enter_key, enter),
            ("[ ]", "tabs"),
            ("r", "review"),
            ("C", "comment"),
            ("Esc", "list"),
            ("m", "merge"),
        ],
    };
    let mut hints = hints
        .into_iter()
        .filter(|(_, label)| !label.is_empty())
        .collect::<Vec<_>>();
    // A reader opened from Notifications: Esc goes back there, and that way
    // back comes third, so the footer's six never drop it (the Conversation has seven).
    if reader
        && workspace.notify.back
        && let Some(at) = hints.iter().position(|hint| *hint == ("Esc", "list"))
    {
        hints.remove(at);
        hints.insert(2.min(hints.len()), ("Esc", "back"));
    }
    if workspace.read_only() {
        // Signed out: write keys leave the footer; pressing one still says why. The accounts
        // popover (`@`) shows how to sign in.
        const WRITES: [&str; 15] = [
            "review",
            "comment",
            "edit",
            "close",
            "reopen",
            "labels",
            "assignees",
            "milestone",
            "viewed",
            "reply",
            "resolve",
            "merge",
            "re-run",
            "dispatch",
            "New issue",
        ];
        // `cancel` is also every form's Esc: only the Actions key goes.
        hints.retain(|(key, label)| !WRITES.contains(label) && (*key, *label) != ("x", "cancel"));
        if !reader {
            hints.push(("@", "sign in"));
        }
    }
    // Where the list can hide: a narrow terminal shows one pane at a time anyway.
    if forge_page
        && !sidebar.is_empty()
        && layout::mode(grid.width(), grid.height()) != Mode::Narrow
    {
        // In a pull request's Conversation it follows `[ ] tabs` on every card, so it never
        // comes and goes with the cursor (R16 prs-11); elsewhere it trails, after the tab's
        // own keys.
        match hints.iter().position(|hint| hint.1 == "tabs") {
            Some(at) if reader && workspace.page == 2 && workspace.pr_tab == pr::CONVERSATION => {
                hints.insert(at + 1, (sidebar.as_str(), "sidebar"))
            }
            _ => hints.push((sidebar.as_str(), "sidebar")),
        }
    }
    if find.is_some() {
        // While a find is active its keys lead; the rest follow as room allows.
        hints.splice(0..0, [("n/N", "next match"), ("Esc", "clear find")]);
        hints.retain(|(key, label)| !(*key == "Esc" && *label != "clear find"));
    }
    // A standing query is decoration beside the keys, so they outrank it; a
    // find position or a refresh in progress keeps its room.
    let decorative = find.is_none() && refresh.is_empty();
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
    if decorative && reader && workspace.page == 1 {
        chrome::footer_keys_first_fill(grid, rect, &hints, &context, message);
    } else if decorative {
        chrome::footer_keys_first(grid, rect, &hints, &context, message);
    } else {
        chrome::footer(grid, rect, &hints, &context, message);
    }
}

/// `text` cut to `cells` at the last word that fits, with one `…`; a single word longer
/// than the room is cut where it must be.
fn word_cut(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    let cut = chrome::ellipsize(text, cells);
    let body = cut.trim_end_matches('…');
    match body.rfind(' ') {
        Some(at) if at > 0 && body[..at].trim_end().width() >= cells / 2 => {
            format!("{}…", body[..at].trim_end())
        }
        _ => cut,
    }
}
