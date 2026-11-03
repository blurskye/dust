use crate::ui::render::{COMPLETION_DETAIL_GAP, COMPLETION_MAX_ROWS, CONTROL_CHARACTER_REPLACEMENT, ZERO_WIDTH_REPLACEMENT};
use std::path::Path;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    config::{AmbiguousWidth, BorderStyle, Config, DashboardConfig},
    ui::{CellGrid, Style, geometry::text_width, window::WindowRect},
};

use super::{
    ContentTabDiagnosticSeverity, ContentTabEntry, ContentTabIdentity, DashboardAction,
    DocumentationPopupKind, DocumentationPopupState, Frame, LanguageCompletionState, UiLspProgress,
    editor::cursor_style,
};

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
pub(crate) const DOCUMENTATION_MAX_WIDTH: u16 = 72;
pub(crate) const DOCUMENTATION_MAX_HEIGHT: u16 = 18;
pub(crate) const DOCUMENTATION_PADDING: u16 = 1;
pub(crate) const DOCUMENTATION_SCROLL_STEP: usize = 4;
pub(crate) const LSP_PROGRESS_MAX_ROWS: u16 = 8;
pub(crate) const LSP_PROGRESS_MAX_WIDTH: u16 = 72;
pub(crate) const LSP_PROGRESS_RIGHT_MARGIN: u16 = 1;
pub(crate) const LSP_PROGRESS_ROWS_FROM_BOTTOM: u16 = 1;
pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub(crate) const SPINNER_INTERVAL_MS: u64 = 80;
pub(crate) const LSP_PROGRESS_DONE: &str = "✓";
pub(crate) const LSP_PROGRESS_DONE_TIMEOUT_MS: u64 = 3_000;

pub(super) fn render_documentation_popup(
    frame: &mut Frame,
    popup: &DocumentationPopupState,
    config: &Config,
    editor_left: u16,
    editor_width: u16,
    height: u16,
    completion_menu: Option<WindowRect>,
) {
    let enabled = config.ui.documentation_popup.enabled;
    let editor_right = editor_left.saturating_add(editor_width);
    let available_height = height;
    let line_count = popup.line_count();
    if !enabled || line_count == 0 || editor_width < 4 || available_height < 3 {
        return;
    }
    let page_rows = crate::ui::geometry::documentation_page_rows(config, editor_width, height);
    let mut scrollable = line_count > page_rows;
    let widest = (0..line_count)
        .filter_map(|index| popup.line_at(index))
        .map(UnicodeWidthStr::width)
        .chain(std::iter::once(UnicodeWidthStr::width(
            popup.title.as_str(),
        )))
        .chain(scrollable.then_some(UnicodeWidthStr::width("j/k or wheel · 0000-0000/0000")))
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
    let mut visible_rows = line_count.saturating_sub(popup.scroll).min(page_rows);
    let mut box_height = u16::try_from(visible_rows.saturating_add(2))
        .unwrap_or(DOCUMENTATION_MAX_HEIGHT)
        .min(DOCUMENTATION_MAX_HEIGHT)
        .min(available_height);
    if box_width < 4 || box_height < 3 {
        return;
    }
    // The dashboard has no cursor to sit beside, so the box goes mid-screen there.
    let (cursor_x, cursor_y) = frame.cursor.map_or(
        (
            editor_left.saturating_add(editor_width.saturating_sub(box_width) / 2),
            (available_height.saturating_sub(box_height) / 2).saturating_sub(1),
        ),
        |(x, y, _)| (x, y),
    );
    let below = cursor_y.saturating_add(1);
    let room_below = available_height.saturating_sub(below);
    let top = if room_below >= box_height || cursor_y < box_height {
        below.min(available_height.saturating_sub(box_height))
    } else {
        cursor_y.saturating_sub(box_height)
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
    let (left, top) = match completion_menu
        .filter(|_| popup.kind == DocumentationPopupKind::Completion)
    {
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
                    && y.saturating_add(box_height) <= available_height
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
                        return;
                    }
                    box_width = box_width.min(right_room.max(left_room));
                    let room = if menu.y > cursor_y {
                        available_height.saturating_sub(menu.y)
                    } else {
                        cursor_y.saturating_sub(menu.y)
                    };
                    box_height = box_height.min(room.max(menu.height).max(3));
                    visible_rows = visible_rows.min(usize::from(box_height - 2));
                    scrollable = line_count > visible_rows;
                    if right_room >= left_room {
                        (beside_right, menu.y)
                    } else {
                        (menu.x - box_width, menu.y)
                    }
                }
            }
        }
    };
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
        BorderStyle::Rounded.glyphs();
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
    let text_left = left
        .saturating_add(1)
        .saturating_add(DOCUMENTATION_PADDING);
    for (row, (index, (line, spans))) in (popup.scroll..popup.scroll.saturating_add(visible_rows))
        .filter_map(|index| popup.at(index).map(|line| (index, line)))
        .enumerate()
    {
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
        let limit = right.saturating_sub(1).saturating_sub(DOCUMENTATION_PADDING);
        // Rendered Markdown: the runs between styled ones keep the line's style.
        let mut x = text_left;
        let mut at = 0;
        for (range, role) in spans {
            let (Some(before), Some(styled)) = (line.get(at..range.start), line.get(range.clone()))
            else {
                continue;
            };
            x = frame.grid.put_text_clipped(x, y, before, style, limit);
            x = frame
                .grid
                .put_text_clipped(x, y, styled, Style::DocumentationSyntax(*role), limit);
            at = range.end;
        }
        frame.grid.put_text_clipped(x, y, line.get(at..).unwrap_or(""), style, limit);
    }
    if scrollable {
        let footer = format!(
            " j/k or wheel · {}-{}/{} ",
            popup.scroll.saturating_add(1),
            popup.scroll.saturating_add(visible_rows).min(line_count),
            line_count
        );
        frame.grid.put_text_clipped(
            left.saturating_add(1),
            bottom,
            &footer,
            Style::DocumentationTitle,
            right.saturating_sub(1),
        );
    }
}

/// Two cells read as a square swatch; the third is the gap before the detail.
const SWATCH_WIDTH: usize = 3;

/// A cell painted in a colour from a language server, with text that stays readable on it.
pub(super) fn color_swatch_style((red, green, blue): (u8, u8, u8)) -> Style {
    let luminance = 299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue);
    let text = if luminance > 128_000 { (0, 0, 0) } else { (255, 255, 255) };
    Style::Terminal(crate::ui::PtyStyle {
        foreground: Some(crate::ui::PtyColor::Rgb(text.0, text.1, text.2)),
        background: Some(crate::ui::PtyColor::Rgb(red, green, blue)),
        ..Default::default()
    })
}

fn boxes_overlap(at: (u16, u16), size: (u16, u16), other: WindowRect) -> bool {
    at.0 < other.x.saturating_add(other.width)
        && other.x < at.0.saturating_add(size.0)
        && at.1 < other.y.saturating_add(other.height)
        && other.y < at.1.saturating_add(size.1)
}

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
    let rows = completion
        .items
        .len()
        .min(usize::from(COMPLETION_MAX_ROWS))
        .min(usize::from(bounds.height.saturating_sub(1)));
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
        if item.color.is_some() { SWATCH_WIDTH } else { 0 }
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
        .saturating_add(if source_width > 0 { gap + source_width } else { 0 })
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
            &item.label,
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
        frame.grid.set_cell(x, y, " ", Style::DocumentationBackground);
    }
    frame
        .grid
        .put_text(left.saturating_add(1), y, text, Style::DocumentationText);
}

pub(super) fn render_dashboard(
    root: &Path,
    configured_actions: &[DashboardAction],
    configured: &DashboardConfig,
    editor_config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let ambiguous = editor_config.editor.ambiguous_width;
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &CONTROL_CHARACTER_REPLACEMENT,
        &ZERO_WIDTH_REPLACEMENT,
        ambiguous == AmbiguousWidth::Wide,
    );
    let measure = |text: &str| u16::try_from(text_width(text, ambiguous)).unwrap_or(u16::MAX);
    let logo: Vec<&str> = configured.logo.lines().collect();
    let logo_width = logo.iter().map(|line| measure(line)).max().unwrap_or(0);
    let actions = u16::try_from(configured_actions.len()).unwrap_or(u16::MAX);
    let version = u16::from(configured.show_version);
    let footer = if configured.show_path { 2 } else { 0 };
    // Like LazyVim's dashboard: the big logo and roomy rows when they fit, then smaller
    // pieces as the window shrinks.
    let big_logo = !logo.is_empty() && logo_width.saturating_add(2) <= width;
    let layouts = [(true, 2), (true, 1), (false, 2), (false, 1)];
    let fitting = layouts.into_iter().find_map(|(use_logo, row_step)| {
        if use_logo && !big_logo {
            return None;
        }
        let header = if use_logo {
            u16::try_from(logo.len()).unwrap_or(u16::MAX).saturating_add(version)
        } else {
            u16::from(configured.show_identity).saturating_add(version)
        };
        let rows = actions.saturating_mul(row_step).saturating_sub(row_step - 1);
        let total = header.saturating_add(2).saturating_add(rows).saturating_add(footer);
        (total <= height).then_some((use_logo, row_step, header, total))
    });
    let Some((use_logo, row_step, header, total)) = fitting.filter(|_| {
        width >= configured.minimum_width && height >= configured.minimum_height
    }) else {
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
    } else if configured.show_identity {
        center_text(&mut grid, y, &configured.identity, Style::Accent, ambiguous);
        y = y.saturating_add(1);
    }
    if configured.show_version {
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
    y = top.saturating_add(header).saturating_add(2);
    let row_width = configured.content_width.min(width.saturating_sub(4));
    let left = width.saturating_sub(row_width) / 2;
    let right = left.saturating_add(row_width);
    for action in configured_actions {
        if !action.icon.is_empty() {
            grid.put_text_clipped(left, y, &action.icon, Style::Accent, right);
        }
        let label_x = if action.icon.is_empty() { left } else { left.saturating_add(3) };
        grid.put_text_clipped(label_x, y, &action.label, Style::Default, right);
        if let Some(key) = &action.key {
            let key_x = right.saturating_sub(measure(key));
            grid.put_text_clipped(key_x, y, key, Style::Accent, right);
        }
        y = y.saturating_add(row_step);
    }
    if configured.show_path {
        let y = y.saturating_sub(row_step).saturating_add(2);
        center_text(
            &mut grid,
            y,
            &dashboard_path(root, usize::from(width.saturating_sub(4))),
            Style::Muted,
            ambiguous,
        );
    }
    Frame { grid, cursor: None }
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
    if text_width(title, ambiguous_width) <= maximum_width {
        return title.to_owned();
    }
    let ellipsis = "…";
    let available = maximum_width.saturating_sub(text_width(ellipsis, ambiguous_width));
    let mut suffix = Vec::new();
    let mut width = 0usize;
    for grapheme in title.graphemes(true).rev() {
        let grapheme_width = text_width(grapheme, ambiguous_width).max(1);
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
        let marker = if item.done { LSP_PROGRESS_DONE } else { spinner };
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
        frame.grid.put_text_clipped(
            server_x,
            y,
            &server,
            Style::Accent,
            right.saturating_sub(server_x),
        );
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
    let format = BUFFER_TAB_FORMAT.replace("{title}", &title);
    let (before, after) = format.split_once("{dirty}").expect("buffer tab format has {dirty}");
    (
        before.to_owned(),
        if entry.dirty {
            BUFFER_DIRTY_MARKER.to_owned()
        } else {
            String::new()
        },
        after.to_owned(),
        if entry.active {
            Style::BufferLineActive
        } else {
            Style::BufferLineInactive
        },
    )
}

pub(super) fn content_tab_segments(
    entry: &ContentTabEntry,
    config: &Config,
) -> Vec<(String, Style)> {
    let (before, dirty, after, style) = formatted_content_tab(entry, config);
    let mut segments = Vec::with_capacity(8);
    if entry.pinned {
        segments.push((format!("{BUFFER_PIN_MARKER} "), Style::BufferLinePin));
    }
    // Inactive tabs are drawn in one quiet style so the active one stands out.
    let accent = |role| if entry.active { role } else { style };
    segments.push((format!(" {} ", entry.icon), accent(Style::BufferLineIcon)));
    segments.push((before, style));
    if !dirty.is_empty() {
        segments.push((dirty, Style::BufferLineDirty));
    }
    if let Some(diagnostic) = entry.diagnostic {
        let marker = match diagnostic.severity {
            ContentTabDiagnosticSeverity::Error => BUFFER_ERROR_MARKER,
            ContentTabDiagnosticSeverity::Warning => BUFFER_WARNING_MARKER,
        };
        segments.push((
            format!(" {marker}{}", diagnostic.count),
            Style::BufferLineDiagnostic,
        ));
    }
    if entry.closeable {
        segments.push((format!(" {BUFFER_CLOSE_MARKER}"), accent(Style::BufferLineClose)));
    }
    segments.push((after, style));
    segments.push((BUFFER_SEPARATOR.to_owned(), Style::BufferLineSeparator));
    segments
}
pub(super) fn content_tab_width(entry: &ContentTabEntry, config: &Config) -> usize {
    content_tab_segments(entry, config)
        .iter()
        .map(|(text, _)| text_width(text, config.editor.ambiguous_width))
        .sum()
}

#[must_use]
pub fn content_tab_at_column(
    entries: &[ContentTabEntry],
    config: &Config,
    width: u16,
    column: u16,
) -> Option<ContentTabIdentity> {
    if entries.is_empty() || column >= width {
        return None;
    }
    let active = entries.iter().position(|e| e.active).unwrap_or(0);
    let left = text_width(
        &BUFFER_OVERFLOW_LEFT,
        config.editor.ambiguous_width,
    );
    let right = text_width(
        &BUFFER_OVERFLOW_RIGHT,
        config.editor.ambiguous_width,
    );
    let available = usize::from(width);
    let mut start = 0;
    while start < active {
        let required = entries[start..=active]
            .iter()
            .map(|e| content_tab_width(e, config))
            .sum::<usize>()
            .saturating_add(if start > 0 { left } else { 0 })
            .saturating_add(if active + 1 < entries.len() { right } else { 0 });
        if required <= available {
            break;
        }
        start += 1
    }
    let mut x = if start > 0 {
        u16::try_from(left).unwrap_or(width)
    } else {
        0
    };
    for (index, entry) in entries.iter().enumerate().skip(start) {
        let item = content_tab_width(entry, config);
        let remaining = usize::from(width.saturating_sub(x));
        let reserve = if index + 1 < entries.len() { right } else { 0 };
        if item.saturating_add(reserve) > remaining && index > active {
            break;
        }
        let end = x
            .saturating_add(u16::try_from(item).unwrap_or(u16::MAX))
            .min(width);
        if column >= x && column < end {
            return Some(entry.identity);
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
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
    let left_width = text_width(
        &BUFFER_OVERFLOW_LEFT,
        config.editor.ambiguous_width,
    );
    let right_width = text_width(
        &BUFFER_OVERFLOW_RIGHT,
        config.editor.ambiguous_width,
    );
    let available = usize::from(width);
    let mut start = 0usize;
    while start < active {
        let required = entries[start..=active]
            .iter()
            .map(|entry| content_tab_width(entry, config))
            .sum::<usize>()
            .saturating_add(if start > 0 { left_width } else { 0 })
            .saturating_add(if active + 1 < entries.len() {
                right_width
            } else {
                0
            });
        if required <= available {
            break;
        }
        start += 1;
    }

    let mut x = 0u16;
    if start > 0 {
        x = grid.put_text_clipped(
            x,
            row,
            &BUFFER_OVERFLOW_LEFT,
            Style::BufferLineSeparator,
            width,
        );
    }
    let mut end = start;
    for (index, entry) in entries.iter().enumerate().skip(start) {
        let item_width = content_tab_width(entry, config);
        let remaining = usize::from(width.saturating_sub(x));
        let reserve_right = if index + 1 < entries.len() {
            right_width
        } else {
            0
        };
        if item_width.saturating_add(reserve_right) > remaining && index > active {
            break;
        }
        for (text, style) in content_tab_segments(entry, config) {
            x = grid.put_text_clipped(x, row, &text, style, width);
        }
        end = index + 1;
        if x >= width {
            break;
        }
    }
    if end < entries.len() && right_width <= available {
        let marker_x = width.saturating_sub(u16::try_from(right_width).unwrap_or(width));
        grid.put_text_clipped(
            marker_x,
            row,
            &BUFFER_OVERFLOW_RIGHT,
            Style::BufferLineSeparator,
            width,
        );
    }
}

/// The workspace path for the dashboard: home as `~`, and the end kept when it is too wide.
fn dashboard_path(root: &Path, width: usize) -> String {
    let mut text = root.display().to_string();
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
        && let Ok(rest) = root.strip_prefix(&home)
    {
        text = format!("~/{}", rest.display());
    }
    let count = text.chars().count();
    if width > 1 && count > width {
        let tail = text.chars().skip(count + 1 - width).collect::<String>();
        text = format!("…{tail}");
    }
    text
}
