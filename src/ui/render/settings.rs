//! The Settings page: a float with the sections down the left and every setting with its value
//! on the right, laid out like VS Code's settings editor.

use crate::config::{AmbiguousWidth, Config};
use crate::language::syntax::Highlight;
use crate::ui::render::editor::PROMPT_CURSOR;
use crate::ui::{Style, geometry::text_width};

use super::{Frame, editor::cursor_style, surfaces::ellipsized};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettingsPageState {
    pub sections: Vec<String>,
    /// Every setting the page knows, in catalog order.
    pub rows: Vec<SettingsRow>,
    /// What the search leaves on screen, with a heading before each section.
    pub lines: Vec<SettingsLine>,
    /// Index into `lines`; always a setting while there is one.
    pub selected: usize,
    /// First screen row of the list that is shown.
    pub scroll: usize,
    pub query: String,
    /// Keys type into the search box.
    pub searching: bool,
    pub edit: Option<SettingsEdit>,
    /// A saved change is waiting for configuration reload.
    pub saving: bool,
    /// What happened to the last change, and whether it failed.
    pub status: Option<(String, bool)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsLine {
    Heading(usize),
    Setting(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsRow {
    pub key: String,
    pub section: usize,
    pub title: String,
    pub about: String,
    pub value: SettingsValue,
    /// Set in the user's config.toml.
    pub modified: bool,
    pub restart: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsValue {
    Bool(bool),
    Choice {
        current: String,
        choices: Vec<String>,
    },
    Number(String),
    Text(String),
    /// Strings edited as one comma-separated line.
    List(String),
    /// The colour theme, chosen in the theme picker with a live preview.
    Theme(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsEdit {
    /// `cursor` is a byte offset into `text`.
    Text {
        text: String,
        cursor: usize,
        error: Option<String>,
    },
    Choice {
        selected: usize,
    },
}

impl SettingsPageState {
    #[must_use]
    pub fn selected_row(&self) -> Option<&SettingsRow> {
        match self.lines.get(self.selected)? {
            SettingsLine::Setting(row) => self.rows.get(*row),
            SettingsLine::Heading(_) => None,
        }
    }
}

/// Screen rows the lines in `lines` take: a heading and a blank row, or a setting's title, its
/// description wrapped to the list and a blank row.
#[must_use]
pub fn settings_lines_height(
    page: &SettingsPageState,
    lines: std::ops::Range<usize>,
    layout: SettingsLayout,
    ambiguous: AmbiguousWidth,
) -> usize {
    lines
        .map(|index| match page.lines.get(index) {
            Some(SettingsLine::Heading(_)) => 2,
            Some(SettingsLine::Setting(_)) => 2 + about_lines(page, index, layout, ambiguous).len(),
            None => 0,
        })
        .sum()
}

/// The description under the setting at `index`, wrapped to the list: the error while its
/// value is being edited, and a note when the change applies after a restart.
fn about_lines(
    page: &SettingsPageState,
    index: usize,
    layout: SettingsLayout,
    ambiguous: AmbiguousWidth,
) -> Vec<String> {
    let Some(SettingsLine::Setting(row)) = page.lines.get(index) else {
        return Vec::new();
    };
    let Some(row) = page.rows.get(*row) else {
        return Vec::new();
    };
    let error = match (index == page.selected, &page.edit) {
        (
            true,
            Some(SettingsEdit::Text {
                error: Some(error), ..
            }),
        ) => Some(error),
        _ => None,
    };
    let mut about = error.cloned().unwrap_or_else(|| row.about.clone());
    if error.is_none() && row.restart {
        about.push_str(" Applies after a restart.");
    }
    let width = usize::from(layout.list_width.saturating_sub(3)).max(1);
    super::wrap_card_text(&about, width, ambiguous)
        .into_iter()
        .map(|line| line.into_iter().map(|(piece, _)| piece).collect())
        .collect()
}

/// Where the page's parts go on a screen of this size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingsLayout {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// Width of the section list, when the page is wide enough for one.
    pub sections: Option<u16>,
    pub list_x: u16,
    pub list_width: u16,
    pub list_top: u16,
    pub list_height: u16,
}

#[must_use]
pub fn settings_layout(width: u16, height: u16) -> Option<SettingsLayout> {
    let page_width = (u32::from(width) * 9 / 10)
        .try_into()
        .unwrap_or(width)
        .clamp(width.min(40), 150)
        .min(width);
    let page_height = (u32::from(height) * 9 / 10)
        .try_into()
        .unwrap_or(height)
        .max(height.min(12))
        .min(height);
    if page_width < 30 || page_height < 10 {
        return None;
    }
    let left = (width - page_width) / 2;
    let top = (height - page_height) / 2;
    let sections = (page_width >= 86).then_some(24);
    let list_x = left + 2 + sections.map_or(0, |width| width + 2);
    Some(SettingsLayout {
        left,
        top,
        width: page_width,
        height: page_height,
        sections,
        list_x,
        list_width: (left + page_width).saturating_sub(2).saturating_sub(list_x),
        list_top: top + 3,
        list_height: page_height - 5,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsHit {
    Section(usize),
    Line(usize),
    Search,
    Choice(usize),
    Border,
}

fn choice_geometry(
    page: &SettingsPageState,
    layout: SettingsLayout,
    ambiguous: AmbiguousWidth,
) -> Option<(crate::ui::window::WindowRect, usize)> {
    let SettingsEdit::Choice { selected } = page.edit.as_ref()? else {
        return None;
    };
    let SettingsValue::Choice { choices, .. } = &page.selected_row()?.value else {
        return None;
    };
    let offset = settings_lines_height(page, 0..page.selected, layout, ambiguous);
    let y = layout
        .list_top
        .checked_add(u16::try_from(offset.checked_sub(page.scroll)?).ok()?)?;
    let bottom = layout.list_top + layout.list_height;
    if y >= bottom {
        return None;
    }
    let widest = choices
        .iter()
        .map(|choice| width_of(choice, AmbiguousWidth::Wide))
        .max()
        .unwrap_or(0);
    let width = widest.saturating_add(6).min(layout.list_width);
    let below = bottom.saturating_sub(y + 1);
    let above = y.saturating_sub(layout.list_top);
    let wanted = u16::try_from(choices.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let (top, height) = if wanted <= below || below >= above {
        (y + 1, wanted.min(below))
    } else {
        let height = wanted.min(above);
        (y - height, height)
    };
    if height < 3 {
        return None;
    }
    let first = (*selected + 1).saturating_sub(usize::from(height - 2));
    Some((
        crate::ui::window::WindowRect {
            x: (layout.list_x + layout.list_width).saturating_sub(width + 1),
            y: top,
            width,
            height,
        },
        first,
    ))
}

/// What a click at this cell lands on.
#[must_use]
pub fn settings_hit(
    page: &SettingsPageState,
    (width, height): (u16, u16),
    ambiguous: AmbiguousWidth,
    column: u16,
    row: u16,
) -> Option<SettingsHit> {
    let layout = settings_layout(width, height)?;
    if let Some((rect, first)) = choice_geometry(page, layout, ambiguous)
        && column >= rect.x
        && column < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
    {
        return Some(
            if row == rect.y
                || row + 1 == rect.y + rect.height
                || column == rect.x
                || column + 1 == rect.x + rect.width
            {
                SettingsHit::Border
            } else {
                SettingsHit::Choice(first + usize::from(row - rect.y - 1))
            },
        );
    }
    if row == layout.top + 1 && column > layout.left && column < layout.left + layout.width {
        return Some(SettingsHit::Search);
    }
    if row < layout.list_top || row >= layout.list_top + layout.list_height {
        return None;
    }
    let offset = usize::from(row - layout.list_top);
    if let Some(sections) = layout.sections
        && column > layout.left
        && column <= layout.left + sections
    {
        return (offset < page.sections.len()).then_some(SettingsHit::Section(offset));
    }
    if column < layout.list_x || column >= layout.list_x + layout.list_width {
        return None;
    }
    let target = page.scroll + offset;
    let mut start = 0;
    for (index, line) in page.lines.iter().enumerate() {
        let end = start + settings_lines_height(page, index..index + 1, layout, ambiguous);
        if (start..end).contains(&target) {
            // The blank row after a setting belongs to no one.
            return (matches!(line, SettingsLine::Setting(_)) && target + 1 < end)
                .then_some(SettingsHit::Line(index));
        }
        start = end;
    }
    None
}

fn control_text(value: &SettingsValue) -> (String, Style) {
    match value {
        SettingsValue::Bool(true) => ("[✓] On".into(), Style::Accent),
        SettingsValue::Bool(false) => ("[ ] Off".into(), Style::Muted),
        SettingsValue::Choice { current, .. } | SettingsValue::Theme(current) => {
            (format!(" {current}  ▾ "), Style::Status)
        }
        SettingsValue::Number(text) | SettingsValue::Text(text) | SettingsValue::List(text) => {
            (format!(" {text} "), Style::Status)
        }
    }
}

fn fill(frame: &mut Frame, x: u16, y: u16, width: u16, style: Style) {
    for column in x..x.saturating_add(width) {
        frame.grid.set_cell(column, y, " ", style);
    }
}

fn draw_box(frame: &mut Frame, x: u16, y: u16, width: u16, height: u16) {
    if width < 2 || height < 2 {
        return;
    }
    for row in y..y + height {
        fill(frame, x, row, width, Style::Default);
        frame.grid.set_cell(x, row, "│", Style::Border);
        frame.grid.set_cell(x + width - 1, row, "│", Style::Border);
    }
    for column in x..x + width {
        frame.grid.set_cell(column, y, "─", Style::Border);
        frame
            .grid
            .set_cell(column, y + height - 1, "─", Style::Border);
    }
    frame.grid.set_cell(x, y, "╭", Style::Border);
    frame.grid.set_cell(x + width - 1, y, "╮", Style::Border);
    frame.grid.set_cell(x, y + height - 1, "╰", Style::Border);
    frame
        .grid
        .set_cell(x + width - 1, y + height - 1, "╯", Style::Border);
}

fn width_of(text: &str, ambiguous: AmbiguousWidth) -> u16 {
    u16::try_from(text_width(text, ambiguous)).unwrap_or(u16::MAX)
}

pub(super) fn render_settings(
    frame: &mut Frame,
    page: &SettingsPageState,
    config: &Config,
    width: u16,
    height: u16,
) {
    let Some(layout) = settings_layout(width, height) else {
        return;
    };
    // The page covers the editor, so its cursor would show through; search and text fields set their own.
    frame.cursor = None;
    let ambiguous = config.editor.ambiguous_width;
    let right = layout.left + layout.width;
    draw_box(frame, layout.left, layout.top, layout.width, layout.height);
    frame
        .grid
        .put_text(layout.left + 2, layout.top, " Settings ", Style::Accent);
    let mut keys: Vec<&str> = page
        .rows
        .iter()
        .filter(|row| row.modified)
        .map(|row| row.key.as_str())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    if !keys.is_empty() {
        let count = format!(" {} modified ", keys.len());
        let x = right.saturating_sub(2 + width_of(&count, ambiguous));
        frame.grid.put_text(x, layout.top, &count, Style::Muted);
    }

    // The search box.
    let search_y = layout.top + 1;
    let search_x = layout.left + 2;
    let search_width = layout.width.saturating_sub(4);
    fill(frame, search_x, search_y, search_width, Style::Status);
    // Commonly Used repeats settings from other sections; count each once.
    let mut shown: Vec<&str> = page
        .lines
        .iter()
        .filter_map(|line| match line {
            SettingsLine::Setting(row) => page.rows.get(*row).map(|row| row.key.as_str()),
            SettingsLine::Heading(_) => None,
        })
        .collect();
    shown.sort_unstable();
    shown.dedup();
    let shown = shown.len();
    let count = format!("{shown} setting{} ", if shown == 1 { "" } else { "s" });
    let count_x = (search_x + search_width).saturating_sub(width_of(&count, ambiguous));
    frame
        .grid
        .put_text(count_x, search_y, &count, Style::Status);
    if page.query.is_empty() && !page.searching {
        frame.grid.put_text_clipped(
            search_x + 1,
            search_y,
            "Search settings  (press /)",
            Style::Status,
            count_x,
        );
    } else {
        let end = frame.grid.put_text_clipped(
            search_x + 1,
            search_y,
            &page.query,
            Style::Status,
            count_x,
        );
        if page.searching {
            frame.cursor = Some((end.min(count_x), search_y, cursor_style(PROMPT_CURSOR)));
        }
    }

    // The sections.
    let current_section = page.selected_row().map(|row| row.section);
    if let Some(sections) = layout.sections {
        let divider = layout.left + 1 + sections;
        for y in layout.list_top..layout.list_top + layout.list_height {
            frame.grid.set_cell(divider, y, "│", Style::Border);
        }
        for (index, title) in page.sections.iter().enumerate() {
            let Some(y) = u16::try_from(index)
                .ok()
                .map(|index| layout.list_top + index)
                .filter(|y| *y < layout.list_top + layout.list_height)
            else {
                break;
            };
            let present = page.lines.contains(&SettingsLine::Heading(index));
            let style = if current_section == Some(index) {
                fill(frame, layout.left + 1, y, sections, Style::Selection);
                frame.grid.set_cell(layout.left + 1, y, "▌", Style::Accent);
                Style::Selection
            } else if present {
                Style::Default
            } else {
                Style::Muted
            };
            let text = ellipsized(title, usize::from(sections.saturating_sub(3)), ambiguous);
            frame.grid.put_text(layout.left + 3, y, &text, style);
        }
    }

    // The settings.
    let list_right = layout.list_x + layout.list_width;
    let bottom = layout.list_top + layout.list_height;
    let mut selected_y = None;
    let mut start = 0usize;
    for (index, line) in page.lines.iter().enumerate() {
        let line_start = start;
        start += settings_lines_height(page, index..index + 1, layout, ambiguous);
        if start <= page.scroll {
            continue;
        }
        let row_y = |offset: usize| {
            (line_start + offset)
                .checked_sub(page.scroll)
                .and_then(|row| u16::try_from(row).ok())
                .map(|row| layout.list_top + row)
                .filter(|y| *y < bottom)
        };
        if row_y(0).is_none() && line_start >= page.scroll {
            break;
        }
        match *line {
            SettingsLine::Heading(section) => {
                if let Some(y) = row_y(0) {
                    let title = page.sections.get(section).map_or("", String::as_str);
                    let end = frame.grid.put_text_clipped(
                        layout.list_x,
                        y,
                        title,
                        Style::Syntax(Highlight::HEADING),
                        list_right,
                    );
                    for x in end + 1..list_right {
                        frame.grid.set_cell(x, y, "─", Style::Border);
                    }
                }
            }
            SettingsLine::Setting(row_index) => {
                let Some(row) = page.rows.get(row_index) else {
                    continue;
                };
                let selected = index == page.selected;
                let base = if selected {
                    Style::Selection
                } else {
                    Style::Default
                };
                let about = about_lines(page, index, layout, ambiguous);
                for offset in 0..1 + about.len() {
                    if let (Some(y), true) = (row_y(offset), selected) {
                        fill(frame, layout.list_x, y, layout.list_width, Style::Selection);
                    }
                    if let (Some(y), true) = (row_y(offset), row.modified || selected) {
                        frame.grid.set_cell(
                            layout.list_x,
                            y,
                            if selected { "▌" } else { "▎" },
                            Style::Accent,
                        );
                    }
                }
                if selected {
                    selected_y = row_y(0);
                }
                let editing = selected.then_some(page.edit.as_ref()).flatten();
                if let Some(y) = row_y(0) {
                    let control_limit = (layout.list_width / 2).clamp(8, 44);
                    let control_x = match editing {
                        Some(SettingsEdit::Text { text, cursor, .. }) => {
                            let field = control_limit
                                .max(16)
                                .min(layout.list_width.saturating_sub(4));
                            let x = list_right.saturating_sub(field + 1);
                            fill(frame, x, y, field, Style::Status);
                            let inner = usize::from(field.saturating_sub(2));
                            let before = &text[..(*cursor).min(text.len())];
                            // Keep the cursor in the field by dropping text from the left.
                            let mut skip = 0;
                            while text_width(&before[skip..], ambiguous) > inner {
                                skip += before[skip..].chars().next().map_or(1, char::len_utf8);
                            }
                            frame.grid.put_text_clipped(
                                x + 1,
                                y,
                                &text[skip..],
                                Style::Status,
                                x + field - 1,
                            );
                            let cursor_x = x + 1 + width_of(&before[skip..], ambiguous);
                            frame.cursor = Some((cursor_x, y, cursor_style(PROMPT_CURSOR)));
                            x
                        }
                        _ => {
                            let (text, style) = control_text(&row.value);
                            let text = ellipsized(&text, usize::from(control_limit), ambiguous);
                            let x = list_right.saturating_sub(width_of(&text, ambiguous) + 1);
                            let style = match (selected, style) {
                                (true, Style::Muted) => Style::Selection,
                                (_, style) => style,
                            };
                            frame.grid.put_text(x, y, &text, style);
                            x
                        }
                    };
                    let title_style = if selected {
                        base
                    } else {
                        Style::Syntax(Highlight::STRONG)
                    };
                    let title = ellipsized(
                        &row.title,
                        usize::from(control_x.saturating_sub(layout.list_x + 4)),
                        ambiguous,
                    );
                    frame
                        .grid
                        .put_text(layout.list_x + 2, y, &title, title_style);
                }
                let error = matches!(editing, Some(SettingsEdit::Text { error: Some(_), .. }));
                let style = match (error, selected) {
                    (true, _) => Style::Error,
                    (false, true) => Style::Selection,
                    (false, false) => Style::Muted,
                };
                for (offset, text) in about.iter().enumerate() {
                    if let Some(y) = row_y(1 + offset) {
                        frame.grid.put_text(layout.list_x + 2, y, text, style);
                    }
                }
            }
        }
    }
    if !page
        .lines
        .iter()
        .any(|line| matches!(line, SettingsLine::Setting(_)))
    {
        let text = if page.query.is_empty() {
            "No settings.".to_owned()
        } else {
            format!("No settings match \"{}\".", page.query)
        };
        frame.grid.put_text_clipped(
            layout.list_x + 2,
            layout.list_top + 1,
            &text,
            Style::Muted,
            list_right,
        );
    }

    // A dropdown under the selected setting while a choice is open.
    if let (Some(SettingsEdit::Choice { selected }), Some(_y), Some(row)) =
        (&page.edit, selected_y, page.selected_row())
    {
        let (current, choices) = match &row.value {
            SettingsValue::Choice { current, choices } => (current.as_str(), choices.as_slice()),
            _ => ("", &[][..]),
        };
        if let Some((rect, first)) = choice_geometry(page, layout, ambiguous) {
            let (box_x, box_top, box_width, box_height) = (rect.x, rect.y, rect.width, rect.height);
            draw_box(frame, box_x, box_top, box_width, box_height);
            let visible = usize::from(box_height - 2);
            for (offset, (index, choice)) in choices
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
                .enumerate()
            {
                let y = box_top + 1 + u16::try_from(offset).unwrap_or(0);
                let style = if index == *selected {
                    fill(frame, box_x + 1, y, box_width - 2, Style::Selection);
                    Style::Selection
                } else {
                    Style::Default
                };
                let mark = if choice == current { "✓" } else { " " };
                frame.grid.put_text(
                    box_x + 2,
                    y,
                    mark,
                    if index == *selected {
                        style
                    } else {
                        Style::Accent
                    },
                );
                frame
                    .grid
                    .put_text_clipped(box_x + 4, y, choice, style, box_x + box_width - 1);
            }
        }
    }

    // Keys, and how the last change went.
    let footer_y = layout.top + layout.height - 2;
    // Each hint with its rank; a narrow page drops the highest ranks first, keeping hints whole.
    let hints: &[(&str, u8)] = match (&page.edit, page.searching) {
        (Some(SettingsEdit::Text { .. }), _) => &[("Enter save", 0), ("Esc cancel", 0)],
        (Some(SettingsEdit::Choice { .. }), _) => {
            &[("↑↓ choose", 1), ("Enter select", 0), ("Esc cancel", 0)]
        }
        (None, true) => &[
            ("Type to filter", 1),
            ("↑↓ move", 2),
            ("Enter done", 0),
            ("Esc clear", 0),
        ],
        (None, false) => &[
            ("↑↓ move", 4),
            ("Tab section", 3),
            ("Space toggle", 2),
            ("Enter change", 0),
            ("r reset", 2),
            ("/ search", 1),
            ("m modified", 1),
            ("o config.toml", 1),
            ("Esc close", 0),
        ],
    };
    let status_width = page
        .status
        .as_ref()
        .map_or(0, |(text, _)| width_of(text, ambiguous) + 4);
    let help_right = right.saturating_sub(2 + status_width);
    let room = help_right.saturating_sub(layout.left + 2);
    let mut help = String::new();
    for limit in (0..=4).rev() {
        help = hints
            .iter()
            .filter(|(_, rank)| *rank <= limit)
            .map(|(hint, _)| *hint)
            .collect::<Vec<_>>()
            .join(" · ");
        if width_of(&help, ambiguous) <= room {
            break;
        }
    }
    let help = ellipsized(&help, usize::from(room), ambiguous);
    frame
        .grid
        .put_text(layout.left + 2, footer_y, &help, Style::Muted);
    if let Some((text, failed)) = &page.status {
        let style = if *failed { Style::Error } else { Style::Accent };
        let text = ellipsized(text, usize::from(layout.width.saturating_sub(6)), ambiguous);
        let x = right.saturating_sub(2 + width_of(&text, ambiguous));
        frame.grid.put_text(x, footer_y, &text, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_hits_precede_the_settings_underneath() {
        let page = SettingsPageState {
            rows: vec![SettingsRow {
                key: "editor.line_numbers".into(),
                section: 0,
                title: "Numbers".into(),
                about: String::new(),
                value: SettingsValue::Choice {
                    current: "absolute".into(),
                    choices: vec!["off".into(), "absolute".into(), "hybrid".into()],
                },
                modified: false,
                restart: false,
            }],
            lines: vec![SettingsLine::Setting(0)],
            edit: Some(SettingsEdit::Choice { selected: 0 }),
            ..Default::default()
        };
        let layout = settings_layout(100, 30).unwrap();
        let ambiguous = AmbiguousWidth::Narrow;
        let (rect, first) = choice_geometry(&page, layout, ambiguous).unwrap();
        assert_eq!(first, 0);
        assert_eq!(
            settings_hit(&page, (100, 30), ambiguous, rect.x + 2, rect.y + 3),
            Some(SettingsHit::Choice(2))
        );
        assert_eq!(
            settings_hit(&page, (100, 30), ambiguous, rect.x, rect.y + 1),
            Some(SettingsHit::Border)
        );
    }

    #[test]
    fn a_long_description_wraps_and_pushes_the_next_setting_down() {
        let row = |title: &str, about: &str| SettingsRow {
            key: title.into(),
            section: 0,
            title: title.into(),
            about: about.into(),
            value: SettingsValue::Bool(true),
            modified: false,
            restart: false,
        };
        let about = "acp runs the agent inside Dust's agent panel; terminal runs the agent's own \
                     command-line program in a terminal on the side.";
        let page = SettingsPageState {
            rows: vec![row("Connection", about), row("Next", "Short.")],
            lines: vec![SettingsLine::Setting(0), SettingsLine::Setting(1)],
            ..Default::default()
        };
        let layout = settings_layout(60, 30).unwrap();
        let ambiguous = AmbiguousWidth::Narrow;
        let lines = about_lines(&page, 0, layout, ambiguous);
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|line| width_of(line, ambiguous) <= layout.list_width - 3)
        );
        assert_eq!(lines.join(" "), about);
        let first = settings_lines_height(&page, 0..1, layout, ambiguous);
        assert_eq!(first, 2 + lines.len());
        let next = layout.list_top + u16::try_from(first).unwrap();
        assert_eq!(
            settings_hit(&page, (60, 30), ambiguous, layout.list_x + 2, next),
            Some(SettingsHit::Line(1))
        );
        assert_eq!(
            settings_hit(&page, (60, 30), ambiguous, layout.list_x + 2, next - 2),
            Some(SettingsHit::Line(0))
        );
    }
}
