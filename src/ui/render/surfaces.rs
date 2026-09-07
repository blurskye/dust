use super::*;

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
    let preview_number_width = preview.map_or(1, |preview| {
        preview
            .first_line
            .saturating_add(preview.lines.len())
            .max(1)
            .to_string()
            .len()
    });
    let list_natural_width = picker
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
    let preview_natural_width = preview.map_or(0, |preview| {
        preview
            .lines
            .iter()
            .take(64)
            .map(|line| text_width(line, config.editor.ambiguous_width))
            .max()
            .unwrap_or(28)
            .saturating_add(preview_number_width)
            .saturating_add(4)
            .clamp(32, 88)
    });
    let side_by_side = preview.is_some()
        && usize::from(width)
            >= list_natural_width
                .saturating_add(preview_natural_width)
                .saturating_add(3);
    let divider = side_by_side.then(|| {
        left.saturating_add(u16::try_from(list_natural_width).unwrap_or(width - 1))
            .saturating_add(1)
            .min(left + width - 2)
    });
    let stacked_divider = (preview.is_some() && !side_by_side).then(|| {
        let minimum = top.saturating_add(6);
        let maximum = top.saturating_add(height.saturating_sub(4));
        top.saturating_add(height.saturating_mul(2) / 3)
            .clamp(minimum, maximum)
    });
    let list_right = divider.unwrap_or(left + width - 1);
    let list_bottom = stacked_divider.unwrap_or(top + height - 1);
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
    frame
        .grid
        .put_text(left + 2, top, &format!(" {} ", picker.title), Style::Accent);
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
    frame
        .grid
        .put_text(content_left, query_y, &picker.query, Style::Default);
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
    let first_row = top + 3;
    let visible_rows = usize::from(list_bottom.saturating_sub(first_row));
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
        let detail = if row.detail.is_empty() {
            String::new()
        } else {
            format!("  {}", row.detail)
        };
        let source = if row.source.is_empty() {
            String::new()
        } else {
            format!("{}{}", configured.source_separator, row.source)
        };
        let text = format!("{} {}{}{}", marker, row.label, detail, source);
        if selected {
            for x in left + 1..list_right {
                frame.grid.set_cell(x, y, " ", Style::Selection);
            }
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
            list_right,
        );
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
        let available_rows = usize::from(preview_bottom.saturating_sub(preview_top));
        let highlighted_offset = preview
            .highlight_line
            .and_then(|line| line.checked_sub(preview.first_line));
        let first_visible_offset = highlighted_offset.map_or(0, |offset| {
            offset.saturating_add(1).saturating_sub(available_rows)
        });
        for (visible_offset, (offset, line)) in preview
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
            let line_index = preview.first_line.saturating_add(offset);
            let highlighted = preview.highlight_line == Some(line_index);
            if highlighted {
                for x in preview_fill_left..preview_right {
                    frame.grid.set_cell(x, y, " ", Style::Selection);
                }
            }
            let number = format!("{:>preview_number_width$} ", line_index.saturating_add(1));
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
    let cursor_x = content_left.saturating_add(
        u16::try_from(text_width(&picker.query, config.editor.ambiguous_width)).unwrap_or(0),
    );
    frame.cursor = Some((
        cursor_x.min(list_right.saturating_sub(1)),
        query_y,
        cursor_style(config.ui.cursor.prompt),
    ));
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
        || format!(" {} ", configured.title),
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
    let clip = |text: &str| {
        text.chars()
            .take(usize::from(content_width))
            .collect::<String>()
    };
    let clip_indented = |text: &str| {
        text.chars()
            .take(usize::from(content_width.saturating_sub(2)))
            .collect::<String>()
    };
    let field = |text: &str, width: usize| {
        let text = text.chars().take(width).collect::<String>();
        format!("{text:<width$}")
    };
    let mut y = top + 1;

    if let Some(picker) = &menu.runtime_picker {
        frame.grid.put_text(
            content_left,
            y,
            &clip("Choose between PATH and this host's Dust fallback; only ACTIVE is used."),
            Style::Muted,
        );
        y += 1;
        let source_help = if picker.options.iter().any(|option| option.source == "nix") {
            "PATH: external  Dust via Nix: verified artifact + private compatibility"
        } else {
            "PATH: external  Dust download: portable non-NixOS copy"
        };
        frame
            .grid
            .put_text(content_left, y, &clip(source_help), Style::Muted);
        y += 1;
        frame.grid.put_text(
            content_left,
            y,
            &clip("j/k choose  Enter select  i install  u remove Dust copy  r refresh  Esc back"),
            Style::Muted,
        );
        y += 1;
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
            .put_text(picker_release_x, y, "BUILD / RELEASE", Style::Muted);
        frame
            .grid
            .put_text(picker_source_x, y, "SOURCE", Style::Muted);
        frame
            .grid
            .put_text(picker_state_x, y, "STATE", Style::Muted);
        frame
            .grid
            .put_text(picker_active_x, y, "ACTIVE", Style::Muted);
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
            let active = if option.selected { "yes" } else { "" };
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
            frame
                .grid
                .put_text(picker_version_x, y, &option.version, style);
            frame
                .grid
                .put_text(picker_release_x, y, &option.release, style);
            frame
                .grid
                .put_text(picker_source_x, y, &option.label, style);
            frame
                .grid
                .put_text(picker_state_x, y, &option.status, style);
            frame.grid.put_text(picker_active_x, y, active, style);
            y += 1;
            frame.grid.put_text(
                content_left + 2,
                y,
                &clip_indented(&option.location),
                if option.installed {
                    Style::Accent
                } else {
                    Style::Muted
                },
            );
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
            let action = if option.source == "path" && option.installed {
                "Enter: use this external PATH build"
            } else if option.installed {
                "Enter: make this build ACTIVE   u: uninstall this copy"
            } else if option.installable {
                "Enter: make this build ACTIVE   i: install this build"
            } else {
                "This version is unavailable on the current host"
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

    let mut help_lines = Vec::new();
    let mut help_line = String::new();
    for word in configured.help.split_whitespace() {
        let extra = usize::from(!help_line.is_empty()) + word.chars().count();
        if !help_line.is_empty()
            && help_line.chars().count().saturating_add(extra) > usize::from(content_width)
        {
            help_lines.push(std::mem::take(&mut help_line));
        }
        if !help_line.is_empty() {
            help_line.push(' ');
        }
        help_line.push_str(word);
    }
    if !help_line.is_empty() {
        help_lines.push(help_line);
    }
    for line in help_lines.into_iter().take(2) {
        frame
            .grid
            .put_text(content_left, y, &clip(&line), Style::Muted);
        y += 1;
    }
    y += 1;
    let category_count = u16::try_from(configured.category_order.len())
        .unwrap_or(1)
        .max(1);
    let category_slot = content_width / category_count;
    for (index, category) in configured.category_order.iter().enumerate() {
        let label = configured
            .categories
            .get(category)
            .map(String::as_str)
            .unwrap_or(category);
        let text = format!(" {label} ");
        let slot_left = content_left.saturating_add(
            u16::try_from(index)
                .unwrap_or(u16::MAX)
                .saturating_mul(category_slot),
        );
        let text_width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
        let x = slot_left.saturating_add(category_slot.saturating_sub(text_width) / 2);
        let style = if category == &menu.category {
            Style::StatusAccent
        } else {
            Style::Status
        };
        frame.grid.put_text(x, y, &text, style);
    }
    y += 2;
    let server_x = content_left.saturating_add(2);
    let version_x = content_left.saturating_add(content_width.saturating_mul(42) / 100);
    let runtime_x = content_left.saturating_add(content_width.saturating_mul(62) / 100);
    let state_x = content_left.saturating_add(content_width.saturating_mul(82) / 100);
    frame.grid.put_text(server_x, y, "SERVER", Style::Muted);
    frame.grid.put_text(version_x, y, "VERSION", Style::Muted);
    frame.grid.put_text(runtime_x, y, "RUNTIME", Style::Muted);
    frame.grid.put_text(state_x, y, "STATE", Style::Muted);
    y += 1;

    let remaining = top
        .saturating_add(height)
        .saturating_sub(1)
        .saturating_sub(y);
    let detail_rows = 10u16.min(remaining.saturating_sub(1));
    let list_bottom = top + height - 1 - detail_rows;
    let visible_rows = usize::from(list_bottom.saturating_sub(y));
    for (index, row) in menu
        .rows
        .iter()
        .enumerate()
        .skip(menu.scroll)
        .take(visible_rows)
    {
        let selected = index == menu.selected;
        let marker = if selected {
            &configured.selected_marker
        } else {
            &configured.row_marker
        };
        let style = if selected {
            Style::Selection
        } else if row.installed {
            Style::Default
        } else {
            Style::Muted
        };
        if selected {
            for x in content_left..content_left.saturating_add(content_width) {
                frame.grid.set_cell(x, y, " ", Style::Selection);
            }
        }
        frame.grid.put_text(content_left, y, marker, style);
        frame.grid.put_text(
            server_x,
            y,
            &field(
                &row.name,
                usize::from(version_x.saturating_sub(server_x).saturating_sub(1)),
            ),
            style,
        );
        frame.grid.put_text(
            version_x,
            y,
            &field(
                &row.version,
                usize::from(runtime_x.saturating_sub(version_x).saturating_sub(1)),
            ),
            style,
        );
        frame.grid.put_text(
            runtime_x,
            y,
            &field(
                &row.runtime,
                usize::from(state_x.saturating_sub(runtime_x).saturating_sub(1)),
            ),
            style,
        );
        frame
            .grid
            .put_text(state_x, y, &clip(&row.lifecycle), style);
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
        let status = row.lifecycle.to_uppercase();
        let status_x = content_left
            .saturating_add(content_width)
            .saturating_sub(u16::try_from(status.chars().count()).unwrap_or(u16::MAX));
        frame.grid.put_text(
            content_left,
            y,
            &clip(&format!("{}  ·  {}", row.name, row.languages)),
            Style::Accent,
        );
        frame.grid.put_text(status_x, y, &status, Style::Accent);
        y += 2;

        let right_x = content_left.saturating_add(content_width / 2);
        let action = if row.installed && row.lifecycle.starts_with("running") {
            if row.can_uninstall {
                "x stop  R restart  u remove"
            } else {
                "x stop  R restart"
            }
        } else if row.installed && !row.can_start {
            "Open a supported file to start"
        } else if row.installed {
            if row.can_uninstall {
                "Enter start  u remove"
            } else {
                "Enter start"
            }
        } else if row.can_install {
            "Enter or i install"
        } else {
            "Manual install required"
        };
        let sessions = if row.sessions.is_empty() {
            "none".into()
        } else {
            row.sessions.join(" · ")
        };
        for (left_detail, right_detail) in [
            (
                format!("Runtime   {}", row.runtime),
                format!("Action    {action}"),
            ),
            (
                format!("Version   {}", row.version),
                format!("Sessions  {sessions}"),
            ),
            (
                format!("Build     {}", row.release),
                if row.can_uninstall {
                    "Owned by   Dust".into()
                } else if row.installed {
                    "Owned by   PATH".into()
                } else {
                    "Owned by   not installed".into()
                },
            ),
        ] {
            if y >= top + height - 1 {
                break;
            }
            frame
                .grid
                .put_text(content_left, y, &clip(&left_detail), Style::Muted);
            frame
                .grid
                .put_text(right_x, y, &clip(&right_detail), Style::Muted);
            y += 1;
        }
        if y < top + height - 1 {
            frame.grid.put_text(
                content_left,
                y,
                &clip(&format!("Binary    {}", row.executable)),
                Style::Muted,
            );
            y += 1;
        }
        if y < top + height - 1 {
            frame
                .grid
                .put_text(content_left, y, &clip(&row.note), Style::Muted);
        }
    }
    frame.cursor = None;
}

pub(super) fn render_prompt(
    frame: &mut Frame,
    prompt: &PromptState,
    config: &Config,
    width: u16,
    height: u16,
) {
    let geometry = prompt_geometry(
        width,
        height,
        &config.ui.command_line,
        prompt.error.is_some() || prompt.help.is_some(),
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
    let show_help = prompt.error.is_none() && prompt.help.is_some() && geometry.height >= 4;
    let bottom_y = geometry.top.saturating_add(if show_help { 3 } else { 2 });
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
        let title = format!(" {} ", prompt.kind.title(&config.ui.command_line.text));
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
    let input_x = geometry
        .left
        .saturating_add(config.ui.command_line.input_padding);
    let input_y = geometry.top.saturating_add(if show_help { 2 } else { 1 });
    let content_right = right.saturating_sub(1);
    if show_help && let Some(help) = &prompt.help {
        frame.grid.put_text_clipped(
            input_x,
            geometry.top.saturating_add(1),
            help,
            Style::Muted,
            content_right,
        );
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
        for x in geometry.left..right {
            frame.grid.set_cell(x, error_y, " ", Style::Default);
        }
        frame.grid.put_text_clipped(
            geometry
                .left
                .saturating_add(config.ui.command_line.input_padding),
            error_y,
            error,
            Style::Error,
            right,
        );
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
