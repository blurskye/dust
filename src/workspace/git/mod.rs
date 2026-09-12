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
mod mutation;
pub use mutation::{
    GitIndexFingerprint, GitMutationError, GitMutationGeneration, GitMutationKind,
    GitMutationRequest, GitMutationResult, GitMutationService, GitResetHunkRequest,
    GitStageHunkRequest, git_index_fingerprint,
};

#[cfg(test)]
mod tests;
