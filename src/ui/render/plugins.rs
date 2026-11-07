//! Retained plugin trees and private cell surface drawing.

use crate::ui::Style;
use crate::ui::grid::Frame;
use crate::ui::render::PluginSurfaceUiState;
use crate::ui::render::PrivateSurfaceUiState;
use crate::windows::WindowRect;
use crossterm::cursor::SetCursorStyle;

pub(super) fn render_plugin_surface(
    frame: &mut Frame,
    surface: &PluginSurfaceUiState,
    rect: WindowRect,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let right = rect.x.saturating_add(rect.width).min(frame.grid.width());
    let bottom = rect.y.saturating_add(rect.height).min(frame.grid.height());
    for y in rect.y..bottom {
        for x in rect.x..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    for (offset, row) in surface.rows.iter().skip(surface.scroll).enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        let y = rect.y.saturating_add(offset);
        if y >= bottom {
            break;
        }
        let indent = row
            .depth
            .saturating_mul(2)
            .min(rect.width.saturating_sub(1));
        let x = rect.x.saturating_add(indent);
        let prefix = if row.actionable { "› " } else { "" };
        let mut text = String::with_capacity(prefix.len().saturating_add(row.text.len()));
        text.push_str(prefix);
        text.push_str(&row.text);
        frame.grid.put_text_clipped(
            x,
            y,
            &text,
            if row.focused {
                Style::Selection
            } else {
                row.style
            },
            right,
        );
    }
}

pub(super) fn render_private_surface(
    frame: &mut Frame,
    surface: &PrivateSurfaceUiState,
    rect: WindowRect,
) {
    if rect.width == 0
        || rect.height == 0
        || surface.width != rect.width
        || surface.height != rect.height
        || surface.dimensions_revision == 0
        || surface.surface_revision == 0
    {
        return;
    }
    let right = rect.x.saturating_add(rect.width).min(frame.grid.width());
    let bottom = rect.y.saturating_add(rect.height).min(frame.grid.height());
    for y in rect.y..bottom {
        for x in rect.x..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    for cell in &surface.cells {
        if cell.continuation || cell.x >= rect.width || cell.y >= rect.height {
            continue;
        }
        let x = rect.x.saturating_add(cell.x);
        let y = rect.y.saturating_add(cell.y);
        if x < right && y < bottom {
            // Private snapshots use narrow Unicode widths for their fixed coordinates.
            let width = unicode_width::UnicodeWidthStr::width(cell.grapheme.as_str()).clamp(1, 2);
            if usize::from(x) + width <= usize::from(right) {
                frame
                    .grid
                    .set_cell_with_width(x, y, &cell.grapheme, cell.style, width);
            }
        }
    }
    if surface.focused {
        frame.cursor = surface
            .cursor
            .filter(|(x, y)| *x < rect.width && *y < rect.height)
            .map(|(x, y)| {
                (
                    rect.x.saturating_add(x),
                    rect.y.saturating_add(y),
                    SetCursorStyle::SteadyBlock,
                )
            });
    }
}
