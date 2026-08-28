use super::*;

pub(super) fn render_documentation_popup(
    frame: &mut Frame,
    popup: &DocumentationPopupState,
    config: &Config,
    editor_left: u16,
    editor_width: u16,
    height: u16,
) {
    let policy = &config.ui.documentation_popup;
    let editor_right = editor_left.saturating_add(editor_width);
    let Some((cursor_x, cursor_y, _)) = frame.cursor else {
        return;
    };
    let available_height = height.saturating_sub(u16::from(config.ui.status_line.enabled));
    let line_count = popup.line_count();
    if !policy.enabled || line_count == 0 || editor_width < 4 || available_height < 3 {
        return;
    }
    let widest = (0..line_count)
        .filter_map(|index| popup.line_at(index))
        .map(UnicodeWidthStr::width)
        .chain(std::iter::once(UnicodeWidthStr::width(
            popup.title.as_str(),
        )))
        .max()
        .unwrap_or(1)
        .saturating_add(usize::from(policy.horizontal_padding).saturating_mul(2))
        .saturating_add(2);
    let box_width = u16::try_from(widest)
        .unwrap_or(policy.max_width)
        .min(policy.max_width)
        .min(editor_width);
    let visible_rows = line_count
        .saturating_sub(popup.scroll)
        .min(usize::from(policy.max_height.saturating_sub(2)))
        .min(usize::from(available_height.saturating_sub(2)));
    let box_height = u16::try_from(visible_rows.saturating_add(2))
        .unwrap_or(policy.max_height)
        .min(policy.max_height)
        .min(available_height);
    if box_width < 4 || box_height < 3 {
        return;
    }
    let below = cursor_y.saturating_add(1);
    let room_below = available_height.saturating_sub(below);
    let top = if (policy.preferred_below && room_below >= box_height) || cursor_y < box_height {
        below.min(available_height.saturating_sub(box_height))
    } else {
        cursor_y.saturating_sub(box_height)
    };
    let left = if cursor_x.saturating_add(box_width) <= editor_right {
        cursor_x.max(editor_left)
    } else {
        cursor_x
            .saturating_add(1)
            .saturating_sub(box_width)
            .max(editor_left)
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
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) = match policy.border
    {
        BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
        BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
        BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
    };
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
        .saturating_add(policy.horizontal_padding);
    for (row, line) in (popup.scroll..popup.scroll.saturating_add(visible_rows))
        .filter_map(|index| popup.line_at(index))
        .enumerate()
    {
        frame.grid.put_text_clipped(
            text_left,
            top.saturating_add(1)
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
            line,
            Style::DocumentationText,
            right
                .saturating_sub(1)
                .saturating_sub(policy.horizontal_padding),
        );
    }
}

pub(super) fn render_language_completion(
    frame: &mut Frame,
    completion: &LanguageCompletionState,
    config: &Config,
    width: u16,
    height: u16,
) {
    if completion.items.is_empty() || width < 4 || height < 3 {
        return;
    }
    let (cursor_x, cursor_y, _) =
        frame
            .cursor
            .unwrap_or((1, 1, cursor_style(config.ui.cursor.normal)));
    let rows = completion
        .items
        .len()
        .min(usize::from(config.ui.command_line.completion_max_rows))
        .min(usize::from(height.saturating_sub(1)));
    if rows == 0 {
        return;
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
    let content_width = visible
        .clone()
        .map(|item| {
            text_width(&item.label, config.editor.ambiguous_width)
                .saturating_add(config.ui.command_line.completion_detail_gap.into())
                .saturating_add(text_width(&item.detail, config.editor.ambiguous_width))
        })
        .max()
        .unwrap_or(1)
        .min(usize::from(width.saturating_sub(2)));
    let panel_width = u16::try_from(content_width)
        .unwrap_or(width)
        .saturating_add(2)
        .min(width);
    let left = cursor_x.min(width.saturating_sub(panel_width));
    let below = cursor_y.saturating_add(1);
    let top = if below.saturating_add(u16::try_from(rows).unwrap_or(0)) <= height {
        below
    } else {
        cursor_y.saturating_sub(u16::try_from(rows).unwrap_or(0))
    };
    for (row, item) in visible.enumerate() {
        let y = top.saturating_add(u16::try_from(row).unwrap_or(0));
        let style = if scroll.saturating_add(row) == selected {
            Style::Selection
        } else {
            Style::Status
        };
        for x in left..left.saturating_add(panel_width) {
            frame.grid.set_cell(x, y, " ", style);
        }
        frame
            .grid
            .put_text(left.saturating_add(1), y, &item.label, style);
        let detail_x = left
            .saturating_add(1)
            .saturating_add(
                u16::try_from(text_width(&item.label, config.editor.ambiguous_width)).unwrap_or(0),
            )
            .saturating_add(config.ui.command_line.completion_detail_gap);
        frame.grid.put_text(detail_x, y, &item.detail, Style::Muted);
    }
}

pub(super) fn render_dashboard(
    root: &Path,
    configured_actions: &[DashboardAction],
    configured: &DashboardConfig,
    editor_config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &editor_config.editor.control_character_replacement,
        &editor_config.editor.zero_width_replacement,
        editor_config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if width < configured.minimum_width || height < configured.minimum_height {
        grid.put_text(
            configured.compact_left.min(width.saturating_sub(1)),
            configured.compact_top.min(height.saturating_sub(1)),
            &configured.resize_message,
            Style::Accent,
        );
        return Frame { grid, cursor: None };
    }
    let content_width = width.min(configured.content_width);
    let left = width.saturating_sub(content_width) / 2;
    let top = height.saturating_sub(configured.block_height) / 2;
    if configured.show_identity {
        center_text(
            &mut grid,
            top.saturating_add(configured.identity_row),
            &configured.identity,
            Style::Accent,
            editor_config.editor.ambiguous_width,
        );
    }
    if configured.show_version {
        center_text(
            &mut grid,
            top.saturating_add(configured.version_row),
            &configured
                .version_format
                .replace("{version}", env!("CARGO_PKG_VERSION")),
            Style::Muted,
            editor_config.editor.ambiguous_width,
        );
    }
    if configured.show_path {
        center_text(
            &mut grid,
            top.saturating_add(configured.path_row),
            &root.display().to_string(),
            Style::Default,
            editor_config.editor.ambiguous_width,
        );
    }
    for (index, action) in configured_actions.iter().enumerate() {
        let y = top
            .saturating_add(configured.actions_row)
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if y >= height {
            break;
        }
        let x = left.saturating_add(content_width / 2).saturating_sub(
            configured
                .action_key_width
                .saturating_add(configured.action_left_offset),
        );
        let key = action.key.as_deref().unwrap_or(&configured.unbound_label);
        let key = configured.key_format.replace("{key}", key);
        grid.put_text(x, y, &key, Style::Accent);
        grid.put_text(
            x.saturating_add(configured.action_key_width),
            y,
            &action.label,
            Style::Default,
        );
    }
    Frame { grid, cursor: None }
}

pub(super) fn text_width(text: &str, ambiguous_width: AmbiguousWidth) -> usize {
    if ambiguous_width == AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(text)
    } else {
        UnicodeWidthStr::width(text)
    }
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
    let policy = &config.ui.lsp_progress;
    let available_rows = bounds.height.saturating_sub(policy.rows_from_bottom);
    let row_count = usize::from(policy.max_rows.min(available_rows)).min(progress.len());
    if row_count == 0 {
        return;
    }
    let available_width = bounds.width.saturating_sub(policy.right_margin);
    let maximum_width = usize::from(policy.max_width.min(available_width));
    if maximum_width == 0 {
        return;
    }
    let spinner = &policy.spinner_frames[spinner_frame % policy.spinner_frames.len()];
    let right = bounds
        .x
        .saturating_add(bounds.width)
        .saturating_sub(policy.right_margin);
    let top = bounds
        .y
        .saturating_add(bounds.height)
        .saturating_sub(policy.rows_from_bottom)
        .saturating_sub(u16::try_from(row_count).unwrap_or(u16::MAX));
    let first = progress.len().saturating_sub(row_count);

    for (row, item) in progress[first..].iter().enumerate() {
        let marker = if item.done {
            &policy.completed_symbol
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
    let format = if entry.active {
        &config.ui.buffer_line.active_format
    } else {
        &config.ui.buffer_line.inactive_format
    };
    let title = truncate_content_title(
        &entry.title,
        usize::from(config.ui.buffer_line.maximum_name_width),
        config.editor.ambiguous_width,
    );
    let format = format.replace("{title}", &title);
    let (before, after) = format
        .split_once("{dirty}")
        .expect("validated buffer-line format");
    (
        before.to_owned(),
        if entry.dirty {
            config.ui.buffer_line.dirty_marker.clone()
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
    if config.ui.buffer_line.show_pins && entry.pinned {
        segments.push((
            format!("{} ", config.ui.buffer_line.pin_marker),
            Style::BufferLinePin,
        ));
    }
    if config.ui.buffer_line.show_icons {
        segments.push((format!("{} ", entry.icon), Style::BufferLineIcon));
    }
    segments.push((before, style));
    if !dirty.is_empty() {
        segments.push((dirty, Style::BufferLineDirty));
    }
    if config.ui.buffer_line.show_diagnostics
        && let Some(diagnostic) = entry.diagnostic
    {
        let marker = match diagnostic.severity {
            ContentTabDiagnosticSeverity::Error => &config.ui.buffer_line.diagnostic_error_marker,
            ContentTabDiagnosticSeverity::Warning => {
                &config.ui.buffer_line.diagnostic_warning_marker
            }
        };
        segments.push((
            format!(" {marker}{}", diagnostic.count),
            Style::BufferLineDiagnostic,
        ));
    }
    if config.ui.buffer_line.show_close && entry.closeable {
        segments.push((
            format!(" {}", config.ui.buffer_line.close_marker),
            Style::BufferLineClose,
        ));
    }
    segments.push((after, style));
    segments.push((
        config.ui.buffer_line.separator.clone(),
        Style::BufferLineSeparator,
    ));
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
        &config.ui.buffer_line.overflow_left,
        config.editor.ambiguous_width,
    );
    let right = text_width(
        &config.ui.buffer_line.overflow_right,
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
        &config.ui.buffer_line.overflow_left,
        config.editor.ambiguous_width,
    );
    let right_width = text_width(
        &config.ui.buffer_line.overflow_right,
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
            &config.ui.buffer_line.overflow_left,
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
            &config.ui.buffer_line.overflow_right,
            Style::BufferLineSeparator,
            width,
        );
    }
}
