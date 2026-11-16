//! A list's rows, kept until what they are built from changes. One `j` asks for the focused
//! list's rows a dozen times (moving, the preview, the paint), and on a big repository Changes
//! and History hold thousands of them: building them each time made every key cost tens of
//! milliseconds.
use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Rows shared between the memo and whoever asked.
pub(super) type Rows = Arc<Vec<Row>>;
pub(super) type History = Arc<Vec<(String, String)>>;
pub(super) type Files = Arc<BTreeSet<PathBuf>>;
/// Changes' rows being built on a thread, with the status entries they were built from.
pub(super) type RowsLater = std::thread::JoinHandle<(Vec<GitStatusEntry>, Vec<Row>)>;
/// A status with at least this many entries has its Changes rows built on a thread: about
/// 10 ms of the UI thread's time per 3,000.
pub(super) const ROWS_LATER: usize = 1_000;

/// The memo for the four rail panes.
#[derive(Default)]
pub(super) struct RowsMemo(RefCell<[Option<Kept>; 4]>);

struct Kept {
    key: u64,
    /// Held so their addresses can't be reused while `key` stands for them.
    history: Option<History>,
    files: Option<Files>,
    rows: Rows,
}

struct Inputs {
    key: u64,
    history: Option<History>,
    files: Option<Files>,
}

fn same<T>(kept: &Option<Arc<T>>, now: &Option<Arc<T>>) -> bool {
    match (kept, now) {
        (Some(kept), Some(now)) => Arc::ptr_eq(kept, now),
        (None, None) => true,
        _ => false,
    }
}

/// Memoised rows for a commit's files (`Commit::rows`), and where each path is in them.
#[derive(Default)]
pub(super) struct CommitMemo(RefCell<Option<(u64, Rows)>>, std::cell::OnceCell<HashMap<PathBuf, usize>>);

impl CommitMemo {
    /// Where `path` is in a commit's `files` (which never change).
    pub(super) fn position(&self, files: &[commit::ChangedFile], path: &Path) -> Option<usize> {
        self.1
            .get_or_init(|| files.iter().enumerate().map(|(index, file)| (file.path.clone(), index)).collect())
            .get(path)
            .copied()
    }

    /// The rows for `filter` and `collapsed` over a commit's files (which never change).
    pub(super) fn get(
        &self,
        filter: &filter::ListFilter,
        collapsed: &BTreeSet<PathBuf>,
        build: impl FnOnce() -> Vec<Row>,
    ) -> Rows {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        filter.input.input.hash(&mut hasher);
        collapsed.hash(&mut hasher);
        let key = hasher.finish();
        if let Some((kept, rows)) = &*self.0.borrow()
            && *kept == key
        {
            return rows.clone();
        }
        let rows = Arc::new(build());
        *self.0.borrow_mut() = Some((key, rows.clone()));
        rows
    }
}

impl GitScreen {
    /// What Changes and History are built from; `None` for the lists that are short or
    /// built from data without an identity (branches, tags, stashes, the reflog, worktrees).
    fn rows_inputs(&self, pane: usize) -> Option<Inputs> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        pane.hash(&mut hasher);
        self.filters[self.slot(pane)].input.input.hash(&mut hasher);
        let (history, files) = match pane {
            0 if self.files_view == 0 => {
                (self.changes_collapsed, self.changes_tree, self.all_files).hash(&mut hasher);
                // The status worker bumps `revision` only when the entries changed, so a
                // refresh that finds the same changes keeps its rows (a warm snapshot from
                // the disk cache has a revision of its own, `warm::REVISION`).
                self.snapshot
                    .as_ref()
                    .map(|snapshot| (snapshot.revision, snapshot.entries.len()))
                    .hash(&mut hasher);
                self.collapsed.hash(&mut hasher);
                (None, self.all_files.then(|| self.files.clone()))
            }
            2 if self.history_view == 0 => (Some(self.history.clone()), None),
            _ => return None,
        };
        Some(Inputs {
            key: hasher.finish(),
            history,
            files,
        })
    }

    /// What Changes is built from apart from the status, so rows built on a thread for a
    /// new status can be checked against the screen when they land.
    pub(super) fn changes_view(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.filters[0].input.input.hash(&mut hasher);
        (self.changes_collapsed, self.changes_tree, self.all_files).hash(&mut hasher);
        self.all_files.then(|| Arc::as_ptr(&self.files) as usize).hash(&mut hasher);
        self.collapsed.hash(&mut hasher);
        hasher.finish()
    }

    /// Keeps `rows`, built elsewhere for the current inputs, as `pane`'s rows.
    pub(super) fn seed_rows(&self, pane: usize, rows: Vec<Row>) {
        if let Some(inputs) = self.rows_inputs(pane) {
            self.rows_memo.0.borrow_mut()[pane] = Some(Kept {
                key: inputs.key,
                history: inputs.history,
                files: inputs.files,
                rows: Arc::new(rows),
            });
        }
    }

    /// Changes' rows for a status with `entries`, built on a thread for the screen's view
    /// as it is now (`changes_view`); the entries come back with them.
    pub(super) fn changes_rows_later(&self, entries: Vec<GitStatusEntry>) -> RowsLater {
        let query = self.filters[0].input.input.clone();
        let (folded, tree) = (self.changes_collapsed, self.changes_tree);
        let files = self.all_files.then(|| self.files.clone());
        let collapsed = self.collapsed.clone();
        std::thread::spawn(move || {
            let mut filter = filter::ListFilter::default();
            filter.input.input = query;
            let rows = changes_rows(&entries, files.as_deref(), tree, folded, &filter, &collapsed);
            (entries, rows)
        })
    }

    pub(super) fn rows(&self, pane: usize) -> Rows {
        let Some(inputs) = self.rows_inputs(pane) else {
            return Arc::new(self.build_rows(pane));
        };
        if let Some(kept) = &self.rows_memo.0.borrow()[pane]
            && kept.key == inputs.key
            && same(&kept.history, &inputs.history)
            && same(&kept.files, &inputs.files)
        {
            return kept.rows.clone();
        }
        let rows = Arc::new(self.build_rows(pane));
        self.rows_memo.0.borrow_mut()[pane] = Some(Kept {
            key: inputs.key,
            history: inputs.history,
            files: inputs.files,
            rows: rows.clone(),
        });
        rows
    }
}

/// Changes' rows: each changed path with its staged marker and code, as a tree or flat,
/// with every tracked file when `files` is given.
pub(super) fn changes_rows(
    entries: &[GitStatusEntry],
    files: Option<&BTreeSet<PathBuf>>,
    tree: bool,
    folded: bool,
    filter: &filter::ListFilter,
    collapsed: &BTreeSet<PathBuf>,
) -> Vec<Row> {
    if folded && !filter.active() {
        return Vec::new();
    }
    let mut paths = entries
        .iter()
        .map(|entry| {
            let marker = match (entry.sources.staged, entry.sources.unstaged) {
                (true, false) => '✓',
                (true, true) => '◐',
                _ => ' ',
            };
            (entry.relative_path.clone(), format!("{marker} {:<2}", entry.change_code().trim()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(files) = files {
        for path in files {
            paths.entry(path.clone()).or_insert("    ".into());
        }
    }
    // `GitScreen::changes_flat`: a short status lists flat unless the tree was asked for.
    if !tree && files.is_none() && entries.len() <= 40 {
        paths
            .into_keys()
            .filter(|path| !filter.active() || filter.matches(&path.to_string_lossy()))
            .map(|path| Row::plain(path.display().to_string(), path))
            .collect()
    } else {
        filter.tree(paths, collapsed)
    }
}

/// `path`'s status entry. A snapshot's entries are sorted by path (`GitStatusSnapshot`), so
/// this is a binary search, not a scan of thousands per painted row.
pub(super) fn entry<'a>(entries: &'a [GitStatusEntry], path: &Path) -> Option<&'a GitStatusEntry> {
    entries
        .binary_search_by(|entry| entry.relative_path.as_path().cmp(path))
        .ok()
        .map(|index| &entries[index])
}

/// The entries at or below `directory`: one run of the sorted list.
pub(super) fn below<'a>(entries: &'a [GitStatusEntry], directory: &Path) -> &'a [GitStatusEntry] {
    let start = entries.partition_point(|entry| entry.relative_path.as_path() < directory);
    let length = entries[start..]
        .iter()
        .take_while(|entry| entry.relative_path.starts_with(directory))
        .count();
    &entries[start..start + length]
}
