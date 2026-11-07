//! Visual buffer rows, wrapping and viewport caches.

use std::cell::RefCell;
use std::collections::BTreeMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::config::AmbiguousWidth;

use crate::config::Config;

use crate::config::EditorConfig;

use crate::config::LineNumbers;

use crate::editor::EditorState;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::DisplayWidthPolicy;

use crate::editor::text::display_column_at;

use crate::ui::geometry::cell_text_width;

use crate::ui::geometry::text_width;

use crate::ui::render::editor::GUTTER_PADDING;

use crate::ui::render::editor::LINE_NUMBER_SEPARATOR;

use crate::ui::render::UiState;

use crate::ui::render::editor::display_column;

#[derive(Clone)]
pub(super) struct InlayText {
    pub(super) position: ByteOffset,
    pub(super) label: String,
    pub(super) style: crate::ui::Style,
    pub(super) ghost: bool,
}

/// Hints inside a source grapheme are drawn immediately after it. Scan each hinted line once.
pub(super) fn projected_inlay_hints(
    state: &EditorState,
    view: crate::editor::ViewId,
    ui: &UiState,
) -> Vec<InlayText> {
    let buffer = state.views[view].buffer;
    let text = &state.buffers[buffer].text;
    let mut hints = ui
        .lsp_inlay_hints
        .iter()
        .chain(&ui.substitute_preview)
        .filter(|hint| hint.buffer == buffer && hint.revision == text.revision())
        .take(10_000)
        .map(|hint| InlayText {
            position: hint.position,
            label: hint.label.clone(),
            style: crate::ui::Style::Muted,
            ghost: false,
        })
        .collect::<Vec<_>>();
    for (range, decoration) in state.buffers[buffer].decorations.drawing() {
        if let Some(text) = &decoration.virtual_text
            && text.position == crate::editor::decorations::VirtualTextPosition::Inline
        {
            hints.push(InlayText {
                position: range.start,
                label: text.text.text.clone(),
                style: crate::ui::render::decorations::style(&text.text.style),
                ghost: false,
            });
        }
    }
    if let Some(ghost) = crate::ui::render::decorations::ghost(state, view, ui) {
        hints.push(InlayText {
            position: ghost.position,
            label: ghost.label.lines().next().unwrap_or_default().into(),
            style: crate::ui::Style::Muted,
            ghost: true,
        });
    }
    hints.sort_by_key(|hint| hint.position);
    let mut projected = Vec::with_capacity(hints.len());
    for group in hints
        .chunk_by(|a, b| text.byte_to_line(a.position).ok() == text.byte_to_line(b.position).ok())
    {
        let Ok(line) = text.byte_to_line(group[0].position) else {
            continue;
        };
        let Ok(start) = text.line_to_byte(line) else {
            continue;
        };
        let Ok(source) = text.line(line) else {
            continue;
        };
        let mut boundaries = source
            .grapheme_indices(true)
            .map(|(at, _)| at)
            .chain(std::iter::once(source.len()))
            .peekable();
        for hint in group {
            let relative = hint.position.get().saturating_sub(start.get());
            while boundaries.peek().is_some_and(|at| *at < relative) {
                boundaries.next();
            }
            let mut hint = hint.clone();
            hint.position =
                ByteOffset(start.get() + boundaries.peek().copied().unwrap_or(source.len()));
            projected.push(hint);
        }
    }
    projected
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
    rows: RefCell<BTreeMap<usize, Vec<VisualRowSlice>>>,
    ghost_width: Option<(ByteOffset, usize)>,
    diagnostic_rows: Option<(usize, Vec<(String, crate::ui::Style)>)>,
    /// Rendered markdown, so rows break and cells are counted where it is drawn.
    markdown: Option<crate::ui::render::markdown::MarkdownMarks<'a>>,
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
        ui: &'a UiState,
        config: &'a Config,
        pane_width: u16,
        wrap: bool,
    ) -> Self {
        let mut inlay_widths = BTreeMap::<ByteOffset, usize>::new();
        let mut labels = BTreeMap::<ByteOffset, Vec<&str>>::new();
        let hints = state
            .views
            .get(view)
            .map_or_else(Vec::new, |_| projected_inlay_hints(state, view, ui));
        {
            let mut per_line = BTreeMap::<usize, usize>::new();
            for hint in &hints {
                let position = hint.position;
                let Ok(line) = text.byte_to_line(position) else {
                    continue;
                };
                let count = per_line.entry(line).or_default();
                if *count >= 32 {
                    continue;
                }
                *count = count.saturating_add(1);
                labels.entry(position).or_default().push(&hint.label);
                let width = cell_text_width(&hint.label, config.editor.ambiguous_width);
                inlay_widths
                    .entry(position)
                    .and_modify(|current| *current = current.saturating_add(width))
                    .or_insert(width);
            }
        }
        let content_width = usize::from(pane_width.saturating_sub(editor_state_gutter_width(
            state,
            text,
            &config.editor,
            pane_width,
        )))
        .max(1);
        for (position, width) in &mut inlay_widths {
            if !wrap {
                continue;
            }
            let line = text.byte_to_line(*position).unwrap_or(0);
            let line_start = text.line_to_byte(line).unwrap_or(ByteOffset(0));
            let source = text.line(line).unwrap_or_default();
            let relative = position.get().saturating_sub(line_start.get());
            let source_width = source
                .get(relative..)
                .unwrap_or_default()
                .graphemes(true)
                .next()
                .map_or(0, |g| {
                    if g == "\t" {
                        let tab_width = config.editor.tab_width.max(1);
                        let column = display_column_at(
                            &source,
                            relative,
                            DisplayWidthPolicy {
                                tab_width,
                                ambiguous_wide: config.editor.ambiguous_width
                                    == AmbiguousWidth::Wide,
                            },
                        );
                        tab_width - (column % tab_width)
                    } else {
                        text_width(g, config.editor.ambiguous_width).max(1)
                    }
                });
            let limit = content_width.saturating_sub(source_width);
            let mut used = 0;
            for label in &labels[position] {
                for grapheme in label.graphemes(true) {
                    let cells = cell_text_width(grapheme, config.editor.ambiguous_width);
                    if used + cells > limit {
                        break;
                    }
                    used += cells;
                }
            }
            *width = used;
        }
        let mut model = Self::new_with_minimum_digits(VisualRowConstruction {
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            minimum_digits: editor_line_number_digits(state),
            inlay_widths,
        });
        model.ghost_width = hints.iter().find(|hint| hint.ghost).map(|ghost| {
            let before = hints
                .iter()
                .filter(|hint| hint.position == ghost.position && !hint.ghost)
                .map(|hint| cell_text_width(&hint.label, config.editor.ambiguous_width))
                .sum::<usize>();
            (
                ghost.position,
                model.inlay_width(ghost.position).saturating_sub(before),
            )
        });
        model.markdown = state.views.get(view).and_then(|view| {
            crate::ui::render::markdown::MarkdownMarks::new(
                ui,
                view,
                text,
                &config.editor,
                model.content_width,
            )
        });
        if let Some(buffer) = state
            .views
            .get(view)
            .and_then(|view| state.buffers.get(view.buffer))
            && !ui.large_file_buffers.contains(&state.views[view].buffer)
        {
            model.diagnostic_rows = crate::ui::render::editor::expanded_diagnostic_rows(
                text,
                state.views[view].buffer,
                buffer.path.as_deref(),
                ui,
                config,
                model.content_width as u16,
            );
        }
        model
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
            if let Some(last) = normalized_folds.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
                continue;
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
            rows: RefCell::default(),
            ghost_width: None,
            diagnostic_rows: None,
            markdown: None,
        }
    }

    /// What a markdown line draws in place of its source, `None` when it shows its source.
    pub(super) fn markdown_line(
        &self,
        line: usize,
    ) -> Option<crate::ui::render::markdown::MarkdownLine<'a>> {
        self.markdown.as_ref()?.line(line)
    }

    #[must_use]
    pub fn is_table_line(&self, line: usize) -> bool {
        self.markdown_line(line)
            .is_some_and(|markdown| markdown.table.is_some())
    }

    /// The cells a grapheme takes as drawn, with what markdown draws before it.
    fn drawn_width(
        &self,
        markdown: Option<&crate::ui::render::markdown::MarkdownLine>,
        at: ByteOffset,
        grapheme: &str,
        source_column: usize,
    ) -> usize {
        let source = self.grapheme_width(grapheme, source_column);
        markdown.map_or(source, |markdown| {
            markdown.drawn_width(at, source, self.ambiguous_width)
        })
    }

    pub(super) fn diagnostic_rows(&self, line: usize) -> &[(String, crate::ui::Style)] {
        self.diagnostic_rows
            .as_ref()
            .filter(|(at, _)| *at == line)
            .map_or(&[], |(_, rows)| rows.as_slice())
    }

    #[must_use]
    pub fn painted_row(&self, start: VisualRowAnchor, mut row: usize) -> Option<VisualRowAnchor> {
        let mut current = self.normalize(start);
        loop {
            if row == 0 {
                return Some(current);
            }
            let next = self.next(current);
            let extra = if next.line != current.line || next == current {
                self.diagnostic_rows(current.line).len()
            } else {
                0
            };
            if row <= extra {
                return Some(current);
            }
            if next == current {
                return None;
            }
            row -= 1 + extra;
            current = next;
        }
    }

    /// Source text actually painted in the viewport, excluding folds and clipped columns.
    #[must_use]
    pub fn visible_text_ranges(
        &self,
        start: VisualRowAnchor,
        height: usize,
        scroll_column: usize,
    ) -> Vec<ByteRange> {
        let mut ranges: Vec<ByteRange> = Vec::new();
        let mut previous = None;
        for row in 0..height {
            let Some(anchor) = self.painted_row(start, row) else {
                break;
            };
            if previous == Some(anchor) {
                continue;
            }
            previous = Some(anchor);
            let line_start = self.text.line_to_byte(anchor.line).unwrap_or(ByteOffset(0));
            let line = self.text.line(anchor.line).unwrap_or_default();
            for relative in self.visible_bytes(anchor, scroll_column, self.content_width) {
                let at = ByteOffset(line_start.get() + relative);
                let length = line[relative..].graphemes(true).next().map_or(0, str::len);
                let end = ByteOffset(at.get() + length);
                if let Some(last) = ranges.last_mut()
                    && last.end == at
                {
                    last.end = end;
                } else {
                    ranges.push(ByteRange::new(at, end).expect("visible text range"));
                }
            }
        }
        ranges
    }

    pub(super) fn inlay_width(&self, position: ByteOffset) -> usize {
        self.inlay_widths.get(&position).copied().unwrap_or(0)
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
        self.rows
            .borrow_mut()
            .entry(line)
            .or_insert_with(|| self.compute_line_rows(line))
            .clone()
    }

    fn compute_line_rows(&self, line: usize) -> Vec<VisualRowSlice> {
        let text = self.text.line(line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let markdown = self.markdown_line(line);
        // A table row is drawn as a block, each of its rows showing part of every cell, whether
        // or not lines wrap.
        if let Some(table) = markdown
            .as_ref()
            .and_then(|markdown| markdown.table.as_ref())
        {
            return table
                .frames
                .iter()
                .map(|frame| VisualRowSlice {
                    byte_start: 0,
                    byte_end: content.len(),
                    source_column_start: 0,
                    used_cells: text_width(frame, self.ambiguous_width).min(self.content_width),
                })
                .collect();
        }
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
            let visible_width = self
                .drawn_width(
                    markdown.as_ref(),
                    ByteOffset(line_start.get().saturating_add(relative)),
                    grapheme,
                    source_column,
                )
                .min(self.content_width);
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
            .saturating_add(
                markdown
                    .as_ref()
                    .and_then(|markdown| markdown.after.as_ref())
                    .map_or(0, |(after, _)| text_width(after, self.ambiguous_width)),
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
        let markdown = self.markdown_line(line);
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
            let visible_width = self
                .drawn_width(
                    markdown.as_ref(),
                    ByteOffset(line_start.get().saturating_add(relative)),
                    grapheme,
                    source_column,
                )
                .min(self.content_width);
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
        self.byte_at_scrolled_cell(anchor, cell, 0)
    }

    /// Hit test the same source slice the renderer uses after horizontal scrolling.
    #[must_use]
    pub fn byte_at_scrolled_cell(
        &self,
        anchor: VisualRowAnchor,
        cell: usize,
        scroll_column: usize,
    ) -> usize {
        let anchor = self.normalize(anchor);
        let mut row = self.row_slice(anchor);
        let text = self.text.line(anchor.line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let line_start = self.text.line_to_byte(anchor.line).unwrap_or(ByteOffset(0));
        let markdown = self.markdown_line(anchor.line);
        if let Some(markdown) = &markdown
            && let Some(table) = &markdown.table
        {
            return content
                .grapheme_indices(true)
                .filter_map(|(relative, grapheme)| {
                    let at = ByteOffset(line_start.get() + relative);
                    let (wrap_row, column) = table.place(at)?;
                    let width = markdown.drawn_width(
                        at,
                        cell_text_width(grapheme, self.ambiguous_width),
                        self.ambiguous_width,
                    );
                    if width == 0 {
                        return None;
                    }
                    let distance = column
                        .saturating_sub(cell)
                        .max(cell.saturating_sub(column.saturating_add(width).saturating_sub(1)));
                    Some(((wrap_row.abs_diff(anchor.wrap_row), distance), relative))
                })
                .min_by_key(|(distance, _)| *distance)
                .map_or(row.byte_start, |(_, relative)| relative);
        }
        if !self.wrap {
            row.byte_start = crate::ui::render::editor::byte_offset_at_display_column(
                content,
                scroll_column,
                self.tab_width,
                self.ambiguous_width,
            );
            row.source_column_start = display_column(
                &content[..row.byte_start],
                self.tab_width,
                self.ambiguous_width,
            );
        }
        let mut source_column = row.source_column_start;
        let mut used = 0usize;
        let mut last_grapheme = row.byte_start;
        for (relative, grapheme) in content[row.byte_start..end].grapheme_indices(true) {
            let relative = row.byte_start.saturating_add(relative);
            last_grapheme = relative;
            let virtual_width = self
                .inlay_width(ByteOffset(line_start.get() + relative))
                .min(self.content_width);
            if cell < used.saturating_add(virtual_width) {
                return relative;
            }
            used = used.saturating_add(virtual_width);
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = self
                .drawn_width(
                    markdown.as_ref(),
                    ByteOffset(line_start.get() + relative),
                    grapheme,
                    source_column,
                )
                .min(self.content_width);
            if cell < used.saturating_add(visible_width) {
                return relative;
            }
            used = used.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        if row.byte_end < content.len() {
            last_grapheme
        } else {
            row.byte_end
        }
    }

    /// Source graphemes that can receive an overlay in this painted row. Horizontal scrolling
    /// skips source columns before drawing inlays, just as the editor renderer does.
    #[must_use]
    pub fn visible_bytes(
        &self,
        anchor: VisualRowAnchor,
        scroll_column: usize,
        width: usize,
    ) -> Vec<usize> {
        let anchor = self.normalize(anchor);
        let row = self.row_slice(anchor);
        let text = self.text.line(anchor.line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let line_start = self.text.line_to_byte(anchor.line).unwrap_or(ByteOffset(0));
        let markdown = self.markdown_line(anchor.line);
        let table = markdown.as_ref().and_then(|line| line.table.as_ref());
        let first = if self.wrap || table.is_some() {
            row.byte_start
        } else {
            crate::ui::render::editor::byte_offset_at_display_column(
                content,
                scroll_column,
                self.tab_width,
                self.ambiguous_width,
            )
        };
        let mut source_column =
            display_column(&content[..first], self.tab_width, self.ambiguous_width);
        let mut used = 0usize;
        let mut bytes = Vec::new();
        for (relative, grapheme) in content[first..end].grapheme_indices(true) {
            let relative = first + relative;
            let at = ByteOffset(line_start.get() + relative);
            if let Some(table) = table {
                let Some((painted_row, column)) = table.place(at) else {
                    continue;
                };
                if painted_row != anchor.wrap_row {
                    continue;
                }
                used = column;
            }
            used = used.saturating_add(self.inlay_width(at));
            if let Some(markdown) = &markdown {
                for (text, _) in markdown.before(at) {
                    used = used.saturating_add(text_width(text, self.ambiguous_width));
                }
                match markdown.at(at) {
                    Some(crate::ui::render::markdown::Drawn::Hidden) => continue,
                    Some(crate::ui::render::markdown::Drawn::Glyph(glyph, _)) => {
                        used = used.saturating_add(text_width(glyph, self.ambiguous_width));
                        continue;
                    }
                    None => {}
                }
            }
            let source_width = self.grapheme_width(grapheme, source_column);
            if used >= width || (!self.wrap && used.saturating_add(source_width) > width) {
                if table.is_some() {
                    continue;
                }
                break;
            }
            bytes.push(relative);
            used = used.saturating_add(source_width.min(self.content_width));
            source_column = source_column.saturating_add(source_width);
        }
        bytes
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
        if !self.wrap && self.markdown.is_none() {
            return 1;
        }
        self.rows
            .borrow_mut()
            .entry(line)
            .or_insert_with(|| self.compute_line_rows(line))
            .len()
    }

    pub(super) fn next_line(&self, line: usize) -> usize {
        let folded_end = self
            .folds
            .iter()
            .find_map(|&(start, end)| (start == line).then_some(end))
            .unwrap_or(line);
        if folded_end >= self.text.len_lines().saturating_sub(1) {
            line
        } else {
            folded_end + 1
        }
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
    pub fn forward(&self, mut anchor: VisualRowAnchor, mut count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        while count > 0 {
            let next = self.next(anchor);
            let step = 1 + if next.line != anchor.line {
                self.diagnostic_rows(anchor.line).len()
            } else {
                0
            };
            if next == anchor || step > count {
                break;
            }
            count -= step;
            anchor = next;
        }
        anchor
    }

    #[must_use]
    pub fn backward(&self, mut anchor: VisualRowAnchor, mut count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        while count > 0 {
            let previous = self.previous(anchor);
            let step = 1 + if previous.line != anchor.line {
                self.diagnostic_rows(previous.line).len()
            } else {
                0
            };
            if previous == anchor || step > count {
                break;
            }
            count -= step;
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
            self.row_cell_for_relative_byte(line, rows[row_index], relative)
                .saturating_sub(
                    self.ghost_width
                        .filter(|(at, _)| *at == cursor)
                        .map_or(0, |(_, width)| width),
                ),
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
        let mut row = 0;
        while row < height {
            if current == target {
                return Some(row);
            }
            let next = self.next(current);
            if next == current {
                break;
            }
            row += 1 + if next.line != current.line {
                self.diagnostic_rows(current.line).len()
            } else {
                0
            };
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
    column < indentation && column.is_multiple_of(indent_width)
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
        .saturating_add(text_width(LINE_NUMBER_SEPARATOR, config.ambiguous_width))
        .saturating_add(usize::from(config.git_signs.enabled))
        .saturating_add(usize::from(GUTTER_PADDING))
}

pub(super) fn git_sign_column(gutter_width: u16, config: &EditorConfig) -> Option<u16> {
    (config.git_signs.enabled && gutter_width > 0).then(|| {
        let trailing_padding = GUTTER_PADDING.min(gutter_width.saturating_sub(1));
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

#[cfg(test)]
mod tests {
    use crate::ui::render::visual_rows::*;

    #[test]
    fn visible_cells_return_whole_graphemes_across_tabs_and_wrapped_rows() {
        let text = crate::editor::text::TextBuffer::from_text("a\t界e\u{301}👩‍💻z");
        let mut config = Config::default();
        config.editor.line_numbers = LineNumbers::Off;
        config.editor.tab_width = 4;
        let ui = UiState::default();
        let model = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            6,
            false,
        );
        let start = VisualRowAnchor::default();
        assert_eq!(model.visible_bytes(start, 2, 5), vec![1, 2]);
        assert_eq!(model.visible_bytes(start, 6, 4), vec![5, 8, 19]);
        assert!(model.visible_bytes(start, 20, 11).is_empty());
        assert_eq!(model.byte_at_cell(start, 20), text.len_bytes());
        let wrapped = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            6,
            true,
        );
        assert_eq!(wrapped.visible_bytes(start, 0, 6), vec![0, 1, 2]);
        assert_eq!(
            wrapped.visible_bytes(
                VisualRowAnchor {
                    line: 0,
                    wrap_row: 1
                },
                0,
                6
            ),
            vec![5, 8, 19]
        );
    }

    #[test]
    fn visible_source_excludes_inlays_and_clipped_wide_graphemes() {
        let text = crate::editor::text::TextBuffer::from_text("a界bcd");
        let mut config = Config::default();
        config.editor.line_numbers = LineNumbers::Off;
        let ui = UiState::default();
        let mut model = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            4,
            false,
        );
        let start = VisualRowAnchor::default();
        // A wide grapheme that does not fit is not drawn at all.
        assert_eq!(model.visible_bytes(start, 0, 2), vec![0]);
        model.inlay_widths.insert(ByteOffset(0), 4);
        assert!(model.visible_bytes(start, 0, 4).is_empty());
        // Scrolling past the source character skips its inlay too.
        assert_eq!(model.visible_bytes(start, 1, 4), vec![1, 4, 5]);
        model.inlay_widths.insert(ByteOffset(4), 4);
        assert_eq!(model.visible_bytes(start, 1, 4), vec![1]);
        // Hit testing still maps an inlay to its associated source character.
        assert_eq!(model.byte_at_cell(start, 2), 0);
    }

    #[test]
    fn visible_source_excludes_replaced_markdown() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(
                crate::editor::text::TextBuffer::from_text("- x [y](url)\nlast"),
                None,
            )
            .unwrap();
        state.views[view].cursor = ByteOffset(13);
        let mut config = Config::default();
        config.editor.line_numbers = LineNumbers::Off;
        config.editor.markdown.render = true;
        let mut ui = UiState::default();
        for (start, end, mark) in [
            (0, 1, crate::language::syntax::MarkdownMark::Bullet(0)),
            (6, 12, crate::language::syntax::MarkdownMark::Hide),
        ] {
            ui.markdown_marks.push(crate::ui::UiMarkdownMark {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
                mark,
            });
        }
        let model = VisualRowModel::new_for_state(
            &state,
            &state.buffers[buffer].text,
            view,
            &ui,
            &config,
            20,
            false,
        );
        assert_eq!(
            model.visible_bytes(VisualRowAnchor::default(), 0, 20),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn visible_ranges_exclude_hidden_markdown_and_unpainted_table_cells() {
        use crate::editor::text::TextBuffer;
        use crate::language::syntax::MarkdownMark;
        use crate::ui::UiMarkdownMark;
        let source =
            "cursor\nz hidden\n| a | long text here and more |\n|---|---|\n| b | c |\nlast";
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text(source), None)
            .unwrap();
        let text = &state.buffers[buffer].text;
        let revision = text.revision();
        let mut config = crate::config::load_bundled_config().config;
        config.editor.line_numbers = LineNumbers::Off;
        config.editor.git_signs.enabled = false;
        let mut ui = UiState::default();
        let mark = |start, end, mark| UiMarkdownMark {
            buffer,
            revision,
            range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
            mark,
        };
        let table_start = source.find('|').unwrap();
        let table_end = source.find("last").unwrap();
        ui.markdown_marks = source
            .match_indices('|')
            .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
            .collect();
        ui.markdown_marks.push(mark(7, 15, MarkdownMark::Hide));
        ui.markdown_marks
            .push(mark(table_start, table_end, MarkdownMark::Table));
        ui.markdown_marks.sort_by_key(|mark| mark.range.start);
        for wrap in [true, false] {
            let model = VisualRowModel::new_for_state(&state, text, view, &ui, &config, 20, wrap);
            assert!(
                model
                    .visible_text_ranges(
                        VisualRowAnchor {
                            line: 1,
                            wrap_row: 0
                        },
                        1,
                        0
                    )
                    .is_empty()
            );
            let visible = model.visible_text_ranges(
                VisualRowAnchor {
                    line: 2,
                    wrap_row: 0,
                },
                2,
                0,
            );
            let painted = visible
                .iter()
                .map(|range| text.slice(*range).unwrap())
                .collect::<String>();
            assert!(painted.contains('a'), "{painted:?}");
            assert!(painted.contains("long text"), "{painted:?}");
            assert!(!painted.contains("more"), "{painted:?}");
            assert!(!painted.contains('|'), "{painted:?}");
        }
    }

    #[test]
    fn paging_reuses_cached_line_slices() {
        let text = crate::editor::text::TextBuffer::from_text(&"a".repeat(1 << 20));
        let mut config = Config::default();
        config.editor.line_numbers = LineNumbers::Off;
        let ui = UiState::default();
        let model = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            80,
            true,
        );
        let last = model.forward(VisualRowAnchor::default(), 1000);
        assert_eq!(last.wrap_row, 1000);
        assert_eq!(model.rows.borrow().len(), 1);
        let pointer = model.rows.borrow()[&0].as_ptr();
        assert_eq!(model.forward(last, 1000).wrap_row, 2000);
        assert_eq!(model.rows.borrow()[&0].as_ptr(), pointer);
        let unwrapped = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            80,
            false,
        );
        assert_eq!(unwrapped.rows_for_line(0), 1);
        assert!(unwrapped.rows.borrow().is_empty());
    }
    #[test]
    fn wrapped_row_padding_hits_stay_on_the_clicked_row() {
        let text = crate::editor::text::TextBuffer::from_text("abc界");
        let mut config = Config::default();
        config.editor.line_numbers = LineNumbers::Off;
        let ui = UiState::default();
        let model = VisualRowModel::new(
            &text,
            crate::editor::ViewId::default(),
            &ui,
            &config,
            4,
            true,
        );
        assert_eq!(model.byte_at_cell(VisualRowAnchor::default(), 3), 2);
        let second = VisualRowAnchor {
            line: 0,
            wrap_row: 1,
        };
        assert_eq!(model.byte_at_cell(second, 0), 3);
        assert_eq!(model.byte_at_cell(second, 1), 3);
        assert_eq!(model.byte_at_cell(second, 3), text.len_bytes());
    }
}
