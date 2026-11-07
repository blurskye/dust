//! Prompt text, confirmation buttons and completion drawing.

use crate::config::BorderStyle;
use crate::config::Config;
use crate::ui::CellGrid;
use crate::ui::Style;
use crate::ui::geometry::text_width;
use crate::ui::grid::Frame;
use crate::ui::render::COMPLETION_DETAIL_GAP;
use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::ui::render::PromptState;
use crate::ui::render::editor::PROMPT_CURSOR;
use crate::ui::render::editor::cursor_style;

use crate::ui::geometry::ellipsized;
use crate::ui::geometry::fitted_footer;

pub(crate) const PROMPT_INPUT_PADDING: u16 = 2;

pub(crate) const PROMPT_TITLE_MIN_WIDTH: u16 = 10;

pub(super) fn prompt_viewport<'a>(
    input: &'a str,
    cursor: usize,
    width: usize,
    grid: &CellGrid,
) -> (&'a str, usize) {
    let cursor = input
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(input.len()))
        .take_while(|at| *at <= cursor)
        .last()
        .unwrap_or(0);
    let before = &input[..cursor];
    let mut cells = 0;
    let mut start = cursor;
    for (at, grapheme) in before.grapheme_indices(true).rev() {
        let size = if grapheme == "\t" {
            1
        } else {
            grid.display_width(grapheme).clamp(1, 2)
        };
        if cells + size >= width {
            break;
        }
        cells += size;
        start = at;
    }
    (&input[start..], cells)
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
        .map(|_| crate::ui::render::confirmation_layout(width, height, config, prompt));
    let geometry = confirmation_layout.as_ref().map_or_else(
        || crate::ui::render::prompt_geometry_for_state(width, height, config, prompt),
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
        let (shown, caret) = prompt_viewport(
            &prompt.input,
            prompt.cursor,
            usize::from(right.saturating_sub(marker_end)),
            &frame.grid,
        );
        frame
            .grid
            .put_text_clipped(marker_end, geometry.top, shown, Style::Default, right);
        frame.cursor = Some((
            marker_end
                .saturating_add(u16::try_from(caret).unwrap_or(0))
                .min(right.saturating_sub(1)),
            geometry.top,
            cursor_style(PROMPT_CURSOR),
        ));
        return;
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        frame.grid.border_glyphs(BorderStyle::Single);
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
    if geometry.width > PROMPT_TITLE_MIN_WIDTH {
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
            // The legend starts two cells in, stops two short of the right edge and has a space
            // on each side.
            let room = right.saturating_sub(geometry.left).saturating_sub(6);
            let hint = fitted_footer(
                &confirmation.hint,
                usize::from(room),
                config.editor.ambiguous_width,
            );
            if !hint.is_empty() {
                frame.grid.put_text_clipped(
                    geometry.left.saturating_add(2),
                    bottom_y,
                    &format!(" {hint} "),
                    Style::Muted,
                    right.saturating_sub(2),
                );
            }
        }
        frame.cursor = None;
        return;
    }
    let input_x = geometry.left.saturating_add(PROMPT_INPUT_PADDING);
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
    let (shown, caret) = prompt_viewport(
        &prompt.input,
        prompt.cursor,
        usize::from(content_right.saturating_sub(marker_end)),
        &frame.grid,
    );
    frame
        .grid
        .put_text_clipped(marker_end, input_y, shown, Style::Default, content_right);
    if let Some(error) = &prompt.error {
        let error_y = geometry.top.saturating_add(3);
        // A long error wraps below the box, since its end usually says what to do.
        let padding = PROMPT_INPUT_PADDING;
        let inner = right
            .saturating_sub(geometry.left)
            .saturating_sub(padding.saturating_mul(2));
        let ambiguous = config.editor.ambiguous_width;
        let lines = crate::ui::render::wrap_plain_text(error, usize::from(inner).max(1), ambiguous);
        for (row, line) in (error_y..height).zip(lines) {
            for x in geometry.left..right {
                frame.grid.set_cell(x, row, " ", Style::Default);
            }
            frame.grid.put_text_clipped(
                geometry.left.saturating_add(padding),
                row,
                &line,
                Style::Error,
                right,
            );
        }
    } else if !prompt.completions.is_empty() {
        let completion_top = geometry.top.saturating_add(3);
        let available = height.saturating_sub(completion_top);
        let rows = u16::try_from(prompt.completions.len())
            .unwrap_or(u16::MAX)
            .min(COMPLETION_MAX_ROWS)
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
            let label_x = geometry.left.saturating_add(PROMPT_INPUT_PADDING);
            let detail_x = label_x
                .saturating_add(
                    u16::try_from(text_width(&item.label, config.editor.ambiguous_width))
                        .unwrap_or(0),
                )
                .saturating_add(COMPLETION_DETAIL_GAP);
            frame
                .grid
                .put_text_clipped(label_x, y, &item.label, style, right);
            let room = usize::from(right.saturating_sub(detail_x));
            let detail = ellipsized(&item.detail, room, config.editor.ambiguous_width);
            let detail_style = if style == Style::Selection {
                style
            } else {
                Style::Muted
            };
            frame
                .grid
                .put_text_clipped(detail_x, y, &detail, detail_style, right);
        }
    }
    let cursor_x = marker_end.saturating_add(u16::try_from(caret).unwrap_or(0));
    frame.cursor = Some((
        cursor_x.min(geometry.left + geometry.width.saturating_sub(2)),
        input_y,
        cursor_style(PROMPT_CURSOR),
    ));
}
use unicode_segmentation::UnicodeSegmentation;
