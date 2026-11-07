//! Documentation popup drawing.

use crate::config::AmbiguousWidth;
use crate::config::BorderStyle;
use crate::config::Config;
use crate::ui::Style;
use crate::ui::geometry::cell_text_width;
use crate::ui::geometry::text_width;
use crate::ui::grid::Frame;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::windows::WindowRect;

pub(crate) const DOCUMENTATION_MAX_WIDTH: u16 = 72;

pub(crate) const DOCUMENTATION_MAX_HEIGHT: u16 = 18;

pub(crate) const DOCUMENTATION_PADDING: u16 = 1;

pub(crate) const DOCUMENTATION_SCROLL_STEP: usize = 4;

pub(super) fn render_documentation_popup(
    frame: &mut Frame,
    popup: &DocumentationPopupState,
    config: &Config,
    bounds: WindowRect,
    completion_menu: Option<WindowRect>,
) -> Option<(usize, usize)> {
    let enabled = config.ui.documentation_popup.enabled;
    let editor_left = bounds.x;
    let editor_width = bounds.width;
    let editor_right = editor_left.saturating_add(editor_width);
    let editor_top = bounds.y;
    let editor_bottom = editor_top.saturating_add(bounds.height);
    let available_height = bounds.height;
    let line_count = popup.line_count();
    if !enabled || line_count == 0 || editor_width < 4 || available_height < 3 {
        return None;
    }
    let page_rows = crate::ui::render::documentation::documentation_page_rows(
        config,
        editor_width,
        available_height,
    );
    let display_count = documentation_display_rows(popup, config, editor_width);
    let scrollable = display_count > page_rows;
    let widest = (0..line_count)
        .filter_map(|index| popup.line_at(index))
        .map(|line| cell_text_width(line, config.editor.ambiguous_width))
        .chain(std::iter::once(cell_text_width(
            &popup.title,
            config.editor.ambiguous_width,
        )))
        .chain(scrollable.then_some(text_width(
            "j/k or wheel · 0000-0000/0000",
            config.editor.ambiguous_width,
        )))
        .max()
        .unwrap_or(1)
        .saturating_add(usize::from(DOCUMENTATION_PADDING).saturating_mul(2))
        .saturating_add(2);
    let width_limit = if popup.kind == DocumentationPopupKind::Completion {
        editor_width.saturating_div(2).max(4)
    } else {
        editor_width
    };
    let mut box_width = u16::try_from(widest)
        .unwrap_or(DOCUMENTATION_MAX_WIDTH)
        .min(DOCUMENTATION_MAX_WIDTH)
        .min(width_limit);
    let scroll = popup.scroll.min(display_count.saturating_sub(page_rows));
    let visible_rows = display_count.saturating_sub(scroll).min(page_rows);
    let mut box_height = u16::try_from(visible_rows.saturating_add(2))
        .unwrap_or(DOCUMENTATION_MAX_HEIGHT)
        .min(DOCUMENTATION_MAX_HEIGHT)
        .min(available_height);
    if box_width < 4 || box_height < 3 {
        return None;
    }
    // The dashboard has no cursor to sit beside, so the box goes mid-screen there.
    let (cursor_x, cursor_y) = frame.cursor.map_or(
        (
            editor_left.saturating_add(editor_width.saturating_sub(box_width) / 2),
            editor_top.saturating_add(
                (available_height.saturating_sub(box_height) / 2).saturating_sub(1),
            ),
        ),
        |(x, y, _)| (x, y),
    );
    let below = cursor_y.saturating_add(1);
    let room_below = editor_bottom.saturating_sub(below);
    let top = if room_below >= box_height || cursor_y.saturating_sub(editor_top) < box_height {
        below
            .min(editor_bottom.saturating_sub(box_height))
            .max(editor_top)
    } else {
        cursor_y.saturating_sub(box_height).max(editor_top)
    };
    let left = if popup.kind == DocumentationPopupKind::Completion {
        let midpoint = editor_left.saturating_add(editor_width / 2);
        if cursor_x < midpoint {
            editor_right.saturating_sub(box_width)
        } else {
            editor_left
        }
    } else if cursor_x.saturating_add(box_width) <= editor_right {
        cursor_x.max(editor_left)
    } else {
        cursor_x
            .saturating_add(1)
            .saturating_sub(box_width)
            .max(editor_left)
    };
    let (left, top) =
        match completion_menu.filter(|_| popup.kind == DocumentationPopupKind::Completion) {
            None => (left, top),
            Some(menu) => {
                let beside_right = menu.x.saturating_add(menu.width);
                let below_menu = menu.y.saturating_add(menu.height);
                let spot = [
                    (beside_right, menu.y),
                    (menu.x.saturating_sub(box_width), menu.y),
                    (left, top),
                    (beside_right, top),
                    (menu.x.saturating_sub(box_width), top),
                    (left, below_menu.max(cursor_y.saturating_add(1))),
                    (left, menu.y.min(cursor_y).saturating_sub(box_height)),
                ]
                .into_iter()
                .find(|(x, y)| {
                    *x >= editor_left
                        && x.saturating_add(box_width) <= editor_right
                        && *y >= editor_top
                        && y.saturating_add(box_height) <= editor_bottom
                        && !boxes_overlap((*x, *y), (box_width, box_height), menu)
                        && !(*y <= cursor_y && y.saturating_add(box_height) > cursor_y)
                });
                match spot {
                    Some(spot) => spot,
                    None => {
                        // Too big for anywhere clear of the menu: a narrower box on the wider side,
                        // as tall as fits from the menu's top without reaching the cursor's line,
                        // or none at all rather than one over the items.
                        let right_room = editor_right.saturating_sub(beside_right);
                        let left_room = menu.x.saturating_sub(editor_left);
                        if right_room.max(left_room) < 20 {
                            return None;
                        }
                        box_width = box_width.min(right_room.max(left_room));
                        let room = if menu.y > cursor_y {
                            editor_bottom.saturating_sub(menu.y)
                        } else {
                            cursor_y.saturating_sub(menu.y)
                        };
                        if room < 3 {
                            return None;
                        }
                        box_height = box_height.min(room);
                        if right_room >= left_room {
                            (beside_right, menu.y)
                        } else {
                            (menu.x - box_width, menu.y)
                        }
                    }
                }
            }
        };
    // Lines wider than the box wrap at spaces instead of losing their ends, and the box grows
    // down for them while that stays clear of the menu and the cursor's line.
    let inner = usize::from(
        box_width
            .saturating_sub(2)
            .saturating_sub(DOCUMENTATION_PADDING.saturating_mul(2)),
    )
    .max(1);
    let rows = (0..line_count)
        .filter_map(|index| popup.at(index).map(|(line, spans)| (index, line, spans)))
        .flat_map(|(index, line, spans)| {
            wrap_ranges(line, inner, config.editor.ambiguous_width)
                .into_iter()
                .map(move |range| (index, line, spans, range))
        })
        .collect::<Vec<_>>();
    let display_count = rows.len();
    let scroll = popup.scroll.min(display_count.saturating_sub(page_rows));
    let wanted = u16::try_from(
        display_count
            .saturating_sub(scroll)
            .min(page_rows)
            .saturating_add(2),
    )
    .unwrap_or(u16::MAX)
    .min(DOCUMENTATION_MAX_HEIGHT)
    .min(editor_bottom.saturating_sub(top));
    if wanted > box_height
        && completion_menu
            .filter(|_| popup.kind == DocumentationPopupKind::Completion)
            .is_none_or(|menu| !boxes_overlap((left, top), (box_width, wanted), menu))
        && !(top <= cursor_y && top.saturating_add(wanted) > cursor_y)
    {
        box_height = wanted;
    }
    let visible_rows = usize::from(box_height.saturating_sub(2)).min(display_count);
    let scroll = popup.scroll.min(display_count.saturating_sub(visible_rows));
    let scrollable = display_count > visible_rows;
    let right = left.saturating_add(box_width);
    let bottom = top.saturating_add(box_height).saturating_sub(1);
    for y in top..=bottom {
        for x in left..right {
            frame
                .grid
                .set_cell(x, y, " ", Style::DocumentationBackground);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        frame.grid.border_glyphs(BorderStyle::Rounded);
    for x in left.saturating_add(1)..right.saturating_sub(1) {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::DocumentationBorder);
        frame.grid.set_cell(
            x,
            bottom,
            &horizontal.to_string(),
            Style::DocumentationBorder,
        );
    }
    for y in top.saturating_add(1)..bottom {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::DocumentationBorder);
        frame.grid.set_cell(
            right.saturating_sub(1),
            y,
            &vertical.to_string(),
            Style::DocumentationBorder,
        );
    }
    for (x, y, symbol) in [
        (left, top, top_left),
        (right.saturating_sub(1), top, top_right),
        (left, bottom, bottom_left),
        (right.saturating_sub(1), bottom, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &symbol.to_string(), Style::DocumentationBorder);
    }
    frame.grid.put_text_clipped(
        left.saturating_add(1),
        top,
        &popup.title,
        Style::DocumentationTitle,
        right.saturating_sub(1),
    );
    let text_left = left.saturating_add(1).saturating_add(DOCUMENTATION_PADDING);
    for (row, (index, line, spans, wrapped)) in
        rows.iter().skip(scroll).take(visible_rows).enumerate()
    {
        let index = *index;
        // A hunk preview reads as a diff.
        let style = match line.chars().next() {
            Some('+') if popup.provider == "Git" => Style::GitSignAdded,
            Some('-') if popup.provider == "Git" => Style::GitSignDeleted,
            _ if popup.heading_at(index) => Style::DocumentationTitle,
            _ => Style::DocumentationText,
        };
        let y = top
            .saturating_add(1)
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        let limit = right
            .saturating_sub(1)
            .saturating_sub(DOCUMENTATION_PADDING);
        // Rendered Markdown: the runs between styled ones keep the line's style.
        let mut x = text_left;
        let mut at = wrapped.start;
        for (range, role) in spans.iter() {
            let (start, end) = (range.start.max(at), range.end.min(wrapped.end));
            let (Some(before), Some(styled)) = (line.get(at..start), line.get(start..end)) else {
                continue;
            };
            if start >= end {
                continue;
            }
            x = frame.grid.put_text_clipped(x, y, before, style, limit);
            x = frame
                .grid
                .put_text_clipped(x, y, styled, Style::DocumentationSyntax(*role), limit);
            at = end;
        }
        frame
            .grid
            .put_text_clipped(x, y, line.get(at..wrapped.end).unwrap_or(""), style, limit);
    }
    if scrollable {
        let footer = format!(
            " j/k or wheel · {}-{}/{} ",
            scroll.saturating_add(1),
            scroll.saturating_add(visible_rows).min(display_count),
            display_count
        );
        frame.grid.put_text_clipped(
            left.saturating_add(1),
            bottom,
            &footer,
            Style::DocumentationTitle,
            right.saturating_sub(1),
        );
    }
    Some((visible_rows, display_count))
}

pub(crate) fn documentation_display_rows(
    popup: &DocumentationPopupState,
    config: &Config,
    editor_width: u16,
) -> usize {
    let limit = if popup.kind == DocumentationPopupKind::Completion {
        editor_width / 2
    } else {
        editor_width
    };
    let widest = (0..popup.line_count())
        .filter_map(|i| popup.line_at(i))
        .chain(std::iter::once(popup.title.as_str()))
        .map(|line| cell_text_width(line, config.editor.ambiguous_width))
        .max()
        .unwrap_or(1);
    let inner = widest
        .saturating_add(4)
        .min(usize::from(DOCUMENTATION_MAX_WIDTH.min(limit)))
        .saturating_sub(4)
        .max(1);
    (0..popup.line_count())
        .filter_map(|i| popup.line_at(i))
        .map(|line| wrap_ranges(line, inner, config.editor.ambiguous_width).len())
        .sum()
}

/// Two cells read as a square swatch; the third is the gap before the detail.
pub(crate) const SWATCH_WIDTH: usize = 3;

/// A cell painted in a colour from a language server, with text that stays readable on it.
pub(super) fn color_swatch_style((red, green, blue): (u8, u8, u8)) -> Style {
    let luminance = 299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue);
    let text = if luminance > 128_000 {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    };
    Style::Terminal(crate::ui::PtyStyle {
        foreground: Some(crate::ui::PtyColor::Rgb(text.0, text.1, text.2)),
        background: Some(crate::ui::PtyColor::Rgb(red, green, blue)),
        ..Default::default()
    })
}

/// The byte ranges of `line` that fit `width` cells each, broken after a space when there is
/// one, else mid-word.
pub(crate) fn wrap_ranges(
    line: &str,
    width: usize,
    ambiguous: AmbiguousWidth,
) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let (mut used, mut end, mut space) = (0, start, None);
        for (offset, grapheme) in line[start..].grapheme_indices(true) {
            let cells = cell_text_width(grapheme, ambiguous);
            if used + cells > width && end > start {
                break;
            }
            used += cells;
            end = start + offset + grapheme.len();
            if grapheme == " " {
                space = Some(end);
            }
        }
        // The final row needs no word break. Scan only the row being wrapped, not
        // the entire remaining message on every iteration.
        let cut = if end == line.len() {
            end
        } else {
            space.unwrap_or(end)
        };
        rows.push(start..cut);
        start = cut;
    }
    if rows.is_empty() {
        rows.push(0..0);
    }
    rows
}

fn boxes_overlap(at: (u16, u16), size: (u16, u16), other: WindowRect) -> bool {
    at.0 < other.x.saturating_add(other.width)
        && other.x < at.0.saturating_add(size.0)
        && at.1 < other.y.saturating_add(other.height)
        && other.y < at.1.saturating_add(size.1)
}

#[must_use]
pub fn documentation_page_rows(config: &Config, width: u16, height: u16) -> usize {
    if !config.ui.documentation_popup.enabled || width < 4 {
        return 0;
    }
    usize::from(DOCUMENTATION_MAX_HEIGHT.saturating_sub(2))
        .min(usize::from(height.saturating_sub(2)))
}
use unicode_segmentation::UnicodeSegmentation;
