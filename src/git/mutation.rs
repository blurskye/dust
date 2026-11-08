//! Checked staging, reset and repository change plans.

use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
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

use crate::git::hunks::GitDiffLayer;

use crate::git::hunks::GitDiffLineKind;

use crate::git::hunks::GitHunk;

use crate::git::hunks::GitHunkError;

use crate::git::hunks::GitHunkRequestId;

use crate::git::hunks::GitHunkId;

use crate::git::hunks::GitHunkPolicy;

use crate::git::hunks::read_hunks_owned;

use crate::git::hunks::validate_hunk_policy;

use crate::git::status::GitRepositoryId;

use crate::git::status::GitUnsavedDocument;

use crate::git::status::unsaved_document_revision;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitIndexFingerprint(pub [u8; 32]);
pub fn git_index_fingerprint(repository: &Path) -> Result<GitIndexFingerprint, GitMutationError> {
    use sha2::Digest;
    use sha2::Sha256;
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
pub struct GitMutationRequestId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitHeadId(pub [u8; 20]);
impl GitHeadId {
    /// Convert without panicking if a future object format returns a different width.
    #[must_use]
    fn from_oid(oid: &git2::Oid) -> Option<Self> {
        let bytes: [u8; 20] = oid.as_bytes().try_into().ok()?;
        Some(Self(bytes))
    }
}
/// Read the commit HEAD resolves to, or `None` for an unborn branch. A repository that
/// cannot be opened, or whose HEAD is unreadable, is an error and never `None`.
pub fn git_head_id(repository: &Path) -> Result<Option<GitHeadId>, GitMutationError> {
    let repo = git2::Repository::open(repository).map_err(|_| GitMutationError::Backend)?;
    match repo.head() {
        Ok(head) => Ok(head.target().and_then(|oid| GitHeadId::from_oid(&oid))),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err(GitMutationError::Backend),
    }
}
/// The branch HEAD points at, or `None` for an unborn branch or a detached HEAD, which has
/// no branch name to show.
///
/// This reads HEAD directly rather than going through the branch service: a surface that
/// names the branch in a title needs it now, not on a worker round trip.
pub fn git_branch_name(repository: &Path) -> Result<Option<String>, GitMutationError> {
    let repo = git2::Repository::open(repository).map_err(|_| GitMutationError::Backend)?;
    match repo.head() {
        Ok(head) if head.is_branch() => Ok(head.shorthand().map(str::to_owned)),
        Ok(_) => Ok(None),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err(GitMutationError::Backend),
    }
}
/// Which HEAD a caller captured when it built a plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHeadExpectation {
    /// The caller captured no HEAD; no HEAD check is performed.
    Uncaptured,
    /// The caller captured an unborn branch.
    Unborn,
    /// The caller captured one exact commit.
    At(GitHeadId),
}
impl GitHeadExpectation {
    #[must_use]
    pub fn matches(self, actual: Option<GitHeadId>) -> bool {
        match (self, actual) {
            (Self::Uncaptured, _) => true,
            (Self::Unborn, None) => true,
            (Self::At(expected), Some(actual)) => expected == actual,
            _ => false,
        }
    }
}
/// A staging or unstaging plan carrying every revision it was captured against. Each kind of
/// mismatch has its own error so a caller can state what actually changed rather than
/// reporting one generic failure.
#[derive(Clone, Debug)]
pub struct GitChangePlanRequest {
    pub kind: GitMutationKind,
    pub policy: GitHunkPolicy,
    /// Snapshot revision the hunk selection was taken from.
    pub hunk_snapshot_revision: u64,
    /// Target hunk for hunk- and line-scoped plans.
    pub hunk_id: Option<GitHunkId>,
    pub selected: Option<GitHunk>,
    /// Strictly increasing indices into `selected.lines` for line-scoped plans.
    pub selected_lines: Vec<u32>,
    pub expected_index: Option<GitIndexFingerprint>,
    /// A staging capture of `None` means missing; hunk plans also require a deletion hunk.
    pub expected_worktree: Option<[u8; 32]>,
    pub expected_head: GitHeadExpectation,
    /// The exact unsaved document revision the caller captured, when it captured one.
    pub expected_document_revision: Option<u64>,
    /// Bounded, sorted unsaved editor content for this repository.
    pub unsaved_documents: Vec<GitUnsavedDocument>,
    pub maximum_patch_bytes: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub struct GitStageHunkRequest {
    pub policy: GitHunkPolicy,
    pub hunk_snapshot_revision: u64,
    pub hunk_id: GitHunkId,
    pub selected: GitHunk,
    pub expected_index: Option<GitIndexFingerprint>,
    /// `None` captures a missing file and is only valid for a deletion hunk.
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
    /// Positions in the hunk's `lines` to reset. Empty means all of it.
    pub selected_lines: Vec<u32>,
    pub expected_index: Option<GitIndexFingerprint>,
    /// `None` captures a missing file and is only valid for a deletion hunk.
    pub expected_worktree: Option<[u8; 32]>,
    pub maximum_patch_bytes: usize,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub enum GitMutationRequest {
    StageHunk(GitStageHunkRequest),
    ResetUnstagedHunk(GitResetHunkRequest),
    Plan(GitChangePlanRequest),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitMutationKind {
    StageHunk,
    ResetUnstagedHunk,
    /// Whole file from the worktree into the index, as one unit.
    StageFile,
    /// Whole file in the index back to HEAD; the worktree is untouched.
    UnstageFile,
    /// One staged hunk in the index back to HEAD; the worktree is untouched.
    UnstageHunk,
    /// An exact line range inside one unstaged hunk, from the worktree into the index.
    StageLines,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitMutationResult {
    pub request_id: GitMutationRequestId,
    pub kind: GitMutationKind,
    /// Workspace-relative path the operation acted on.
    pub path: PathBuf,
    pub before: GitIndexFingerprint,
    pub after: GitIndexFingerprint,
    /// Present only for hunk- and line-scoped operations.
    pub hunk: Option<GitHunkId>,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitMutationError {
    #[error("invalid Git mutation request")]
    Invalid,
    #[error("Git index identity is stale")]
    StaleIndex,
    #[error("Git worktree identity is stale")]
    StaleWorktree,
    #[error("Git HEAD changed since the plan was captured")]
    StaleHead,
    #[error("the unsaved document revision changed since the plan was captured")]
    StaleDocument,
    #[error("the path has unsaved editor changes and cannot be staged from disk")]
    UnsavedDocument,
    #[error("the selected line range is invalid for the captured hunk")]
    InvalidRange,
    #[error("nothing was selected")]
    NothingSelected,
    #[error("the operation requires a captured hunk selection")]
    MissingSelection,
    #[error("the operation would change nothing")]
    NoChange,
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
pub(super) enum GitMutationEvent {
    Complete(Result<GitMutationResult, GitMutationError>),
}
#[derive(Default)]
pub struct GitMutationWorker {
    pub(super) next: u64,
    active: Option<GitMutationRequestId>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitMutationEvent>>,
    accepted: Option<GitMutationResult>,
}
impl GitMutationWorker {
    pub fn start(
        &mut self,
        request: GitMutationRequest,
    ) -> Result<GitMutationRequestId, GitMutationError> {
        match &request {
            GitMutationRequest::StageHunk(request) => validate_legacy_request(
                request.maximum_patch_bytes,
                request.deadline,
                request.hunk_snapshot_revision,
                request.policy.layer,
                request.expected_index.is_some()
                    && (request.expected_worktree.is_some() || request.selected.new_mode == 0),
                &request.policy,
            )?,
            GitMutationRequest::ResetUnstagedHunk(request) => validate_legacy_request(
                request.maximum_patch_bytes,
                request.deadline,
                request.hunk_snapshot_revision,
                request.policy.layer,
                request.expected_index.is_some()
                    && (request.expected_worktree.is_some() || request.selected.new_mode == 0),
                &request.policy,
            )?,
            GitMutationRequest::Plan(plan) => validate_change_plan(plan)?,
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let request_id = GitMutationRequestId(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-mutation-{}", request_id.0))
            .spawn(move || {
                let result = match request {
                    GitMutationRequest::StageHunk(request) => {
                        stage_hunk(request_id, &request, &worker)
                    }
                    GitMutationRequest::ResetUnstagedHunk(request) => {
                        reset_hunk(request_id, &request, &worker)
                    }
                    GitMutationRequest::Plan(plan) => apply_change_plan(request_id, &plan, &worker),
                };
                let _ = tx.send(GitMutationEvent::Complete(result));
            })
            .map_err(|_| GitMutationError::Stopped)?;
        self.active = Some(request_id);
        self.cancel = Some(cancel);
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
                if expected != Some(result.request_id) {
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
impl Drop for GitMutationWorker {
    fn drop(&mut self) {
        self.cancel()
    }
}
/// Hash one worktree file so a caller can capture it before planning a mutation.
pub fn worktree_hash(path: &Path) -> Result<([u8; 32], Vec<u8>), GitMutationError> {
    use sha2::Digest;
    use sha2::Sha256;
    let bytes =
        crate::files::read_file_bounded(path, 8 * 1024 * 1024).map_err(|error| match error {
            crate::files::VfsError::NotRegularFile(_) => GitMutationError::UnsupportedPath,
            crate::files::VfsError::Io { source, .. }
                if crate::files::is_symlink_error(&source) =>
            {
                GitMutationError::UnsupportedPath
            }
            crate::files::VfsError::DocumentTooLarge { .. } => GitMutationError::Invalid,
            _ => GitMutationError::Backend,
        })?;
    let hash = Sha256::digest(&bytes).into();
    Ok((hash, bytes))
}
pub(super) fn mutation_hunk_error(error: GitHunkError) -> GitMutationError {
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

pub(super) fn exact_mutation_repository(
    policy: &GitHunkPolicy,
) -> Result<(PathBuf, git2::Repository, PathBuf), GitMutationError> {
    validate_hunk_policy(policy).map_err(|_| GitMutationError::Invalid)?;
    let root = crate::files::canonicalize(&policy.scope_root)
        .map_err(|_| GitMutationError::StaleRepository)?;
    let repository =
        git2::Repository::discover(&root).map_err(|_| GitMutationError::StaleRepository)?;
    let workdir = crate::files::canonicalize(
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

/// Keep Git's index lock from the first read through the final replacement. Libgit2 writes
/// to a temporary index while the real index stays locked and unchanged.
struct IndexLock {
    path: PathBuf,
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
struct IndexWrite {
    _lock: IndexLock,
    path: PathBuf,
    // Only the path is kept: Windows refuses to rename over a file that is held open,
    // and libgit2 writes the index by renaming over it.
    temporary: tempfile::TempPath,
}
impl IndexWrite {
    fn start(repo: &git2::Repository) -> Result<Self, GitMutationError> {
        let path = repo
            .index()
            .map_err(|_| GitMutationError::Backend)?
            .path()
            .ok_or(GitMutationError::Backend)?
            .to_path_buf();
        let lock_path = path.with_extension("lock");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|_| GitMutationError::StaleIndex)?;
        let lock = IndexLock { path: lock_path };
        let temporary =
            tempfile::NamedTempFile::new_in(path.parent().ok_or(GitMutationError::Backend)?)
                .map_err(|_| GitMutationError::Backend)?
                .into_temp_path();
        if path.exists() {
            std::fs::copy(&path, &temporary).map_err(|_| GitMutationError::Backend)?;
        } else {
            std::fs::remove_file(&temporary).map_err(|_| GitMutationError::Backend)?;
        }
        let mut index = git2::Index::open(&temporary).map_err(|_| GitMutationError::Backend)?;
        repo.set_index(&mut index)
            .map_err(|_| GitMutationError::Backend)?;
        Ok(Self {
            _lock: lock,
            path,
            temporary,
        })
    }
    fn commit(&self) -> Result<(), GitMutationError> {
        // Windows flushes a file only through a writable handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&self.temporary)
            .and_then(|file| file.sync_all())
            .map_err(|_| GitMutationError::Backend)?;
        std::fs::rename(&self.temporary, &self.path).map_err(|_| GitMutationError::Backend)
    }
}

pub(super) fn reset_hunk(
    request_id: GitMutationRequestId,
    r: &GitResetHunkRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, repository) = exact_mutation_repository(&r.policy)?;
    let _index_write = IndexWrite::start(&repo)?;
    let repo_path = repository_relative_path(&root, &repository, &r.policy.file)?;
    let before = git_index_fingerprint(&repository)?;
    if r.expected_index != Some(before) {
        return Err(GitMutationError::StaleIndex);
    }
    let file = root.join(&r.policy.file);
    let before_worktree = worktree_state(&file)?;
    if !before_worktree.matches_capture(r.expected_worktree, r.selected.new_mode == 0) {
        return Err(GitMutationError::StaleWorktree);
    }
    let snapshot =
        read_hunks_owned(GitHunkRequestId(0), &r.policy, cancel).map_err(mutation_hunk_error)?;
    let current = snapshot
        .hunks
        .iter()
        .find(|h| h.id == r.hunk_id)
        .ok_or(GitMutationError::WrongHunk)?;
    if current != &r.selected {
        return Err(GitMutationError::WrongHunk);
    }
    let name = checked_name(&repo_path)?;
    validate_selected_lines(current, &r.selected_lines)?;
    let chosen = |index: usize| {
        r.selected_lines.is_empty() || r.selected_lines.binary_search(&(index as u32)).is_ok()
    };
    // Going back: added lines come out and deleted ones return. An addition left alone stays as
    // context, and a deletion left alone stays gone.
    let mut body = String::new();
    let mut emitted = Vec::new();
    let mut kept_lines = 0u32;
    for index in lines_in_pairs(current) {
        let line = &current.lines[index];
        let mark = match (line.kind, chosen(index)) {
            (GitDiffLineKind::Context, _) | (GitDiffLineKind::Add, false) => ' ',
            (GitDiffLineKind::Add, true) => '-',
            (GitDiffLineKind::Delete, true) => '+',
            (GitDiffLineKind::Delete, false) => continue,
        };
        if mark != '-' {
            kept_lines += 1;
        }
        emitted.push((mark, line.text.as_str()));
    }
    append_patch_lines(&mut body, emitted);
    let target_mode = if current.old_mode == 0 && kept_lines == 0 {
        0
    } else if current.new_mode == 0 {
        current.old_mode
    } else {
        current.new_mode
    };
    let header = patch_header(name, current.new_mode, target_mode);
    let target_start = patch_start(current.new_start, current.new_lines, kept_lines);
    let patch = format!(
        "{header}@@ -{},{} +{},{} @@\n{body}",
        current.new_start, current.new_lines, target_start, kept_lines
    );
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
    if worktree_state(&file)? != before_worktree {
        return Err(GitMutationError::StaleWorktree);
    }
    let diff = git2::Diff::from_buffer(patch.as_bytes()).map_err(|_| GitMutationError::Backend)?;
    verify_patch_paths(&diff, &repo_path)?;
    repo.apply(&diff, git2::ApplyLocation::WorkDir, None)
        .map_err(|_| GitMutationError::Backend)?;
    let after = git_index_fingerprint(&repository)?;
    if after != before || worktree_state(&file)? == before_worktree {
        return Err(GitMutationError::Postcondition);
    }
    let remaining = read_hunks_owned(GitHunkRequestId(0), &r.policy, cancel)
        .map_err(|_| GitMutationError::Postcondition)?;
    // Part of a hunk can leave a hunk with the same header, and so the same id.
    if r.selected_lines.is_empty() && remaining.hunks.iter().any(|h| h.id == r.hunk_id) {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        request_id,
        kind: GitMutationKind::ResetUnstagedHunk,
        path: r.policy.file.clone(),
        before,
        after,
        hunk: Some(r.hunk_id),
    })
}

pub(super) fn stage_hunk(
    request_id: GitMutationRequestId,
    r: &GitStageHunkRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, repository) = exact_mutation_repository(&r.policy)?;
    let index_write = IndexWrite::start(&repo)?;
    let repo_path = repository_relative_path(&root, &repository, &r.policy.file)?;
    let before = git_index_fingerprint(&repository)?;
    if r.expected_index != Some(before) {
        return Err(GitMutationError::StaleIndex);
    }
    let file = root.join(&r.policy.file);
    let worktree = worktree_state(&file)?;
    if !worktree.matches_capture(r.expected_worktree, r.selected.new_mode == 0) {
        return Err(GitMutationError::StaleWorktree);
    }
    let snapshot =
        read_hunks_owned(GitHunkRequestId(0), &r.policy, cancel).map_err(mutation_hunk_error)?;
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
    let patch = whole_hunk_patch(checked_name(&repo_path)?, current);
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
    if worktree_state(&file)? != worktree {
        return Err(GitMutationError::StaleWorktree);
    }
    let diff = git2::Diff::from_buffer(&patch).map_err(|_| GitMutationError::Backend)?;
    verify_patch_paths(&diff, &repo_path)?;
    repo.apply(&diff, git2::ApplyLocation::Index, None)
        .map_err(|_| GitMutationError::Backend)?;
    index_write.commit()?;
    let after = git_index_fingerprint(&repository)?;
    if before == after || worktree_state(&file)? != worktree {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        request_id,
        kind: GitMutationKind::StageHunk,
        path: r.policy.file.clone(),
        before,
        after,
        hunk: Some(r.hunk_id),
    })
}

/// Hunk operations require a caller-supplied index fingerprint and worktree capture (a hash
/// or a missing file for deletion hunks), unstaged layer only. The plan path below can
/// add HEAD and document capture without changing this behaviour.
pub(super) fn validate_legacy_request(
    maximum_patch_bytes: usize,
    deadline: Duration,
    snapshot_revision: u64,
    layer: GitDiffLayer,
    exact: bool,
    policy: &GitHunkPolicy,
) -> Result<(), GitMutationError> {
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
    Ok(())
}

/// A plan must carry an index capture, and each kind must carry exactly the selection it
/// needs. The unsaved list shares the status policy's path-byte limit.
pub(super) fn validate_change_plan(plan: &GitChangePlanRequest) -> Result<(), GitMutationError> {
    if plan.maximum_patch_bytes == 0
        || plan.maximum_patch_bytes > 1024 * 1024
        || plan.deadline.is_zero()
        || plan.deadline > Duration::from_secs(5)
        || plan.expected_index.is_none()
        || validate_hunk_policy(&plan.policy).is_err()
        || crate::git::status::validate_unsaved_documents(
            &plan.unsaved_documents,
            plan.unsaved_documents.len(),
            16 * 1024 * 1024,
        )
        .is_err()
    {
        return Err(GitMutationError::Invalid);
    }
    let whole_file =
        plan.hunk_id.is_none() && plan.selected.is_none() && plan.selected_lines.is_empty();
    // Buffer-only hunks have no disk hash. They can still reach the worker's unsaved
    // refusal, which offers save-and-stage without applying buffer coordinates to disk.
    let unsaved = unsaved_document_revision(&plan.unsaved_documents, &plan.policy.file).is_some();
    match plan.kind {
        GitMutationKind::StageFile | GitMutationKind::UnstageFile => {
            if !whole_file {
                return Err(GitMutationError::Invalid);
            }
            Ok(())
        }
        GitMutationKind::StageHunk => {
            if plan.hunk_snapshot_revision == 0
                || plan.policy.layer != GitDiffLayer::Unstaged
                || plan.hunk_id.is_none()
                || plan.selected.is_none()
                || !plan.selected_lines.is_empty()
                || (plan.expected_worktree.is_none()
                    && !unsaved
                    && !plan
                        .selected
                        .as_ref()
                        .is_some_and(|hunk| hunk.new_mode == 0))
            {
                return Err(GitMutationError::Invalid);
            }
            Ok(())
        }
        GitMutationKind::UnstageHunk => {
            if plan.hunk_snapshot_revision == 0
                || plan.policy.layer != GitDiffLayer::Staged
                || plan.hunk_id.is_none()
                || plan.selected.is_none()
                || !plan.selected_lines.is_empty()
            {
                return Err(GitMutationError::Invalid);
            }
            Ok(())
        }
        GitMutationKind::StageLines => {
            if plan.hunk_snapshot_revision == 0
                || plan.policy.layer != GitDiffLayer::Unstaged
                || plan.hunk_id.is_none()
                || plan.selected.is_none()
                || plan.selected_lines.is_empty()
                || (plan.expected_worktree.is_none()
                    && !unsaved
                    && !plan
                        .selected
                        .as_ref()
                        .is_some_and(|hunk| hunk.new_mode == 0))
            {
                return Err(GitMutationError::Invalid);
            }
            Ok(())
        }
        GitMutationKind::ResetUnstagedHunk => Err(GitMutationError::Invalid),
    }
}

#[derive(Eq, PartialEq)]
enum WorktreeState {
    Missing,
    Regular { sha256: [u8; 32] },
}

impl WorktreeState {
    fn matches_capture(&self, expected: Option<[u8; 32]>, missing: bool) -> bool {
        match (self, expected) {
            (Self::Regular { sha256 }, Some(expected)) => *sha256 == expected,
            (Self::Missing, None) => missing,
            _ => false,
        }
    }
}

fn worktree_state(file: &Path) -> Result<WorktreeState, GitMutationError> {
    match std::fs::symlink_metadata(file) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(GitMutationError::UnsupportedPath),
        Ok(metadata) if metadata.file_type().is_file() => {
            let (sha256, _) = worktree_hash(file)?;
            Ok(WorktreeState::Regular { sha256 })
        }
        Ok(_) => Err(GitMutationError::UnsupportedPath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(WorktreeState::Missing),
        Err(_) => Err(GitMutationError::Backend),
    }
}

fn repository_relative_path(
    root: &Path,
    workdir: &Path,
    file: &Path,
) -> Result<PathBuf, GitMutationError> {
    let prefix = root
        .strip_prefix(workdir)
        .map_err(|_| GitMutationError::OutsideScope)?;
    let repo_path = prefix.join(file);
    if repo_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(GitMutationError::OutsideScope);
    }
    // Patch text names the file as Git does, with `/` on every platform.
    #[cfg(windows)]
    let repo_path = PathBuf::from(
        repo_path
            .to_str()
            .ok_or(GitMutationError::Invalid)?
            .replace('\\', "/"),
    );
    Ok(repo_path)
}

fn checked_name(file: &Path) -> Result<&str, GitMutationError> {
    file.to_str()
        .filter(|name| !name.contains(['\n', '\r']))
        .ok_or(GitMutationError::Invalid)
}

fn append_patch_text(patch: &mut String, text: &str) {
    patch.push_str(text);
    if !text.ends_with('\n') {
        patch.push_str("\n\\ No newline at end of file\n");
    }
}

fn patch_path(prefix: &str, name: &str) -> String {
    let mut quoted = String::from("\"");
    for byte in prefix.bytes().chain(name.bytes()) {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b' '..=b'~' => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn verify_patch_paths(diff: &git2::Diff<'_>, path: &Path) -> Result<(), GitMutationError> {
    if diff.deltas().len() != 1 {
        return Err(GitMutationError::Invalid);
    }
    let delta = diff.get_delta(0).ok_or(GitMutationError::Invalid)?;
    let old = delta.old_file();
    let new = delta.new_file();
    if old.path().is_none() && new.path().is_none()
        || [old.path(), new.path()]
            .into_iter()
            .flatten()
            .any(|parsed| parsed != path)
    {
        return Err(GitMutationError::Invalid);
    }
    Ok(())
}

fn patch_header(name: &str, old_mode: u32, new_mode: u32) -> String {
    let old = patch_path("a/", name);
    let new = patch_path("b/", name);
    let mut header = format!("diff --git {old} {new}\n");
    if old_mode == 0 {
        header.push_str(&format!(
            "new file mode {new_mode:06o}\n--- /dev/null\n+++ {new}\n"
        ));
    } else if new_mode == 0 {
        header.push_str(&format!(
            "deleted file mode {old_mode:06o}\n--- {old}\n+++ /dev/null\n"
        ));
    } else {
        header.push_str(&format!(
            "index 0000000..0000000 {old_mode:06o}\n--- {old}\n+++ {new}\n"
        ));
    }
    header
}

fn patch_start(start: u32, source_lines: u32, target_lines: u32) -> u32 {
    start
        .saturating_add(u32::from(source_lines == 0))
        .saturating_sub(u32::from(target_lines == 0))
}

fn whole_hunk_patch(name: &str, hunk: &GitHunk) -> Vec<u8> {
    let new_mode = if hunk.new_mode == 0 {
        0
    } else if hunk.old_mode == 0 {
        hunk.new_mode
    } else {
        hunk.old_mode
    };
    let mut patch = patch_header(name, hunk.old_mode, new_mode);
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start,
        hunk.old_lines,
        patch_start(hunk.old_start, hunk.old_lines, hunk.new_lines),
        hunk.new_lines
    ));
    for line in &hunk.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '+',
            GitDiffLineKind::Delete => '-',
        });
        append_patch_text(&mut patch, &line.text);
    }
    patch.into_bytes()
}

/// Build a patch for an exact line range inside one hunk. Unselected additions are dropped
/// because they must not enter the index; unselected deletions become context because the
/// original line stays. Line counts are recomputed from the emitted body, and the new side
/// starts where the old side does because only this hunk is being applied.
fn selected_lines_patch(
    name: &str,
    hunk: &GitHunk,
    selected: &[u32],
) -> Result<Vec<u8>, GitMutationError> {
    validate_selected_lines(hunk, selected)?;
    let mut body = String::new();
    let mut old_lines = 0u32;
    let mut new_lines = 0u32;
    let mut applied = 0usize;
    for index in lines_in_pairs(hunk) {
        let line = &hunk.lines[index];
        let chosen = selected.binary_search(&(index as u32)).is_ok();
        match line.kind {
            GitDiffLineKind::Context => {
                body.push(' ');
                append_patch_text(&mut body, &line.text);
                old_lines += 1;
                new_lines += 1;
            }
            GitDiffLineKind::Delete => {
                body.push(if chosen { '-' } else { ' ' });
                append_patch_text(&mut body, &line.text);
                old_lines += 1;
                if chosen {
                    applied += 1;
                } else {
                    new_lines += 1;
                }
            }
            GitDiffLineKind::Add => {
                if chosen {
                    body.push('+');
                    append_patch_text(&mut body, &line.text);
                    new_lines += 1;
                    applied += 1;
                }
            }
        }
    }
    if applied == 0 {
        return Err(GitMutationError::NothingSelected);
    }
    let modes = patch_header(
        name,
        hunk.old_mode,
        if new_lines == 0 {
            0
        } else if hunk.old_mode == 0 {
            hunk.new_mode
        } else {
            hunk.old_mode
        },
    );
    let header = format!(
        "{modes}@@ -{},{old_lines} +{},{new_lines} @@\n",
        hunk.old_start,
        patch_start(hunk.old_start, old_lines, new_lines)
    );
    Ok(format!("{header}{body}").into_bytes())
}

fn validate_selected_lines(hunk: &GitHunk, selected: &[u32]) -> Result<(), GitMutationError> {
    let mut previous: Option<u32> = None;
    for index in selected {
        if previous.is_some_and(|last| last >= *index) {
            return Err(GitMutationError::InvalidRange);
        }
        let line = hunk
            .lines
            .get(*index as usize)
            .ok_or(GitMutationError::InvalidRange)?;
        if line.kind == GitDiffLineKind::Context {
            return Err(GitMutationError::InvalidRange);
        }
        previous = Some(*index);
    }
    Ok(())
}

/// The hunk's lines with each removed line next to the added line that replaces it, so a removed
/// line kept as context stays in its place relative to the chosen changes around it.
fn lines_in_pairs(hunk: &GitHunk) -> Vec<usize> {
    fn flush(order: &mut Vec<usize>, removed: &mut Vec<usize>, added: &mut Vec<usize>) {
        for position in 0..removed.len().max(added.len()) {
            order.extend(removed.get(position));
            order.extend(added.get(position));
        }
        removed.clear();
        added.clear();
    }
    let (mut order, mut removed, mut added) = (Vec::new(), Vec::new(), Vec::new());
    for (index, line) in hunk.lines.iter().enumerate() {
        match line.kind {
            GitDiffLineKind::Delete => removed.push(index),
            GitDiffLineKind::Add => added.push(index),
            GitDiffLineKind::Context => {
                flush(&mut order, &mut removed, &mut added);
                order.push(index);
            }
        }
    }
    flush(&mut order, &mut removed, &mut added);
    order
}

/// Emit removals before additions between context lines, including their EOF markers.
fn append_patch_lines(patch: &mut String, mut lines: Vec<(char, &str)>) {
    let mut start = 0;
    for end in 0..=lines.len() {
        if end == lines.len() || lines[end].0 == ' ' {
            lines[start..end].sort_by_key(|(mark, _)| u8::from(*mark != '-'));
            start = end + 1;
        }
    }
    for (mark, text) in lines {
        patch.push(mark);
        append_patch_text(patch, text);
    }
}

/// Build the reverse patch for one captured hunk: the sides swap and the markers invert, so
/// applying it forward to the index returns that hunk to HEAD.
fn reverse_hunk_patch(name: &str, hunk: &GitHunk) -> Vec<u8> {
    let mode = if hunk.old_mode == 0 {
        0
    } else if hunk.new_mode == 0 {
        hunk.old_mode
    } else {
        hunk.new_mode
    };
    let mut patch = patch_header(name, hunk.new_mode, mode);
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.new_start,
        hunk.new_lines,
        patch_start(hunk.new_start, hunk.new_lines, hunk.old_lines),
        hunk.old_lines
    ));
    let lines = hunk
        .lines
        .iter()
        .map(|line| {
            let mark = match line.kind {
                GitDiffLineKind::Context => ' ',
                GitDiffLineKind::Add => '-',
                GitDiffLineKind::Delete => '+',
            };
            (mark, line.text.as_str())
        })
        .collect();
    append_patch_lines(&mut patch, lines);
    patch.into_bytes()
}

/// Resolve the captured hunk against a fresh read at the plan's layer and produce its patch.
fn captured_hunk_patch(
    p: &GitChangePlanRequest,
    repo_path: &Path,
    selection: Option<&[u32]>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, GitMutationError> {
    let snapshot =
        read_hunks_owned(GitHunkRequestId(0), &p.policy, cancel).map_err(mutation_hunk_error)?;
    let hunk_id = p.hunk_id.ok_or(GitMutationError::MissingSelection)?;
    let selected = p
        .selected
        .as_ref()
        .ok_or(GitMutationError::MissingSelection)?;
    let current = snapshot
        .hunks
        .iter()
        .find(|hunk| hunk.id == hunk_id)
        .ok_or(GitMutationError::WrongHunk)?;
    if current != selected {
        return Err(GitMutationError::WrongHunk);
    }
    let name = checked_name(repo_path)?;
    Ok(match (p.kind, selection) {
        (GitMutationKind::UnstageHunk, _) => reverse_hunk_patch(name, current),
        (_, None) => whole_hunk_patch(name, current),
        (_, Some(lines)) => selected_lines_patch(name, current, lines)?,
    })
}

fn stage_whole_file(
    repo: &git2::Repository,
    repo_path: &Path,
    worktree: &WorktreeState,
) -> Result<(), GitMutationError> {
    let mut index = repo.index().map_err(|_| GitMutationError::Backend)?;
    match worktree {
        WorktreeState::Regular { .. } => index
            .add_path(repo_path)
            .map_err(|_| GitMutationError::Backend)?,
        WorktreeState::Missing => index
            .remove_path(repo_path)
            .map_err(|_| GitMutationError::Backend)?,
    }
    index.write().map_err(|_| GitMutationError::Backend)
}

fn unstage_whole_file(repo: &git2::Repository, repo_path: &Path) -> Result<(), GitMutationError> {
    let mut index = repo.index().map_err(|_| GitMutationError::Backend)?;
    let tree = match repo.head() {
        Ok(head) => Some(head.peel_to_tree().map_err(|_| GitMutationError::Backend)?),
        Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
        Err(_) => return Err(GitMutationError::Backend),
    };
    let entry = match tree.as_ref().map(|tree| tree.get_path(repo_path)) {
        Some(Ok(entry)) => Some(entry),
        Some(Err(error))
            if error.code() == git2::ErrorCode::NotFound
                && error.class() == git2::ErrorClass::Tree =>
        {
            None
        }
        Some(Err(_)) => return Err(GitMutationError::Backend),
        None => None,
    };
    if let Some(entry) = entry {
        let mut restored = index.get_path(repo_path, 0).unwrap_or(git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: entry.id(),
            flags: 0,
            flags_extended: 0,
            path: checked_name(repo_path)?.as_bytes().to_vec(),
        });
        restored.id = entry.id();
        restored.mode = entry.filemode() as u32;
        // The restored blob no longer describes the cached worktree stat data.
        restored.ctime = git2::IndexTime::new(0, 0);
        restored.mtime = git2::IndexTime::new(0, 0);
        restored.file_size = 0;
        index
            .remove_path(repo_path)
            .map_err(|_| GitMutationError::Backend)?;
        index
            .add(&restored)
            .map_err(|_| GitMutationError::Backend)?;
    } else {
        index
            .remove_path(repo_path)
            .map_err(|_| GitMutationError::Backend)?;
    }
    index.write().map_err(|_| GitMutationError::Backend)
}

/// Apply one captured change plan. Every captured revision is checked before the first write,
/// the index and worktree are re-checked immediately before the write, and staging is proven
/// not to have touched the worktree afterwards.
pub(super) fn apply_change_plan(
    request_id: GitMutationRequestId,
    p: &GitChangePlanRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, workdir) = exact_mutation_repository(&p.policy)?;
    let index_write = IndexWrite::start(&repo)?;
    let repo_path = repository_relative_path(&root, &workdir, &p.policy.file)?;

    if !p.expected_head.matches(git_head_id(&workdir)?) {
        return Err(GitMutationError::StaleHead);
    }
    let unsaved_here = unsaved_document_revision(&p.unsaved_documents, &p.policy.file);
    if let Some(expected) = p.expected_document_revision
        && unsaved_here != Some(expected)
    {
        return Err(GitMutationError::StaleDocument);
    }
    let before = git_index_fingerprint(&workdir)?;
    if p.expected_index != Some(before) {
        return Err(GitMutationError::StaleIndex);
    }

    let from_worktree = matches!(
        p.kind,
        GitMutationKind::StageFile | GitMutationKind::StageHunk | GitMutationKind::StageLines
    );
    // Refuse staging unsaved editor content before queuing the operation.
    if from_worktree && unsaved_here.is_some() {
        return Err(GitMutationError::UnsavedDocument);
    }

    let file = root.join(&p.policy.file);
    let worktree = if from_worktree {
        Some(worktree_state(&file)?)
    } else {
        None
    };
    if let Some(worktree) = &worktree {
        match (worktree, p.expected_worktree) {
            (WorktreeState::Regular { sha256 }, Some(expected)) if *sha256 == expected => {}
            // Whole-file staging reads its input on this worker, like git add. Hunk plans
            // still require the worktree hash captured with their preview.
            (_, None) if p.kind == GitMutationKind::StageFile => {}
            (WorktreeState::Missing, None) => {}
            _ => return Err(GitMutationError::StaleWorktree),
        }
    }

    let patch = match p.kind {
        GitMutationKind::StageFile | GitMutationKind::UnstageFile => None,
        GitMutationKind::StageHunk | GitMutationKind::UnstageHunk => {
            Some(captured_hunk_patch(p, &repo_path, None, cancel)?)
        }
        GitMutationKind::StageLines => Some(captured_hunk_patch(
            p,
            &repo_path,
            Some(&p.selected_lines),
            cancel,
        )?),
        GitMutationKind::ResetUnstagedHunk => return Err(GitMutationError::Invalid),
    };

    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    if started.elapsed() > p.deadline {
        return Err(GitMutationError::Deadline);
    }
    // Re-check the captures immediately before the first write.
    if git_index_fingerprint(&workdir)? != before {
        return Err(GitMutationError::StaleIndex);
    }
    if !p.expected_head.matches(git_head_id(&workdir)?) {
        return Err(GitMutationError::StaleHead);
    }
    if let Some(worktree) = &worktree
        && worktree_state(&file)? != *worktree
    {
        return Err(GitMutationError::StaleWorktree);
    }

    if let Some(patch) = patch {
        if patch.len() > p.maximum_patch_bytes {
            return Err(GitMutationError::Invalid);
        }
        let diff = git2::Diff::from_buffer(&patch).map_err(|_| GitMutationError::Backend)?;
        verify_patch_paths(&diff, &repo_path)?;
        repo.apply(&diff, git2::ApplyLocation::Index, None)
            .map_err(|_| GitMutationError::Backend)?;
    } else if p.kind == GitMutationKind::StageFile {
        stage_whole_file(
            &repo,
            &repo_path,
            worktree.as_ref().ok_or(GitMutationError::Invalid)?,
        )?;
    } else {
        unstage_whole_file(&repo, &repo_path)?;
    }

    index_write.commit()?;
    let after = git_index_fingerprint(&workdir)?;
    if before == after {
        return Err(GitMutationError::NoChange);
    }
    // Staging must never write the worktree.
    if let Some(worktree) = &worktree
        && worktree_state(&file)? != *worktree
    {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        request_id,
        kind: p.kind,
        path: p.policy.file.clone(),
        before,
        after,
        hunk: p.hunk_id,
    })
}

#[cfg(test)]
mod tests {
    use crate::git::mutation::*;

    // FIFOs and symlinks are Unix special files here.
    #[cfg(unix)]
    #[test]
    fn worktree_hash_rejects_special_files_and_bounds_regular_reads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "regular\n").unwrap();
        assert_eq!(worktree_hash(&file).unwrap().1, b"regular\n");
        let symlink = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &symlink).unwrap();
        assert!(
            worktree_hash(&symlink).is_err(),
            "even regular-file links are not followed"
        );
        let fifo = dir.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert_eq!(worktree_hash(&fifo), Err(GitMutationError::UnsupportedPath));
        let large = std::fs::File::create(&file).unwrap();
        large.set_len(8 * 1024 * 1024 + 1).unwrap();
        assert_eq!(worktree_hash(&file), Err(GitMutationError::Invalid));
    }

    #[test]
    fn parsed_patch_paths_must_match_the_requested_file() {
        let patch = b"diff --git a/foo b/foo\n--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n-old\n+new\n";
        let diff = git2::Diff::from_buffer(patch).unwrap();
        assert_eq!(verify_patch_paths(&diff, Path::new("foo")), Ok(()));
        assert_eq!(
            verify_patch_paths(&diff, Path::new("foo ")),
            Err(GitMutationError::Invalid)
        );
    }

    #[test]
    fn index_mutations_hold_git_lock() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a"), "a\n").unwrap();
        std::fs::write(dir.path().join("b"), "b\n").unwrap();
        let transaction = IndexWrite::start(&repo).unwrap();
        stage_whole_file(
            &repo,
            Path::new("a"),
            &worktree_state(&dir.path().join("a")).unwrap(),
        )
        .unwrap();
        let external = git2::Repository::open(dir.path()).unwrap();
        let mut other = external.index().unwrap();
        other.add_path(Path::new("b")).unwrap();
        assert_eq!(other.write().unwrap_err().code(), git2::ErrorCode::Locked);
        transaction.commit().unwrap();
        drop(transaction);
        other.read(true).unwrap();
        other.add_path(Path::new("b")).unwrap();
        other.write().unwrap();
        let index = git2::Repository::open(dir.path()).unwrap().index().unwrap();
        assert!(index.get_path(Path::new("a"), 0).is_some());
        assert!(index.get_path(Path::new("b"), 0).is_some());
    }
    #[test]
    fn cancellation_suppresses_a_completion_delivered_after_cancel() {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut service = GitMutationWorker {
            next: 1,
            active: Some(GitMutationRequestId(1)),
            cancel: Some(Arc::clone(&cancellation)),
            receiver: Some(rx),
            accepted: None,
        };
        let (release, wait) = mpsc::sync_channel(0);
        let worker = thread::spawn(move || {
            wait.recv().unwrap();
            tx.send(GitMutationEvent::Complete(Err(
                GitMutationError::StaleWorktree,
            )))
            .is_err()
        });
        service.cancel();
        assert!(cancellation.load(Ordering::Acquire));
        release.send(()).unwrap();
        assert!(worker.join().unwrap(), "cancel drops the result receiver");
        assert!(service.poll().unwrap().is_none());
        assert_eq!(service.next, 1);
    }
}
