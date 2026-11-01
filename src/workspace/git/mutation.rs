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

use super::{
    hunks::{
        GitDiffLayer, GitDiffLineKind, GitHunk, GitHunkError, GitHunkGeneration, GitHunkId,
        GitHunkPolicy, read_hunks_owned, validate_hunk_policy,
    },
    status::{GitRepositoryId, GitUnsavedDocument, unsaved_document_revision},
};

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
    pub generation: GitMutationGeneration,
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
pub struct GitMutationService {
    pub(super) next: u64,
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
        match &request {
            GitMutationRequest::StageHunk(request) => validate_legacy_request(
                request.maximum_patch_bytes,
                request.deadline,
                request.hunk_snapshot_revision,
                request.policy.layer,
                request.expected_index.is_some() && request.expected_worktree.is_some(),
                &request.policy,
            )?,
            GitMutationRequest::ResetUnstagedHunk(request) => validate_legacy_request(
                request.maximum_patch_bytes,
                request.deadline,
                request.hunk_snapshot_revision,
                request.policy.layer,
                request.expected_index.is_some() && request.expected_worktree.is_some(),
                &request.policy,
            )?,
            GitMutationRequest::Plan(plan) => validate_change_plan(plan)?,
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
                    GitMutationRequest::Plan(plan) => {
                        apply_change_plan(generation, &plan, &worker)
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
/// Hash one worktree file so a caller can capture it before planning a mutation.
pub fn worktree_hash(path: &Path) -> Result<([u8; 32], Vec<u8>), GitMutationError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|_| GitMutationError::Backend)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(GitMutationError::Invalid);
    }
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

pub(super) fn reset_hunk(
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
    let chosen = |index: usize| {
        r.selected_lines.is_empty() || r.selected_lines.contains(&(index as u32))
    };
    // Going back: added lines come out and deleted ones return. An addition left alone stays as
    // context, and a deletion left alone stays gone.
    let mut body = String::new();
    let mut kept_lines = 0u32;
    for (index, line) in current.lines.iter().enumerate() {
        let mark = match (line.kind, chosen(index)) {
            (GitDiffLineKind::Context, _) | (GitDiffLineKind::Add, false) => ' ',
            (GitDiffLineKind::Add, true) => '-',
            (GitDiffLineKind::Delete, true) => '+',
            (GitDiffLineKind::Delete, false) => continue,
        };
        if mark != '-' {
            kept_lines += 1;
        }
        body.push(mark);
        body.push_str(&line.text);
    }
    let patch = format!(
        "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{1},{3} @@\n{body}",
        name, current.new_start, current.new_lines, kept_lines
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
    // Part of a hunk can leave a hunk with the same header, and so the same id.
    if r.selected_lines.is_empty() && remaining.hunks.iter().any(|h| h.id == r.hunk_id) {
        return Err(GitMutationError::Postcondition);
    }
    Ok(GitMutationResult {
        generation,
        kind: GitMutationKind::ResetUnstagedHunk,
        path: r.policy.file.clone(),
        before,
        after,
        hunk: Some(r.hunk_id),
    })
}

pub(super) fn stage_hunk(
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
        path: r.policy.file.clone(),
        before,
        after,
        hunk: Some(r.hunk_id),
    })
}

/// Existing hunk operations keep their original capture discipline: caller-supplied index
/// fingerprint and worktree hash, unstaged layer only. Extracted so the plan path below can
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
/// needs. The unsaved list reuses W-08's existing path-byte ceiling rather than inventing one.
pub(super) fn validate_change_plan(plan: &GitChangePlanRequest) -> Result<(), GitMutationError> {
    if plan.maximum_patch_bytes == 0
        || plan.maximum_patch_bytes > 1024 * 1024
        || plan.deadline.is_zero()
        || plan.deadline > Duration::from_secs(5)
        || plan.expected_index.is_none()
        || validate_hunk_policy(&plan.policy).is_err()
        || super::status::validate_unsaved_documents(
            &plan.unsaved_documents,
            plan.unsaved_documents.len(),
            16 * 1024 * 1024,
        )
        .is_err()
    {
        return Err(GitMutationError::Invalid);
    }
    let whole_file = plan.hunk_id.is_none() && plan.selected.is_none() && plan.selected_lines.is_empty();
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
                || plan.expected_worktree.is_none()
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
                || plan.expected_worktree.is_none()
            {
                return Err(GitMutationError::Invalid);
            }
            Ok(())
        }
        GitMutationKind::ResetUnstagedHunk => Err(GitMutationError::Invalid),
    }
}

enum WorktreeState {
    Missing,
    Regular { sha256: [u8; 32] },
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
    Ok(repo_path)
}

fn checked_name(file: &Path) -> Result<&str, GitMutationError> {
    file.to_str()
        .filter(|name| !name.contains(['\n', '\r']))
        .ok_or(GitMutationError::Invalid)
}

/// Build the forward patch for one captured hunk. The unstaged diff runs index to disk, so
/// applying it forward to the index stages and applying it reversed to the index unstages.
fn whole_hunk_patch(name: &str, hunk: &GitHunk) -> Vec<u8> {
    let mut patch = format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n{}", hunk.header);
    for line in &hunk.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '+',
            GitDiffLineKind::Delete => '-',
        });
        patch.push_str(&line.text);
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
                body.push_str(&line.text);
                old_lines += 1;
                new_lines += 1;
            }
            GitDiffLineKind::Delete => {
                body.push(if chosen { '-' } else { ' ' });
                body.push_str(&line.text);
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
                    body.push_str(&line.text);
                    new_lines += 1;
                    applied += 1;
                }
            }
        }
    }
    if applied == 0 {
        return Err(GitMutationError::NothingSelected);
    }
    let header = format!(
        "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -{},{old_lines} +{},{new_lines} @@\n",
        hunk.old_start, hunk.old_start
    );
    Ok(format!("{header}{body}").into_bytes())
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

/// Build the reverse patch for one captured hunk: the sides swap and the markers invert, so
/// applying it forward to the index returns that hunk to HEAD.
fn reverse_hunk_patch(name: &str, hunk: &GitHunk) -> Vec<u8> {
    let mut patch = format!(
        "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{3},{4} @@\n",
        name, hunk.new_start, hunk.new_lines, hunk.old_start, hunk.old_lines
    );
    for line in &hunk.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '-',
            GitDiffLineKind::Delete => '+',
        });
        patch.push_str(&line.text);
    }
    patch.into_bytes()
}

/// Resolve the captured hunk against a fresh read at the plan's layer and produce its patch.
fn captured_hunk_patch(
    p: &GitChangePlanRequest,
    selection: Option<&[u32]>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, GitMutationError> {
    let snapshot = read_hunks_owned(GitHunkGeneration(0), &p.policy, cancel)
        .map_err(mutation_hunk_error)?;
    let hunk_id = p.hunk_id.ok_or(GitMutationError::MissingSelection)?;
    let selected = p.selected.as_ref().ok_or(GitMutationError::MissingSelection)?;
    let current = snapshot
        .hunks
        .iter()
        .find(|hunk| hunk.id == hunk_id)
        .ok_or(GitMutationError::WrongHunk)?;
    if current != selected {
        return Err(GitMutationError::WrongHunk);
    }
    let name = checked_name(&p.policy.file)?;
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

fn unstage_whole_file(
    repo: &git2::Repository,
    repo_path: &Path,
) -> Result<(), GitMutationError> {
    let name = checked_name(repo_path)?;
    match repo
        .head()
        .ok()
        .and_then(|head| head.peel(git2::ObjectType::Commit).ok())
    {
        Some(target) => repo
            .reset_default(Some(&target), [name])
            .map_err(|_| GitMutationError::Backend),
        None => {
            let mut index = repo.index().map_err(|_| GitMutationError::Backend)?;
            index
                .remove_path(repo_path)
                .map_err(|_| GitMutationError::Backend)?;
            index.write().map_err(|_| GitMutationError::Backend)
        }
    }
}

/// Apply one captured change plan. Every captured revision is checked before the first write,
/// the index and worktree are re-checked immediately before the write, and staging is proven
/// not to have touched the worktree afterwards.
pub(super) fn apply_change_plan(
    generation: GitMutationGeneration,
    p: &GitChangePlanRequest,
    cancel: &AtomicBool,
) -> Result<GitMutationResult, GitMutationError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitMutationError::Cancelled);
    }
    let started = Instant::now();
    let (root, repo, workdir) = exact_mutation_repository(&p.policy)?;
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
    // W-12: staging a path with unsaved editor content is refused before admission.
    if from_worktree && unsaved_here.is_some() {
        return Err(GitMutationError::UnsavedDocument);
    }

    let file = root.join(&p.policy.file);
    let worktree = worktree_state(&file)?;
    if from_worktree {
        match (&worktree, p.expected_worktree) {
            (WorktreeState::Regular { sha256 }, Some(expected)) if *sha256 == expected => {}
            (WorktreeState::Missing, None) if p.kind == GitMutationKind::StageFile => {}
            _ => return Err(GitMutationError::StaleWorktree),
        }
    }

    let patch = match p.kind {
        GitMutationKind::StageFile | GitMutationKind::UnstageFile => None,
        GitMutationKind::StageHunk | GitMutationKind::UnstageHunk => {
            Some(captured_hunk_patch(p, None, cancel)?)
        }
        GitMutationKind::StageLines => {
            Some(captured_hunk_patch(p, Some(&p.selected_lines), cancel)?)
        }
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
    if from_worktree {
        match (worktree_state(&file)?, &worktree) {
            (WorktreeState::Regular { sha256 }, WorktreeState::Regular { sha256: pre })
                if sha256 == *pre => {}
            (WorktreeState::Missing, WorktreeState::Missing) => {}
            _ => return Err(GitMutationError::StaleWorktree),
        }
    }

    if let Some(patch) = patch {
        if patch.len() > p.maximum_patch_bytes {
            return Err(GitMutationError::Invalid);
        }
        let diff = git2::Diff::from_buffer(&patch).map_err(|_| GitMutationError::Backend)?;
        repo.apply(&diff, git2::ApplyLocation::Index, None)
            .map_err(|_| GitMutationError::Backend)?;
    } else if p.kind == GitMutationKind::StageFile {
        stage_whole_file(&repo, &repo_path, &worktree)?;
    } else {
        unstage_whole_file(&repo, &repo_path)?;
    }

    let after = git_index_fingerprint(&workdir)?;
    if before == after {
        return Err(GitMutationError::NoChange);
    }
    if from_worktree {
        // Staging must never write the worktree.
        match (worktree_state(&file)?, &worktree) {
            (WorktreeState::Regular { sha256 }, WorktreeState::Regular { sha256: pre })
                if sha256 == *pre => {}
            (WorktreeState::Missing, WorktreeState::Missing) => {}
            _ => return Err(GitMutationError::Postcondition),
        }
    }
    Ok(GitMutationResult {
        generation,
        kind: p.kind,
        path: p.policy.file.clone(),
        before,
        after,
        hunk: p.hunk_id,
    })
}
