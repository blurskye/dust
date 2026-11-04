//! Branch and worktree authority: what exists, what a mutation would affect, and what is
//! refused. Every refusal is decided before the first write, so a refused request leaves the
//! repository byte-unchanged.
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

/// A worktree identity captured for a document at the moment it was opened.
///
/// This is deliberately not a `GitRepositoryId`: linked worktrees share a repository but are
/// distinct roots with their own HEAD and index, so a resource bound to one must not be
/// retargeted when another is opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeIdentity {
    /// Canonical path of the document the identity was captured for.
    pub document: PathBuf,
    /// Canonical root of the worktree that contained the document.
    pub root: PathBuf,
    /// Stable id derived from the worktree root, in the same spirit as `GitRepositoryId`.
    pub id: u64,
}
impl WorktreeIdentity {
    /// Capture the worktree a document belongs to by walking up from it to the nearest
    /// ancestor holding a `.git` entry. A linked worktree has a `.git` file pointing at its
    /// administrative directory, so the nearest ancestor is that worktree rather than the main
    /// checkout. A document outside any worktree has no identity rather than a fabricated one.
    pub fn capture(document: &Path) -> Option<Self> {
        let document = std::fs::canonicalize(document).ok()?;
        let mut cursor = document.parent();
        while let Some(directory) = cursor {
            if directory.join(".git").exists() {
                let root = directory.to_path_buf();
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                root.hash(&mut hasher);
                return Some(Self {
                    document,
                    root,
                    id: hasher.finish(),
                });
            }
            cursor = directory.parent();
        }
        None
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitBranchGeneration(pub u64);
#[derive(Clone, Debug)]
pub struct GitBranchListRequest {
    pub repository: PathBuf,
    pub deadline: Duration,
}
#[derive(Clone, Debug)]
pub enum GitBranchRequest {
    List(GitBranchListRequest),
    CreateWorktree {
        repository: PathBuf,
        name: String,
        path: PathBuf,
        /// Branch or ref to check out in the new worktree.
        reference: String,
        deadline: Duration,
    },
    RemoveWorktree {
        repository: PathBuf,
        name: String,
        deadline: Duration,
    },
    /// Check out a REVISION rather than a branch. A review must not create or move a ref, so its
    /// worktree is DETACHED at the revision that was inspected.
    CreateDetachedWorktree {
        repository: PathBuf,
        name: String,
        path: PathBuf,
        revision: String,
        deadline: Duration,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBranchEntry {
    pub name: String,
    pub remote: bool,
    pub upstream: Option<String>,
    /// The worktree that has this branch checked out, in its own path terms.
    pub checked_out: Option<PathBuf>,
    pub head: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitWorktreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub dirty: bool,
    /// A Git operation in progress in that worktree, named.
    pub operation: Option<String>,
    pub locked: bool,
    pub main: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBranchList {
    pub generation: GitBranchGeneration,
    pub branches: Vec<GitBranchEntry>,
    pub worktrees: Vec<GitWorktreeEntry>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitBranchResult {
    List(GitBranchList),
    Changed,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitBranchError {
    #[error("invalid Git branch request")]
    Invalid,
    #[error("not a Git repository")]
    NotRepository,
    #[error("branch `{0}` does not exist")]
    UnknownBranch(String),
    #[error("worktree `{0}` does not exist")]
    UnknownWorktree(String),
    #[error("a worktree named `{0}` already exists")]
    WorktreeExists(String),
    #[error("branch `{0}` is checked out in another worktree")]
    RefusedCheckedOut(String),
    #[error("worktree `{0}` has uncommitted changes")]
    RefusedDirtyWorktree(String),
    #[error("worktree `{0}` is locked")]
    RefusedLockedWorktree(String),
    #[error("worktree `{0}` has a Git operation in progress: {1}")]
    RefusedActiveOperation(String, String),
    #[error("destination `{0}` is not empty")]
    RefusedDestinationNotEmpty(String),
    #[error("branch `{0}` is already checked out in another worktree")]
    RefusedBranchAlreadyCheckedOut(String),
    #[error("Git branch deadline exceeded")]
    Deadline,
    #[error("Git branch request was cancelled")]
    Cancelled,
    #[error("Git branch worker stopped")]
    Stopped,
    #[error("Git failed: {0}")]
    Backend(String),
}
pub(super) enum GitBranchEvent {
    Complete(Result<GitBranchResult, GitBranchError>),
}
#[derive(Default)]
pub struct GitBranchService {
    pub(super) next: u64,
    active: Option<GitBranchGeneration>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitBranchEvent>>,
    accepted: Option<GitBranchResult>,
}
impl GitBranchService {
    pub fn start(
        &mut self,
        request: GitBranchRequest,
    ) -> Result<GitBranchGeneration, GitBranchError> {
        if !valid_request(&request) {
            return Err(GitBranchError::Invalid);
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitBranchGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-branch-{}", generation.0))
            .spawn(move || {
                let result = run(generation, &request, &worker);
                let _ = tx.send(GitBranchEvent::Complete(result));
            })
            .map_err(|_| GitBranchError::Stopped)?;
        self.active = Some(generation);
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release)
        }
        self.active = None;
        self.receiver = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitBranchResult>, GitBranchError> {
        let event = match self.receiver.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return Ok(None),
        };
        match event {
            Ok(GitBranchEvent::Complete(result)) => {
                let expected = self.active.take();
                self.receiver = None;
                self.cancel = None;
                let result = result?;
                // A page carries its generation; a mutation is accepted for the active request.
                if let GitBranchResult::List(list) = &result
                    && Some(list.generation) != expected
                {
                    return Ok(None);
                }
                self.accepted = Some(result);
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.cancel();
                Err(GitBranchError::Stopped)
            }
        }
    }
}
impl Drop for GitBranchService {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn valid_request(request: &GitBranchRequest) -> bool {
    let (repository, deadline) = match request {
        GitBranchRequest::List(request) => (&request.repository, request.deadline),
        GitBranchRequest::CreateWorktree {
            repository,
            name,
            path,
            reference,
            deadline,
        } => {
            if name.trim().is_empty() || path.as_os_str().is_empty() || reference.trim().is_empty()
            {
                return false;
            }
            (repository, *deadline)
        }
        GitBranchRequest::RemoveWorktree {
            repository,
            name,
            deadline,
        } => {
            if name.trim().is_empty() {
                return false;
            }
            (repository, *deadline)
        }
        GitBranchRequest::CreateDetachedWorktree {
            repository,
            name,
            path,
            revision,
            deadline,
        } => {
            if name.trim().is_empty() || path.as_os_str().is_empty() || revision.trim().is_empty() {
                return false;
            }
            (repository, *deadline)
        }
    };
    !repository.as_os_str().is_empty() && !deadline.is_zero() && deadline <= Duration::from_secs(30)
}

fn open(repository: &Path) -> Result<git2::Repository, GitBranchError> {
    git2::Repository::open(repository).map_err(|_| GitBranchError::NotRepository)
}

/// One linked worktree as read for attribution: name, path, branch and lock state.
struct WorktreeRef {
    name: String,
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

/// The linked worktrees and the branch each has checked out.
fn worktree_branches(repository: &git2::Repository) -> Result<Vec<WorktreeRef>, GitBranchError> {
    let names = repository.worktrees().map_err(backend)?;
    let mut found = Vec::new();
    for index in 0..names.len() {
        let Some(name) = names.get(index) else {
            continue;
        };
        let Ok(worktree) = repository.find_worktree(name) else {
            continue;
        };
        let path = worktree.path().to_path_buf();
        let branch = git2::Repository::open(&path).ok().and_then(|opened| {
            opened
                .head()
                .ok()
                .and_then(|head| head.shorthand().map(str::to_owned))
        });
        let locked = worktree
            .is_locked()
            .is_ok_and(|status| !matches!(status, git2::WorktreeLockStatus::Unlocked));
        found.push(WorktreeRef {
            name: name.to_owned(),
            path,
            branch,
            locked,
        });
    }
    Ok(found)
}

/// Git's own reason, without libgit2's class and code suffix.
fn backend(error: impl std::fmt::Display) -> GitBranchError {
    let message = error.to_string();
    let reason = message.split("; class=").next().unwrap_or(&message);
    GitBranchError::Backend(reason.chars().take(512).collect())
}

fn operation_name(state: git2::RepositoryState) -> Option<String> {
    match state {
        git2::RepositoryState::Clean => None,
        git2::RepositoryState::Merge => Some("merge".into()),
        git2::RepositoryState::Revert | git2::RepositoryState::RevertSequence => {
            Some("revert".into())
        }
        git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
            Some("cherry-pick".into())
        }
        git2::RepositoryState::Bisect => Some("bisect".into()),
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive
        | git2::RepositoryState::RebaseMerge => Some("rebase".into()),
        git2::RepositoryState::ApplyMailbox | git2::RepositoryState::ApplyMailboxOrRebase => {
            Some("apply-mailbox".into())
        }
    }
}

fn read_list(
    generation: GitBranchGeneration,
    request: &GitBranchListRequest,
    cancel: &AtomicBool,
) -> Result<GitBranchResult, GitBranchError> {
    let repository = open(&request.repository)?;
    let started = Instant::now();
    // Opened from a linked worktree, `repository` is that worktree; the main checkout sits
    // beside the common Git directory.
    let linked_from = repository.is_worktree().then(|| {
        repository
            .commondir()
            .parent()
            .and_then(|root| git2::Repository::open(root).ok())
    });
    let main = linked_from
        .as_ref()
        .and_then(Option::as_ref)
        .unwrap_or(&repository);
    let main_path = main
        .workdir()
        .and_then(|path| std::fs::canonicalize(path).ok());
    let mut worktrees = vec![GitWorktreeEntry {
        name: "main".into(),
        path: main_path.clone().unwrap_or_default(),
        branch: main
            .head()
            .ok()
            .and_then(|head| head.shorthand().map(str::to_owned)),
        dirty: has_changes(main)?,
        operation: operation_name(main.state()),
        locked: false,
        main: true,
    }];
    for linked in worktree_branches(&repository)? {
        if cancel.load(Ordering::Acquire) {
            return Err(GitBranchError::Cancelled);
        }
        if started.elapsed() > request.deadline {
            return Err(GitBranchError::Deadline);
        }
        let opened = git2::Repository::open(&linked.path).ok();
        let (dirty, operation) = match &opened {
            Some(opened) => (has_changes(opened)?, operation_name(opened.state())),
            None => (false, None),
        };
        worktrees.push(GitWorktreeEntry {
            name: linked.name,
            path: linked.path,
            branch: linked.branch,
            dirty,
            operation,
            locked: linked.locked,
            main: false,
        });
    }
    let mut branches = Vec::new();
    for kind in [git2::BranchType::Local, git2::BranchType::Remote] {
        let iterator = repository.branches(Some(kind)).map_err(backend)?;
        for entry in iterator {
            if cancel.load(Ordering::Acquire) {
                return Err(GitBranchError::Cancelled);
            }
            let (branch, _) = entry.map_err(backend)?;
            let Some(name) = branch.name().ok().flatten().map(str::to_owned) else {
                continue;
            };
            let upstream = branch
                .upstream()
                .ok()
                .and_then(|upstream| upstream.name().ok().flatten().map(str::to_owned));
            let checked_out = worktrees
                .iter()
                .find(|worktree| worktree.branch.as_deref() == Some(name.as_str()))
                .map(|worktree| worktree.path.clone());
            branches.push(GitBranchEntry {
                name,
                remote: kind == git2::BranchType::Remote,
                upstream,
                checked_out,
                head: !branch.is_head().eq(&false) && branch.is_head(),
            });
        }
    }
    Ok(GitBranchResult::List(GitBranchList {
        generation,
        branches,
        worktrees,
    }))
}

fn has_changes(repository: &git2::Repository) -> Result<bool, GitBranchError> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false);
    let statuses = repository.statuses(Some(&mut options)).map_err(backend)?;
    Ok(!statuses.is_empty())
}

/// A worktree inside the main checkout would otherwise show as untracked in its status.
fn exclude_from_status(repository: &git2::Repository, path: &Path) {
    let Some(root) = repository.commondir().parent() else {
        return;
    };
    let Ok(relative) = path.strip_prefix(root) else {
        return;
    };
    let relative = relative.to_string_lossy();
    if relative.is_empty() || relative.contains(['\n', '\r']) {
        return;
    }
    let mut line = String::from("/");
    for ch in relative.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']' | ' ' | '#' | '!') {
            line.push('\\');
        }
        line.push(ch);
    }
    line.push('/');
    let exclude = repository.commondir().join("info").join("exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if current.lines().any(|existing| existing == line) {
        return;
    }
    if let Some(parent) = exclude.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &exclude,
        format!(
            "{current}{}{line}\n",
            if current.is_empty() || current.ends_with('\n') {
                ""
            } else {
                "\n"
            }
        ),
    );
}

fn checked_out_path(
    repository: &git2::Repository,
    name: &str,
) -> Result<Option<PathBuf>, GitBranchError> {
    Ok(worktree_branches(repository)?
        .into_iter()
        .find(|linked| linked.branch.as_deref() == Some(name))
        .map(|linked| linked.path))
}

fn run(
    generation: GitBranchGeneration,
    request: &GitBranchRequest,
    cancel: &AtomicBool,
) -> Result<GitBranchResult, GitBranchError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitBranchError::Cancelled);
    }
    match request {
        GitBranchRequest::List(request) => read_list(generation, request, cancel),
        GitBranchRequest::CreateDetachedWorktree {
            repository,
            name,
            path,
            revision,
            deadline,
        } => {
            let repository = open(repository)?;
            let _ = deadline;
            // Validate everything that could refuse before creating anything.
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (!metadata.file_type().is_dir()
                    || std::fs::read_dir(path).map_err(backend)?.next().is_some())
            {
                return Err(GitBranchError::RefusedDestinationNotEmpty(
                    path.display().to_string(),
                ));
            }
            if operation_name(repository.state()).is_some() {
                return Err(GitBranchError::RefusedActiveOperation(
                    "main".into(),
                    operation_name(repository.state()).unwrap_or_default(),
                ));
            }
            if repository.find_worktree(name).is_ok() {
                return Err(GitBranchError::UnknownWorktree(name.clone()));
            }
            // The revision must be a real object: a review checks out what was INSPECTED.
            let object = repository
                .revparse_single(revision)
                .map_err(|_| GitBranchError::UnknownBranch(revision.clone()))?;
            let oid = object
                .peel_to_commit()
                .map_err(|_| GitBranchError::UnknownBranch(revision.clone()))?
                .id();
            // Git requires a new worktree to START on a branch (libgit2: "reference is not a
            // branch"), and the main HEAD's branch is already checked out - so a temporary branch
            // is made for the start, and REMOVED once HEAD is detached. The branch list is
            // therefore exactly what it was before the review was looked at.
            let temporary_branch = format!("dust-review/{name}");
            let starting = repository
                .reference(
                    &format!("refs/heads/{temporary_branch}"),
                    oid,
                    true,
                    "review checkout",
                )
                .map_err(backend)?;
            let mut options = git2::WorktreeAddOptions::new();
            options.reference(Some(&starting));
            let created = repository.worktree(name, path, Some(&options));
            drop(starting);
            created.map_err(backend)?;
            // DETACHED: no branch is created and none is moved by looking at a review.
            let linked = git2::Repository::open(path).map_err(backend)?;
            linked.set_head_detached(oid).map_err(backend)?;
            let mut checkout = git2::build::CheckoutBuilder::new();
            checkout.force();
            linked.checkout_head(Some(&mut checkout)).map_err(backend)?;
            // The starting branch has served its purpose: HEAD is detached now, so it can go and
            // no branch is left behind by looking at a review.
            if let Ok(mut branch) =
                repository.find_branch(&temporary_branch, git2::BranchType::Local)
            {
                let _ = branch.delete();
            }
            Ok(GitBranchResult::Changed)
        }
        GitBranchRequest::CreateWorktree {
            repository,
            name,
            path,
            reference,
            deadline,
        } => {
            let repository = open(repository)?;
            let _ = deadline;
            // A relative path starts at the main checkout, from any worktree.
            let main_root = repository.commondir().parent().map(Path::to_path_buf);
            let path = main_root
                .as_deref()
                .map_or(path.clone(), |root| root.join(path));
            let path = path.as_path();
            // Validate everything that could refuse before creating anything.
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (!metadata.file_type().is_dir()
                    || std::fs::read_dir(path).map_err(backend)?.next().is_some())
            {
                return Err(GitBranchError::RefusedDestinationNotEmpty(
                    path.display().to_string(),
                ));
            }
            let short = reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference.as_str());
            if checked_out_path(&repository, short)?.is_some() {
                return Err(GitBranchError::RefusedBranchAlreadyCheckedOut(
                    short.to_owned(),
                ));
            }
            if operation_name(repository.state()).is_some() {
                return Err(GitBranchError::RefusedActiveOperation(
                    "main".into(),
                    operation_name(repository.state()).unwrap_or_default(),
                ));
            }
            if repository.find_worktree(name).is_ok() {
                return Err(GitBranchError::WorktreeExists(name.clone()));
            }
            // A branch that doesn't exist yet starts at HEAD, like `git worktree add -b`.
            let existing = repository.find_branch(short, git2::BranchType::Local).ok();
            let new_branch = existing.is_none();
            let branch = match existing {
                Some(branch) => branch,
                None => {
                    let head = repository
                        .head()
                        .and_then(|head| head.peel_to_commit())
                        .map_err(backend)?;
                    repository
                        .branch(short, &head, false)
                        .map_err(|_| GitBranchError::UnknownBranch(short.to_owned()))?
                }
            };
            let reference = branch.into_reference();
            let mut options = git2::WorktreeAddOptions::new();
            options.reference(Some(&reference));
            let added = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .map_err(backend)
                .and_then(|()| {
                    repository
                        .worktree(name, path, Some(&options))
                        .map_err(backend)
                });
            if let Err(error) = added {
                if new_branch
                    && let Ok(mut branch) = repository.find_branch(short, git2::BranchType::Local)
                {
                    let _ = branch.delete();
                }
                return Err(error);
            }
            exclude_from_status(&repository, path);
            Ok(GitBranchResult::Changed)
        }
        GitBranchRequest::RemoveWorktree {
            repository,
            name,
            deadline,
        } => {
            let repository = open(repository)?;
            let _ = deadline;
            let worktree = repository
                .find_worktree(name)
                .map_err(|_| GitBranchError::UnknownWorktree(name.clone()))?;
            if !matches!(
                worktree
                    .is_locked()
                    .unwrap_or(git2::WorktreeLockStatus::Unlocked),
                git2::WorktreeLockStatus::Unlocked
            ) {
                return Err(GitBranchError::RefusedLockedWorktree(name.clone()));
            }
            let path = worktree.path().to_path_buf();
            if path.try_exists().map_err(backend)? {
                let opened = git2::Repository::open(&path).map_err(backend)?;
                let workdir = opened.workdir().and_then(|path| path.canonicalize().ok());
                if workdir != Some(path.canonicalize().map_err(backend)?) {
                    return Err(GitBranchError::RefusedDirtyWorktree(name.clone()));
                }
                if has_changes(&opened)? {
                    return Err(GitBranchError::RefusedDirtyWorktree(name.clone()));
                }
                if let Some(operation) = operation_name(opened.state()) {
                    return Err(GitBranchError::RefusedActiveOperation(
                        name.clone(),
                        operation,
                    ));
                }
            }
            let mut options = git2::WorktreePruneOptions::new();
            options.valid(true);
            worktree.prune(Some(&mut options)).map_err(backend)?;
            // Pruning removes the worktree's administrative data. The directory itself is
            // only removed here, after every dirtiness and operation check above.
            if path.exists() {
                std::fs::remove_dir_all(&path).map_err(backend)?;
            }
            Ok(GitBranchResult::Changed)
        }
    }
}
