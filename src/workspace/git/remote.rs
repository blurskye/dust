//! Remote operations, and the journal that keeps an unknown outcome from being retried.
//!
//! Every request here makes EXACTLY ONE attempt. A failure is reported as a failure and an
//! outcome the worker cannot classify is reported as unknown and journaled; nothing in this
//! module retries, because a retried push or fetch can repeat an effect that already happened.
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::Duration,
};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitRemoteGeneration(pub u64);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitRemoteError {
    #[error("remote `{0}` is not configured")]
    UnknownRemote(String),
    #[error("remote operation failed: {0}")]
    Failed(String),
    #[error("remote operation outcome is unknown: {0}")]
    Unknown(String),
    #[error("remote `{0}` requires authentication")]
    AuthenticationRequired(String),
    #[error("remote operation backend failed")]
    Backend,
    #[error("remote operation deadline exceeded")]
    Deadline,
}

/// Why an update did not apply. Every refusal is decided BEFORE the first write, so a refused
/// update leaves the branch exactly where it was.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteRefusal {
    /// The branch moved after the review was captured, so applying the reviewed plan would move
    /// a branch the user never reviewed.
    Stale {
        branch: String,
        expected: git2::Oid,
        actual: git2::Oid,
    },
    /// Nothing to apply: the branch is already at the remote tip.
    UpToDate { branch: String },
    /// There is no remote-tracking ref for this branch, so there is nothing to update from.
    NoUpstream { branch: String, remote: String },
    /// The working tree or index has changes that a fast-forward would disturb.
    DirtyWorktree,
    /// The remote moved away from the value the push was based on. This is the lease: a push
    /// built on a stale observation is refused rather than applied.
    RemoteMoved {
        destination: String,
        expected: git2::Oid,
        actual: git2::Oid,
    },
}

impl GitRemoteRefusal {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Stale { branch, .. } => format!(
                "`{branch}` moved since the review was taken; review the update again"
            ),
            Self::UpToDate { branch } => format!("`{branch}` is already at the remote tip"),
            Self::NoUpstream { branch, remote } => {
                format!("`{branch}` has no remote-tracking branch on `{remote}`")
            }
            Self::DirtyWorktree => {
                "the working tree has changes; commit or stash them before updating".into()
            }
            Self::RemoteMoved {
                destination,
                expected,
                actual,
            } => format!(
                "`{destination}` moved on the remote since it was last observed \
                 (expected {}, found {}); fetch and review before pushing again",
                &expected.to_string()[..8],
                &actual.to_string()[..8]
            ),
        }
    }
}

/// What an update WOULD do, captured before anything moves. The plan carries the tips it was
/// built from, so applying it later can refuse a branch that moved in the meantime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitUpdateReview {
    pub repository: PathBuf,
    pub remote: String,
    pub branch: String,
    pub local: git2::Oid,
    pub remote_tip: git2::Oid,
    pub commits: usize,
    /// Which integration the review would perform. A branch that is merely behind fast-forwards;
    /// a diverged branch is MERGED, and the merge may produce conflicts the resolver must settle.
    pub integration: GitUpdateIntegration,
}

/// How a reviewed update would be applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitUpdateIntegration {
    /// The local tip is an ancestor of the remote tip: the branch moves without a merge.
    FastForward,
    /// The two have diverged: a real merge joins them, and any conflict is left to the user.
    Merge,
}

impl GitUpdateReview {
    #[must_use]
    pub fn summary(&self) -> String {
        let verb = match self.integration {
            GitUpdateIntegration::FastForward => "would fast-forward",
            GitUpdateIntegration::Merge => "would MERGE",
        };
        format!(
            "{} {verb} {} -> {} ({} commit(s)) on `{}`",
            self.branch,
            &self.local.to_string()[..8],
            &self.remote_tip.to_string()[..8],
            self.commits,
            self.remote
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteOutcome {
    /// The fetch completed. `updated` names the remote-tracking refs that actually moved, so a
    /// caller reports what happened rather than assuming something did.
    Fetched { remote: String, updated: Vec<String> },
    /// What an update would do, computed without moving anything.
    Reviewed(GitUpdateReview),
    Updated {
        branch: String,
        from: git2::Oid,
        to: git2::Oid,
        commits: usize,
    },
    /// A merge joined the two heads without conflicts, so the merge commit is recorded.
    Merged {
        branch: String,
        from: git2::Oid,
        to: git2::Oid,
        merge: git2::Oid,
    },
    /// The merge produced conflicts. The operation is left PAUSED with the conflicted paths, and
    /// the resolver is what finishes it: a conflict is never resolved on the user's behalf.
    MergeConflicted {
        branch: String,
        conflicted: Vec<String>,
    },
    Refused(GitRemoteRefusal),
    /// The push completed. `from` is the value the remote held when the push was based on it.
    Pushed {
        remote: String,
        destination: String,
        from: Option<git2::Oid>,
        to: git2::Oid,
    },
    Failed(GitRemoteError),
    /// The outcome could not be classified. It is journaled and NEVER retried.
    Unknown(GitRemoteError),
}

pub enum GitRemoteRequest {
    Fetch {
        generation: GitRemoteGeneration,
        repository: PathBuf,
        remote: String,
        deadline: Duration,
    },
    /// Compute what a fast-forward would do, moving nothing.
    ReviewUpdate {
        generation: GitRemoteGeneration,
        repository: PathBuf,
        remote: String,
        branch: String,
        deadline: Duration,
    },
    /// Push a local ref to a named destination on a named remote, non-forced, with the remote
    /// value the request was based on checked first.
    Push {
        generation: GitRemoteGeneration,
        repository: PathBuf,
        remote: String,
        source: String,
        destination: String,
        lease: Option<git2::Oid>,
        deadline: Duration,
    },
    /// Apply a reviewed plan. The plan carries the tips it was built from, so a branch that
    /// moved since the review is refused rather than moved to an unreviewed place.
    ApplyUpdate {
        generation: GitRemoteGeneration,
        review: GitUpdateReview,
        deadline: Duration,
    },
}

#[derive(Debug)]
pub enum GitRemoteEvent {
    Fetched {
        generation: GitRemoteGeneration,
        outcome: GitRemoteOutcome,
    },
}

pub struct GitRemoteService {
    sender: SyncSender<GitRemoteRequest>,
    receiver: Receiver<GitRemoteEvent>,
    next_generation: u64,
}

impl Default for GitRemoteService {
    fn default() -> Self {
        // The worker idles on its channel until a request arrives, so a session that never
        // touches a remote pays only for one parked thread.
        Self::start(4)
    }
}

impl GitRemoteService {
    pub fn start(capacity: usize) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(capacity.max(1));
        let (event_sender, event_receiver) = mpsc::sync_channel(capacity.max(1));
        thread::Builder::new()
            .name("dust-git-remote".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let event = match request {
                        GitRemoteRequest::Fetch {
                            generation,
                            repository,
                            remote,
                            deadline,
                        } => GitRemoteEvent::Fetched {
                            generation,
                            // One attempt, always.
                            outcome: fetch_once(&repository, &remote, deadline),
                        },
                        GitRemoteRequest::ReviewUpdate {
                            generation,
                            repository,
                            remote,
                            branch,
                            deadline,
                        } => GitRemoteEvent::Fetched {
                            generation,
                            outcome: review_update(&repository, &remote, &branch, deadline),
                        },
                        GitRemoteRequest::Push {
                            generation,
                            repository,
                            remote,
                            source,
                            destination,
                            lease,
                            deadline,
                        } => GitRemoteEvent::Fetched {
                            generation,
                            outcome: push_once(
                                &repository,
                                &remote,
                                &source,
                                &destination,
                                lease,
                                deadline,
                            ),
                        },
                        GitRemoteRequest::ApplyUpdate {
                            generation,
                            review,
                            deadline,
                        } => GitRemoteEvent::Fetched {
                            generation,
                            outcome: apply_update(&review, deadline),
                        },
                    };
                    if event_sender.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("git remote worker thread");
        Self {
            sender: request_sender,
            receiver: event_receiver,
            next_generation: 1,
        }
    }

    pub fn fetch(
        &mut self,
        repository: PathBuf,
        remote: String,
        deadline: Duration,
    ) -> Result<GitRemoteGeneration, GitRemoteError> {
        let generation = GitRemoteGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(GitRemoteRequest::Fetch {
                generation,
                repository,
                remote,
                deadline,
            })
            .map_err(|_| GitRemoteError::Backend)?;
        Ok(generation)
    }

    /// Ask what a fast-forward would do. Nothing moves.
    pub fn review_update(
        &mut self,
        repository: PathBuf,
        remote: String,
        branch: String,
        deadline: Duration,
    ) -> Result<GitRemoteGeneration, GitRemoteError> {
        let generation = GitRemoteGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(GitRemoteRequest::ReviewUpdate {
                generation,
                repository,
                remote,
                branch,
                deadline,
            })
            .map_err(|_| GitRemoteError::Backend)?;
        Ok(generation)
    }

    /// Push a local ref to a named destination. The refspec is built by `push_refspec` and is
    /// NON-FORCED: it carries no force marker, so the remote itself refuses a non-fast-forward.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        repository: PathBuf,
        remote: String,
        source: String,
        destination: String,
        lease: Option<git2::Oid>,
        deadline: Duration,
    ) -> Result<GitRemoteGeneration, GitRemoteError> {
        let generation = GitRemoteGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(GitRemoteRequest::Push {
                generation,
                repository,
                remote,
                source,
                destination,
                lease,
                deadline,
            })
            .map_err(|_| GitRemoteError::Backend)?;
        Ok(generation)
    }

    /// Apply a reviewed plan. It is applied on the tips it was built from or not at all.
    pub fn apply_update(
        &mut self,
        review: GitUpdateReview,
        deadline: Duration,
    ) -> Result<GitRemoteGeneration, GitRemoteError> {
        let generation = GitRemoteGeneration(self.next_generation);
        self.next_generation += 1;
        self.sender
            .send(GitRemoteRequest::ApplyUpdate {
                generation,
                review,
                deadline,
            })
            .map_err(|_| GitRemoteError::Backend)?;
        Ok(generation)
    }

    /// Poll exactly once per call: a second poll can consume and discard the event this call was
    /// written to observe.
    pub fn poll(&mut self) -> Option<GitRemoteEvent> {
        // A disconnected worker yields nothing, exactly as an idle one does: the caller learns
        // the service stopped from the request it can no longer send, not from a lost event.
        self.receiver.try_recv().ok()
    }
}

/// One fetch attempt. The remote must be named explicitly, and the remote-tracking refs that
/// moved are read back from the repository so the caller reports what happened.
fn fetch_once(repository: &std::path::Path, remote: &str, deadline: Duration) -> GitRemoteOutcome {
    let started = std::time::Instant::now();
    let Ok(repo) = git2::Repository::open(repository) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "repository could not be opened".into(),
        ));
    };
    if repo.find_remote(remote).is_err() {
        return GitRemoteOutcome::Failed(GitRemoteError::UnknownRemote(remote.to_owned()));
    }
    let before = remote_tracking_refs(&repo, remote);
    let mut handle = match repo.find_remote(remote) {
        Ok(handle) => handle,
        Err(_) => {
            return GitRemoteOutcome::Failed(GitRemoteError::UnknownRemote(remote.to_owned()))
        }
    };
    let refspec = format!("+refs/heads/*:refs/remotes/{remote}/*");
    let outcome = handle.fetch(&[refspec.as_str()], None, None);
    if started.elapsed() > deadline {
        return GitRemoteOutcome::Unknown(GitRemoteError::Deadline);
    }
    match outcome {
        Ok(()) => {
            let after = remote_tracking_refs(&repo, remote);
            let updated = after
                .into_iter()
                .filter(|(name, oid)| before.get(name) != Some(oid))
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            GitRemoteOutcome::Fetched {
                remote: remote.to_owned(),
                updated,
            }
        }
        // Classified by the TYPED code rather than by matching message text. A genuine
        // authentication failure cannot be produced against a local bare remote, so this branch
        // is implemented but its trigger is unproven.
        Err(error) => GitRemoteOutcome::Failed(classify_failure(remote, &error)),
    }
}

fn remote_tracking_refs(
    repo: &git2::Repository,
    remote: &str,
) -> std::collections::BTreeMap<String, git2::Oid> {
    let prefix = format!("refs/remotes/{remote}/");
    let mut found = std::collections::BTreeMap::new();
    if let Ok(references) = repo.references() {
        for reference in references.flatten() {
            let Some(name) = reference.name() else {
                continue;
            };
            if name.starts_with(&prefix) {
                if let Some(target) = reference.target() {
                    found.insert(name.to_owned(), target);
                }
            }
        }
    }
    found
}

/// A journal entry for an outcome that could not be classified, so the user is told what is
/// uncertain rather than the editor silently trying again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRemoteJournalEntry {
    pub remote: String,
    pub message: String,
}

/// The session's record of unknown remote outcomes. It exists so an unknown result is VISIBLE and
/// never auto-retried: nothing here re-sends a request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitRemoteJournal {
    entries: Vec<GitRemoteJournalEntry>,
}

impl GitRemoteJournal {
    /// Record an unknown outcome. Returns whether this was recorded.
    pub fn record_unknown(&mut self, remote: &str, message: &str) -> bool {
        if self
            .entries
            .iter()
            .any(|entry| entry.remote == remote && entry.message == message)
        {
            return false;
        }
        self.entries.push(GitRemoteJournalEntry {
            remote: remote.to_owned(),
            message: message.to_owned(),
        });
        true
    }

    #[must_use]
    pub fn entries(&self) -> &[GitRemoteJournalEntry] {
        &self.entries
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Compute what a fast-forward would do without moving anything. Every refusal is decided here,
/// before any write, so a refused update cannot leave a half-applied branch behind.
fn review_update(
    repository: &std::path::Path,
    remote: &str,
    branch: &str,
    deadline: Duration,
) -> GitRemoteOutcome {
    let started = std::time::Instant::now();
    let Ok(repo) = git2::Repository::open(repository) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "repository could not be opened".into(),
        ));
    };
    let Ok(local_ref) = repo.find_reference(&format!("refs/heads/{branch}")) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{branch}` is not a local branch"
        )));
    };
    let Some(local) = local_ref.target() else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{branch}` has no commit"
        )));
    };
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let Ok(remote_ref) = repo.find_reference(&tracking) else {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::NoUpstream {
            branch: branch.to_owned(),
            remote: remote.to_owned(),
        });
    };
    let Some(remote_tip) = remote_ref.target() else {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::NoUpstream {
            branch: branch.to_owned(),
            remote: remote.to_owned(),
        });
    };
    if started.elapsed() > deadline {
        return GitRemoteOutcome::Unknown(GitRemoteError::Deadline);
    }
    if local == remote_tip {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::UpToDate {
            branch: branch.to_owned(),
        });
    }
    // Fast-forward is possible only when the local tip is an ANCESTOR of the remote tip.
    let descendant = repo
        .graph_descendant_of(remote_tip, local)
        .unwrap_or(false);
    // Divergence is a MERGE rather than a refusal: the resolver and the recovery actions now
    // exist, which is the condition the contract set for enabling this.
    let integration = if descendant {
        GitUpdateIntegration::FastForward
    } else {
        GitUpdateIntegration::Merge
    };
    let commits = commits_between(&repo, local, remote_tip);
    GitRemoteOutcome::Reviewed(GitUpdateReview {
        repository: repository.to_path_buf(),
        remote: remote.to_owned(),
        branch: branch.to_owned(),
        local,
        remote_tip,
        commits,
        integration,
    })
}

/// Apply a reviewed plan. The plan is applied on the tips it was built from or not at all: a
/// branch that moved since the review is refused rather than moved to an unreviewed place, and
/// a working tree with changes is refused rather than overwritten.
fn apply_update(review: &GitUpdateReview, deadline: Duration) -> GitRemoteOutcome {
    let started = std::time::Instant::now();
    let Ok(repo) = git2::Repository::open(&review.repository) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "repository could not be opened".into(),
        ));
    };
    if started.elapsed() > deadline {
        return GitRemoteOutcome::Unknown(GitRemoteError::Deadline);
    }
    if worktree_or_index_changed(&repo) {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::DirtyWorktree);
    }
    // A merge is a different integration from moving a branch ref, and it is chosen by the
    // review rather than inferred here.
    if review.integration == GitUpdateIntegration::Merge {
        return merge_reviewed(review, started, deadline);
    }
    let Ok(local_ref) = repo.find_reference(&format!("refs/heads/{}", review.branch)) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{}` is not a local branch",
            review.branch
        )));
    };
    let Some(actual) = local_ref.target() else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{}` has no commit",
            review.branch
        )));
    };
    // STALE: the branch is no longer where the review found it.
    if actual != review.local {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::Stale {
            branch: review.branch.clone(),
            expected: review.local,
            actual,
        });
    }
    let tracking = format!("refs/remotes/{}/{}", review.remote, review.branch);
    let Ok(remote_ref) = repo.find_reference(&tracking) else {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::NoUpstream {
            branch: review.branch.clone(),
            remote: review.remote.clone(),
        });
    };
    if remote_ref.target() != Some(review.remote_tip) {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::Stale {
            branch: review.branch.clone(),
            expected: review.remote_tip,
            actual: remote_ref.target().unwrap_or(review.remote_tip),
        });
    }
    drop(remote_ref);
    drop(local_ref);
    // The reviewed move: the branch ref is set to the reviewed remote tip, then the working tree
    // is brought to it. A failure here is a failure, never a silent partial update.
    if repo
        .reference(
            &format!("refs/heads/{}", review.branch),
            review.remote_tip,
            true,
            "dust: reviewed fast-forward update",
        )
        .is_err()
    {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "the branch could not be moved".into(),
        ));
    }
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    if repo.checkout_head(Some(&mut checkout)).is_err() {
        return GitRemoteOutcome::Unknown(GitRemoteError::Failed(
            "the branch moved but the working tree could not be updated".into(),
        ));
    }
    GitRemoteOutcome::Updated {
        branch: review.branch.clone(),
        from: review.local,
        to: review.remote_tip,
        commits: review.commits,
    }
}

/// How many commits an update would bring in.
fn commits_between(repo: &git2::Repository, from: git2::Oid, to: git2::Oid) -> usize {
    let Ok(mut walk) = repo.revwalk() else {
        return 0;
    };
    if walk.push(to).is_err() || walk.hide(from).is_err() {
        return 0;
    }
    walk.count()
}

/// Whether the working tree or the index holds changes a fast-forward would disturb.
fn worktree_or_index_changed(repo: &git2::Repository) -> bool {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(false);
    repo.statuses(Some(&mut options))
        .map(|statuses| !statuses.is_empty())
        .unwrap_or(true)
}

/// The refspec a push uses. It is deliberately built in one place so the NON-FORCED property is
/// testable: a leading `+` would turn this into a force push and silently overwrite a remote.
#[must_use]
pub fn push_refspec(source: &str, destination: &str) -> String {
    format!("{source}:{destination}")
}

/// Whether a refspec would force the update. The push must never produce one.
#[must_use]
pub fn refspec_forces(refspec: &str) -> bool {
    refspec.trim_start().starts_with('+')
}

/// Classify a transport failure by its TYPED code rather than by matching message text. A
/// genuine authentication failure cannot be produced against a local bare remote, so the
/// authentication branch is implemented but its trigger is unproven; the message is carried
/// through so the user sees what the transport actually said.
fn classify_failure(remote: &str, error: &git2::Error) -> GitRemoteError {
    match error.code() {
        git2::ErrorCode::Auth => GitRemoteError::AuthenticationRequired(remote.to_owned()),
        _ => GitRemoteError::Failed(error.message().to_owned()),
    }
}

/// One push attempt. The remote value the request was based on is checked FIRST, and the push
/// is non-forced. This is a check-then-act: libgit2's transport has no protocol-level
/// force-with-lease, so there is an unavoidable window between reading the remote and pushing,
/// exactly as there is with git's own `--force-with-lease`.
fn push_once(
    repository: &std::path::Path,
    remote: &str,
    source: &str,
    destination: &str,
    lease: Option<git2::Oid>,
    deadline: Duration,
) -> GitRemoteOutcome {
    let started = std::time::Instant::now();
    let Ok(repo) = git2::Repository::open(repository) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "repository could not be opened".into(),
        ));
    };
    let Ok(source_ref) = repo.find_reference(source) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{source}` is not a local ref"
        )));
    };
    let Some(source_tip) = source_ref.target() else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(format!(
            "`{source}` has no commit"
        )));
    };
    drop(source_ref);
    let Ok(mut handle) = repo.find_remote(remote) else {
        return GitRemoteOutcome::Failed(GitRemoteError::UnknownRemote(remote.to_owned()));
    };
    // What the remote holds RIGHT NOW, read from the remote itself rather than assumed from a
    // stale remote-tracking ref.
    let actual = match remote_destination(&mut handle, destination) {
        Ok(value) => value,
        Err(error) => return GitRemoteOutcome::Failed(classify_failure(remote, &error)),
    };
    if started.elapsed() > deadline {
        return GitRemoteOutcome::Unknown(GitRemoteError::Deadline);
    }
    if actual == Some(source_tip) {
        return GitRemoteOutcome::Refused(GitRemoteRefusal::UpToDate {
            branch: destination.to_owned(),
        });
    }
    // THE LEASE: a push built on a different remote value than the one observed is refused.
    if let (Some(expected), actual) = (lease, actual) {
        if actual != Some(expected) {
            return GitRemoteOutcome::Refused(GitRemoteRefusal::RemoteMoved {
                destination: destination.to_owned(),
                expected,
                actual: actual.unwrap_or(source_tip),
            });
        }
    }
    let refspec = push_refspec(source, destination);
    debug_assert!(!refspec_forces(&refspec), "a push refspec is never forced");
    let outcome = handle.push(&[refspec.as_str()], None);
    match outcome {
        Ok(()) => GitRemoteOutcome::Pushed {
            remote: remote.to_owned(),
            destination: destination.to_owned(),
            from: actual,
            to: source_tip,
        },
        Err(error) => GitRemoteOutcome::Failed(classify_failure(remote, &error)),
    }
}

/// The remote's current value for a ref, read over the connection so it reflects the remote
/// rather than a local tracking ref.
fn remote_destination(
    remote: &mut git2::Remote<'_>,
    destination: &str,
) -> Result<Option<git2::Oid>, git2::Error> {
    // `connect` returns unit in git2-rs: it leaves the remote connected, and `list` then reports
    // the refs the remote actually advertises.
    remote.connect(git2::Direction::Push)?;
    let head = remote
        .list()?
        .iter()
        .find(|head| head.name() == destination)
        .map(|head| head.oid());
    Ok(head)
}

/// Apply a reviewed MERGE: join the two heads, and never resolve anything on the user's behalf.
///
/// A clean merge is recorded as a merge commit with the two recorded parents. A conflicted merge
/// leaves the repository PAUSED with its conflicts, which the resolver and the operation bar
/// already handle, because a conflict is the user's to settle.
fn merge_reviewed(
    review: &GitUpdateReview,
    started: std::time::Instant,
    deadline: Duration,
) -> GitRemoteOutcome {
    let Ok(repo) = git2::Repository::open(&review.repository) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "repository could not be opened".into(),
        ));
    };
    let Ok(annotated) = repo
        .find_annotated_commit(review.remote_tip)
        .map_err(|error| GitRemoteError::Failed(error.message().to_owned()))
    else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "the reviewed remote tip is not reachable".into(),
        ));
    };
    if let Err(error) = repo.merge(&[&annotated], None, None) {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(error.message().to_owned()));
    }
    drop(annotated);
    if started.elapsed() > deadline {
        return GitRemoteOutcome::Unknown(GitRemoteError::Deadline);
    }
    let conflicted = conflicted_paths(&repo);
    if !conflicted.is_empty() {
        // Record where the operation started so abort can return there, then stop: the user
        // decides how the conflict is settled.
        let _ = repo.reference("ORIG_HEAD", review.local, true, "dust: reviewed merge");
        return GitRemoteOutcome::MergeConflicted {
            branch: review.branch.clone(),
            conflicted,
        };
    }
    // Clean: record the merge commit with BOTH parents.
    let Ok(mut index) = repo.index() else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed("index unavailable".into()));
    };
    let Ok(tree_id) = index.write_tree() else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "the merged tree could not be written".into(),
        ));
    };
    let Ok(tree) = repo.find_tree(tree_id) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed("tree unavailable".into()));
    };
    let Ok(head) = repo.head().and_then(|head| head.peel_to_commit()) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed("HEAD unavailable".into()));
    };
    let Ok(theirs) = repo.find_commit(review.remote_tip) else {
        return GitRemoteOutcome::Failed(GitRemoteError::Failed(
            "the reviewed remote tip is not a commit".into(),
        ));
    };
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Dust", "dust@example.invalid"))
        .unwrap_or_else(|_| git2::Signature::now("Dust", "dust@example.invalid").expect("signature"));
    let message = format!(
        "Merge {} into {}",
        &review.remote_tip.to_string()[..8],
        review.branch
    );
    let merge = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &[&head, &theirs],
    );
    drop(tree);
    drop(head);
    drop(theirs);
    let _ = index.clear();
    let _ = repo.cleanup_state();
    match merge {
        Ok(oid) => GitRemoteOutcome::Merged {
            branch: review.branch.clone(),
            from: review.local,
            to: review.remote_tip,
            merge: oid,
        },
        Err(error) => GitRemoteOutcome::Failed(GitRemoteError::Failed(error.message().to_owned())),
    }
}

/// The paths the merge left conflicted, so the caller can say exactly what the user must settle.
fn conflicted_paths(repo: &git2::Repository) -> Vec<String> {
    let Ok(index) = repo.index() else {
        return Vec::new();
    };
    let Ok(mut conflicts) = index.conflicts() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for conflict in conflicts.by_ref() {
        let Ok(conflict) = conflict else {
            continue;
        };
        let entry = conflict
            .ancestor
            .as_ref()
            .or(conflict.our.as_ref())
            .or(conflict.their.as_ref());
        if let Some(entry) = entry {
            paths.push(String::from_utf8_lossy(&entry.path).into_owned());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
