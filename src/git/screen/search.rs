//! Literal, case-sensitive find within the currently loaded source, never Git metadata.
use super::*;
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub(super) struct SourceMatch {
    pub row: usize,
    pub bytes: Range<usize>,
}

/// Owned by Preview: replacing its content cannot leave matches from an old file/layer.
#[derive(Default)]
pub(super) struct SourceSearch {
    pub query: String,
    pub(in crate::git::screen) matches: Vec<SourceMatch>,
    pub(in crate::git::screen) current: Option<usize>,
}
impl SourceSearch {
    pub fn find(preview: &Preview, query: String) -> Self {
        Self::in_lines(
            preview.rows.iter().enumerate().filter_map(|(row, source)| {
                (source.old.is_some() || source.new.is_some())
                    .then_some((row, source.text.as_str()))
            }),
            query,
        )
    }

    /// Callers supply only actual source/prose, in row order, never UI chrome.
    pub fn in_lines<'a>(lines: impl IntoIterator<Item = (usize, &'a str)>, query: String) -> Self {
        let mut matches = Vec::new();
        if !query.is_empty() {
            for (row, text) in lines {
                matches.extend(text.match_indices(query.as_str()).map(|(start, text)| {
                    SourceMatch {
                        row,
                        bytes: start..start + text.len(),
                    }
                }));
            }
        }
        Self {
            query,
            matches,
            current: None,
        }
    }

    pub fn append_lines<'a>(&mut self, lines: impl IntoIterator<Item = (usize, &'a str)>) {
        self.matches
            .extend(Self::in_lines(lines, self.query.clone()).matches);
    }

    pub fn active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn selected(&self, cursor: Option<usize>) -> Option<&SourceMatch> {
        self.current
            .and_then(|index| self.matches.get(index))
            .filter(|found| Some(found.row) == cursor)
    }

    pub fn row_matches(&self, row: usize) -> &[SourceMatch] {
        let start = self.matches.partition_point(|found| found.row < row);
        let end = self.matches.partition_point(|found| found.row <= row);
        &self.matches[start..end]
    }

    pub fn leave_match(&mut self) {
        self.current = None;
    }

    pub fn current_match(&self) -> Option<&SourceMatch> {
        self.current.and_then(|index| self.matches.get(index))
    }

    pub fn position(&self, cursor: Option<usize>) -> (Option<usize>, usize) {
        (
            self.current
                .filter(|_| self.selected(cursor).is_some())
                .map(|index| index + 1),
            self.matches.len(),
        )
    }

    pub(super) fn step(
        &mut self,
        cursor: Option<usize>,
        backwards: bool,
        initial: bool,
    ) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let count = self.matches.len();
        let current = self.current.filter(|_| self.selected(cursor).is_some());
        let index = if let Some(current) = current.filter(|_| !initial) {
            if backwards {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            }
        } else if backwards {
            self.matches
                .iter()
                .rposition(|found| cursor.is_some_and(|row| found.row < row))
                .unwrap_or(count - 1)
        } else {
            self.matches
                .iter()
                .position(|found| {
                    cursor.is_none_or(|row| {
                        if initial {
                            found.row >= row
                        } else {
                            found.row > row
                        }
                    })
                })
                .unwrap_or(0)
        };
        self.current = Some(index);
        Some(self.matches[index].row)
    }
}

impl GitScreen {
    pub(super) fn search_key(&mut self, key: &KeyEvent) -> bool {
        if self.page != 0 || self.focus != 4 || key.modifiers.control || key.modifiers.alt {
            return false;
        }
        match key.code {
            KeyCode::Char('/') => {
                let mut query = Field::new(
                    "Query · literal, case-sensitive",
                    self.preview.search.query.clone(),
                );
                query.required = false;
                self.notice.clear();
                self.form = Some(Form {
                    title: "Find source".into(),
                    action: FormAction::SourceSearch,
                    fields: vec![query],
                    selected: 0,
                });
            }
            KeyCode::Char('n' | 'N') if self.preview.search.active() => {
                self.step_source_match(key.code == KeyCode::Char('N'), false);
            }
            KeyCode::Escape if self.preview.search.active() && self.preview_anchor.is_none() => {
                self.preview.search = SourceSearch::default();
            }
            _ => return false,
        }
        true
    }

    pub(super) fn find_source(&mut self, query: String) -> Result<(), String> {
        if self.page != 0 {
            return self.github.find_detail(query);
        }
        self.preview.search = SourceSearch::find(&self.preview, query);
        // Finding code is not a staging range gesture. Do not extend an old range
        // to the search result, or let the prompt authorize any repository write.
        self.preview_anchor = None;
        self.step_source_match(false, true);
        Ok(())
    }

    fn step_source_match(&mut self, backwards: bool, initial: bool) {
        if let Some(row) = self
            .preview
            .search
            .step(self.preview_cursor, backwards, initial)
        {
            self.preview_cursor = Some(row);
            self.preview.cursor_byte = self
                .preview
                .search
                .selected(Some(row))
                .map(|found| (row, found.bytes.start));
            self.preview_anchor = None;
        }
    }

    /// Same grapheme/tab columns as paint_source. The renderer supplies the actual
    /// code width after gutters (and, for split diffs, the relevant half-pane).
    pub(super) fn reveal_source_match(&mut self, available: usize) {
        let Some((row, byte)) = self
            .preview
            .cursor_byte
            .filter(|(row, _)| Some(*row) == self.preview_cursor)
        else {
            return;
        };
        let bytes = self
            .preview
            .search
            .selected(self.preview_cursor)
            .map_or(byte..byte + 1, |found| found.bytes.clone());
        let text = &self.preview.rows[row].text;
        let (mut column, mut start, mut end) = (0, None, 0);
        for (at, grapheme) in text.grapheme_indices(true) {
            let size = if grapheme == "\t" {
                4 - column % 4
            } else {
                grapheme.width().max(1)
            };
            if at < bytes.end && at + grapheme.len() > bytes.start {
                start.get_or_insert(column);
                end = column + size;
            }
            column += size;
        }
        let Some(start) = start else {
            return;
        };
        let available = available.max(2);
        if end <= available {
            self.preview_column = 0;
        } else if start < self.preview_column {
            self.preview_column = start;
        }
        let capacity = available.saturating_sub(usize::from(self.preview_column > 0));
        if end > self.preview_column.saturating_add(capacity) {
            self.preview_column = end.saturating_add(1).saturating_sub(available).min(start);
        }
    }

}
