//! Notifications, key hints and overlay drawing.

use crate::config::AmbiguousWidth;
use crate::config::Config;
use crate::config::DashboardConfig;
use crate::ui::CellGrid;
use crate::ui::Style;
use crate::ui::geometry::cell_text_width;
use crate::ui::geometry::text_width;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::Frame;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::ui::render::COMPLETION_DETAIL_GAP;
use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::ui::render::ContentTabDiagnosticSeverity;
use crate::ui::render::ContentTabEntry;
use crate::ui::render::ContentTabIdentity;
use crate::ui::render::DashboardAction;
use crate::ui::render::LanguageCompletionState;
use crate::ui::render::UiLspProgress;
use crate::ui::render::editor::cursor_style;
use crate::windows::WindowRect;
use std::path::Path;

use crate::ui::render::documentation::SWATCH_WIDTH;
use crate::ui::render::documentation::color_swatch_style;

pub(crate) const BUFFER_NAME_MAX_WIDTH: u16 = 32;

pub(crate) const BUFFER_TAB_FORMAT: &str = " {title}{dirty}  ";

pub(crate) const BUFFER_SEPARATOR: &str = "│";

pub(crate) const BUFFER_DIRTY_MARKER: &str = " ●";

pub(crate) const BUFFER_PIN_MARKER: &str = "P";

pub(crate) const BUFFER_CLOSE_MARKER: &str = "x";

pub(crate) const BUFFER_ERROR_MARKER: &str = "E";

pub(crate) const BUFFER_WARNING_MARKER: &str = "W";

pub(crate) const BUFFER_OVERFLOW_LEFT: &str = "‹ ";

pub(crate) const BUFFER_OVERFLOW_RIGHT: &str = " ›";

pub(crate) const SCRATCH_ICON: &str = "\u{f039a}";

pub(crate) const VIRTUAL_ICON: &str = "\u{f0219}";

pub(crate) const TERMINAL_ICON: &str = "\u{f489}";

pub(crate) const CUSTOM_ICON: &str = "\u{f0214}";

pub(crate) const AGENT_ICON: &str = "\u{f06a9}";

pub(crate) const LSP_PROGRESS_MAX_ROWS: u16 = 8;

pub(crate) const LSP_PROGRESS_MAX_WIDTH: u16 = 72;

pub(crate) const LSP_PROGRESS_RIGHT_MARGIN: u16 = 1;

pub(crate) const LSP_PROGRESS_ROWS_FROM_BOTTOM: u16 = 1;

pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) const SPINNER_INTERVAL_MS: u64 = 80;

pub(crate) const LSP_PROGRESS_DONE: &str = "✓";

pub(crate) const LSP_PROGRESS_DONE_TIMEOUT_MS: u64 = 3_000;

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
    let swatch = |item: &crate::ui::CompletionItem| {
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
            &crate::ui::geometry::ellipsized(
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

pub(super) fn render_dashboard(
    root: &Path,
    configured_actions: &[DashboardAction],
    selected: usize,
    configured: &DashboardConfig,
    editor_config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let ambiguous = editor_config.editor.ambiguous_width;
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        ambiguous == AmbiguousWidth::Wide,
    );
    let measure = |text: &str| u16::try_from(text_width(text, ambiguous)).unwrap_or(u16::MAX);
    let logo: Vec<&str> = configured.logo.lines().collect();
    let logo_width = logo.iter().map(|line| measure(line)).max().unwrap_or(0);
    let actions = u16::try_from(configured_actions.len()).unwrap_or(u16::MAX);
    let footer = if configured.show_path { 2 } else { 0 };
    // Keep a full logo away from the window edges. Drop it before compressing entries,
    // rather than squeezing the header against the top of a terminal dock's workspace.
    let big_logo = !logo.is_empty() && logo_width.saturating_add(2) <= width;
    let layouts = [
        (true, false, configured.show_version, 2),
        (false, configured.show_identity, configured.show_version, 2),
        (false, configured.show_identity, configured.show_version, 1),
        (false, false, configured.show_version, 1),
        (false, false, false, 1),
    ];
    let fitting = layouts
        .into_iter()
        .find_map(|(use_logo, identity, version, row_step)| {
            if use_logo && !big_logo {
                return None;
            }
            let header = if use_logo {
                u16::try_from(logo.len())
                    .unwrap_or(u16::MAX)
                    .saturating_add(u16::from(version))
            } else {
                u16::from(identity).saturating_add(u16::from(version))
            };
            let rows = actions
                .saturating_mul(row_step)
                .saturating_sub(row_step - 1);
            let header = header.saturating_add(if header > 0 { 2 } else { 0 });
            let total = header.saturating_add(rows).saturating_add(footer);
            let required = total.saturating_add(if use_logo { 2 } else { 0 });
            (required <= height).then_some((use_logo, identity, version, row_step, header, total))
        });
    let Some((use_logo, identity, version, row_step, header, total)) = fitting
        .filter(|_| width >= configured.minimum_width && height >= configured.minimum_height)
    else {
        grid.put_text(
            1.min(width.saturating_sub(1)),
            1.min(height.saturating_sub(1)),
            &configured.resize_message,
            Style::Accent,
        );
        return Frame { grid, cursor: None };
    };
    let top = height.saturating_sub(total) / 2;
    let mut y = top;
    if use_logo {
        let left = width.saturating_sub(logo_width) / 2;
        for line in &logo {
            grid.put_text(left, y, line, Style::Accent);
            y = y.saturating_add(1);
        }
    } else if identity {
        center_text(&mut grid, y, &configured.identity, Style::Accent, ambiguous);
        y = y.saturating_add(1);
    }
    if version {
        center_text(
            &mut grid,
            y,
            &configured
                .version_format
                .replace("{version}", env!("CARGO_PKG_VERSION")),
            Style::Muted,
            ambiguous,
        );
    }
    y = top.saturating_add(header);
    let row_width = configured.content_width.min(width.saturating_sub(4));
    let left = width.saturating_sub(row_width) / 2;
    let right = left.saturating_add(row_width);
    let selected = selected.min(configured_actions.len().saturating_sub(1));
    let mut cursor = None;
    for (index, action) in configured_actions.iter().enumerate() {
        if !action.icon.is_empty() {
            grid.put_text_clipped(left, y, &action.icon, Style::Accent, right);
        }
        let label_x = if action.icon.is_empty() {
            left
        } else {
            left.saturating_add(3)
        };
        let key_x = action
            .key
            .as_ref()
            .map(|key| right.saturating_sub(measure(key)));
        let label_right = key_x.map_or(right, |x| x.saturating_sub(1));
        grid.put_text_clipped(label_x, y, &action.label, Style::Default, label_right);
        // Like LazyVim, the cursor rests on the label of the item Enter runs.
        if index == selected && label_x < right && y < height {
            cursor = Some((label_x, y, cursor_style(editor_config.ui.cursor.normal)));
        }
        if let (Some(key), Some(key_x)) = (&action.key, key_x) {
            grid.put_text_clipped(key_x, y, key, Style::Accent, right);
        }
        y = y.saturating_add(row_step);
    }
    if configured.show_path {
        let y = y.saturating_sub(row_step).saturating_add(2);
        center_text(
            &mut grid,
            y,
            &dashboard_path(root, usize::from(width.saturating_sub(4)), ambiguous),
            Style::Muted,
            ambiguous,
        );
    }
    Frame { grid, cursor }
}

pub(super) fn center_text(
    grid: &mut CellGrid,
    y: u16,
    text: &str,
    style: Style,
    ambiguous_width: AmbiguousWidth,
) {
    if y >= grid.height() {
        return;
    }
    let text_width = u16::try_from(text_width(text, ambiguous_width)).unwrap_or(u16::MAX);
    let x = grid.width().saturating_sub(text_width) / 2;
    grid.put_text(x, y, text, style);
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

pub(super) fn render_lsp_progress(
    frame: &mut Frame,
    progress: &[UiLspProgress],
    spinner_frame: usize,
    config: &Config,
    bounds: WindowRect,
) {
    let available_rows = bounds.height.saturating_sub(LSP_PROGRESS_ROWS_FROM_BOTTOM);
    let row_count = usize::from(LSP_PROGRESS_MAX_ROWS.min(available_rows)).min(progress.len());
    if row_count == 0 {
        return;
    }
    let available_width = bounds.width.saturating_sub(LSP_PROGRESS_RIGHT_MARGIN);
    let maximum_width = usize::from(LSP_PROGRESS_MAX_WIDTH.min(available_width));
    if maximum_width == 0 {
        return;
    }
    let spinner = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];
    let right = bounds
        .x
        .saturating_add(bounds.width)
        .saturating_sub(LSP_PROGRESS_RIGHT_MARGIN);
    let top = bounds
        .y
        .saturating_add(bounds.height)
        .saturating_sub(LSP_PROGRESS_ROWS_FROM_BOTTOM)
        .saturating_sub(u16::try_from(row_count).unwrap_or(u16::MAX));
    let first = progress.len().saturating_sub(row_count);

    for (row, item) in progress[first..].iter().enumerate() {
        let marker = if item.done {
            LSP_PROGRESS_DONE
        } else {
            spinner
        };
        let activity = match (item.title.is_empty(), item.message.is_empty()) {
            (false, false) if item.title != item.message => {
                format!("{}: {}", item.title, item.message)
            }
            (false, _) => item.title.clone(),
            (_, false) => item.message.clone(),
            _ => "Working".into(),
        };
        let mut server = truncate_content_title(
            &item.server,
            maximum_width.saturating_sub(2),
            config.editor.ambiguous_width,
        );
        if server.is_empty() {
            server = "LSP".into();
        }
        let server_width = text_width(&server, config.editor.ambiguous_width);
        let mut percentage = item
            .percentage
            .map(|percentage| format!("{percentage:>3}% "))
            .unwrap_or_default();
        let fixed = 2_usize
            .saturating_add(server_width)
            .saturating_add(text_width(&percentage, config.editor.ambiguous_width));
        if fixed > maximum_width {
            percentage.clear();
        }
        let suffix_width =
            server_width.saturating_add(text_width(&percentage, config.editor.ambiguous_width));
        let activity_width = maximum_width.saturating_sub(3).saturating_sub(suffix_width);
        let activity =
            truncate_content_title(&activity, activity_width, config.editor.ambiguous_width);
        let line = if activity.is_empty() {
            format!("{marker} {percentage}{server}")
        } else {
            format!("{marker} {activity} {percentage}{server}")
        };
        let line_width = u16::try_from(text_width(&line, config.editor.ambiguous_width))
            .unwrap_or(u16::MAX)
            .min(available_width);
        let x = right.saturating_sub(line_width);
        let y = top.saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        frame
            .grid
            .put_text_clipped(x, y, &line, Style::Muted, right);
        frame.grid.put_text(x, y, marker, Style::Accent);
        let server_x = right.saturating_sub(u16::try_from(server_width).unwrap_or(u16::MAX));
        frame
            .grid
            .put_text_clipped(server_x, y, &server, Style::Accent, right);
    }
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

pub(crate) fn builtin_content_tab_segments(
    entry: &ContentTabEntry,
    config: &Config,
) -> Vec<crate::ui::render::chrome::ChromeSegment> {
    use crate::ui::render::chrome::ChromeSegment;
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

pub(crate) fn content_segment_at_column(
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

/// The workspace path for the dashboard: home as `~`, and the end kept when it is too wide.
fn dashboard_path(root: &Path, width: usize, ambiguous_width: AmbiguousWidth) -> String {
    let mut text = root.display().to_string();
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
        && let Ok(rest) = root.strip_prefix(&home)
    {
        text = format!("~/{}", rest.display());
    }
    truncate_content_title(&text, width, ambiguous_width)
}
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const NOTIFICATION_MAX_WIDTH: u16 = 120;

pub(crate) fn error_notification_geometry(width: u16, height: u16) -> Option<WindowRect> {
    let margin = 1_u16.min(width / 2);
    let available = NOTIFICATION_MAX_WIDTH.min(width.saturating_sub(margin.saturating_mul(2)));
    (available >= 4 && height >= 3).then_some(WindowRect {
        x: width.saturating_sub(margin).saturating_sub(available),
        y: 1.min(height.saturating_sub(3)),
        width: available,
        height: 3,
    })
}

#[cfg(test)]
mod tests {
    use crate::ui::render::overlays::*;

    #[test]
    fn dashboard_paths_keep_their_suffix_within_display_columns() {
        for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
            let path = format!("/tmp/{}e\u{301}.rs", "界".repeat(20));
            let shown = dashboard_path(Path::new(&path), 36, ambiguous);
            assert!(shown.starts_with('…'));
            assert!(shown.ends_with("e\u{301}.rs"));
            assert!(cell_text_width(&shown, ambiguous) <= 36);
            for width in 0..4 {
                let shown = dashboard_path(Path::new(&path), width, ambiguous);
                assert!(cell_text_width(&shown, ambiguous) <= width);
            }
        }
    }
}
