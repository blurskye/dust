use super::*;

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
pub(super) fn worktree_hash(path: &Path) -> Result<([u8; 32], Vec<u8>), GitMutationError> {
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
        before,
        after,
        hunk: r.hunk_id,
    })
}
