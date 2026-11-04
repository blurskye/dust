//! Branch and worktree listing, creation and removal. Check refusal conditions before writes;
//! retain running mutations until their completion is handled.
use std::{
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
    #[error("a worktree mutation is still running; wait for it to finish")]
    Busy,
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
    mutating: bool,
}
impl GitBranchService {
    pub fn start(
        &mut self,
        request: GitBranchRequest,
    ) -> Result<GitBranchGeneration, GitBranchError> {
        if !valid_request(&request) {
            return Err(GitBranchError::Invalid);
        }
        if self.mutating && self.receiver.is_some() {
            return Err(GitBranchError::Busy);
        }
        self.cancel();
        let mutating = !matches!(request, GitBranchRequest::List(_));
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
        self.mutating = mutating;
        self.active = Some(generation);
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release)
        }
        // A mutation may already be writing. Keep its completion owned until poll handles it.
        if !self.mutating {
            self.active = None;
            self.receiver = None;
        }
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
                self.mutating = false;
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
                self.mutating = false;
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

fn check_request(cancel: &AtomicBool, expires: Instant) -> Result<(), GitBranchError> {
    if cancel.load(Ordering::Acquire) {
        Err(GitBranchError::Cancelled)
    } else if Instant::now() >= expires {
        Err(GitBranchError::Deadline)
    } else {
        Ok(())
    }
}

fn main_repository(repository: &git2::Repository) -> Result<git2::Repository, GitBranchError> {
    if !repository.is_worktree() {
        return open(repository.workdir().ok_or(GitBranchError::NotRepository)?);
    }
    let main = open(repository.commondir())?;
    // libgit2 honors core.worktree; otherwise it infers a conventional .git directory.
    // A separate Git directory without core.worktree has no backlink to its primary checkout.
    let path = main.workdir().ok_or(GitBranchError::NotRepository)?;
    if !path.join(".git").exists() {
        return Err(GitBranchError::Backend(
            "cannot locate the primary checkout; open it directly or set core.worktree".into(),
        ));
    }
    let checked = open(path)?;
    if checked.is_worktree()
        || checked.commondir().canonicalize().map_err(backend)?
            != repository.commondir().canonicalize().map_err(backend)?
    {
        return Err(GitBranchError::NotRepository);
    }
    Ok(main)
}

fn checkout_options(cancel: &AtomicBool, expires: Instant) -> git2::build::CheckoutBuilder<'_> {
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.notify_on(git2::CheckoutNotificationType::all());
    checkout.notify(move |_, _, _, _, _| check_request(cancel, expires).is_ok());
    checkout
}

/// One linked worktree as read for attribution: name, path, branch and lock state.
struct WorktreeRef {
    name: String,
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

/// The linked worktrees and the branch each has checked out.
fn worktree_branches(
    repository: &git2::Repository,
    cancel: &AtomicBool,
    expires: Instant,
) -> Result<Vec<WorktreeRef>, GitBranchError> {
    check_request(cancel, expires)?;
    let names = repository.worktrees().map_err(backend)?;
    let mut found = Vec::new();
    for index in 0..names.len() {
        check_request(cancel, expires)?;
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
                .filter(|head| head.is_branch())
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
    check_request(cancel, expires)?;
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
    expires: Instant,
) -> Result<GitBranchResult, GitBranchError> {
    check_request(cancel, expires)?;
    let repository = open(&request.repository)?;
    let main = main_repository(&repository)?;
    let main_path = main
        .workdir()
        .and_then(|path| std::fs::canonicalize(path).ok());
    let mut worktrees = vec![GitWorktreeEntry {
        name: "main".into(),
        path: main_path.clone().unwrap_or_default(),
        branch: main
            .head()
            .ok()
            .filter(|head| head.is_branch())
            .and_then(|head| head.shorthand().map(str::to_owned)),
        dirty: has_changes(&main, cancel, expires)?,
        operation: operation_name(main.state()),
        locked: false,
        main: true,
    }];
    for linked in worktree_branches(&repository, cancel, expires)? {
        check_request(cancel, expires)?;
        let opened = git2::Repository::open(&linked.path).ok();
        let (dirty, operation) = match &opened {
            Some(opened) => (has_changes(opened, cancel, expires)?, operation_name(opened.state())),
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
            check_request(cancel, expires)?;
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
    check_request(cancel, expires)?;
    Ok(GitBranchResult::List(GitBranchList {
        generation,
        branches,
        worktrees,
    }))
}

fn has_changes(
    repository: &git2::Repository,
    cancel: &AtomicBool,
    expires: Instant,
) -> Result<bool, GitBranchError> {
    check_request(cancel, expires)?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false);
    let statuses = repository.statuses(Some(&mut options)).map_err(backend)?;
    check_request(cancel, expires)?;
    Ok(!statuses.is_empty())
}

/// A worktree inside the main checkout would otherwise show as untracked in its status.
fn exclude_from_status(
    repository: &git2::Repository,
    path: &Path,
    cancel: &AtomicBool,
    expires: Instant,
) -> Result<(), GitBranchError> {
    use std::io::{Read, Write};
    let root = repository.workdir().ok_or(GitBranchError::NotRepository)?;
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(());
    };
    let relative = relative.to_string_lossy();
    if relative.is_empty() || relative.contains(['\n', '\r']) {
        return Ok(());
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
    check_request(cancel, expires)?;
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).map_err(backend)?;
    }
    check_request(cancel, expires)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&exclude)
        .map_err(backend)?;
    let mut file = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, error)| backend(error))?;
    check_request(cancel, expires)?;
    let mut current = Vec::new();
    file.read_to_end(&mut current).map_err(backend)?;
    if current.split(|byte| *byte == b'\n').any(|existing| existing == line.as_bytes()) {
        return Ok(());
    }
    let addition = format!("{}{line}\n", if current.is_empty() || current.ends_with(b"\n") {
        ""
    } else {
        "\n"
    });
    check_request(cancel, expires)?;
    file.write_all(addition.as_bytes()).map_err(backend)
}

fn checked_out_path(
    repository: &git2::Repository,
    name: &str,
    cancel: &AtomicBool,
    expires: Instant,
) -> Result<Option<PathBuf>, GitBranchError> {
    Ok(worktree_branches(repository, cancel, expires)?
        .into_iter()
        .find(|linked| linked.branch.as_deref() == Some(name))
        .map(|linked| linked.path))
}

fn run(
    generation: GitBranchGeneration,
    request: &GitBranchRequest,
    cancel: &AtomicBool,
) -> Result<GitBranchResult, GitBranchError> {
    let deadline = match request {
        GitBranchRequest::List(request) => request.deadline,
        GitBranchRequest::CreateWorktree { deadline, .. }
        | GitBranchRequest::RemoveWorktree { deadline, .. }
        | GitBranchRequest::CreateDetachedWorktree { deadline, .. } => *deadline,
    };
    let expires = Instant::now() + deadline;
    check_request(cancel, expires)?;
    match request {
        GitBranchRequest::List(request) => read_list(generation, request, cancel, expires),
        GitBranchRequest::CreateDetachedWorktree {
            repository,
            name,
            path,
            revision,
            ..
        } => {
            let repository = open(repository)?;
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
            check_request(cancel, expires)?;
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
            let created = check_request(cancel, expires).and_then(|()| {
                repository.worktree(name, path, Some(&options)).map_err(backend)
            });
            drop(starting);
            if let Err(error) = created {
                if let Ok(mut branch) = repository.find_branch(&temporary_branch, git2::BranchType::Local) {
                    let _ = branch.delete();
                }
                check_request(cancel, expires)?;
                return Err(error);
            }
            // DETACHED: no branch is created and none is moved by looking at a review.
            let linked = git2::Repository::open(path).map_err(backend)?;
            linked.set_head_detached(oid).map_err(backend)?;
            let mut checkout = checkout_options(cancel, expires);
            checkout.force();
            let checked_out = linked.checkout_head(Some(&mut checkout));
            // The starting branch has served its purpose: HEAD is detached now, so it can go and
            // no branch is left behind by looking at a review.
            if let Ok(mut branch) =
                repository.find_branch(&temporary_branch, git2::BranchType::Local)
            {
                let _ = branch.delete();
            }
            check_request(cancel, expires)?;
            checked_out.map_err(backend)?;
            Ok(GitBranchResult::Changed)
        }
        GitBranchRequest::CreateWorktree {
            repository,
            name,
            path,
            reference,
            ..
        } => {
            let repository = open(repository)?;
            // Absolute destinations do not require locating a separate primary checkout.
            // Relative destinations must never be anchored to the metadata directory.
            let main = if path.is_absolute() {
                main_repository(&repository).ok()
            } else {
                Some(main_repository(&repository)?)
            };
            let path = if path.is_absolute() {
                path.clone()
            } else {
                main.as_ref()
                    .and_then(|main| main.workdir())
                    .ok_or(GitBranchError::NotRepository)?
                    .join(path)
            };
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
            if checked_out_path(&repository, short, cancel, expires)?.is_some() {
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
            check_request(cancel, expires)?;
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
            let added = check_request(cancel, expires).and_then(|()| path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .map_err(backend)
                .and_then(|()| {
                    check_request(cancel, expires)?;
                    repository
                        .worktree(name, path, Some(&options))
                        .map_err(backend)
                }));
            if let Err(error) = added {
                if new_branch
                    && let Ok(mut branch) = repository.find_branch(short, git2::BranchType::Local)
                {
                    let _ = branch.delete();
                }
                check_request(cancel, expires)?;
                return Err(error);
            }
            check_request(cancel, expires)?;
            if let Some(main) = main {
                exclude_from_status(&main, path, cancel, expires)?;
            }
            check_request(cancel, expires)?;
            Ok(GitBranchResult::Changed)
        }
        GitBranchRequest::RemoveWorktree {
            repository,
            name,
            ..
        } => {
            let repository = open(repository)?;
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
                if has_changes(&opened, cancel, expires)? {
                    return Err(GitBranchError::RefusedDirtyWorktree(name.clone()));
                }
                if let Some(operation) = operation_name(opened.state()) {
                    return Err(GitBranchError::RefusedActiveOperation(
                        name.clone(),
                        operation,
                    ));
                }
            }
            check_request(cancel, expires)?;
            // Keep the administrative data if deletion fails, so the checkout remains registered.
            if path.exists() {
                std::fs::remove_dir_all(&path).map_err(backend)?;
            }
            let mut options = git2::WorktreePruneOptions::new();
            options.valid(true);
            worktree.prune(Some(&mut options)).map_err(backend)?;
            check_request(cancel, expires)?;
            Ok(GitBranchResult::Changed)
        }
    }
}

#[cfg(test)]
mod exclude_tests {
    use super::*;

    #[test]
    fn concurrent_exclude_updates_preserve_patterns_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(root.path()).unwrap();
        let exclude = repository.commondir().join("info/exclude");
        std::fs::write(&exclude, b"/retained-\xff").unwrap();
        let barrier = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for index in 0..16 {
                let root = root.path();
                let barrier = &barrier;
                scope.spawn(move || {
                    let repository = git2::Repository::open(root).unwrap();
                    let cancel = AtomicBool::new(false);
                    let expires = Instant::now() + Duration::from_secs(10);
                    barrier.wait();
                    for name in [format!("child-{index}"), "shared".into()] {
                        exclude_from_status(&repository, &root.join(name), &cancel, expires).unwrap();
                    }
                });
            }
        });
        let bytes = std::fs::read(exclude).unwrap();
        assert!(bytes.starts_with(b"/retained-\xff\n"));
        let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        for name in (0..16).map(|index| format!("/child-{index}/")).chain(["/shared/".into()]) {
            assert_eq!(lines.iter().filter(|line| **line == name.as_bytes()).count(), 1, "{name}");
        }
    }
}
