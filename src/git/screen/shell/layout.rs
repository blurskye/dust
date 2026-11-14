//! Breakpoints → rectangles (DESIGN.md §2.5). Pane rectangles depend on the terminal
//! size and on which rail panes are empty, never on how many rows a pane holds.

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
        Mode::Medium => (20 + width / 10).clamp(28, 36),
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

/// `empty[i]` collapses rail pane `i` to two rows and gives its rows to the pane above.
pub(crate) fn layout(width: u16, height: u16, banner: bool, empty: [bool; 4]) -> Layout {
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
    for pane in (0..4).rev() {
        if empty[pane] && sizes[pane] > 2 {
            let spare = sizes[pane] - 2;
            sizes[pane] = 2;
            // The pane above takes the rows; the first pane gives to the one below.
            let to = if pane == 0 { 1 } else { pane - 1 };
            sizes[to] += spare;
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
        assert_eq!(heights(120, 36), (32, [12, 8, 11, 3]));
        assert_eq!(heights(100, 30), (30, [10, 6, 9, 3]));
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
        assert_eq!(empty.rail[3].h, 2);
        let collapsed = layout(120, 36, false, [false, true, false, false]);
        assert_eq!(collapsed.rail[1].h, 2);
        assert_eq!(collapsed.rail[0].h, full.rail[0].h + full.rail[1].h - 2);
        assert_eq!(
            collapsed.rail.iter().map(|rect| rect.h).sum::<u16>(),
            full.body.h
        );
    }
}
