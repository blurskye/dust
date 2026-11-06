//! Diff hunks, signs and background hunk reads.

use std::hash::Hash;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;

use crate::git::mutation::GitIndexFingerprint;

use crate::git::mutation::git_index_fingerprint;

use crate::git::mutation::worktree_hash;

use crate::git::status::GitChangeSource;

use crate::git::status::GitRepositoryId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GitDiffLayer {
    Unstaged,
    Staged,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitHunkId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitHunkRequestId(pub u64);
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
    pub old_mode: u32,
    pub new_mode: u32,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GitChangeRun {
    /// One-based line on the new side where the run starts.
    start: u32,
    added: u32,
    deleted: bool,
    /// Where the run sits in the hunk's `lines`.
    first: u32,
    last: u32,
}

impl GitHunk {
    /// Git keeps context around changes and joins near ones into one hunk, so a hunk can hold
    /// several separate changes.
    fn change_runs(&self) -> Vec<GitChangeRun> {
        let mut runs = Vec::new();
        let mut current: Option<GitChangeRun> = None;
        // With no lines on the new side, git names the line before the change.
        let mut next = self.new_start + u32::from(self.new_lines == 0);
        for (index, line) in self.lines.iter().enumerate() {
            let index = index as u32;
            if line.kind == GitDiffLineKind::Context {
                runs.extend(current.take());
                next += 1;
                continue;
            }
            let run = current.get_or_insert(GitChangeRun {
                start: next,
                added: 0,
                deleted: false,
                first: index,
                last: index,
            });
            run.last = index;
            if line.kind == GitDiffLineKind::Add {
                run.added += 1;
                next += 1;
            } else {
                run.deleted = true;
            }
        }
        runs.extend(current);
        runs
    }

    /// The one-based lines a person lands on for each change in this hunk. A deletion has no line
    /// of its own, so it lands on the line above.
    #[must_use]
    pub fn change_lines(&self) -> Vec<u32> {
        self.change_runs()
            .into_iter()
            .map(|run| {
                if run.added > 0 {
                    run.start
                } else {
                    run.start.saturating_sub(1).max(1)
                }
            })
            .collect()
    }

    /// The positions in `lines` of the change on this one-based line, when the hunk holds several
    /// changes and this line is in one of them. Empty means the hunk as a whole.
    #[must_use]
    pub fn change_at(&self, line: u32) -> Vec<u32> {
        let runs = self.change_runs();
        if runs.len() < 2 {
            return Vec::new();
        }
        runs.iter()
            .find(|run| {
                let lands = if run.added > 0 {
                    run.start
                } else {
                    run.start.saturating_sub(1).max(1)
                };
                line >= lands && line < lands + run.added.max(1)
            })
            .map_or_else(Vec::new, |run| (run.first..=run.last).collect())
    }

    /// The diff lines to stage for these one-based file lines. An added line brings the removed
    /// line it replaces, and the last added line of a change brings any removed lines left over.
    #[must_use]
    pub fn changes_on_lines(&self, start: u32, end: u32) -> Vec<u32> {
        let mut chosen = Vec::new();
        for run in self.change_runs() {
            let offsets = run.first..=run.last;
            let of_kind = |kind: GitDiffLineKind| {
                offsets
                    .clone()
                    .filter(|index| self.lines[*index as usize].kind == kind)
                    .collect::<Vec<u32>>()
            };
            let (removed, added) = (
                of_kind(GitDiffLineKind::Delete),
                of_kind(GitDiffLineKind::Add),
            );
            if added.is_empty() {
                let lands = run.start.saturating_sub(1).max(1);
                if (start..=end).contains(&lands) {
                    chosen.extend(removed);
                }
                continue;
            }
            for (position, offset) in added.iter().enumerate() {
                if !(start..=end).contains(&(run.start + position as u32)) {
                    continue;
                }
                chosen.push(*offset);
                if position + 1 == added.len() {
                    chosen.extend(removed.iter().skip(position));
                } else {
                    chosen.extend(removed.get(position));
                }
            }
        }
        chosen.sort_unstable();
        chosen
    }

    /// Whether this one-based line is part of the hunk, its context included.
    #[must_use]
    pub fn covers_line(&self, line: u32) -> bool {
        let start = self.new_start.max(1);
        line >= start && line < start + self.new_lines.max(1)
    }
}

#[must_use]
pub fn git_hunk_signs(snapshot: &GitHunkSnapshot) -> Vec<GitHunkSign> {
    let mut signs = std::collections::BTreeMap::new();
    let mut mark = |line: usize, kind: GitHunkSignKind| {
        signs
            .entry(line)
            .and_modify(|existing| {
                if kind > *existing {
                    *existing = kind;
                }
            })
            .or_insert(kind);
    };
    for run in snapshot.hunks.iter().flat_map(GitHunk::change_runs) {
        let first = run.start.saturating_sub(1) as usize;
        match (run.added, run.deleted) {
            (0, _) => mark(first.saturating_sub(1), GitHunkSignKind::Deleted),
            (added, deleted) => {
                let kind = if deleted {
                    GitHunkSignKind::Changed
                } else {
                    GitHunkSignKind::Added
                };
                for line in first..first + added as usize {
                    mark(line, kind);
                }
            }
        }
    }
    signs
        .into_iter()
        .map(|(line, kind)| GitHunkSign { line, kind })
        .collect()
}

/// Staged changes as signs on the buffer's lines. Their lines are the index's, so the
/// unstaged diff moves them. A line it changed shows that change and has no staged sign.
#[must_use]
pub fn git_staged_signs(staged: &GitHunkSnapshot, unstaged: &GitHunkSnapshot) -> Vec<GitHunkSign> {
    let changed = git_hunk_signs(unstaged)
        .into_iter()
        .map(|sign| sign.line)
        .collect::<std::collections::BTreeSet<_>>();
    git_hunk_signs(staged)
        .into_iter()
        .filter_map(|sign| {
            let line = index_line_in_buffer(sign.line, &unstaged.hunks)?;
            (!changed.contains(&line)).then_some(GitHunkSign { line, ..sign })
        })
        .collect()
}

/// Where a zero-based index line sits in the buffer, if it is still there.
pub fn index_line_in_buffer(line: usize, unstaged: &[GitHunk]) -> Option<usize> {
    let line = u32::try_from(line + 1).ok()?;
    let mut shift = 0i64;
    for hunk in unstaged {
        // With no lines on a side, git names the line before the change.
        let first_old = hunk.old_start + u32::from(hunk.old_lines == 0);
        if line < first_old {
            break;
        }
        let next_old = first_old + hunk.old_lines;
        if line < next_old {
            let kept = hunk.lines.iter().find(|diff| diff.old_line == Some(line));
            return kept
                .and_then(|diff| diff.new_line)
                .map(|new| new as usize - 1);
        }
        let first_new = hunk.new_start + u32::from(hunk.new_lines == 0);
        shift = i64::from(first_new + hunk.new_lines) - i64::from(next_old);
    }
    usize::try_from(i64::from(line) + shift - 1).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHunkSnapshot {
    pub request_id: GitHunkRequestId,
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
impl GitHunkSnapshot {
    /// State what this snapshot is relative to. Staged is HEAD to index; unstaged is index to
    /// disk; a snapshot computed from editor bytes is the unsaved source at that exact
    /// document revision. A caller never has to infer this from the layer alone.
    #[must_use]
    pub fn source(&self) -> GitChangeSource {
        match (self.layer, self.buffer_revision) {
            (GitDiffLayer::Staged, _) => GitChangeSource::Staged,
            (GitDiffLayer::Unstaged, Some(document_revision)) => {
                GitChangeSource::Unsaved { document_revision }
            }
            (GitDiffLayer::Unstaged, None) => GitChangeSource::Unstaged,
        }
    }
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
    #[error("unmerged paths cannot be compared to the index")]
    Unmerged,
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
pub(super) enum GitHunkEvent {
    Complete(Result<GitHunkSnapshot, GitHunkError>),
}
#[derive(Default)]
pub struct GitHunkWorker {
    next: u64,
    active: Option<GitHunkRequestId>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitHunkEvent>>,
    accepted: Option<GitHunkSnapshot>,
    revision: u64,
}
impl GitHunkWorker {
    pub fn start(&mut self, policy: GitHunkPolicy) -> Result<GitHunkRequestId, GitHunkError> {
        self.start_owned(policy, None)
    }

    pub fn start_buffer(
        &mut self,
        policy: GitHunkPolicy,
        input: GitHunkBufferInput,
    ) -> Result<GitHunkRequestId, GitHunkError> {
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
    ) -> Result<GitHunkRequestId, GitHunkError> {
        validate_hunk_policy(&policy)?;
        self.cancel();
        self.next = self.next.saturating_add(1);
        let request_id = GitHunkRequestId(self.next);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancellation);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-hunk-{}", request_id.0))
            .spawn(move || {
                let result = match input {
                    Some(input) => read_buffer_hunks_owned(request_id, &policy, input, &worker),
                    None => read_hunks_owned(request_id, &policy, &worker),
                };
                let _ = tx.send(GitHunkEvent::Complete(result));
            })
            .map_err(|_| GitHunkError::Stopped)?;
        self.active = Some(request_id);
        self.cancel = Some(cancellation);
        self.receiver = Some(rx);
        Ok(request_id)
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
                if expected != Some(snapshot.request_id) {
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
    pub const fn request_id(&self) -> u64 {
        self.next
    }
}
impl Drop for GitHunkWorker {
    fn drop(&mut self) {
        self.cancel()
    }
}
pub(super) fn validate_hunk_policy(p: &GitHunkPolicy) -> Result<(), GitHunkError> {
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
    read_hunks_owned(GitHunkRequestId(0), p, &AtomicBool::new(false))
}

pub(super) fn open_hunk_repository(
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

fn check_hunk_request(
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
    started: Instant,
) -> Result<(), GitHunkError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitHunkError::Cancelled);
    }
    if started.elapsed() >= p.deadline {
        return Err(GitHunkError::Deadline);
    }
    Ok(())
}

pub(super) fn collect_hunks_from_diff(
    diff: &git2::Diff<'_>,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
    started: Instant,
) -> Result<Vec<GitHunk>, GitHunkError> {
    check_hunk_request(p, cancel, started)?;
    if diff.deltas().any(|delta| delta.flags().is_binary()) {
        return Err(GitHunkError::BinaryUnsupported);
    }
    let mut hunks: Vec<GitHunk> = Vec::new();
    let mut current_header = Vec::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;
    let mut abort = None;
    let print_result = diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
        if let Err(error) = check_hunk_request(p, cancel, started) {
            abort = Some(error);
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
                old_mode: delta.old_file().mode().into(),
                new_mode: delta.new_file().mode().into(),
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
            let Ok(text) = std::str::from_utf8(line.content()) else {
                abort = Some(GitHunkError::BinaryUnsupported);
                return false;
            };
            hunks.last_mut().unwrap().lines.push(GitDiffLine {
                kind,
                old_line: line.old_lineno(),
                new_line: line.new_lineno(),
                text: text.to_owned(),
            });
        }
        true
    });
    if let Some(error) = abort {
        return Err(error);
    }
    print_result.map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    check_hunk_request(p, cancel, started)?;
    let mut identities = std::collections::BTreeSet::new();
    if hunks.iter().any(|h| !identities.insert(h.id)) {
        return Err(GitHunkError::IdentityCollision);
    }
    Ok(hunks)
}

pub(super) fn read_hunks_owned(
    request_id: GitHunkRequestId,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
) -> Result<GitHunkSnapshot, GitHunkError> {
    let started = Instant::now();
    validate_hunk_policy(p)?;
    check_hunk_request(p, cancel, started)?;
    let (root, repo, workdir, repo_path) = open_hunk_repository(p)?;
    check_hunk_request(p, cancel, started)?;
    let index_before =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    check_hunk_request(p, cancel, started)?;
    let mut options = git2::DiffOptions::new();
    options.pathspec(&repo_path).disable_pathspec_match(true);
    let diff = match p.layer {
        GitDiffLayer::Unstaged => {
            options
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                .show_untracked_content(true);
            repo.diff_index_to_workdir(None, Some(&mut options))
        }
        GitDiffLayer::Staged => {
            let tree = repo.head().and_then(|h| h.peel_to_tree());
            match tree {
                Ok(tree) => repo.diff_tree_to_index(Some(&tree), None, Some(&mut options)),
                Err(error) if error.code() == git2::ErrorCode::UnbornBranch => {
                    repo.diff_tree_to_index(None, None, Some(&mut options))
                }
                Err(error) => Err(error),
            }
        }
    }
    .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    let hunks = collect_hunks_from_diff(&diff, p, cancel, started)?;
    let index_fingerprint =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    if index_fingerprint != index_before {
        return Err(GitHunkError::StaleIndex);
    }
    check_hunk_request(p, cancel, started)?;
    let worktree_sha256 = if p.layer == GitDiffLayer::Unstaged {
        worktree_hash(&root.join(&p.file)).ok().map(|value| value.0)
    } else {
        None
    };
    check_hunk_request(p, cancel, started)?;
    Ok(GitHunkSnapshot {
        request_id,
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

pub(super) fn collect_hunks_from_patch(
    patch: &git2::Patch<'_>,
    p: &GitHunkPolicy,
    cancel: &AtomicBool,
    started: Instant,
) -> Result<Vec<GitHunk>, GitHunkError> {
    check_hunk_request(p, cancel, started)?;
    if patch.delta().flags().contains(git2::DiffFlags::BINARY) {
        return Err(GitHunkError::BinaryUnsupported);
    }
    let mut hunks = Vec::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;
    for hunk_index in 0..patch.num_hunks() {
        check_hunk_request(p, cancel, started)?;
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
            old_mode: patch.delta().old_file().mode().into(),
            new_mode: patch.delta().new_file().mode().into(),
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
            check_hunk_request(p, cancel, started)?;
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
                let text = std::str::from_utf8(line.content())
                    .map_err(|_| GitHunkError::BinaryUnsupported)?;
                accepted.lines.push(GitDiffLine {
                    kind,
                    old_line: line.old_lineno(),
                    new_line: line.new_lineno(),
                    text: text.to_owned(),
                });
            }
        }
        hunks.push(accepted);
    }
    check_hunk_request(p, cancel, started)?;
    let mut identities = std::collections::BTreeSet::new();
    if hunks.iter().any(|hunk| !identities.insert(hunk.id)) {
        return Err(GitHunkError::IdentityCollision);
    }
    Ok(hunks)
}

pub(super) fn read_buffer_hunks_owned(
    request_id: GitHunkRequestId,
    p: &GitHunkPolicy,
    input: GitHunkBufferInput,
    cancel: &AtomicBool,
) -> Result<GitHunkSnapshot, GitHunkError> {
    let started = Instant::now();
    validate_hunk_policy(p)?;
    if p.layer != GitDiffLayer::Unstaged || input.bytes.len() > p.maximum_bytes {
        return Err(GitHunkError::InvalidPolicy);
    }
    check_hunk_request(p, cancel, started)?;
    let (_root, repo, workdir, repo_path) = open_hunk_repository(p)?;
    check_hunk_request(p, cancel, started)?;
    let index_before =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    check_hunk_request(p, cancel, started)?;
    let index = repo
        .index()
        .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))?;
    check_hunk_request(p, cancel, started)?;
    if (1..=3).any(|stage| index.get_path(&repo_path, stage).is_some()) {
        return Err(GitHunkError::Unmerged);
    }
    let index_blob = index
        .get_path(&repo_path, 0)
        .map(|entry| {
            repo.find_blob(entry.id)
                .map_err(|e| GitHunkError::Backend(e.message().chars().take(1024).collect()))
        })
        .transpose()?;
    check_hunk_request(p, cancel, started)?;
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
    let hunks = collect_hunks_from_patch(&patch, p, cancel, started)?;
    let index_fingerprint =
        git_index_fingerprint(&workdir).map_err(|_| GitHunkError::StaleRepository)?;
    if index_fingerprint != index_before {
        return Err(GitHunkError::StaleIndex);
    }
    check_hunk_request(p, cancel, started)?;
    Ok(GitHunkSnapshot {
        request_id,
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

#[cfg(test)]
mod staged_sign_tests {
    use crate::git::hunks::GitDiffLine;
    use crate::git::hunks::GitDiffLineKind;
    use crate::git::hunks::GitHunk;
    use crate::git::hunks::GitHunkId;
    use crate::git::hunks::index_line_in_buffer;

    fn hunk(old: (u32, u32), new: (u32, u32), lines: Vec<GitDiffLine>) -> GitHunk {
        GitHunk {
            id: GitHunkId(0),
            old_mode: 0o100644,
            new_mode: 0o100644,
            old_start: old.0,
            old_lines: old.1,
            new_start: new.0,
            new_lines: new.1,
            header: String::new(),
            lines,
        }
    }

    fn line(kind: GitDiffLineKind, old_line: Option<u32>, new_line: Option<u32>) -> GitDiffLine {
        GitDiffLine {
            kind,
            old_line,
            new_line,
            text: String::new(),
        }
    }

    #[test]
    fn index_lines_follow_unstaged_changes() {
        let inserted = [hunk((3, 0), (4, 2), Vec::new())];
        assert_eq!(index_line_in_buffer(2, &inserted), Some(2));
        assert_eq!(index_line_in_buffer(3, &inserted), Some(5));
        let deleted = [hunk(
            (3, 2),
            (2, 0),
            vec![
                line(GitDiffLineKind::Delete, Some(3), None),
                line(GitDiffLineKind::Delete, Some(4), None),
            ],
        )];
        assert_eq!(index_line_in_buffer(1, &deleted), Some(1));
        assert_eq!(index_line_in_buffer(2, &deleted), None);
        assert_eq!(index_line_in_buffer(4, &deleted), Some(2));
        let context = [hunk(
            (5, 2),
            (5, 3),
            vec![
                line(GitDiffLineKind::Context, Some(5), Some(5)),
                line(GitDiffLineKind::Add, None, Some(6)),
                line(GitDiffLineKind::Context, Some(6), Some(7)),
            ],
        )];
        assert_eq!(index_line_in_buffer(5, &context), Some(6));
        assert_eq!(index_line_in_buffer(6, &context), Some(7));
    }
}
