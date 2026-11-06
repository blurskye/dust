//! Picker items, fuzzy ranking and streamed results.

use std::collections::HashSet;

use crate::editor::text::ByteRange;

use crate::workspace::ABSOLUTE_PICKER_ITEM_LIMIT;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PickerItemId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerAction {
    OpenLocation { uri: String, range: ByteRange },
    InvokeCommand { command: String },
    ApplyCodeAction { index: usize },
    None,
}

/// What a picker row is. A section header groups the entries under it and is never an action
/// candidate: selection moves over entries only, so a header can never be accepted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PickerItemKind {
    #[default]
    Entry,
    Section,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerItem {
    pub id: PickerItemId,
    pub label: String,
    pub detail: String,
    pub source: String,
    /// A short badge for a row that has one, such as a path whose buffer has unsaved edits.
    /// Empty means no badge.
    pub badge: String,
    pub action: PickerAction,
    pub kind: PickerItemKind,
}

/// One row of a picker presentation: the item's identity, its three text fields and what kind
/// of row it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerRowPresentation {
    pub id: PickerItemId,
    pub label: String,
    pub detail: String,
    pub source: String,
    pub badge: String,
    pub kind: PickerItemKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerPresentation {
    pub title: String,
    pub query: String,
    pub rows: Vec<PickerRowPresentation>,
    /// Entries before the query filters them.
    pub total: usize,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
}

#[derive(Clone, Debug)]
pub struct PickerState {
    stream_id: u64,
    title: String,
    query: String,
    items: Vec<PickerItem>,
    visible: Vec<usize>,
    selected: Option<usize>,
    scroll: usize,
    maximum_items: usize,
    loading: bool,
}

impl PickerState {
    #[must_use]
    pub fn new(stream_id: u64, title: String, maximum_items: usize) -> Self {
        Self {
            stream_id,
            title,
            query: String::new(),
            items: Vec::new(),
            visible: Vec::new(),
            selected: None,
            scroll: 0,
            maximum_items: maximum_items.clamp(1, ABSOLUTE_PICKER_ITEM_LIMIT),
            loading: true,
        }
    }

    #[must_use]
    pub const fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub fn append(
        &mut self,
        stream_id: u64,
        items: impl IntoIterator<Item = PickerItem>,
        complete: bool,
    ) -> bool {
        if stream_id != self.stream_id {
            return false;
        }
        let remaining = self.maximum_items.saturating_sub(self.items.len());
        let mut known = self
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<HashSet<_>>();
        self.items.extend(
            items
                .into_iter()
                .filter(|item| known.insert(item.id.clone()))
                .take(remaining),
        );
        self.loading = !complete;
        self.recompute();
        true
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.recompute();
    }

    pub fn set_unfiltered_query(&mut self, query: String) {
        self.query = query;
        self.visible = (0..self.items.len()).collect();
        // The cursor opens on the first entry, never on a section header.
        self.selected = self.first_entry();
        self.scroll = 0;
    }

    /// Typing narrows to what is being looked for, so the best match is the one to accept.
    /// Deleting keeps the row, so clearing the query returns to where the list was.
    pub fn insert_query(&mut self, text: &str) {
        self.query.push_str(text);
        self.recompute();
        self.selected = self.first_entry();
        self.scroll = 0;
    }

    pub fn backspace_query(&mut self) {
        if let Some((last, _)) =
            unicode_segmentation::UnicodeSegmentation::grapheme_indices(self.query.as_str(), true)
                .next_back()
        {
            self.query.truncate(last);
        }
        self.recompute();
    }

    pub fn move_selection(&mut self, direction: i8, wrap: bool) {
        if self.visible.is_empty() {
            self.selected = None;
            return;
        }
        // A section header is never an action candidate, so selection steps over it. A
        // picker whose every row is a header has nothing to select.
        let entries: Vec<usize> = self
            .visible
            .iter()
            .enumerate()
            .filter(|(_, index)| {
                self.items
                    .get(**index)
                    .is_some_and(|item| item.kind == PickerItemKind::Entry)
            })
            .map(|(position, _)| position)
            .collect();
        let Some(&first) = entries.first() else {
            self.selected = None;
            return;
        };
        let Some(&last) = entries.last() else {
            self.selected = None;
            return;
        };
        let position = self
            .selected
            .filter(|selected| entries.contains(selected))
            .map_or_else(
                || {
                    entries
                        .iter()
                        .position(|entry| *entry > self.selected.unwrap_or(0))
                },
                |selected| {
                    let at = entries.iter().position(|entry| *entry == selected);
                    at.map(|at| {
                        if direction < 0 {
                            at.checked_sub(1)
                                .unwrap_or(if wrap { entries.len() - 1 } else { at })
                        } else if at + 1 < entries.len() {
                            at + 1
                        } else if wrap {
                            0
                        } else {
                            at
                        }
                    })
                },
            );
        self.selected = Some(match position {
            Some(at) => entries[at],
            None => {
                if direction < 0 {
                    last
                } else {
                    first
                }
            }
        });
    }

    #[must_use]
    pub fn selected_is_last(&self) -> bool {
        self.selected == self.visible.len().checked_sub(1)
    }

    pub fn select_last(&mut self) {
        self.selected = self.visible.len().checked_sub(1);
    }

    /// A row that has gone leaves the selection where the list put it, so there is still one.
    pub fn select_id(&mut self, id: &PickerItemId) {
        if let Some(position) = self
            .visible
            .iter()
            .position(|index| &self.items[*index].id == id)
        {
            self.selected = Some(position);
        }
    }

    pub fn ensure_selected_visible(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        let Some(selected) = self.selected else {
            self.scroll = 0;
            return;
        };
        if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll.saturating_add(visible_rows) {
            self.scroll = selected.saturating_add(1).saturating_sub(visible_rows);
        }
    }

    #[must_use]
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.selected
            .and_then(|selected| self.visible.get(selected))
            .and_then(|index| self.items.get(*index))
    }

    #[must_use]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    #[must_use]
    pub fn presentation(&self) -> PickerPresentation {
        PickerPresentation {
            title: self.title.clone(),
            query: self.query.clone(),
            rows: self
                .visible
                .iter()
                .filter_map(|index| self.items.get(*index))
                .map(|item| PickerRowPresentation {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    detail: item.detail.clone(),
                    source: item.source.clone(),
                    badge: item.badge.clone(),
                    kind: item.kind,
                })
                .collect(),
            total: self
                .items
                .iter()
                .filter(|item| item.kind == PickerItemKind::Entry)
                .count(),
            selected: self.selected,
            scroll: self.scroll,
            loading: self.loading,
        }
    }

    /// The first visible position whose row is an entry rather than a section header, so the
    /// cursor never opens on a header and a header can never be accepted.
    fn first_entry(&self) -> Option<usize> {
        self.visible.iter().position(|index| {
            self.items
                .get(*index)
                .is_some_and(|item| item.kind == PickerItemKind::Entry)
        })
    }

    fn recompute(&mut self) {
        if self.query.is_empty() {
            let selected_id = self.selected_item().map(|item| item.id.clone());
            self.visible = (0..self.items.len()).collect();
            self.selected = selected_id
                .and_then(|id| {
                    self.visible
                        .iter()
                        .position(|index| self.items[*index].id == id)
                })
                .or_else(|| self.first_entry());
            self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
            return;
        }
        let mut section = None;
        let mut scored = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if item.kind == PickerItemKind::Section {
                    section = Some(index);
                    return None;
                }
                fuzzy_score(&self.query, &item.label, &item.detail)
                    .map(|score| (section, index, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(
            |(left_section, left_index, left_score), (right_section, right_index, right_score)| {
                left_section
                    .cmp(right_section)
                    .then_with(|| right_score.cmp(left_score))
                    .then_with(|| {
                        self.items[*left_index]
                            .label
                            .cmp(&self.items[*right_index].label)
                    })
                    .then(left_index.cmp(right_index))
            },
        );
        let selected_id = self.selected_item().map(|item| item.id.clone());
        self.visible.clear();
        let mut previous_section = None;
        for (section, index, _) in scored {
            if section != previous_section {
                if let Some(header) = section {
                    self.visible.push(header);
                }
                previous_section = section;
            }
            self.visible.push(index);
        }
        self.selected = selected_id
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.items[*index].id == id)
            })
            .or_else(|| self.first_entry());
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
    }
}

/// How well `query` matches a row, `None` when it doesn't. Each word of the query must
/// match on its own, in any order, as in LazyVim's pickers: `tutor py` finds
/// `assets/defaults/tutor/python/practice.py`.
pub fn fuzzy_score(query: &str, label: &str, detail: &str) -> Option<i64> {
    let haystack = format!("{label} {detail}").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .map(|word| word_score(&haystack, word))
        .sum()
}

fn word_score(haystack: &str, query: &str) -> Option<i64> {
    let mut previous_end = None;
    // The query typed as one piece beats the same letters scattered across the row.
    let mut score = if haystack.contains(query) {
        1000_i64
    } else {
        0
    };
    let mut search_start = 0;
    for needle in query.chars() {
        let relative = haystack.get(search_start..)?.find(needle)?;
        let position = search_start + relative;
        score += 100 - i64::try_from(position).unwrap_or(100);
        if previous_end == Some(position) {
            score += 25;
        }
        search_start = position + needle.len_utf8();
        previous_end = Some(search_start);
    }
    Some(score)
}
