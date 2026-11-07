//! Debugger inspector layout and drawing.
//!
//! Narrow terminals show one selectable inspector instead of squeezing the source.
//! All regions are computed together to preserve the source's minimum readable size.

use unicode_segmentation::UnicodeSegmentation;

use crate::windows::WindowRect;

/// The width at which inspectors are worth docking on BOTH sides of the source.
pub(crate) const DEBUG_WIDE_MIN_WIDTH: u16 = 96;
/// One side inspector's width.
pub(crate) const DEBUG_SIDE_WIDTH: u16 = 28;
/// The console below the source.
pub(crate) const DEBUG_CONSOLE_HEIGHT: u16 = 8;
/// What the SOURCE must keep at any terminal width: the readable minimum, never traded away.
pub(crate) const DEBUG_SOURCE_MIN_WIDTH: u16 = 40;
/// What the source must keep vertically.
pub(crate) const DEBUG_SOURCE_MIN_HEIGHT: u16 = 6;

/// Which inspector a narrow terminal shows, one at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebugPane {
    Sessions,
    Threads,
    Stack,
    Breakpoints,
    Variables,
    Watches,
    Console,
}

impl DebugPane {
    pub(crate) const ALL: [Self; 7] = [
        Self::Sessions,
        Self::Threads,
        Self::Stack,
        Self::Breakpoints,
        Self::Variables,
        Self::Watches,
        Self::Console,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Threads => "Threads",
            Self::Stack => "Stack",
            Self::Breakpoints => "Breakpoints",
            Self::Variables => "Variables",
            Self::Watches => "Watches",
            Self::Console => "Console",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DebugLayoutMode {
    /// Inspectors on both sides, the console below, the source dominant between them.
    Wide,
    /// ONE inspector at a time, so the source is never compressed below its minimum.
    Narrow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DebugLayout {
    pub mode: DebugLayoutMode,
    /// The dominant region: the source stays here.
    pub source: WindowRect,
    pub left: Option<WindowRect>,
    pub right: Option<WindowRect>,
    pub console: Option<WindowRect>,
}

impl DebugLayout {
    fn at(self, x: u16, y: u16) -> Self {
        let moved = |rect: WindowRect| WindowRect {
            x: rect.x + x,
            y: rect.y + y,
            ..rect
        };
        Self {
            mode: self.mode,
            source: moved(self.source),
            left: self.left.map(moved),
            right: self.right.map(moved),
            console: self.console.map(moved),
        }
    }
}

/// Where a narrow terminal's strip puts each inspector's name: what is drawn is what a click finds.
fn pane_tabs(strip: WindowRect, selected: DebugPane) -> Vec<(DebugPane, String, u16, u16)> {
    let inner = strip.width.saturating_sub(2);
    let mut tabs = Vec::new();
    let mut x = strip.x + 1;
    let mut y = strip.y;
    for pane in DebugPane::ALL {
        let label = if pane == selected {
            format!("[{}]", pane.label())
        } else {
            pane.label().to_owned()
        };
        let width = label.chars().count() as u16 + 1;
        if x > strip.x + 1 && x.saturating_sub(strip.x + 1) + width > inner {
            y = y.saturating_add(1);
            x = strip.x + 1;
            if y >= strip.y.saturating_add(strip.height) {
                break;
            }
        }
        tabs.push((pane, label, x, y));
        x = x.saturating_add(width);
    }
    tabs
}

/// The inspector tab under a click in a narrow terminal's strip.
#[must_use]
pub fn debug_pane_tab_at(
    region: WindowRect,
    selected: DebugPane,
    column: u16,
    row: u16,
) -> Option<DebugPane> {
    let layout = debug_layout_in(region)?;
    if layout.mode != DebugLayoutMode::Narrow {
        return None;
    }
    pane_tabs(layout.console?, selected)
        .into_iter()
        .find(|(_, label, x, y)| {
            *y == row && column >= *x && column < x.saturating_add(label.chars().count() as u16)
        })
        .map(|(pane, ..)| pane)
}

/// Long values go on to the next rows, since a value's end is often the part being looked for.
pub(super) fn wrap_rows(body: &[String], width: u16, grid: &crate::ui::CellGrid) -> Vec<String> {
    let width = usize::from(width).max(3);
    let mut wrapped = Vec::new();
    for line in body {
        let mut rest = line.as_str();
        let mut indent = "";
        while grid.display_width(rest) > width - indent.len() {
            let mut used = 0;
            let mut end = 0;
            let mut space = None;
            for (at, grapheme) in rest.grapheme_indices(true) {
                let cells = grid.display_width(grapheme).clamp(1, 2);
                if used + cells > width - indent.len() {
                    break;
                }
                used += cells;
                end = at + grapheme.len();
                if grapheme == " " {
                    space = Some(end);
                }
            }
            let cut = space.unwrap_or(end);
            wrapped.push(format!("{indent}{}", rest[..cut].trim_end()));
            rest = &rest[cut..];
            // Leave room for a two-cell grapheme so every row consumes input.
            indent = &"  "[..width.saturating_sub(2).min(2)];
        }
        wrapped.push(format!("{indent}{rest}"));
    }
    wrapped
}

/// The arrangement inside `region`, the workspace without its tab and status lines.
pub(crate) fn debug_layout_in(region: WindowRect) -> Option<DebugLayout> {
    debug_layout(region.width, region.height).map(|layout| layout.at(region.x, region.y))
}

/// What the tiled editor windows give up to the panes: left, right and bottom. Windows are laid
/// out around the panes, so cursors and mouse clicks stay correct.
#[must_use]
pub fn debug_pane_insets(width: u16, height: u16) -> Option<(u16, u16, u16)> {
    let layout = debug_layout(width, height)?;
    Some((
        layout.source.x,
        width - layout.source.x - layout.source.width,
        height - layout.source.height,
    ))
}

/// The coordinated arrangement for a terminal of this size. `None` when there is no room for a
/// debug layout worth having, in which case the caller leaves the ordinary workspace alone.
pub(crate) fn debug_layout(width: u16, height: u16) -> Option<DebugLayout> {
    if height < DEBUG_SOURCE_MIN_HEIGHT + 2 || width < DEBUG_SOURCE_MIN_WIDTH + 2 {
        return None;
    }
    let wide = width >= DEBUG_WIDE_MIN_WIDTH
        && width >= DEBUG_SIDE_WIDTH * 2 + DEBUG_SOURCE_MIN_WIDTH
        && height >= DEBUG_CONSOLE_HEIGHT + DEBUG_SOURCE_MIN_HEIGHT;
    if !wide {
        // NARROW: the source keeps its width, and the inspectors are replaced by ONE selectable
        // inspector in a strip below it rather than squeezing the source into four columns.
        // Tabs can wrap, but the heading and at least one value must still fit below them.
        let tab_rows = DebugPane::ALL
            .into_iter()
            .filter_map(|selected| {
                pane_tabs(
                    WindowRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    selected,
                )
                .last()
                .map(|(_, _, _, y)| y + 1)
            })
            .max()
            .unwrap_or(0);
        let strip = DEBUG_CONSOLE_HEIGHT.min(height / 3).max(tab_rows + 2);
        if height - strip < DEBUG_SOURCE_MIN_HEIGHT {
            return None;
        }
        return Some(DebugLayout {
            mode: DebugLayoutMode::Narrow,
            source: WindowRect {
                x: 0,
                y: 0,
                width,
                height: height - strip,
            },
            left: None,
            right: None,
            console: Some(WindowRect {
                x: 0,
                y: height - strip,
                width,
                height: strip,
            }),
        });
    }
    let console_height = DEBUG_CONSOLE_HEIGHT.min(height / 3);
    let body = height - console_height;
    // A wide terminal has room to read values rather than clip them.
    let side_width = (width / 5).clamp(DEBUG_SIDE_WIDTH, 48);
    let source_width = width - side_width * 2;
    Some(DebugLayout {
        mode: DebugLayoutMode::Wide,
        source: WindowRect {
            x: side_width,
            y: 0,
            width: source_width,
            height: body,
        },
        left: Some(WindowRect {
            x: 0,
            y: 0,
            width: side_width,
            height: body,
        }),
        right: Some(WindowRect {
            x: side_width + source_width,
            y: 0,
            width: side_width,
            height: body,
        }),
        console: Some(WindowRect {
            x: 0,
            y: body,
            width,
            height: console_height,
        }),
    })
}

/// What the debug panes SHOW, projected from the session's own state: what it is attached to or
/// running, the breakpoints it holds, its watches and its console. The arrangement is decided by
/// [`debug_layout`]; this is only the content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugPanesUiState {
    /// The inspector a NARROW terminal shows, one at a time.
    pub selected: DebugPane,
    /// What the session IS: attached to what, or which program it launched.
    pub target: String,
    /// Running, or why it is paused.
    pub status: String,
    pub threads: Vec<String>,
    pub stack: Vec<String>,
    pub breakpoints: Vec<String>,
    pub variables: Vec<String>,
    pub watches: Vec<String>,
    pub console: Vec<String>,
}

/// Cuts text to the width, ending in an ellipsis so a cut line doesn't read as the whole value.
fn clip(text: &str, width: u16, grid: &crate::ui::CellGrid) -> String {
    let width = usize::from(width);
    if grid.display_width(text) <= width {
        return text.to_owned();
    }
    let ellipsis = grid.display_width("…").min(width);
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grid.display_width(grapheme).clamp(1, 2);
        if used + cells > width.saturating_sub(ellipsis) {
            break;
        }
        result.push_str(grapheme);
        used += cells;
    }
    if ellipsis > 0 {
        result.push('…');
    }
    result
}

fn fill(grid: &mut crate::ui::CellGrid, rect: WindowRect) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            grid.set_cell(x, y, " ", crate::ui::Style::Default);
        }
    }
}

fn rows(pane: DebugPane, panes: &DebugPanesUiState) -> Vec<String> {
    let body = match pane {
        DebugPane::Console => panes.console.iter().rev().take(64).rev().cloned().collect(),
        DebugPane::Watches => panes.watches.clone(),
        DebugPane::Breakpoints => panes.breakpoints.clone(),
        DebugPane::Threads => panes.threads.clone(),
        DebugPane::Stack => panes.stack.clone(),
        DebugPane::Variables => panes.variables.clone(),
        DebugPane::Sessions => vec![panes.target.clone(), panes.status.clone()],
    };
    if body.is_empty() {
        vec![
            match pane {
                DebugPane::Console => "No output yet",
                DebugPane::Watches => "No watches",
                DebugPane::Breakpoints => "No breakpoints",
                DebugPane::Threads => "No threads",
                DebugPane::Stack => "No stack frames",
                DebugPane::Variables => "No variables in this frame",
                DebugPane::Sessions => "No debug session",
            }
            .to_owned(),
        ]
    } else {
        body
    }
}

fn draw_region(grid: &mut crate::ui::CellGrid, rect: WindowRect, title: &str, body: &[String]) {
    fill(grid, rect);
    let inner = rect.width.saturating_sub(2);
    grid.put_text_clipped(
        rect.x + 1,
        rect.y,
        &clip(title, inner, grid),
        crate::ui::Style::Accent,
        rect.x.saturating_add(rect.width),
    );
    for (index, line) in body
        .iter()
        .take(rect.height.saturating_sub(1) as usize)
        .enumerate()
    {
        let y = rect.y + 1 + index as u16;
        if y >= rect.y.saturating_add(rect.height) {
            break;
        }
        grid.put_text_clipped(
            rect.x + 1,
            y,
            &clip(line, inner, grid),
            crate::ui::Style::Muted,
            rect.x.saturating_add(rect.width),
        );
    }
}

/// Draw the coordinated arrangement into the regions it reserved. The workspace content was
/// rendered at `layout.source`, so these regions hold no source: the panels cannot cover it.
pub(crate) fn render_debug_panes(
    frame: &mut crate::ui::grid::Frame,
    layout: DebugLayout,
    panes: &DebugPanesUiState,
) {
    match layout.mode {
        DebugLayoutMode::Wide => {
            if let Some(left) = layout.left {
                // Sessions, THREADS, STACK and breakpoints on ONE side.
                let mut body = rows(DebugPane::Sessions, panes);
                for (title, pane) in [
                    ("threads:", DebugPane::Threads),
                    ("stack:", DebugPane::Stack),
                    ("breakpoints:", DebugPane::Breakpoints),
                ] {
                    let mut values = rows(pane, panes);
                    // A condition is what the breakpoint means, so it wraps rather than cuts.
                    if pane == DebugPane::Breakpoints {
                        values = wrap_rows(&values, left.width.saturating_sub(2), &frame.grid);
                    }
                    if !values.is_empty() {
                        body.push(title.to_owned());
                        body.extend(values);
                    }
                }
                draw_region(&mut frame.grid, left, "Debug", &body);
            }
            if let Some(right) = layout.right {
                // Variables and watches on the OTHER.
                let mut body = rows(DebugPane::Variables, panes);
                let watches = rows(DebugPane::Watches, panes);
                if !watches.is_empty() {
                    body.push("watches:".to_owned());
                    body.extend(watches);
                }
                let body = wrap_rows(&body, right.width.saturating_sub(2), &frame.grid);
                draw_region(&mut frame.grid, right, "Variables · Watches", &body);
            }
            if let Some(console) = layout.console {
                // The newest output is what matters, so the tail is what fits.
                let body = rows(DebugPane::Console, panes);
                let start = body
                    .len()
                    .saturating_sub(console.height.saturating_sub(1) as usize);
                draw_region(&mut frame.grid, console, "Console", &body[start..]);
            }
        }
        DebugLayoutMode::Narrow => {
            if let Some(strip) = layout.console {
                // Narrow terminals replace the panes with SELECTABLE inspectors: the strip names
                // every inspector and shows ONE, so the source keeps its width.
                fill(&mut frame.grid, strip);
                // The selector WRAPS rather than truncating: a narrow terminal is exactly where
                // every inspector must still be reachable, so no name may be clipped away.
                let inner = strip.width.saturating_sub(2);
                let mut x = strip.x + 1;
                let mut y = strip.y;
                let mut content_top = strip.y;
                for (_, label, tab_x, tab_y) in pane_tabs(strip, panes.selected) {
                    frame.grid.put_text_clipped(
                        tab_x,
                        tab_y,
                        &label,
                        crate::ui::Style::Default,
                        strip.x.saturating_add(strip.width),
                    );
                    x = tab_x.saturating_add(label.chars().count() as u16 + 1);
                    y = tab_y;
                    content_top = tab_y.saturating_add(1);
                }
                const NEXT_PANE_HINT: &str = "· <leader>dn";
                let hint_width = NEXT_PANE_HINT.chars().count() as u16;
                if x.saturating_sub(strip.x + 1) + hint_width <= inner {
                    frame.grid.put_text_clipped(
                        x,
                        y,
                        NEXT_PANE_HINT,
                        crate::ui::Style::Muted,
                        strip.x.saturating_add(strip.width),
                    );
                }
                let mut body = rows(panes.selected, panes);
                if panes.selected == DebugPane::Console {
                    let room = usize::from(
                        strip
                            .y
                            .saturating_add(strip.height)
                            .saturating_sub(content_top)
                            .saturating_sub(1),
                    );
                    body = body.split_off(body.len().saturating_sub(room));
                }
                body.insert(0, format!("{}:", panes.selected.label()));
                for (index, line) in body
                    .iter()
                    .take(strip.height.saturating_sub(content_top - strip.y) as usize)
                    .enumerate()
                {
                    let row = content_top.saturating_add(index as u16);
                    if row >= strip.y.saturating_add(strip.height) {
                        break;
                    }
                    frame.grid.put_text_clipped(
                        strip.x + 1,
                        row,
                        &clip(line, inner, &frame.grid),
                        crate::ui::Style::Muted,
                        strip.x.saturating_add(strip.width),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::render::debug::*;

    #[test]
    fn wide_inspector_rows_stay_inside_their_region() {
        let mut grid = crate::ui::CellGrid::new_with_policy(80, 10, "�", "◌", true);
        for x in 30..80 {
            grid.set_cell(x, 1, "x", crate::ui::Style::Default);
        }
        let rect = WindowRect {
            x: 0,
            y: 0,
            width: 30,
            height: 8,
        };
        draw_region(&mut grid, rect, "Debug", &["界".repeat(20)]);
        assert_eq!(grid.get(30, 1).unwrap().symbol, "x");
        let wrapped = wrap_rows(&["·".repeat(20)], 10, &grid);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|row| grid.display_width(row) <= 10));
    }

    #[test]
    fn narrow_wrapping_leaves_room_for_wide_graphemes() {
        for wide in [false, true] {
            let grid = crate::ui::CellGrid::new_with_policy(3, 10, "�", "◌", wide);
            let rows = wrap_rows(&["1. 界界".into()], 3, &grid);
            assert_eq!(rows, ["1.", " 界", " 界"]);
            assert!(rows.iter().all(|row| grid.display_width(row) <= 3));
        }
    }

    #[test]
    fn narrow_layout_keeps_a_value_row_below_tabs_and_heading() {
        assert!(debug_layout(80, 8).is_none());
        for width in [42, 60, 80] {
            for height in 8..24 {
                let Some(layout) = debug_layout(width, height) else {
                    continue;
                };
                assert!(layout.source.height >= DEBUG_SOURCE_MIN_HEIGHT);
                let strip = layout.console.unwrap();
                for selected in DebugPane::ALL {
                    let tabs = pane_tabs(strip, selected);
                    assert_eq!(tabs.len(), DebugPane::ALL.len());
                    let last_tab_y = tabs.last().unwrap().3;
                    assert!(last_tab_y + 2 < strip.y + strip.height);
                }
            }
        }
    }

    #[test]
    fn empty_inspectors_show_a_hint_at_release_terminal_sizes() {
        for (width, height) in [(160, 45), (100, 30), (80, 24)] {
            for selected in DebugPane::ALL {
                let mut frame = crate::ui::grid::Frame {
                    grid: crate::ui::CellGrid::new(width, height),
                    cursor: None,
                };
                let panes = DebugPanesUiState {
                    selected,
                    target: "python: launched demo.py".into(),
                    status: "paused: entry".into(),
                    threads: Vec::new(),
                    stack: Vec::new(),
                    breakpoints: Vec::new(),
                    variables: Vec::new(),
                    watches: Vec::new(),
                    console: Vec::new(),
                };
                render_debug_panes(&mut frame, debug_layout(width, height).unwrap(), &panes);
                let screen = (0..height)
                    .map(|y| frame.grid.row_text(y))
                    .collect::<Vec<_>>()
                    .join("\n");
                if width >= DEBUG_WIDE_MIN_WIDTH {
                    for hint in [
                        "No threads",
                        "No stack frames",
                        "No breakpoints",
                        "No variables",
                        "No watches",
                        "No output yet",
                    ] {
                        assert!(screen.contains(hint), "{screen}");
                    }
                } else {
                    assert!(screen.contains(&rows(selected, &panes)[0]), "{screen}");
                }
            }
        }
    }

    #[test]
    fn narrow_console_shows_the_newest_output() {
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(80, 24),
            cursor: None,
        };
        let layout = debug_layout(80, 24).unwrap();
        assert_eq!(layout.mode, DebugLayoutMode::Narrow);
        let panes = DebugPanesUiState {
            selected: DebugPane::Console,
            target: String::new(),
            status: String::new(),
            threads: Vec::new(),
            stack: Vec::new(),
            breakpoints: Vec::new(),
            variables: Vec::new(),
            watches: Vec::new(),
            console: (0..100).map(|n| format!("output {n}")).collect(),
        };
        render_debug_panes(&mut frame, layout, &panes);
        assert!((0..24).any(|row| frame.grid.row_text(row).contains("output 99")));
    }
}
