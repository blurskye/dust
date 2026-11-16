//! Persistent chrome: top bar, operation banner, pane frames, sub-tabs,
//! chips, receipts and the one-row footer.
use super::glyphs::{Glyph, glyph};
use super::layout::Rect;
use crate::config::ExplorerIconMode;
use crate::ui::{CellGrid, Emphasis, GitChip, GitRole, GitSurface, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) const fn st(role: GitRole, surface: GitSurface) -> Style {
    Style::Git {
        role,
        surface,
        emphasis: Emphasis::None,
    }
}
pub(crate) const fn bold(role: GitRole, surface: GitSurface) -> Style {
    Style::Git {
        role,
        surface,
        emphasis: Emphasis::Bold,
    }
}

pub(crate) fn width(text: &str) -> u16 {
    text.width() as u16
}

/// Cut `text` to `cells` columns, ending in `…` when anything was dropped.
pub(crate) fn ellipsize(text: &str, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    if text.width() <= cells {
        return text.into();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        used += grapheme.width().max(1);
        if used >= cells {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

/// Keep the end of a path: `…/backoff`.
pub(crate) fn ellipsize_start(text: &str, cells: usize) -> String {
    if text.width() <= cells || cells == 0 {
        return if cells == 0 {
            String::new()
        } else {
            text.into()
        };
    }
    let graphemes = text.graphemes(true).collect::<Vec<_>>();
    let mut tail = Vec::new();
    let mut used = 1;
    for grapheme in graphemes.iter().rev() {
        used += grapheme.width().max(1);
        if used > cells {
            break;
        }
        tail.push(*grapheme);
    }
    tail.reverse();
    format!("…{}", tail.concat())
}

pub(crate) fn fill(grid: &mut CellGrid, rect: Rect, style: Style) {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            grid.set_cell(x, y, " ", style);
        }
    }
}

/// A run of styled text; returns the column after it.
pub(crate) type Span = (String, Style);

pub(crate) fn spans_width(spans: &[Span]) -> u16 {
    spans.iter().map(|(text, _)| width(text)).sum()
}

pub(crate) fn put_spans(
    grid: &mut CellGrid,
    mut x: u16,
    y: u16,
    spans: &[Span],
    right: u16,
) -> u16 {
    for (text, style) in spans {
        x = grid.put_text_clipped(x, y, text, *style, right);
    }
    x
}

/// The selection edge in a list's gutter, on each of the selected record's rows. Only the
/// focused pane draws it (R15 focus): one bar on screen says where `j`/`k` go, and an
/// unfocused pane's selection reads from its inactive band alone, as the Git rail does.
pub(crate) fn selection_edge(
    grid: &mut CellGrid,
    x: u16,
    rows: std::ops::Range<u16>,
    focused: bool,
    surface: GitSurface,
    mode: ExplorerIconMode,
) {
    if !focused {
        return;
    }
    for y in rows {
        grid.set_cell(
            x,
            y,
            glyph(Glyph::Cursor, mode),
            bold(GitRole::Focus, surface),
        );
    }
}

/// A bordered pane with `glyph title` on the top border and a muted count at its right end.
pub(crate) fn pane(grid: &mut CellGrid, rect: Rect, title: &[Span], count: &str, focused: bool) {
    if rect.w < 4 || rect.h < 2 {
        return;
    }
    let surface = GitSurface::Pane;
    fill(grid, rect, st(GitRole::Text, surface));
    let edge = st(
        if focused {
            GitRole::Focus
        } else {
            GitRole::Border
        },
        surface,
    );
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    for x in rect.x + 1..right {
        grid.set_cell(x, rect.y, "─", edge);
        grid.set_cell(x, bottom, "─", edge);
    }
    for y in rect.y + 1..bottom {
        grid.set_cell(rect.x, y, "│", edge);
        grid.set_cell(right, y, "│", edge);
    }
    grid.set_cell(rect.x, rect.y, "╭", edge);
    grid.set_cell(right, rect.y, "╮", edge);
    grid.set_cell(rect.x, bottom, "╰", edge);
    grid.set_cell(right, bottom, "╯", edge);
    let count_width = if count.is_empty() {
        0
    } else {
        width(count) + 2
    };
    let title_right = right.saturating_sub(1 + count_width);
    let mut x = grid.put_text_clipped(rect.x + 1, rect.y, " ", edge, title_right);
    for (index, (text, style)) in title.iter().enumerate() {
        // The first span is the title proper; it takes the focus colour.
        let style = if index <= 1 && focused {
            recolour(*style, GitRole::Focus)
        } else {
            *style
        };
        let room = usize::from(title_right.saturating_sub(x));
        x = grid.put_text_clipped(x, rect.y, &ellipsize(text, room), style, title_right);
    }
    grid.put_text_clipped(x, rect.y, " ", edge, title_right);
    if !count.is_empty() && x + count_width < right {
        let at = right - count_width;
        grid.put_text_clipped(
            at,
            rect.y,
            &format!(" {count} "),
            st(GitRole::Muted, surface),
            right,
        );
    }
}

fn recolour(style: Style, role: GitRole) -> Style {
    match style {
        Style::Git {
            surface, emphasis, ..
        } => Style::Git {
            role,
            surface,
            emphasis,
        },
        other => other,
    }
}

/// `Local 4 │ Remotes 3 │ Tags 2` on one interior row; the active tab is a filled chip.
pub(crate) fn sub_tabs(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    right: u16,
    tabs: &[String],
    active: usize,
    focused: bool,
) {
    let surface = GitSurface::Pane;
    let mut at = x;
    // Every tab keeps one space either side of its `│` at every width: a row
    // that is too wide loses its counts first (see [`fit_tabs`]), never its gutters.
    for (index, tab) in tabs.iter().enumerate() {
        // Every tab needs both pads, the last one too, so the row never touches the border
        //; a row fits or not whichever tab is active.
        let pad = 2;
        // A tab that does not fit whole is replaced by `…`, never cut mid-word.
        let need = width(tab) + pad + u16::from(index > 0);
        if at + need > right {
            if at + 2 <= right {
                grid.put_text_clipped(at, y, " …", st(GitRole::Muted, surface), right);
            }
            break;
        }
        if index > 0 {
            at = grid.put_text_clipped(at, y, "│", st(GitRole::Border, surface), right);
        }
        let style = if index == active {
            if focused {
                bold(GitRole::OnFocus, GitSurface::ChipActive)
            } else {
                bold(GitRole::Focus, GitSurface::Chip(GitChip::Focus))
            }
        } else {
            st(GitRole::Muted, surface)
        };
        let text = format!(" {tab} ");
        at = grid.put_text_clipped(at, y, &text, style, right);
    }
}

/// ` name ` per tab joined by `│`, as [`sub_tabs`] paints them, the last tab's trailing
/// pad included so the row keeps a gutter before the border. The same whichever tab is
/// active.
fn padded_width(tabs: &[String]) -> u16 {
    tabs.iter().map(|tab| width(tab) + 2).sum::<u16>() + tabs.len().saturating_sub(1) as u16
}

/// Whether `tabs` fit `room` whole: every name, count and gutter.
pub(crate) fn tabs_fit(tabs: &[String], room: u16) -> bool {
    padded_width(tabs) <= room
}

/// Tabs made to fit `room` columns. Names never change with width, and
/// counts are on every tab or on none: every count goes at once, and only then does the tail behind the active tab fold
/// into a counted `+2 more`, never a bare `…`.
pub(crate) fn fit_tabs(tabs: &[String], active: usize, room: u16) -> Vec<String> {
    fit_tabs_reserving(tabs, active, room, 0)
}

/// `Commits 2` → `Commits`; a name without a trailing count is kept whole.
fn without_count(tab: &str) -> String {
    match tab.rsplit_once(' ') {
        Some((name, count))
            if !name.is_empty()
                && !count.is_empty()
                && count
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '+' || c == '/') =>
        {
            name.to_owned()
        }
        _ => tab.to_owned(),
    }
}

/// [`fit_tabs`] keeping `reserve` columns free for tabs that come and go.
pub(crate) fn fit_tabs_reserving(
    tabs: &[String],
    active: usize,
    room: u16,
    reserve: u16,
) -> Vec<String> {
    let total = padded_width;
    let fits = |tabs: &[String]| total(tabs) + reserve <= room;
    if fits(tabs) {
        return tabs.to_vec();
    }
    let quiet = tabs
        .iter()
        .map(|tab| without_count(tab))
        .collect::<Vec<_>>();
    if total(&quiet) <= room {
        return quiet;
    }
    // Fold the tail behind the active tab into `+N more`.
    let mut kept = quiet.len();
    while kept > active + 1 && kept > 1 {
        kept -= 1;
        let mut candidate = quiet[..kept].to_vec();
        candidate.push(format!("+{} more", quiet.len() - kept));
        if total(&candidate) <= room {
            return candidate;
        }
    }
    quiet
}

/// A chip: one space of padding each side on a tinted surface.
pub(crate) fn chip(
    grid: &mut CellGrid,
    x: u16,
    y: u16,
    right: u16,
    text: &str,
    hue: GitChip,
) -> u16 {
    let role = match hue {
        GitChip::Focus => GitRole::Focus,
        GitChip::Added => GitRole::Added,
        GitChip::Removed => GitRole::Removed,
        GitChip::Modified => GitRole::Modified,
        GitChip::Ref => GitRole::Ref,
        GitChip::RemoteRef => GitRole::RemoteRef,
        GitChip::Tag => GitRole::Tag,
        GitChip::Muted => GitRole::Muted,
    };
    grid.put_text_clipped(
        x,
        y,
        &format!(" {text} "),
        st(role, GitSurface::Chip(hue)),
        right,
    )
}

/// The backdrop of a sheet over `keep`. Behind a sheet the screen's body is
/// cleared outright, pane borders and rules included, so no cut word, count chip or rule
/// stub reads as part of the sheet at any size. Only the top bar (where you are) and the
/// footer row (the screen's own hints, muted) stay.
pub(crate) fn scrim(grid: &mut CellGrid, keep: Rect) {
    let (width, height) = (grid.width(), grid.height());
    for y in 0..height {
        for x in 0..width {
            let margin =
                x + 1 >= keep.x && x <= keep.right() && y + 1 >= keep.y && y <= keep.bottom();
            if margin {
                grid.set_cell(x, y, " ", st(GitRole::Text, GitSurface::Canvas));
                continue;
            }
            // The top bar stays as it is: it says where you are.
            if y == 0 {
                continue;
            }
            let Some(cell) = grid.get(x, y) else { continue };
            if cell.continuation {
                continue;
            }
            if y + 1 == height {
                // The footer's keys go flat so the sheet is the only thing that reads as live.
                let symbol = cell.symbol.clone();
                grid.set_cell(x, y, &symbol, st(GitRole::Muted, GitSurface::Canvas));
            } else {
                grid.set_cell(x, y, " ", st(GitRole::Text, GitSurface::Canvas));
            }
        }
    }
}

/// The Git tab's backdrop for a centred sheet: `area` (the panes) recedes around `keep`,
/// the way a close or reopen sheet dims the forge: text keeps its place in the border
/// colour, and a one-cell margin around the sheet is cleared, so no panel word runs into
/// its edge (`Ta╭─`). The top bar, an operation banner, the tutor and the footer outside
/// `area` stay as they are.
pub(crate) fn scrim_within(grid: &mut CellGrid, keep: Rect, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let inside = x >= keep.x && x < keep.right() && y >= keep.y && y < keep.bottom();
            if inside {
                continue;
            }
            let margin =
                x + 1 >= keep.x && x <= keep.right() && y + 1 >= keep.y && y <= keep.bottom();
            let Some(cell) = grid.get(x, y) else { continue };
            if cell.continuation {
                continue;
            }
            let symbol = if margin {
                " ".to_owned()
            } else {
                cell.symbol.clone()
            };
            grid.set_cell(x, y, &symbol, st(GitRole::Border, GitSurface::Canvas));
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub glyph: Glyph,
    pub role: GitRole,
    pub label: &'static str,
    pub count: Option<String>,
    /// The count is a badge (unread), not a total: it stays when the bar drops its counts.
    pub badge: bool,
    /// Status marks after the count, in their own colours, kept at every width like a badge:
    /// the Actions tab's `✕ 2` failing runs and `●` while one runs.
    pub marks: Vec<(String, GitRole)>,
}

pub(crate) struct TopBar<'a> {
    pub repo: &'a str,
    pub head: &'a str,
    pub detached: bool,
    pub operation: bool,
    /// `None` when the branch has no upstream.
    pub divergence: Option<(u32, u32)>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub mode: ExplorerIconMode,
}

/// How far row 0 has shrunk. Tab names never change with width, and counts are on every
/// tab or on none: the wordmark goes first, then inactive icons, then the
/// repository and branch shorten, then every count goes at once, and only as a last resort
/// do inactive tabs fold to icon and count. The active tab keeps its name and chip, and its
/// icon goes with the others'.
#[derive(Clone, Copy)]
struct Squeeze {
    counts: bool,
    wordmark: bool,
    inactive_icons: bool,
    repo: Option<usize>,
    head: Option<usize>,
    inactive_labels: bool,
}

const SQUEEZES: [Squeeze; 7] = {
    const FULL: Squeeze = Squeeze {
        counts: true,
        wordmark: true,
        inactive_icons: true,
        repo: None,
        head: None,
        inactive_labels: true,
    };
    const PLAIN: Squeeze = Squeeze {
        wordmark: false,
        inactive_icons: false,
        ..FULL
    };
    [
        FULL,
        Squeeze {
            wordmark: false,
            ..FULL
        },
        PLAIN,
        Squeeze {
            repo: Some(12),
            ..PLAIN
        },
        Squeeze {
            repo: Some(12),
            head: Some(14),
            ..PLAIN
        },
        Squeeze {
            repo: Some(12),
            head: Some(14),
            counts: false,
            ..PLAIN
        },
        // Last resort: inactive tabs fold to icon and count.
        Squeeze {
            counts: true,
            wordmark: false,
            inactive_icons: true,
            repo: Some(12),
            head: Some(14),
            inactive_labels: false,
        },
    ]
};

/// Row 0. Shrinks in the order of [`Squeeze`] until it fits.
pub(crate) fn top_bar(grid: &mut CellGrid, width_cells: u16, bar: &TopBar<'_>) {
    let surface = GitSurface::TopBar;
    let base = st(GitRole::Text, surface);
    for x in 0..width_cells {
        grid.set_cell(x, 0, " ", base);
    }
    for (index, squeeze) in SQUEEZES.iter().enumerate() {
        let left = left_cluster(bar, *squeeze);
        let right = right_cluster(bar, *squeeze);
        let need = spans_width(&left) + spans_width(&right) + 3;
        if need <= width_cells || index == SQUEEZES.len() - 1 {
            put_spans(grid, 1, 0, &left, width_cells);
            let at = width_cells.saturating_sub(spans_width(&right) + 1);
            put_spans(grid, at.max(spans_width(&left) + 2), 0, &right, width_cells);
            return;
        }
    }
}

fn separator() -> Span {
    (" │ ".into(), st(GitRole::Border, GitSurface::TopBar))
}

fn left_cluster(bar: &TopBar<'_>, squeeze: Squeeze) -> Vec<Span> {
    let surface = GitSurface::TopBar;
    let mut spans = Vec::new();
    if squeeze.wordmark {
        spans.push(("DUSTY GIT".into(), bold(GitRole::Text, surface)));
        spans.push(separator());
    }
    let repo = match squeeze.repo {
        Some(room) => ellipsize(bar.repo, room),
        None => bar.repo.to_owned(),
    };
    spans.push((repo, st(GitRole::Text, surface)));
    spans.push(separator());
    let head = match squeeze.head {
        Some(room) => ellipsize_start(bar.head, room),
        None => bar.head.to_owned(),
    };
    spans.push((
        format!("{} ", glyph(Glyph::Branch, bar.mode)),
        st(GitRole::Ref, surface),
    ));
    spans.push((head, bold(GitRole::Ref, surface)));
    if bar.detached {
        spans.push((" (detached)".into(), st(GitRole::Muted, surface)));
    }
    if bar.operation {
        spans.push((
            format!(" {}", glyph(Glyph::Running, bar.mode)),
            bold(GitRole::Modified, surface),
        ));
    }
    spans.push(separator());
    match bar.divergence {
        Some((ahead, behind)) => {
            let quiet = ahead == 0 && behind == 0;
            let role = |live| if quiet { GitRole::Muted } else { live };
            spans.push((
                format!("{}{ahead}", glyph(Glyph::Ahead, bar.mode)),
                st(
                    role(if ahead > 0 {
                        GitRole::Added
                    } else {
                        GitRole::Muted
                    }),
                    surface,
                ),
            ));
            spans.push((" ".into(), st(GitRole::Text, surface)));
            spans.push((
                format!("{}{behind}", glyph(Glyph::Behind, bar.mode)),
                st(
                    role(if behind > 0 {
                        GitRole::Removed
                    } else {
                        GitRole::Muted
                    }),
                    surface,
                ),
            ));
        }
        None => spans.push(("· no upstream".into(), st(GitRole::Muted, surface))),
    }
    spans
}

fn right_cluster(bar: &TopBar<'_>, squeeze: Squeeze) -> Vec<Span> {
    let surface = GitSurface::TopBar;
    let mut spans = Vec::new();
    for (index, tab) in bar.tabs.iter().enumerate() {
        if index > 0 {
            spans.push(separator());
        }
        let mark = glyph(tab.glyph, bar.mode);
        let count = tab
            .count
            .as_deref()
            .filter(|_| squeeze.counts || tab.badge)
            .unwrap_or("");
        if index == bar.active {
            // One icon rule for every tab (r11 NOT-08, gitea-10): when the inactive icons go,
            // the active tab's goes too; its chip is the cue.
            let mark = if squeeze.inactive_icons || !squeeze.inactive_labels {
                mark
            } else {
                ""
            };
            let parts = [mark, tab.label, count]
                .into_iter()
                .chain(tab.marks.iter().map(|(text, _)| text.as_str()))
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            spans.push((
                format!(" {} ", parts.join(" ")),
                bold(GitRole::OnFocus, GitSurface::ChipActive),
            ));
            continue;
        }
        let mut parts: Vec<Span> = Vec::new();
        if squeeze.inactive_icons || !squeeze.inactive_labels {
            parts.push((mark.into(), st(tab.role, surface)));
        }
        if squeeze.inactive_labels {
            parts.push((tab.label.into(), st(GitRole::Muted, surface)));
        }
        if !count.is_empty() {
            let role = if tab.badge {
                GitRole::Focus
            } else {
                GitRole::Muted
            };
            parts.push((count.into(), st(role, surface)));
        }
        for (text, role) in &tab.marks {
            parts.push((text.clone(), st(*role, surface)));
        }
        for (at, (text, style)) in parts.into_iter().enumerate() {
            spans.push((if at == 0 { text } else { format!(" {text}") }, style));
        }
    }
    spans
}

/// Row 1 while a Git operation is in progress.
pub(crate) fn banner(grid: &mut CellGrid, rect: Rect, spans: &[Span], actions: &[(&str, &str)]) {
    let surface = GitSurface::Warning;
    fill(grid, rect, st(GitRole::Text, surface));
    let mut right_spans = Vec::new();
    for (index, (key, label)) in actions.iter().enumerate() {
        if index > 0 {
            right_spans.push((" · ".to_owned(), st(GitRole::Muted, surface)));
        }
        right_spans.push(((*key).to_owned(), bold(GitRole::Focus, surface)));
        right_spans.push((format!(" {label}"), st(GitRole::Text, surface)));
    }
    let right_at = rect.right().saturating_sub(spans_width(&right_spans) + 1);
    put_spans(grid, rect.x + 1, rect.y, spans, right_at.saturating_sub(1));
    put_spans(grid, right_at, rect.y, &right_spans, rect.right());
}

thread_local! {
    /// Columns at the left of the footer row that the mode chip (`modal.rs`) will paint.
    static FOOTER_INSET: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

/// Reserves `columns` at the left of every footer drawn until it is reset to 0.
pub(crate) fn set_footer_inset(columns: u16) {
    FOOTER_INSET.with(|inset| inset.set(columns));
}

/// `rect` less the columns the mode chip reserves.
pub(crate) fn footer_rect(rect: Rect) -> Rect {
    let inset = FOOTER_INSET
        .with(std::cell::Cell::get)
        .min(rect.w.saturating_sub(1));
    Rect::new(rect.x + inset, rect.y, rect.w - inset, rect.h)
}

/// The last row: at most five `key label` pairs, then `? keys`; context on the right.
pub(crate) fn footer(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
) {
    footer_with(grid, rect, hints, context, message, 6);
}

/// `footer` with up to `most` hints: a screen that ranks its own keys by priority (Actions'
/// run pane) fills the row until the width runs out.
pub(crate) fn footer_with(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
    most: usize,
) {
    footer_ranked(grid, rect, hints, context, message, most, false);
}

/// `footer` whose context is decoration (a repository tag, a standing query): keys outrank
/// it, and it takes only what the hints leave.
pub(crate) fn footer_keys_first(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
) {
    footer_ranked(grid, rect, hints, context, message, 6, true);
}

/// [`footer_keys_first`] without the six-hint cap: the width alone decides how many show,
/// in the order given (r15 narrow pass: Notifications' `/ filter` never showed at 190).
pub(crate) fn footer_keys_first_fill(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
) {
    footer_ranked(grid, rect, hints, context, message, usize::MAX, true);
}

fn footer_ranked(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
    most: usize,
    keys_first: bool,
) {
    let surface = GitSurface::TopBar;
    fill(grid, rect, st(GitRole::Text, surface));
    // A modal screen's mode chip takes the left of the row: the hints lay out after it and
    // drop by priority instead of running under it.
    let rect = footer_rect(rect);
    // `? keys` always follows the hints that fit.
    let keys = |grid: &mut CellGrid, x: u16| {
        let x = grid.put_text_clipped(x, rect.y, "?", bold(GitRole::Focus, surface), rect.right());
        grid.put_text_clipped(
            x,
            rect.y,
            " keys",
            st(GitRole::Muted, surface),
            rect.right(),
        )
    };
    // A notice sits on the right, where the context was, and the hints stay: a
    // toast never takes away the keys. Only a notice too long to share the row takes it.
    let notice = message
        .filter(|(text, _)| !text.is_empty())
        .map(|(text, error)| {
            let (mark, role) = if error {
                ("✕ ", GitRole::Removed)
            } else {
                ("", GitRole::Modified)
            };
            (format!("{mark}{text}"), role)
        });
    if let Some((text, role)) = &notice
        && width(text) + 6 + 4 > rect.w.saturating_sub(2)
    {
        grid.put_text_clipped(
            rect.x + 1,
            rect.y,
            &ellipsize(text, usize::from(rect.w.saturating_sub(2))),
            bold(*role, surface),
            rect.right(),
        );
        return;
    }
    // The context (a path, a find position) keeps up to a third of the row; hints that do
    // not fit are dropped, and `? keys` is never cut. A decorative context yields to the
    // keys instead (`keys_first`); a notice always keeps its room.
    let keys_width = 6u16;
    let room = rect.w.saturating_sub(1);
    let pairs = hints.iter().take(most).copied().collect::<Vec<_>>();
    let size = |key: &str, label: &str| width(key) + width(label) + 1 + 3;
    let wanted_context = match &notice {
        Some((text, _)) => width(text) + 2,
        None if context.is_empty() => 0,
        None => width(context).min(rect.w / 3) + 2,
    };
    let reserved = if notice.is_some() || !keys_first {
        wanted_context
    } else {
        0
    };
    let mut used = keys_width;
    let mut shown = Vec::new();
    for (key, label) in &pairs {
        let next = size(key, label);
        // Hints come in priority order: the first that does not fit ends the row, so a
        // smaller, lesser key never shows where a more important one was dropped.
        if used + next + reserved > room {
            break;
        }
        used += next;
        shown.push((*key, *label));
    }
    let mut x = rect.x + 1;
    for (key, label) in &shown {
        x = grid.put_text_clipped(x, rect.y, key, bold(GitRole::Focus, surface), rect.right());
        x = grid.put_text_clipped(
            x,
            rect.y,
            &format!(" {label}"),
            st(GitRole::Muted, surface),
            rect.right(),
        );
        x = grid.put_text_clipped(x, rect.y, " · ", st(GitRole::Border, surface), rect.right());
    }
    x = keys(grid, x);
    if let Some((text, role)) = &notice {
        let at = rect.right().saturating_sub(width(text) + 1).max(x + 2);
        grid.put_text_clipped(at, rect.y, text, bold(*role, surface), rect.right());
        return;
    }
    if !context.is_empty() {
        let free = rect.right().saturating_sub(x + 3);
        let cells = usize::from(free.min(wanted_context.saturating_sub(2)));
        // A query (`label:bug review-requested:@me`) loses whole terms from the front, never
        // half a term; a path keeps its tail.
        let text = if context.contains(':') && context.width() > cells {
            let mut kept: Vec<&str> = Vec::new();
            for term in context.split_whitespace().rev() {
                let next =
                    kept.iter().map(|term| term.width() + 1).sum::<usize>() + term.width() + 2;
                if next > cells {
                    break;
                }
                kept.push(term);
            }
            kept.reverse();
            if kept.is_empty() {
                String::new()
            } else {
                format!("… {}", kept.join(" "))
            }
        } else if keys_first && context.width() > cells {
            // A decorative tag (`orbit-dev/orbit`) shows whole or not at all: `…orbit`
            // reads as a word cut in half.
            String::new()
        } else {
            ellipsize_start(context, cells)
        };
        if cells >= 4 && !text.is_empty() {
            let at = rect.right().saturating_sub(width(&text) + 1);
            grid.put_text_clipped(at, rect.y, &text, st(GitRole::Muted, surface), rect.right());
        }
    }
}

/// How a receipt band reads: a success, a neutral notice (an operation that moved on but
/// still needs the user, which must not look like a failure), or an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptTone {
    Ok,
    Notice,
    /// No receipt reports a failure yet (refusals stay in the notice row), but the band keeps
    /// the `ReceiptErr` surface.
    #[allow(dead_code)]
    Error,
}

/// A one-row receipt band at the top of the reader interior. Notices use the warning
/// surface and `⚠`.
pub(crate) fn receipt(
    grid: &mut CellGrid,
    rect: Rect,
    tone: ReceiptTone,
    text: &str,
    mode: ExplorerIconMode,
) {
    let (surface, mark, role) = match tone {
        ReceiptTone::Ok => (GitSurface::ReceiptOk, Glyph::ReceiptOk, GitRole::Added),
        ReceiptTone::Notice => (GitSurface::Warning, Glyph::Warning, GitRole::Modified),
        ReceiptTone::Error => (GitSurface::ReceiptErr, Glyph::ReceiptErr, GitRole::Removed),
    };
    fill(
        grid,
        Rect::new(rect.x, rect.y, rect.w, 1),
        st(GitRole::Text, surface),
    );
    let x = grid.put_text_clipped(
        rect.x + 1,
        rect.y,
        &format!("{} ", glyph(mark, mode)),
        bold(role, surface),
        rect.right(),
    );
    grid.put_text_clipped(
        x,
        rect.y,
        &ellipsize(text, usize::from(rect.right().saturating_sub(x + 1))),
        st(GitRole::Text, surface),
        rect.right(),
    );
}

/// One date grammar for the Git tab: a commit from the last day reads as
/// its age (`now`, `5m`, `2h`), an older one as `Nov 12`, and one from another year as
/// `Nov 12, 2025`. `stamp` is a Git date: strict ISO (`%aI`, which has the hour) or a bare
/// `2026-11-12` (`%as`, `--date=short`), which can only say `today`. `now` is Unix
/// seconds. Anything else is shown as given; the full stamp is for copying, not lists.
pub(crate) fn when(stamp: &str, now: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let stamp = stamp.trim();
    let Some(at) = crate::forge::records::epoch(stamp) else {
        return stamp.to_owned();
    };
    let timed = stamp.len() > 10;
    let age = now - at;
    if timed && (0..86_400).contains(&age) {
        return if age < 60 {
            "now".into()
        } else if age < 3600 {
            format!("{}m", age / 60)
        } else {
            format!("{}h", age / 3600)
        };
    }
    // The date as the author's clock wrote it, as `git log` prints it.
    let mut parts = stamp[..10].split('-');
    let (Some(year), Some(month), Some(day)) = (
        parts.next().and_then(|part| part.parse::<i64>().ok()),
        parts.next().and_then(|part| part.parse::<usize>().ok()),
        parts.next().and_then(|part| part.parse::<u8>().ok()),
    ) else {
        return stamp.to_owned();
    };
    // Today's civil date in UTC (Howard Hinnant's civil-from-days).
    let days = now.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let today_day = doy - (153 * mp + 2) / 5 + 1;
    let today_month = if mp < 10 { mp + 3 } else { mp - 9 };
    let today_year = yoe + era * 400 + i64::from(today_month <= 2);
    if !timed && (year, month as i64, i64::from(day)) == (today_year, today_month, today_day) {
        return "today".into();
    }
    let name = MONTHS[month.clamp(1, 12) - 1];
    if year == today_year {
        format!("{name} {day}")
    } else {
        format!("{name} {day}, {year}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(width_cells: u16) -> String {
        let mut grid = CellGrid::new(width_cells, 1);
        let tab = |glyph, role, label, count: Option<&str>| Tab {
            glyph,
            role,
            label,
            count: count.map(Into::into),
            badge: false,
            marks: Vec::new(),
        };
        top_bar(
            &mut grid,
            width_cells,
            &TopBar {
                repo: "orbit",
                head: "fix/backoff",
                detached: false,
                operation: false,
                divergence: Some((2, 0)),
                tabs: vec![
                    tab(Glyph::Repo, GitRole::Focus, "Git", None),
                    tab(Glyph::PullRequest, GitRole::Ref, "Pull requests", Some("8")),
                    tab(Glyph::Issue, GitRole::Added, "Issues", Some("10")),
                    tab(Glyph::Actions, GitRole::Modified, "Actions", None),
                ],
                active: 0,
                mode: ExplorerIconMode::Unicode,
            },
        );
        grid.row_text(0).trim_end().to_owned()
    }

    #[test]
    fn wide_bar_reads_like_the_design() {
        let text = bar(190);
        assert!(
            text.starts_with(" DUSTY GIT │ orbit │ ⎇ fix/backoff │ ↑2 ↓0"),
            "{text}"
        );
        assert!(
            text.ends_with("◆ Git  │ ⎇ Pull requests 8 │ ○ Issues 10 │ ● Actions"),
            "{text}"
        );
    }

    #[test]
    fn bar_shrinks_in_order_and_keeps_divergence() {
        assert!(bar(100).contains("DUSTY GIT"));
        let medium = bar(80);
        assert!(!medium.contains("DUSTY GIT"), "{medium}");
        assert!(medium.contains("↑2 ↓0"), "{medium}");
        let narrow = bar(64);
        assert!(
            narrow.contains("↑2 ↓0") && narrow.contains("⎇ 8"),
            "{narrow}"
        );
        assert!(!narrow.contains("Pull"), "{narrow}");
    }

    /// r8 R7-03: a count is on every tab or on none, whatever the width; icons go first.
    #[test]
    fn counts_are_on_every_tab_or_none() {
        for width in 60..=200 {
            let text = bar(width);
            if text.contains("Pull requests") {
                assert_eq!(
                    text.contains("Pull requests 8"),
                    text.contains("Issues 10"),
                    "{width}: {text}"
                );
            }
        }
        let hundred = bar(100);
        assert!(
            hundred.contains("Pull requests 8") && hundred.contains("Issues 10"),
            "{hundred}"
        );
    }

    /// r9 R8-10: behind a sheet nothing of the screen's body survives, not a word, a pane
    /// rule nor a tab separator; the top bar and the footer row do.
    #[test]
    fn scrim_leaves_no_fragment_of_the_screen_behind() {
        for (w, h) in [(100u16, 30u16), (120, 36), (190, 48)] {
            let mut grid = CellGrid::new(w, h);
            let text = st(GitRole::Text, GitSurface::Pane);
            for y in 1..h {
                let row = match y {
                    1 => format!("╭ Issues {}╮", "─".repeat(usize::from(w) - 11)),
                    _ if y + 2 == h => format!("╰{}╯", "─".repeat(usize::from(w) - 2)),
                    _ if y + 1 == h => "Enter open · ? keys".to_owned(),
                    _ => format!(
                        "│ All 10 │ Bugs 4 │ Other 4 ago long logs {}│",
                        "Merge ".repeat(usize::from(w) / 6 - 8)
                    ),
                };
                grid.put_text_clipped(0, y, &row, text, w);
            }
            let sheet = Rect::new(w / 4, 3, w / 2, h - 8);
            scrim(&mut grid, sheet);
            for y in 1..h - 1 {
                for x in 0..w {
                    let cell = grid.get(x, y).expect("cell");
                    assert_eq!(
                        cell.symbol, " ",
                        "{w}x{h}: `{}` at ({x},{y}) survives behind the sheet",
                        cell.symbol
                    );
                }
            }
            assert!(
                grid.row_text(h - 1).contains("Enter open"),
                "the footer stays"
            );
        }
    }

    /// Reader and section tabs: counts on all or none, and every `│` keeps its gutters.
    #[test]
    fn sub_tab_counts_are_on_every_tab_or_none() {
        let tabs: Vec<String> = [
            "Overview",
            "Commits 2",
            "Checks 3",
            "Files 2",
            "Activity 6",
            "Merge",
        ]
        .map(Into::into)
        .into();
        for room in 20..=90 {
            let fitted = fit_tabs(&tabs, 0, room);
            let counted = fitted
                .iter()
                .filter(|tab| tab.ends_with(|c: char| c.is_ascii_digit()))
                .count();
            assert!(counted == 0 || counted == 4, "{room}: {fitted:?}");
        }
        let quiet = tabs
            .iter()
            .map(|tab| without_count(tab))
            .collect::<Vec<_>>();
        // The last tab keeps its trailing pad too (a gutter before the border), and
        // whether counts fit never depends on which tab is active.
        for active in 0..tabs.len() {
            assert_eq!(
                fit_tabs(&tabs, active, 64),
                tabs,
                "padded, every count fits in 64"
            );
            assert_eq!(fit_tabs(&tabs, active, 63), quiet);
        }
        let mut grid = CellGrid::new(80, 1);
        for room in [40u16, 54, 56, 62, 63, 64, 80] {
            grid.put_text_clipped(
                0,
                0,
                &" ".repeat(80),
                st(GitRole::Text, GitSurface::Pane),
                80,
            );
            sub_tabs(&mut grid, 0, 0, room, &fit_tabs(&tabs, 2, room), 2, true);
            let row = grid.row_text(0);
            for (at, _) in row.match_indices('│') {
                assert!(
                    row[..at].ends_with(' ') && row[at + '│'.len_utf8()..].starts_with(' '),
                    "{room}: {row}"
                );
            }
        }
    }

    /// The footer keeps whole hints at every width, after a mode chip's inset too: a row that
    /// runs out drops the lowest hints, never half a word (`en · [ ] sections`, R15).
    #[test]
    fn footer_hints_stay_whole_at_every_width() {
        let hints = [
            ("Enter", "open"),
            ("[ ]", "sections"),
            ("/", "filter"),
            ("F", "narrow"),
            ("r", "rerun"),
            ("d", "dispatch"),
            ("Space e", "sidebar"),
        ];
        for inset in [0u16, 10] {
            for columns in 20u16..=200 {
                let mut grid = CellGrid::new(columns, 1);
                set_footer_inset(inset);
                footer_with(
                    &mut grid,
                    Rect::new(0, 0, columns, 1),
                    &hints,
                    "orbit-dev/orbit",
                    None,
                    usize::MAX,
                );
                set_footer_inset(0);
                let row = grid.row_text(0);
                let shown = row
                    .chars()
                    .skip(usize::from(inset.min(columns - 1)))
                    .collect::<String>();
                let shown = shown.trim();
                let mut parts = shown
                    .split(" · ")
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>();
                // `? keys` ends the hints; a context may follow it.
                let last = parts.pop().unwrap_or_default();
                assert!(
                    last.starts_with("? keys") || shown.is_empty(),
                    "{inset}/{columns}: {row:?}"
                );
                for part in &parts {
                    assert!(
                        hints
                            .iter()
                            .any(|(key, label)| *part == format!("{key} {label}")),
                        "{inset}/{columns}: cut hint {part:?} in {row:?}"
                    );
                }
                // The first hint goes last: a narrow row still opens with the key that opens.
                if !parts.is_empty() {
                    assert_eq!(parts[0], "Enter open", "{inset}/{columns}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn one_date_grammar_for_the_git_tab() {
        let now = crate::forge::records::epoch("2026-11-16T12:00:00Z").unwrap();
        assert_eq!(super::when("2026-11-16T11:59:30Z", now), "now");
        assert_eq!(super::when("2026-11-16T11:15:00Z", now), "45m");
        assert_eq!(super::when("2026-11-16T10:00:00+00:00", now), "2h");
        assert_eq!(super::when("2026-11-16T12:00:00+02:00", now), "2h");
        assert_eq!(super::when("2026-11-15T09:00:00Z", now), "Nov 15");
        assert_eq!(super::when("2026-11-12", now), "Nov 12");
        assert_eq!(super::when("2026-11-16", now), "today");
        assert_eq!(super::when("2025-06-08T12:00:00Z", now), "Jun 8, 2025");
        assert_eq!(super::when("2025-06-08", now), "Jun 8, 2025");
        assert_eq!(super::when("not a date", now), "not a date");
        // A clock ahead of ours is a date, never a negative age.
        assert_eq!(super::when("2026-11-17T09:00:00Z", now), "Nov 17");
    }
}
