use std::collections::BTreeMap;

use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::{AmbiguousWidth, Config, CursorShape, LineNumbers},
    editor::{
        EditorMode, EditorState, SelectionKind,
        text::{
            ByteOffset, ByteRange, DisplayWidthPolicy, PositionCodec,
            byte_range_for_display_columns, display_column_at,
        },
    },
    ui::{CellGrid, Style, geometry::text_width},
};

use super::{
    DebugSignKind, Frame, UiDiagnostic, UiGitSignKind, UiState,
    visual_rows::{
        VisualRowAnchor, VisualRowModel, active_indent_guide, editor_line_number_digits,
        editor_state_content_width, editor_state_gutter_width, git_sign_column,
        is_indent_guide_column, leading_indentation_width, one_cell_symbol, syntax_role_style,
    },
};

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
        Style::Selection | Style::Search => style,
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

pub(super) fn render_editor_view(
    state: &EditorState,
    view_id: crate::editor::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    if let Some(panel) = ui.agent_panel.as_ref().filter(|panel| panel.view == view_id) {
        let focused = state.active_view == Some(view_id) && !ui.explorer_focused;
        // Alone on screen the panel keeps the status line under it, like any editor view.
        let status = config.ui.status_line.enabled && height > 1;
        let mut frame = super::agent::render_agent_panel(panel, focused, config, width, height, status);
        if status {
            render_status_line(&mut frame.grid, state, view_id, ui, config, width, height - 1);
        }
        return frame;
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
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
    let neutral_git_sign = (config.editor.git_signs.enabled && gutter_width > 0).then(|| {
        one_cell_symbol(
            &config.editor.git_signs.unchanged,
            "|",
            config.editor.ambiguous_width,
        )
    });
    if let (Some(symbol), Some(column)) = (
        neutral_git_sign,
        git_sign_column(gutter_width, &config.editor),
    ) {
        for row in 0..body_height {
            grid.set_cell(column, row, symbol, Style::Muted);
        }
    }
    let indent_guide_symbol = config.editor.indent_guides.enabled.then(|| {
        one_cell_symbol(
            &config.editor.indent_guides.character,
            "|",
            config.editor.ambiguous_width,
        )
    });
    let active_indent_scope = config
        .editor
        .indent_guides
        .active_scope
        .then_some(())
        .and(indent_guide_symbol)
        .and_then(|_| {
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
        config.editor.end_of_buffer_marker.clone()
    } else {
        let marker_width = text_width(
            &config.editor.end_of_buffer_marker,
            config.editor.ambiguous_width,
        );
        format!(
            "{}{}{}",
            " ".repeat(digits.saturating_sub(marker_width)),
            config.editor.end_of_buffer_marker,
            config.editor.line_number_separator
        )
    };
    let large_file = ui.large_file_buffers.contains(&view.buffer);
    let wrap_enabled = content_width > 0
        && config.editor.wrap
        && !(large_file && config.editor.large_file.disable_wrap);
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
    // What is being typed in `/` or `:s` shows as you type, as with Vim's incsearch.
    let typed_substitute = ui
        .prompt
        .as_ref()
        .filter(|prompt| prompt.kind == crate::ui::PromptKind::Command)
        .and_then(|prompt| crate::editor::input::substitute_pattern(&prompt.input));
    let highlighted = match ui.prompt.as_ref() {
        Some(prompt)
            if matches!(
                prompt.kind,
                crate::ui::PromptKind::SearchForward | crate::ui::PromptKind::SearchBackward
            ) =>
        {
            Some(prompt.input.as_str())
        }
        _ => typed_substitute.as_deref().or(ui.search_query.as_deref()),
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
                crate::editor::input::search_matches(
                    &source,
                    query,
                    config.search.smart_case,
                    config.search.regex,
                )
                .unwrap_or_default()
                .into_iter()
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
    if !(large_file && config.editor.large_file.disable_diagnostics) {
        if let Some(path) = &buffer.path {
            for diagnostic in ui
                .diagnostics
                .iter()
                .filter(|diagnostic| &diagnostic.path == path)
                .take(2_000)
            {
                if let Ok(line) = buffer.text.byte_to_line(diagnostic.range.start) {
                    diagnostic_lines.entry(line).or_default().push(diagnostic);
                }
            }
            for diagnostics in diagnostic_lines.values_mut() {
                diagnostics.sort_unstable_by_key(|diagnostic| {
                    (
                        diagnostic.severity,
                        diagnostic.range.start,
                        &diagnostic.message,
                    )
                });
            }
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
    let mut inlay_hints = ui
        .lsp_inlay_hints
        .iter()
        .chain(&ui.substitute_preview)
        .filter(|hint| hint.buffer == view.buffer && hint.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    inlay_hints.sort_unstable_by_key(|hint| hint.position);
    let mut inlay_cursor = 0usize;
    let color_spans = ui
        .lsp_colors
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    let mut color_cursor = 0usize;
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
            let label = format!("{number:>digits$}{}", config.editor.line_number_separator);
            grid.put_text_clipped(
                0,
                screen_row,
                &label,
                if line_index == current_line {
                    Style::Accent
                } else {
                    Style::Muted
                },
                gutter_width,
            );
        }
        if let Some(git_sign_column) =
            git_sign_column(gutter_width, &config.editor).filter(|_| first_line_row == 0)
        {
            grid.set_cell(
                git_sign_column,
                screen_row,
                one_cell_symbol(
                    &config.editor.git_signs.unchanged,
                    "|",
                    config.editor.ambiguous_width,
                ),
                Style::Muted,
            );
            let debug_sign = ui.debug_signs.iter().find(|sign| {
                sign.line == line_index && buffer.path.as_deref() == Some(sign.path.as_path())
            });
            if let Some(sign) = debug_sign {
                let signs = &config.editor.debug_signs;
                let (symbol, fallback, style) = match sign.kind {
                    DebugSignKind::Breakpoint => (&signs.breakpoint, "*", Style::Error),
                    DebugSignKind::ConditionalBreakpoint => {
                        (&signs.conditional_breakpoint, "?", Style::Error)
                    }
                    DebugSignKind::Execution => (&signs.execution, ">", Style::Warning),
                };
                grid.set_cell(
                    git_sign_column,
                    screen_row,
                    one_cell_symbol(symbol, fallback, config.editor.ambiguous_width),
                    style,
                );
            } else if let Some(sign) = ui.git_signs.iter().find(|sign| {
                sign.buffer == view.buffer
                    && sign.buffer_revision == buffer.text.revision().0
                    && sign.line == line_index
            }) {
                let (symbol, fallback, style) = match sign.kind {
                    UiGitSignKind::Added => {
                        (&config.editor.git_signs.added, "|", Style::GitSignAdded)
                    }
                    UiGitSignKind::Changed => {
                        (&config.editor.git_signs.changed, "|", Style::GitSignChanged)
                    }
                    UiGitSignKind::Deleted => {
                        (&config.editor.git_signs.deleted, "_", Style::GitSignDeleted)
                    }
                };
                let symbol = if sign.staged {
                    &config.editor.git_signs.staged
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
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let full_content = line.strip_suffix('\n').unwrap_or(&line);
        let leading_indentation = leading_indentation_width(full_content, config.editor.tab_width);
        let can_wrap = wrap_enabled;
        let skipped_bytes = if can_wrap {
            line_rows[first_line_row].byte_start
        } else {
            byte_offset_at_display_column(
                full_content,
                view.scroll_column,
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        };
        let skipped_column = if can_wrap {
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
        while inlay_hints
            .get(inlay_cursor)
            .is_some_and(|hint| hint.position < visible_start)
        {
            inlay_cursor = inlay_cursor.saturating_add(1);
        }
        let mut rendered_inlays = 0usize;
        let mut x = gutter_width;
        let mut source_column = skipped_column;
        let mut visual_row_index = first_line_row;
        for (relative, grapheme) in visible_content.grapheme_indices(true) {
            let source_relative = skipped_bytes.saturating_add(relative);
            if can_wrap
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
            while let Some(hint) = inlay_hints
                .get(inlay_cursor)
                .filter(|hint| hint.position == global)
            {
                if rendered_inlays < 32 {
                    x = grid.put_text_clipped(x, screen_row, &hint.label, Style::Muted, width);
                    rendered_inlays = rendered_inlays.saturating_add(1);
                }
                inlay_cursor = inlay_cursor.saturating_add(1);
            }
            let style = if is_selected(
                view.selection,
                global,
                &buffer.text,
                DisplayWidthPolicy {
                    tab_width: config.editor.tab_width,
                    ambiguous_wide: config.editor.ambiguous_width == AmbiguousWidth::Wide,
                },
            ) || view.multi_marks.iter().any(|mark| {
                mark.contains(global) || (mark.is_empty() && mark.start == global)
            }) || view.block_insert.contains(&global)
            {
                Style::Selection
            } else if search_ranges.iter().any(|range| range.contains(global))
                || flash.is_some_and(|flash| flash.matches.iter().any(|range| range.contains(global)))
            {
                Style::Search
            } else if let Some(style) = diagnostic_lines.get(&line_index).and_then(|diagnostics| {
                diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        diagnostic.range.contains(global)
                            || (diagnostic.range.is_empty() && diagnostic.range.start == global)
                    })
                    .map(|diagnostic| diagnostic_style(diagnostic.severity))
                    .next()
            }) {
                style
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
                super::overlays::color_swatch_style(span.rgb)
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
            } else if let Some(span) = {
                while semantic_spans
                    .get(semantic_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    semantic_cursor = semantic_cursor.saturating_add(1);
                }
                semantic_spans
                    .get(semantic_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                syntax_role_style(span.role)
            } else if let Some(span) = {
                while syntax_spans
                    .get(syntax_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    syntax_cursor = syntax_cursor.saturating_add(1);
                }
                syntax_spans
                    .get(syntax_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                syntax_role_style(span.role)
            } else {
                Style::Default
            };
            let grapheme_column = source_column;
            let cells = if grapheme == "\t" {
                config.editor.tab_width - (source_column % config.editor.tab_width)
            } else {
                text_width(grapheme, config.editor.ambiguous_width).max(1)
            };
            let visible_cells = if can_wrap {
                cells.min(content_width)
            } else {
                cells
            };
            let advance = u16::try_from(visible_cells).unwrap_or(u16::MAX);
            if !can_wrap && x.saturating_add(advance) > width {
                break;
            }
            source_column = source_column.saturating_add(cells);
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
                    grid.set_cell(x, screen_row, label.encode_utf8(&mut bytes), Style::StatusAccent);
                } else if can_wrap && cells > content_width {
                    let replacement = if text_width(
                        &config.editor.control_character_replacement,
                        config.editor.ambiguous_width,
                    ) == 1
                    {
                        config.editor.control_character_replacement.as_str()
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
        if let Some(fold) = ui.lsp_folds.iter().chain(ui.syntax_folds.iter()).find(|fold| {
            fold.view == view_id && fold.collapsed && fold.start_line == line_index
        }) {
            let hidden = fold.end_line.saturating_sub(fold.start_line);
            x = grid.put_text_clipped(x, screen_row, &format!(" ⋯ {hidden} lines"), Style::Muted, width);
        }
        while let Some(hint) = inlay_hints
            .get(inlay_cursor)
            .filter(|hint| hint.position <= line_end)
        {
            if hint.position == line_end && rendered_inlays < 32 {
                x = grid.put_text_clipped(x, screen_row, &hint.label, Style::Muted, width);
                rendered_inlays = rendered_inlays.saturating_add(1);
            }
            inlay_cursor = inlay_cursor.saturating_add(1);
        }
        if let Some(diagnostics) = diagnostic_lines.get(&line_index) {
            if let Some(diagnostic) = diagnostics.first() {
                let text = format!("  ●● {}", diagnostic_label(diagnostic));
                grid.put_text_clipped(
                    x,
                    screen_row,
                    &text,
                    diagnostic_style(diagnostic.severity),
                    width,
                );
            }
        }
        screen_row = screen_row.saturating_add(1);
        if ui.expanded_diagnostic.as_ref().is_some_and(|expanded| {
            expanded.line == line_index
                && buffer.path.as_ref() == Some(&expanded.path)
                && diagnostic_lines
                    .get(&line_index)
                    .is_some_and(|diagnostics| {
                        diagnostics
                            .iter()
                            .any(|diagnostic| expanded.severities.contains(&diagnostic.severity))
                    })
        }) {
            if let Some(diagnostics) = diagnostic_lines.get(&line_index) {
                if screen_row < body_height {
                    grid.put_text_clipped(
                        gutter_width,
                        screen_row,
                        "Diagnostics:",
                        Style::Muted,
                        width,
                    );
                    screen_row = screen_row.saturating_add(1);
                }
                let severities = &ui
                    .expanded_diagnostic
                    .as_ref()
                    .expect("checked expanded diagnostic")
                    .severities;
                for (index, diagnostic) in diagnostics
                    .iter()
                    .filter(|diagnostic| severities.contains(&diagnostic.severity))
                    .enumerate()
                {
                    let text = format!(
                        "{}. {}",
                        index.saturating_add(1),
                        diagnostic_label(diagnostic)
                    );
                    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
                    for row in super::debug::wrap_rows(&lines, width.saturating_sub(gutter_width)) {
                        if screen_row >= body_height {
                            break;
                        }
                        grid.put_text_clipped(
                            gutter_width,
                            screen_row,
                            &row,
                            diagnostic_style(diagnostic.severity),
                            width,
                        );
                        screen_row = screen_row.saturating_add(1);
                    }
                }
            }
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
    let bar_rows = u16::from(config.ui.status_line.enabled && ui.operation_bar.is_some() && height > 1);
    if bar_rows == 1 {
        render_operation_bar(&mut grid, ui, width, height - 2);
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
            EditorMode::Replace => config.ui.cursor.replace,
            EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock
            | EditorMode::Multi => config.ui.cursor.visual,
            EditorMode::OperatorPending => config.ui.cursor.operator_pending,
            EditorMode::CommandLine => config.ui.cursor.command,
            EditorMode::Prompt => config.ui.cursor.prompt,
            EditorMode::Normal => config.ui.cursor.normal,
        };
        (x, y, cursor_style(configured))
    });
    Frame { grid, cursor }
}

/// Draw the operation bar. At narrow widths the blockers are dropped and then the phase is
/// shortened, because the operation and the cancellation are what must not be lost.
fn render_operation_bar(grid: &mut CellGrid, ui: &UiState, width: u16, y: u16) {
    let Some(bar) = ui.operation_bar.as_ref() else {
        return;
    };
    grid.fill_row(y, Style::Status);
    let row = bar.row(width);
    grid.put_text_clipped(0, y, &row, Style::Status, width);
}

pub(super) fn render_status_line(
    grid: &mut CellGrid,
    state: &EditorState,
    view_id: crate::editor::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    status_y: u16,
) {
    let Some(view) = state.views.get(view_id) else {
        return;
    };
    let Some(buffer) = state.buffers.get(view.buffer) else {
        return;
    };
    grid.fill_row(status_y, Style::Status);
    // The agent panel's input has its own Insert and Normal modes.
    let panel = ui.agent_panel.as_ref().filter(|panel| panel.view == view_id);
    let agent = panel.is_some();
    let mode_label = config
        .ui
        .status_line
        .mode_labels
        .get(match panel {
            Some(panel) if panel.normal => "normal",
            Some(_) => "insert",
            None => mode_name(view.mode),
        })
        .expect("resolved configuration validates mode labels");
    let mode = format!(
        "{}{}{}",
        config.ui.status_line.mode_padding, mode_label, config.ui.status_line.mode_padding
    );
    let name = buffer
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || {
                buffer
                    .virtual_owner()
                    .unwrap_or(&config.ui.status_line.scratch_label)
                    .to_owned()
            },
            |name| name.to_string_lossy().into_owned(),
        );
    // The compact indicator of the same state, so the operation survives where the bar cannot be
    // drawn in full.
    let operation_indicator = ui
        .operation_bar
        .as_ref()
        .map(|bar| format!(" {} ", bar.indicator()))
        .unwrap_or_default();
    let dirty = if buffer.dirty {
        config.ui.status_line.dirty_marker.as_str()
    } else {
        ""
    };
    let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
    let position = if agent {
        String::new()
    } else {
        config
            .ui
            .status_line
            .position_format
            .replace("{line}", &(current_line + 1).to_string())
            .replace(
                "{column}",
                &(byte_column(&buffer.text, view.cursor) + 1).to_string(),
            )
    };
    let lsp_status = ui
        .lsp_server
        .as_ref()
        .map(|server| config.ui.status_line.lsp_format.replace("{server}", server))
        .unwrap_or_default();
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
    let diagnostic_status = if errors + warnings > 0 {
        config
            .ui
            .status_line
            .diagnostic_format
            .replace("{errors}", &errors.to_string())
            .replace("{warnings}", &warnings.to_string())
    } else {
        String::new()
    };
    let git_status = if ui.git_changed_files > 0 {
        config
            .ui
            .status_line
            .git_format
            .replace("{count}", &ui.git_changed_files.to_string())
    } else {
        String::new()
    };
    let task_status = if ui.running_tasks > 0 {
        config
            .ui
            .status_line
            .task_format
            .replace("{count}", &ui.running_tasks.to_string())
    } else {
        String::new()
    };
    let root_status = config
        .ui
        .status_line
        .root_format
        .replace("{root}", &workspace_title(state, config));
    // The operation indicator is drawn FIRST, so it survives every configured component layout:
    // a paused operation must not be hidden by a user's statusline choices.
    let mut x = if operation_indicator.is_empty() {
        0
    } else {
        grid.put_text_clipped(0, status_y, &operation_indicator, Style::StatusAccent, width)
    };
    let separator = &config.ui.status_line.component_separator;
    let separator_cells = text_cells(separator, config);
    // The right side holds the position, the root and the server. The field furthest from the
    // edge goes first when the mode and the file name need the room.
    let left_needed = text_cells(&mode, config)
        .saturating_add(separator_cells)
        .saturating_add(text_cells(&name, config))
        .saturating_add(text_cells(dirty, config))
        .saturating_add(x);
    let mut right_cells = 0_u16;
    let mut right_fields = Vec::new();
    for component in config.ui.status_line.components.iter().rev() {
        let text = match component.as_str() {
            "position" => position.as_str(),
            "root" => root_status.as_str(),
            "lsp" => lsp_status.as_str(),
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        let cells = text_cells(text, config).saturating_add(separator_cells);
        if component != "position"
            && left_needed.saturating_add(right_cells).saturating_add(cells) >= width
        {
            continue;
        }
        right_cells = right_cells.saturating_add(cells);
        right_fields.push(text);
    }
    let left_limit = width.saturating_sub(right_cells);
    let mut right_x = left_limit.saturating_add(separator_cells);
    for text in right_fields.iter().rev() {
        right_x = grid.put_text_clipped(right_x, status_y, text, Style::Status, width);
        right_x = right_x.saturating_add(separator_cells);
    }
    for component in &config.ui.status_line.components {
        let (text, style) = match component.as_str() {
            "mode" => (mode.as_str(), Style::StatusAccent),
            "file" => (if buffer.dirty { "" } else { name.as_str() }, Style::Status),
            "large_file" => (
                if ui.large_file_buffers.contains(&view.buffer) {
                    config.editor.large_file.status_label.as_str()
                } else {
                    ""
                },
                Style::StatusAccent,
            ),
            "diagnostics" => (
                diagnostic_status.as_str(),
                if errors > 0 {
                    Style::Error
                } else {
                    Style::Warning
                },
            ),
            "git" => (git_status.as_str(), Style::StatusAccent),
            "tasks" => (task_status.as_str(), Style::StatusAccent),
            _ => continue,
        };
        let owned_file;
        let text = if component == "file" && buffer.dirty {
            owned_file = format!("{name}{dirty}");
            owned_file.as_str()
        } else {
            text
        };
        if text.is_empty() {
            continue;
        }
        // The mode and the file name are cut to fit; other fields go whole or not at all.
        if !matches!(component.as_str(), "mode" | "file")
            && x.saturating_add(text_cells(text, config)) > left_limit
        {
            continue;
        }
        x = grid.put_text_clipped(x, status_y, text, style, left_limit);
        x = grid.put_text_clipped(x, status_y, separator, Style::Status, left_limit);
    }
}

fn text_cells(text: &str, config: &Config) -> u16 {
    u16::try_from(text_width(text, config.editor.ambiguous_width)).unwrap_or(u16::MAX)
}

/// The open folder's name; a linked worktree also names its repository.
pub(crate) fn workspace_title(state: &EditorState, config: &Config) -> String {
    let folder = state
        .workspace_root
        .file_name()
        .unwrap_or(state.workspace_root.as_os_str())
        .to_string_lossy();
    match &state.workspace_repository {
        Some(repository) => config
            .ui
            .status_line
            .worktree_format
            .replace("{repository}", repository)
            .replace("{worktree}", &folder),
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
        let mut hints = ui
            .lsp_inlay_hints
            .iter()
            .chain(&ui.substitute_preview)
            .filter(|hint| {
                hint.buffer == view.buffer
                    && hint.revision == buffer.text.revision()
                    && hint.position >= visible_start
                    && hint.position <= view.cursor
            })
            .collect::<Vec<_>>();
        hints.sort_unstable_by_key(|hint| hint.position);
        let virtual_cells = hints
            .into_iter()
            .take(32)
            .map(|hint| text_width(&hint.label, config.editor.ambiguous_width))
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

pub(super) fn byte_offset_at_display_column(
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

pub(super) fn is_selected(
    selection: Option<crate::editor::Selection>,
    offset: ByteOffset,
    text: &crate::editor::text::TextBuffer,
    width_policy: DisplayWidthPolicy,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    match selection.kind {
        SelectionKind::Character => {
            let start = selection.anchor.min(selection.head);
            let end = selection.anchor.max(selection.head);
            offset >= start && offset <= end
        }
        SelectionKind::Line => {
            let line = text.byte_to_line(offset).unwrap_or(usize::MAX);
            let anchor = text.byte_to_line(selection.anchor).unwrap_or(0);
            let head = text.byte_to_line(selection.head).unwrap_or(0);
            (anchor.min(head)..=anchor.max(head)).contains(&line)
        }
        SelectionKind::Block => {
            let point_line = text.byte_to_line(offset).unwrap_or(usize::MAX);
            let anchor_line = text.byte_to_line(selection.anchor).unwrap_or(0);
            let head_line = text.byte_to_line(selection.head).unwrap_or(0);
            if !(anchor_line.min(head_line)..=anchor_line.max(head_line)).contains(&point_line) {
                return false;
            }
            let Ok(anchor_start) = text.line_to_byte(anchor_line) else {
                return false;
            };
            let Ok(head_start) = text.line_to_byte(head_line) else {
                return false;
            };
            let Ok(anchor_text) = text.line(anchor_line) else {
                return false;
            };
            let Ok(head_text) = text.line(head_line) else {
                return false;
            };
            let anchor_content = anchor_text.strip_suffix('\n').unwrap_or(&anchor_text);
            let head_content = head_text.strip_suffix('\n').unwrap_or(&head_text);
            let anchor_column = display_column_at(
                anchor_content,
                selection.anchor.get().saturating_sub(anchor_start.get()),
                width_policy,
            );
            let head_column = display_column_at(
                head_content,
                selection.head.get().saturating_sub(head_start.get()),
                width_policy,
            );
            let Ok(point_start) = text.line_to_byte(point_line) else {
                return false;
            };
            let Ok(point_text) = text.line(point_line) else {
                return false;
            };
            let point_content = point_text.strip_suffix('\n').unwrap_or(&point_text);
            byte_range_for_display_columns(point_content, anchor_column, head_column, width_policy)
                .is_some_and(|(start, end)| {
                    let relative = offset.get().saturating_sub(point_start.get());
                    (start..end).contains(&relative)
                })
        }
    }
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
