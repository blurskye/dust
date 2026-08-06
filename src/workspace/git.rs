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
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GitStatusEntry {
    pub relative_path: PathBuf,
    pub status: GitFileStatus,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusSnapshot {
    pub generation: GitStatusGeneration,
    pub root_revision: u64,
    pub repository: PathBuf,
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
enum Event {
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
        if policy.maximum_files == 0
            || policy.maximum_files > 100_000
            || policy.maximum_path_bytes == 0
            || policy.maximum_path_bytes > 16 * 1024 * 1024
            || policy.deadline.is_zero()
            || policy.deadline > Duration::from_secs(30)
        {
            return Err(GitStatusError::InvalidPolicy);
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitStatusGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-status-{}", generation.0))
            .spawn(move || {
                let result = read_status(generation, &policy, &worker);
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
fn backend(message: &str) -> GitStatusError {
    GitStatusError::Backend(message.chars().take(1024).collect())
}
fn git_status_precedence(status: &GitFileStatus) -> u8 {
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

fn read_status(
    generation: GitStatusGeneration,
    p: &GitStatusPolicy,
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
        let Some(repo_path) = item.path().map(PathBuf::from) else {
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
        let status = if s.is_conflicted() {
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
        entries.push(GitStatusEntry {
            relative_path: path,
            status,
        });
    }
    entries.sort();
    Ok(GitStatusSnapshot {
        generation,
        root_revision: p.root_revision,
        repository: workdir,
        scope_root: root,
        repository_id,
        entries,
        truncated,
        revision: 0,
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GitDiffLayer {
    Unstaged,
    Staged,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitHunkId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitHunkGeneration(pub u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitDiffLineKind {
    Context,
    Add,
    Delete,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffLine {
    pub kind: GitDiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHunk {
    pub id: GitHunkId,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
    pub lines: Vec<GitDiffLine>,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GitHunkSignKind {
    Added,
    Changed,
    Deleted,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GitHunkSign {
    pub line: usize,
    pub kind: GitHunkSignKind,
}

#[must_use]
pub fn git_hunk_signs(snapshot: &GitHunkSnapshot) -> Vec<GitHunkSign> {
    let mut signs = std::collections::BTreeMap::new();
    for hunk in &snapshot.hunks {
        let has_add = hunk
            .lines
            .iter()
            .any(|line| line.kind == GitDiffLineKind::Add);
        let has_delete = hunk
            .lines
            .iter()
            .any(|line| line.kind == GitDiffLineKind::Delete);
        let kind = match (has_add, has_delete) {
            (true, false) => GitHunkSignKind::Added,
            (false, true) => GitHunkSignKind::Deleted,
            _ => GitHunkSignKind::Changed,
        };
        let mut inserted = false;
        for line in &hunk.lines {
            if let Some(line_number) = line.new_line.filter(|_| line.kind == GitDiffLineKind::Add) {
                let line = line_number.saturating_sub(1) as usize;
                signs
                    .entry(line)
                    .and_modify(|existing| {
                        if kind > *existing {
                            *existing = kind;
                        }
                    })
                    .or_insert(kind);
                inserted = true;
            }
        }
        if !inserted {
            signs
                .entry(hunk.new_start.saturating_sub(1) as usize)
                .and_modify(|existing| {
                    if kind > *existing {
                        *existing = kind;
                    }
                })
                .or_insert(kind);
        }
    }
    signs
        .into_iter()
        .map(|(line, kind)| GitHunkSign { line, kind })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHunkSnapshot {
    pub generation: GitHunkGeneration,
    pub revision: u64,
    pub repository_id: GitRepositoryId,
    pub status_revision: u64,
    pub root_revision: u64,
    pub file: PathBuf,
    pub layer: GitDiffLayer,
    pub buffer_revision: Option<u64>,
    pub index_fingerprint: GitIndexFingerprint,
    pub worktree_sha256: Option<[u8; 32]>,
    pub hunks: Vec<GitHunk>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHunkBufferInput {
    pub revision: u64,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug)]
pub struct GitHunkPolicy {
    pub scope_root: PathBuf,
    pub repository_id: GitRepositoryId,
    pub status_revision: u64,
    pub root_revision: u64,
    pub file: PathBuf,
    pub layer: GitDiffLayer,
    pub maximum_hunks: usize,
    pub maximum_lines: usize,
    pub maximum_bytes: usize,
    pub deadline: Duration,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitHunkError {
    #[error("invalid hunk policy")]
    InvalidPolicy,
    #[error("stale repository identity")]
    StaleRepository,
    #[error("Git index changed during hunk calculation")]
    StaleIndex,
    #[error("hunk path is outside workspace scope")]
    OutsideScope,
    #[error("binary diffs are unsupported")]
    BinaryUnsupported,
    #[error("hunk limit exceeded")]
    Limit,
    #[error("hunk deadline exceeded")]
    Deadline,
    #[error("Git hunk request was cancelled")]
    Cancelled,
    #[error("Git hunk identity collision")]
    IdentityCollision,
    #[error("Git hunk worker stopped")]
    Stopped,
    #[error("Git hunk backend failed: {0}")]
    Backend(String),
}
enum GitHunkEvent {
    Complete(Result<GitHunkSnapshot, GitHunkError>),
}
#[derive(Default)]
pub struct GitHunkService {
    next: u64,
    active: Option<GitHunkGeneration>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitHunkEvent>>,
    accepted: Option<GitHunkSnapshot>,
    revision: u64,
}
impl GitHunkService {
    pub fn start(&mut self, policy: GitHunkPolicy) -> Result<GitHunkGeneration, GitHunkError> {
        self.start_owned(policy, None)
    }

    pub fn start_buffer(
        &mut self,
        policy: GitHunkPolicy,
        input: GitHunkBufferInput,
    ) -> Result<GitHunkGeneration, GitHunkError> {
        if policy.layer != GitDiffLayer::Unstaged {
            return Err(GitHunkError::InvalidPolicy);
        }
        if input.bytes.len() > policy.maximum_bytes {
            return Err(GitHunkError::Limit);
        }
        self.start_owned(policy, Some(input))
    }

    fn start_owned(
        &mut self,
        policy: GitHunkPolicy,
        input: Option<GitHunkBufferInput>,
    ) -> Result<GitHunkGeneration, GitHunkError> {
        validate_hunk_policy(&policy)?;
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitHunkGeneration(self.next);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancellation);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-hunk-{}", generation.0))
            .spawn(move || {
                let result = match input {
                    Some(input) => read_buffer_hunks_owned(generation, &policy, input, &worker),
                    None => read_hunks_owned(generation, &policy, &worker),
                };
                let _ = tx.send(GitHunkEvent::Complete(result));
            })
            .map_err(|_| GitHunkError::Stopped)?;
        self.active = Some(generation);
        self.cancel = Some(cancellation);
        self.receiver = Some(rx);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Release)
        }
        self.active = None;
        self.receiver = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitHunkSnapshot>, GitHunkError> {
        let event = match self.receiver.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return Ok(None),
        };
        match event {
            Ok(GitHunkEvent::Complete(result)) => {
                let expected = self.active.take();
                self.receiver = None;
                self.cancel = None;
                let mut snapshot = result?;
                if expected != Some(snapshot.generation) {
                    return Ok(None);
                }
                let changed = self.accepted.as_ref().is_none_or(|old| {
                    old.repository_id != snapshot.repository_id
                        || old.status_revision != snapshot.status_revision
                        || old.root_revision != snapshot.root_revision
                        || old.file != snapshot.file
                        || old.layer != snapshot.layer
                        || old.buffer_revision != snapshot.buffer_revision
                        || old.hunks != snapshot.hunks
                });
                if changed {
                    self.revision = self.revision.saturating_add(1)
                }
                snapshot.revision = self.revision;
                self.accepted = Some(snapshot);
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.active = None;
                self.receiver = None;
                self.cancel = None;
                Err(GitHunkError::Stopped)
            }
        }
    }
    pub fn snapshot_if_changed(&self, last: u64) -> Option<&GitHunkSnapshot> {
        self.accepted.as_ref().filter(|s| s.revision != last)
    }
    pub const fn generation(&self) -> u64 {
        self.next
    }
}
impl Drop for GitHunkService {
    fn drop(&mut self) {
        self.cancel()
    }
}
fn validate_hunk_policy(p: &GitHunkPolicy) -> Result<(), GitHunkError> {
    if p.file.as_os_str().is_empty()
        || p.file.is_absolute()
        || p.file
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || p.maximum_hunks == 0
        || p.maximum_hunks > 10_000
        || p.maximum_lines == 0
        || p.maximum_lines > 100_000
        || p.maximum_bytes == 0
        || p.maximum_bytes > 8 * 1024 * 1024
        || p.deadline.is_zero()
        || p.deadline > Duration::from_secs(10)
    {
        Err(GitHunkError::InvalidPolicy)
    } else {
        Ok(())
    }
}

pub fn read_hunks(p: &GitHunkPolicy) -> Result<GitHunkSnapshot, GitHunkError> {
    read_hunks_owned(GitHunkGeneration(0), p, &AtomicBool::new(false))
}

fn open_hunk_repository(
    p: &GitHunkPolicy,
) -> Result<(PathBuf, git2::Repository, PathBuf, PathBuf), GitHunkError> {
    let root = std::fs::canonicalize(&p.scope_root)
        .map_err(|e| GitHunkError::Backend(e.to_string().chars().take(1024).collect()))?;
    let mut checked = root.clone();
    for component in &p.file {
        checked.push(component);
        match std::fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(GitHunkError::OutsideScope);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(GitHunkError::Backend(
                    error.to_string().chars().take(1024).collect(),
                ));
            }
        }
    }
    let repo = git2::Repository::discover(&root)
        .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    let workdir = std::fs::canonicalize(repo.workdir().ok_or(GitHunkError::OutsideScope)?)
        .map_err(|_| GitHunkError::OutsideScope)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    if GitRepositoryId(hasher.finish()) != p.repository_id {
        return Err(GitHunkError::StaleRepository);
    }
    let prefix = root
        .strip_prefix(&workdir)
        .map_err(|_| GitHunkError::OutsideScope)?;
    let repo_path = prefix.join(&p.file);
    if repo_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(GitHunkError::OutsideScope);
    }
    Ok((root, repo, workdir, repo_path))
}

fn collect_hunks_from_diff(
    diff: &git2::Diff<'_>,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
) -> Result<Vec<GitHunk>, GitHunkError> {
    if diff.deltas().any(|delta| delta.flags().is_binary()) {
        return Err(GitHunkError::BinaryUnsupported);
    }
    let started = Instant::now();
    let mut hunks: Vec<GitHunk> = Vec::new();
    let mut current_header = Vec::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;
    let mut abort = None;
    let print_result = diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
        if cancel.load(Ordering::Acquire) {
            abort = Some(GitHunkError::Cancelled);
            return false;
        }
        if started.elapsed() > p.deadline {
            abort = Some(GitHunkError::Deadline);
            return false;
        }
        if delta.flags().contains(git2::DiffFlags::BINARY) {
            abort = Some(GitHunkError::BinaryUnsupported);
            return false;
        }
        let Some(h) = hunk else { return true };
        let header = h.header().to_vec();
        if header != current_header {
            if hunks.len() >= p.maximum_hunks {
                abort = Some(GitHunkError::Limit);
                return false;
            }
            current_header = header.clone();
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            p.layer.hash(&mut hash);
            p.file.hash(&mut hash);
            p.status_revision.hash(&mut hash);
            header.hash(&mut hash);
            hunks.push(GitHunk {
                id: GitHunkId(hash.finish()),
                old_start: h.old_start(),
                old_lines: h.old_lines(),
                new_start: h.new_start(),
                new_lines: h.new_lines(),
                header: String::from_utf8_lossy(&header)
                    .chars()
                    .take(1024)
                    .collect(),
                lines: Vec::new(),
            });
        }
        let kind = match line.origin() {
            ' ' => Some(GitDiffLineKind::Context),
            '+' => Some(GitDiffLineKind::Add),
            '-' => Some(GitDiffLineKind::Delete),
            _ => None,
        };
        if let Some(kind) = kind {
            lines += 1;
            bytes = bytes.saturating_add(line.content().len());
            if lines > p.maximum_lines || bytes > p.maximum_bytes {
                abort = Some(GitHunkError::Limit);
                return false;
            }
            hunks.last_mut().unwrap().lines.push(GitDiffLine {
                kind,
                old_line: line.old_lineno(),
                new_line: line.new_lineno(),
                text: String::from_utf8_lossy(line.content()).into_owned(),
            });
        }
        true
    });
    if let Some(error) = abort {
        return Err(error);
    }
    print_result.map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    if cancel.load(Ordering::Acquire) {
        return Err(GitHunkError::Cancelled);
    }
    let mut identities = std::collections::BTreeSet::new();
    if hunks.iter().any(|h| !identities.insert(h.id)) {
        return Err(GitHunkError::IdentityCollision);
    }
    Ok(hunks)
}

fn read_hunks_owned(
    generation: GitHunkGeneration,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
) -> Result<GitHunkSnapshot, GitHunkError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitHunkError::Cancelled);
    }
    if p.file.as_os_str().is_empty()
        || p.file
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || p.file.is_absolute()
        || p.maximum_hunks == 0
        || p.maximum_hunks > 10_000
        || p.maximum_lines == 0
        || p.maximum_lines > 100_000
        || p.maximum_bytes == 0
        || p.maximum_bytes > 8 * 1024 * 1024
        || p.deadline.is_zero()
        || p.deadline > Duration::from_secs(10)
    {
        return Err(GitHunkError::InvalidPolicy);
    }
    let (root, repo, workdir, repo_path) = open_hunk_repository(p)?;
    let index_before =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    let mut options = git2::DiffOptions::new();
    options.pathspec(&repo_path);
    let diff = match p.layer {
        GitDiffLayer::Unstaged => repo.diff_index_to_workdir(None, Some(&mut options)),
        GitDiffLayer::Staged => {
            let tree = repo.head().and_then(|h| h.peel_to_tree()).map_err(|e| e);
            match tree {
                Ok(tree) => repo.diff_tree_to_index(Some(&tree), None, Some(&mut options)),
                Err(error) => Err(error),
            }
        }
    }
    .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    let hunks = collect_hunks_from_diff(&diff, p, cancel)?;
    let index_fingerprint =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    if index_fingerprint != index_before {
        return Err(GitHunkError::StaleIndex);
    }
    let worktree_sha256 = worktree_hash(&root.join(&p.file)).ok().map(|value| value.0);
    Ok(GitHunkSnapshot {
        generation,
        revision: 0,
        repository_id: p.repository_id,
        status_revision: p.status_revision,
        root_revision: p.root_revision,
        file: p.file.clone(),
        layer: p.layer,
        buffer_revision: None,
        index_fingerprint,
        worktree_sha256,
        hunks,
    })
}

fn collect_hunks_from_patch(
    patch: &git2::Patch<'_>,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
) -> Result<Vec<GitHunk>, GitHunkError> {
    if patch.delta().flags().contains(git2::DiffFlags::BINARY) {
        return Err(GitHunkError::BinaryUnsupported);
    }
    let started = Instant::now();
    let mut hunks = Vec::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;
    for hunk_index in 0..patch.num_hunks() {
        if cancel.load(Ordering::Acquire) {
            return Err(GitHunkError::Cancelled);
        }
        if started.elapsed() > p.deadline {
            return Err(GitHunkError::Deadline);
        }
        if hunks.len() >= p.maximum_hunks {
            return Err(GitHunkError::Limit);
        }
        let (hunk, line_count) = patch
            .hunk(hunk_index)
            .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
        let header = hunk.header().to_vec();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        p.layer.hash(&mut hash);
        p.file.hash(&mut hash);
        p.status_revision.hash(&mut hash);
        header.hash(&mut hash);
        let mut accepted = GitHunk {
            id: GitHunkId(hash.finish()),
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            header: String::from_utf8_lossy(&header)
                .chars()
                .take(1024)
                .collect(),
            lines: Vec::new(),
        };
        for line_index in 0..line_count {
            if cancel.load(Ordering::Acquire) {
                return Err(GitHunkError::Cancelled);
            }
            if started.elapsed() > p.deadline {
                return Err(GitHunkError::Deadline);
            }
            let line = patch
                .line_in_hunk(hunk_index, line_index)
                .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
            let kind = match line.origin() {
                ' ' => Some(GitDiffLineKind::Context),
                '+' => Some(GitDiffLineKind::Add),
                '-' => Some(GitDiffLineKind::Delete),
                _ => None,
            };
            if let Some(kind) = kind {
                lines += 1;
                bytes = bytes.saturating_add(line.content().len());
                if lines > p.maximum_lines || bytes > p.maximum_bytes {
                    return Err(GitHunkError::Limit);
                }
                accepted.lines.push(GitDiffLine {
                    kind,
                    old_line: line.old_lineno(),
                    new_line: line.new_lineno(),
                    text: String::from_utf8_lossy(line.content()).into_owned(),
                });
            }
        }
        hunks.push(accepted);
    }
    let mut identities = std::collections::BTreeSet::new();
    if hunks.iter().any(|hunk| !identities.insert(hunk.id)) {
        return Err(GitHunkError::IdentityCollision);
    }
    Ok(hunks)
}

fn read_buffer_hunks_owned(
    generation: GitHunkGeneration,
    p: &GitHunkPolicy,
    input: GitHunkBufferInput,
    cancel: &AtomicBool,
) -> Result<GitHunkSnapshot, GitHunkError> {
    validate_hunk_policy(p)?;
    if p.layer != GitDiffLayer::Unstaged || input.bytes.len() > p.maximum_bytes {
        return Err(GitHunkError::InvalidPolicy);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(GitHunkError::Cancelled);
    }
    let (_root, repo, workdir, repo_path) = open_hunk_repository(p)?;
    let index_before =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    let index = repo
        .index()
        .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    let index_blob = index
        .get_path(&repo_path, 0)
        .map(|entry| {
            repo.find_blob(entry.id)
                .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))
        })
        .transpose()?;
    let mut options = git2::DiffOptions::new();
    let patch = match index_blob.as_ref() {
        Some(blob) => git2::Patch::from_blob_and_buffer(
            blob,
            Some(&repo_path),
            &input.bytes,
            Some(&repo_path),
            Some(&mut options),
        ),
        None => git2::Patch::from_buffers(
            &[],
            Some(&repo_path),
            &input.bytes,
            Some(&repo_path),
            Some(&mut options),
        ),
    }
    .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    let hunks = collect_hunks_from_patch(&patch, p, cancel)?;
    if cancel.load(Ordering::Acquire) {
        return Err(GitHunkError::Cancelled);
    }
    let index_fingerprint =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    if index_fingerprint != index_before {
        return Err(GitHunkError::StaleIndex);
    }
    Ok(GitHunkSnapshot {
        generation,
        revision: 0,
        repository_id: p.repository_id,
        status_revision: p.status_revision,
        root_revision: p.root_revision,
        file: p.file.clone(),
        layer: p.layer,
        buffer_revision: Some(input.revision),
        index_fingerprint,
        worktree_sha256: None,
        hunks,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitIndexFingerprint(pub [u8; 32]);
pub fn git_index_fingerprint(repository: &Path) -> Result<GitIndexFingerprint, GitMutationError> {
    use sha2::{Digest, Sha256};
    let repo = git2::Repository::open(repository).map_err(|_| GitMutationError::Backend)?;
    let index = repo.index().map_err(|_| GitMutationError::Backend)?;
    let mut entries = index.iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.flags.cmp(&b.flags)));
    let mut hash = Sha256::new();
    for e in entries {
        hash.update((e.path.len() as u64).to_le_bytes());
        hash.update(&e.path);
        hash.update(e.mode.to_le_bytes());
        hash.update(e.id.as_bytes());
        hash.update(e.flags.to_le_bytes());
        hash.update(e.flags_extended.to_le_bytes());
    }
    Ok(GitIndexFingerprint(hash.finalize().into()))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitMutationGeneration(pub u64);
#[derive(Clone, Debug)]
pub struct GitStageHunkRequest {
    pub policy: GitHunkPolicy,
    pub hunk_snapshot_revision: u64,
    pub hunk_id: GitHunkId,
    pub selected: GitHunk,
    pub expected_index: Option<GitIndexFingerprint>,
    pub expected_worktree: Option<[u8; 32]>,
    pub maximum_patch_bytes: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub struct GitResetHunkRequest {
    pub policy: GitHunkPolicy,
    pub hunk_snapshot_revision: u64,
    pub hunk_id: GitHunkId,
    pub selected: GitHunk,
    pub expected_index: Option<GitIndexFingerprint>,
    pub expected_worktree: Option<[u8; 32]>,
    pub maximum_patch_bytes: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub enum GitMutationRequest {
    StageHunk(GitStageHunkRequest),
    ResetUnstagedHunk(GitResetHunkRequest),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitMutationKind {
    StageHunk,
    ResetUnstagedHunk,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitMutationResult {
    pub generation: GitMutationGeneration,
    pub kind: GitMutationKind,
    pub before: GitIndexFingerprint,
    pub after: GitIndexFingerprint,
    pub hunk: GitHunkId,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitMutationError {
    #[error("invalid Git mutation request")]
    Invalid,
    #[error("Git index identity is stale")]
    StaleIndex,
    #[error("Git worktree identity is stale")]
    StaleWorktree,
    #[error("Git repository or root identity is stale")]
    StaleRepository,
    #[error("Git hunk identity is stale")]
    WrongHunk,
    #[error("Git mutation path is outside the canonical workspace scope")]
    OutsideScope,
    #[error("Git mutation path type is unsupported")]
    UnsupportedPath,
    #[error("Git mutation deadline exceeded")]
    Deadline,
    #[error("Git mutation was cancelled")]
    Cancelled,
    #[error("Git mutation worker stopped")]
    Stopped,
    #[error("Git mutation backend failed")]
    Backend,
    #[error("Git mutation postcondition failed after apply")]
    Postcondition,
}
enum GitMutationEvent {
    Complete(Result<GitMutationResult, GitMutationError>),
}
#[derive(Default)]
pub struct GitMutationService {
    next: u64,
    active: Option<GitMutationGeneration>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitMutationEvent>>,
    accepted: Option<GitMutationResult>,
}
impl GitMutationService {
    pub fn start(
        &mut self,
        request: GitMutationRequest,
    ) -> Result<GitMutationGeneration, GitMutationError> {
        let (maximum_patch_bytes, deadline, snapshot_revision, layer, policy, exact) =
            match &request {
                GitMutationRequest::StageHunk(request) => (
                    request.maximum_patch_bytes,
                    request.deadline,
                    request.hunk_snapshot_revision,
                    request.policy.layer,
                    &request.policy,
                    request.expected_index.is_some() && request.expected_worktree.is_some(),
                ),
                GitMutationRequest::ResetUnstagedHunk(request) => (
                    request.maximum_patch_bytes,
                    request.deadline,
                    request.hunk_snapshot_revision,
                    request.policy.layer,
                    &request.policy,
                    request.expected_index.is_some() && request.expected_worktree.is_some(),
                ),
            };
        if maximum_patch_bytes == 0
            || maximum_patch_bytes > 1024 * 1024
            || deadline.is_zero()
            || deadline > Duration::from_secs(5)
            || snapshot_revision == 0
            || layer != GitDiffLayer::Unstaged
            || !exact
            || validate_hunk_policy(policy).is_err()
        {
            return Err(GitMutationError::Invalid);
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitMutationGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-mutation-{}", generation.0))
            .spawn(move || {
                let result = match request {
                    GitMutationRequest::StageHunk(request) => {
                        stage_hunk(generation, &request, &worker)
                    }
                    GitMutationRequest::ResetUnstagedHunk(request) => {
                        reset_hunk(generation, &request, &worker)
                    }
                };
                let _ = tx.send(GitMutationEvent::Complete(result));
            })
            .map_err(|_| GitMutationError::Stopped)?;
        self.active = Some(generation);
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Release)
        }
        self.active = None;
        self.receiver = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitMutationResult>, GitMutationError> {
        let event = match self.receiver.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return Ok(None),
        };
        match event {
            Ok(GitMutationEvent::Complete(result)) => {
                let expected = self.active.take();
                self.receiver = None;
                self.cancel = None;
                let result = result?;
                if expected != Some(result.generation) {
                    return Ok(None);
                }
                self.accepted = Some(result);
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.cancel();
                Err(GitMutationError::Stopped)
            }
        }
    }
}
impl Drop for GitMutationService {
    fn drop(&mut self) {
        self.cancel()
    }
}
fn worktree_hash(path: &Path) -> Result<([u8; 32], Vec<u8>), GitMutationError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|_| GitMutationError::Backend)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(GitMutationError::Invalid);
    }
    let hash = Sha256::digest(&bytes).into();
    Ok((hash, bytes))
}
fn mutation_hunk_error(error: GitHunkError) -> GitMutationError {
    match error {
        GitHunkError::Cancelled => GitMutationError::Cancelled,
        GitHunkError::Deadline => GitMutationError::Deadline,
        GitHunkError::OutsideScope => GitMutationError::OutsideScope,
        GitHunkError::BinaryUnsupported => GitMutationError::UnsupportedPath,
        GitHunkError::StaleRepository => GitMutationError::StaleRepository,
        GitHunkError::StaleIndex => GitMutationError::StaleIndex,
        _ => GitMutationError::WrongHunk,
    }
}

fn exact_mutation_repository(
    policy: &GitHunkPolicy,
) -> Result<(PathBuf, git2::Repository, PathBuf), GitMutationError> {
    validate_hunk_policy(policy).map_err(|_| GitMutationError::Invalid)?;
    let root =
        std::fs::canonicalize(&policy.scope_root).map_err(|_| GitMutationError::StaleRepository)?;
    let repository =
        git2::Repository::discover(&root).map_err(|_| GitMutationError::StaleRepository)?;
    let workdir = std::fs::canonicalize(
        repository
            .workdir()
            .ok_or(GitMutationError::StaleRepository)?,
    )
    .map_err(|_| GitMutationError::StaleRepository)?;
    if !root.starts_with(&workdir) {
        return Err(GitMutationError::OutsideScope);
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    if GitRepositoryId(hasher.finish()) != policy.repository_id {
        return Err(GitMutationError::StaleRepository);
    }
    let mut checked = root.clone();
    for component in &policy.file {
        checked.push(component);
        match std::fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(GitMutationError::OutsideScope);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(GitMutationError::Backend),
        }
    }
    Ok((root, repository, workdir))
}

fn reset_hunk(
    generation: GitMutationGeneration,
    r: &GitResetHunkRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, repository) = exact_mutation_repository(&r.policy)?;
    let before = git_index_fingerprint(&repository)?;
    if r.expected_index != Some(before) {
        return Err(GitMutationError::StaleIndex);
    }
    let file = root.join(&r.policy.file);
    let metadata =
        std::fs::symlink_metadata(&file).map_err(|_| GitMutationError::UnsupportedPath)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(GitMutationError::UnsupportedPath);
    }
    let (pre_hash, pre_bytes) = worktree_hash(&file)?;
    if r.expected_worktree != Some(pre_hash) {
        return Err(GitMutationError::StaleWorktree);
    }
    let snapshot =
        read_hunks_owned(GitHunkGeneration(0), &r.policy, cancel).map_err(mutation_hunk_error)?;
    let current = snapshot
        .hunks
        .iter()
        .find(|h| h.id == r.hunk_id)
        .ok_or(GitMutationError::WrongHunk)?;
    if current != &r.selected {
        return Err(GitMutationError::WrongHunk);
    }
    let Some(name) = r.policy.file.to_str().filter(|s| !s.contains(['\n', '\r'])) else {
        return Err(GitMutationError::Invalid);
    };
    let mut patch = format!(
        "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{3},{4} @@\n",
        name, current.new_start, current.new_lines, current.old_start, current.old_lines
    );
    for line in &current.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '-',
            GitDiffLineKind::Delete => '+',
        });
        patch.push_str(&line.text)
    }
    if patch.len() > r.maximum_patch_bytes {
        return Err(GitMutationError::Invalid);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    if started.elapsed() > r.deadline {
        return Err(GitMutationError::Deadline);
    }
    if git_index_fingerprint(&repository)? != before {
        return Err(GitMutationError::StaleIndex);
    }
    if worktree_hash(&file)?.0 != pre_hash {
        return Err(GitMutationError::StaleWorktree);
    }
    let diff = git2::Diff::from_buffer(patch.as_bytes()).map_err(|_| GitMutationError::Backend)?;
    repo.apply(&diff, git2::ApplyLocation::WorkDir, None)
        .map_err(|_| GitMutationError::Backend)?;
    let after = git_index_fingerprint(&repository)?;
    let (post_hash, post_bytes) = worktree_hash(&file)?;
    if after != before || post_hash == pre_hash || post_bytes == pre_bytes {
        return Err(GitMutationError::Postcondition);
    }
    let remaining = read_hunks_owned(GitHunkGeneration(0), &r.policy, cancel)
        .map_err(|_| GitMutationError::Postcondition)?;
    if remaining.hunks.iter().any(|h| h.id == r.hunk_id) {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        generation,
        kind: GitMutationKind::ResetUnstagedHunk,
        before,
        after,
        hunk: r.hunk_id,
    })
}

fn stage_hunk(
    generation: GitMutationGeneration,
    r: &GitStageHunkRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, repository) = exact_mutation_repository(&r.policy)?;
    let before = git_index_fingerprint(&repository)?;
    if r.expected_index != Some(before) {
        return Err(GitMutationError::StaleIndex);
    }
    let file = root.join(&r.policy.file);
    let metadata =
        std::fs::symlink_metadata(&file).map_err(|_| GitMutationError::UnsupportedPath)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(GitMutationError::UnsupportedPath);
    }
    let (worktree_hash_before, worktree) = worktree_hash(&file)?;
    if r.expected_worktree != Some(worktree_hash_before) {
        return Err(GitMutationError::StaleWorktree);
    }
    let snapshot =
        read_hunks_owned(GitHunkGeneration(0), &r.policy, cancel).map_err(mutation_hunk_error)?;
    let current = snapshot
        .hunks
        .iter()
        .find(|h| h.id == r.hunk_id)
        .ok_or(GitMutationError::WrongHunk)?;
    if current != &r.selected {
        return Err(GitMutationError::WrongHunk);
    }
    if started.elapsed() > r.deadline {
        return Err(GitMutationError::Deadline);
    }
    let mut patch = format!(
        "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n{1}",
        r.policy.file.to_string_lossy(),
        current.header
    );
    for line in &current.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '+',
            GitDiffLineKind::Delete => '-',
        });
        patch.push_str(&line.text)
    }
    if patch.len() > r.maximum_patch_bytes {
        return Err(GitMutationError::Invalid);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    if started.elapsed() > r.deadline {
        return Err(GitMutationError::Deadline);
    }
    if git_index_fingerprint(&repository)? != before {
        return Err(GitMutationError::StaleIndex);
    }
    if worktree_hash(&file)?.0 != worktree_hash_before {
        return Err(GitMutationError::StaleWorktree);
    }
    let diff = git2::Diff::from_buffer(patch.as_bytes()).map_err(|_| GitMutationError::Backend)?;
    repo.apply(&diff, git2::ApplyLocation::Index, None)
        .map_err(|_| GitMutationError::Backend)?;
    let after = git_index_fingerprint(&repository)?;
    if before == after || worktree_hash(&file)?.1 != worktree {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        generation,
        kind: GitMutationKind::StageHunk,
        before,
        after,
        hunk: r.hunk_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn commit(repo: &git2::Repository, path: &str, text: &str) {
        let root = repo.workdir().unwrap();
        std::fs::write(root.join(path), text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
    }
    #[test]
    fn status_is_bounded_sorted_and_idle() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "tracked.txt", "one");
        std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new").unwrap();
        let mut service = GitStatusService::default();
        service
            .start(GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 3,
                maximum_files: 10,
                maximum_path_bytes: 1024,
                deadline: Duration::from_secs(2),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            if let Some(s) = service.poll().unwrap() {
                break s.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert_eq!(
            snapshot.entries,
            vec![
                GitStatusEntry {
                    relative_path: "new.txt".into(),
                    status: GitFileStatus::Untracked
                },
                GitStatusEntry {
                    relative_path: "tracked.txt".into(),
                    status: GitFileStatus::Modified
                }
            ]
        );
        for _ in 0..256 {
            assert!(service.snapshot_if_changed(snapshot.revision).is_none())
        }
    }
    #[test]
    fn nested_root_is_scoped_and_ceilings_truncate() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        commit(&repo, "nested/tracked.txt", "one");
        std::fs::write(dir.path().join("sibling.txt"), "outside").unwrap();
        std::fs::write(dir.path().join("nested/tracked.txt"), "two").unwrap();
        std::fs::write(dir.path().join("nested/new.txt"), "new").unwrap();
        let mut service = GitStatusService::default();
        service
            .start(GitStatusPolicy {
                root: dir.path().join("nested"),
                root_revision: 4,
                maximum_files: 1,
                maximum_path_bytes: 1024,
                deadline: Duration::from_secs(2),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            if let Some(s) = service.poll().unwrap() {
                break s.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert!(snapshot.truncated);
        assert_eq!(snapshot.entries.len(), 1);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|e| !e.relative_path.starts_with("sibling.txt"))
        );
        assert_eq!(
            GitStatusService::default()
                .start(GitStatusPolicy {
                    root: dir.path().into(),
                    root_revision: 1,
                    maximum_files: 0,
                    maximum_path_bytes: 1,
                    deadline: Duration::from_secs(1)
                })
                .unwrap_err()
                .to_string(),
            "invalid git status policy"
        );
    }
    #[test]
    fn unstaged_unicode_hunks_are_bounded_and_revision_owned() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "u.txt", "alpha βeta\nlast\n");
        let workdir = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        workdir.hash(&mut hasher);
        let repository_id = GitRepositoryId(hasher.finish());
        std::fs::write(dir.path().join("u.txt"), "alpha γamma\nlast\n").unwrap();
        let policy = GitHunkPolicy {
            scope_root: workdir,
            repository_id,
            status_revision: 4,
            root_revision: 3,
            file: "u.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let mut service = GitHunkService::default();
        let generation = service.start(policy.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            if let Some(snapshot) = service.poll().unwrap() {
                break snapshot.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert_eq!(snapshot.generation, generation);
        for _ in 0..256 {
            assert!(service.snapshot_if_changed(snapshot.revision).is_none())
        }
        assert_eq!(snapshot.status_revision, 4);
        assert_eq!(snapshot.hunks.len(), 1);
        assert!(
            snapshot.hunks[0]
                .lines
                .iter()
                .any(|line| line.kind == GitDiffLineKind::Add && line.text.contains("γamma"))
        );
        let mut tiny = policy;
        tiny.maximum_lines = 1;
        assert_eq!(read_hunks(&tiny), Err(GitHunkError::Limit));
    }
    #[test]
    fn buffer_hunks_compare_exact_unsaved_revision_to_index_without_worktree_mutation() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "live.txt", "one\ntwo\nthree\n");
        let workdir = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        workdir.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: workdir,
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 9,
            root_revision: 4,
            file: "live.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        assert!(read_hunks(&policy).unwrap().hunks.is_empty());
        let mut service = GitHunkService::default();
        service
            .start_buffer(
                policy.clone(),
                GitHunkBufferInput {
                    revision: 7,
                    bytes: b"one\nthree\n".to_vec(),
                },
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            if let Some(snapshot) = service.poll().unwrap() {
                break snapshot.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert_eq!(snapshot.buffer_revision, Some(7));
        assert_eq!(snapshot.worktree_sha256, None);
        assert!(
            snapshot
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.kind == GitDiffLineKind::Delete && line.text == "two\n")
        );
        assert!(
            git_hunk_signs(&snapshot)
                .iter()
                .any(|sign| sign.kind == GitHunkSignKind::Deleted)
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("live.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(
            service.start_buffer(
                GitHunkPolicy {
                    layer: GitDiffLayer::Staged,
                    ..policy.clone()
                },
                GitHunkBufferInput {
                    revision: 8,
                    bytes: Vec::new(),
                },
            ),
            Err(GitHunkError::InvalidPolicy)
        );
        assert_eq!(
            service.start_buffer(
                GitHunkPolicy {
                    maximum_bytes: 1,
                    ..policy
                },
                GitHunkBufferInput {
                    revision: 8,
                    bytes: b"xx".to_vec(),
                },
            ),
            Err(GitHunkError::Limit)
        );
    }

    #[test]
    fn libgit2_can_stage_one_exact_hunk_without_changing_worktree() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        commit(&repo, "two.txt", &original);
        let changed = original
            .replace("line 2\n", "changed 2\n")
            .replace("line 18\n", "changed 18\n");
        std::fs::write(dir.path().join("two.txt"), &changed).unwrap();
        let workdir = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        workdir.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: workdir,
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 1,
            root_revision: 1,
            file: "two.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        assert_eq!(snapshot.hunks.len(), 2);
        let h = &snapshot.hunks[0];
        let mut patch = format!(
            "diff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n{}",
            h.header
        );
        for line in &h.lines {
            patch.push(match line.kind {
                GitDiffLineKind::Context => ' ',
                GitDiffLineKind::Add => '+',
                GitDiffLineKind::Delete => '-',
            });
            patch.push_str(&line.text)
        }
        let diff = git2::Diff::from_buffer(patch.as_bytes()).unwrap();
        repo.apply(&diff, git2::ApplyLocation::Index, None).unwrap();
        let staged = read_hunks(&GitHunkPolicy {
            layer: GitDiffLayer::Staged,
            ..policy.clone()
        })
        .unwrap();
        let unstaged = read_hunks(&policy).unwrap();
        assert_eq!(staged.hunks.len(), 1);
        assert_eq!(unstaged.hunks.len(), 1);
        assert!(
            unstaged.hunks[0]
                .lines
                .iter()
                .any(|l| l.text.contains("changed 18"))
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two.txt")).unwrap(),
            changed
        );
    }
    #[test]
    fn mutation_service_stages_exact_hunk_and_rejects_stale_index() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        commit(&repo, "m.txt", &original);
        let changed = original
            .replace("line 2\n", "changed 2\n")
            .replace("line 18\n", "changed 18\n");
        std::fs::write(dir.path().join("m.txt"), &changed).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: root.clone(),
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 1,
            root_revision: 1,
            file: "m.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        let index = git_index_fingerprint(&root).unwrap();
        let request = GitStageHunkRequest {
            policy: policy.clone(),
            hunk_snapshot_revision: 1,
            hunk_id: snapshot.hunks[0].id,
            selected: snapshot.hunks[0].clone(),
            expected_index: Some(index),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 64 * 1024,
            deadline: Duration::from_secs(2),
        };
        let mut service = GitMutationService::default();
        let generation = service
            .start(GitMutationRequest::StageHunk(request.clone()))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let result = loop {
            if let Some(result) = service.poll().unwrap() {
                break result.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert_eq!(result.generation, generation);
        assert_eq!(result.kind, GitMutationKind::StageHunk);
        assert_ne!(result.before, result.after);
        assert_eq!(
            std::fs::read_to_string(root.join("m.txt")).unwrap(),
            changed
        );
        let mut stale = request;
        stale.expected_index = Some(index);
        let current = git_index_fingerprint(&root).unwrap();
        let mut second = GitMutationService::default();
        second.start(GitMutationRequest::StageHunk(stale)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match second.poll() {
                Err(GitMutationError::StaleIndex) => break,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
        assert_eq!(git_index_fingerprint(&root).unwrap(), current);
    }
    #[test]
    fn libgit2_can_reset_one_exact_hunk_without_changing_index() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        commit(&repo, "reset.txt", &original);
        let changed = original
            .replace("line 2\n", "changed 2\n")
            .replace("line 18\n", "changed 18\n");
        std::fs::write(dir.path().join("reset.txt"), &changed).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: root.clone(),
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 1,
            root_revision: 1,
            file: "reset.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        assert_eq!(snapshot.hunks.len(), 2);
        let h = &snapshot.hunks[0];
        let mut patch = format!(
            "diff --git a/reset.txt b/reset.txt\n--- a/reset.txt\n+++ b/reset.txt\n@@ -{},{} +{},{} @@\n",
            h.new_start, h.new_lines, h.old_start, h.old_lines
        );
        for line in &h.lines {
            patch.push(match line.kind {
                GitDiffLineKind::Context => ' ',
                GitDiffLineKind::Add => '-',
                GitDiffLineKind::Delete => '+',
            });
            patch.push_str(&line.text)
        }
        let before = git_index_fingerprint(&root).unwrap();
        let diff = git2::Diff::from_buffer(patch.as_bytes()).unwrap();
        repo.apply(&diff, git2::ApplyLocation::WorkDir, None)
            .unwrap();
        assert_eq!(git_index_fingerprint(&root).unwrap(), before);
        let remaining = read_hunks(&policy).unwrap();
        assert_eq!(remaining.hunks.len(), 1);
        assert!(
            remaining.hunks[0]
                .lines
                .iter()
                .any(|line| line.text.contains("changed 18"))
        );
        assert!(
            std::fs::read_to_string(root.join("reset.txt"))
                .unwrap()
                .contains("line 2\n")
        );
        assert!(
            read_hunks(&GitHunkPolicy {
                layer: GitDiffLayer::Staged,
                ..policy
            })
            .unwrap()
            .hunks
            .is_empty()
        );
    }
    fn wait_mutation_error(service: &mut GitMutationService) -> GitMutationError {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match service.poll() {
                Err(error) => return error,
                Ok(_) => {}
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn mutation_service_rejects_stale_worktree_wrong_hunk_and_suppresses_cancelled_late_work() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "negative.txt", "one\ntwo\nthree\n");
        std::fs::write(dir.path().join("negative.txt"), "one\nchanged\nthree\n").unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: root.clone(),
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 9,
            root_revision: 4,
            file: "negative.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        let base = GitStageHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: snapshot.hunks[0].id,
            selected: snapshot.hunks[0].clone(),
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(2),
        };

        let mut wrong = base.clone();
        wrong.selected.header.push_str(" stale");
        let mut service = GitMutationService::default();
        service.start(GitMutationRequest::StageHunk(wrong)).unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::WrongHunk
        );

        let mut stale_worktree = base.clone();
        stale_worktree.expected_worktree = Some([0; 32]);
        service
            .start(GitMutationRequest::StageHunk(stale_worktree.clone()))
            .unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::StaleWorktree
        );
        assert_eq!(
            stage_hunk(GitMutationGeneration(77), &base, &AtomicBool::new(true)),
            Err(GitMutationError::Cancelled)
        );
        service
            .start(GitMutationRequest::StageHunk(stale_worktree))
            .unwrap();
        service.cancel();
        let generation = service.next;
        for _ in 0..256 {
            assert!(service.poll().unwrap().is_none());
            assert_eq!(service.next, generation);
        }
    }

    #[cfg(unix)]
    #[test]
    fn mutation_service_rejects_symlink_root_replacement_binary_and_directory_paths() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "valid.txt", "one\ntwo\n");
        std::fs::write(dir.path().join("valid.txt"), "one\nchanged\n").unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        let repository_id = GitRepositoryId(hasher.finish());
        let policy = GitHunkPolicy {
            scope_root: root.clone(),
            repository_id,
            status_revision: 3,
            root_revision: 2,
            file: "valid.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        let request_for = |policy: GitHunkPolicy, worktree: [u8; 32]| GitStageHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: snapshot.hunks[0].id,
            selected: snapshot.hunks[0].clone(),
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: Some(worktree),
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(2),
        };
        let mut service = GitMutationService::default();

        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("outside.txt"), "changed\n").unwrap();
        symlink(outside.path().join("outside.txt"), root.join("linked.txt")).unwrap();
        let mut symlink_policy = policy.clone();
        symlink_policy.file = "linked.txt".into();
        let linked_hash = worktree_hash(&root.join("linked.txt")).unwrap().0;
        service
            .start(GitMutationRequest::StageHunk(request_for(
                symlink_policy,
                linked_hash,
            )))
            .unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::OutsideScope
        );

        let replacement = tempdir().unwrap();
        git2::Repository::init(replacement.path()).unwrap();
        let mut replacement_policy = policy.clone();
        replacement_policy.scope_root = std::fs::canonicalize(replacement.path()).unwrap();
        service
            .start(GitMutationRequest::StageHunk(request_for(
                replacement_policy,
                snapshot.worktree_sha256.unwrap(),
            )))
            .unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::StaleRepository
        );

        std::fs::create_dir(root.join("directory")).unwrap();
        let mut directory_policy = policy.clone();
        directory_policy.file = "directory".into();
        service
            .start(GitMutationRequest::StageHunk(request_for(
                directory_policy,
                snapshot.worktree_sha256.unwrap(),
            )))
            .unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::UnsupportedPath
        );

        std::fs::write(root.join("binary.bin"), "plain\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("binary.bin")).unwrap();
        index.write().unwrap();
        std::fs::write(root.join("binary.bin"), b"\0binary\0").unwrap();
        let mut binary_policy = policy;
        binary_policy.file = "binary.bin".into();
        let binary_hash = worktree_hash(&root.join("binary.bin")).unwrap().0;
        let mut binary_request = request_for(binary_policy, binary_hash);
        binary_request.expected_index = Some(git_index_fingerprint(&root).unwrap());
        service
            .start(GitMutationRequest::StageHunk(binary_request))
            .unwrap();
        assert_eq!(
            wait_mutation_error(&mut service),
            GitMutationError::UnsupportedPath
        );
    }

    #[test]
    fn git_status_aggregation_and_hunk_signs_are_deterministic_and_typed() {
        let root = PathBuf::from("/workspace");
        let entries = vec![
            GitStatusEntry {
                relative_path: "src/a.rs".into(),
                status: GitFileStatus::Modified,
            },
            GitStatusEntry {
                relative_path: "src/nested/b.rs".into(),
                status: GitFileStatus::Conflicted,
            },
            GitStatusEntry {
                relative_path: "docs/readme.md".into(),
                status: GitFileStatus::Untracked,
            },
            GitStatusEntry {
                relative_path: "../escape".into(),
                status: GitFileStatus::Conflicted,
            },
        ];
        let aggregate = aggregate_git_status(&root, &entries);
        assert_eq!(aggregate[&root.join("src")], GitFileStatus::Conflicted);
        assert_eq!(aggregate[&root], GitFileStatus::Conflicted);
        assert_eq!(aggregate[&root.join("docs")], GitFileStatus::Untracked);
        assert!(!aggregate.contains_key(&root.join("../escape")));

        let snapshot = GitHunkSnapshot {
            generation: GitHunkGeneration(1),
            revision: 1,
            repository_id: GitRepositoryId(1),
            status_revision: 1,
            root_revision: 1,
            file: "a".into(),
            layer: GitDiffLayer::Unstaged,
            buffer_revision: None,
            index_fingerprint: GitIndexFingerprint([0; 32]),
            worktree_sha256: Some([1; 32]),
            hunks: vec![
                GitHunk {
                    id: GitHunkId(1),
                    old_start: 1,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: 1,
                    header: String::new(),
                    lines: vec![GitDiffLine {
                        kind: GitDiffLineKind::Add,
                        old_line: None,
                        new_line: Some(1),
                        text: "add\n".into(),
                    }],
                },
                GitHunk {
                    id: GitHunkId(2),
                    old_start: 3,
                    old_lines: 1,
                    new_start: 3,
                    new_lines: 1,
                    header: String::new(),
                    lines: vec![
                        GitDiffLine {
                            kind: GitDiffLineKind::Delete,
                            old_line: Some(3),
                            new_line: None,
                            text: "old\n".into(),
                        },
                        GitDiffLine {
                            kind: GitDiffLineKind::Add,
                            old_line: None,
                            new_line: Some(3),
                            text: "new\n".into(),
                        },
                    ],
                },
                GitHunk {
                    id: GitHunkId(3),
                    old_start: 5,
                    old_lines: 1,
                    new_start: 5,
                    new_lines: 0,
                    header: String::new(),
                    lines: vec![GitDiffLine {
                        kind: GitDiffLineKind::Delete,
                        old_line: Some(5),
                        new_line: None,
                        text: "gone\n".into(),
                    }],
                },
            ],
        };
        assert_eq!(
            git_hunk_signs(&snapshot),
            vec![
                GitHunkSign {
                    line: 0,
                    kind: GitHunkSignKind::Added
                },
                GitHunkSign {
                    line: 2,
                    kind: GitHunkSignKind::Changed
                },
                GitHunkSign {
                    line: 4,
                    kind: GitHunkSignKind::Deleted
                },
            ]
        );
    }

    #[test]
    fn reset_service_restores_only_selected_hunk_and_preserves_index() {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        commit(&repo, "service-reset.txt", &original);
        let changed = original
            .replace("line 2\n", "changed 2\n")
            .replace("line 18\n", "changed 18\n");
        std::fs::write(dir.path().join("service-reset.txt"), &changed).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        let policy = GitHunkPolicy {
            scope_root: root.clone(),
            repository_id: GitRepositoryId(hasher.finish()),
            status_revision: 1,
            root_revision: 1,
            file: "service-reset.txt".into(),
            layer: GitDiffLayer::Unstaged,
            maximum_hunks: 10,
            maximum_lines: 100,
            maximum_bytes: 4096,
            deadline: Duration::from_secs(2),
        };
        let snapshot = read_hunks(&policy).unwrap();
        let before = git_index_fingerprint(&root).unwrap();
        let hash = worktree_hash(&root.join("service-reset.txt")).unwrap().0;
        let request = GitResetHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: snapshot.hunks[0].id,
            selected: snapshot.hunks[0].clone(),
            expected_index: Some(before),
            expected_worktree: Some(hash),
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(2),
        };
        let mut service = GitMutationService::default();
        service
            .start(GitMutationRequest::ResetUnstagedHunk(request))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let result = loop {
            if let Some(result) = service.poll().unwrap() {
                break result.clone();
            }
            assert!(Instant::now() < deadline);
            thread::yield_now()
        };
        assert_eq!(result.kind, GitMutationKind::ResetUnstagedHunk);
        assert_eq!(result.before, result.after);
        assert_eq!(git_index_fingerprint(&root).unwrap(), before);
        let text = std::fs::read_to_string(root.join("service-reset.txt")).unwrap();
        assert!(text.contains("line 2\n") && text.contains("changed 18\n"));
    }
}
