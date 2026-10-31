//! The coordinated debug arrangement.
//!
//! W-25: "sessions, threads, stack and breakpoints on one side, the dominant source in the
//! centre, lazily loaded variables and watches on the other, and an interactive console bound to
//! the selected frame. Narrow terminals replace panes with selectable inspectors rather than
//! compressing the source below a readable minimum."
//!
//! The arrangement is computed HERE, as one decision, so the regions cannot disagree with each
//! other: the source is whatever the inspectors leave, and the inspectors exist only when the
//! source can still be read.

use super::WindowRect;

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
        let strip = DEBUG_CONSOLE_HEIGHT.min(height / 3);
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

fn clip(text: &str, width: u16) -> String {
    text.chars().take(width as usize).collect()
}

fn fill(grid: &mut super::CellGrid, rect: WindowRect) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            grid.set_cell(x, y, " ", super::Style::Default);
        }
    }
}

fn rows(pane: DebugPane, panes: &DebugPanesUiState) -> Vec<String> {
    match pane {
        DebugPane::Console => panes.console.iter().rev().take(64).rev().cloned().collect(),
        DebugPane::Watches => panes.watches.clone(),
        DebugPane::Breakpoints => panes.breakpoints.clone(),
        DebugPane::Threads => panes.threads.clone(),
        DebugPane::Stack => panes.stack.clone(),
        DebugPane::Variables => panes.variables.clone(),
        DebugPane::Sessions => vec![panes.target.clone(), panes.status.clone()],
    }
}

fn draw_region(grid: &mut super::CellGrid, rect: WindowRect, title: &str, body: &[String]) {
    fill(grid, rect);
    let inner = rect.width.saturating_sub(2);
    grid.put_text(rect.x + 1, rect.y, &clip(title, inner), super::Style::Accent);
    for (index, line) in body.iter().take(rect.height.saturating_sub(1) as usize).enumerate() {
        let y = rect.y + 1 + index as u16;
        if y >= rect.y.saturating_add(rect.height) {
            break;
        }
        grid.put_text(rect.x + 1, y, &clip(line, inner), super::Style::Muted);
    }
}

/// Draw the coordinated arrangement into the regions it reserved. The workspace content was
/// rendered at `layout.source`, so these regions hold no source: the panels cannot cover it.
pub(crate) fn render_debug_panes(
    frame: &mut super::Frame,
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
                    let values = rows(pane, panes);
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
                draw_region(&mut frame.grid, right, "Variables · Watches", &body);
            }
            if let Some(console) = layout.console {
                // The newest output is what matters, so the tail is what fits.
                let body = rows(DebugPane::Console, panes);
                let start = body.len().saturating_sub(console.height.saturating_sub(1) as usize);
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
                for pane in DebugPane::ALL {
                    let label = if pane == panes.selected {
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
                    frame.grid.put_text(x, y, &label, super::Style::Border);
                    x = x.saturating_add(width);
                    content_top = y.saturating_add(1);
                }
                let mut body = rows(panes.selected, panes);
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
                    frame.grid.put_text(
                        strip.x + 1,
                        row,
                        &clip(line, inner),
                        super::Style::Muted,
                    );
                }
            }
        }
    }
}
