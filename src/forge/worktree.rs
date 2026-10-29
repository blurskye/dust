//! Checking a review out into a worktree (W-23).
//!
//! The contract: "Checking out a review into a worktree records the inspected revision, leaves the
//! current branch untouched, and returns to the review afterwards."
//!
//! The revision is taken FROM the room's pin rather than from a caller, so the worktree and the
//! inspection cannot disagree about what was looked at. The worktree itself is created by the
//! workspace's own worktree owner - this module builds requests for it rather than writing a second
//! source writer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::review::ReviewRoom;
use crate::workspace::git::GitBranchRequest;

/// A review checked out: the worktree, and the revision it holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCheckout {
    pub name: String,
    pub path: PathBuf,
    /// The revision the ROOM pinned, which is what the worktree is detached at.
    pub revision: String,
}

impl ReviewCheckout {
    #[must_use]
    pub fn new(name: impl Into<String>, path: PathBuf, revision: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path,
            revision: revision.into(),
        }
    }
}

/// The request that checks the review's OWN pinned revision into a worktree. The revision comes
/// from the room, so "records the inspected revision" cannot be got wrong by a caller.
#[must_use]
pub fn checkout_request(
    room: &ReviewRoom,
    repository: &Path,
    name: &str,
    path: &Path,
    deadline: Duration,
) -> GitBranchRequest {
    GitBranchRequest::CreateDetachedWorktree {
        repository: repository.to_path_buf(),
        name: name.to_owned(),
        path: path.to_path_buf(),
        revision: room.pinned().as_str().to_owned(),
        deadline,
    }
}

/// The request that closes a review checkout. The review itself is untouched, so the caller returns
/// to the same review afterwards.
#[must_use]
pub fn leave_request(repository: &Path, name: &str, deadline: Duration) -> GitBranchRequest {
    GitBranchRequest::RemoveWorktree {
        repository: repository.to_path_buf(),
        name: name.to_owned(),
        deadline,
    }
}
