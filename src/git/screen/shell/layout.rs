//! Breakpoints → rectangles (DESIGN.md §2.5). Pane rectangles depend on the terminal
//! size and on what the rail panes hold (counts that moving focus or a cursor cannot
//! change), never on which pane has focus.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }
    pub const fn right(&self) -> u16 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> u16 {
        self.y + self.h
    }
    /// The interior of a bordered box.
    pub const fn inner(&self) -> Self {
        Self {
            x: self.x + 1,
            y: self.y + 1,
            w: self.w.saturating_sub(2),
            h: self.h.saturating_sub(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Wide,
    Medium,
    /// One surface at a time.
    Narrow,
    TooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub mode: Mode,
    pub top: Rect,
    pub banner: Option<Rect>,
    pub body: Rect,
    /// Changes, Branches, History, Stashes. Zero-sized in narrow mode.
    pub rail: [Rect; 4],
    pub reader: Rect,
    pub footer: Rect,
}

pub(crate) const MIN_WIDTH: u16 = 64;
pub(crate) const MIN_HEIGHT: u16 = 20;

pub(crate) fn mode(width: u16, height: u16) -> Mode {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        Mode::TooSmall
    } else if width < 100 {
        Mode::Narrow
    } else if width >= 160 && height >= 40 {
        Mode::Wide
    } else {
        Mode::Medium
    }
}

pub(crate) fn rail_width(width: u16, mode: Mode) -> u16 {
    match mode {
        // History subjects need the room more than the reader needs the last columns.
        Mode::Wide => (25 + width / 7).clamp(44, 60),
        // A quarter of the width: at 120 columns a History subject keeps ~20 cells.
        Mode::Medium => (width / 4 + 7).clamp(32, 44),
        _ => width,
    }
}

/// Split `total` rows by percentages, giving rounding leftovers to the largest
/// fractional parts so the panes always fill the rail exactly.
fn split(total: u16, percent: [u16; 4], minimum: [u16; 4]) -> [u16; 4] {
    let total32 = u32::from(total);
    let mut sizes = percent.map(|p| (total32 * u32::from(p) / 100) as u16);
    let mut fractions = [0usize, 1, 2, 3];
    fractions.sort_by_key(|&i| std::cmp::Reverse(total32 * u32::from(percent[i]) % 100));
    let mut left = total.saturating_sub(sizes.iter().sum());
    for &i in fractions.iter().cycle().take(8) {
        if left == 0 {
            break;
        }
        sizes[i] += 1;
        left -= 1;
    }
    for i in 0..4 {
        while sizes[i] < minimum[i] {
            let donor = (0..4)
                .filter(|&j| j != i && sizes[j] > minimum[j])
                .max_by_key(|&j| sizes[j]);
            let Some(donor) = donor else { break };
            sizes[donor] -= 1;
            sizes[i] += 1;
        }
    }
    sizes
}

/// Rows of a collapsed (empty) rail pane: its title border, one body line that says why
/// it is empty (`No stashes`), and its bottom border.
pub(crate) const COLLAPSED: u16 = 3;

/// `empty[i]` collapses rail pane `i` to [`COLLAPSED`] rows and gives its rows to the pane
/// above. The percentage budgets of §2.5, for screens that only need the body and reader.
pub(crate) fn layout(width: u16, height: u16, banner: bool, empty: [bool; 4]) -> Layout {
    sized(width, height, banner, empty, None)
}

/// The Git tab's rail sized by content (DESIGN.md §2.5): `rows[i]` is the interior rows
/// pane `i` wants (Changes, Branches and Stashes; History's entry is ignored). Changes
/// keeps at least its budget, Branches and Stashes shrink to their content, all three grow
/// up to what History's budget leaves, and History takes every row left over. Inputs are
/// counts (changed files, local branches, stashes), so focus, cursor and view moves (a
/// commit's files, Reflog, sub-tabs) never move a rectangle.
pub(crate) fn fitted(width: u16, height: u16, banner: bool, empty: [bool; 4], rows: [u16; 4]) -> Layout {
    sized(width, height, banner, empty, Some(rows))
}

fn sized(width: u16, height: u16, banner: bool, empty: [bool; 4], rows: Option<[u16; 4]>) -> Layout {
    let mode = mode(width, height);
    let top = Rect::new(0, 0, width, 1);
    let banner_rect = (banner && height > 3).then(|| Rect::new(0, 1, width, 1));
    let body_y = 1 + u16::from(banner_rect.is_some());
    let body_h = height.saturating_sub(body_y + 1);
    let body = Rect::new(0, body_y, width, body_h);
    let footer = Rect::new(0, height.saturating_sub(1), width, 1);
    let mut result = Layout {
        mode,
        top,
        banner: banner_rect,
        body,
        rail: [Rect::default(); 4],
        reader: body,
        footer,
    };
    if !matches!(mode, Mode::Wide | Mode::Medium) {
        return result;
    }
    let rail = rail_width(width, mode);
    let (percent, minimum) = if mode == Mode::Wide {
        ([36, 24, 34, 6], [5, 5, 5, 3])
    } else {
        ([36, 22, 36, 6], [3, 3, 3, 3])
    };
    let mut sizes = split(body_h, percent, minimum);
    match rows {
        Some(rows) => sizes = fit(body_h, sizes, percent, minimum, empty, rows),
        None => {
            for pane in (0..4).rev() {
                if empty[pane] && sizes[pane] > COLLAPSED {
                    let spare = sizes[pane] - COLLAPSED;
                    sizes[pane] = COLLAPSED;
                    // The pane above takes the rows; the first pane gives to the one below.
                    let to = if pane == 0 { 1 } else { pane - 1 };
                    sizes[to] += spare;
                }
            }
        }
    }
    let mut y = body_y;
    for (pane, size) in sizes.iter().enumerate() {
        result.rail[pane] = Rect::new(0, y, rail, *size);
        y += size;
    }
    result.reader = Rect::new(rail + 1, body_y, width - rail - 1, body_h);
    result
}

/// Content sizing for [`fitted`]: History and Changes keep at least their budgets (Changes
/// also shows a commit's files, every tracked file and worktrees, whose counts are not
/// inputs); Branches and Stashes shrink to their content (borders included), never below
/// their minimum, and give the rest away. Changes grows past its budget for a long working
/// tree. When the lists want more than there is, rows go one at a time to the pane furthest
/// below its percentage share, so a long list never starves a short one.
fn fit(total: u16, budget: [u16; 4], percent: [u16; 4], minimum: [u16; 4], empty: [bool; 4], rows: [u16; 4]) -> [u16; 4] {
    const LISTS: [usize; 3] = [0, 1, 3];
    let floor = |pane: usize| match pane {
        0 => budget[0],
        _ if empty[pane] => COLLAPSED.min(minimum[pane]),
        _ => minimum[pane],
    };
    // `rows[pane] == 0` for a pane with content means its count is not known yet: the
    // pane keeps its budget.
    let want = |pane: usize| {
        if empty[pane] && pane != 0 {
            COLLAPSED
        } else if rows[pane] == 0 {
            budget[pane].max(floor(pane))
        } else {
            rows[pane].saturating_add(2).max(floor(pane))
        }
    };
    let room = total.saturating_sub(budget[2]);
    let mut sizes = [0u16; 4];
    if LISTS.iter().map(|&pane| want(pane)).sum::<u16>() <= room {
        for pane in LISTS {
            sizes[pane] = want(pane);
        }
    } else {
        for pane in LISTS {
            sizes[pane] = floor(pane).min(want(pane));
        }
        let mut left = room.saturating_sub(LISTS.iter().map(|&pane| sizes[pane]).sum());
        while left > 0 {
            let Some(pane) = LISTS
                .iter()
                .copied()
                .filter(|&pane| sizes[pane] < want(pane))
                .min_by_key(|&pane| u32::from(sizes[pane]) * 100 / u32::from(percent[pane].max(1)))
            else {
                break;
            };
            sizes[pane] += 1;
            left -= 1;
        }
    }
    sizes[2] = total.saturating_sub(LISTS.iter().map(|&pane| sizes[pane]).sum());
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heights(width: u16, height: u16) -> (u16, [u16; 4]) {
        let layout = layout(width, height, false, [false; 4]);
        (layout.rail[0].w, layout.rail.map(|rect| rect.h))
    }

    #[test]
    fn budgets_match_the_design_table() {
        assert_eq!(heights(190, 48), (52, [16, 11, 16, 3]));
        // A non-empty Stashes pane always keeps one visible row.
        assert_eq!(heights(120, 36), (37, [12, 8, 11, 3]));
        assert_eq!(heights(100, 30), (32, [10, 6, 9, 3]));
        assert_eq!(mode(64, 20), Mode::Narrow);
        assert_eq!(mode(63, 20), Mode::TooSmall);
        assert_eq!(mode(190, 39), Mode::Medium);
    }

    #[test]
    fn banner_shifts_the_body_by_one_row() {
        let plain = layout(120, 36, false, [false; 4]);
        let banner = layout(120, 36, true, [false; 4]);
        assert_eq!(banner.body.y, plain.body.y + 1);
        assert_eq!(banner.body.h + 1, plain.body.h);
        assert_eq!(banner.footer, plain.footer);
    }

    #[test]
    fn an_empty_pane_collapses_and_gives_rows_above() {
        let full = layout(120, 36, false, [false; 4]);
        let empty = layout(120, 36, false, [false, false, false, true]);
        assert_eq!(empty.rail[3].h, COLLAPSED);
        let collapsed = layout(120, 36, false, [false, true, false, false]);
        assert_eq!(collapsed.rail[1].h, COLLAPSED);
        assert_eq!(collapsed.rail[0].h, full.rail[0].h + full.rail[1].h - COLLAPSED);
        assert_eq!(
            collapsed.rail.iter().map(|rect| rect.h).sum::<u16>(),
            full.body.h
        );
    }

    fn fitted_heights(width: u16, height: u16, empty: [bool; 4], rows: [u16; 4]) -> [u16; 4] {
        fitted(width, height, false, empty, rows).rail.map(|rect| rect.h)
    }

    #[test]
    fn short_lists_give_their_rows_to_history() {
        // The orbit fixture: 7 changes, a sub-tab row and 4 branches, 1 stash. Branches
        // sits tight and its spare rows go to History.
        assert_eq!(fitted_heights(120, 36, [false; 4], [7, 5, 0, 1]), [12, 7, 12, 3]);
        assert_eq!(fitted_heights(190, 48, [false; 4], [7, 5, 0, 1]), [16, 7, 20, 3]);
        // No stashes: title, `No stashes` and the bottom border.
        assert_eq!(fitted_heights(120, 36, [false, false, false, true], [7, 5, 0, 0])[3], COLLAPSED);
        // A clean tree keeps Changes' budget: it also lists a commit's files.
        assert_eq!(fitted_heights(190, 48, [false; 4], [0, 5, 0, 1])[0], 16);
        // Counts not known yet (0) keep every budget.
        assert_eq!(fitted_heights(120, 36, [false; 4], [0; 4]), [12, 8, 11, 3]);
        assert_eq!(fitted_heights(190, 48, [false; 4], [0; 4]), [16, 11, 16, 3]);
    }

    #[test]
    fn long_lists_share_what_history_leaves_and_history_keeps_its_budget() {
        for (width, height) in [(100, 30), (120, 36), (190, 48)] {
            let budget = layout(width, height, false, [false; 4]).rail.map(|rect| rect.h);
            let sizes = fitted_heights(width, height, [false; 4], [60, 40, 0, 9]);
            assert_eq!(sizes[2], budget[2], "{width}x{height}: {sizes:?}");
            assert_eq!(sizes.iter().sum::<u16>(), budget.iter().sum::<u16>());
            // Both overflowing: they split the room by their percentages, as the budget does.
            assert!(sizes[0].abs_diff(budget[0]) <= 1 && sizes[1].abs_diff(budget[1]) <= 1, "{width}x{height}: {sizes:?}");
            assert!(sizes[3] >= 3);
            // One long list takes the other's slack.
            let lopsided = fitted_heights(width, height, [false; 4], [60, 2, 0, 1]);
            assert_eq!(lopsided[1], 4.max(if width >= 160 { 5 } else { 3 }));
            assert!(lopsided[0] > budget[0] + 1, "{width}x{height}: {lopsided:?}");
        }
    }

    #[test]
    fn rectangles_do_not_depend_on_focus_or_history_rows() {
        // History's own count is not an input: browsing branches changes it.
        let a = fitted(120, 36, false, [false; 4], [7, 5, 0, 1]);
        let b = fitted(120, 36, false, [false; 4], [7, 5, 900, 1]);
        assert_eq!(a, b);
    }
}
