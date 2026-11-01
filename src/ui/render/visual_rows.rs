use std::collections::BTreeMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::{AmbiguousWidth, Config, EditorConfig, LineNumbers},
    editor::{
        EditorState,
        text::{ByteOffset, ByteRange, DisplayWidthPolicy, display_column_at},
    },
    ui::{Style, geometry::text_width},
};

use super::{UiState, editor::display_column};

pub(super) fn syntax_role_style(role: crate::language::syntax::SyntaxRole) -> Style {
    match role {
        crate::language::syntax::SyntaxRole::Comment => Style::SyntaxComment,
        crate::language::syntax::SyntaxRole::String => Style::SyntaxString,
        crate::language::syntax::SyntaxRole::Number => Style::SyntaxNumber,
        crate::language::syntax::SyntaxRole::Keyword => Style::SyntaxKeyword,
        crate::language::syntax::SyntaxRole::Type => Style::SyntaxType,
        crate::language::syntax::SyntaxRole::Variable => Style::SyntaxVariable,
        crate::language::syntax::SyntaxRole::Heading => Style::MarkupHeading,
        crate::language::syntax::SyntaxRole::Strong => Style::MarkupStrong,
        crate::language::syntax::SyntaxRole::Emphasis => Style::MarkupEmphasis,
        crate::language::syntax::SyntaxRole::Code => Style::MarkupCode,
        crate::language::syntax::SyntaxRole::Link => Style::MarkupLink,
        crate::language::syntax::SyntaxRole::Quote => Style::MarkupQuote,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct VisualRowAnchor {
    pub line: usize,
    pub wrap_row: usize,
}

/// Pure visual-row ownership shared by viewport follow, rendering, paging,
/// cursor projection, and mouse hit testing. It reads immutable editor/UI
/// snapshots only and performs no parsing or service work.
pub struct VisualRowModel<'a> {
    text: &'a crate::editor::text::TextBuffer,
    folds: Vec<(usize, usize)>,
    wrap: bool,
    content_width: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
    inlay_widths: BTreeMap<ByteOffset, usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisualRowSlice {
    pub byte_start: usize,
    pub byte_end: usize,
    pub source_column_start: usize,
    pub used_cells: usize,
}

struct VisualRowConstruction<'text, 'input> {
    text: &'text crate::editor::text::TextBuffer,
    view: crate::editor::ViewId,
    ui: &'input UiState,
    config: &'input Config,
    pane_width: u16,
    wrap: bool,
    minimum_digits: usize,
    inlay_widths: BTreeMap<ByteOffset, usize>,
}

impl<'a> VisualRowModel<'a> {
    #[must_use]
    pub fn new(
        text: &'a crate::editor::text::TextBuffer,
        view: crate::editor::ViewId,
        ui: &UiState,
        config: &Config,
        pane_width: u16,
        wrap: bool,
    ) -> Self {
        Self::new_with_minimum_digits(VisualRowConstruction {
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            minimum_digits: line_number_digits(text),
            inlay_widths: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn new_for_state(
        state: &EditorState,
        text: &'a crate::editor::text::TextBuffer,
        view: crate::editor::ViewId,
        ui: &UiState,
        config: &Config,
        pane_width: u16,
        wrap: bool,
    ) -> Self {
        let mut inlay_widths = BTreeMap::<ByteOffset, usize>::new();
        if let Some(buffer) = state.views.get(view).map(|view| view.buffer) {
            let revision = text.revision();
            let mut hints = ui
                .lsp_inlay_hints
                .iter()
                .chain(&ui.substitute_preview)
                .filter(|hint| hint.buffer == buffer && hint.revision == revision)
                .take(10_000)
                .collect::<Vec<_>>();
            hints.sort_unstable_by_key(|hint| hint.position);
            let mut per_line = BTreeMap::<usize, usize>::new();
            for hint in hints {
                let Ok(line) = text.byte_to_line(hint.position) else {
                    continue;
                };
                let count = per_line.entry(line).or_default();
                if *count >= 32 {
                    continue;
                }
                *count = count.saturating_add(1);
                let width = text_width(&hint.label, config.editor.ambiguous_width);
                inlay_widths
                    .entry(hint.position)
                    .and_modify(|current| *current = current.saturating_add(width))
                    .or_insert(width);
            }
        }
        Self::new_with_minimum_digits(VisualRowConstruction {
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            minimum_digits: editor_line_number_digits(state),
            inlay_widths,
        })
    }

    fn new_with_minimum_digits(construction: VisualRowConstruction<'a, '_>) -> Self {
        let VisualRowConstruction {
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            minimum_digits,
            inlay_widths,
        } = construction;
        let maximum = text.len_lines().saturating_sub(1);
        let mut folds = ui
            .lsp_folds
            .iter()
            .chain(ui.syntax_folds.iter())
            .filter(|fold| fold.view == view && fold.collapsed)
            .filter_map(|fold| {
                let start = fold.start_line.min(maximum);
                let end = fold.end_line.min(maximum);
                (start < end).then_some((start, end))
            })
            .collect::<Vec<_>>();
        folds.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
        let mut normalized_folds: Vec<(usize, usize)> = Vec::with_capacity(folds.len());
        for (start, end) in folds {
            if let Some(last) = normalized_folds.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            normalized_folds.push((start, end));
        }
        Self {
            text,
            folds: normalized_folds,
            wrap,
            content_width: usize::from(pane_width.saturating_sub(editor_gutter_width_with_digits(
                text,
                &config.editor,
                pane_width,
                minimum_digits,
            )))
            .max(1),
            tab_width: config.editor.tab_width.max(1),
            ambiguous_width: config.editor.ambiguous_width,
            inlay_widths,
        }
    }

    pub(super) fn containing_fold(&self, line: usize) -> Option<(usize, usize)> {
        self.folds
            .iter()
            .copied()
            .find(|&(start, end)| line > start && line <= end)
    }

    pub(super) fn grapheme_width(&self, grapheme: &str, source_column: usize) -> usize {
        if grapheme == "\t" {
            self.tab_width - (source_column % self.tab_width)
        } else {
            text_width(grapheme, self.ambiguous_width).max(1)
        }
    }

    /// Return exact grapheme-atomic slices for one logical line. A grapheme is
    /// never split across rows. If it is wider than a tiny content body, it
    /// alone consumes one row and its visible width is clipped to that body.
    #[must_use]
    pub fn line_rows(&self, line: usize) -> Vec<VisualRowSlice> {
        let text = self.text.line(line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        if !self.wrap {
            return vec![VisualRowSlice {
                byte_start: 0,
                byte_end: content.len(),
                source_column_start: 0,
                used_cells: display_column(content, self.tab_width, self.ambiguous_width),
            }];
        }
        let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
        if content.is_empty() {
            return vec![VisualRowSlice {
                byte_start: 0,
                byte_end: 0,
                source_column_start: 0,
                used_cells: self
                    .inlay_widths
                    .get(&line_start)
                    .copied()
                    .unwrap_or(0)
                    .min(self.content_width),
            }];
        }
        let mut rows = Vec::new();
        let mut row_start = 0usize;
        let mut row_source_column = 0usize;
        let mut row_used = 0usize;
        let mut source_column = 0usize;
        for (relative, grapheme) in content.grapheme_indices(true) {
            let virtual_width = self
                .inlay_widths
                .get(&ByteOffset(line_start.get().saturating_add(relative)))
                .copied()
                .unwrap_or(0)
                .min(self.content_width);
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if row_used > 0
                && row_used
                    .saturating_add(virtual_width)
                    .saturating_add(visible_width)
                    > self.content_width
            {
                rows.push(VisualRowSlice {
                    byte_start: row_start,
                    byte_end: relative,
                    source_column_start: row_source_column,
                    used_cells: row_used,
                });
                row_start = relative;
                row_source_column = source_column;
                row_used = 0;
            }
            row_used = row_used
                .saturating_add(virtual_width)
                .saturating_add(visible_width)
                .min(self.content_width);
            source_column = source_column.saturating_add(source_width);
        }
        row_used = row_used
            .saturating_add(
                self.inlay_widths
                    .get(&ByteOffset(line_start.get().saturating_add(content.len())))
                    .copied()
                    .unwrap_or(0),
            )
            .min(self.content_width);
        rows.push(VisualRowSlice {
            byte_start: row_start,
            byte_end: content.len(),
            source_column_start: row_source_column,
            used_cells: row_used,
        });
        rows
    }

    #[must_use]
    pub fn row_slice(&self, anchor: VisualRowAnchor) -> VisualRowSlice {
        let anchor = self.normalize(anchor);
        self.line_rows(anchor.line)
            .get(anchor.wrap_row)
            .copied()
            .unwrap_or(VisualRowSlice {
                byte_start: 0,
                byte_end: 0,
                source_column_start: 0,
                used_cells: 0,
            })
    }

    pub(super) fn row_cell_for_relative_byte(
        &self,
        line: usize,
        row: VisualRowSlice,
        byte: usize,
    ) -> usize {
        let text = self.text.line(line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
        let mut source_column = row.source_column_start;
        let mut cell = 0usize;
        for (relative, grapheme) in content[row.byte_start..end].grapheme_indices(true) {
            let relative = row.byte_start.saturating_add(relative);
            if byte < relative {
                return cell.min(self.content_width.saturating_sub(1));
            }
            cell = cell.saturating_add(
                self.inlay_widths
                    .get(&ByteOffset(line_start.get().saturating_add(relative)))
                    .copied()
                    .unwrap_or(0)
                    .min(self.content_width),
            );
            if byte == relative {
                return cell.min(self.content_width.saturating_sub(1));
            }
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if byte < relative.saturating_add(grapheme.len()) {
                return cell.min(self.content_width.saturating_sub(1));
            }
            cell = cell.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        if end == content.len() && byte >= end {
            cell = cell.saturating_add(
                self.inlay_widths
                    .get(&ByteOffset(line_start.get().saturating_add(content.len())))
                    .copied()
                    .unwrap_or(0)
                    .min(self.content_width),
            );
        }
        // Exact-width line endings stay on the final owned cell; no synthetic
        // empty row is introduced between adjacent logical lines.
        cell.min(self.content_width.saturating_sub(1))
    }

    #[must_use]
    pub fn cell_for_byte(&self, anchor: VisualRowAnchor, byte: usize) -> usize {
        let anchor = self.normalize(anchor);
        self.row_cell_for_relative_byte(anchor.line, self.row_slice(anchor), byte)
    }

    #[must_use]
    pub fn byte_at_cell(&self, anchor: VisualRowAnchor, cell: usize) -> usize {
        let anchor = self.normalize(anchor);
        let row = self.row_slice(anchor);
        let text = self.text.line(anchor.line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let line_start = self.text.line_to_byte(anchor.line).unwrap_or(ByteOffset(0));
        let mut source_column = row.source_column_start;
        let mut used = 0usize;
        for (relative, grapheme) in content[row.byte_start..end].grapheme_indices(true) {
            let relative = row.byte_start.saturating_add(relative);
            let virtual_width = self
                .inlay_widths
                .get(&ByteOffset(line_start.get().saturating_add(relative)))
                .copied()
                .unwrap_or(0)
                .min(self.content_width);
            if cell < used.saturating_add(virtual_width) {
                return relative;
            }
            used = used.saturating_add(virtual_width);
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if cell < used.saturating_add(visible_width) {
                return relative;
            }
            used = used.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        row.byte_end
    }

    #[must_use]
    pub fn normalize(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let maximum = self.text.len_lines().saturating_sub(1);
        let mut line = anchor.line.min(maximum);
        if let Some((start, _)) = self.containing_fold(line) {
            line = start;
        }
        VisualRowAnchor {
            line,
            wrap_row: anchor
                .wrap_row
                .min(self.rows_for_line(line).saturating_sub(1)),
        }
    }

    #[must_use]
    pub fn rows_for_line(&self, line: usize) -> usize {
        self.line_rows(line).len()
    }

    pub(super) fn next_line(&self, line: usize) -> usize {
        let folded_end = self
            .folds
            .iter()
            .find_map(|&(start, end)| (start == line).then_some(end))
            .unwrap_or(line);
        folded_end
            .saturating_add(1)
            .min(self.text.len_lines().saturating_sub(1))
    }

    pub(super) fn previous_line(&self, line: usize) -> usize {
        let previous = line.saturating_sub(1);
        self.containing_fold(previous)
            .map_or(previous, |(start, _)| start)
    }

    #[must_use]
    pub fn next(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let anchor = self.normalize(anchor);
        if anchor.wrap_row + 1 < self.rows_for_line(anchor.line) {
            return VisualRowAnchor {
                line: anchor.line,
                wrap_row: anchor.wrap_row + 1,
            };
        }
        let line = self.next_line(anchor.line);
        if line == anchor.line {
            anchor
        } else {
            VisualRowAnchor { line, wrap_row: 0 }
        }
    }

    #[must_use]
    pub fn previous(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let anchor = self.normalize(anchor);
        if anchor.wrap_row > 0 {
            return VisualRowAnchor {
                line: anchor.line,
                wrap_row: anchor.wrap_row - 1,
            };
        }
        let line = self.previous_line(anchor.line);
        if line == anchor.line {
            anchor
        } else {
            VisualRowAnchor {
                line,
                wrap_row: self.rows_for_line(line).saturating_sub(1),
            }
        }
    }

    #[must_use]
    pub fn forward(&self, mut anchor: VisualRowAnchor, count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        for _ in 0..count {
            let next = self.next(anchor);
            if next == anchor {
                break;
            }
            anchor = next;
        }
        anchor
    }

    #[must_use]
    pub fn backward(&self, mut anchor: VisualRowAnchor, count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        for _ in 0..count {
            let previous = self.previous(anchor);
            if previous == anchor {
                break;
            }
            anchor = previous;
        }
        anchor
    }

    #[must_use]
    pub fn cursor_projection(&self, cursor: ByteOffset) -> (VisualRowAnchor, usize) {
        let line = self.text.byte_to_line(cursor).unwrap_or(0);
        if let Some((start, _)) = self.containing_fold(line) {
            return (
                VisualRowAnchor {
                    line: start,
                    wrap_row: 0,
                },
                0,
            );
        }
        if !self.wrap {
            let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
            let prefix = self
                .text
                .slice(ByteRange::new(line_start, cursor).expect("cursor line range"))
                .unwrap_or_default();
            return (
                VisualRowAnchor { line, wrap_row: 0 },
                display_column(&prefix, self.tab_width, self.ambiguous_width),
            );
        }
        let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
        let relative = cursor.get().saturating_sub(line_start.get());
        let rows = self.line_rows(line);
        let row_index = rows
            .iter()
            .enumerate()
            .find_map(|(index, row)| {
                (relative < row.byte_end || (relative == row.byte_end && index + 1 == rows.len()))
                    .then_some(index)
            })
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        let anchor = VisualRowAnchor {
            line,
            wrap_row: row_index,
        };
        (
            anchor,
            self.row_cell_for_relative_byte(line, rows[row_index], relative),
        )
    }

    #[must_use]
    pub fn cursor_anchor(&self, cursor: ByteOffset) -> VisualRowAnchor {
        self.cursor_projection(cursor).0
    }

    #[must_use]
    pub fn screen_row(
        &self,
        start: VisualRowAnchor,
        target: VisualRowAnchor,
        height: usize,
    ) -> Option<usize> {
        let mut current = self.normalize(start);
        let target = self.normalize(target);
        for row in 0..height {
            if current == target {
                return Some(row);
            }
            let next = self.next(current);
            if next == current {
                break;
            }
            current = next;
        }
        None
    }
}

pub(super) fn one_cell_symbol<'a>(
    symbol: &'a str,
    fallback: &'static str,
    ambiguous_width: AmbiguousWidth,
) -> &'a str {
    if text_width(symbol, ambiguous_width) == 1 {
        symbol
    } else {
        fallback
    }
}

pub(super) fn leading_indentation_width(text: &str, tab_width: usize) -> usize {
    let mut column = 0usize;
    for byte in text.bytes() {
        match byte {
            b' ' => column = column.saturating_add(1),
            b'\t' => {
                column = column.saturating_add(tab_width - (column % tab_width));
            }
            _ => break,
        }
    }
    column
}

pub(super) fn is_indent_guide_column(
    column: usize,
    indentation: usize,
    indent_width: usize,
) -> bool {
    column < indentation && column % indent_width == 0
}

pub(super) const INDENT_GUIDE_SCOPE_SCAN_LIMIT: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ActiveIndentGuide {
    column: usize,
    first_line: usize,
    last_line: usize,
}

impl ActiveIndentGuide {
    pub(super) fn contains(self, line: usize, column: usize) -> bool {
        (self.first_line..=self.last_line).contains(&line) && self.column == column
    }
}

pub(super) fn line_indentation(
    text: &crate::editor::text::TextBuffer,
    line: usize,
    tab_width: usize,
) -> Option<(usize, bool)> {
    let source = text.line(line).ok()?;
    let content = source.trim_end_matches(['\r', '\n']);
    Some((
        leading_indentation_width(content, tab_width),
        content.trim().is_empty(),
    ))
}

pub(super) fn active_indent_guide(
    text: &crate::editor::text::TextBuffer,
    cursor: ByteOffset,
    current_line: usize,
    tab_width: usize,
    indent_width: usize,
    ambiguous_wide: bool,
) -> Option<ActiveIndentGuide> {
    let source = text.line(current_line).ok()?;
    let content = source.trim_end_matches(['\r', '\n']);
    let indentation = leading_indentation_width(content, tab_width);
    let trimmed = content.trim_start();
    let closes_scope = trimmed.starts_with(['}', ')', ']']);
    let opens_scope = trimmed.ends_with('{') || trimmed.ends_with(':');
    let line_start = text.line_to_byte(current_line).ok()?;
    let cursor_column = display_column_at(
        content,
        cursor
            .get()
            .saturating_sub(line_start.get())
            .min(content.len()),
        DisplayWidthPolicy {
            tab_width,
            ambiguous_wide,
        },
    );
    let cursor_on_scope_delimiter = (opens_scope || closes_scope) && cursor_column >= indentation;
    let column = if cursor_on_scope_delimiter {
        indentation
    } else if indentation > 0 {
        cursor_column
            .min(indentation.saturating_sub(1))
            .checked_div(indent_width)?
            .saturating_mul(indent_width)
    } else {
        return None;
    };
    let minimum_indentation = column.saturating_add(indent_width);
    let (mut seed, scan_direction) = if closes_scope {
        (current_line.checked_sub(1)?, -1_i8)
    } else if opens_scope {
        (current_line.checked_add(1)?, 1_i8)
    } else {
        (current_line, 1_i8)
    };

    for _ in 0..INDENT_GUIDE_SCOPE_SCAN_LIMIT {
        let (candidate_indentation, blank) = line_indentation(text, seed, tab_width)?;
        if !blank {
            if candidate_indentation < minimum_indentation {
                return None;
            }
            break;
        }
        seed = match scan_direction {
            -1 => seed.checked_sub(1)?,
            1 => seed
                .checked_add(1)
                .filter(|line| *line < text.len_lines())?,
            _ => unreachable!(),
        };
    }

    let mut first_line = seed;
    for _ in 0..INDENT_GUIDE_SCOPE_SCAN_LIMIT {
        let Some(previous) = first_line.checked_sub(1) else {
            break;
        };
        let (candidate_indentation, blank) = line_indentation(text, previous, tab_width)?;
        if !blank && candidate_indentation < minimum_indentation {
            break;
        }
        first_line = previous;
    }

    let mut last_line = seed;
    for _ in 0..INDENT_GUIDE_SCOPE_SCAN_LIMIT {
        let Some(next) = last_line
            .checked_add(1)
            .filter(|line| *line < text.len_lines())
        else {
            break;
        };
        let (candidate_indentation, blank) = line_indentation(text, next, tab_width)?;
        if !blank && candidate_indentation < minimum_indentation {
            break;
        }
        last_line = next;
    }

    Some(ActiveIndentGuide {
        column,
        first_line,
        last_line,
    })
}

pub(super) fn line_number_digits(text: &crate::editor::text::TextBuffer) -> usize {
    text.visible_lines().max(1).to_string().len()
}

#[must_use]
pub fn editor_line_number_digits(state: &EditorState) -> usize {
    state
        .buffers
        .values()
        .map(|buffer| line_number_digits(&buffer.text))
        .max()
        .unwrap_or(1)
}

pub(super) fn configured_editor_gutter_width(
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    minimum_digits: usize,
) -> usize {
    if config.line_numbers == LineNumbers::Off {
        return 0;
    }
    line_number_digits(text)
        .max(minimum_digits)
        .saturating_add(text_width(
            &config.line_number_separator,
            config.ambiguous_width,
        ))
        .saturating_add(usize::from(config.git_signs.enabled))
        .saturating_add(usize::from(config.gutter_padding))
}

pub(super) fn git_sign_column(gutter_width: u16, config: &EditorConfig) -> Option<u16> {
    config.git_signs.enabled.then(|| {
        let trailing_padding = config.gutter_padding.min(gutter_width.saturating_sub(1));
        gutter_width
            .saturating_sub(trailing_padding)
            .saturating_sub(1)
    })
}

pub(super) fn editor_gutter_width_with_digits(
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
    minimum_digits: usize,
) -> u16 {
    let maximum = pane_width.saturating_sub(1);
    u16::try_from(configured_editor_gutter_width(text, config, minimum_digits))
        .unwrap_or(u16::MAX)
        .min(maximum)
}

/// Effective display-cell gutter for an exact pane width. Every non-empty
/// pane reserves at least one cell for text/cursor projection.
#[must_use]
pub fn editor_gutter_width(
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> u16 {
    editor_gutter_width_with_digits(text, config, pane_width, line_number_digits(text))
}

#[must_use]
pub fn editor_state_gutter_width(
    state: &EditorState,
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> u16 {
    editor_gutter_width_with_digits(text, config, pane_width, editor_line_number_digits(state))
}

/// Display cells available to buffer content in an exact pane viewport.
/// A zero-width pane has no synthetic content cell.
#[must_use]
pub fn editor_content_width(
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> usize {
    usize::from(pane_width.saturating_sub(editor_gutter_width(text, config, pane_width)))
}

#[must_use]
pub fn editor_state_content_width(
    state: &EditorState,
    text: &crate::editor::text::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> usize {
    usize::from(
        pane_width.saturating_sub(editor_state_gutter_width(state, text, config, pane_width)),
    )
}
