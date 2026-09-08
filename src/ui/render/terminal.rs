use super::*;

pub(super) fn active_compositor<'a>(
    state: &EditorState,
    ui: &'a UiState,
    width: u16,
    height: u16,
) -> Option<&'a CompositorLayoutState> {
    let revision = state.window_revision()?;
    ui.compositor_layout.as_ref().filter(|compositor| {
        compositor.source_revision == revision
            && Some(compositor.graph_revision) == ui.window_graph_revision
            && compositor.projection.bounds.width == width
            && compositor.projection.bounds.height == height
    })
}

pub(crate) fn terminal_geometry_from_rect(rect: WindowRect) -> Option<TerminalPanelGeometry> {
    (rect.width > 0 && rect.height >= 3).then_some(TerminalPanelGeometry {
        x: rect.x,
        width: rect.width,
        top: rect.y,
        height: rect.height,
        body_top: rect.y.saturating_add(1),
        body_height: rect.height.saturating_sub(2),
        status_y: rect.y.saturating_add(rect.height).saturating_sub(1),
    })
}

pub(super) fn terminal_tab_label(tab: &TerminalTabState, config: &Config) -> String {
    let mut title = config
        .ui
        .terminal_panel
        .title_format
        .replace("{id}", &tab.id.to_string())
        .replace("{profile}", &tab.profile);
    if let Some(status) = match tab.phase {
        TerminalPhase::Preflight => Some(" [preflight]"),
        TerminalPhase::Starting => Some(" [starting]"),
        TerminalPhase::Failed => Some(" [failed]"),
        _ => None,
    } {
        title.push_str(status);
    }
    if tab.phase == TerminalPhase::Exited {
        title.push_str(
            &tab.exit_code
                .map_or_else(|| " [signal]".into(), |code| format!(" [exit {code}]")),
        );
    }
    title
}

#[must_use]
pub fn terminal_tab_at_column(
    terminal: &TerminalPanelState,
    config: &Config,
    width: u16,
    column: u16,
) -> Option<u64> {
    let mut x = 1_u16;
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(
                u16::try_from(text_width(
                    &config.ui.terminal_panel.tab_separator,
                    config.editor.ambiguous_width,
                ))
                .unwrap_or(u16::MAX),
            );
        }
        let label = terminal_tab_label(tab, config);
        let end = x
            .saturating_add(
                u16::try_from(text_width(&label, config.editor.ambiguous_width))
                    .unwrap_or(u16::MAX),
            )
            .min(width);
        if column >= x && column < end {
            return Some(tab.id);
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
}

pub(super) fn dock_terminal_panel(
    workspace: Frame,
    terminal: &TerminalPanelState,
    geometry: TerminalPanelGeometry,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    for y in 0..workspace.grid.height().min(height) {
        for x in 0..workspace.grid.width().min(width) {
            let Some(cell) = workspace.grid.get(x, y) else {
                continue;
            };
            if !cell.continuation {
                grid.set_cell(x, y, &cell.symbol, cell.style);
            }
        }
    }
    let right = geometry.x.saturating_add(geometry.width).min(width);
    let bottom = geometry.top.saturating_add(geometry.height).min(height);
    for y in geometry.top..bottom {
        for x in geometry.x..right {
            grid.set_cell(x, y, " ", Style::TerminalBackground);
        }
    }
    for x in geometry.x..right {
        grid.set_cell(
            x,
            geometry.top,
            &config.ui.terminal_panel.divider,
            Style::TerminalBorder,
        );
    }
    let mut title_x = geometry.x.saturating_add(1);
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            title_x = grid.put_text_clipped(
                title_x,
                geometry.top,
                &config.ui.terminal_panel.tab_separator,
                Style::TerminalBorder,
                right,
            );
        }
        let title = terminal_tab_label(tab, config);
        title_x = grid.put_text_clipped(
            title_x,
            geometry.top,
            &title,
            if tab.id == terminal.id {
                Style::TerminalTitle
            } else {
                Style::TerminalBorder
            },
            right,
        );
    }

    let visible_rows = usize::from(geometry.body_height).min(terminal.cells.len());
    let source_start = terminal.cells_start;
    for (screen_row, row) in terminal.cells.iter().take(visible_rows).enumerate() {
        let source_row = source_start.saturating_add(screen_row);
        let y = geometry
            .body_top
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        for (column, cell) in row
            .iter()
            .take(usize::from(geometry.width.min(terminal.columns)))
            .enumerate()
        {
            if cell.continuation {
                continue;
            }
            let selected = terminal.selection.is_some_and(|(first, second, linewise)| {
                let (start, end) = if first <= second {
                    (first, second)
                } else {
                    (second, first)
                };
                if linewise {
                    (start.0..=end.0).contains(&source_row)
                } else {
                    (source_row, column) >= start && (source_row, column) <= end
                }
            });
            grid.set_cell(
                geometry
                    .x
                    .saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
                y,
                &cell.text,
                if selected {
                    Style::Selection
                } else {
                    Style::Terminal(cell.style)
                },
            );
        }
    }

    for x in geometry.x..right {
        grid.set_cell(x, geometry.status_y, " ", Style::TerminalStatus);
    }
    let mode = match terminal.mode {
        TerminalPanelMode::Input => &config.ui.terminal_panel.input_mode_label,
        TerminalPanelMode::Normal => &config.ui.terminal_panel.normal_mode_label,
        TerminalPanelMode::Visual => &config.ui.terminal_panel.visual_mode_label,
    };
    let mut status_line = if terminal.focused {
        format!(" {mode}  {}: {}", terminal.id, terminal.profile)
    } else {
        format!(" {}: {}", terminal.id, terminal.profile)
    };
    if terminal.truncated {
        status_line.push_str(" · output truncated");
    }
    grid.put_text_clipped(
        geometry.x,
        geometry.status_y,
        &status_line,
        if terminal.focused {
            Style::TerminalStatusAccent
        } else {
            Style::TerminalStatus
        },
        right,
    );

    let cursor = if terminal.focused {
        terminal.cursor.and_then(|(column, source_row)| {
            let projected_row = source_row.checked_sub(source_start)?;
            let y = geometry
                .body_top
                .saturating_add(u16::try_from(projected_row).ok()?);
            (column < usize::from(geometry.width) && y < geometry.status_y).then_some((
                geometry.x.saturating_add(u16::try_from(column).ok()?),
                y,
                crossterm::cursor::SetCursorStyle::SteadyBlock,
            ))
        })
    } else {
        workspace.cursor
    };
    Frame { grid, cursor }
}

pub(super) fn render_key_hints(
    frame: &mut Frame,
    hints: &KeyHintUiState,
    config: &Config,
    editor_left: u16,
    editor_width: u16,
    terminal_height: u16,
) {
    let policy = &config.ui.key_hints;
    let available_height = terminal_height.saturating_sub(1);
    if editor_width < 12 || available_height < 4 || hints.rows.is_empty() {
        return;
    }
    let row_count = hints
        .rows
        .len()
        .min(usize::from(policy.max_rows).saturating_mul(usize::from(policy.max_columns)));
    let columns = usize::from(policy.max_columns)
        .min(row_count.div_ceil(usize::from(policy.max_rows)))
        .max(1);
    let rows_per_column = row_count
        .div_ceil(columns)
        .min(usize::from(policy.max_rows));
    let row_stride = usize::from(policy.row_gap) + 1;
    let body_height = rows_per_column
        .saturating_mul(row_stride)
        .saturating_sub(usize::from(policy.row_gap));
    let desired_height = 3usize
        .saturating_add(usize::from(policy.vertical_padding).saturating_mul(2))
        .saturating_add(body_height);
    let box_height = u16::try_from(desired_height)
        .unwrap_or(available_height)
        .min(available_height);
    if box_height < 4 {
        return;
    }
    let widest = hints
        .rows
        .iter()
        .take(row_count)
        .map(|row| {
            UnicodeWidthStr::width(row.next_key.as_str())
                + UnicodeWidthStr::width(row.label.as_str())
                + row
                    .icon
                    .as_deref()
                    .map_or(0, |icon| UnicodeWidthStr::width(icon) + 1)
                + 3
        })
        .max()
        .unwrap_or(1);
    let desired_width = widest
        .saturating_mul(columns)
        .saturating_add(usize::from(policy.column_gap).saturating_mul(columns.saturating_sub(1)))
        .saturating_add(usize::from(policy.horizontal_padding).saturating_mul(2))
        .saturating_add(2);
    let box_width = u16::try_from(desired_width)
        .unwrap_or(policy.max_width)
        .min(policy.max_width)
        .min(editor_width);
    if box_width < 12 {
        return;
    }
    let left = match policy.anchor {
        KeyHintAnchor::BottomLeft => editor_left,
        KeyHintAnchor::BottomRight => {
            editor_left.saturating_add(editor_width.saturating_sub(box_width))
        }
    };
    let top = available_height.saturating_sub(box_height);
    let right = left.saturating_add(box_width);
    let bottom = top.saturating_add(box_height).saturating_sub(1);
    for y in top..=bottom {
        for x in left..right {
            frame.grid.set_cell(x, y, " ", Style::KeyHintBackground);
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
            .set_cell(x, top, &horizontal.to_string(), Style::KeyHintBorder);
        frame
            .grid
            .set_cell(x, bottom, &horizontal.to_string(), Style::KeyHintBorder);
    }
    for y in top.saturating_add(1)..bottom {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::KeyHintBorder);
        frame.grid.set_cell(
            right.saturating_sub(1),
            y,
            &vertical.to_string(),
            Style::KeyHintBorder,
        );
    }
    frame
        .grid
        .set_cell(left, top, &top_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        top,
        &top_right.to_string(),
        Style::KeyHintBorder,
    );
    frame
        .grid
        .set_cell(left, bottom, &bottom_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        bottom,
        &bottom_right.to_string(),
        Style::KeyHintBorder,
    );
    let inner_left = left
        .saturating_add(1)
        .saturating_add(policy.horizontal_padding);
    let inner_right = right.saturating_sub(1 + policy.horizontal_padding);
    if inner_left >= inner_right {
        return;
    }
    frame.grid.put_text_clipped(
        inner_left,
        top.saturating_add(1 + policy.vertical_padding),
        &hints.prefix,
        Style::KeyHintPrefix,
        inner_right,
    );
    let body_top = top.saturating_add(2 + policy.vertical_padding);
    let gap_total = policy
        .column_gap
        .saturating_mul(u16::try_from(columns.saturating_sub(1)).unwrap_or(0));
    let column_width = inner_right
        .saturating_sub(inner_left)
        .saturating_sub(gap_total)
        / u16::try_from(columns).unwrap_or(1);
    if column_width < 3 {
        return;
    }
    for (index, row) in hints.rows.iter().take(row_count).enumerate() {
        let column = index / rows_per_column;
        let row_index = index % rows_per_column;
        let x = inner_left.saturating_add(
            u16::try_from(column)
                .unwrap_or(0)
                .saturating_mul(column_width.saturating_add(policy.column_gap)),
        );
        let y = body_top.saturating_add(
            u16::try_from(row_index.saturating_mul(row_stride)).unwrap_or(u16::MAX),
        );
        if y >= bottom {
            continue;
        }
        let column_right = x.saturating_add(column_width).min(inner_right);
        let mut cursor = frame.grid.put_text_clipped(
            x,
            y,
            &format!("[{}]", row.next_key),
            Style::KeyHintKey,
            column_right,
        );
        if let Some(icon) = &row.icon {
            cursor = frame.grid.put_text_clipped(
                cursor.saturating_add(1),
                y,
                icon,
                Style::KeyHintGroup,
                column_right,
            );
        }
        frame.grid.put_text_clipped(
            cursor.saturating_add(1),
            y,
            &row.label,
            if row.group {
                Style::KeyHintGroup
            } else {
                Style::KeyHintCommand
            },
            column_right,
        );
    }
}
