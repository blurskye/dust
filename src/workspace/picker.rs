use super::*;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PickerItemId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerAction {
    OpenLocation { uri: String, range: ByteRange },
    InvokeCommand { command: String },
    ApplyCodeAction { index: usize },
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerItem {
    pub id: PickerItemId,
    pub label: String,
    pub detail: String,
    pub source: String,
    pub action: PickerAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerPresentation {
    pub title: String,
    pub query: String,
    pub rows: Vec<(PickerItemId, String, String, String)>,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
}

#[derive(Clone, Debug)]
pub struct PickerState {
    generation: u64,
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
    pub fn new(generation: u64, title: String, maximum_items: usize) -> Self {
        Self {
            generation,
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
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn append(
        &mut self,
        generation: u64,
        items: impl IntoIterator<Item = PickerItem>,
        complete: bool,
    ) -> bool {
        if generation != self.generation {
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
        self.selected = (!self.visible.is_empty()).then_some(0);
        self.scroll = 0;
    }

    pub fn insert_query(&mut self, text: &str) {
        self.query.push_str(text);
        self.recompute();
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
        let current = self.selected.unwrap_or(0).min(self.visible.len() - 1);
        self.selected = Some(if direction < 0 {
            if current == 0 && wrap {
                self.visible.len() - 1
            } else {
                current.saturating_sub(1)
            }
        } else if current + 1 == self.visible.len() && wrap {
            0
        } else {
            (current + 1).min(self.visible.len() - 1)
        });
    }

    #[must_use]
    pub fn selected_is_last(&self) -> bool {
        self.selected == self.visible.len().checked_sub(1)
    }

    pub fn select_last(&mut self) {
        self.selected = self.visible.len().checked_sub(1);
    }

    pub fn select_id(&mut self, id: &PickerItemId) {
        self.selected = self
            .visible
            .iter()
            .position(|index| &self.items[*index].id == id);
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
                .map(|item| {
                    (
                        item.id.clone(),
                        item.label.clone(),
                        item.detail.clone(),
                        item.source.clone(),
                    )
                })
                .collect(),
            selected: self.selected,
            scroll: self.scroll,
            loading: self.loading,
        }
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
                .or_else(|| (!self.visible.is_empty()).then_some(0));
            self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
            return;
        }
        let mut scored = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                fuzzy_score(&self.query, &item.label, &item.detail).map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(left_index, left_score), (right_index, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    self.items[*left_index]
                        .label
                        .cmp(&self.items[*right_index].label)
                })
                .then(left_index.cmp(right_index))
        });
        let selected_id = self.selected_item().map(|item| item.id.clone());
        self.visible = scored.into_iter().map(|(index, _)| index).collect();
        self.selected = selected_id
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.items[*index].id == id)
            })
            .or_else(|| (!self.visible.is_empty()).then_some(0));
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
    }
}

pub(super) fn fuzzy_score(query: &str, label: &str, detail: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let haystack = format!("{label} {detail}").to_lowercase();
    let mut previous = None;
    let mut score = 0_i64;
    let mut search_start = 0;
    for needle in query.to_lowercase().chars() {
        let relative = haystack.get(search_start..)?.find(needle)?;
        let position = search_start + relative;
        score += 100 - i64::try_from(position).unwrap_or(100);
        if previous.is_some_and(|previous| previous + needle.len_utf8() == position) {
            score += 25;
        }
        previous = Some(position);
        search_start = position + needle.len_utf8();
    }
    Some(score)
}
