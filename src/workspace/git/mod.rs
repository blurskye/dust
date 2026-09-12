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
mod status;
pub use status::{
    GitFileStatus, GitRepositoryId, GitStatusEntry, GitStatusError, GitStatusGeneration,
    GitStatusPolicy, GitStatusService, GitStatusSnapshot, aggregate_git_status,
};
mod hunks;
pub use hunks::{
    GitDiffLayer, GitDiffLine, GitDiffLineKind, GitHunk, GitHunkBufferInput, GitHunkError,
    GitHunkGeneration, GitHunkId, GitHunkPolicy, GitHunkService, GitHunkSign, GitHunkSignKind,
    GitHunkSnapshot, git_hunk_signs, read_hunks,
};
use hunks::{read_hunks_owned, validate_hunk_policy};
mod mutation;
use mutation::worktree_hash;
pub use mutation::{
    GitIndexFingerprint, GitMutationError, GitMutationGeneration, GitMutationKind,
    GitMutationRequest, GitMutationResult, GitMutationService, GitResetHunkRequest,
    GitStageHunkRequest, git_index_fingerprint,
};

#[cfg(test)]
mod tests;
