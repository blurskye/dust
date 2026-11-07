//! Undo trees, redo branches and persisted history.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use serde::Deserialize;

use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::editor::text::TextBuffer;

/// One replacement: at byte `start`, `removed` became `inserted`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Change {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    #[serde(default)]
    pub removed_endings: Vec<crate::editor::text::LineEnding>,
    #[serde(default)]
    pub inserted_endings: Vec<crate::editor::text::LineEnding>,
}

impl Change {
    fn inverse(&self) -> Self {
        Self {
            start: self.start,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
            removed_endings: self.inserted_endings.clone(),
            inserted_endings: self.removed_endings.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UndoNode {
    parent: Option<usize>,
    children: Vec<usize>,
    /// The changes from the parent's text to this node's, applied one after another.
    changes: Vec<Change>,
    #[serde(skip)]
    marks: Option<(
        crate::editor::decorations::MarkPositions,
        crate::editor::decorations::MarkPositions,
    )>,
}

/// Branch-preserving undo tree. Each node stores the changes from its parent, so memory
/// grows with what was typed rather than with the file size. Nodes sit behind `Arc` so
/// cloning the editor state for a transaction does not copy the history.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(from = "SavedTree", into = "SavedTree")]
pub struct UndoTree {
    nodes: Vec<Arc<UndoNode>>,
    current: usize,
    merge_active: bool,
}

/// The tree as written to disk.
#[derive(Deserialize, Serialize)]
struct SavedTree {
    nodes: Vec<UndoNode>,
    current: usize,
}

impl From<SavedTree> for UndoTree {
    fn from(saved: SavedTree) -> Self {
        Self {
            nodes: saved.nodes.into_iter().map(Arc::new).collect(),
            current: saved.current,
            merge_active: false,
        }
    }
}

impl From<UndoTree> for SavedTree {
    fn from(tree: UndoTree) -> Self {
        Self {
            nodes: tree.nodes.into_iter().map(Arc::unwrap_or_clone).collect(),
            current: tree.current,
        }
    }
}

impl Default for UndoTree {
    fn default() -> Self {
        Self {
            nodes: vec![Arc::new(UndoNode {
                parent: None,
                children: Vec::new(),
                changes: Vec::new(),
                marks: None,
            })],
            current: 0,
            merge_active: false,
        }
    }
}

impl UndoTree {
    pub fn record(&mut self, mut changes: Vec<Change>, merge: bool) {
        changes.retain(|change| {
            change.removed != change.inserted || change.removed_endings != change.inserted_endings
        });
        if changes.is_empty() {
            return;
        }
        if merge && self.merge_active && self.current != 0 {
            Arc::make_mut(&mut self.nodes[self.current])
                .changes
                .extend(changes);
            return;
        }
        let next = self.nodes.len();
        self.nodes.push(Arc::new(UndoNode {
            parent: Some(self.current),
            children: Vec::new(),
            changes,
            marks: None,
        }));
        Arc::make_mut(&mut self.nodes[self.current])
            .children
            .push(next);
        self.current = next;
        self.merge_active = merge;
    }

    pub(crate) fn record_with_marks(
        &mut self,
        changes: Vec<Change>,
        merge: bool,
        before: crate::editor::decorations::MarkPositions,
        after: crate::editor::decorations::MarkPositions,
    ) {
        if !changes.iter().any(|change| {
            change.removed != change.inserted || change.removed_endings != change.inserted_endings
        }) {
            return;
        }
        self.record(changes, merge);
        let node = Arc::make_mut(&mut self.nodes[self.current]);
        if let Some((_, previous_after)) = &mut node.marks {
            *previous_after = after;
        } else {
            node.marks = Some((before, after));
        }
    }

    pub(crate) fn mark_positions(
        &self,
        undo: bool,
    ) -> Option<crate::editor::decorations::MarkPositions> {
        if undo {
            self.nodes[self.current]
                .marks
                .as_ref()
                .map(|(before, _)| before.clone())
        } else {
            let child = *self.nodes[self.current].children.last()?;
            self.nodes[child]
                .marks
                .as_ref()
                .map(|(_, after)| after.clone())
        }
    }

    pub fn boundary(&mut self) {
        self.merge_active = false;
    }

    /// Forget the history, for text that changed without being recorded.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Step to the parent and return the changes that turn the current text into its text.
    pub fn undo(&mut self) -> Option<Vec<Change>> {
        let node = &self.nodes[self.current];
        let parent = node.parent?;
        let changes = node.changes.iter().rev().map(Change::inverse).collect();
        self.current = parent;
        self.merge_active = false;
        Some(changes)
    }

    /// Step to the newest child and return the changes that turn the current text into its text.
    pub fn redo(&mut self) -> Option<Vec<Change>> {
        let child = *self.nodes[self.current].children.last()?;
        self.current = child;
        self.merge_active = false;
        Some(self.nodes[child].changes.clone())
    }

    /// Count reachable history steps without copying their text changes.
    pub(crate) fn available_steps(&self, undo: bool, limit: usize) -> usize {
        let mut current = self.current;
        let mut steps = 0;
        while steps < limit {
            let node = &self.nodes[current];
            let next = if undo {
                node.parent
            } else {
                node.children.last().copied()
            };
            let Some(next) = next else { break };
            current = next;
            steps += 1;
        }
        steps
    }

    #[must_use]
    pub fn redo_branch_count(&self) -> usize {
        self.nodes[self.current].children.len()
    }

    #[must_use]
    pub const fn current_node(&self) -> usize {
        self.current
    }

    /// Whether anything was ever recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    /// A tree read back from disk is only used when its links are consistent.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.current < self.nodes.len()
            && self.nodes[0].parent.is_none()
            && self.nodes.iter().enumerate().all(|(index, node)| {
                let mut children = std::collections::BTreeSet::new();
                node.children.iter().all(|&child| {
                    child > index
                        && child < self.nodes.len()
                        && self.nodes[child].parent == Some(index)
                        && children.insert(child)
                }) && node.parent.is_none_or(|parent| {
                    parent < index && self.nodes[parent].children.contains(&index)
                }) && (index == 0 || node.parent.is_some())
            })
    }
}

/// The undo history kept on disk for a file, valid only while the file still holds the text
/// it was saved with (like Vim's `undofile`).
#[derive(Deserialize, Serialize)]
struct UndoFile {
    text_sha256: String,
    tree: UndoTree,
}

fn undo_file(directory: &Path, path: &Path) -> PathBuf {
    let name = Sha256::digest(path.as_os_str().as_encoded_bytes());
    directory.join(format!("{name:x}.json"))
}

fn text_sha256(text: &TextBuffer) -> String {
    let mut digest = Sha256::new();
    for chunk in text.chunks() {
        digest.update(chunk);
    }
    format!("{:x}", digest.finalize())
}

/// The saved history of `path`, when it was saved against exactly `text`.
pub(crate) fn load_undo_history(
    directory: &Path,
    path: &Path,
    text: &TextBuffer,
    maximum_bytes: usize,
) -> Option<UndoTree> {
    let file = undo_file(directory, path);
    if fs::metadata(&file).ok()?.len() > maximum_bytes as u64 {
        return None;
    }
    let saved: UndoFile = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    (saved.tree.is_well_formed() && saved.text_sha256 == text_sha256(text)).then_some(saved.tree)
}

/// Write the history of `path`, whose file now holds `text`, on a worker thread. A history
/// larger than `maximum_bytes` is not kept.
pub(crate) fn save_undo_history(
    directory: PathBuf,
    path: PathBuf,
    text: TextBuffer,
    tree: UndoTree,
    maximum_bytes: usize,
) -> std::sync::mpsc::Receiver<Result<(), String>> {
    type Save = (
        PathBuf,
        PathBuf,
        TextBuffer,
        UndoTree,
        usize,
        Vec<std::sync::mpsc::Sender<Result<(), String>>>,
    );
    type Queue = (
        std::sync::Mutex<std::collections::BTreeMap<PathBuf, Save>>,
        std::sync::Condvar,
    );
    static QUEUE: std::sync::OnceLock<Arc<Queue>> = std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let queue = Arc::new((
            std::sync::Mutex::new(std::collections::BTreeMap::<PathBuf, Save>::new()),
            std::sync::Condvar::new(),
        ));
        let worker = queue.clone();
        thread::spawn(move || {
            loop {
                let mut jobs = worker.0.lock().expect("undo queue");
                while jobs.is_empty() {
                    jobs = worker.1.wait(jobs).expect("undo queue");
                }
                let (_, (directory, path, text, tree, maximum_bytes, replies)) =
                    jobs.pop_first().expect("queued undo save");
                drop(jobs);
                let result = write_undo_history(directory, path, text, tree, maximum_bytes)
                    .map_err(|error| error.to_string());
                for reply in replies {
                    let _ = reply.send(result.clone());
                }
            }
        });
        queue
    });
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut jobs = queue.0.lock().expect("undo queue");
    let key = undo_file(&directory, &path);
    let mut replies = jobs.remove(&key).map_or_else(Vec::new, |save| save.5);
    replies.push(sender);
    jobs.insert(key, (directory, path, text, tree, maximum_bytes, replies));
    drop(jobs);
    queue.1.notify_one();
    receiver
}

fn write_undo_history(
    directory: PathBuf,
    path: PathBuf,
    text: TextBuffer,
    tree: UndoTree,
    maximum_bytes: usize,
) -> std::io::Result<()> {
    let file = undo_file(&directory, &path);
    if tree.is_empty() {
        return remove_undo_file(&file);
    }
    let saved = UndoFile {
        text_sha256: text_sha256(&text),
        tree,
    };
    let json = serde_json::to_vec(&saved)?;
    if json.len() > maximum_bytes {
        return remove_undo_file(&file);
    }
    let partial = file.with_extension(format!("{}.partial", std::process::id()));
    let result = fs::create_dir_all(&directory)
        .and_then(|()| fs::write(&partial, json))
        .and_then(|()| fs::rename(&partial, &file));
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn remove_undo_file(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::transaction::undo::Change;
    use crate::editor::transaction::undo::UndoTree;

    fn apply(text: &mut String, changes: &[Change]) {
        for change in changes {
            let end = change.start + change.removed.len();
            assert_eq!(&text[change.start..end], change.removed);
            text.replace_range(change.start..end, &change.inserted);
        }
    }

    fn insert(at: usize, text: &str) -> Vec<Change> {
        vec![Change {
            start: at,
            removed: String::new(),
            inserted: text.into(),
            removed_endings: Vec::new(),
            inserted_endings: Vec::new(),
        }]
    }

    #[test]
    fn history_save_reports_failure_and_removes_partial_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let file = crate::editor::transaction::undo::undo_file(root.path(), &path);
        std::fs::create_dir(&file).unwrap(); // Publishing over a directory must fail.
        let mut tree = UndoTree::default();
        tree.record(insert(0, "x"), false);
        let reply = crate::editor::transaction::undo::save_undo_history(
            root.path().into(),
            path,
            crate::editor::text::TextBuffer::from_text("x"),
            tree,
            1 << 20,
        );
        assert!(
            reply
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .is_err()
        );
        assert!(
            !file
                .with_extension(format!("{}.partial", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn successive_history_saves_publish_the_latest_tree() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        for count in 1..=20 {
            let mut tree = UndoTree::default();
            tree.record(insert(0, &"x".repeat(count)), false);
            crate::editor::transaction::undo::save_undo_history(
                root.path().into(),
                path.clone(),
                crate::editor::text::TextBuffer::from_text(&"x".repeat(count)),
                tree,
                1 << 20,
            );
        }
        let text = crate::editor::text::TextBuffer::from_text(&"x".repeat(20));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(mut tree) = crate::editor::transaction::undo::load_undo_history(
                root.path(),
                &path,
                &text,
                1 << 20,
            ) {
                assert_eq!(tree.undo().unwrap()[0].removed, "x".repeat(20));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn preserves_redo_branch_after_new_edit() {
        let mut text = String::from("a");
        let mut tree = UndoTree::default();
        for (at, typed) in [(1, "b"), (2, "c")] {
            apply(&mut text, &insert(at, typed));
            tree.record(insert(at, typed), false);
        }
        apply(&mut text, &tree.undo().unwrap());
        apply(&mut text, &insert(2, "X"));
        tree.record(insert(2, "X"), false);
        assert_eq!(text, "abX");
        apply(&mut text, &tree.undo().unwrap());
        assert_eq!(text, "ab");
        assert_eq!(tree.redo_branch_count(), 2);
        assert_eq!(tree.available_steps(true, usize::MAX), 1);
        assert_eq!(tree.available_steps(false, usize::MAX), 1);
        assert_eq!(tree.available_steps(true, 0), 0);
        assert_eq!(tree.available_steps(false, 0), 0);
        apply(&mut text, &tree.redo().unwrap());
        assert_eq!(tree.available_steps(true, 1), 1);
        assert_eq!(tree.available_steps(true, usize::MAX), 2);
        assert_eq!(tree.available_steps(false, usize::MAX), 0);
        assert_eq!(text, "abX");
    }

    #[test]
    fn saved_undo_trees_reject_inconsistent_and_duplicate_child_links() {
        let mut tree = UndoTree::default();
        tree.record(insert(0, "a"), false);
        tree.record(insert(1, "b"), false);
        assert!(tree.is_well_formed());

        let mut inconsistent = tree.clone();
        std::sync::Arc::make_mut(&mut inconsistent.nodes[0])
            .children
            .push(2);
        let restored: UndoTree =
            serde_json::from_str(&serde_json::to_string(&inconsistent).unwrap()).unwrap();
        assert!(!restored.is_well_formed());

        std::sync::Arc::make_mut(&mut tree.nodes[0])
            .children
            .push(1);
        let restored: UndoTree =
            serde_json::from_str(&serde_json::to_string(&tree).unwrap()).unwrap();
        assert!(!restored.is_well_formed());
    }

    #[test]
    fn merged_changes_undo_together_and_round_trip_through_json() {
        let mut text = String::from("x");
        let mut tree = UndoTree::default();
        for (at, typed) in [(1, "a"), (2, "b"), (3, "c")] {
            apply(&mut text, &insert(at, typed));
            tree.record(insert(at, typed), true);
        }
        let mut restored: UndoTree =
            serde_json::from_str(&serde_json::to_string(&tree).unwrap()).unwrap();
        assert!(restored.is_well_formed());
        apply(&mut text, &restored.undo().unwrap());
        assert_eq!(text, "x");
        assert!(restored.undo().is_none());
        apply(&mut text, &restored.redo().unwrap());
        assert_eq!(text, "xabc");
    }
}
