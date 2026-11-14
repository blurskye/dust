//! Persistent chrome (DESIGN.md §2): top bar, operation banner, pane frames, sub-tabs,
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
        return if cells == 0 { String::new() } else { text.into() };
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

pub(crate) fn put_spans(grid: &mut CellGrid, mut x: u16, y: u16, spans: &[Span], right: u16) -> u16 {
    for (text, style) in spans {
        x = grid.put_text_clipped(x, y, text, *style, right);
    }
    x
}

/// A bordered pane with `glyph title` on the top border and a muted count at its right end.
pub(crate) fn pane(
    grid: &mut CellGrid,
    rect: Rect,
    title: &[Span],
    count: &str,
    focused: bool,
) {
    if rect.w < 4 || rect.h < 2 {
        return;
    }
    let surface = GitSurface::Pane;
    fill(grid, rect, st(GitRole::Text, surface));
    let edge = st(if focused { GitRole::Focus } else { GitRole::Border }, surface);
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
    let count_width = if count.is_empty() { 0 } else { width(count) + 2 };
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

/// `Local │ Remotes 3 │ Tags 2` on one interior row; the active tab is a filled chip.
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
    for (index, tab) in tabs.iter().enumerate() {
        // A tab that does not fit whole is replaced by `…`, never cut mid-word.
        let need = width(tab) + 2 + u16::from(index > 0);
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
        at = grid.put_text_clipped(at, y, &format!(" {tab} "), style, right);
    }
}

/// Tabs made to fit `room` columns: the longest names shorten first (`Features 2` →
/// `Feat 2`), the active one last; when even that is too wide the tail folds into a counted
/// `+2`, never a bare `…` (DESIGN.md §4.5).
pub(crate) fn fit_tabs(tabs: &[String], active: usize, room: u16) -> Vec<String> {
    fit_tabs_reserving(tabs, active, room, 0)
}

/// The short name of every tab label that has one; the rest keep their names.
const SHORT_TABS: [(&str, &str); 6] = [
    ("Overview", "Info"),
    ("Activity", "Log"),
    ("Pipelines", "Pipes"),
    ("Discussion", "Talk"),
    ("Features", "Feat"),
    ("Other", "Misc"),
];

/// One rule at every width: full names when the row fits (with `reserve` columns kept for
/// tabs that come and go, such as Merge), else every tab's short name, else the tail folded
/// into `+N more`. A row never mixes long and short names.
pub(crate) fn fit_tabs_reserving(tabs: &[String], active: usize, room: u16, reserve: u16) -> Vec<String> {
    let total = |tabs: &[String]| tabs.iter().map(|tab| width(tab) + 3).sum::<u16>();
    if total(tabs) + reserve <= room {
        return tabs.to_vec();
    }
    let short = tabs
        .iter()
        .map(|tab| match SHORT_TABS.iter().find(|(long, _)| tab.starts_with(long)) {
            Some((long, short)) => format!("{short}{}", &tab[long.len()..]),
            None => tab.clone(),
        })
        .collect::<Vec<_>>();
    if total(&short) <= room {
        return short;
    }
    // Fold the tail behind the active tab into `+N more`.
    let mut kept = short.len();
    while kept > active + 1 && kept > 1 {
        kept -= 1;
        let mut candidate = short[..kept].to_vec();
        candidate.push(format!("+{} more", short.len() - kept));
        if total(&candidate) <= room {
            return candidate;
        }
    }
    short
}

/// A chip: one space of padding each side on a tinted surface.
pub(crate) fn chip(grid: &mut CellGrid, x: u16, y: u16, right: u16, text: &str, hue: GitChip) -> u16 {
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
    grid.put_text_clipped(x, y, &format!(" {text} "), st(role, GitSurface::Chip(hue)), right)
}

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub glyph: Glyph,
    pub role: GitRole,
    pub label: &'static str,
    pub short: &'static str,
    pub count: Option<String>,
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

/// Row 0. Shrinks in the order of DESIGN.md §2.1 until it fits.
pub(crate) fn top_bar(grid: &mut CellGrid, width_cells: u16, bar: &TopBar<'_>) {
    let surface = GitSurface::TopBar;
    let base = st(GitRole::Text, surface);
    for x in 0..width_cells {
        grid.set_cell(x, 0, " ", base);
    }
    for step in 0..=7 {
        let left = left_cluster(bar, step);
        let right = right_cluster(bar, step);
        let need = spans_width(&left) + spans_width(&right) + 3;
        if need <= width_cells || step == 7 {
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

fn left_cluster(bar: &TopBar<'_>, step: u8) -> Vec<Span> {
    let surface = GitSurface::TopBar;
    let mut spans = Vec::new();
    if step < 2 {
        spans.push(("DUSTY GIT".into(), bold(GitRole::Text, surface)));
        spans.push(separator());
    }
    if step < 6 {
        let repo = if step >= 5 {
            ellipsize(bar.repo, 12)
        } else {
            bar.repo.to_owned()
        };
        spans.push((repo, st(GitRole::Text, surface)));
        spans.push(separator());
    }
    let head = if step >= 6 {
        ellipsize_start(bar.head, 14)
    } else {
        bar.head.to_owned()
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
    if step < 6 {
        spans.push(separator());
    } else {
        spans.push((" ".into(), st(GitRole::Text, surface)));
    }
    match bar.divergence {
        Some((ahead, behind)) => {
            let quiet = ahead == 0 && behind == 0;
            let role = |live| if quiet { GitRole::Muted } else { live };
            spans.push((
                format!("{}{ahead}", glyph(Glyph::Ahead, bar.mode)),
                st(role(if ahead > 0 { GitRole::Added } else { GitRole::Muted }), surface),
            ));
            spans.push((" ".into(), st(GitRole::Text, surface)));
            spans.push((
                format!("{}{behind}", glyph(Glyph::Behind, bar.mode)),
                st(
                    role(if behind > 0 { GitRole::Removed } else { GitRole::Muted }),
                    surface,
                ),
            ));
        }
        None => spans.push(("· no upstream".into(), st(GitRole::Muted, surface))),
    }
    spans
}

fn right_cluster(bar: &TopBar<'_>, step: u8) -> Vec<Span> {
    let surface = GitSurface::TopBar;
    let mut spans = Vec::new();
    for (index, tab) in bar.tabs.iter().enumerate() {
        if index > 0 {
            spans.push(if step >= 7 {
                (" ".into(), st(GitRole::Text, surface))
            } else {
                separator()
            });
        }
        let label = if step >= 4 {
            ""
        } else if step >= 1 {
            tab.short
        } else {
            tab.label
        };
        let mark = glyph(tab.glyph, bar.mode);
        let count = tab.count.as_deref().unwrap_or("");
        if index == bar.active {
            let parts = [mark, label, count]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            spans.push((
                format!(" {} ", parts.join(" ")),
                bold(GitRole::OnFocus, GitSurface::ChipActive),
            ));
        } else {
            spans.push((mark.into(), st(tab.role, surface)));
            if !label.is_empty() {
                spans.push((format!(" {label}"), st(GitRole::Muted, surface)));
            }
            if !count.is_empty() {
                spans.push((
                    format!("{}{count}", if step >= 7 { "" } else { " " }),
                    st(GitRole::Muted, surface),
                ));
            }
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

/// The last row: at most five `key label` pairs, then `? keys`; context on the right.
pub(crate) fn footer(
    grid: &mut CellGrid,
    rect: Rect,
    hints: &[(&str, &str)],
    context: &str,
    message: Option<(&str, bool)>,
) {
    let surface = GitSurface::TopBar;
    fill(grid, rect, st(GitRole::Text, surface));
    // `? keys` always follows the hints that fit.
    let keys = |grid: &mut CellGrid, x: u16| {
        let x = grid.put_text_clipped(x, rect.y, "?", bold(GitRole::Focus, surface), rect.right());
        grid.put_text_clipped(x, rect.y, " keys", st(GitRole::Muted, surface), rect.right())
    };
    if let Some((text, error)) = message.filter(|(text, _)| !text.is_empty()) {
        let (mark, role) = if error {
            ("✕ ", GitRole::Removed)
        } else {
            ("", GitRole::Modified)
        };
        // A notice takes the whole row until the next action clears it.
        grid.put_text_clipped(
            rect.x + 1,
            rect.y,
            &ellipsize(&format!("{mark}{text}"), usize::from(rect.w.saturating_sub(2))),
            bold(role, surface),
            rect.right(),
        );
        return;
    }
    // The context (a path, a find position) keeps up to a third of the row; hints
    // that do not fit are dropped, and `? keys` is never cut.
    let keys_width = 6u16;
    let room = rect.w.saturating_sub(1);
    let pairs = hints.iter().take(6).copied().collect::<Vec<_>>();
    let size = |key: &str, label: &str| width(key) + width(label) + 1 + 3;
    let wanted_context = if context.is_empty() { 0 } else { width(context).min(rect.w / 3) + 2 };
    let mut used = keys_width;
    let mut shown = Vec::new();
    for (key, label) in &pairs {
        let next = size(*key, *label);
        if used + next + wanted_context > room {
            continue;
        }
        used += next;
        shown.push((*key, *label));
    }
    let mut x = rect.x + 1;
    for (key, label) in &shown {
        x = grid.put_text_clipped(x, rect.y, key, bold(GitRole::Focus, surface), rect.right());
        x = grid.put_text_clipped(x, rect.y, &format!(" {label}"), st(GitRole::Muted, surface), rect.right());
        x = grid.put_text_clipped(x, rect.y, " · ", st(GitRole::Border, surface), rect.right());
    }
    x = keys(grid, x);
    if !context.is_empty() {
        let free = rect.right().saturating_sub(x + 3);
        let cells = usize::from(free.min(wanted_context.saturating_sub(2)));
        // A query (`label:bug review-requested:@me`) loses whole terms from the front, never
        // half a term; a path keeps its tail.
        let text = if context.contains(':') && context.width() > cells {
            let mut kept: Vec<&str> = Vec::new();
            for term in context.split_whitespace().rev() {
                let next = kept.iter().map(|term| term.width() + 1).sum::<usize>() + term.width() + 2;
                if next > cells {
                    break;
                }
                kept.push(term);
            }
            kept.reverse();
            if kept.is_empty() { String::new() } else { format!("… {}", kept.join(" ")) }
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
    /// the `ReceiptErr` surface DESIGN.md §4.6 defines.
    #[allow(dead_code)]
    Error,
}

/// A one-row receipt band at the top of the reader interior. Notices use the warning
/// surface and `⚠` (DESIGN.md §4.6).
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
    fill(grid, Rect::new(rect.x, rect.y, rect.w, 1), st(GitRole::Text, surface));
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

/// A centred modal sheet with an accent border; returns its interior.
pub(crate) fn sheet(grid: &mut CellGrid, body: Rect, title: &str, rows: u16) -> Rect {
    let w = (body.w * 6 / 10).clamp(56.min(body.w), 110.min(body.w));
    let h = (rows + 2).min(body.h.saturating_sub(2)).max(3);
    let rect = Rect::new(
        body.x + (body.w - w) / 2,
        body.y + (body.h.saturating_sub(h)) / 2,
        w,
        h,
    );
    let surface = GitSurface::Sheet;
    fill(grid, rect, st(GitRole::Text, surface));
    let edge = st(GitRole::Focus, surface);
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
    grid.put_text_clipped(
        rect.x + 2,
        rect.y,
        &format!(" {} ", ellipsize(title, usize::from(w.saturating_sub(6)))),
        bold(GitRole::Focus, surface),
        right,
    );
    Rect::new(rect.x + 2, rect.y + 1, w.saturating_sub(4), h.saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(width_cells: u16) -> String {
        let mut grid = CellGrid::new(width_cells, 1);
        let tab = |glyph, role, label, short, count: Option<&str>| Tab {
            glyph,
            role,
            label,
            short,
            count: count.map(Into::into),
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
                    tab(Glyph::Repo, GitRole::Focus, "Git", "Git", None),
                    tab(Glyph::PullRequest, GitRole::Ref, "Pull requests", "PRs", Some("8")),
                    tab(Glyph::Issue, GitRole::Added, "Issues", "Issues", Some("10")),
                    tab(Glyph::Actions, GitRole::Modified, "Actions", "CI", None),
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
        assert!(text.starts_with(" DUSTY GIT │ orbit │ ⎇ fix/backoff │ ↑2 ↓0"), "{text}");
        assert!(text.ends_with("◆ Git  │ ⎇ Pull requests 8 │ ○ Issues 10 │ ● Actions"), "{text}");
    }

    #[test]
    fn bar_shrinks_in_order_and_keeps_divergence() {
        assert!(bar(100).contains("DUSTY GIT"));
        let medium = bar(80);
        assert!(!medium.contains("DUSTY GIT"), "{medium}");
        assert!(medium.contains("↑2 ↓0"), "{medium}");
        let narrow = bar(64);
        assert!(narrow.contains("↑2 ↓0") && narrow.contains("⎇ 8"), "{narrow}");
        assert!(!narrow.contains("Pull"), "{narrow}");
    }
}
