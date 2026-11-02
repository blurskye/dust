use crossterm::cursor::SetCursorStyle;

use crate::{
    config::{BorderStyle, Config},
    ui::{Style, geometry::text_width, window::WindowRect},
};

use super::{
    CellAlign, DiffLayout, DiffPreview, DiffRow, DiffRowKind, DiffSideRow, Frame, PickerPreviewBody,
    PickerRowKind, PickerUiState, PluginSurfaceUiState, PrivateSurfaceUiState, PromptState,
    ToolMenuState, editor::cursor_style, overlays::truncate_content_title,
};

/// What fits of `text`, ending in an ellipsis when some had to go.
pub(super) fn ellipsized(text: &str, maximum: usize, ambiguous: crate::config::AmbiguousWidth) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if text_width(text, ambiguous) <= maximum || maximum == 0 {
        return if maximum == 0 { String::new() } else { text.to_owned() };
    }
    let mut kept = String::new();
    let mut width = 1;
    for grapheme in text.graphemes(true) {
        width += text_width(grapheme, ambiguous).max(1);
        if width > maximum {
            break;
        }
        kept.push_str(grapheme);
    }
    format!("{kept}…")
}

fn looks_like_path(text: &str) -> bool {
    text.contains('/') && !text.contains(char::is_whitespace)
}

/// A `Label  /some/path` detail row keeps its label and the end of its path.
fn clip_path_tail(
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
    if label_width + 2 + detail_width <= available || detail.is_empty() && label_width <= available {
        return (label.to_owned(), detail.to_owned());
    }
    let label = if looks_like_path(label) {
        let room = if detail.is_empty() { available } else { available * 2 / 3 };
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

/// Key hints broken between items into at most two rows, so narrow screens keep them all.
pub(super) fn wrapped_key_help(
    help: &str,
    maximum: usize,
    ambiguous: crate::config::AmbiguousWidth,
) -> Vec<String> {
    const SEPARATOR: &str = " · ";
    let mut first = String::new();
    let mut rest = Vec::new();
    for item in help.split(SEPARATOR) {
        let candidate = if first.is_empty() {
            item.to_owned()
        } else {
            format!("{first}{SEPARATOR}{item}")
        };
        if rest.is_empty() && text_width(&candidate, ambiguous) <= maximum {
            first = candidate;
        } else {
            rest.push(item);
        }
    }
    let mut rows = vec![ellipsized(&first, maximum, ambiguous)];
    if !rest.is_empty() {
        rows.push(ellipsized(&rest.join(SEPARATOR), maximum, ambiguous));
    }
    rows
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

/// A footer legend shortened to whole entries that fit.
///
/// A narrow surface drops whole actions rather than clipping one, so a footer can never show
/// half an action that reads as a different one.
fn fitted_footer(text: &str, width: usize, ambiguous_width: crate::config::AmbiguousWidth) -> String {
    if text_width(text, ambiguous_width) <= width {
        return text.to_owned();
    }
    let mut fitted = String::new();
    for entry in text.split(" · ") {
        let candidate = if fitted.is_empty() {
            entry.to_owned()
        } else {
            format!("{fitted} · {entry}")
        };
        if text_width(&candidate, ambiguous_width) > width {
            break;
        }
        fitted = candidate;
    }
    fitted
}

fn text_width_u16(text: &str, config: &Config) -> u16 {
    u16::try_from(text_width(text, config.editor.ambiguous_width)).unwrap_or(u16::MAX)
}

/// Draw one diff row, emphasising the spans that changed inside it.
///
/// The emphasis uses the theme's search style, which is the existing style for highlighted
/// text. A dedicated diff-emphasis style would be a theme-schema change and U-16 does not
/// ask for one.
fn paint_diff_row(frame: &mut Frame, config: &Config, x: u16, y: u16, right: u16, row: &DiffRow) {
    let style = diff_row_style(row.kind);
    if row.spans.is_empty() {
        frame.grid.put_text_clipped(x, y, &row.text, style, right);
        return;
    }
    let mut cursor_x = x;
    let mut cursor = 0usize;
    for span in &row.spans {
        let start = span.start.min(row.text.len());
        let end = span.end.min(row.text.len());
        if end < start
            || !row.text.is_char_boundary(start)
            || !row.text.is_char_boundary(end)
        {
            continue;
        }
        if start > cursor {
            let piece = &row.text[cursor..start];
            frame.grid.put_text_clipped(cursor_x, y, piece, style, right);
            cursor_x = cursor_x.saturating_add(text_width_u16(piece, config));
        }
        if end > start {
            let piece = &row.text[start..end];
            frame
                .grid
                .put_text_clipped(cursor_x, y, piece, Style::Search, right);
            cursor_x = cursor_x.saturating_add(text_width_u16(piece, config));
        }
        cursor = end;
    }
    if cursor < row.text.len() {
        frame
            .grid
            .put_text_clipped(cursor_x, y, &row.text[cursor..], style, right);
    }
}

fn render_diff_preview(
    frame: &mut Frame,
    diff: &DiffPreview,
    side_scroll: (usize, usize),
    config: &Config,
    rect: WindowRect,
) {
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
                let left_row = left_cells.get(position.saturating_add(left_offset)).copied();
                let right_row = rows.get(position.saturating_add(right_offset));
                {
                    frame.grid.set_cell(divider, y, "│", Style::Border);
                    if let Some(cell) = left_row.and_then(|row| row.old.as_ref()) {
                        let (text, spans) = diff.side_cell_text(cell);
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
                        let (text, spans) = diff.side_cell_text(cell);
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
            frame
                .grid
                .put_text_clipped(x, y, &cell.grapheme, cell.style, right);
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

#[must_use]
pub fn picker_geometry(config: &Config, bounds: WindowRect) -> Option<WindowRect> {
    let configured = &config.ui.picker;
    let width = u16::try_from(u32::from(bounds.width) * u32::from(configured.width_percent) / 100)
        .unwrap_or(bounds.width)
        .max(configured.minimum_width.min(bounds.width))
        .min(bounds.width);
    let height =
        u16::try_from(u32::from(bounds.height) * u32::from(configured.height_percent) / 100)
            .unwrap_or(bounds.height)
            .max(configured.minimum_height.min(bounds.height))
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
    geometry: Option<crate::ui::geometry::PickerContentGeometry>,
) {
    let configured = &config.ui.picker;
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
        match configured.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
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
        .or_else(|| crate::ui::geometry::picker_content_geometry(picker, config, rect))
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
                    usize::from(footer_right.saturating_sub(left.saturating_add(2)).saturating_sub(2)),
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

    let count = format!(
        "{}/{}",
        picker
            .selected
            .map_or(0, |selected| selected.saturating_add(1)),
        picker.rows.len()
    );
    let status = if picker.loading {
        format!("{}  {count}", configured.loading_label)
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
        let marker = if selected {
            &configured.selected_marker
        } else {
            &configured.row_marker
        };
        let source = if row.source.is_empty() {
            String::new()
        } else {
            format!("{}{}", configured.source_separator, row.source)
        };
        // Every picker uses the same trailing provenance column. Reserve at
        // most a third of the row so long sources cannot crowd out the label.
        let source_right = list_right.saturating_sub(1);
        let source_width = u16::try_from(text_width(&source, config.editor.ambiguous_width))
            .unwrap_or(u16::MAX)
            .min(source_right.saturating_sub(content_left) / 3);
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
            // selection style so no lane or status colour is lost under the cursor.
            frame
                .grid
                .put_text_clipped(content_left, y, marker, Style::Selection, list_right);
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
                    list_right
                        .saturating_sub(cell_x)
                        .saturating_sub(fixed)
                } else {
                    cell.width
                };
                let cell_right = cell_x.saturating_add(width).min(list_right);
                let style = if selected && cell.style == Style::Default {
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
                let available_rows =
                    usize::from(preview_bottom.saturating_sub(preview_top));
                let highlighted_offset = text
                    .highlight_line
                    .and_then(|line| line.checked_sub(text.first_line));
                let first_visible_offset = highlighted_offset.map_or(0, |offset| {
                    offset.saturating_add(1).saturating_sub(available_rows)
                });
                let line_spans = text.language.map_or_else(Vec::new, |language| {
                    preview_line_spans(language, &text.lines)
                });
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
                            frame.grid.put_text_clipped(x, y, &line[at..], Style::Default, preview_right);
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
        cursor_style(config.ui.cursor.prompt),
    ));
}

/// Syntax colours for preview lines, as byte ranges within each line. Captures can nest; the
/// first one that covers a byte wins, as in markdown code blocks.
fn preview_line_spans(
    language: crate::language::syntax::SyntaxLanguage,
    lines: &[String],
) -> Vec<Vec<(std::ops::Range<usize>, crate::language::syntax::Highlight)>> {
    let source = lines.join("\n");
    if source.len() > 512 * 1024 {
        return Vec::new();
    }
    let mut spans = crate::language::syntax::highlight(language, &source);
    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut per_line = vec![Vec::new(); lines.len()];
    let mut line = 0;
    let mut line_start = 0;
    let mut covered = 0;
    for span in spans {
        if span.start < covered || span.start >= span.end {
            continue;
        }
        covered = span.end;
        let mut start = span.start;
        while start < span.end && line < lines.len() {
            let line_end = line_start + lines[line].len();
            if start > line_end {
                line_start = line_end + 1;
                line += 1;
                continue;
            }
            let end = span.end.min(line_end);
            if start < end {
                per_line[line].push((start - line_start..end - line_start, span.role));
            }
            if span.end <= line_end {
                break;
            }
            start = line_end + 1;
        }
    }
    per_line
}

pub(super) fn render_tool_menu(
    frame: &mut Frame,
    menu: &ToolMenuState,
    config: &Config,
    terminal_width: u16,
    terminal_height: u16,
) {
    let configured = &config.ui.tool_manager;
    let width = (u32::from(terminal_width) * u32::from(configured.width_percent) / 100)
        .try_into()
        .unwrap_or(terminal_width);
    let height = (u32::from(terminal_height) * u32::from(configured.height_percent) / 100)
        .try_into()
        .unwrap_or(terminal_height);
    let width = width
        .max(configured.minimum_width.min(terminal_width))
        .min(terminal_width);
    let mut height = height
        .max(configured.minimum_height.min(terminal_height))
        .min(terminal_height);
    if let Some(picker) = &menu.runtime_picker {
        let compact_height = u16::try_from(picker.options.len())
            .unwrap_or(u16::MAX)
            .saturating_mul(2)
            .saturating_add(11)
            .max(configured.minimum_height);
        height = height.min(compact_height);
    }
    if width < 4 || height < 6 {
        return;
    }
    let left = terminal_width.saturating_sub(width) / 2;
    let top = terminal_height.saturating_sub(height) / 2;
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        match configured.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
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
    let title = menu.runtime_picker.as_ref().map_or_else(
        || {
            if menu.searching || !menu.query.is_empty() {
                let cursor = if menu.searching { "▏" } else { "" };
                format!(" {} · /{}{cursor} ", configured.title, menu.query)
            } else {
                format!(" {} ", configured.title)
            }
        },
        |picker| {
            format!(
                " {} · {} · choose build ",
                configured.title, picker.server_id
            )
        },
    );
    frame.grid.put_text(left + 2, top, &title, Style::Accent);
    let content_left = left + 2;
    let content_width = width.saturating_sub(4);
    let ambiguous = config.editor.ambiguous_width;
    let clip = |text: &str| ellipsized(text, usize::from(content_width), ambiguous);
    let clip_indented = |text: &str| {
        truncate_content_title(text, usize::from(content_width.saturating_sub(2)), ambiguous)
    };
    let mut y = top + 1;

    if let Some(picker) = &menu.runtime_picker {
        for (row, help) in wrapped_key_help(
            "j/k move · Enter use this build · i install · u uninstall · r refresh · Esc back",
            usize::from(content_width),
            ambiguous,
        )
        .iter()
        .enumerate()
        {
            frame.grid.put_text(content_left, y + row as u16, help, Style::Muted);
        }
        y += 2;
        let picker_version_x = content_left.saturating_add(2);
        let picker_release_x = content_left.saturating_add(content_width.saturating_mul(22) / 100);
        let picker_source_x = content_left.saturating_add(content_width.saturating_mul(46) / 100);
        let picker_state_x = content_left.saturating_add(content_width.saturating_mul(68) / 100);
        let picker_active_x = content_left.saturating_add(content_width.saturating_mul(91) / 100);
        frame
            .grid
            .put_text(picker_version_x, y, "VERSION", Style::Muted);
        frame
            .grid
            .put_text(picker_release_x, y, "RELEASE", Style::Muted);
        frame
            .grid
            .put_text(picker_source_x, y, "SOURCE", Style::Muted);
        frame
            .grid
            .put_text(picker_state_x, y, "STATE", Style::Muted);
        frame
            .grid
            .put_text(picker_active_x, y, "IN USE", Style::Muted);
        y += 1;

        let detail_top = top.saturating_add(height).saturating_sub(5);
        let visible = usize::from(detail_top.saturating_sub(y) / 2).max(1);
        for (index, option) in picker
            .options
            .iter()
            .enumerate()
            .skip(picker.scroll)
            .take(visible)
        {
            let focused = index == picker.selected;
            let marker = if focused {
                &configured.selected_marker
            } else {
                &configured.row_marker
            };
            let active = if option.selected && option.installed { "●" } else { "" };
            let style = if focused {
                Style::Selection
            } else if option.selected {
                Style::Accent
            } else if option.installable || option.installed {
                Style::Default
            } else {
                Style::Muted
            };
            if focused {
                for x in content_left..content_left.saturating_add(content_width) {
                    frame.grid.set_cell(x, y, " ", Style::Selection);
                }
            }
            frame.grid.put_text(content_left, y, marker, style);
            let column = |text: &str, from: u16, to: u16| {
                ellipsized(text, usize::from(to.saturating_sub(from).saturating_sub(1)), ambiguous)
            };
            frame.grid.put_text(
                picker_version_x,
                y,
                &column(&option.version, picker_version_x, picker_release_x),
                style,
            );
            let release = if option.release == "catalog build" {
                "—"
            } else {
                option.release.as_str()
            };
            frame.grid.put_text(
                picker_release_x,
                y,
                &column(release, picker_release_x, picker_source_x),
                style,
            );
            frame.grid.put_text(
                picker_source_x,
                y,
                &column(&option.label, picker_source_x, picker_state_x),
                style,
            );
            frame.grid.put_text(
                picker_state_x,
                y,
                &column(&option.status, picker_state_x, picker_active_x),
                style,
            );
            frame.grid.put_text(picker_active_x, y, active, style);
            y += 1;
            if option.installed {
                frame.grid.put_text(
                    content_left + 2,
                    y,
                    &clip_indented(&option.location),
                    Style::Muted,
                );
            }
            y += 1;
        }

        if let Some(option) = picker.options.get(picker.selected) {
            y = detail_top;
            frame.grid.put_text(
                content_left,
                y,
                &clip(&horizontal.to_string().repeat(usize::from(content_width))),
                Style::Border,
            );
            y += 1;
            let action = if option.selected && option.installed {
                if option.uninstallable {
                    "In use · u uninstall"
                } else {
                    "In use"
                }
            } else if option.installed {
                if option.uninstallable {
                    "Enter use this build · u uninstall"
                } else {
                    "Enter use this build"
                }
            } else if option.installable {
                "Enter or i install this build"
            } else {
                "Not available on this system"
            };
            frame
                .grid
                .put_text(content_left, y, &clip(action), Style::Accent);
            y += 1;
            frame
                .grid
                .put_text(content_left, y, &clip(&option.note), Style::Muted);
        }
        frame.cursor = None;
        return;
    }

    for (row, help) in wrapped_key_help(&configured.help, usize::from(content_width), ambiguous)
        .iter()
        .enumerate()
    {
        frame.grid.put_text(content_left, y + row as u16, help, Style::Muted);
    }
    y += 2;
    let mut tab_x = content_left;
    for category in &configured.category_order {
        let label = configured
            .categories
            .get(category)
            .map(String::as_str)
            .unwrap_or(category);
        let text = format!(" {label} ");
        let style = if category == &menu.category {
            Style::StatusAccent
        } else {
            Style::Status
        };
        frame.grid.put_text(tab_x, y, &text, style);
        tab_x = tab_x.saturating_add(u16::try_from(text.chars().count()).unwrap_or(0) + 1);
    }
    y += 2;
    let name_x = content_left.saturating_add(2);
    let version_x = content_left.saturating_add(content_width.saturating_mul(50) / 100);
    let runtime_x = content_left.saturating_add(content_width.saturating_mul(64) / 100);
    let state_x = content_left.saturating_add(content_width.saturating_mul(78) / 100);
    let right = content_left.saturating_add(content_width);
    // Too narrow for every column: the version and source stay in the details below.
    let compact = content_width < 70;
    frame.grid.put_text(name_x, y, "NAME", Style::Muted);
    if !compact {
        frame.grid.put_text(version_x, y, "VERSION", Style::Muted);
        frame.grid.put_text(runtime_x, y, "SOURCE", Style::Muted);
    }
    frame.grid.put_text(state_x, y, "STATE", Style::Muted);
    y += 1;

    let remaining = top
        .saturating_add(height)
        .saturating_sub(1)
        .saturating_sub(y);
    // A short window keeps what the user can act on, the note and the keys, and gives the list
    // the rest.
    let short = remaining < 12;
    let usual_detail_rows = if short { 5u16 } else { 9 };
    // A failure explains itself in the details, so it takes rows from the list until it fits.
    let failure_rows = menu
        .rows
        .get(menu.selected)
        .filter(|row| tool_state_glyph(row).1 == Style::Error)
        .map_or(0, |row| {
            let note = super::wrap_card_text(&row.note, usize::from(content_width), ambiguous).len();
            usual_detail_rows - 2 + u16::try_from(note).unwrap_or(u16::MAX)
        })
        .min(remaining.saturating_sub(2));
    let detail_rows = usual_detail_rows
        .max(failure_rows)
        .min(remaining.saturating_sub(1));
    let list_bottom = top + height - 1 - detail_rows;
    let visible_rows = usize::from(list_bottom.saturating_sub(y));
    let scroll = menu.scroll.max((menu.selected + 1).saturating_sub(visible_rows));
    for (index, row) in menu
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_rows)
    {
        let selected = index == menu.selected;
        let style = if selected {
            Style::Selection
        } else if row.installed {
            Style::Default
        } else {
            Style::Muted
        };
        if selected {
            for x in content_left..right {
                frame.grid.set_cell(x, y, " ", Style::Selection);
            }
        }
        let (glyph, glyph_style) = tool_state_glyph(row);
        frame.grid.put_text(content_left, y, glyph, glyph_style);
        let name_right = if compact { state_x } else { version_x }.saturating_sub(2);
        let column = |text: &str, from: u16, to: u16| {
            ellipsized(text, usize::from(to.saturating_sub(from)), ambiguous)
        };
        frame
            .grid
            .put_text(name_x, y, &column(&row.name, name_x, name_right), style);
        let languages_x = name_x
            .saturating_add(u16::try_from(text_width(&row.name, config.editor.ambiguous_width)).unwrap_or(0))
            .saturating_add(2);
        if !row.languages.is_empty() && languages_x < name_right {
            frame.grid.put_text_clipped(
                languages_x,
                y,
                &column(&row.languages, languages_x, name_right),
                if selected { Style::Selection } else { Style::Muted },
                name_right,
            );
        }
        if !compact {
            let runtime_right = runtime_x.saturating_sub(1);
            frame
                .grid
                .put_text(version_x, y, &column(&row.version, version_x, runtime_right), style);
            let state_left = state_x.saturating_sub(1);
            frame
                .grid
                .put_text(runtime_x, y, &column(&row.runtime, runtime_x, state_left), style);
        }
        // The list shows the short state; byte counts stay in the detail pane.
        let state = row.lifecycle.split(" (").next().unwrap_or_default();
        frame.grid.put_text(
            state_x,
            y,
            &column(state, state_x, right),
            if selected { Style::Selection } else { glyph_style },
        );
        y += 1;
    }

    if let Some(row) = menu.rows.get(menu.selected) {
        y = list_bottom;
        frame.grid.put_text(
            content_left,
            y,
            &clip(&horizontal.to_string().repeat(usize::from(content_width))),
            Style::Border,
        );
        y += 1;
        let (_, state_style) = tool_state_glyph(row);
        let status_x = right
            .saturating_sub(u16::try_from(row.lifecycle.chars().count()).unwrap_or(u16::MAX));
        frame
            .grid
            .put_text_clipped(content_left, y, &row.name, Style::Accent, status_x);
        if !row.languages.is_empty() {
            let x = content_left
                .saturating_add(u16::try_from(text_width(&row.name, config.editor.ambiguous_width)).unwrap_or(0))
                .saturating_add(2);
            frame
                .grid
                .put_text_clipped(x, y, &row.languages, Style::Muted, status_x.saturating_sub(1));
        }
        frame.grid.put_text(status_x, y, &row.lifecycle, state_style);
        y += if short { 1 } else { 2 };
        let mut details = Vec::new();
        if row.installed {
            let location = if row.executable.is_empty() {
                row.runtime.clone()
            } else {
                format!("{}  {}", row.runtime, row.executable)
            };
            details.push(format!("Source    {location}"));
        } else if row.runtime != "—" {
            details.push(format!("Source    {} (not installed)", row.runtime));
        }
        if row.version != "—" {
            let release = match row.release.as_str() {
                "—" | "catalog build" | "reported by executable" | "" => String::new(),
                release => format!("  ({release})"),
            };
            details.push(format!("Version   {}{release}", row.version));
        }
        if !row.sessions.is_empty() {
            details.push(format!("Sessions  {}", row.sessions.join(" · ")));
        }
        let bottom = top + height - 1;
        if short {
            details.clear();
        }
        for line in details {
            if y >= bottom {
                break;
            }
            let line = if looks_like_path(line.rsplit(' ').next().unwrap_or_default()) {
                clip_path_tail(&line, usize::from(content_width), ambiguous)
            } else {
                clip(&line)
            };
            frame.grid.put_text(content_left, y, &line, Style::Muted);
            y += 1;
        }
        y += u16::from(!short);
        if y < bottom {
            let note_style = if state_style == Style::Error {
                Style::Error
            } else {
                Style::Default
            };
            // Install failures explain themselves here, so the note wraps above the keys.
            let keys_row = bottom.saturating_sub(1);
            for line in super::wrap_card_text(&row.note, usize::from(content_width), ambiguous) {
                if y >= keys_row {
                    break;
                }
                let line: String = line.into_iter().map(|(piece, _)| piece).collect();
                frame.grid.put_text(content_left, y, &clip(&line), note_style);
                y += 1;
            }
        }
        let keys = tool_row_keys(row).join(" · ");
        if y < bottom && !keys.is_empty() {
            frame.grid.put_text(content_left, y, &clip(&keys), Style::Accent);
        }
    }
    frame.cursor = None;
}

/// Whether the state is a transient install phase (downloading, verifying, installing 40%…).
fn tool_busy(state: &str) -> bool {
    !matches!(
        state,
        "installed" | "not installed" | "install failed" | "not working" | "crashed" | "starting"
            | "restarting"
    ) && !state.starts_with("running")
}

/// The glyph and colour for a tool's state: filled when installed, half while busy, a cross
/// when broken, hollow when not installed.
fn tool_state_glyph(row: &crate::ui::ToolMenuRow) -> (&'static str, Style) {
    match row.lifecycle.as_str() {
        "install failed" | "not working" | "crashed" => ("✗", Style::Error),
        "not installed" => ("○", Style::Muted),
        "starting" | "restarting" => ("◐", Style::Warning),
        state if tool_busy(state) => ("◐", Style::Warning),
        _ => ("●", Style::GitSignAdded),
    }
}

/// The keys that do something for this row right now.
fn tool_row_keys(row: &crate::ui::ToolMenuRow) -> Vec<&'static str> {
    let state = row.lifecycle.as_str();
    let mut keys = Vec::new();
    if tool_busy(state) {
        return keys;
    }
    if row.installed {
        if row.lsp {
            if state == "installed" || state == "crashed" {
                keys.push(if row.can_start {
                    "Enter start"
                } else {
                    "open a matching file to start"
                });
            } else {
                keys.push("x stop");
                keys.push("R restart");
            }
        }
        if row.can_uninstall {
            keys.push("u uninstall");
        }
    } else if row.can_install {
        keys.push(if state == "install failed" {
            "i retry install"
        } else {
            "i install"
        });
    }
    if row.lsp {
        keys.push("p versions");
    }
    keys
}

pub(super) fn render_prompt(
    frame: &mut Frame,
    prompt: &PromptState,
    config: &Config,
    width: u16,
    height: u16,
) {
    let confirmation_layout = prompt
        .confirmation
        .as_ref()
        .map(|_| super::confirmation_layout(width, height, config, prompt));
    let geometry = confirmation_layout.as_ref().map_or_else(
        || super::prompt_geometry_for_state(width, height, config, prompt),
        |layout| layout.geometry,
    );
    if geometry.width == 0 || geometry.height == 0 {
        return;
    }
    let right = geometry.left.saturating_add(geometry.width);
    for y in geometry.top..geometry.top.saturating_add(geometry.height) {
        for x in geometry.left..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    if geometry.height < 3 {
        if let Some(confirmation) = &prompt.confirmation {
            if !confirmation_layout
                .as_ref()
                .is_some_and(|layout| layout.fully_visible)
            {
                frame.grid.put_text_clipped(
                    geometry.left,
                    geometry.top,
                    "Resize to review the full target",
                    Style::Warning,
                    right,
                );
                frame.cursor = None;
                return;
            }
            frame.grid.put_text_clipped(
                geometry.left,
                geometry.top,
                &confirmation.message,
                Style::Default,
                right,
            );
            if geometry.height > 1 {
                frame.grid.put_text_clipped(
                    geometry.left,
                    geometry.top.saturating_add(1),
                    &if confirmation.selected == crate::ui::ConfirmationChoice::Yes {
                        format!(
                            "[ {} ]  {}",
                            confirmation.positive_label, confirmation.negative_label
                        )
                    } else {
                        format!(
                            "{}  [ {} ]",
                            confirmation.positive_label, confirmation.negative_label
                        )
                    },
                    Style::Selection,
                    right,
                );
            }
            frame.cursor = None;
            return;
        }
        let marker_end = frame.grid.put_text_clipped(
            geometry.left,
            geometry.top,
            prompt.kind.marker(&config.ui.command_line.text),
            Style::Accent,
            right,
        );
        frame.grid.put_text_clipped(
            marker_end,
            geometry.top,
            &prompt.input,
            Style::Default,
            right,
        );
        let prefix = prompt.input.get(..prompt.cursor).unwrap_or(&prompt.input);
        frame.cursor = Some((
            marker_end
                .saturating_add(
                    u16::try_from(text_width(prefix, config.editor.ambiguous_width)).unwrap_or(0),
                )
                .min(right.saturating_sub(1)),
            geometry.top,
            cursor_style(config.ui.cursor.prompt),
        ));
        return;
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        match config.ui.command_line.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
    let show_help = prompt.confirmation.is_none()
        && prompt.error.is_none()
        && prompt.help.is_some()
        && geometry.height >= 4;
    let bottom_y = if prompt.confirmation.is_some() {
        geometry
            .top
            .saturating_add(geometry.height.saturating_sub(1))
    } else {
        geometry.top.saturating_add(if show_help { 3 } else { 2 })
    };
    for x in geometry.left..geometry.left.saturating_add(geometry.width) {
        frame
            .grid
            .set_cell(x, geometry.top, &horizontal.to_string(), Style::Border);
        if geometry.height >= 3 {
            frame
                .grid
                .set_cell(x, bottom_y, &horizontal.to_string(), Style::Border);
        }
    }
    frame.grid.set_cell(
        geometry.left,
        geometry.top,
        &top_left.to_string(),
        Style::Border,
    );
    frame.grid.set_cell(
        geometry.left + geometry.width - 1,
        geometry.top,
        &top_right.to_string(),
        Style::Border,
    );
    if geometry.height >= 3 {
        frame.grid.set_cell(
            geometry.left,
            bottom_y,
            &bottom_left.to_string(),
            Style::Border,
        );
        frame.grid.set_cell(
            geometry.left + geometry.width - 1,
            bottom_y,
            &bottom_right.to_string(),
            Style::Border,
        );
    }
    for y in geometry.top.saturating_add(1)..bottom_y {
        frame
            .grid
            .set_cell(geometry.left, y, &vertical.to_string(), Style::Border);
        frame.grid.set_cell(
            geometry.left + geometry.width - 1,
            y,
            &vertical.to_string(),
            Style::Border,
        );
    }
    if config.ui.command_line.show_title && geometry.width > config.ui.command_line.title_min_width
    {
        let title = format!(
            " {} ",
            prompt.confirmation.as_ref().map_or_else(
                || prompt.kind.title(&config.ui.command_line.text),
                |confirmation| confirmation.title.as_str(),
            )
        );
        let title_width =
            u16::try_from(text_width(&title, config.editor.ambiguous_width)).unwrap_or(0);
        let title_x = geometry
            .left
            .saturating_add(geometry.width.saturating_sub(title_width) / 2)
            .max(geometry.left.saturating_add(1));
        frame.grid.put_text_clipped(
            title_x,
            geometry.top,
            &title,
            Style::Accent,
            right.saturating_sub(1),
        );
    }
    if let Some(confirmation) = &prompt.confirmation {
        let content_left = geometry.left.saturating_add(2);
        let line_limit = usize::from(geometry.height.saturating_sub(3));
        let layout = confirmation_layout
            .as_ref()
            .expect("confirmation layout was constructed");
        let fully_visible = layout.fully_visible;
        for (row, line) in layout.lines.iter().take(line_limit).enumerate() {
            let y = geometry
                .top
                .saturating_add(1)
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            let mut x = content_left;
            for (unit, unit_width) in &line.units {
                if x.saturating_add(*unit_width) > right.saturating_sub(1) {
                    break;
                }
                frame.grid.set_cell(x, y, unit, Style::Default);
                x = x.saturating_add(*unit_width);
            }
        }
        if !fully_visible {
            frame.grid.put_text_clipped(
                content_left,
                bottom_y.saturating_sub(1),
                "Resize to review the full target",
                Style::Warning,
                right.saturating_sub(1),
            );
            frame.cursor = None;
            return;
        }
        let positive = format!("[ {} ]", confirmation.positive_label);
        let negative = format!("[ {} ]", confirmation.negative_label);
        let buttons_width = text_width(&positive, config.editor.ambiguous_width)
            .saturating_add(1)
            .saturating_add(text_width(&negative, config.editor.ambiguous_width));
        let buttons_x = geometry.left.saturating_add(
            geometry
                .width
                .saturating_sub(u16::try_from(buttons_width).unwrap_or(geometry.width))
                / 2,
        );
        let button_y = bottom_y.saturating_sub(1);
        let no_x = frame.grid.put_text_clipped(
            buttons_x,
            button_y,
            &positive,
            if confirmation.selected == crate::ui::ConfirmationChoice::Yes {
                Style::Selection
            } else {
                Style::Default
            },
            right.saturating_sub(1),
        );
        frame.grid.put_text_clipped(
            no_x.saturating_add(1),
            button_y,
            &negative,
            if confirmation.selected == crate::ui::ConfirmationChoice::No {
                Style::Selection
            } else {
                Style::Default
            },
            right.saturating_sub(1),
        );
        if !confirmation.hint.is_empty() && geometry.width > 8 {
            frame.grid.put_text_clipped(
                geometry.left.saturating_add(2),
                bottom_y,
                &format!(" {} ", confirmation.hint),
                Style::Muted,
                right.saturating_sub(2),
            );
        }
        frame.cursor = None;
        return;
    }
    let input_x = geometry
        .left
        .saturating_add(config.ui.command_line.input_padding);
    let input_y = geometry.top.saturating_add(if show_help { 2 } else { 1 });
    let content_right = right.saturating_sub(1);
    if show_help {
        if let Some(help) = &prompt.help {
            frame.grid.put_text_clipped(
                input_x,
                geometry.top.saturating_add(1),
                help,
                Style::Muted,
                content_right,
            );
        }
    }
    let marker_end = frame.grid.put_text_clipped(
        input_x,
        input_y,
        prompt.kind.marker(&config.ui.command_line.text),
        Style::Accent,
        content_right,
    );
    frame.grid.put_text_clipped(
        marker_end,
        input_y,
        &prompt.input,
        Style::Default,
        content_right,
    );
    if let Some(error) = &prompt.error {
        let error_y = geometry
            .top
            .saturating_add(3)
            .saturating_add(config.ui.command_line.attachment_gap);
        // A long error wraps below the box, since its end usually says what to do.
        let padding = config.ui.command_line.input_padding;
        let inner = right.saturating_sub(geometry.left).saturating_sub(padding.saturating_mul(2));
        let ambiguous = config.editor.ambiguous_width;
        let lines = super::wrap_card_text(error, usize::from(inner).max(1), ambiguous);
        for (row, line) in (error_y..height).zip(lines) {
            for x in geometry.left..right {
                frame.grid.set_cell(x, row, " ", Style::Default);
            }
            let line: String = line.into_iter().map(|(piece, _)| piece).collect();
            frame.grid.put_text_clipped(
                geometry.left.saturating_add(padding),
                row,
                &line,
                Style::Error,
                right,
            );
        }
    } else if !prompt.completions.is_empty() {
        let completion_top = geometry
            .top
            .saturating_add(3)
            .saturating_add(config.ui.command_line.attachment_gap);
        let available = height.saturating_sub(completion_top);
        let rows = u16::try_from(prompt.completions.len())
            .unwrap_or(u16::MAX)
            .min(config.ui.command_line.completion_max_rows)
            .min(available);
        let rows = usize::from(rows);
        let maximum_scroll = prompt.completions.len().saturating_sub(rows);
        // Prompt selection is global to the candidate list, while rendering is bounded to a
        // terminal-sized viewport. Keep the selected candidate visible instead of leaving the
        // terminal cursor on a row that is no longer drawn.
        let scroll = prompt
            .selected_completion
            .filter(|index| *index < prompt.completions.len())
            .map_or(0, |selected| {
                selected
                    .saturating_add(1)
                    .saturating_sub(rows)
                    .min(maximum_scroll)
            });
        for row in 0..rows {
            let index = scroll.saturating_add(row);
            let y = completion_top.saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            let style = if prompt.selected_completion == Some(index) {
                Style::Selection
            } else {
                Style::Status
            };
            for x in geometry.left..geometry.left.saturating_add(geometry.width) {
                frame.grid.set_cell(x, y, " ", style);
            }
            let item = &prompt.completions[index];
            let label_x = geometry
                .left
                .saturating_add(config.ui.command_line.input_padding);
            let detail_x = label_x
                .saturating_add(
                    u16::try_from(text_width(&item.label, config.editor.ambiguous_width))
                        .unwrap_or(0),
                )
                .saturating_add(config.ui.command_line.completion_detail_gap);
            frame
                .grid
                .put_text_clipped(label_x, y, &item.label, style, right);
            frame
                .grid
                .put_text_clipped(detail_x, y, &item.detail, Style::Muted, right);
        }
    }
    let prefix = prompt.input.get(..prompt.cursor).unwrap_or(&prompt.input);
    let cursor_x = marker_end.saturating_add(
        u16::try_from(text_width(prefix, config.editor.ambiguous_width)).unwrap_or(0),
    );
    frame.cursor = Some((
        cursor_x.min(geometry.left + geometry.width.saturating_sub(2)),
        input_y,
        cursor_style(config.ui.cursor.prompt),
    ));
}
