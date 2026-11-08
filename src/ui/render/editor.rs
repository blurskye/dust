//! Text buffers, selections, cursors and editor gutter drawing.

use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::grid::ZERO_WIDTH_REPLACEMENT;
use crate::ui::render::SCRATCH_LABEL;
use std::collections::BTreeMap;

use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::config::AmbiguousWidth;

use crate::config::Config;

use crate::config::CursorShape;

use crate::config::ExplorerIconMode;

use crate::config::LineNumbers;

use crate::editor::EditorMode;

use crate::editor::EditorState;

use crate::editor::SelectionKind;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::DisplayWidthPolicy;

use crate::editor::text::PositionCodec;

use crate::editor::text::byte_range_for_display_columns;

use crate::editor::text::display_column_at;

use crate::ui::CellGrid;

use crate::ui::Style;

use crate::ui::geometry::cell_text_width;

use crate::ui::geometry::text_width;

use crate::ui::render::DebugSignKind;

use crate::ui::grid::Frame;

use crate::ui::render::TerminalPanelMode;

use crate::ui::render::UiDiagnostic;

use crate::ui::render::UiGitSignKind;

use crate::ui::render::UiState;

use crate::ui::render::visual_rows::VisualRowAnchor;

use crate::ui::render::visual_rows::VisualRowModel;

use crate::ui::render::visual_rows::active_indent_guide;

use crate::ui::render::visual_rows::editor_line_number_digits;

use crate::ui::render::visual_rows::editor_state_content_width;

use crate::ui::render::visual_rows::editor_state_gutter_width;

use crate::ui::render::visual_rows::git_sign_column;

use crate::ui::render::visual_rows::is_indent_guide_column;

use crate::ui::render::visual_rows::leading_indentation_width;

use crate::ui::render::visual_rows::one_cell_symbol;

use crate::ui::render::visual_rows::projected_inlay_hints;

pub(crate) const END_OF_BUFFER_MARKER: &str = "~";
pub(crate) const LINE_NUMBER_SEPARATOR: &str = " ";
pub(crate) const GUTTER_PADDING: u16 = 1;
pub(crate) const GIT_SIGN_UNCHANGED: &str = "│";
// Heavier than the unchanged line beside it, so a change stands out as LazyVim's bar does.
pub(crate) const GIT_SIGN_ADDED: &str = "┃";
pub(crate) const GIT_SIGN_CHANGED: &str = "┃";
pub(crate) const GIT_SIGN_DELETED: &str = "_";
pub(crate) const GIT_SIGN_STAGED: &str = "┊";
pub(crate) const INDENT_GUIDE: &str = "│";
pub(crate) const LARGE_FILE_LABEL: &str = "LARGE";
pub(crate) const STATUS_SEPARATOR: &str = "  ";
pub(crate) const STATUS_MODE_PADDING: &str = " ";
pub(crate) const STATUS_DIRTY_MARKER: &str = " [+]";
pub(crate) const STATUS_LSP_FORMAT: &str = "{server}";
pub(crate) const STATUS_GIT_FORMAT: &str = "{branch}";
pub(crate) const STATUS_ROOT_FORMAT: &str = "{root}";
pub(crate) const REPLACE_CURSOR: CursorShape = CursorShape::Underline;
pub(crate) const VISUAL_CURSOR: CursorShape = CursorShape::Block;
pub(crate) const OPERATOR_PENDING_CURSOR: CursorShape = CursorShape::Block;
pub(crate) const COMMAND_CURSOR: CursorShape = CursorShape::Bar;
pub(crate) const PROMPT_CURSOR: CursorShape = CursorShape::Bar;

fn diagnostic_text_style(severity: crate::workspace::DiagnosticSeverity) -> Style {
    match severity {
        crate::workspace::DiagnosticSeverity::Error => Style::DiagnosticError,
        crate::workspace::DiagnosticSeverity::Warning => Style::DiagnosticWarning,
        crate::workspace::DiagnosticSeverity::Information
        | crate::workspace::DiagnosticSeverity::Hint => Style::DiagnosticHint,
    }
}

fn diagnostic_style(severity: crate::workspace::DiagnosticSeverity) -> Style {
    match severity {
        crate::workspace::DiagnosticSeverity::Error => Style::Error,
        crate::workspace::DiagnosticSeverity::Warning => Style::Warning,
        crate::workspace::DiagnosticSeverity::Information
        | crate::workspace::DiagnosticSeverity::Hint => Style::Muted,
    }
}

/// Guides keep their own colour over syntax (a multi-line string's indentation is part of the
/// string), but a selection or search match still shows through them.
fn indent_guide_style(style: Style, active: bool) -> Style {
    match style {
        Style::Selection | Style::SelectionSyntax(_) | Style::Search => style,
        _ if active => Style::Muted,
        _ => Style::Border,
    }
}

fn diagnostic_label(diagnostic: &UiDiagnostic) -> String {
    let message = diagnostic
        .message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let tag = diagnostic
        .code
        .as_deref()
        .or((!diagnostic.source.is_empty()).then_some(diagnostic.source.as_str()));
    tag.map_or(message.clone(), |tag| format!("{message} [{tag}]"))
}

pub(super) fn expanded_diagnostic_rows(
    text: &crate::editor::text::TextBuffer,
    buffer: crate::editor::BufferId,
    path: Option<&std::path::Path>,
    ui: &UiState,
    config: &Config,
    content_width: u16,
) -> Option<(usize, Vec<(String, Style)>)> {
    let expanded = ui.expanded_diagnostic.as_ref()?;
    if !expanded
        .buffer
        .map_or_else(|| path == Some(expanded.path.as_path()), |id| id == buffer)
    {
        return None;
    }
    let mut diagnostics = ui
        .diagnostics
        .iter()
        .filter(|d| {
            d.buffer
                .map_or_else(|| path == Some(d.path.as_path()), |id| id == buffer)
        })
        .take(2_000)
        .filter(|d| {
            text.byte_to_line(d.range.start).ok() == Some(expanded.line)
                && expanded.severities.contains(&d.severity)
        })
        .collect::<Vec<_>>();
    diagnostics.sort_unstable_by_key(|d| (d.severity, d.range.start, &d.message));
    if diagnostics.is_empty() {
        return None;
    }
    let grid = CellGrid::new_with_policy(
        0,
        0,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let mut rows = vec![("Diagnostics:".into(), Style::Muted)];
    for (index, diagnostic) in diagnostics.into_iter().enumerate() {
        let text = format!("{}. {}", index + 1, diagnostic_label(diagnostic));
        let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.extend(
            crate::ui::render::debug::wrap_rows(&lines, content_width, &grid)
                .into_iter()
                .map(|row| (row, diagnostic_style(diagnostic.severity))),
        );
    }
    Some((expanded.line, rows))
}

pub(super) fn render_editor_view(
    state: &EditorState,
    view_id: crate::editor::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    if let Some(panel) = ui
        .agent_panel
        .as_ref()
        .filter(|panel| panel.view == view_id)
    {
        let focused = state.active_view() == Some(view_id) && !ui.explorer_focused;
        // Alone on screen the panel keeps the status line under it, like any editor view.
        let status = config.ui.status_line.enabled && height > 1;
        let mut frame = crate::ui::render::agent::render_agent_panel(
            panel, focused, config, width, height, status,
        );
        if status {
            render_status_line(
                &mut frame.grid,
                state,
                view_id,
                ui,
                config,
                width,
                height - 1,
            );
        }
        return frame;
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let Some(view) = state.views.get(view_id) else {
        return Frame { grid, cursor: None };
    };
    let buffer = &state.buffers[view.buffer];
    let body_height = height
        .saturating_sub(u16::from(config.ui.status_line.enabled))
        .saturating_sub(u16::from(
            config.ui.status_line.enabled && ui.operation_bar.is_some() && height > 1,
        ));
    let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
    let digits = editor_line_number_digits(state);
    let gutter_width = editor_state_gutter_width(state, &buffer.text, &config.editor, width);
    let content_width = editor_state_content_width(state, &buffer.text, &config.editor, width);
    let neutral_git_sign = (config.editor.git_signs.enabled && gutter_width > 0)
        .then(|| one_cell_symbol(GIT_SIGN_UNCHANGED, "|", config.editor.ambiguous_width));
    if let (Some(symbol), Some(column)) = (
        neutral_git_sign,
        git_sign_column(gutter_width, &config.editor),
    ) {
        for row in 0..body_height {
            grid.set_cell(column, row, symbol, Style::Muted);
        }
    }
    let indent_guide_symbol = config
        .editor
        .indent_guides
        .enabled
        .then(|| one_cell_symbol(INDENT_GUIDE, "|", config.editor.ambiguous_width));
    let active_indent_scope = indent_guide_symbol.and_then(|_| {
        active_indent_guide(
            &buffer.text,
            view.cursor,
            current_line,
            config.editor.tab_width,
            config.editor.indent_width,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        )
    });
    let end_of_buffer_label = if config.editor.line_numbers == LineNumbers::Off {
        END_OF_BUFFER_MARKER.to_owned()
    } else {
        let marker_width = text_width(END_OF_BUFFER_MARKER, config.editor.ambiguous_width);
        format!(
            "{}{}{}",
            " ".repeat(digits.saturating_sub(marker_width)),
            END_OF_BUFFER_MARKER,
            LINE_NUMBER_SEPARATOR
        )
    };
    let large_file = ui.large_file_buffers.contains(&view.buffer);
    let wrap_enabled = content_width > 0 && config.editor.wrap && !large_file;
    let visual_rows = VisualRowModel::new_for_state(
        state,
        &buffer.text,
        view_id,
        ui,
        config,
        width,
        wrap_enabled,
    );
    let configured_start = visual_rows.normalize(VisualRowAnchor {
        line: view.scroll_line,
        wrap_row: view.scroll_wrap_row,
    });
    let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
    let start_anchor = if configured_start.line == cursor_anchor.line
        && visual_rows
            .screen_row(
                configured_start,
                cursor_anchor,
                usize::from(body_height.max(1)),
            )
            .is_none()
    {
        visual_rows.backward(cursor_anchor, usize::from(body_height.saturating_sub(1)))
    } else {
        configured_start
    };
    let flash = ui.flash.as_ref().filter(|flash| flash.view == view_id);
    let yanked = ui
        .yanked
        .as_ref()
        .filter(|yanked| yanked.view == view_id && yanked.revision == buffer.text.revision().0);
    // What is being typed in `/` or `:s` shows as you type, as with Vim's incsearch.
    let typed_substitute = ui
        .prompt
        .as_ref()
        .filter(|prompt| prompt.kind == crate::ui::PromptKind::Command)
        .and_then(|prompt| crate::editor::input::substitute_pattern(&prompt.input));
    let search_prompt = ui.prompt.as_ref().is_some_and(|prompt| {
        matches!(
            prompt.kind,
            crate::ui::PromptKind::SearchForward | crate::ui::PromptKind::SearchBackward
        )
    });
    let substitute_flags = ui
        .prompt
        .as_ref()
        .and_then(|prompt| crate::editor::input::typed_substitution(&prompt.input))
        .map(|(_, _, _, flags)| flags)
        .unwrap_or_default();
    let whole_word = ui.search_whole_word && !search_prompt && typed_substitute.is_none();
    let highlighted = match ui.prompt.as_ref() {
        Some(prompt)
            if matches!(
                prompt.kind,
                crate::ui::PromptKind::SearchForward | crate::ui::PromptKind::SearchBackward
            ) =>
        {
            Some(prompt.input.as_str())
        }
        _ => typed_substitute
            .as_deref()
            .filter(|pattern| !pattern.is_empty())
            .or(ui.search_query.as_deref()),
    };
    let search_ranges = if config.search.highlight && !large_file {
        highlighted
            .filter(|query| !query.is_empty())
            .map_or_else(Vec::new, |query| {
                // Only the lines on screen are searched, so a large file costs nothing extra.
                let last_line = visual_rows
                    .forward(start_anchor, usize::from(body_height))
                    .line
                    .saturating_add(1)
                    .min(buffer.text.len_lines());
                let first = buffer
                    .text
                    .line_to_byte(start_anchor.line)
                    .map_or(0, ByteOffset::get);
                let end = buffer
                    .text
                    .line_to_byte(last_line)
                    .map_or(buffer.text.len_bytes(), ByteOffset::get);
                let source = ByteRange::new(ByteOffset(first), ByteOffset(end.max(first)))
                    .ok()
                    .and_then(|range| buffer.text.slice(range).ok())
                    .unwrap_or_default();
                let matches = if typed_substitute.is_some() && !search_prompt {
                    crate::editor::input::substitution_regex(query, &substitute_flags)
                        .map(|regex| {
                            let mut offset = 0;
                            let mut matches = Vec::new();
                            for line in source.split_inclusive('\n') {
                                for found in regex.find_iter(line.trim_end_matches(['\n', '\r'])) {
                                    matches.push(offset + found.start()..offset + found.end());
                                    if !substitute_flags.contains('g') {
                                        break;
                                    }
                                }
                                offset += line.len();
                            }
                            matches
                        })
                        .unwrap_or_default()
                } else {
                    crate::editor::input::search_matches(
                        &source,
                        query,
                        config.search.smart_case,
                        false,
                    )
                    .unwrap_or_default()
                };
                matches
                    .into_iter()
                    .filter(|range| {
                        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
                        !whole_word
                            || source[..range.start]
                                .chars()
                                .next_back()
                                .is_none_or(|ch| !word(ch))
                                && source[range.end..]
                                    .chars()
                                    .next()
                                    .is_none_or(|ch| !word(ch))
                    })
                    .filter_map(|range| {
                        ByteRange::new(
                            ByteOffset(first + range.start),
                            ByteOffset(first + range.end),
                        )
                        .ok()
                    })
                    .collect()
            })
    } else {
        Vec::new()
    };
    let mut diagnostic_lines = BTreeMap::<usize, Vec<&UiDiagnostic>>::new();
    let mut diagnostic_ranges = BTreeMap::<usize, Vec<&UiDiagnostic>>::new();
    let mut visible_lines = Vec::new();
    let mut anchor = start_anchor;
    for _ in 0..body_height {
        if visible_lines.last() != Some(&anchor.line) {
            visible_lines.push(anchor.line);
        }
        let next = visual_rows.next(anchor);
        if next == anchor {
            break;
        }
        anchor = next;
    }
    if !large_file {
        for diagnostic in ui
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.buffer.map_or_else(
                    || buffer.path.as_ref() == Some(&diagnostic.path),
                    |id| id == view.buffer,
                )
            })
            .take(2_000)
        {
            if let Ok(line) = buffer.text.byte_to_line(diagnostic.range.start) {
                diagnostic_lines.entry(line).or_default().push(diagnostic);
                let mut end_line = buffer
                    .text
                    .byte_to_line(diagnostic.range.end)
                    .unwrap_or(line);
                if !diagnostic.range.is_empty()
                    && buffer.text.line_to_byte(end_line).ok() == Some(diagnostic.range.end)
                {
                    end_line = end_line.saturating_sub(1);
                }
                for &row in visible_lines
                    .iter()
                    .filter(|&&row| row >= line && row <= end_line)
                {
                    diagnostic_ranges.entry(row).or_default().push(diagnostic);
                }
            }
        }
        for diagnostics in diagnostic_lines
            .values_mut()
            .chain(diagnostic_ranges.values_mut())
        {
            diagnostics.sort_unstable_by_key(|diagnostic| {
                (
                    diagnostic.severity,
                    diagnostic.range.start,
                    &diagnostic.message,
                )
            });
        }
    }

    let mut syntax_spans = ui
        .syntax_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    syntax_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut syntax_cursor = 0_usize;
    let mut semantic_spans = ui
        .lsp_semantic_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    semantic_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut semantic_cursor = 0usize;
    let mut highlight_spans = ui
        .lsp_highlight_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    highlight_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut highlight_cursor = 0usize;
    // The tutor marks what its step is about, until the text changes.
    let tutor_target = ui
        .tutor
        .as_ref()
        .and_then(|bar| bar.target.as_ref())
        .filter(|target| target.buffer == view.buffer && target.revision == buffer.text.revision());
    let tutor_line = ui
        .tutor
        .as_ref()
        .and_then(|card| card.line)
        .filter(|(target, _)| *target == view.buffer)
        .map(|(_, line)| line);
    let inlay_hints = projected_inlay_hints(state, view_id, ui);
    let decorations = buffer.decorations.drawing().collect::<Vec<_>>();
    let mut inlay_cursor = 0usize;
    let color_spans = ui
        .lsp_colors
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    let mut color_cursor = 0usize;
    let width_policy = DisplayWidthPolicy {
        tab_width: config.editor.tab_width,
        ambiguous_wide: config.editor.ambiguous_width == AmbiguousWidth::Wide,
    };
    let selection_lines = view.selection.map(|selection| {
        let anchor = buffer.text.byte_to_line(selection.anchor).unwrap_or(0);
        let head = buffer.text.byte_to_line(selection.head).unwrap_or(0);
        anchor.min(head)..=anchor.max(head)
    });
    let block_columns = view
        .selection
        .filter(|selection| selection.kind == SelectionKind::Block)
        .and_then(|selection| {
            let column = |offset| {
                let line = buffer.text.byte_to_line(offset).ok()?;
                let start = buffer.text.line_to_byte(line).ok()?;
                let source = buffer.text.line(line).ok()?;
                Some(display_column_at(
                    source.trim_end_matches('\n'),
                    offset.get().saturating_sub(start.get()),
                    width_policy,
                ))
            };
            Some((column(selection.anchor)?, column(selection.head)?))
        });
    let mut screen_row = 0_u16;
    let mut line_index = start_anchor.line;
    // The empty line after a final newline is drawn only while the cursor is on it.
    let drawn_lines = buffer.text.visible_lines().max(current_line + 1);
    while screen_row < body_height && line_index < drawn_lines {
        if let Some(fold) = ui
            .lsp_folds
            .iter()
            .chain(ui.syntax_folds.iter())
            .find(|fold| {
                fold.view == view_id
                    && fold.collapsed
                    && line_index > fold.start_line
                    && line_index <= fold.end_line
            })
        {
            line_index = fold.end_line.saturating_add(1);
            continue;
        }
        let line_rows = visual_rows.line_rows(line_index);
        let first_line_row = if line_index == start_anchor.line {
            start_anchor.wrap_row.min(line_rows.len().saturating_sub(1))
        } else {
            0
        };
        if gutter_width > 0 && first_line_row == 0 {
            let number = match config.editor.line_numbers {
                LineNumbers::Absolute => line_index + 1,
                LineNumbers::Relative => current_line.abs_diff(line_index),
                LineNumbers::Hybrid => {
                    if line_index == current_line {
                        line_index + 1
                    } else {
                        current_line.abs_diff(line_index)
                    }
                }
                LineNumbers::Off => 0,
            };
            let number = if config.editor.line_numbers == LineNumbers::Relative
                && line_index == current_line
            {
                line_index + 1
            } else {
                number
            };
            // The tutor's line has an arrow in place of its number.
            let tutor_line = tutor_line == Some(line_index);
            let label = if tutor_line {
                format!("{:>digits$}{}", "▶", LINE_NUMBER_SEPARATOR)
            } else {
                format!("{number:>digits$}{}", LINE_NUMBER_SEPARATOR)
            };
            grid.put_text_clipped(
                0,
                screen_row,
                &label,
                if line_index == current_line || tutor_line {
                    Style::Accent
                } else {
                    Style::Muted
                },
                gutter_width,
            );
            // Beside the number, where it takes no room and hides no sign.
            let separator = u16::try_from(digits).unwrap_or(u16::MAX);
            if let Some(worst) = diagnostic_lines
                .get(&line_index)
                .and_then(|found| found.first())
                && !LINE_NUMBER_SEPARATOR.is_empty()
                && git_sign_column(gutter_width, &config.editor)
                    .is_none_or(|column| separator < column)
                && separator < gutter_width
            {
                grid.set_cell(separator, screen_row, "●", diagnostic_style(worst.severity));
            }
        }
        if let Some(git_sign_column) = git_sign_column(gutter_width, &config.editor)
            .or_else(|| {
                (gutter_width > 0
                    && ui
                        .debug_signs
                        .iter()
                        .any(|sign| buffer.path.as_deref() == Some(sign.path.as_path())))
                .then(|| gutter_width - 1)
            })
            .filter(|_| gutter_width > 0 && first_line_row == 0)
        {
            grid.set_cell(
                git_sign_column,
                screen_row,
                one_cell_symbol(GIT_SIGN_UNCHANGED, "|", config.editor.ambiguous_width),
                Style::Muted,
            );
            let debug_sign = ui.debug_signs.iter().find(|sign| {
                sign.line == line_index && buffer.path.as_deref() == Some(sign.path.as_path())
            });
            if let Some(sign) = debug_sign {
                let (symbol, fallback, style) = match sign.kind {
                    DebugSignKind::Breakpoint => ("●", "*", Style::Error),
                    DebugSignKind::ConditionalBreakpoint => ("◆", "?", Style::Error),
                    DebugSignKind::DisabledBreakpoint => ("○", "o", Style::Muted),
                    DebugSignKind::Execution => ("▶", ">", Style::Warning),
                };
                grid.set_cell(
                    git_sign_column,
                    screen_row,
                    one_cell_symbol(symbol, fallback, config.editor.ambiguous_width),
                    style,
                );
            } else if let Some(sign) = ui.git_signs.iter().find(|sign| {
                config.editor.git_signs.enabled
                    && sign.buffer == view.buffer
                    && sign.buffer_revision == buffer.text.revision().0
                    && sign.line == line_index
            }) {
                let (symbol, fallback, style) = match sign.kind {
                    UiGitSignKind::Added => (&GIT_SIGN_ADDED, "|", Style::GitSignAdded),
                    UiGitSignKind::Changed => (&GIT_SIGN_CHANGED, "|", Style::GitSignChanged),
                    UiGitSignKind::Deleted => (&GIT_SIGN_DELETED, "_", Style::GitSignDeleted),
                };
                let symbol = if sign.staged {
                    &GIT_SIGN_STAGED
                } else {
                    symbol
                };
                grid.set_cell(
                    git_sign_column,
                    screen_row,
                    one_cell_symbol(symbol, fallback, config.editor.ambiguous_width),
                    style,
                );
            }
        }
        if first_line_row == 0
            && gutter_width > 0
            && let Some(sign) = decorations.iter().find_map(|(range, decoration)| {
                (buffer.text.byte_to_line(range.start).ok() == Some(line_index))
                    .then_some(decoration.sign.as_ref())
                    .flatten()
            })
        {
            let column = git_sign_column(gutter_width, &config.editor).unwrap_or(gutter_width - 1);
            grid.set_cell(
                column,
                screen_row,
                &sign.text,
                crate::ui::render::decorations::style(&sign.style),
            );
        }
        let mut overlays = Vec::new();
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let full_content = line.strip_suffix('\n').unwrap_or(&line);
        // Resolve the selection once per source line, not once per painted grapheme.
        let selected_bytes = view.selection.and_then(|selection| match selection.kind {
            SelectionKind::Character => {
                let start = selection.anchor.min(selection.head);
                let mut end = selection.anchor.max(selection.head);
                if view.mode == EditorMode::Insert {
                    if end == start {
                        return None;
                    }
                    end = ByteOffset(end.get() - 1);
                }
                Some(start..=end)
            }
            SelectionKind::Line if selection_lines.as_ref()?.contains(&line_index) => {
                Some(line_start..=ByteOffset(line_start.get() + full_content.len()))
            }
            SelectionKind::Block if selection_lines.as_ref()?.contains(&line_index) => {
                let (anchor, head) = block_columns?;
                let (start, end) =
                    byte_range_for_display_columns(full_content, anchor, head, width_policy)?;
                Some(ByteOffset(line_start.get() + start)..=ByteOffset(line_start.get() + end - 1))
            }
            _ => None,
        });
        let markdown_line = visual_rows.markdown_line(line_index);
        let table = markdown_line
            .as_ref()
            .and_then(|markdown| markdown.table.as_ref());
        let leading_indentation = leading_indentation_width(full_content, config.editor.tab_width);
        let can_wrap = wrap_enabled;
        // A table row is placed whole: each grapheme goes where its cell wraps it. Tables fit
        // the window, so they don't scroll sideways with the rest of the text.
        let table_row = screen_row;
        if let Some(table) = table {
            for (row, frame) in table.frames.iter().enumerate().skip(first_line_row) {
                let y = table_row
                    .saturating_add(u16::try_from(row - first_line_row).unwrap_or(u16::MAX));
                if y >= body_height {
                    break;
                }
                grid.put_text_clipped(gutter_width, y, frame, table.style, width);
            }
        }
        let skipped_bytes = if table.is_some() {
            0
        } else if can_wrap {
            line_rows[first_line_row].byte_start
        } else {
            byte_offset_at_display_column(
                full_content,
                view.scroll_column,
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        };
        let skipped_column = if table.is_some() {
            0
        } else if can_wrap {
            line_rows[first_line_row].source_column_start
        } else {
            display_column(
                &full_content[..skipped_bytes],
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        };
        let visible_content = &full_content[skipped_bytes..];
        let visible_start = ByteOffset(line_start.get().saturating_add(skipped_bytes));
        let line_end = ByteOffset(
            line_start
                .get()
                .saturating_add(skipped_bytes)
                .saturating_add(visible_content.len()),
        );
        let mut rendered_inlays = 0usize;
        while inlay_hints
            .get(inlay_cursor)
            .is_some_and(|hint| hint.position < visible_start)
        {
            if inlay_hints[inlay_cursor].position >= line_start {
                rendered_inlays += 1;
            }
            inlay_cursor = inlay_cursor.saturating_add(1);
        }
        let mut x = gutter_width;
        let mut source_column = skipped_column;
        let mut visual_row_index = first_line_row;
        for (relative, grapheme) in visible_content.grapheme_indices(true) {
            let grapheme_column = source_column;
            let cells = if grapheme == "\t" {
                config.editor.tab_width - (source_column % config.editor.tab_width)
            } else {
                text_width(grapheme, config.editor.ambiguous_width).max(1)
            };
            source_column = source_column.saturating_add(cells);
            let source_relative = skipped_bytes.saturating_add(relative);
            if let Some(table) = table {
                let at = ByteOffset(line_start.get() + relative);
                let Some((row, column)) = table.place(at) else {
                    continue;
                };
                let Some(row) = row.checked_sub(first_line_row) else {
                    continue;
                };
                screen_row = table_row.saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
                if screen_row >= body_height {
                    continue;
                }
                x = gutter_width.saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
                if x >= width {
                    continue;
                }
            } else if can_wrap
                && line_rows
                    .get(visual_row_index.saturating_add(1))
                    .is_some_and(|row| source_relative >= row.byte_start)
            {
                screen_row = screen_row.saturating_add(1);
                if screen_row >= body_height {
                    break;
                }
                visual_row_index = visual_row_index.saturating_add(1);
                x = gutter_width;
            }
            let global = ByteOffset(line_start.get() + skipped_bytes + relative);
            let inlay_right = x
                .saturating_add(u16::try_from(visual_rows.inlay_width(global)).unwrap_or(u16::MAX))
                .min(width);
            while let Some(hint) = inlay_hints
                .get(inlay_cursor)
                .filter(|hint| hint.position <= global)
            {
                if rendered_inlays < 32 {
                    x = grid.put_text_clipped(x, screen_row, &hint.label, hint.style, inlay_right);
                    rendered_inlays = rendered_inlays.saturating_add(1);
                }
                inlay_cursor = inlay_cursor.saturating_add(1);
            }
            for (range, decoration) in &decorations {
                if global <= range.start
                    && range.start.get() < global.get() + grapheme.len()
                    && let Some(text) = &decoration.virtual_text
                    && text.position == crate::editor::decorations::VirtualTextPosition::Overlay
                {
                    overlays.push((x, screen_row, &text.text));
                }
            }
            if let Some(markdown) = &markdown_line {
                for (text, style) in markdown.before(global) {
                    x = grid.put_text_clipped(x, screen_row, text, style, width);
                }
                match markdown.at(global) {
                    Some(crate::ui::render::markdown::Drawn::Hidden) => continue,
                    Some(crate::ui::render::markdown::Drawn::Glyph(glyph, style)) => {
                        x = grid.put_text_clipped(x, screen_row, glyph, style, width);
                        continue;
                    }
                    None => {}
                }
            }
            while syntax_spans
                .get(syntax_cursor)
                .is_some_and(|span| span.range.end <= global)
            {
                syntax_cursor = syntax_cursor.saturating_add(1);
            }
            while semantic_spans
                .get(semantic_cursor)
                .is_some_and(|span| span.range.end <= global)
            {
                semantic_cursor = semantic_cursor.saturating_add(1);
            }
            let syntax_role = semantic_spans
                .get(semantic_cursor)
                .filter(|span| span.range.contains(global))
                .map(|span| span.role)
                .or_else(|| {
                    syntax_spans
                        .get(syntax_cursor)
                        .filter(|span| span.range.contains(global))
                        .map(|span| span.role)
                });
            let style = if selected_bytes
                .as_ref()
                .is_some_and(|range| range.contains(&global))
                || view
                    .multi_marks
                    .iter()
                    .any(|mark| mark.contains(global) || (mark.is_empty() && mark.start == global))
                || view.block_insert.contains(&global)
            {
                syntax_role.map_or(Style::Selection, Style::SelectionSyntax)
            } else if search_ranges.iter().any(|range| range.contains(global))
                || flash
                    .is_some_and(|flash| flash.matches.iter().any(|range| range.contains(global)))
                || yanked
                    .is_some_and(|yanked| yanked.ranges.iter().any(|range| range.contains(global)))
                || tutor_target.is_some_and(|target| target.range.contains(global))
            {
                Style::Search
            } else if let Some(style) = diagnostic_ranges.get(&line_index).and_then(|diagnostics| {
                diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        diagnostic.range.contains(global)
                            || (diagnostic.range.is_empty() && diagnostic.range.start == global)
                    })
                    .map(|diagnostic| diagnostic_text_style(diagnostic.severity))
                    .next()
            }) {
                style
            } else if let Some(style) = decorations.iter().find_map(|(range, decoration)| {
                range
                    .contains(global)
                    .then_some(decoration.highlight.as_deref())
                    .flatten()
            }) {
                crate::ui::render::decorations::style(style)
            } else if let Some(span) = {
                // Colours sort by start; a skipped span ends before this grapheme.
                while color_spans
                    .get(color_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    color_cursor = color_cursor.saturating_add(1);
                }
                color_spans
                    .get(color_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                crate::ui::render::documentation::color_swatch_style(span.rgb)
            } else if let Some(span) = {
                while highlight_spans
                    .get(highlight_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    highlight_cursor = highlight_cursor.saturating_add(1);
                }
                highlight_spans
                    .get(highlight_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                let _ = span;
                Style::Accent
            } else if let Some(role) = syntax_role {
                Style::Syntax(role)
            } else {
                Style::Default
            };
            let style = markdown_line
                .as_ref()
                .map_or(style, |markdown| markdown.restyle(global, style));
            let visible_cells = if table.is_some() && grapheme == "\t" {
                // Table layout treats a tab as one space within its column.
                1
            } else if can_wrap {
                cells.min(content_width)
            } else {
                cells
            };
            let advance = u16::try_from(visible_cells).unwrap_or(u16::MAX);
            if !can_wrap && x.saturating_add(advance) > width {
                break;
            }
            if grapheme == "\t" {
                for cell in 0..visible_cells {
                    if x >= width {
                        break;
                    }
                    let guide = indent_guide_symbol.filter(|_| {
                        is_indent_guide_column(
                            grapheme_column.saturating_add(cell),
                            leading_indentation,
                            config.editor.indent_width,
                        )
                    });
                    let active_guide = guide.is_some_and(|_| {
                        active_indent_scope.is_some_and(|scope| {
                            scope.contains(line_index, grapheme_column.saturating_add(cell))
                        })
                    });
                    grid.set_cell(
                        x,
                        screen_row,
                        guide.unwrap_or(" "),
                        if guide.is_some() {
                            indent_guide_style(style, active_guide)
                        } else {
                            style
                        },
                    );
                    x = x.saturating_add(1);
                }
            } else if x < width {
                if let Some(label) = flash.and_then(|flash| {
                    flash
                        .labels
                        .iter()
                        .find(|(at, _)| *at == global)
                        .map(|(_, label)| *label)
                }) {
                    let mut bytes = [0; 4];
                    grid.set_cell(
                        x,
                        screen_row,
                        label.encode_utf8(&mut bytes),
                        Style::StatusAccent,
                    );
                } else if can_wrap && cells > content_width {
                    let replacement =
                        if text_width(CONTROL_CHARACTER_REPLACEMENT, config.editor.ambiguous_width)
                            == 1
                        {
                            CONTROL_CHARACTER_REPLACEMENT
                        } else {
                            "?"
                        };
                    grid.set_cell(x, screen_row, replacement, style);
                } else if grapheme == " " {
                    if let Some(guide) = indent_guide_symbol.filter(|_| {
                        is_indent_guide_column(
                            grapheme_column,
                            leading_indentation,
                            config.editor.indent_width,
                        )
                    }) {
                        let active_guide = active_indent_scope
                            .is_some_and(|scope| scope.contains(line_index, grapheme_column));
                        grid.set_cell(
                            x,
                            screen_row,
                            guide,
                            indent_guide_style(style, active_guide),
                        );
                    } else {
                        grid.set_cell(x, screen_row, grapheme, style);
                    }
                } else {
                    grid.set_cell(x, screen_row, grapheme, style);
                }
                x = x.saturating_add(advance);
            }
        }
        if full_content.is_empty()
            && x < width
            && selected_bytes
                .as_ref()
                .is_some_and(|range| range.contains(&line_start))
        {
            grid.set_cell(x, screen_row, " ", Style::Selection);
        }
        if let Some(table) = table {
            let last = table.frames.len().saturating_sub(1);
            screen_row = table_row
                .saturating_add(
                    u16::try_from(last.saturating_sub(first_line_row)).unwrap_or(u16::MAX),
                )
                .min(body_height);
            let frame = table.frames.get(last).map_or("", String::as_str);
            x = gutter_width
                .saturating_add(
                    u16::try_from(text_width(frame, config.editor.ambiguous_width))
                        .unwrap_or(u16::MAX),
                )
                .min(width);
        }
        if let Some(markdown) = &markdown_line {
            if let Some((text, style)) = &markdown.after {
                x = grid.put_text_clipped(x, screen_row, text, *style, width);
            }
            if let Some(rule) = markdown.rule {
                while x < width {
                    let next = grid.put_text_clipped(x, screen_row, rule, Style::Muted, width);
                    if next == x {
                        break;
                    }
                    x = next;
                }
            }
            if let Some(fill) = markdown.fill {
                for column in x..width {
                    grid.set_cell(column, screen_row, " ", fill);
                }
            }
        }
        if let Some((start, end)) = visual_rows
            .containing_fold(line_index.saturating_add(1))
            .filter(|(start, _)| *start == line_index)
        {
            let hidden = end.saturating_sub(start);
            x = grid.put_text_clipped(
                x,
                screen_row,
                &format!(" ⋯ {hidden} lines"),
                Style::Muted,
                width,
            );
        }
        while let Some(hint) = inlay_hints
            .get(inlay_cursor)
            .filter(|hint| hint.position <= line_end)
        {
            if hint.position == line_end && rendered_inlays < 32 {
                x = grid.put_text_clipped(x, screen_row, &hint.label, hint.style, width);
                rendered_inlays = rendered_inlays.saturating_add(1);
            }
            inlay_cursor = inlay_cursor.saturating_add(1);
        }
        for (range, decoration) in &decorations {
            if let Some(text) = &decoration.virtual_text {
                match text.position {
                    crate::editor::decorations::VirtualTextPosition::EndOfLine
                        if buffer.text.byte_to_line(range.start).ok() == Some(line_index) =>
                    {
                        x = grid.put_text_clipped(
                            x,
                            screen_row,
                            &text.text.text,
                            crate::ui::render::decorations::style(&text.text.style),
                            width,
                        );
                    }
                    crate::editor::decorations::VirtualTextPosition::Overlay
                        if range.start == line_end =>
                    {
                        overlays.push((x, screen_row, &text.text));
                    }
                    _ => {}
                }
            }
        }
        for (column, row, text) in overlays {
            grid.put_text_clipped(
                column,
                row,
                &text.text,
                crate::ui::render::decorations::style(&text.style),
                width,
            );
        }
        if let Some(diagnostics) = diagnostic_lines.get(&line_index)
            && let Some(diagnostic) = diagnostics.first()
        {
            let text = format!("  ●● {}", diagnostic_label(diagnostic));
            grid.put_text_clipped(
                x,
                screen_row,
                &text,
                diagnostic_style(diagnostic.severity),
                width,
            );
        }
        screen_row = screen_row.saturating_add(1);
        for (row, style) in visual_rows.diagnostic_rows(line_index) {
            if screen_row >= body_height {
                break;
            }
            grid.put_text_clipped(gutter_width, screen_row, row, *style, width);
            screen_row = screen_row.saturating_add(1);
        }
        line_index = line_index.saturating_add(1);
    }
    while screen_row < body_height {
        grid.put_text_clipped(
            0,
            screen_row,
            &end_of_buffer_label,
            Style::Muted,
            if gutter_width > 0 {
                gutter_width
            } else {
                width
            },
        );
        if let (Some(symbol), Some(column)) = (
            neutral_git_sign,
            git_sign_column(gutter_width, &config.editor),
        ) {
            // The sign column is a viewport rule, not a property of only
            // materialized buffer lines. Keep it continuous through the
            // end-of-buffer region, including a one-cell gutter where the
            // marker and sign occupy the same cell.
            grid.set_cell(column, screen_row, symbol, Style::Muted);
        }
        screen_row = screen_row.saturating_add(1);
    }

    // The operation bar sits directly above the status line so a paused operation is visible
    // without displacing the editor, and it is drawn ONLY when an operation is genuinely paused.
    let bar_rows =
        u16::from(config.ui.status_line.enabled && ui.operation_bar.is_some() && height > 1);
    if bar_rows == 1 {
        render_operation_bar(&mut grid, ui, config, width, height - 2);
    }
    if config.ui.status_line.enabled && height > 0 {
        render_status_line(&mut grid, state, view_id, ui, config, width, height - 1);
    }

    let cursor = editor_cursor(EditorCursorContext {
        state,
        view_id,
        ui,
        config,
        width,
        body_height,
        gutter: gutter_width,
        wrap_enabled,
        visual_rows: &visual_rows,
        start_anchor,
    })
    .map(|(x, y)| {
        let configured = match view.mode {
            EditorMode::Insert => config.ui.cursor.insert,
            EditorMode::Replace => REPLACE_CURSOR,
            EditorMode::VisualChar
            | EditorMode::VisualLine
            | EditorMode::VisualBlock
            | EditorMode::Multi => VISUAL_CURSOR,
            EditorMode::OperatorPending => OPERATOR_PENDING_CURSOR,
            EditorMode::CommandLine => COMMAND_CURSOR,
            EditorMode::Prompt => PROMPT_CURSOR,
            EditorMode::Normal => config.ui.cursor.normal,
        };
        (x, y, cursor_style(configured))
    });
    Frame { grid, cursor }
}

/// Draw the operation bar. At narrow widths the blockers are dropped and then the phase is
/// shortened, because the operation and the cancellation are what must not be lost.
pub(super) fn render_operation_bar(
    grid: &mut CellGrid,
    ui: &UiState,
    config: &Config,
    width: u16,
    y: u16,
) {
    let Some(bar) = ui.operation_bar.as_ref() else {
        return;
    };
    grid.fill_row(y, Style::Status);
    let row = bar.row(width, config.editor.ambiguous_width);
    grid.put_text_clipped(0, y, &row, Style::Status, width);
}

/// One bar along the screen's foot, laid out like lualine: the mode and branch in blocks on the
/// left, the file after them, then the server and root, and the progress and position in blocks
/// on the right.
pub(super) fn render_status_line(
    grid: &mut CellGrid,
    state: &EditorState,
    view_id: crate::editor::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    status_y: u16,
) -> Vec<(String, std::ops::Range<u16>)> {
    let Some(view) = state.views.get(view_id) else {
        return Vec::new();
    };
    let Some(buffer) = state.buffers.get(view.buffer) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    grid.fill_row(status_y, Style::Status);
    let status = &config.ui.status_line;
    let default_order = crate::ui::render::chrome::ChromeLayout::default().status;
    let custom_order = ui.chrome.status != default_order;
    let components = if custom_order {
        &ui.chrome.status
    } else {
        &status.components
    };
    let shown = |component: &str| {
        status.components.iter().any(|item| item == component)
            && ui.chrome.status.iter().any(|item| item == component)
    };
    // The agent panel's input has its own Insert and Normal modes.
    let panel = ui
        .agent_panel
        .as_ref()
        .filter(|panel| panel.view == view_id);
    let agent = panel.is_some();
    // So does a focused terminal, whose typing mode is its own.
    let terminal = ui
        .terminal_panel
        .as_ref()
        .filter(|terminal| terminal.focused);
    let mode_key = match (terminal, panel) {
        (Some(terminal), _) => match terminal.mode {
            TerminalPanelMode::Input => "terminal",
            TerminalPanelMode::Normal => "normal",
            TerminalPanelMode::Visual => "visual_char",
        },
        (None, Some(panel)) if panel.normal => "normal",
        (None, Some(_)) => "insert",
        (None, None) => mode_name(view.mode),
    };
    let mode_label = status
        .mode_labels
        .get(mode_key)
        .expect("resolved configuration validates mode labels");
    let (mode_style, mode_edge) = status_mode_styles(mode_key);
    let icons = config.ui.explorer.icon_mode == ExplorerIconMode::NerdFont;
    let (end_edge, start_edge, branch_icon, error_icon, warning_icon) = if icons {
        (
            "\u{e0b0}",
            "\u{e0b2}",
            "\u{e0a0} ",
            "\u{f057} ",
            "\u{f071} ",
        )
    } else {
        (" ", " ", "", "E", "W")
    };
    let mode = if shown("mode") {
        format!("{STATUS_MODE_PADDING}{mode_label}{STATUS_MODE_PADDING}")
    } else {
        String::new()
    };
    let name = buffer
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || buffer.virtual_title().unwrap_or(SCRATCH_LABEL).to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    let file = if shown("file") {
        let icon = crate::files::file_icon(&name)
            .filter(|_| icons && buffer.path.is_some())
            .map_or_else(String::new, |icon| format!("{} ", icon.glyph));
        let dirty = if buffer.dirty {
            STATUS_DIRTY_MARKER
        } else {
            ""
        };
        format!("{icon}{name}{dirty}")
    } else {
        String::new()
    };
    let (errors, warnings) = buffer.path.as_ref().map_or((0, 0), |path| {
        ui.diagnostics
            .iter()
            .filter(|diagnostic| &diagnostic.path == path)
            .fold(
                (0usize, 0usize),
                |(errors, warnings), diagnostic| match diagnostic.severity {
                    crate::workspace::DiagnosticSeverity::Error => {
                        (errors.saturating_add(1), warnings)
                    }
                    crate::workspace::DiagnosticSeverity::Warning => {
                        (errors, warnings.saturating_add(1))
                    }
                    crate::workspace::DiagnosticSeverity::Information
                    | crate::workspace::DiagnosticSeverity::Hint => (errors, warnings),
                },
            )
    });
    let cursor_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
    // After the file, in the configured order. These go whole or not at all.
    let mut left_fields = Vec::new();
    for component in components {
        match component.as_str() {
            "breadcrumbs" => {
                let trail = ui
                    .breadcrumbs
                    .iter()
                    .filter(|(buffer, _)| *buffer == view.buffer)
                    .flat_map(|(_, symbols)| symbols)
                    .filter(|symbol| {
                        symbol.range.start <= view.cursor && view.cursor < symbol.range.end
                    })
                    .map(|symbol| symbol.name.as_str())
                    .collect::<Vec<_>>();
                if !trail.is_empty() {
                    let deepest = &trail[trail.len().saturating_sub(3)..];
                    left_fields.push((
                        component.as_str(),
                        format!("\u{203a} {}", deepest.join(" \u{203a} ")),
                        Style::Status,
                    ));
                }
            }
            "diagnostics" => {
                if errors > 0 {
                    left_fields.push((
                        component.as_str(),
                        format!("{error_icon}{errors}"),
                        Style::StatusError,
                    ));
                }
                if warnings > 0 {
                    left_fields.push((
                        component.as_str(),
                        format!("{warning_icon}{warnings}"),
                        Style::StatusWarning,
                    ));
                }
            }
            "tasks" if ui.running_tasks > 0 => left_fields.push((
                component.as_str(),
                format!("Tasks {}", ui.running_tasks),
                Style::Status,
            )),
            "large_file" if ui.large_file_buffers.contains(&view.buffer) => left_fields.push((
                component.as_str(),
                LARGE_FILE_LABEL.to_owned(),
                Style::StatusWarning,
            )),
            name => {
                if let Some((text, style)) = ui.chrome.status_text.get(name) {
                    left_fields.push((component.as_str(), text.clone(), *style));
                }
            }
        }
    }
    let branch = ui
        .git_branch
        .as_ref()
        .filter(|_| shown("git"))
        .map(|branch| {
            format!(
                " {branch_icon}{} ",
                STATUS_GIT_FORMAT.replace("{branch}", branch)
            )
        });
    // The operation's bar sits above this row whenever there is a row for it. Without one the
    // indicator is drawn FIRST, so a paused operation is not hidden by a user's component choices.
    let operation_indicator = ui
        .operation_bar
        .as_ref()
        .filter(|_| status_y < 2)
        .map(|bar| format!(" {} ", bar.indicator()))
        .unwrap_or_default();
    // Everything left of the file: the indicator, the mode, one blank section cell and both edges.
    let left_fixed = text_cells(&operation_indicator, config)
        .saturating_add(text_cells(&mode, config))
        .saturating_add(text_cells(&file, config))
        .saturating_add(4);
    // The position always shows; the rest of the right side goes first when the left needs room.
    let position = (!agent && shown("position")).then(|| {
        let text = format!(
            "{}:{}",
            cursor_line + 1,
            byte_column(&buffer.text, view.cursor) + 1
        );
        format!(" {text} ")
    });
    let mut right_cells = position
        .as_deref()
        .map_or(0, |text| text_cells(text, config).saturating_add(1));
    let branch = branch.filter(|text| {
        left_fixed
            .saturating_add(text_cells(text, config))
            .saturating_add(right_cells)
            <= width
    });
    let left_needed =
        left_fixed.saturating_add(branch.as_deref().map_or(0, |text| text_cells(text, config)));
    let mut fits = |cells: u16| {
        let fits = left_needed
            .saturating_add(right_cells)
            .saturating_add(cells)
            < width;
        if fits {
            right_cells = right_cells.saturating_add(cells);
        }
        fits
    };
    let separator = STATUS_SEPARATOR;
    let separator_cells = text_cells(separator, config);
    // What goes first when the room runs out: the server, then the progress, then the root.
    let root = shown("root")
        .then(|| STATUS_ROOT_FORMAT.replace("{root}", &workspace_title(state)))
        .filter(|text| fits(text_cells(text, config).saturating_add(separator_cells)));
    let progress = (!agent && shown("progress"))
        .then(|| {
            format!(
                " {} ",
                status_progress(cursor_line, buffer.text.visible_lines())
            )
        })
        .filter(|text| fits(text_cells(text, config).saturating_add(1)));
    let lsp = ui
        .lsp_server
        .as_ref()
        .filter(|_| shown("lsp"))
        .map(|server| STATUS_LSP_FORMAT.replace("{server}", server))
        .filter(|text| fits(text_cells(text, config).saturating_add(separator_cells)));

    let left_limit = width.saturating_sub(right_cells);
    let mut x = grid.put_text_clipped(
        0,
        status_y,
        &operation_indicator,
        Style::StatusAccent,
        left_limit,
    );
    let left_order: Vec<&str> = if custom_order {
        components.iter().map(String::as_str).collect()
    } else {
        ["mode", "git", "file"]
            .into_iter()
            .chain(
                components
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !matches!(*name, "mode" | "git" | "file")),
            )
            .collect()
    };
    for name in left_order {
        match name {
            "mode" => {
                x = grid.put_text_clipped(x, status_y, &mode, mode_style, left_limit);
                x = grid.put_text_clipped(x, status_y, end_edge, mode_edge, left_limit);
            }
            "git" => {
                x = grid.put_text_clipped(
                    x,
                    status_y,
                    branch.as_deref().unwrap_or(" "),
                    Style::StatusSection,
                    left_limit,
                );
                x = grid.put_text_clipped(
                    x,
                    status_y,
                    end_edge,
                    Style::StatusEdgeSection,
                    left_limit,
                );
            }
            "file" => {
                x = x.saturating_add(1);
                x = grid.put_text_clipped(x, status_y, &file, Style::Status, left_limit);
            }
            _ => {
                for (_, text, style) in left_fields
                    .iter()
                    .filter(|(segment, _, _)| *segment == name)
                {
                    let next = x.saturating_add(separator_cells);
                    // At least one blank before the right side, so the two never read as one word.
                    if next.saturating_add(text_cells(text, config)) >= left_limit {
                        continue;
                    }
                    x = grid.put_text_clipped(next, status_y, text, *style, left_limit);
                    if ui.chrome.status_text.contains_key(name) {
                        segments.push((name.to_owned(), next..x));
                    }
                }
            }
        }
    }

    let mut x = left_limit;
    let right_order: Vec<&str> = if custom_order {
        components.iter().map(String::as_str).collect()
    } else {
        components
            .iter()
            .map(String::as_str)
            .filter(|name| matches!(*name, "root" | "lsp"))
            .chain(["progress", "position"])
            .collect()
    };
    for name in right_order {
        match name {
            "root" | "lsp" => {
                if let Some(text) = if name == "root" { &root } else { &lsp } {
                    x = grid.put_text_clipped(x, status_y, text, Style::Status, width);
                    x = x.saturating_add(separator_cells);
                }
            }
            "progress" => {
                if let Some(text) = &progress {
                    x = grid.put_text_clipped(
                        x,
                        status_y,
                        start_edge,
                        Style::StatusEdgeSection,
                        width,
                    );
                    x = grid.put_text_clipped(x, status_y, text, Style::StatusSection, width);
                }
            }
            "position" => {
                if let Some(text) = &position {
                    x = grid.put_text_clipped(x, status_y, start_edge, mode_edge, width);
                    x = grid.put_text_clipped(x, status_y, text, mode_style, width);
                }
            }
            _ => {}
        }
    }
    segments
}

/// A mode's block on the status line and the edge that joins it to the next section.
fn status_mode_styles(mode: &str) -> (Style, Style) {
    match mode {
        "insert" | "terminal" => (Style::StatusInsert, Style::StatusEdgeInsert),
        "visual_char" | "visual_line" | "visual_block" | "multi" => {
            (Style::StatusVisual, Style::StatusEdgeVisual)
        }
        "replace" => (Style::StatusReplace, Style::StatusEdgeReplace),
        "command_line" | "prompt" => (Style::StatusCommand, Style::StatusEdgeCommand),
        _ => (Style::StatusAccent, Style::StatusEdgeNormal),
    }
}

/// Where the cursor is in the file, as lualine says it.
fn status_progress(line: usize, lines: usize) -> String {
    if line == 0 {
        "Top".to_owned()
    } else if line + 1 >= lines {
        "Bot".to_owned()
    } else {
        format!("{}%", (line + 1) * 100 / lines)
    }
}

fn text_cells(text: &str, config: &Config) -> u16 {
    u16::try_from(text_width(text, config.editor.ambiguous_width)).unwrap_or(u16::MAX)
}

/// The open folder's name; a linked worktree also names its repository.
pub(crate) fn workspace_title(state: &EditorState) -> String {
    let folder = state
        .workspace_root
        .file_name()
        .unwrap_or(state.workspace_root.as_os_str())
        .to_string_lossy();
    match &state.workspace_repository {
        Some(repository) => format!("{repository} › {folder}"),
        None => folder.into_owned(),
    }
}

struct EditorCursorContext<'a> {
    state: &'a EditorState,
    view_id: crate::editor::ViewId,
    ui: &'a UiState,
    config: &'a Config,
    width: u16,
    body_height: u16,
    gutter: u16,
    wrap_enabled: bool,
    visual_rows: &'a VisualRowModel<'a>,
    start_anchor: VisualRowAnchor,
}

fn editor_cursor(context: EditorCursorContext<'_>) -> Option<(u16, u16)> {
    let EditorCursorContext {
        state,
        view_id,
        ui,
        config,
        width,
        body_height,
        gutter,
        wrap_enabled,
        visual_rows,
        start_anchor,
    } = context;
    let view = state.views.get(view_id)?;
    let buffer = state.buffers.get(view.buffer)?;
    let point = PositionCodec::new(&buffer.text)
        .byte_to_point(view.cursor)
        .ok()?;
    let (cursor_anchor, cursor_cell) = visual_rows.cursor_projection(view.cursor);
    if wrap_enabled {
        let row = visual_rows.screen_row(start_anchor, cursor_anchor, usize::from(body_height))?;
        let y = u16::try_from(row).ok()?;
        let x = gutter.saturating_add(u16::try_from(cursor_cell).ok()?);
        (x < width && y < body_height).then_some((x, y))
    } else {
        let column = cursor_cell;
        let line = buffer.text.line(point.line).ok()?;
        let content = line.strip_suffix('\n').unwrap_or(&line);
        let snapped_scroll = snap_display_column(
            content,
            view.scroll_column,
            config.editor.tab_width,
            config.editor.ambiguous_width,
        );
        let skipped_bytes = byte_offset_at_display_column(
            content,
            view.scroll_column,
            config.editor.tab_width,
            config.editor.ambiguous_width,
        );
        let line_start = buffer
            .text
            .line_to_byte(point.line)
            .unwrap_or(ByteOffset(0));
        let visible_start = ByteOffset(line_start.get().saturating_add(skipped_bytes));
        let hints = projected_inlay_hints(state, view_id, ui);
        let virtual_cells = hints
            .into_iter()
            .filter(|hint| {
                hint.position >= line_start
                    && hint.position <= ByteOffset(line_start.get() + content.len())
            })
            .take(32)
            .filter(|hint| {
                !hint.ghost && hint.position >= visible_start && hint.position <= view.cursor
            })
            .map(|hint| cell_text_width(&hint.label, config.editor.ambiguous_width))
            .fold(0usize, usize::saturating_add);
        let visible_column = column
            .checked_sub(snapped_scroll)?
            .saturating_add(virtual_cells);
        let x = gutter.saturating_add(u16::try_from(visible_column).ok()?);
        let y = u16::try_from(visual_rows.screen_row(
            start_anchor,
            cursor_anchor,
            usize::from(body_height),
        )?)
        .ok()?;
        (x < width && y < body_height).then_some((x, y))
    }
}

#[must_use]
pub fn snap_display_column(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let byte = byte_offset_at_display_column(text, target, tab_width, ambiguous_width);
    display_column(&text[..byte], tab_width, ambiguous_width)
}

/// Snap forward to the first complete display boundary at or after `target`.
#[must_use]
pub fn snap_display_column_forward(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    for grapheme in text.graphemes(true) {
        if column >= target {
            return column;
        }
        column = column.saturating_add(if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            text_width(grapheme, ambiguous_width).max(1)
        });
    }
    column
}

pub(crate) fn byte_offset_at_display_column(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    let mut byte = 0_usize;
    for (relative, grapheme) in text.grapheme_indices(true) {
        let width = if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            text_width(grapheme, ambiguous_width).max(1)
        };
        if column.saturating_add(width) > target {
            return relative;
        }
        column = column.saturating_add(width);
        byte = relative.saturating_add(grapheme.len());
    }
    byte
}

pub(super) fn display_column(
    text: &str,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            column += tab_width - (column % tab_width);
        } else {
            column += text_width(grapheme, ambiguous_width).max(1);
        }
    }
    column
}

pub(super) fn byte_column(text: &crate::editor::text::TextBuffer, offset: ByteOffset) -> usize {
    PositionCodec::new(text)
        .byte_to_point(offset)
        .map_or(0, |point| point.byte_column)
}

pub(super) fn mode_name(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Normal => "normal",
        EditorMode::Insert => "insert",
        EditorMode::Replace => "replace",
        EditorMode::VisualChar => "visual_char",
        EditorMode::VisualLine => "visual_line",
        EditorMode::VisualBlock => "visual_block",
        EditorMode::Multi => "multi",
        EditorMode::OperatorPending => "operator_pending",
        EditorMode::CommandLine => "command_line",
        EditorMode::Prompt => "prompt",
    }
}

pub(super) fn cursor_style(shape: CursorShape) -> SetCursorStyle {
    match shape {
        CursorShape::Default => SetCursorStyle::DefaultUserShape,
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Bar => SetCursorStyle::SteadyBar,
        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
        CursorShape::BlinkingBlock => SetCursorStyle::BlinkingBlock,
        CursorShape::BlinkingBar => SetCursorStyle::BlinkingBar,
        CursorShape::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
    }
}
