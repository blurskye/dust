//! Notifications, key hints and overlay drawing.

use crate::CellGrid;
use crate::Style;
use crate::config::AmbiguousWidth;
use crate::config::Config;
use crate::geometry::cell_text_width;
use crate::geometry::text_width;
use crate::grid::Frame;
use crate::render::COMPLETION_DETAIL_GAP;
use crate::render::COMPLETION_MAX_ROWS;
use crate::render::ContentTabDiagnosticSeverity;
use crate::render::ContentTabEntry;
use crate::render::ContentTabIdentity;
use crate::render::LanguageCompletionState;
use crate::render::editor::cursor_style;
use crate::windows::WindowRect;

use crate::render::documentation::SWATCH_WIDTH;
use crate::render::documentation::color_swatch_style;

pub const BUFFER_NAME_MAX_WIDTH: u16 = 32;

pub const BUFFER_TAB_FORMAT: &str = " {title}{dirty}  ";

pub const BUFFER_SEPARATOR: &str = "│";

pub const BUFFER_DIRTY_MARKER: &str = " ●";

pub const BUFFER_PIN_MARKER: &str = "P";

pub const BUFFER_CLOSE_MARKER: &str = "x";

pub const BUFFER_ERROR_MARKER: &str = "E";

pub const BUFFER_WARNING_MARKER: &str = "W";

pub const BUFFER_OVERFLOW_LEFT: &str = "‹ ";

pub const BUFFER_OVERFLOW_RIGHT: &str = " ›";

pub const SCRATCH_ICON: &str = "\u{f039a}";

pub const VIRTUAL_ICON: &str = "\u{f0219}";

pub const TERMINAL_ICON: &str = "\u{f489}";

pub const CUSTOM_ICON: &str = "\u{f0214}";

pub const AGENT_ICON: &str = "\u{f06a9}";

pub const LSP_PROGRESS_MAX_ROWS: u16 = 8;

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub const SPINNER_INTERVAL_MS: u64 = 80;

pub const LSP_PROGRESS_DONE_TIMEOUT_MS: u64 = 3_000;

/// Draws the menu and returns where it went, so its documentation can sit clear of it.
pub(super) fn render_language_completion(
    frame: &mut Frame,
    completion: &LanguageCompletionState,
    config: &Config,
    bounds: WindowRect,
) -> Option<WindowRect> {
    if completion.items.is_empty() || bounds.width < 4 || bounds.height < 2 {
        return None;
    }
    let (cursor_x, cursor_y, _) =
        frame
            .cursor
            .unwrap_or((bounds.x, bounds.y, cursor_style(config.ui.cursor.normal)));
    let right = bounds.x.saturating_add(bounds.width);
    let bottom = bounds.y.saturating_add(bounds.height);
    let room_below = bottom.saturating_sub(cursor_y.saturating_add(1));
    let room_above = cursor_y.saturating_sub(bounds.y);
    let available = room_below.max(room_above);
    let rows = completion
        .items
        .len()
        .min(usize::from(COMPLETION_MAX_ROWS))
        .min(usize::from(available));
    if rows == 0 {
        return None;
    }
    let selected = completion.selected.min(completion.items.len() - 1);
    let maximum_scroll = completion.items.len().saturating_sub(rows);
    let mut scroll = completion.scroll.min(maximum_scroll);
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll.saturating_add(rows) {
        scroll = selected.saturating_add(1).saturating_sub(rows);
    }
    let visible = completion.items.iter().skip(scroll).take(rows);
    let ambiguous = config.editor.ambiguous_width;
    let gap = usize::from(COMPLETION_DETAIL_GAP);
    let icon_width = if completion.items.iter().any(|item| item.icon.is_some()) {
        2u16
    } else {
        0
    };
    let swatch = |item: &crate::CompletionItem| {
        if item.color.is_some() {
            SWATCH_WIDTH
        } else {
            0
        }
    };
    // Measured over every item, so the menu keeps its width while it scrolls.
    let label_width = completion
        .items
        .iter()
        .map(|item| text_width(&item.label, ambiguous).saturating_add(swatch(item)))
        .max()
        .unwrap_or(1);
    let source_width = completion
        .items
        .iter()
        .map(|item| text_width(&item.detail, ambiguous))
        .max()
        .unwrap_or(0);
    let maximum_content_width = if bounds.width >= 12 {
        bounds.width.saturating_mul(2) / 3
    } else {
        bounds.width
    }
    .saturating_sub(2);
    let content_width = usize::from(icon_width)
        .saturating_add(label_width)
        .saturating_add(if source_width > 0 {
            gap + source_width
        } else {
            0
        })
        .min(usize::from(maximum_content_width));
    let panel_width = u16::try_from(content_width)
        .unwrap_or(bounds.width)
        .saturating_add(2)
        .min(bounds.width);
    let left = cursor_x
        .saturating_sub(completion.typed)
        .saturating_sub(icon_width)
        .saturating_sub(1)
        .max(bounds.x)
        .min(right.saturating_sub(panel_width));
    // Leave a readable documentation column when indentation would strand the menu
    // between two gaps that are both too narrow for its preview.
    let left = if bounds.width >= 60
        && left.saturating_sub(bounds.x) < 20
        && right.saturating_sub(left.saturating_add(panel_width)) < 20
    {
        right
            .saturating_sub(panel_width)
            .saturating_sub(20)
            .max(bounds.x)
    } else {
        left
    };
    let row_count = u16::try_from(rows).unwrap_or(bounds.height);
    let below = cursor_y.saturating_add(1);
    let top = if below.saturating_add(row_count) <= bottom {
        below
    } else {
        cursor_y.saturating_sub(row_count).max(bounds.y)
    };
    let content_right = left.saturating_add(panel_width).saturating_sub(1);
    for (row, item) in visible.enumerate() {
        let y = top.saturating_add(u16::try_from(row).unwrap_or(0));
        let selected = scroll.saturating_add(row) == selected;
        let styled = |style| if selected { Style::Selection } else { style };
        let fill = styled(Style::DocumentationBackground);
        for x in left..left.saturating_add(panel_width) {
            frame.grid.set_cell(x, y, " ", fill);
        }
        if let Some(icon) = &item.icon {
            frame.grid.put_text_clipped(
                left.saturating_add(1),
                y,
                icon,
                styled(Style::DocumentationTitle),
                content_right,
            );
        }
        let label_x = left.saturating_add(1).saturating_add(icon_width);
        let label_end = label_x
            .saturating_add(u16::try_from(text_width(&item.label, ambiguous)).unwrap_or(u16::MAX));
        frame.grid.put_text_clipped(
            label_x,
            y,
            &crate::geometry::ellipsized(
                &item.label,
                usize::from(content_right.saturating_sub(label_x)),
                ambiguous,
            ),
            styled(Style::DocumentationText),
            content_right,
        );
        let mut used = label_end;
        if let Some(rgb) = item.color {
            let swatch_x = label_end.saturating_add(1);
            let swatch = color_swatch_style(rgb);
            for x in swatch_x..swatch_x.saturating_add(2).min(content_right) {
                frame.grid.set_cell(x, y, " ", swatch);
            }
            used = swatch_x.saturating_add(2);
        }
        // Right-aligned, and left out of a row whose label needs the room.
        let source_x = content_right
            .saturating_sub(u16::try_from(text_width(&item.detail, ambiguous)).unwrap_or(u16::MAX));
        if !item.detail.is_empty() && source_x > used {
            frame.grid.put_text_clipped(
                source_x,
                y,
                &item.detail,
                styled(Style::DocumentationBorder),
                content_right,
            );
        }
    }
    Some(WindowRect {
        x: left,
        y: top,
        width: panel_width,
        height: row_count,
    })
}

/// A small box on the row below the cursor, or above it on the bottom row.
pub(super) fn render_lsp_loading(frame: &mut Frame, text: &str, config: &Config) {
    let Some((cursor_x, cursor_y, _)) = frame.cursor else {
        return;
    };
    let width = u16::try_from(text_width(text, config.editor.ambiguous_width))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let left = cursor_x.min(frame.grid.width().saturating_sub(width));
    let below = cursor_y.saturating_add(1);
    let y = if below < frame.grid.height().saturating_sub(2) {
        below
    } else {
        cursor_y.saturating_sub(1)
    };
    for x in left..left.saturating_add(width) {
        frame
            .grid
            .set_cell(x, y, " ", Style::DocumentationBackground);
    }
    frame
        .grid
        .put_text(left.saturating_add(1), y, text, Style::DocumentationText);
}

pub(super) fn truncate_content_title(
    title: &str,
    maximum_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> String {
    if maximum_width == 0 {
        return String::new();
    }
    if cell_text_width(title, ambiguous_width) <= maximum_width {
        return title.to_owned();
    }
    let ellipsis = "…";
    let ellipsis_width = text_width(ellipsis, ambiguous_width);
    if ellipsis_width > maximum_width {
        return String::new();
    }
    let available = maximum_width - ellipsis_width;
    let mut suffix = Vec::new();
    let mut width = 0usize;
    for grapheme in title.graphemes(true).rev() {
        let grapheme_width = cell_text_width(grapheme, ambiguous_width);
        if width.saturating_add(grapheme_width) > available {
            break;
        }
        suffix.push(grapheme);
        width = width.saturating_add(grapheme_width);
    }
    suffix.reverse();
    format!("{ellipsis}{}", suffix.concat())
}

pub(super) fn formatted_content_tab(
    entry: &ContentTabEntry,
    config: &Config,
) -> (String, String, String, Style) {
    let title = truncate_content_title(
        &entry.title,
        usize::from(BUFFER_NAME_MAX_WIDTH),
        config.editor.ambiguous_width,
    );
    let (before, after) = BUFFER_TAB_FORMAT
        .split_once("{dirty}")
        .expect("buffer tab format has {dirty}");
    (
        before.replace("{title}", &title),
        if entry.dirty {
            BUFFER_DIRTY_MARKER.to_owned()
        } else {
            String::new()
        },
        after.replace("{title}", &title),
        if entry.active {
            Style::BufferLineActive
        } else {
            Style::BufferLineInactive
        },
    )
}

pub fn builtin_content_tab_segments(
    entry: &ContentTabEntry,
    config: &Config,
) -> Vec<crate::render::chrome::ChromeSegment> {
    use crate::render::chrome::ChromeSegment;
    let (before, dirty, after, style) = formatted_content_tab(entry, config);
    let mut segments = Vec::with_capacity(8);
    let mut push = |name: &str, text, style| {
        segments.push(ChromeSegment {
            name: name.into(),
            text,
            style,
        })
    };
    if entry.pinned {
        push("pin", format!("{BUFFER_PIN_MARKER} "), Style::BufferLinePin);
    }
    // Inactive tabs are drawn in one quiet style so the active one stands out.
    let accent = |role| if entry.active { role } else { style };
    push(
        "icon",
        format!(" {} ", entry.icon),
        accent(Style::BufferLineIcon),
    );
    push("title", before, style);
    if !dirty.is_empty() {
        push("dirty", dirty, Style::BufferLineDirty);
    }
    if let Some(diagnostic) = entry.diagnostic {
        let marker = match diagnostic.severity {
            ContentTabDiagnosticSeverity::Error => BUFFER_ERROR_MARKER,
            ContentTabDiagnosticSeverity::Warning => BUFFER_WARNING_MARKER,
        };
        push(
            "diagnostics",
            format!(" {marker}{}", diagnostic.count),
            Style::BufferLineDiagnostic,
        );
    }
    if entry.closeable {
        push(
            "close",
            format!(" {BUFFER_CLOSE_MARKER}"),
            accent(Style::BufferLineClose),
        );
    }
    push("suffix", after, style);
    push(
        "separator",
        BUFFER_SEPARATOR.to_owned(),
        Style::BufferLineSeparator,
    );
    segments
}

pub(super) fn content_tab_segments(
    entry: &ContentTabEntry,
    config: &Config,
) -> Vec<(String, Style)> {
    entry
        .segments
        .clone()
        .unwrap_or_else(|| builtin_content_tab_segments(entry, config))
        .into_iter()
        .map(|segment| (segment.text, segment.style))
        .collect()
}

pub(super) fn content_tab_width(entry: &ContentTabEntry, config: &Config) -> usize {
    content_tab_segments(entry, config)
        .iter()
        .map(|(text, _)| cell_text_width(text, config.editor.ambiguous_width))
        .sum()
}

#[must_use]
pub fn content_tab_at_column(
    entries: &[ContentTabEntry],
    config: &Config,
    width: u16,
    column: u16,
) -> Option<ContentTabIdentity> {
    content_tab_hit(entries, config, width, column).map(|(identity, _)| identity)
}

pub fn content_segment_at_column(
    entries: &[ContentTabEntry],
    config: &Config,
    width: u16,
    column: u16,
) -> Option<String> {
    let (identity, offset) = content_tab_hit(entries, config, width, column)?;
    let entry = entries.iter().find(|entry| entry.identity == identity)?;
    let mut x = 0;
    for segment in entry.segments.as_ref()? {
        let end = x + cell_text_width(&segment.text, config.editor.ambiguous_width);
        if usize::from(offset) >= x && usize::from(offset) < end {
            return Some(segment.name.clone());
        }
        x = end;
    }
    None
}

fn content_tab_hit(
    entries: &[ContentTabEntry],
    config: &Config,
    width: u16,
    column: u16,
) -> Option<(ContentTabIdentity, u16)> {
    if entries.is_empty() || column >= width {
        return None;
    }
    let active = entries.iter().position(|e| e.active).unwrap_or(0);
    let left = text_width(BUFFER_OVERFLOW_LEFT, config.editor.ambiguous_width);
    let right = text_width(BUFFER_OVERFLOW_RIGHT, config.editor.ambiguous_width);
    let available = usize::from(width);
    let widths = entries
        .iter()
        .map(|entry| content_tab_width(entry, config))
        .collect::<Vec<_>>();
    let range = visible_content_tabs(&widths, active, available, left, right);
    if range.end < entries.len() && right <= available && usize::from(column) >= available - right {
        return None;
    }
    let mut x = if range.start > 0 {
        u16::try_from(left).unwrap_or(width)
    } else {
        0
    };
    for index in range {
        let entry = &entries[index];
        let item = widths[index];
        let end = x
            .saturating_add(u16::try_from(item).unwrap_or(u16::MAX))
            .min(width);
        if column >= x && column < end {
            return Some((entry.identity, column - x));
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
}

fn visible_content_tabs(
    widths: &[usize],
    active: usize,
    available: usize,
    left: usize,
    right: usize,
) -> std::ops::Range<usize> {
    let mut start = 0;
    let mut required = widths[..=active].iter().sum::<usize>();
    while start < active {
        let markers =
            if start > 0 { left } else { 0 } + if active + 1 < widths.len() { right } else { 0 };
        if required.saturating_add(markers) <= available {
            break;
        }
        required = required.saturating_sub(widths[start]);
        start += 1;
    }
    let mut used = if start > 0 { left } else { 0 };
    let mut end = start;
    for (index, width) in widths.iter().enumerate().skip(start) {
        let reserve = if index + 1 < widths.len() { right } else { 0 };
        if width.saturating_add(reserve) > available.saturating_sub(used) && index > active {
            break;
        }
        used = used.saturating_add(*width);
        end = index + 1;
        if used >= available {
            break;
        }
    }
    start..end
}

pub(super) fn render_content_tab_line(
    grid: &mut CellGrid,
    entries: &[ContentTabEntry],
    config: &Config,
    row: u16,
) {
    let width = grid.width();
    if width == 0 || row >= grid.height() {
        return;
    }
    grid.fill_row(row, Style::BufferLineBackground);
    if entries.is_empty() {
        return;
    }
    let active = entries.iter().position(|entry| entry.active).unwrap_or(0);
    let left_width = text_width(BUFFER_OVERFLOW_LEFT, config.editor.ambiguous_width);
    let right_width = text_width(BUFFER_OVERFLOW_RIGHT, config.editor.ambiguous_width);
    let available = usize::from(width);
    let segments = entries
        .iter()
        .map(|entry| content_tab_segments(entry, config))
        .collect::<Vec<_>>();
    let widths = segments
        .iter()
        .map(|segments| {
            segments
                .iter()
                .map(|(text, _)| cell_text_width(text, config.editor.ambiguous_width))
                .sum()
        })
        .collect::<Vec<usize>>();
    let range = visible_content_tabs(&widths, active, available, left_width, right_width);

    let mut x = 0u16;
    if range.start > 0 {
        x = grid.put_text_clipped(
            x,
            row,
            BUFFER_OVERFLOW_LEFT,
            Style::BufferLineSeparator,
            width,
        );
    }
    for index in range.clone() {
        for (text, style) in &segments[index] {
            x = grid.put_text_clipped(x, row, text, *style, width);
        }
    }
    if range.end < entries.len() && right_width <= available {
        let marker_x = width.saturating_sub(u16::try_from(right_width).unwrap_or(width));
        grid.put_text_clipped(
            marker_x,
            row,
            BUFFER_OVERFLOW_RIGHT,
            Style::BufferLineSeparator,
            width,
        );
    }
}

use unicode_segmentation::UnicodeSegmentation;

pub const NOTIFICATION_MAX_WIDTH: u16 = 120;

pub fn error_notification_geometry(width: u16, height: u16) -> Option<WindowRect> {
    let margin = 1_u16.min(width / 2);
    let available = NOTIFICATION_MAX_WIDTH.min(width.saturating_sub(margin.saturating_mul(2)));
    (available >= 4 && height >= 3).then_some(WindowRect {
        x: width.saturating_sub(margin).saturating_sub(available),
        y: 1.min(height.saturating_sub(3)),
        width: available,
        height: 3,
    })
}
