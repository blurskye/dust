//! Display columns and text width.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Deterministic terminal-cell policy shared by editing range construction and
/// terminal projection. Columns are zero-based display cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayWidthPolicy {
    pub tab_width: usize,
    pub ambiguous_wide: bool,
}

impl Default for DisplayWidthPolicy {
    fn default() -> Self {
        Self {
            tab_width: 4,
            ambiguous_wide: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub column_start: usize,
    pub column_end: usize,
}

#[must_use]
pub fn grapheme_cell_width(grapheme: &str, column: usize, policy: DisplayWidthPolicy) -> usize {
    if grapheme == "\t" {
        return policy.tab_width.max(1) - (column % policy.tab_width.max(1));
    }
    let width = if policy.ambiguous_wide {
        UnicodeWidthStr::width_cjk(grapheme)
    } else {
        UnicodeWidthStr::width(grapheme)
    };
    width.max(1)
}

#[must_use]
pub fn display_spans(text: &str, policy: DisplayWidthPolicy) -> Vec<DisplaySpan> {
    let mut column = 0_usize;
    text.grapheme_indices(true)
        .map(|(byte_start, grapheme)| {
            let width = grapheme_cell_width(grapheme, column, policy);
            let span = DisplaySpan {
                byte_start,
                byte_end: byte_start.saturating_add(grapheme.len()),
                column_start: column,
                column_end: column.saturating_add(width),
            };
            column = span.column_end;
            span
        })
        .collect()
}

/// Display column at a UTF-8/grapheme boundary. Invalid interior offsets snap
/// to the start of the containing grapheme rather than splitting it.
#[must_use]
pub fn display_column_at(text: &str, byte_offset: usize, policy: DisplayWidthPolicy) -> usize {
    if byte_offset == 0 {
        return 0;
    }
    let mut column = 0_usize;
    for (byte_start, grapheme) in text.grapheme_indices(true) {
        if byte_offset < byte_start.saturating_add(grapheme.len()) {
            return column;
        }
        column = column.saturating_add(grapheme_cell_width(grapheme, column, policy));
        if byte_offset == byte_start.saturating_add(grapheme.len()) {
            return column;
        }
    }
    column
}

/// Return the whole-grapheme byte range intersecting an inclusive display-cell
/// rectangle. Tabs and wide glyphs are selected atomically.
#[must_use]
pub fn byte_range_for_display_columns(
    text: &str,
    first_column: usize,
    last_column: usize,
    policy: DisplayWidthPolicy,
) -> Option<(usize, usize)> {
    let low = first_column.min(last_column);
    let high_exclusive = first_column.max(last_column).saturating_add(1);
    let mut start = None;
    let mut end = None;
    for span in display_spans(text, policy) {
        if span.column_end > low && span.column_start < high_exclusive {
            start.get_or_insert(span.byte_start);
            end = Some(span.byte_end);
        }
    }
    start.zip(end)
}

/// Byte insertion point for a requested display column. If the column falls
/// inside a tab or wide grapheme, insertion occurs before that grapheme. The
/// second result is the number of spaces needed when the line is too short.
#[must_use]
pub fn insertion_at_display_column(
    text: &str,
    target_column: usize,
    policy: DisplayWidthPolicy,
) -> (usize, usize) {
    let spans = display_spans(text, policy);
    for span in &spans {
        if target_column <= span.column_start {
            return (span.byte_start, 0);
        }
        if target_column < span.column_end {
            return (span.byte_start, 0);
        }
    }
    let end_column = spans.last().map_or(0, |span| span.column_end);
    (text.len(), target_column.saturating_sub(end_column))
}

#[cfg(test)]
mod tests {
    use crate::editor::text::display::DisplayWidthPolicy;
    use crate::editor::text::display::byte_range_for_display_columns;
    use crate::editor::text::display::display_column_at;
    use crate::editor::text::display::insertion_at_display_column;

    #[test]
    fn display_column_lookup_handles_long_lines_and_interior_offsets() {
        let policy = DisplayWidthPolicy::default();
        let text = format!("\t界e\u{301}{}", "x".repeat(1_000_000));
        assert_eq!(display_column_at(&text, 0, policy), 0);
        assert_eq!(display_column_at(&text, 2, policy), 4);
        assert_eq!(display_column_at(&text, 6, policy), 6);
        assert_eq!(display_column_at(&text, 7, policy), 7);
        assert_eq!(display_column_at("\t界", usize::MAX, policy), 6);
    }

    #[test]
    fn display_ranges_never_split_tabs_wide_or_combining_graphemes() {
        let policy = DisplayWidthPolicy {
            tab_width: 4,
            ambiguous_wide: false,
        };
        let text = "\t界e\u{301}x";
        assert_eq!(display_column_at(text, 1, policy), 4);
        assert_eq!(
            byte_range_for_display_columns(text, 2, 2, policy),
            Some((0, 1))
        );
        assert_eq!(
            byte_range_for_display_columns(text, 4, 4, policy),
            Some((1, 4))
        );
        assert_eq!(
            byte_range_for_display_columns(text, 6, 6, policy),
            Some((4, 7))
        );
        assert_eq!(insertion_at_display_column(text, 3, policy), (0, 0));
        assert_eq!(insertion_at_display_column("x", 3, policy), (1, 2));

        let wide_ambiguous = DisplayWidthPolicy {
            tab_width: 4,
            ambiguous_wide: true,
        };
        assert_eq!(display_column_at("·x", "·".len(), wide_ambiguous), 2);
        assert_eq!(
            byte_range_for_display_columns("·x", 1, 1, wide_ambiguous),
            Some((0, "·".len()))
        );
    }
    #[test]
    fn display_columns_stream_prefixes_and_snap_inside_graphemes() {
        let policy = DisplayWidthPolicy::default();
        let text = format!("\t界e\u{301}{}", "x".repeat(1 << 20));
        for (offset, column) in [(0, 0), (1, 4), (2, 4), (4, 6), (5, 6), (6, 6), (7, 7)] {
            assert_eq!(display_column_at(&text, offset, policy), column);
        }
        assert_eq!(display_column_at("x", usize::MAX, policy), 1);
    }
}
