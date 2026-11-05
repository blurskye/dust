//! The first-start setup: one question per page, its choices down the left and a picture of
//! the highlighted one on the right, in the Settings page's float.

use crate::config::Config;
use crate::ui::{Style, geometry::text_width};

use super::{
    Frame,
    settings::{SetupState, draw_box, fill, settings_layout},
    surfaces::ellipsized,
};

/// The list of choices is at least this wide beside the preview, and at most the second.
const LIST_WIDTH: (u16, u16) = (40, 60);

pub(super) fn render_setup(
    frame: &mut Frame,
    setup: &SetupState,
    status: Option<&(String, bool)>,
    config: &Config,
    width: u16,
    height: u16,
) {
    let ambiguous = config.editor.ambiguous_width;
    let Some(layout) = settings_layout(width, height) else {
        for y in 0..height {
            fill(frame, 0, y, width, Style::Default);
        }
        frame.grid.put_text_clipped(
            0,
            height / 2,
            "Setup: make the window larger, or Esc to skip",
            Style::Accent,
            width,
        );
        return;
    };
    let Some(page) = setup.pages.get(setup.page) else {
        return;
    };
    let picked = setup.picked.get(setup.page).copied().unwrap_or(0);
    let (left, top) = (layout.left, layout.top);
    let right = left + layout.width;
    let bottom = top + layout.height;
    draw_box(frame, left, top, layout.width, layout.height);
    frame
        .grid
        .put_text(left + 2, top, " Welcome to Dust ", Style::Accent);

    // Where the person is: every page's title, the current one highlighted.
    let mut x = left + 2;
    for (index, other) in setup.pages.iter().enumerate() {
        let style = match index.cmp(&setup.page) {
            std::cmp::Ordering::Equal => Style::Accent,
            std::cmp::Ordering::Less => Style::Default,
            std::cmp::Ordering::Greater => Style::Muted,
        };
        if index > 0 {
            x = frame
                .grid
                .put_text_clipped(x, top + 1, " › ", Style::Muted, right - 2);
        }
        x = frame
            .grid
            .put_text_clipped(x, top + 1, &other.title, style, right - 2);
    }
    frame.grid.put_text_clipped(
        left + 2,
        top + 3,
        &page.question,
        Style::Syntax(crate::language::syntax::Highlight::HEADING),
        right - 2,
    );

    let preview_width = page
        .choices
        .iter()
        .flat_map(|choice| choice.preview.lines())
        .map(|line| u16::try_from(text_width(line, ambiguous)).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    let list_x = left + 2;
    let room = layout.width.saturating_sub(4);
    // The preview goes beside the list when both fit, and is left out otherwise.
    let show_preview = preview_width > 0 && room >= LIST_WIDTH.0 + 3 + preview_width;
    let list_right = if show_preview {
        list_x + (room - 3 - preview_width).min(LIST_WIDTH.1)
    } else {
        right - 2
    };
    let list_width = usize::from(list_right - list_x);
    let mut y = top + 5;
    for (index, choice) in page.choices.iter().enumerate() {
        if y + 1 >= bottom - 2 {
            break;
        }
        let selected = index == picked;
        if selected {
            fill(frame, list_x, y, list_right - list_x, Style::Selection);
            fill(frame, list_x, y + 1, list_right - list_x, Style::Selection);
            frame.grid.set_cell(list_x, y, "▌", Style::Accent);
            frame.grid.set_cell(list_x, y + 1, "▌", Style::Accent);
        }
        let label = ellipsized(&choice.label, list_width.saturating_sub(2), ambiguous);
        frame.grid.put_text(
            list_x + 2,
            y,
            &label,
            if selected {
                Style::Selection
            } else {
                Style::Default
            },
        );
        let about = ellipsized(&choice.about, list_width.saturating_sub(2), ambiguous);
        frame.grid.put_text(
            list_x + 2,
            y + 1,
            &about,
            if selected {
                Style::Selection
            } else {
                Style::Muted
            },
        );
        y += 3;
    }
    if show_preview && let Some(choice) = page.choices.get(picked) {
        let preview_x = list_right + 3 + (right - 2 - list_right - 3 - preview_width) / 2;
        for (offset, line) in choice.preview.lines().enumerate() {
            let line_y = top + 5 + u16::try_from(offset).unwrap_or(u16::MAX);
            if line_y >= bottom - 2 {
                break;
            }
            frame
                .grid
                .put_text_clipped(preview_x, line_y, line, Style::Default, right - 2);
        }
    }

    let last = setup.page + 1 == setup.pages.len();
    let help = format!(
        "j/k choose · enter {} · backspace back · esc skip",
        if last { "finish" } else { "next" }
    );
    let footer_y = bottom - 2;
    frame
        .grid
        .put_text_clipped(left + 2, footer_y, &help, Style::Muted, right - 2);
    if let Some((text, failed)) = status {
        let style = if *failed { Style::Error } else { Style::Accent };
        let text = ellipsized(text, usize::from(layout.width.saturating_sub(6)), ambiguous);
        let width = u16::try_from(text_width(&text, ambiguous)).unwrap_or(u16::MAX);
        frame
            .grid
            .put_text(right.saturating_sub(2 + width), footer_y, &text, style);
    }
}
