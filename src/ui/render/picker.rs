//! Picker rows, selection and preview drawing.

use crate::config::BorderStyle;
use crate::config::Config;
#[cfg(test)]
use crate::ui::CellGrid;
use crate::ui::DiffLayout;
use crate::ui::DiffPreview;
use crate::ui::Style;
use crate::ui::diff::DiffRow;
use crate::ui::diff::DiffRowKind;
use crate::ui::diff::DiffSideRow;
use crate::ui::geometry::text_width;
use crate::ui::grid::Frame;
use crate::ui::render::CellAlign;
use crate::ui::render::PickerPreviewBody;
use crate::ui::render::PickerRowKind;
use crate::ui::render::PickerUiState;
use crate::ui::render::editor::PROMPT_CURSOR;
use crate::ui::render::editor::cursor_style;
use crate::ui::render::overlays::truncate_content_title;
use crate::windows::WindowRect;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
use crate::ui::geometry::ellipsized;
use crate::ui::geometry::fitted_footer;
#[cfg(test)]
use crate::ui::render::prompt::prompt_viewport;

pub(crate) const PICKER_MAXIMUM_PREVIEW_BYTES: usize = 256 * 1024;

pub(crate) fn looks_like_path(text: &str) -> bool {
    text.contains('/') && !text.contains(char::is_whitespace)
}

/// A `Label  /some/path` detail row keeps its label and the end of its path.
pub(crate) fn clip_path_tail(
    line: &str,
    maximum: usize,
    ambiguous: crate::config::AmbiguousWidth,
) -> String {
    let path = line.rsplit(' ').next().unwrap_or_default();
    let label = &line[..line.len() - path.len()];
    let room = maximum.saturating_sub(text_width(label, ambiguous));
    format!("{label}{}", truncate_content_title(path, room, ambiguous))
}

/// A picker row's label and detail within `available` cells. A long path loses its start, since
/// its end is what tells two rows apart.
pub(super) fn fitted_picker_row(
    label: &str,
    detail: &str,
    available: usize,
    ambiguous: crate::config::AmbiguousWidth,
) -> (String, String) {
    let label_width = text_width(label, ambiguous);
    let detail_width = text_width(detail, ambiguous);
    if label_width + 2 + detail_width <= available || detail.is_empty() && label_width <= available
    {
        return (label.to_owned(), detail.to_owned());
    }
    let label = if looks_like_path(label) {
        let room = if detail.is_empty() {
            available
        } else {
            available * 2 / 3
        };
        truncate_content_title(label, room, ambiguous)
    } else {
        label.to_owned()
    };
    let room = available.saturating_sub(text_width(&label, ambiguous) + 2);
    let detail = if looks_like_path(detail) {
        truncate_content_title(detail, room, ambiguous)
    } else {
        detail.to_owned()
    };
    (label, detail)
}

/// Draw a diff that was carried as structure rather than flattened text.
///
/// This is the base form: the provenance line, each hunk header, the exact count of any
/// unchanged run collapsed before a hunk, and every line with both of its numbers and a
/// classification glyph. Intra-line emphasis, side-by-side mode and syntax highlighting are
/// the diff renderer's own work and are not claimed here.
/// The style a diff row's own text is drawn in.
fn diff_row_style(kind: DiffRowKind) -> Style {
    match kind {
        DiffRowKind::Provenance | DiffRowKind::Collapsed => Style::Muted,
        DiffRowKind::Header => Style::Accent,
        DiffRowKind::Added => Style::StatusAccent,
        DiffRowKind::Removed => Style::Error,
        DiffRowKind::Context => Style::Default,
        DiffRowKind::Truncated => Style::Warning,
    }
}

/// Draw one diff row, emphasising the spans that changed inside it.
///
/// The emphasis uses the theme's search style, which is the existing style for highlighted
/// text. A dedicated diff-emphasis style would be a theme-schema change and U-16 does not
/// ask for one.
fn paint_diff_row(frame: &mut Frame, _config: &Config, x: u16, y: u16, right: u16, row: &DiffRow) {
    let style = diff_row_style(row.kind);
    if row.spans.is_empty() {
        frame.grid.put_text_clipped(x, y, &row.text, style, right);
        return;
    }
    let mut cursor_x = x;
    for (at, grapheme) in row.text.grapheme_indices(true) {
        let changed = row
            .spans
            .iter()
            .any(|span| span.start < at + grapheme.len() && span.end > at);
        let next_x = frame.grid.put_text_clipped(
            cursor_x,
            y,
            grapheme,
            if changed { Style::Search } else { style },
            right,
        );
        if next_x == cursor_x {
            break;
        }
        cursor_x = next_x;
    }
}

fn render_diff_preview(
    frame: &mut Frame,
    diff: &DiffPreview,
    side_scroll: (usize, usize),
    config: &Config,
    rect: WindowRect,
) {
    let number_width = diff.number_width();
    let (left, top) = (rect.x, rect.y);
    let right = rect.x.saturating_add(rect.width);
    let bottom = rect.y.saturating_add(rect.height);
    match diff.layout {
        DiffLayout::Unified => {
            let rows = diff.rows();
            let pinned = diff.pinned_rows();
            let scrolled = rows.iter().skip(pinned.saturating_add(side_scroll.0));
            let mut y = top;
            for row in rows.iter().take(pinned).chain(scrolled) {
                if y >= bottom {
                    break;
                }
                paint_diff_row(frame, config, left, y, right, row);
                y = y.saturating_add(1);
            }
        }
        DiffLayout::SideBySide => {
            // The divider takes one column and the two sides split what remains.
            let width = right.saturating_sub(left);
            let side = width.saturating_sub(1) / 2;
            let divider = left.saturating_add(side);
            let left_right = divider;
            let right_left = divider.saturating_add(1);
            // Each side has its own sequence and its own offset, so one side scrolls without
            // moving the other. With equal offsets the two sides stay paired.
            let rows = diff.side_by_side_rows();
            let left_cells: Vec<&DiffSideRow> = rows.iter().collect();
            let (left_offset, right_offset) = side_scroll;
            let mut y = top;
            for (position, row) in rows.iter().enumerate() {
                if y >= bottom {
                    break;
                }
                if let Some(text) = &row.spanning {
                    let style = diff_row_style(row.kind);
                    frame.grid.put_text_clipped(left, y, text, style, right);
                    continue;
                }
                let left_row = left_cells
                    .get(position.saturating_add(left_offset))
                    .copied();
                let right_row = rows.get(position.saturating_add(right_offset));
                {
                    frame.grid.set_cell(divider, y, "│", Style::Border);
                    if let Some(cell) = left_row.and_then(|row| row.old.as_ref()) {
                        let (text, spans) =
                            DiffPreview::side_cell_text_with_width(cell, number_width);
                        paint_diff_row(
                            frame,
                            config,
                            left,
                            y,
                            left_right,
                            &DiffRow {
                                kind: DiffRowKind::Removed,
                                text,
                                spans,
                            },
                        );
                    }
                    if let Some(cell) = right_row.and_then(|row| row.new.as_ref()) {
                        let (text, spans) =
                            DiffPreview::side_cell_text_with_width(cell, number_width);
                        paint_diff_row(
                            frame,
                            config,
                            right_left,
                            y,
                            right,
                            &DiffRow {
                                kind: DiffRowKind::Added,
                                text,
                                spans,
                            },
                        );
                    }
                }
                y = y.saturating_add(1);
            }
        }
    }
}

#[must_use]
pub fn picker_geometry(bounds: WindowRect) -> Option<WindowRect> {
    let width = u16::try_from(u32::from(bounds.width) * 88 / 100)
        .unwrap_or(bounds.width)
        .max(40.min(bounds.width))
        .min(bounds.width);
    let height = u16::try_from(u32::from(bounds.height) * 82 / 100)
        .unwrap_or(bounds.height)
        .max(10.min(bounds.height))
        .min(bounds.height);
    if width < 4 || height < 6 {
        return None;
    }
    Some(WindowRect {
        x: bounds
            .x
            .saturating_add(bounds.width.saturating_sub(width) / 2),
        y: bounds
            .y
            .saturating_add(bounds.height.saturating_sub(height) / 2),
        width,
        height,
    })
}

pub(super) fn render_picker(
    frame: &mut Frame,
    picker: &PickerUiState,
    config: &Config,
    rect: WindowRect,
    geometry: Option<crate::ui::render::picker::PickerContentGeometry>,
) {
    let WindowRect {
        x: left,
        y: top,
        width,
        height,
    } = rect;
    if width < 4 || height < 6 {
        return;
    }
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        frame.grid.border_glyphs(BorderStyle::Single);
    for x in left..left + width {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::Border);
        frame
            .grid
            .set_cell(x, top + height - 1, &horizontal.to_string(), Style::Border);
    }
    for y in top..top + height {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::Border);
        frame
            .grid
            .set_cell(left + width - 1, y, &vertical.to_string(), Style::Border);
    }
    for (x, y, character) in [
        (left, top, top_left),
        (left + width - 1, top, top_right),
        (left, top + height - 1, bottom_left),
        (left + width - 1, top + height - 1, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &character.to_string(), Style::Border);
    }
    let preview = picker
        .preview
        .as_ref()
        .filter(|_| width >= 40 && height >= 12);
    let preview_number_width = preview.map_or(1, |preview| match &preview.body {
        PickerPreviewBody::Text(text) => text
            .first_line
            .saturating_add(text.lines.len())
            .max(1)
            .to_string()
            .len(),
        PickerPreviewBody::Diff(diff) => diff.number_width(),
    });
    let Some(geometry) = geometry
        .filter(|geometry| geometry.outer == rect)
        .or_else(|| crate::ui::render::picker::picker_content_geometry(picker, config, rect))
    else {
        return;
    };
    let divider = geometry.divider;
    let stacked_divider = geometry.stacked_divider;
    let list_right = geometry.list.x.saturating_add(geometry.list.width);
    if let Some(divider) = divider {
        for y in top + 1..top + height - 1 {
            frame
                .grid
                .set_cell(divider, y, &vertical.to_string(), Style::Border);
        }
    }
    if let Some(divider_y) = stacked_divider {
        for x in left + 1..left + width - 1 {
            frame
                .grid
                .set_cell(x, divider_y, &horizontal.to_string(), Style::Border);
        }
    }
    frame.grid.put_text_clipped(
        left + 2,
        top,
        &format!(" {} ", picker.title),
        Style::Accent,
        list_right,
    );
    // The footer spans the surface, not just the list column, because it describes the whole
    // operation in front of the user rather than the list widget.
    let footer_right = left.saturating_add(width).saturating_sub(1);
    frame.grid.put_text_clipped(
        left + 2,
        top + height - 1,
        &format!(
            " {} ",
            match picker.footer.as_deref() {
                Some(footer) => fitted_footer(
                    footer,
                    usize::from(
                        footer_right
                            .saturating_sub(left.saturating_add(2))
                            .saturating_sub(2)
                    ),
                    config.editor.ambiguous_width,
                ),
                None => picker.input_hint.clone(),
            }
        ),
        Style::Muted,
        footer_right,
    );
    if let Some(preview) = preview {
        let (title_x, title_y, title_right) = if let Some(divider) = divider {
            (divider + 2, top, left + width - 1)
        } else {
            (left + 2, stacked_divider.unwrap_or(top), left + width - 1)
        };
        frame.grid.put_text_clipped(
            title_x,
            title_y,
            &format!(" {} ", preview.title),
            Style::Accent,
            title_right,
        );
    }
    let content_left = left + 2;
    let query_y = top + 1;

    // As in LazyVim: how many entries the query leaves, of how many there are.
    let shown = picker
        .rows
        .iter()
        .filter(|row| row.kind == PickerRowKind::Entry)
        .count();
    let count = format!("{shown}/{}", picker.total.max(shown));
    let status = if picker.loading {
        format!("loading…  {count}")
    } else {
        count
    };
    let status_width =
        u16::try_from(text_width(&status, config.editor.ambiguous_width)).unwrap_or(0);
    let status_x = list_right.saturating_sub(status_width).saturating_sub(1);
    let query_right = if status_x > content_left {
        status_x.saturating_sub(1)
    } else {
        list_right
    };
    // Keep long queries inside their list, never over the count or preview.
    let query_width = usize::from(query_right.saturating_sub(content_left).saturating_sub(1));
    let mut start = picker.query.len();
    let mut used = 0usize;
    for (byte, grapheme) in
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(picker.query.as_str(), true)
            .rev()
    {
        used = used.saturating_add(text_width(grapheme, config.editor.ambiguous_width));
        if used > query_width {
            break;
        }
        start = byte;
    }
    let query = &picker.query[start..];
    frame
        .grid
        .put_text_clipped(content_left, query_y, query, Style::Default, query_right);
    if status_x > content_left {
        frame
            .grid
            .put_text_clipped(status_x, query_y, &status, Style::Muted, list_right);
    }
    for x in left + 1..list_right {
        frame
            .grid
            .set_cell(x, top + 2, &horizontal.to_string(), Style::Border);
    }
    let first_row = geometry.list.y;
    let visible_rows = usize::from(geometry.list.height);
    for (display_row, (index, row)) in picker
        .rows
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(visible_rows)
        .enumerate()
    {
        let y = first_row + u16::try_from(display_row).unwrap_or(0);
        let selected = picker.selected == Some(index);
        let marker = if selected { ">" } else { " " };
        // Every picker uses the same trailing provenance column. Reserve at
        // most a third of the row so long sources cannot crowd out the label.
        let source_right = list_right.saturating_sub(1);
        let mut source_room = source_right.saturating_sub(content_left) / 3;
        let source = if looks_like_path(&row.source) {
            // A path gives way to the label and detail, and keeps its end, which tells two
            // rows apart; a path too short to say anything is left out.
            let ambiguous = config.editor.ambiguous_width;
            let wanted = text_width(&row.label, ambiguous)
                + if row.detail.is_empty() {
                    0
                } else {
                    2 + text_width(&row.detail, ambiguous)
                }
                + 3;
            let left_over =
                usize::from(source_right.saturating_sub(content_left)).saturating_sub(wanted);
            source_room = source_room.min(u16::try_from(left_over).unwrap_or(u16::MAX));
            if source_room < 12 {
                source_room = 0;
                String::new()
            } else {
                truncate_content_title(&row.source, usize::from(source_room), ambiguous)
            }
        } else {
            row.source.clone()
        };
        let source_width = u16::try_from(text_width(&source, config.editor.ambiguous_width))
            .unwrap_or(u16::MAX)
            .min(source_room);
        let source_x = source_right.saturating_sub(source_width);
        let label_right = if source_width == 0 {
            list_right
        } else {
            source_x.saturating_sub(1)
        };
        let ambiguous = config.editor.ambiguous_width;
        let (label, detail) = fitted_picker_row(
            &row.label,
            &row.detail,
            usize::from(label_right.saturating_sub(content_left))
                .saturating_sub(text_width(marker, ambiguous) + 1),
            ambiguous,
        );
        let text = if detail.is_empty() {
            format!("{marker} {label}")
        } else {
            format!("{marker} {label}  {detail}")
        };
        if selected {
            for x in left + 1..list_right {
                frame.grid.set_cell(x, y, " ", Style::Selection);
            }
        }
        if row.kind == PickerRowKind::Section {
            // A section header is drawn as a header: full width, no marker, and never the
            // row a selection sits on because selection steps over it.
            frame.grid.put_text_clipped(
                content_left,
                y,
                &format!("── {} ", row.label),
                Style::Accent,
                list_right,
            );
            continue;
        }
        if !row.cells.is_empty() {
            // A row with cells draws every column at its own declared width with its own
            // style, so a status column, a lane and a badge are coloured independently.
            // The marker keeps focus legible; a cell that carries a meaningful style keeps
            // it on the selected row, and only the default style is promoted to the
            // selection style so no lane or status colour is lost under the cursor. Muted
            // text is promoted too, so the selection band runs unbroken under a description.
            let marker_style = if selected {
                Style::Selection
            } else {
                Style::Default
            };
            frame
                .grid
                .put_text_clipped(content_left, y, marker, marker_style, list_right);
            let mut cell_x = content_left.saturating_add(2);
            for (position, cell) in row.cells.iter().enumerate() {
                if cell_x >= list_right {
                    break;
                }
                // A zero-width column takes what the fixed columns beside it leave.
                let width = if cell.width == 0 {
                    let fixed: u16 = row.cells[position + 1..]
                        .iter()
                        .map(|rest| rest.width)
                        .sum();
                    list_right.saturating_sub(cell_x).saturating_sub(fixed)
                } else {
                    cell.width
                };
                let cell_right = cell_x.saturating_add(width).min(list_right);
                let style = if selected
                    && matches!(
                        cell.style,
                        Style::Default | Style::Muted | Style::Accent | Style::Syntax(_)
                    ) {
                    Style::Selection
                } else {
                    cell.style
                };
                let text_x = match cell.align {
                    CellAlign::Left => cell_x,
                    CellAlign::Right => {
                        let measured = text_width(&cell.text, config.editor.ambiguous_width);
                        let measured = u16::try_from(measured).unwrap_or(u16::MAX);
                        cell_x.saturating_add(width.saturating_sub(measured))
                    }
                };
                frame
                    .grid
                    .put_text_clipped(text_x, y, &cell.text, style, cell_right);
                cell_x = cell_x.saturating_add(width);
            }
            continue;
        }
        frame.grid.put_text_clipped(
            content_left,
            y,
            &text,
            if selected {
                Style::Selection
            } else {
                Style::Default
            },
            label_right,
        );
        if source_width > 0 {
            frame.grid.put_text_clipped(
                source_x,
                y,
                &source,
                if selected {
                    Style::Selection
                } else {
                    Style::Muted
                },
                source_right,
            );
        }
    }
    if let Some(preview) = preview {
        let (preview_left, preview_fill_left, preview_top) = if let Some(divider) = divider {
            (divider + 2, divider + 1, top + 1)
        } else {
            (
                left + 2,
                left + 1,
                stacked_divider.unwrap_or(top).saturating_add(1),
            )
        };
        let preview_right = left + width - 1;
        let preview_bottom = top + height - 1;
        match &preview.body {
            PickerPreviewBody::Text(text) => {
                let available_rows = usize::from(preview_bottom.saturating_sub(preview_top));
                let highlighted_offset = text
                    .highlight_line
                    .and_then(|line| line.checked_sub(text.first_line));
                let first_visible_offset = highlighted_offset.map_or(0, |offset| {
                    offset.saturating_add(1).saturating_sub(available_rows)
                });
                let line_spans = &text.spans;
                for (visible_offset, (offset, line)) in text
                    .lines
                    .iter()
                    .enumerate()
                    .skip(first_visible_offset)
                    .take(available_rows)
                    .enumerate()
                {
                    let y = preview_top + u16::try_from(visible_offset).unwrap_or(u16::MAX);
                    if y >= preview_bottom {
                        break;
                    }
                    let line_index = text.first_line.saturating_add(offset);
                    let highlighted = text.highlight_line == Some(line_index);
                    if highlighted {
                        for x in preview_fill_left..preview_right {
                            frame.grid.set_cell(x, y, " ", Style::Selection);
                        }
                    }
                    let number =
                        format!("{:>preview_number_width$} ", line_index.saturating_add(1));
                    frame.grid.put_text_clipped(
                        preview_left,
                        y,
                        &number,
                        if highlighted {
                            Style::Selection
                        } else {
                            Style::Muted
                        },
                        preview_right,
                    );
                    let text_left = preview_left.saturating_add(
                        u16::try_from(preview_number_width)
                            .unwrap_or(0)
                            .saturating_add(1),
                    );
                    match line_spans.get(offset).filter(|_| !highlighted) {
                        Some(spans) => {
                            let mut x = text_left;
                            let mut at = 0;
                            for (range, role) in spans {
                                x = frame.grid.put_text_clipped(
                                    x,
                                    y,
                                    &line[at..range.start],
                                    Style::Default,
                                    preview_right,
                                );
                                x = frame.grid.put_text_clipped(
                                    x,
                                    y,
                                    &line[range.clone()],
                                    Style::Syntax(*role),
                                    preview_right,
                                );
                                at = range.end;
                            }
                            frame.grid.put_text_clipped(
                                x,
                                y,
                                &line[at..],
                                Style::Default,
                                preview_right,
                            );
                        }
                        None => {
                            frame.grid.put_text_clipped(
                                text_left,
                                y,
                                line,
                                if highlighted {
                                    Style::Selection
                                } else {
                                    Style::Default
                                },
                                preview_right,
                            );
                        }
                    }
                }
            }
            PickerPreviewBody::Diff(diff) => {
                render_diff_preview(
                    frame,
                    diff,
                    picker.preview_side_scroll,
                    config,
                    WindowRect {
                        x: preview_left,
                        y: preview_top,
                        width: preview_right.saturating_sub(preview_left),
                        height: preview_bottom.saturating_sub(preview_top),
                    },
                );
            }
        }
    }
    let cursor_x = content_left.saturating_add(
        u16::try_from(text_width(query, config.editor.ambiguous_width)).unwrap_or(0),
    );
    frame.cursor = Some((
        cursor_x.min(query_right.saturating_sub(1)),
        query_y,
        cursor_style(PROMPT_CURSOR),
    ));
}

#[cfg(test)]
mod tests {
    use crate::ui::render::picker::*;

    #[test]
    fn emphasized_diff_stops_at_a_wide_grapheme_that_cannot_fit() {
        let config = crate::config::load_bundled_config().config;
        let mut frame = Frame {
            grid: CellGrid::new(4, 1),
            cursor: None,
        };
        frame.grid.set_cell(3, 0, "|", Style::Border);
        paint_diff_row(
            &mut frame,
            &config,
            0,
            0,
            3,
            &DiffRow {
                kind: DiffRowKind::Added,
                text: "ab界z".into(),
                spans: vec![crate::ui::DiffSpan { start: 0, end: 6 }],
            },
        );
        assert_eq!(frame.grid.row_text(0), "ab |");
        assert_eq!(frame.grid.get(0, 0).unwrap().style, Style::Search);
    }

    #[test]
    fn prompt_viewport_keeps_the_grapheme_caret_visible() {
        let grid = CellGrid::new(20, 1);
        let input = format!("{}é👩‍💻z", "a".repeat(80));
        let (shown, caret) = prompt_viewport(&input, input.len(), 8, &grid);
        assert!(shown.ends_with("é👩‍💻z"));
        assert!(caret < 8);
        assert!(!shown.starts_with('\u{301}'));
        assert_eq!(prompt_viewport(&input, 0, 8, &grid), (input.as_str(), 0));
    }
    #[test]
    fn ellipsis_width_is_included_in_the_budget() {
        for ambiguous in [
            crate::config::AmbiguousWidth::Narrow,
            crate::config::AmbiguousWidth::Wide,
        ] {
            for maximum in 0..20 {
                let shown = ellipsized("abcdefghijklmnopqrst", maximum, ambiguous);
                assert!(
                    text_width(&shown, ambiguous) <= maximum,
                    "{maximum}: {shown}"
                );
            }
        }
    }
}
use crate::ui::PickerSurfaceKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PickerContentGeometry {
    pub outer: WindowRect,
    pub list: WindowRect,
    pub divider: Option<u16>,
    pub stacked_divider: Option<u16>,
    /// The rows the preview has inside the border, zero when none is showing.
    pub preview_rows: u16,
}

/// Theme selection leaves most of the editor visible for live color preview.
/// Other picker sizes retain their configured behavior.
#[must_use]
pub(crate) fn picker_window_geometry(
    picker: &PickerUiState,
    bounds: WindowRect,
) -> Option<WindowRect> {
    let mut rect = crate::ui::picker_geometry(bounds)?;
    if picker.surface == crate::ui::PickerSurfaceKind::Theme {
        rect.width = rect.width.min(64);
        rect.height = rect
            .height
            .min(u16::try_from(picker.rows.len().clamp(2, 8)).unwrap_or(8) + 4);
        rect.x = bounds
            .x
            .saturating_add(bounds.width.saturating_sub(rect.width) / 2);
        rect.y = bounds
            .y
            .saturating_add((bounds.height.saturating_sub(rect.height) / 4).min(4));
    } else if picker.surface == crate::ui::PickerSurfaceKind::Choice {
        let widest = picker
            .rows
            .iter()
            .map(|row| row.label.chars().count() + row.detail.chars().count() + 2)
            .max()
            .unwrap_or(0);
        let wanted = u16::try_from(widest + 8).unwrap_or(u16::MAX).max(48);
        rect.width = rect.width.min(wanted);
        // Border, query, divider, the rows and the footer.
        rect.height = rect
            .height
            .min(u16::try_from(picker.rows.len().clamp(2, 12)).unwrap_or(12) + 5);
        rect.x = bounds
            .x
            .saturating_add(bounds.width.saturating_sub(rect.width) / 2);
        rect.y = bounds
            .y
            .saturating_add(bounds.height.saturating_sub(rect.height) / 3);
    }
    Some(rect)
}

/// The preview decision uses the same bounded content sample as painting.
#[must_use]
pub fn picker_content_geometry(
    picker: &PickerUiState,
    config: &Config,
    rect: WindowRect,
) -> Option<PickerContentGeometry> {
    let WindowRect {
        x: left,
        y: top,
        width,
        height,
    } = rect;
    if width < 4 || height < 6 {
        return None;
    }
    let preview = picker
        .preview
        .as_ref()
        .filter(|_| width >= 40 && height >= 12);
    let list_width = picker
        .rows
        .iter()
        .take(64)
        .map(|row| {
            text_width(&row.label, config.editor.ambiguous_width)
                .saturating_add(text_width(&row.detail, config.editor.ambiguous_width))
                .saturating_add(text_width(&row.source, config.editor.ambiguous_width))
                .saturating_add(8)
        })
        .max()
        .unwrap_or(28)
        .clamp(28, 72);
    let preview_width = preview.map_or(0, |preview| {
        // Measurement reads the same rows the painter draws, so the two cannot disagree
        // about how wide a preview is.
        let (rows, numbers) = match &preview.body {
            crate::ui::PickerPreviewBody::Text(text) => (
                text.lines().to_vec(),
                text.first_line
                    .saturating_add(text.lines().len())
                    .max(1)
                    .to_string()
                    .len(),
            ),
            crate::ui::PickerPreviewBody::Diff(diff) => {
                // The measured rows already include both line-number columns.
                let width = diff.content_width(config.editor.ambiguous_width);
                (vec![], width.saturating_add(4))
            }
        };
        if rows.is_empty() {
            return numbers.clamp(32, 88);
        }
        rows.iter()
            .take(64)
            .map(|line| text_width(line, config.editor.ambiguous_width))
            .max()
            .unwrap_or(28)
            .saturating_add(numbers)
            .saturating_add(4)
            .clamp(32, 88)
    });
    let side_by_side = preview.is_some()
        && usize::from(width) >= list_width.saturating_add(preview_width).saturating_add(3);
    let divider = side_by_side.then(|| {
        left.saturating_add(u16::try_from(list_width).unwrap_or(width - 1))
            .saturating_add(1)
            .min(left.saturating_add(width - 2))
    });
    let stacked_divider = (preview.is_some() && !side_by_side).then(|| {
        // A short list of changes leaves its spare rows to the diff.
        let list_rows = if picker.surface == PickerSurfaceKind::Changes {
            u16::try_from(picker.rows.len())
                .unwrap_or(u16::MAX)
                .saturating_add(3)
        } else {
            u16::MAX
        };
        let list_share = (height.saturating_mul(2) / 3).min(list_rows);
        top.saturating_add(list_share).clamp(
            top.saturating_add(6),
            top.saturating_add(height.saturating_sub(4)),
        )
    });
    let list_right = divider.unwrap_or(left.saturating_add(width - 1));
    let list_bottom = stacked_divider.unwrap_or(top.saturating_add(height - 1));
    let x = left.saturating_add(2);
    let y = top.saturating_add(3);
    let preview_rows = if preview.is_some() {
        height
            .saturating_sub(stacked_divider.map_or(0, |divider| divider.saturating_sub(top)))
            .saturating_sub(2)
    } else {
        0
    };
    Some(PickerContentGeometry {
        outer: rect,
        list: WindowRect {
            x,
            y,
            width: list_right.saturating_sub(x),
            height: list_bottom.saturating_sub(y),
        },
        divider,
        stacked_divider,
        preview_rows,
    })
}
