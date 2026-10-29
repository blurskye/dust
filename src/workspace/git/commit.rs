//! Staged-only commit composition. Worker-owned, bounded, and pinned to the HEAD and index
//! it captured. This owner never runs repository hooks and never signs: libgit2 provides no
//! hook execution at any layer, so a commit that would need one is refused rather than
//! performed without it.
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

use super::{
    GitHeadExpectation, GitHeadId, GitIndexFingerprint, git_head_id, git_index_fingerprint,
};

/// Hook names this owner would have to run and cannot.
const HOOK_NAMES: [&str; 4] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitCommitGeneration(pub u64);
#[derive(Clone, Debug)]
pub struct GitCommitRequest {
    /// Canonical worktree path of the repository to commit into.
    pub repository: PathBuf,
    pub message: String,
    /// Replace the tip instead of adding a child of it.
    pub amend: bool,
    pub expected_head: GitHeadExpectation,
    pub expected_index: GitIndexFingerprint,
    pub deadline: Duration,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommitResult {
    pub generation: GitCommitGeneration,
    pub commit: GitHeadId,
    pub amended: bool,
    /// The message the amended commit replaced, shown to the user rather than discarded.
    pub replaced_message: Option<String>,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitCommitError {
    #[error("invalid commit request")]
    Invalid,
    #[error("Git HEAD changed since the message was composed")]
    StaleHead,
    #[error("Git index changed since the message was composed")]
    StaleIndex,
    #[error("nothing is staged; stage changes before committing")]
    EmptyIndex,
    #[error("this repository configures the {0} hook, and Dust cannot run hooks yet")]
    HookConfigured(String),
    #[error("this repository requests commit signing ({0}), and Dust cannot sign commits yet")]
    SigningRequested(String),
    #[error("commit identity is incomplete: {0} is not configured")]
    MissingIdentity(String),
    #[error("the amended commit does not exist yet")]
    NothingToAmend,
    #[error("Git commit deadline exceeded")]
    Deadline,
    #[error("Git commit was cancelled")]
    Cancelled,
    #[error("Git commit worker stopped")]
    Stopped,
    #[error("Git commit backend failed")]
    Backend,
    #[error("Git commit postcondition failed")]
    Postcondition,
}
pub(super) enum GitCommitEvent {
    Complete(Result<GitCommitResult, GitCommitError>),
}
#[derive(Default)]
pub struct GitCommitService {
    pub(super) next: u64,
    active: Option<GitCommitGeneration>,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<GitCommitEvent>>,
    accepted: Option<GitCommitResult>,
}
impl GitCommitService {
    pub fn start(
        &mut self,
        request: GitCommitRequest,
    ) -> Result<GitCommitGeneration, GitCommitError> {
        if request.repository.as_os_str().is_empty()
            || request.message.trim().is_empty()
            || request.message.len() > 100_000
            || request.message.contains('\0')
            || request.deadline.is_zero()
            || request.deadline > Duration::from_secs(5)
        {
            return Err(GitCommitError::Invalid);
        }
        self.cancel();
        self.next = self.next.saturating_add(1);
        let generation = GitCommitGeneration(self.next);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("dust-git-commit-{}", generation.0))
            .spawn(move || {
                let result = compose_commit(generation, &request, &worker);
                let _ = tx.send(GitCommitEvent::Complete(result));
            })
            .map_err(|_| GitCommitError::Stopped)?;
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
    pub fn poll(&mut self) -> Result<Option<&GitCommitResult>, GitCommitError> {
        let event = match self.receiver.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return Ok(None),
        };
        match event {
            Ok(GitCommitEvent::Complete(result)) => {
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
                Err(GitCommitError::Stopped)
            }
        }
    }
}
impl Drop for GitCommitService {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

/// The first hook this repository would expect us to run. A `*.sample` file is not a hook,
/// and a non-executable file is not one either.
fn configured_hook(repo: &git2::Repository) -> Option<String> {
    let configured = repo
        .config()
        .ok()
        .and_then(|config| config.get_string("core.hooksPath").ok());
    let directory = match configured {
        Some(path) => {
            let path = PathBuf::from(path);
            match (path.is_absolute(), repo.workdir()) {
                (true, _) => path,
                (false, Some(workdir)) => workdir.join(path),
                (false, None) => return None,
            }
        }
        None => repo.path().join("hooks"),
    };
    for name in HOOK_NAMES {
        let path = directory.join(name);
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && metadata.file_type().is_file()
            && is_executable(&metadata)
        {
            return Some(path.display().to_string());
        }
    }
    None
}

fn signing_setting(repo: &git2::Repository) -> Option<String> {
    let config = repo.config().ok()?;
    if config.get_bool("commit.gpgsign").unwrap_or(false) {
        return Some("commit.gpgsign".into());
    }
    None
}

fn committer(repo: &git2::Repository) -> Result<git2::Signature<'static>, GitCommitError> {
    let config = repo.config().map_err(|_| GitCommitError::Backend)?;
    let name = config
        .get_string("user.name")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let email = config
        .get_string("user.email")
        .ok()
        .filter(|value| !value.trim().is_empty());
    match (name, email) {
        (Some(name), Some(email)) => {
            git2::Signature::now(&name, &email).map_err(|_| GitCommitError::Backend)
        }
        (None, Some(_)) => Err(GitCommitError::MissingIdentity("user.name".into())),
        (Some(_), None) => Err(GitCommitError::MissingIdentity("user.email".into())),
        (None, None) => Err(GitCommitError::MissingIdentity(
            "user.name and user.email".into(),
        )),
    }
}

pub(super) fn compose_commit(
    generation: GitCommitGeneration,
    request: &GitCommitRequest,
    cancel: &AtomicBool,
) -> Result<GitCommitResult, GitCommitError> {
    if cancel.load(Ordering::Acquire) {
        return Err(GitCommitError::Cancelled);
    }
    let started = Instant::now();
    let repo = git2::Repository::open(&request.repository).map_err(|_| GitCommitError::Backend)?;

    // Every capture is checked before the first write.
    if !request
        .expected_head
        .matches(git_head_id(&request.repository).map_err(|_| GitCommitError::Backend)?)
    {
        return Err(GitCommitError::StaleHead);
    }
    let before = git_index_fingerprint(&request.repository).map_err(|_| GitCommitError::Backend)?;
    if request.expected_index != before {
        return Err(GitCommitError::StaleIndex);
    }
    // Hooks and signing are refusals, not warnings: performing the commit without a hook the
    // repository configured would bypass it silently.
    if let Some(hook) = configured_hook(&repo) {
        return Err(GitCommitError::HookConfigured(hook));
    }
    if let Some(setting) = signing_setting(&repo) {
        return Err(GitCommitError::SigningRequested(setting));
    }

    let index_tree = repo
        .index()
        .and_then(|mut index| index.write_tree())
        .map_err(|_| GitCommitError::Backend)?;
    let head = repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok());
    if request.amend && head.is_none() {
        return Err(GitCommitError::NothingToAmend);
    }
    let head_tree = head.as_ref().and_then(|commit| commit.tree().ok());
    if !request.amend && head_tree.as_ref().map(git2::Tree::id) == Some(index_tree) {
        return Err(GitCommitError::EmptyIndex);
    }
    let replaced_message = request
        .amend
        .then(|| {
            head.as_ref()
                .and_then(|commit| commit.message().map(str::to_owned))
        })
        .flatten();

    let signature = committer(&repo)?;
    let tree = repo
        .find_tree(index_tree)
        .map_err(|_| GitCommitError::Backend)?;
    let parents: Vec<git2::Commit> = match (&head, request.amend) {
        (Some(commit), true) => commit.parents().collect(),
        (Some(commit), false) => vec![commit.clone()],
        _ => Vec::new(),
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    if cancel.load(Ordering::Acquire) {
        return Err(GitCommitError::Cancelled);
    }
    if started.elapsed() > request.deadline {
        return Err(GitCommitError::Deadline);
    }
    // Re-check both captures immediately before the write.
    if !request
        .expected_head
        .matches(git_head_id(&request.repository).map_err(|_| GitCommitError::Backend)?)
    {
        return Err(GitCommitError::StaleHead);
    }
    if git_index_fingerprint(&request.repository).map_err(|_| GitCommitError::Backend)? != before {
        return Err(GitCommitError::StaleIndex);
    }

    // libgit2 refuses `git_commit_create` with a ref update when the first parent is not the
    // current tip, and an amend's first parent is by definition the tip's parent. Such a
    // commit is therefore written as an object and the resolved branch ref is moved explicitly.
    let oid = if request.amend {
        let name = repo
            .head()
            .ok()
            .and_then(|head| head.name().map(str::to_owned))
            .ok_or(GitCommitError::Backend)?;
        let oid = repo
            .commit(
                None,
                &signature,
                &signature,
                &request.message,
                &tree,
                &parent_refs,
            )
            .map_err(|_| GitCommitError::Backend)?;
        repo.reference(
            &name,
            oid,
            true,
            &format!("commit (amend): {}", request.message.trim()),
        )
        .map_err(|_| GitCommitError::Backend)?;
        oid
    } else {
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &request.message,
            &tree,
            &parent_refs,
        )
        .map_err(|_| GitCommitError::Backend)?
    };
    let after = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .ok_or(GitCommitError::Postcondition)?;
    if after != oid {
        return Err(GitCommitError::Postcondition);
    }
    Ok(GitCommitResult {
        generation,
        commit: GitHeadId(after.as_bytes().try_into().unwrap_or([0; 20])),
        amended: request.amend,
        replaced_message,
    })
}

/// Read the tip message so an amend composer can show what it replaces.
pub fn git_tip_message(repository: &Path) -> Result<Option<String>, GitCommitError> {
    let repo = git2::Repository::open(repository).map_err(|_| GitCommitError::Backend)?;
    Ok(repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .and_then(|commit| commit.message().map(str::to_owned)))
}
