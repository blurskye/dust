use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitStatusGeneration(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitRepositoryId(pub u64);
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitFileStatus {
    Untracked,
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
}
/// What a change set is relative to. A caller never has to infer this: the reader that
/// produced the data states it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitChangeSource {
    /// HEAD to index: what a commit would record.
    Staged,
    /// Index to disk: changed but not yet staged.
    Unstaged,
    /// Unsaved editor content relative to the index, at one exact document revision.
    Unsaved { document_revision: u64 },
}
/// Which sources currently differ for one path. A false bit is not a claim that the source
/// matches; it means the reader attributed no change to it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitChangeSources {
    pub staged: bool,
    pub unstaged: bool,
    pub unsaved_document_revision: Option<u64>,
}
impl GitChangeSource {
    /// Plain-text label for this source, for a picker row's secondary text.
    #[must_use]
    pub const fn source_label(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Unsaved { .. } => "unsaved",
        }
    }
}
impl GitChangeSources {
    /// Plain-text label naming every source that currently differs for one path. Returned as a
    /// `&'static str` so composing a bounded row allocates nothing, and so a path that is both
    /// staged and further modified is never described as only one of them.
    #[must_use]
    pub const fn source_label(&self) -> &'static str {
        match (
            self.staged,
            self.unstaged,
            self.unsaved_document_revision.is_some(),
        ) {
            (true, true, true) => "staged+unstaged+unsaved",
            (true, true, false) => "staged+unstaged",
            (true, false, true) => "staged+unsaved",
            (false, true, true) => "unstaged+unsaved",
            (true, false, false) => "staged",
            (false, true, false) => "unstaged",
            (false, false, true) => "unsaved",
            (false, false, false) => "unchanged",
        }
    }
    #[must_use]
    pub const fn contains(&self, source: GitChangeSource) -> bool {
        match source {
            GitChangeSource::Staged => self.staged,
            GitChangeSource::Unstaged => self.unstaged,
            GitChangeSource::Unsaved { document_revision } => {
                matches!(self.unsaved_document_revision, Some(held) if held == document_revision)
            }
        }
    }
    #[must_use]
    pub const fn count(&self) -> u8 {
        self.staged as u8 + self.unstaged as u8 + self.unsaved_document_revision.is_some() as u8
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count() == 0
    }
}
/// Unsaved editor content offered to a Git reader as data. The Git owner holds no editor
/// state; a caller supplies the bounded list and the reader only attributes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitUnsavedDocument {
    pub relative_path: PathBuf,
    pub document_revision: u64,
}
impl GitUnsavedDocument {
    #[must_use]
    pub fn new(relative_path: PathBuf, document_revision: u64) -> Self {
        Self {
            relative_path,
            document_revision,
        }
    }
}
/// Resolve the unsaved revision for one workspace-relative path. The list is expected to be
/// sorted and duplicate-free by the validator; a linear scan keeps the reader allocation-free.
#[must_use]
pub fn unsaved_document_revision(
    documents: &[GitUnsavedDocument],
    relative_path: &Path,
) -> Option<u64> {
    documents
        .iter()
        .find(|document| document.relative_path == relative_path)
        .map(|document| document.document_revision)
}
/// Reject an unsaved list that is unsorted, duplicated, revision-zero, or beyond the
/// already-bounded status policy. No new ceiling is introduced: the list is bounded by the
/// same file and path-byte limits the caller already accepted.
pub fn validate_unsaved_documents(
    documents: &[GitUnsavedDocument],
    maximum_files: usize,
    maximum_path_bytes: usize,
) -> Result<(), GitStatusError> {
    if documents.len() > maximum_files || documents.len() > 100_000 {
        return Err(GitStatusError::InvalidPolicy);
    }
    let mut bytes = 0usize;
    let mut previous: Option<&Path> = None;
    for document in documents {
        if document.document_revision == 0
            || document.relative_path.is_absolute()
            || document
                .relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(GitStatusError::InvalidPolicy);
        }
        if previous.is_some_and(|last| last >= document.relative_path.as_path()) {
            return Err(GitStatusError::InvalidPolicy);
        }
        bytes = bytes.saturating_add(document.relative_path.as_os_str().len());
        previous = Some(document.relative_path.as_path());
    }
    if bytes > maximum_path_bytes {
        return Err(GitStatusError::InvalidPolicy);
    }
    Ok(())
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GitStatusEntry {
    pub relative_path: PathBuf,
    /// Repository-relative source of a rename, including sources outside a nested scope.
    pub original_path: Option<PathBuf>,
    pub status: GitFileStatus,
    pub sources: GitChangeSources,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusSnapshot {
    pub generation: GitStatusGeneration,
    pub root_revision: u64,
    pub repository: PathBuf,
    /// The folder holding this checkout's index and HEAD.
    pub git_dir: PathBuf,
    /// The parent repository's name when the scope is a linked worktree.
    pub worktree_repository: Option<String>,
    pub scope_root: PathBuf,
    pub repository_id: GitRepositoryId,
    pub entries: Vec<GitStatusEntry>,
    pub truncated: bool,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub struct GitStatusPolicy {
    pub root: PathBuf,
    pub root_revision: u64,
    pub maximum_files: usize,
    pub maximum_path_bytes: usize,
    pub deadline: Duration,
}
#[derive(Debug, Error)]
pub enum GitStatusError {
    #[error("invalid git status policy")]
    InvalidPolicy,
    #[error("not a Git repository")]
    NotRepository,
    #[error("Git repository is outside workspace scope")]
    OutsideScope,
    #[error("bare Git repositories are unsupported")]
    BareUnsupported,
    #[error("Git status deadline exceeded")]
    Deadline,
    #[error("Git status was cancelled")]
    Cancelled,
    #[error("Git status worker stopped")]
    Stopped,
    #[error("Git backend failed: {0}")]
    Backend(String),
}
#[derive(Debug)]
pub(super) enum Event {
    Complete(Result<GitStatusSnapshot, GitStatusError>),
}
#[derive(Default)]
pub struct GitStatusService {
    next: u64,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<Event>>,
    accepted: Option<GitStatusSnapshot>,
    active_generation: Option<GitStatusGeneration>,
    revision: u64,
}
impl GitStatusService {
    pub fn start(
        &mut self,
        policy: GitStatusPolicy,
    ) -> Result<GitStatusGeneration, GitStatusError> {
        self.start_with_unsaved(policy, Vec::new())
    }

    /// Start a status read that also attributes unsaved editor content. The list is bounded
    /// by the policy's own file and path-byte limits and must be sorted by path.
    pub fn start_with_unsaved(
        &mut self,
        policy: GitStatusPolicy,
        unsaved: Vec<GitUnsavedDocument>,
    ) -> Result<GitStatusGeneration, GitStatusError> {
        if policy.maximum_files == 0
            || policy.maximum_files > 100_000
            || policy.maximum_path_bytes == 0
            || policy.maximum_path_bytes > 16 * 1024 * 1024
            || policy.deadline.is_zero()
            || policy.deadline > Duration::from_secs(30)
        {
            return Err(GitStatusError::InvalidPolicy);
        }
        validate_unsaved_documents(&unsaved, policy.maximum_files, policy.maximum_path_bytes)?;
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitStatusGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-status-{}", generation.0))
            .spawn(move || {
                let result = read_status(generation, &policy, &unsaved, &worker);
                let _ = tx.send(Event::Complete(result));
            })
            .map_err(|_| GitStatusError::Stopped)?;
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        self.active_generation = Some(generation);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Release)
        }
        self.receiver = None;
        self.active_generation = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitStatusSnapshot>, GitStatusError> {
        let Some(rx) = &self.receiver else {
            return Ok(None);
        };
        match rx.try_recv() {
            Ok(Event::Complete(result)) => {
                let mut snapshot = result?;
                if self.active_generation != Some(snapshot.generation) {
                    return Ok(None);
                }
                let changed = self.accepted.as_ref().is_none_or(|old| {
                    old.repository != snapshot.repository
                        || old.scope_root != snapshot.scope_root
                        || old.entries != snapshot.entries
                        || old.truncated != snapshot.truncated
                        || old.root_revision != snapshot.root_revision
                });
                if changed {
                    self.revision = self.revision.saturating_add(1)
                }
                snapshot.revision = self.revision;
                self.accepted = Some(snapshot);
                self.receiver = None;
                self.active_generation = None;
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(GitStatusError::Stopped),
        }
    }
    pub const fn generation(&self) -> u64 {
        self.next
    }
    pub fn snapshot_if_changed(&self, last: u64) -> Option<&GitStatusSnapshot> {
        self.accepted.as_ref().filter(|s| s.revision != last)
    }
}
impl Drop for GitStatusService {
    fn drop(&mut self) {
        self.cancel()
    }
}
pub(super) fn backend(message: &str) -> GitStatusError {
    GitStatusError::Backend(message.chars().take(1024).collect())
}
pub(super) fn git_status_precedence(status: &GitFileStatus) -> u8 {
    match status {
        GitFileStatus::Untracked => 0,
        GitFileStatus::Added => 1,
        GitFileStatus::Modified => 2,
        GitFileStatus::Renamed => 3,
        GitFileStatus::Deleted => 4,
        GitFileStatus::Conflicted => 5,
    }
}

/// Build immutable file and ancestor-directory decorations. Conflicted wins,
/// followed by deleted, renamed, modified, added, and untracked.
#[must_use]
pub fn aggregate_git_status(
    scope_root: &Path,
    entries: &[GitStatusEntry],
) -> std::collections::BTreeMap<PathBuf, GitFileStatus> {
    let mut result = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.relative_path.is_absolute()
            || entry
                .relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            continue;
        }
        let absolute = scope_root.join(&entry.relative_path);
        if !absolute.starts_with(scope_root) {
            continue;
        }
        let mut current = Some(absolute.as_path());
        while let Some(path) = current {
            if !path.starts_with(scope_root) {
                break;
            }
            let replace = result.get(path).is_none_or(|existing| {
                git_status_precedence(&entry.status) > git_status_precedence(existing)
            });
            if replace {
                result.insert(path.to_path_buf(), entry.status.clone());
            }
            if path == scope_root {
                break;
            }
            current = path.parent();
        }
    }
    result
}

pub(super) fn read_status(
    generation: GitStatusGeneration,
    p: &GitStatusPolicy,
    unsaved: &[GitUnsavedDocument],
    cancel: &AtomicBool,
) -> Result<GitStatusSnapshot, GitStatusError> {
    let root = std::fs::canonicalize(&p.root).map_err(|e| backend(&e.to_string()))?;
    let repo = git2::Repository::discover(&root).map_err(|_| GitStatusError::NotRepository)?;
    let workdir = repo.workdir().ok_or(GitStatusError::BareUnsupported)?;
    let workdir = std::fs::canonicalize(workdir).map_err(|e| backend(&e.to_string()))?;
    if !root.starts_with(&workdir) && !workdir.starts_with(&root) {
        return Err(GitStatusError::OutsideScope);
    }
    let prefix = root.strip_prefix(&workdir).unwrap_or(Path::new(""));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    let repository_id = GitRepositoryId(hasher.finish());
    let started = Instant::now();
    let statuses = repo
        .statuses(Some(
            git2::StatusOptions::new()
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true),
        ))
        .map_err(|e| backend(e.message()))?;
    let mut entries = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for item in statuses.iter() {
        if cancel.load(Ordering::Acquire) {
            return Err(GitStatusError::Cancelled);
        }
        if started.elapsed() > p.deadline {
            return Err(GitStatusError::Deadline);
        }
        let Some(repo_path) = item
            .index_to_workdir()
            .and_then(|delta| delta.new_file().path().map(Path::to_path_buf))
            .or_else(|| {
                item.head_to_index()
                    .and_then(|delta| delta.new_file().path().map(Path::to_path_buf))
            })
            .or_else(|| item.path().map(PathBuf::from))
        else {
            continue;
        };
        if !repo_path.starts_with(prefix) {
            continue;
        }
        let path = repo_path
            .strip_prefix(prefix)
            .unwrap_or(&repo_path)
            .to_path_buf();
        let cost = path.as_os_str().len();
        if entries.len() >= p.maximum_files || bytes.saturating_add(cost) > p.maximum_path_bytes {
            truncated = true;
            break;
        }
        bytes += cost;
        let s = item.status();
        let conflicted = s.is_conflicted();
        let staged = conflicted
            || s.is_index_new()
            || s.is_index_modified()
            || s.is_index_deleted()
            || s.is_index_renamed()
            || s.is_index_typechange();
        let unstaged = conflicted
            || s.is_wt_new()
            || s.is_wt_modified()
            || s.is_wt_deleted()
            || s.is_wt_renamed()
            || s.is_wt_typechange();
        let status = if conflicted {
            GitFileStatus::Conflicted
        } else if s.is_wt_new() {
            GitFileStatus::Untracked
        } else if s.is_index_new() {
            GitFileStatus::Added
        } else if s.is_wt_deleted() || s.is_index_deleted() {
            GitFileStatus::Deleted
        } else if s.is_wt_renamed() || s.is_index_renamed() {
            GitFileStatus::Renamed
        } else {
            GitFileStatus::Modified
        };
        let unsaved_document_revision = unsaved_document_revision(unsaved, &path);
        let original_path = if s.is_wt_renamed() {
            item.index_to_workdir()
                .and_then(|delta| delta.old_file().path().map(Path::to_path_buf))
        } else if s.is_index_renamed() {
            item.head_to_index()
                .and_then(|delta| delta.old_file().path().map(Path::to_path_buf))
        } else {
            None
        };
        entries.push(GitStatusEntry {
            relative_path: path,
            original_path,
            status,
            sources: GitChangeSources {
                staged,
                unstaged,
                unsaved_document_revision,
            },
        });
    }
    entries.sort();
    // Unsaved editor content is a third source. A path whose only change is in an unsaved
    // buffer produces no repository status, so it is merged in here rather than being
    // presented as either staged or unstaged.
    for document in unsaved {
        let position = match entries
            .binary_search_by(|entry| entry.relative_path.as_path().cmp(&document.relative_path))
        {
            Ok(_) => continue,
            Err(position) => position,
        };
        let cost = document.relative_path.as_os_str().len();
        if entries.len() >= p.maximum_files || bytes.saturating_add(cost) > p.maximum_path_bytes {
            truncated = true;
            break;
        }
        bytes += cost;
        entries.insert(
            position,
            GitStatusEntry {
                relative_path: document.relative_path.clone(),
                original_path: None,
                status: GitFileStatus::Modified,
                sources: GitChangeSources {
                    staged: false,
                    unstaged: false,
                    unsaved_document_revision: Some(document.document_revision),
                },
            },
        );
    }
    entries.sort();
    Ok(GitStatusSnapshot {
        generation,
        root_revision: p.root_revision,
        repository: workdir,
        git_dir: repo.path().to_path_buf(),
        worktree_repository: crate::files::linked_worktree_repository(&root),
        scope_root: root,
        repository_id,
        entries,
        truncated,
        revision: 0,
    })
}
