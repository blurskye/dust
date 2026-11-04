mod branches;
mod browse;
pub use browse::git_browse_url;
mod conflict;
pub use conflict::{
    GitConflictChoice, GitConflictEntry, GitConflictError, GitConflictResolveError,
    GitConflictSide, GitConflictState, GitOperationAction, GitOperationBar, GitRecoveryOutcome,
    GitRecoveryRefusal, GitReplacementError, GitReplacementPlan, abort_operation, apply_replacement, continue_operation,
    plan_replacement, read_conflict_state, read_side_content,
};
mod status;
pub use status::{
    GitChangeSource, GitChangeSources, GitFileStatus, GitRepositoryId, GitStatusEntry,
    GitStatusError, GitStatusGeneration, GitStatusPolicy, GitStatusService, GitStatusSnapshot,
    GitUnsavedDocument, aggregate_git_status, unsaved_document_revision,
    validate_unsaved_documents,
};
mod hunks;
pub use branches::{
    GitBranchEntry, GitBranchError, GitBranchGeneration, GitBranchList, GitBranchListRequest,
    GitBranchRequest, GitBranchResult, GitBranchService, GitWorktreeEntry, WorktreeIdentity,
};
pub use hunks::{
    GitDiffLayer, GitDiffLine, GitDiffLineKind, GitHunk, GitHunkBufferInput, GitHunkError,
    GitHunkGeneration, GitHunkId, GitHunkPolicy, GitHunkService, GitHunkSign, GitHunkSignKind,
    GitHunkSnapshot, git_hunk_signs, git_staged_signs, index_line_in_buffer, read_hunks,
};
mod mutation;
pub use mutation::{
    GitChangePlanRequest, GitHeadExpectation, GitHeadId, GitIndexFingerprint, GitMutationError,
    GitMutationGeneration, GitMutationKind, GitMutationRequest, GitMutationResult,
    GitMutationService, GitResetHunkRequest, GitStageHunkRequest, git_branch_name, git_head_id,
    git_index_fingerprint, worktree_hash,
};

#[cfg(test)]
mod tests;
