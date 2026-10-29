//! Cherry-pick and revert, with their truthful recovery limits.
//!
//! The authority matrix in `docs/progress/git-operation-authority.md` records both as
//! native-via-git2 with "conflicts pause; abort restores". This module implements exactly that
//! and states the limits rather than smoothing them over:
//!
//! - a conflicting pick or revert leaves the operation PAUSED, which the resolver and the
//!   operation bar already handle;
//! - reverting a MERGE commit needs a chosen parent, so it is REFUSED with that reason rather
//!   than applied against an arbitrary parent;
//! - recovery is the existing abort, which returns the branch to the revision git recorded.
use std::path::Path;

use thiserror::Error;

/// Which operation is being applied, so a refusal and a paused state both name it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitPickKind {
    CherryPick,
    Revert,
}

impl GitPickKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitPickRefusal {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("`{0}` is not a commit in this repository")]
    UnknownCommit(String),
    #[error(
        "{operation} of the merge commit {commit} needs a chosen parent: reverting a merge \
         requires naming which side to keep, and this editor will not pick one for you"
    )]
    MergeCommitNeedsParent {
        operation: &'static str,
        commit: String,
    },
    #[error("the working tree has changes; commit or stash them first")]
    DirtyWorktree,
    #[error(
        "{operation} was previewed against {expected} and HEAD is now {actual}; review it again"
    )]
    StalePreview {
        operation: &'static str,
        expected: String,
        actual: String,
    },
    #[error("the {0} failed: {1}")]
    Failed(&'static str, String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitPickOutcome {
    /// Applied cleanly and recorded as a new commit.
    Applied {
        operation: GitPickKind,
        commit: git2::Oid,
    },
    /// LEFT PAUSED: the paths the user must settle, which the resolver and the bar handle.
    Conflicted {
        operation: GitPickKind,
        conflicted: Vec<String>,
    },
    Refused(GitPickRefusal),
}

/// Resolve the commit a command names, by OID or by ref, so a caller can pass what the history
/// surface actually holds.
pub fn resolve_commit(repository: &Path, spec: &str) -> Result<git2::Oid, GitPickRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitPickRefusal::NotRepository)?;
    if let Ok(oid) = git2::Oid::from_str(spec.trim()) {
        if repo.find_commit(oid).is_ok() {
            return Ok(oid);
        }
    }
    repo.revparse_single(spec.trim())
        .ok()
        .and_then(|object| object.peel_to_commit().ok())
        .map(|commit| commit.id())
        .ok_or_else(|| GitPickRefusal::UnknownCommit(spec.to_owned()))
}

/// Apply a cherry-pick or a revert. Every refusal is decided BEFORE the first write.
pub fn apply_pick(
    repository: &Path,
    kind: GitPickKind,
    spec: &str,
) -> Result<GitPickOutcome, GitPickRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitPickRefusal::NotRepository)?;
    let oid = resolve_commit(repository, spec)?;
    let commit = repo
        .find_commit(oid)
        .map_err(|_| GitPickRefusal::UnknownCommit(spec.to_owned()))?;
    // Reverting a MERGE has no single meaning, and picking one parent on the user's behalf would
    // record a change they never asked for.
    if kind == GitPickKind::Revert && commit.parent_count() > 1 {
        return Err(GitPickRefusal::MergeCommitNeedsParent {
            operation: kind.name(),
            commit: oid.to_string(),
        });
    }
    if worktree_changed(&repo) {
        return Err(GitPickRefusal::DirtyWorktree);
    }
    // Record where the operation started, so the existing abort can return there.
    let started = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .ok_or_else(|| GitPickRefusal::Failed(kind.name(), "HEAD has no commit".into()))?;
    let outcome = match kind {
        GitPickKind::CherryPick => repo.cherrypick(&commit, None),
        GitPickKind::Revert => repo.revert(&commit, None),
    };
    if let Err(error) = outcome {
        return Err(GitPickRefusal::Failed(
            kind.name(),
            error.message().to_owned(),
        ));
    }
    drop(commit);
    let conflicted = conflicted_paths(&repo);
    if !conflicted.is_empty() {
        // The state git left is the paused operation the resolver already settles.
        let _ = repo.reference("ORIG_HEAD", started, true, "dust: pick started");
        return Ok(GitPickOutcome::Conflicted {
            operation: kind,
            conflicted,
        });
    }
    // Clean: record the commit with the current HEAD as its parent.
    let mut index = repo
        .index()
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    let tree_id = index
        .write_tree()
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    let head = repo
        .find_commit(started)
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Dust", "dust@example.invalid"))
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    let message = format!("{} {}", kind.name(), &oid.to_string()[..8]);
    let recorded = repo
        .commit(Some("HEAD"), &signature, &signature, &message, &tree, &[&head])
        .map_err(|error| GitPickRefusal::Failed(kind.name(), error.message().to_owned()))?;
    drop(tree);
    drop(head);
    let _ = index.clear();
    let _ = repo.cleanup_state();
    // The prior HEAD is retained by the reflog, which is the truthful recovery limit: the commit
    // is reachable again, but nothing here rewrites it back automatically.
    Ok(GitPickOutcome::Applied {
        operation: kind,
        commit: recorded,
    })
}

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

fn worktree_changed(repo: &git2::Repository) -> bool {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(false);
    repo.statuses(Some(&mut options))
        .map(|statuses| !statuses.is_empty())
        .unwrap_or(true)
}

/// How much of the repository a reset rewrites. Each mode states its own recovery limit because
/// they do not destroy the same things.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitResetMode {
    /// Move HEAD only. The index and the working tree are untouched, so nothing is lost.
    Soft,
    /// Move HEAD and reset the index. The working tree keeps its bytes, so nothing is lost
    /// either — the changes return to being unstaged.
    Mixed,
    /// Move HEAD, reset the index and DISCARD working-tree changes. This is the destructive one.
    Hard,
}

impl GitResetMode {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "soft" => Some(Self::Soft),
            "mixed" => Some(Self::Mixed),
            "hard" => Some(Self::Hard),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Mixed => "mixed",
            Self::Hard => "hard",
        }
    }

    /// The truthful recovery limit, stated for the mode rather than for reset in general.
    #[must_use]
    pub fn recovery_limit(self) -> &'static str {
        match self {
            Self::Soft => {
                "HEAD moves; the index and working tree keep their exact bytes, so nothing is lost"
            }
            Self::Mixed => {
                "HEAD and the index move; working-tree bytes are untouched and the changes simply \
                 become unstaged"
            }
            Self::Hard => {
                "working-tree changes ARE DISCARDED and are not recoverable from the reflog, which \
                 records commits rather than uncommitted work"
            }
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitResetRefusal {
    #[error("repository could not be opened")]
    NotRepository,
    #[error("`{0}` is not a commit in this repository")]
    UnknownTarget(String),
    #[error(
        "a hard reset would DISCARD uncommitted changes in {0}; commit or stash them first, \
         because this never throws work away"
    )]
    WouldDiscard(String),
    #[error("the reset failed: {0}")]
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitResetOutcome {
    pub mode: GitResetMode,
    pub from: git2::Oid,
    pub to: git2::Oid,
}

/// Reset HEAD to a named revision.
///
/// The DESTRUCTIVE mode is gated exactly as abort is: when the working tree holds changes the
/// reset would discard, it is REFUSED and names them rather than proceeding. The reflog records
/// commits, not uncommitted work, so discarding it would be genuinely unrecoverable.
pub fn reset_to(
    repository: &Path,
    mode: GitResetMode,
    spec: &str,
) -> Result<GitResetOutcome, GitResetRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitResetRefusal::NotRepository)?;
    let target = resolve_commit(repository, spec)
        .map_err(|_| GitResetRefusal::UnknownTarget(spec.to_owned()))?;
    let from = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .ok_or_else(|| GitResetRefusal::Failed("HEAD has no commit".into()))?;
    if mode == GitResetMode::Hard {
        let risky = uncommitted_paths(&repo);
        if !risky.is_empty() {
            return Err(GitResetRefusal::WouldDiscard(risky.join(", ")));
        }
    }
    let object = repo
        .find_object(target, None)
        .map_err(|error| GitResetRefusal::Failed(error.message().to_owned()))?;
    let reset_kind = match mode {
        GitResetMode::Soft => git2::ResetType::Soft,
        GitResetMode::Mixed => git2::ResetType::Mixed,
        GitResetMode::Hard => git2::ResetType::Hard,
    };
    repo.reset(&object, reset_kind, None)
        .map_err(|error| GitResetRefusal::Failed(error.message().to_owned()))?;
    Ok(GitResetOutcome {
        mode,
        from,
        to: target,
    })
}

/// The paths a hard reset would discard: modified, staged or untracked entries in the worktree or
/// index. Untracked files count, because a hard reset removes them too.
fn uncommitted_paths(repo: &git2::Repository) -> Vec<String> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let Ok(statuses) = repo.statuses(Some(&mut options)) else {
        return vec!["(working tree state could not be read)".to_owned()];
    };
    let mut paths = statuses
        .iter()
        .filter_map(|entry| entry.path().map(str::to_owned))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

/// A capability this editor does NOT perform natively, with the truthful reason and the command
/// a user can run themselves.
///
/// The authority matrix records two DIFFERENT reasons and they are kept apart here: bisect and
/// LFS have no libgit2 API at all, while submodules have a partial API with no complete
/// add/update/deinit flow. All three additionally lack the owner's approval for an exec lane, so
/// none of them is executed, tracked or reported as native completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHandoff {
    Bisect,
    Lfs,
    Submodules,
}

impl GitHandoff {
    /// Every operation this editor hands off rather than performs.
    pub const ALL: [Self; 3] = [Self::Bisect, Self::Lfs, Self::Submodules];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bisect => "bisect",
            Self::Lfs => "lfs",
            Self::Submodules => "submodules",
        }
    }

    /// Why the editor does not do this natively. The two reasons are different facts.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Bisect => {
                "libgit2 exposes no bisect API, so there is nothing to call"
            }
            Self::Lfs => {
                "libgit2 has no LFS filter API, so there is nothing to call"
            }
            Self::Submodules => {
                "libgit2 has a submodule API but no complete add, update and deinit flow, so a \
                 native path would be a partial one presented as whole"
            }
        }
    }

    /// The exact command a user runs themselves.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Bisect => "git bisect start",
            Self::Lfs => "git lfs pull",
            Self::Submodules => "git submodule update --init --recursive",
        }
    }

    /// The disposition shown to the user. It states the limits rather than implying support.
    #[must_use]
    pub fn disposition(self) -> String {
        format!(
            "`{name}` is NOT performed by this editor: {reason}. Run `{command}` in a terminal \
             yourself. This editor does not execute it, does not track its state, and does not \
             report its outcome as native completion.",
            name = self.name(),
            reason = self.reason(),
            command = self.command(),
        )
    }
}

/// What a pick or revert WOULD do, captured before anything moves.
///
/// The preview carries the HEAD it was computed against, and the apply is checked against THAT
/// value rather than re-derived: a preview is only a guarantee if the value it named is the value
/// the write is checked against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitPickPreview {
    pub kind: GitPickKind,
    pub target: git2::Oid,
    pub subject: String,
    /// The paths the operation would change, read from the commit itself.
    pub changes: Vec<String>,
    /// The HEAD the preview was computed against.
    pub head: git2::Oid,
}

impl GitPickPreview {
    /// What a confirmation must show.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} {} `{}` onto {} — changes: {}",
            self.kind.name(),
            &self.target.to_string()[..8],
            self.subject,
            &self.head.to_string()[..8],
            if self.changes.is_empty() {
                "(no paths)".to_owned()
            } else {
                self.changes.join(", ")
            }
        )
    }
}

/// Compute what a pick or revert would do, moving nothing.
pub fn plan_pick(
    repository: &Path,
    kind: GitPickKind,
    spec: &str,
) -> Result<GitPickPreview, GitPickRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitPickRefusal::NotRepository)?;
    let target = resolve_commit(repository, spec)?;
    let commit = repo
        .find_commit(target)
        .map_err(|_| GitPickRefusal::UnknownCommit(spec.to_owned()))?;
    // The same limit the apply enforces, decided here so the preview never offers something the
    // apply would refuse.
    if kind == GitPickKind::Revert && commit.parent_count() > 1 {
        return Err(GitPickRefusal::MergeCommitNeedsParent {
            operation: kind.name(),
            commit: target.to_string(),
        });
    }
    let subject = commit.summary().unwrap_or_default().to_owned();
    let changes = commit_changes(&repo, &commit);
    let head = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .ok_or_else(|| GitPickRefusal::Failed(kind.name(), "HEAD has no commit".into()))?;
    drop(commit);
    Ok(GitPickPreview {
        kind,
        target,
        subject,
        changes,
        head,
    })
}

/// Apply a preview. It is applied against the HEAD the preview NAMED, or refused.
pub fn apply_reviewed_pick(
    repository: &Path,
    preview: &GitPickPreview,
) -> Result<GitPickOutcome, GitPickRefusal> {
    let repo = git2::Repository::open(repository).map_err(|_| GitPickRefusal::NotRepository)?;
    let current = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .ok_or_else(|| GitPickRefusal::Failed(preview.kind.name(), "HEAD has no commit".into()))?;
    if current != preview.head {
        return Err(GitPickRefusal::StalePreview {
            operation: preview.kind.name(),
            expected: preview.head.to_string(),
            actual: current.to_string(),
        });
    }
    if repo.find_commit(preview.target).is_err() {
        return Err(GitPickRefusal::UnknownCommit(preview.target.to_string()));
    }
    drop(repo);
    apply_pick(repository, preview.kind, &preview.target.to_string())
}

/// The paths a commit changes against its first parent.
fn commit_changes(repo: &git2::Repository, commit: &git2::Commit<'_>) -> Vec<String> {
    let Ok(tree) = commit.tree() else {
        return Vec::new();
    };
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for delta in diff.deltas() {
        for file in [delta.new_file(), delta.old_file()] {
            if let Some(path) = file.path() {
                paths.push(path.display().to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
