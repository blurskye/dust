//! Sidebar filters retain the unfiltered selection by Git identity, never by row number.
use super::*;
use std::collections::BTreeMap;

pub(super) struct ListFilter {
    pub input: PromptState,
    pub editing: bool,
    original: Option<PathBuf>,
    // Keep the last visible identity while a temporary query has no matches.
    last_match: Option<PathBuf>,
}
impl Default for ListFilter {
    fn default() -> Self {
        Self {
            input: PromptState::new(PromptKind::Text),
            editing: false,
            original: None,
            last_match: None,
        }
    }
}
impl ListFilter {
    pub fn active(&self) -> bool {
        !self.input.input.trim().is_empty()
    }
    pub(super) fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.input
            .input
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
    }
    pub fn rows(&self, rows: Vec<Row>) -> Vec<Row> {
        if !self.active() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| self.matches(&format!("{} {}", row.value.display(), row.label)))
            .collect()
    }
    pub fn tree(
        &self,
        mut paths: BTreeMap<PathBuf, String>,
        collapsed: &BTreeSet<PathBuf>,
    ) -> Vec<Row> {
        if self.active() {
            paths.retain(|path, _| self.matches(&path.to_string_lossy()));
            // Searching exposes matching files without changing the saved folds.
            tree::rows(paths, &BTreeSet::new())
        } else {
            tree::rows(paths, collapsed)
        }
    }
}

const COMMIT_FILES: usize = 9;

impl GitScreen {
    pub(super) fn filter_target(&self) -> Option<usize> {
        if self.page != 0 || self.focus >= 4 || self.form.is_some() {
            None
        } else if self.focus == 0 && self.last_list == 2 && self.commit.is_some() {
            Some(COMMIT_FILES)
        } else {
            Some(self.slot(self.focus))
        }
    }
    pub(super) fn list_filter(&self, target: usize) -> &ListFilter {
        if target == COMMIT_FILES {
            &self.commit.as_ref().expect("commit filter target").filter
        } else {
            &self.filters[target]
        }
    }
    pub(super) fn list_filter_mut(&mut self, target: usize) -> &mut ListFilter {
        if target == COMMIT_FILES {
            &mut self.commit.as_mut().expect("commit filter target").filter
        } else {
            &mut self.filters[target]
        }
    }
    fn filter_rows(&self, target: usize) -> memo::Rows {
        if target == COMMIT_FILES {
            self.commit.as_ref().expect("commit filter target").rows()
        } else {
            self.rows(Self::filter_pane(target))
        }
    }
    fn filter_pane(target: usize) -> usize {
        match target {
            0 | 2 | 7 => 0,
            1 | 3 | 4 => 1,
            5 | 8 => 2,
            _ => 3,
        }
    }
    fn filter_cursor(&self, target: usize) -> usize {
        if target == COMMIT_FILES {
            self.commit.as_ref().expect("commit filter target").cursor
        } else {
            self.selected[target]
        }
    }
    fn set_filter_cursor(&mut self, target: usize, cursor: usize) {
        if target == COMMIT_FILES {
            self.commit.as_mut().expect("commit filter target").cursor = cursor;
        } else {
            self.selected[target] = cursor;
            self.offsets[Self::filter_pane(target)] = 0;
        }
    }
    pub(super) fn filter_identity(&self, target: usize) -> Option<PathBuf> {
        self.filter_rows(target)
            .get(self.filter_cursor(target))
            .map(|row| row.value.clone())
    }
    fn filter_preview(&mut self, target: usize) {
        self.preview_offset = 0;
        self.preview_column = 0;
        self.preview_cursor = None;
        self.preview_anchor = None;
        if target == COMMIT_FILES {
            let commit = self.commit.as_mut().expect("commit filter target");
            if commit.rows().is_empty() {
                self.jobs.cancel_preview();
                self.preview = "No files match · / edit filter · Esc clear".into();
            } else {
                commit.select_row();
                self.load_commit_file();
            }
        } else {
            self.select_history();
            self.load_preview();
        }
    }
    pub(super) fn update_filter(&mut self, target: usize, previous: Option<PathBuf>) {
        if target == 0 {
            self.clear_file_selection();
        }
        let filter = self.list_filter(target);
        let desired = if filter.active() {
            [previous.as_ref(), filter.last_match.as_ref()]
        } else {
            [filter.original.as_ref(), None]
        };
        let rows = self.filter_rows(target);
        let cursor = desired
            .into_iter()
            .flatten()
            .find_map(|value| {
                rows.iter().position(|row| {
                    // Tree headings are context, not search results. Starting a
                    // search from a folded directory must select a matching file.
                    &row.value == value && (!filter.active() || !row.directory)
                })
            })
            .unwrap_or_else(|| rows.iter().position(|row| !row.directory).unwrap_or(0));
        if let Some(row) = rows.get(cursor)
            && !row.directory
        {
            self.list_filter_mut(target).last_match = Some(row.value.clone());
        }
        self.set_filter_cursor(target, cursor);
        self.filter_preview(target);
    }
    fn clear_filter(&mut self, target: usize) {
        let filter = self.list_filter_mut(target);
        filter.input.input.clear();
        filter.input.cursor = 0;
        filter.editing = false;
        self.update_filter(target, None);
        let filter = self.list_filter_mut(target);
        filter.original = None;
        filter.last_match = None;
        self.notice.clear();
    }
    pub(super) fn paste_filter(&mut self, text: &str) -> bool {
        let Some(target) = self.filter_target() else {
            return false;
        };
        if !self.list_filter(target).editing {
            return false;
        }
        let previous = self.filter_identity(target);
        self.remember_filter_match(target);
        self.list_filter_mut(target)
            .input
            .insert_text(&text.replace(['\r', '\n'], " "));
        self.update_filter(target, previous);
        true
    }
    pub(super) fn remember_filter_match(&mut self, target: usize) {
        if let Some(row) = self
            .filter_rows(target)
            .get(self.filter_cursor(target))
            .filter(|row| !row.directory)
        {
            self.list_filter_mut(target).last_match = Some(row.value.clone());
        }
    }
    pub(super) fn filter_key(&mut self, key: &KeyEvent) -> bool {
        let Some(target) = self.filter_target() else {
            return false;
        };
        if !self.list_filter(target).editing {
            if key.code == KeyCode::Escape && self.list_filter(target).active() {
                self.clear_filter(target);
                return true;
            }
            if key.code != KeyCode::Char('/') || key.modifiers.control || key.modifiers.alt {
                return false;
            }
            if target == 0 {
                self.clear_file_selection();
            }
            let original = self.filter_identity(target);
            self.remember_filter_match(target);
            let filter = self.list_filter_mut(target);
            if !filter.active() {
                filter.original = original.clone();
                filter.last_match = original;
            }
            filter.editing = true;
            filter.input.cursor = filter.input.input.len();
            self.notice.clear();
            return true;
        }
        // A modal screen's query edits on the editor's engine (`modal.rs`); Esc in its
        // Normal mode, or `q`, clears and closes the filter as Esc does here.
        let escape = KeyEvent::plain(KeyCode::Escape);
        let key = match self.modal_filter_key(target, key) {
            super::modal::BoxKey::Handled(_) => return true,
            super::modal::BoxKey::Close => &escape,
            super::modal::BoxKey::Pass => key,
        };
        if key.code == KeyCode::Escape {
            self.clear_filter(target);
            return true;
        }
        if key.code == KeyCode::Enter {
            let filter = self.list_filter_mut(target);
            filter.editing = false;
            if !filter.active() {
                filter.original = None;
            }
            return true;
        }
        if matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) {
            let delta = match key.code {
                KeyCode::Up => -1,
                KeyCode::Down => 1,
                KeyCode::PageUp => -15,
                _ => 15,
            };
            let cursor = self
                .filter_cursor(target)
                .saturating_add_signed(delta)
                .min(self.filter_rows(target).len().saturating_sub(1));
            self.set_filter_cursor(target, cursor);
            self.remember_filter_match(target);
            self.filter_preview(target);
            return true;
        }
        let previous = self.filter_identity(target);
        self.remember_filter_match(target);
        let input = &mut self.list_filter_mut(target).input;
        let mut changed = true;
        match key.code {
            KeyCode::Char('u') if key.modifiers.control => {
                input.input.clear();
                input.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.control && !key.modifiers.alt => {
                input.insert_text(&c.to_string())
            }
            KeyCode::Backspace => input.delete_previous_grapheme(),
            KeyCode::Left => {
                input.cursor = input.input[..input.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(at, _)| at);
                changed = false;
            }
            KeyCode::Right => {
                input.cursor += input.input[input.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len);
                changed = false;
            }
            KeyCode::Home => {
                input.cursor = 0;
                changed = false;
            }
            KeyCode::End => {
                input.cursor = input.input.len();
                changed = false;
            }
            _ => changed = false,
        }
        if changed {
            self.update_filter(target, previous);
        }
        true
    }
}
