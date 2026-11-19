//! Identity-bound file marks. Only expanded Changes rows participate in a range.
use super::*;

#[derive(Default)]
pub(super) struct FileSelection {
    pub marked: BTreeSet<PathBuf>,
    range: Option<FileRange>,
}
struct FileRange {
    anchor: PathBuf,
    previous: BTreeSet<PathBuf>,
}
impl FileSelection {
    pub fn active(&self) -> bool {
        self.range.is_some()
    }
}

impl GitScreen {
    pub(super) fn file_selection_context(&self) -> bool {
        self.page == 0
            && self.last_list == 0
            && self.files_view == 0
            && self.commit.is_none()
            && !self.all_files
    }

    pub(super) fn clear_file_selection(&mut self) {
        self.file_selection = FileSelection::default();
    }

    pub(super) fn finish_file_range(&mut self) {
        self.file_selection.range = None;
    }

    /// A refresh can remove/reorder rows, but must never extend a range or add targets.
    pub(super) fn reconcile_file_selection(&mut self) {
        self.finish_file_range();
        if !self.file_selection_context() {
            self.clear_file_selection();
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            self.file_selection.marked.retain(|path| {
                snapshot
                    .entries
                    .binary_search_by(|entry| entry.relative_path.cmp(path))
                    .is_ok()
                    && self.filters[0].matches(&path.to_string_lossy())
            });
        }
    }

    pub(super) fn file_selection_key(&mut self, key: &KeyEvent) -> bool {
        if self.focus != 0
            || !self.file_selection_context()
            || self.filters[0].editing
            || key.modifiers.control
            || key.modifiers.alt
        {
            return false;
        }
        if key.code == KeyCode::Escape && !self.file_selection.marked.is_empty() {
            self.clear_file_selection();
            self.notice.clear();
            return true;
        }
        if !matches!(key.code, KeyCode::Char('x' | 'V')) {
            return false;
        }
        let result = self.mark_file(key.code == KeyCode::Char('V'));
        self.report(result);
        true
    }

    fn mark_file(&mut self, range: bool) -> Result<(), String> {
        if range && self.file_selection.active() {
            self.finish_file_range();
            self.notice.clear();
            return Ok(());
        }
        let row = self
            .selected_row()
            .filter(|row| !row.directory)
            .ok_or("Select a changed file; file marks never select directory contents")?;
        if !self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .entries
                .iter()
                .any(|entry| entry.relative_path == row.value)
        }) {
            return Err("Select a changed file".into());
        }
        self.finish_file_range();
        if range {
            self.file_selection.range = Some(FileRange {
                anchor: row.value,
                previous: self.file_selection.marked.clone(),
            });
            self.extend_file_range();
        } else if !self.file_selection.marked.remove(&row.value) {
            self.file_selection.marked.insert(row.value);
        }
        self.notice.clear();
        Ok(())
    }

    pub(super) fn extend_file_range(&mut self) {
        let Some(range) = &self.file_selection.range else {
            return;
        };
        let rows = self.rows(0);
        let Some(anchor) = rows.iter().position(|row| row.value == range.anchor) else {
            self.finish_file_range();
            return;
        };
        let cursor = self.selected[0].min(rows.len().saturating_sub(1));
        let mut marked = range.previous.clone();
        for row in &rows[anchor.min(cursor)..=anchor.max(cursor)] {
            if !row.directory {
                marked.insert(row.value.clone());
            }
        }
        self.file_selection.marked = marked;
    }

    pub(super) fn stage_marked(&mut self) -> Result<(), String> {
        self.finish_file_range();
        let paths = self.file_selection.marked.clone();
        self.stage_paths(&paths)
    }

    pub(super) fn file_selection_summary(&self) -> String {
        let visible = self
            .rows(0)
            .iter()
            .filter(|row| !row.directory && self.file_selection.marked.contains(&row.value))
            .count();
        let hidden = self.file_selection.marked.len().saturating_sub(visible);
        let folded = if hidden == 0 {
            String::new()
        } else {
            format!(" · {hidden} folded")
        };
        format!(
            "{} marked{folded} · {} · Esc clear",
            self.file_selection.marked.len(),
            if self.file_selection.active() {
                "j/k extend · V finish"
            } else {
                "s stages · u unstages the set"
            },
        )
    }
}
