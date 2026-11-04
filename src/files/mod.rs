#![forbid(unsafe_code)]

mod icons;
pub use icons::{ExplorerIcon, ExplorerIconColor, directory_icon, file_icon};
mod recovery;
pub use recovery::{
    DOCUMENT_BYTE_CEILING, RECOVERY_VERSION, RecoveryCleanup, RecoveryDiscovery, RecoveryRecord,
    cleanup_recovery, discover_recovery, recovery_size_allowed, write_recovery,
};
mod explorer_intent;
pub(crate) use explorer_intent::{ExplorerIoReply, ExplorerRenameSource, PreparedExplorerMutation};
mod vfs;
pub use vfs::{
    CheckedResourceIntent, ExplorerMutation, RequestId, ResourceIntentKind, VfsRequest, VfsResult,
    VfsWorker, WriteExpectation, atomic_write, checked_atomic_write, read_file,
};
pub(crate) use vfs::{VfsReply, lock_file};
mod project_root;
pub use project_root::{
    PROJECT_ROOT_ANCESTOR_CEILING, PROJECT_ROOT_MARKER_CEILING, ProjectRootResolution,
    ProjectRootService, ProjectRootStrategy,
};
mod directory;
pub use directory::{
    DirectoryEntry, DirectoryEntryKind, DirectoryListing, DirectoryService,
    linked_worktree_repository,
};
mod watch;
pub use watch::{
    ExplorerDirectoryFingerprint, ExplorerWatchBatch, ExplorerWatchEntryFingerprint,
    ExplorerWatchEntryKind, ExplorerWatchError, ExplorerWatchEvent, ExplorerWatchGeneration,
    ExplorerWatchPolicy, ExplorerWatchService, diff_explorer_fingerprints,
    explorer_directory_fingerprint,
};
mod workspace_search;
pub use workspace_search::{
    WorkspaceSearchError, WorkspaceSearchEvent, WorkspaceSearchMatch, WorkspaceSearchPolicy,
    WorkspaceSearchService, WorkspaceSearchSummary,
};
mod error;
pub use error::VfsError;
mod workspace_files;
pub use workspace_files::{
    WorkspaceFileEntry, WorkspaceFileError, WorkspaceFileEvent, WorkspaceFilePolicy,
    WorkspaceFileService, WorkspaceFileSummary,
};

#[cfg(test)]
mod tests;

pub(crate) use vfs::{participating_roots, roots_current};
