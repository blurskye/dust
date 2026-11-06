//! The agent panel: its session tabs on top (drawn like the buffer line), the conversation,
//! the subagents or the slash-command menu, the input box with what the agent is doing in
//! its top border, and a footer with where the session works and its model. It is drawn in
//! place of the pane that holds it, so the pane still moves, resizes and zooms like any
//! other window.

use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;

use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use std::ops::Range;

use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::config::AmbiguousWidth;

use crate::config::Config;

use crate::editor::ViewId;

use crate::ui::CellGrid;

use crate::ui::Style;

use crate::ui::geometry::text_width;

use crate::ui::markdown::Styled;

use crate::ui::render::ContentTabEntry;

use crate::ui::grid::Frame;

use crate::ui::render::overlays::render_content_tab_line;

/// The input box grows with what is typed up to this many rows, then scrolls.
const INPUT_ROW_LIMIT: usize = 8;
/// Rows of the slash-command menu and of the subagent list.
const LIST_ROW_LIMIT: usize = 8;

/// One session of the shown conversation's family: the main session and its subagents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentListRow {
    pub shown: bool,
    pub name: String,
    pub task: String,
    /// A spinner frame while it works, `✓` or `✗` once done.
    pub status: String,
}

/// What the session hands the renderer for the pane that shows `view`.
#[derive(Clone, Debug)]
pub struct AgentPanelUi {
    pub view: ViewId,
    /// The sessions, as buffer-line entries whose identity is the session's index.
    pub tabs: Vec<ContentTabEntry>,
    /// The conversation, wrapped to the pane's width less the side padding.
    pub transcript: Styled,
    /// Byte ranges of the transcript drawn as the user's messages.
    pub bands: Vec<Range<usize>>,
    /// Rows scrolled up from the end; 0 follows new output.
    pub scroll: usize,
    /// What goes with the next question.
    pub context: Vec<String>,
    /// The shown session's family, when it has subagents.
    pub family: Vec<AgentListRow>,
    /// Slash commands matching what is typed, by name and description, and the selected one.
    pub menu: Vec<(String, String)>,
    pub selected: usize,
    /// What the agent is doing, such as `⠋ Working`, shown in the input's top border.
    pub busy: String,
    /// Whether the input is in Vim's Normal mode.
    pub normal: bool,
    pub input: String,
    /// Byte offset of the cursor in `input`.
    pub cursor: usize,
    pub placeholder: String,
    /// Left of the footer: where the session works.
    pub status: String,
    /// Right of the footer: model, mode and thinking level.
    pub settings: String,
}

impl AgentPanelUi {
    fn list_height(&self, height: u16) -> u16 {
        let count = if self.menu.is_empty() {
            self.family.len()
        } else {
            self.menu.len()
        };
        u16::try_from(count.min(LIST_ROW_LIMIT))
            .unwrap_or(0)
            .min(height.saturating_sub(8) / 2)
    }

    fn transcript_height_for_input(&self, height: u16, input_rows: usize) -> usize {
        let chips = u16::from(!self.context.is_empty());
        let list = self.list_height(height);
        let room = usize::from(height.saturating_sub(6 + chips + list)).max(1);
        let shown = input_rows.min(INPUT_ROW_LIMIT).min(room);
        usize::from(height.saturating_sub(4 + chips + list)).saturating_sub(shown)
    }

    /// Rows available to the transcript inside a pane, excluding the global status line.
    #[must_use]
    pub fn transcript_height(&self, config: &Config, width: u16, height: u16) -> usize {
        if width < 8 || height < 6 {
            return 0;
        }
        let grid = CellGrid::new_with_policy(
            0,
            0,
            CONTROL_CHARACTER_REPLACEMENT,
            ZERO_WIDTH_REPLACEMENT,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        );
        let (rows, _) = wrap_input(&self.input, self.cursor, usize::from(width - 2), &grid);
        self.transcript_height_for_input(height, rows.len())
    }
}

/// Where the transcript's lines start, without the trailing empty ones outside the bands, so
/// the user's last message keeps its padding while the answer is awaited.
#[must_use]
pub fn transcript_lines(text: &str, bands: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        lines.push(start..start + line.len());
        start += line.len() + 1;
    }
    while lines
        .last()
        .is_some_and(|line| line.is_empty() && !bands.iter().any(|band| band.contains(&line.start)))
    {
        lines.pop();
    }
    lines
}

pub(super) fn render_agent_panel(
    panel: &AgentPanelUi,
    focused: bool,
    config: &Config,
    width: u16,
    height: u16,
    status_row: bool,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    // The last row is left for the status line the caller draws there.
    let height = height - u16::from(status_row);
    if width < 8 || height < 6 {
        return Frame { grid, cursor: None };
    }
    render_content_tab_line(&mut grid, &panel.tabs, config, 0);

    let inner = width - 2;
    let chips = u16::from(!panel.context.is_empty());
    let list = panel.list_height(height);
    let (rows, (cursor_row, cursor_column)) =
        wrap_input(&panel.input, panel.cursor, usize::from(inner), &grid);
    let area = panel.transcript_height_for_input(height, rows.len());
    let shown = usize::from(height - 4 - chips - list) - area;
    let first = cursor_row.saturating_sub(shown - 1).min(rows.len() - shown);
    let footer = height - 1;
    let bottom = footer - 1;
    let input_top = bottom - u16::try_from(shown).unwrap_or(1);
    let top = input_top - 1;
    let border = if focused {
        Style::Accent
    } else {
        Style::Border
    };
    let horizontal = if grid.display_width("─") == 1 {
        "─"
    } else {
        "-"
    };
    for column in 0..width {
        grid.set_cell(column, top, horizontal, border);
        grid.set_cell(column, bottom, horizontal, border);
    }
    if !panel.busy.is_empty() {
        let x = grid.put_text(2, top, " ", border);
        let x = grid.put_text_clipped(x, top, &panel.busy, Style::Accent, width - 2);
        grid.put_text(x, top, " ", border);
    }
    let list_top = top - list;
    if panel.menu.is_empty() && list > 0 {
        // When the family is longer than the room, the last row counts the rest, and the
        // conversation on screen stays listed.
        let room = usize::from(list);
        let hidden = panel.family.len().saturating_sub(room);
        let take = if hidden > 0 {
            room.saturating_sub(1)
        } else {
            room
        };
        let mut rows = panel.family.iter().take(take).collect::<Vec<_>>();
        if hidden > 0 {
            if take > 0
                && let Some(shown) = panel.family.iter().skip(take).find(|row| row.shown)
            {
                rows.pop();
                rows.push(shown);
            }
            let y = list_top + u16::try_from(take).unwrap_or(0);
            let more = panel.family.len() - rows.len();
            grid.put_text_clipped(3, y, &format!("+{more} more"), Style::Muted, width - 1);
        }
        for (offset, row) in rows.into_iter().enumerate() {
            let y = list_top + u16::try_from(offset).unwrap_or(0);
            let (mark, style) = if row.shown {
                ("● ", Style::Syntax(crate::config::Highlight::STRONG))
            } else {
                ("○ ", Style::Muted)
            };
            let x = grid.put_text(1, y, mark, style);
            let x = grid.put_text_clipped(x, y, &row.name, style, width - 1);
            let x =
                grid.put_text_clipped(x.saturating_add(2), y, &row.task, Style::Muted, width - 1);
            let status = match row.status.as_str() {
                "✓" => Style::GitSignAdded,
                "✗" => Style::Error,
                _ => Style::Accent,
            };
            grid.put_text_clipped(x.saturating_add(2), y, &row.status, status, width - 1);
        }
    } else {
        let shown = usize::from(list);
        let first = panel.selected.saturating_sub(shown.saturating_sub(1));
        for (offset, (name, description)) in panel.menu.iter().skip(first).take(shown).enumerate() {
            let y = list_top + u16::try_from(offset).unwrap_or(0);
            let selected = first + offset == panel.selected;
            if selected {
                grid.fill_row(y, Style::Selection);
            }
            let (name_style, text_style) = if selected {
                (Style::Selection, Style::Selection)
            } else {
                (Style::Accent, Style::Muted)
            };
            let x = grid.put_text_clipped(1, y, &format!("/{name}"), name_style, width - 1);
            let start = x.saturating_add(2).max(18);
            let room = usize::from((width - 1).saturating_sub(start));
            let description =
                crate::ui::geometry::ellipsized(description, room, config.editor.ambiguous_width);
            grid.put_text_clipped(start, y, &description, text_style, width - 1);
        }
    }
    if panel.input.is_empty() {
        grid.put_text_clipped(1, input_top, &panel.placeholder, Style::Muted, width - 1);
    }
    for (offset, row) in rows.iter().skip(first).take(shown).enumerate() {
        let y = input_top + u16::try_from(offset).unwrap_or(0);
        grid.put_text_clipped(1, y, row, Style::Default, width - 1);
    }
    if chips > 0 {
        let mut x = 1;
        for label in &panel.context {
            x = grid.put_text_clipped(
                x,
                list_top - 1,
                &format!("+ {label}"),
                Style::Accent,
                width - 1,
            );
            x = x.saturating_add(2);
        }
    }
    // The folder the session works in keeps at least its last name; the model text shortens first.
    let ambiguous = config.editor.ambiguous_width;
    let folder = panel.status.rsplit('/').next().unwrap_or_default();
    let status_need =
        text_width(&panel.status, ambiguous).min(text_width(folder, ambiguous) + 2) + 2;
    let settings = fitted_settings(
        &panel.settings,
        usize::from(width).saturating_sub(2 + status_need),
        ambiguous,
    );
    let settings_width = u16::try_from(text_width(&settings, ambiguous)).unwrap_or(width);
    let settings_x = width.saturating_sub(settings_width + 1);
    let status = left_ellipsized(
        &panel.status,
        usize::from(settings_x.saturating_sub(2)),
        ambiguous,
    );
    grid.put_text_clipped(
        1,
        footer,
        &status,
        Style::Muted,
        settings_x.saturating_sub(1),
    );
    grid.put_text_clipped(settings_x, footer, &settings, Style::Muted, width - 1);

    let lines = transcript_lines(&panel.transcript.text, &panel.bands);
    let hidden = lines.len().saturating_sub(area);
    let scroll = panel.scroll.min(hidden);
    for (offset, line) in lines.iter().skip(hidden - scroll).take(area).enumerate() {
        let y = 1 + u16::try_from(offset).unwrap_or(0);
        draw_line(&mut grid, y, &panel.transcript, &panel.bands, line.clone());
    }
    if area > 0 && scroll > 0 {
        let more = format!(" ↓ {scroll} more ");
        let more_width =
            u16::try_from(text_width(&more, config.editor.ambiguous_width)).unwrap_or(0);
        let y = u16::try_from(area).unwrap_or(1);
        grid.put_text(
            width.saturating_sub(more_width + 1),
            y,
            &more,
            Style::StatusAccent,
        );
    }

    let cursor = focused.then(|| {
        let row = u16::try_from(cursor_row - first).unwrap_or(0);
        let column = u16::try_from(cursor_column).unwrap_or(0).min(inner);
        let shape = if panel.normal {
            SetCursorStyle::SteadyBlock
        } else {
            SetCursorStyle::SteadyBar
        };
        (1 + column, input_top + row, shape)
    });
    Frame { grid, cursor }
}

/// "model · mode · thinking" in `room` columns: a long model name is shortened first, so the
/// mode and thinking level stay readable in a narrow panel.
/// `text` shortened from the left with `…`, so its end stays.
fn left_ellipsized(text: &str, room: usize, ambiguous: AmbiguousWidth) -> String {
    if text_width(text, ambiguous) <= room {
        return text.to_owned();
    }
    let ellipsis_width = text_width("…", ambiguous);
    if room < ellipsis_width {
        return String::new();
    }
    let mut tail = String::new();
    for grapheme in text.graphemes(true).rev() {
        let mut candidate = grapheme.to_owned();
        candidate.push_str(&tail);
        if text_width(&candidate, ambiguous) + ellipsis_width > room {
            break;
        }
        tail = candidate;
    }
    format!("…{tail}")
}

fn fitted_settings(settings: &str, room: usize, ambiguous: AmbiguousWidth) -> String {
    if text_width(settings, ambiguous) <= room {
        return settings.to_owned();
    }
    let separator_width = text_width(" · ", ambiguous);
    match settings.split_once(" · ") {
        Some((model, rest)) if text_width(rest, ambiguous) + separator_width + 6 <= room => {
            let model = crate::ui::geometry::ellipsized(
                model,
                room - text_width(rest, ambiguous) - separator_width,
                ambiguous,
            );
            format!("{model} · {rest}")
        }
        _ => crate::ui::geometry::ellipsized(settings, room, ambiguous),
    }
}

fn draw_line(
    grid: &mut CellGrid,
    y: u16,
    transcript: &Styled,
    bands: &[Range<usize>],
    line: Range<usize>,
) {
    let banded = bands.iter().any(|band| band.contains(&line.start));
    if banded {
        grid.fill_row(y, Style::Selection);
    }
    let spans = &transcript.spans;
    let mut span = spans.partition_point(|(range, _)| range.end <= line.start);
    let right = grid.width().saturating_sub(1);
    let mut x = 1;
    for (offset, grapheme) in transcript.text[line.clone()].grapheme_indices(true) {
        let at = line.start + offset;
        while span < spans.len() && spans[span].0.end <= at {
            span += 1;
        }
        let style = match spans.get(span) {
            _ if banded => Style::Selection,
            Some((range, role)) if range.start <= at => Style::Syntax(*role),
            _ => Style::Default,
        };
        x = grid.put_text_clipped(x, y, grapheme, style, right);
    }
}

/// The input split into rows of `width` cells, and the cursor's row and column.
fn wrap_input(
    input: &str,
    cursor: usize,
    width: usize,
    grid: &CellGrid,
) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut at = None;
    for (index, grapheme) in input.grapheme_indices(true) {
        if grapheme == "\n" {
            if (index..index + grapheme.len()).contains(&cursor) {
                at = Some((rows.len() - 1, column));
            }
            rows.push(String::new());
            column = 0;
            continue;
        }
        let safe = if grapheme == "\t" {
            " ".to_owned()
        } else {
            grid.confirmation_grapheme(grapheme)
        };
        let character_width = grid.display_width(&safe).clamp(1, 2);
        if column + character_width > width {
            rows.push(String::new());
            column = 0;
        }
        if (index..index + grapheme.len()).contains(&cursor) {
            at = Some((rows.len() - 1, column));
        }
        if let Some(row) = rows.last_mut() {
            row.push_str(if grapheme == "\t" { " " } else { &safe });
        }
        column += character_width;
    }
    let end = (rows.len() - 1, column);
    (rows, at.unwrap_or(end))
}

#[cfg(test)]
mod tests {
    use crate::ui::render::agent::*;

    fn panel() -> AgentPanelUi {
        AgentPanelUi {
            view: ViewId::default(),
            tabs: Vec::new(),
            transcript: Styled::default(),
            bands: Vec::new(),
            scroll: 0,
            context: Vec::new(),
            family: Vec::new(),
            menu: Vec::new(),
            selected: 0,
            busy: String::new(),
            normal: false,
            input: String::new(),
            cursor: 0,
            placeholder: String::new(),
            status: String::new(),
            settings: String::new(),
        }
    }

    #[test]
    fn input_borders_remain_one_cell_wide() {
        let mut config = Config::default();
        config.editor.ambiguous_width = AmbiguousWidth::Wide;
        let frame = render_agent_panel(&panel(), true, &config, 20, 12, false);
        for y in [8, 10] {
            assert_eq!(frame.grid.row_text(y), "-".repeat(20));
        }
    }

    #[test]
    fn scrolling_without_transcript_rows_preserves_the_tabs() {
        let mut panel = panel();
        panel.context.push("file.rs".into());
        panel.transcript.text = "first\nsecond\nthird".into();
        let before = render_agent_panel(&panel, true, &Config::default(), 30, 6, false);
        panel.scroll = 2;
        let after = render_agent_panel(&panel, true, &Config::default(), 30, 6, false);
        assert_eq!(before.grid.row_text(0), after.grid.row_text(0));
        assert!(!after.grid.row_text(0).contains("more"));
    }

    #[test]
    fn input_measurement_matches_grid_substitutions() {
        let grid = CellGrid::new_with_policy(20, 5, "�", "◌", true);
        assert_eq!(wrap_input("·x", 3, 10, &grid).1, (0, 3));
        assert_eq!(
            wrap_input("a\tx", 3, 2, &grid),
            (vec!["a ".into(), "x".into()], (1, 1))
        );
        assert_eq!(
            wrap_input("aaaaaaaa👩‍💻z", 19, 10, &grid).0,
            ["aaaaaaaa👩‍💻", "z"]
        );
    }

    #[test]
    fn input_wraps_and_the_cursor_follows() {
        assert_eq!(
            wrap_input("", 0, 4, &CellGrid::new(20, 10)),
            (vec![String::new()], (0, 0))
        );
        assert_eq!(
            wrap_input("abcdef", 6, 4, &CellGrid::new(20, 10)),
            (vec!["abcd".into(), "ef".into()], (1, 2))
        );
        assert_eq!(
            wrap_input("abcd", 4, 4, &CellGrid::new(20, 10)),
            (vec!["abcd".into()], (0, 4))
        );
        assert_eq!(
            wrap_input("ab\ncd", 2, 4, &CellGrid::new(20, 10)),
            (vec!["ab".into(), "cd".into()], (0, 2))
        );
        assert_eq!(
            wrap_input("ab\ncd", 3, 4, &CellGrid::new(20, 10)),
            (vec!["ab".into(), "cd".into()], (1, 0))
        );
    }

    #[test]
    fn input_cursor_inside_a_grapheme_uses_its_display_position() {
        let grid = CellGrid::new(20, 10);
        assert_eq!(wrap_input("e\u{301}x", 1, 10, &grid).1, (0, 0));
        assert_eq!(wrap_input("abe\u{301}x", 3, 2, &grid).1, (1, 0));
        assert_eq!(wrap_input("e\u{301}x", 3, 10, &grid).1, (0, 1));
    }

    #[test]
    fn a_narrow_footer_shortens_the_model_before_the_mode() {
        let settings = "Claude Opus 5.5 with a long name · Bypass permissions · high";
        let fitted = fitted_settings(settings, 44, AmbiguousWidth::Narrow);
        assert!(fitted.ends_with("· Bypass permissions · high"), "{fitted}");
        assert!(text_width(&fitted, AmbiguousWidth::Narrow) <= 44);
        assert_eq!(
            fitted_settings(settings, 80, AmbiguousWidth::Narrow),
            settings
        );
    }

    #[test]
    fn wide_footer_punctuation_fits_its_budget() {
        let ambiguous = AmbiguousWidth::Wide;
        let path = "~/dev/qa/fixtures/agents";
        let shown = left_ellipsized(path, 10, ambiguous);
        assert!(shown.ends_with("/agents"), "{shown}");
        assert_eq!(text_width(&shown, ambiguous), 10);
        let settings = "Long model name · Mode · high";
        for room in 0..60 {
            assert!(text_width(&left_ellipsized(path, room, ambiguous), ambiguous) <= room);
            let fitted = fitted_settings(settings, room, ambiguous);
            assert!(text_width(&fitted, ambiguous) <= room, "{room}: {fitted}");
        }
        assert!(fitted_settings(settings, 22, ambiguous).ends_with(" · Mode · high"));
    }

    #[test]
    fn a_long_folder_keeps_its_last_name() {
        let shown = left_ellipsized("~/dev/qa/fixtures/agents", 10, AmbiguousWidth::Narrow);
        assert_eq!(shown, "…es/agents");
        assert_eq!(
            left_ellipsized("~/agents", 10, AmbiguousWidth::Narrow),
            "~/agents"
        );
    }

    #[test]
    fn a_long_subagent_list_counts_the_rest_and_keeps_the_shown_one() {
        let row = |index: usize| AgentListRow {
            shown: index == 10,
            name: "claude".into(),
            task: format!("task {index}"),
            status: "✓".into(),
        };
        let panel = AgentPanelUi {
            family: (0..12).map(row).collect(),
            ..panel()
        };
        let frame = render_agent_panel(&panel, true, &Config::default(), 60, 40, false);
        let lines = (0..40)
            .map(|y| {
                (0..60)
                    .filter_map(|x| frame.grid.get(x, y).map(|cell| cell.symbol.to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let listed = lines.iter().filter(|line| line.contains("task ")).count();
        assert_eq!(listed, LIST_ROW_LIMIT - 1, "{lines:#?}");
        assert!(
            lines.iter().any(|line| line.contains("● claude  task 10")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("+5 more")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_small_panel_keeps_the_family_within_its_allocated_rows() {
        let panel = AgentPanelUi {
            family: (0..12)
                .map(|index| AgentListRow {
                    shown: index == 10,
                    name: "claude".into(),
                    task: format!("task {index}"),
                    status: "✓".into(),
                })
                .collect(),
            ..panel()
        };
        for height in [8, 9, 10, 11] {
            let frame = render_agent_panel(&panel, true, &Config::default(), 40, height, false);
            let top = height - 4;
            assert_eq!(frame.grid.row_text(top), "─".repeat(40));
            let lines = (0..height)
                .map(|y| frame.grid.row_text(y))
                .collect::<Vec<_>>();
            assert!(
                !lines.iter().any(|line| line.contains("claude")),
                "{lines:#?}"
            );
            if height >= 10 {
                assert!(
                    lines[usize::from(top - 1)].contains("+12 more"),
                    "{lines:#?}"
                );
            } else {
                assert!(
                    !lines.iter().any(|line| line.contains("more")),
                    "{lines:#?}"
                );
            }
        }
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn trailing_empty_lines_are_not_shown() {
        assert_eq!(transcript_lines("a\n\nb\n\n", &[]), [0..1, 2..2, 3..4]);
        assert_eq!(
            transcript_lines("a\n\nb\n\n", std::slice::from_ref(&(2..6))),
            [0..1, 2..2, 3..4, 5..5]
        );
    }
}
