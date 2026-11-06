//! Filesystem reads, writes and directory traversal.

#![forbid(unsafe_code)]

mod icons;
pub use icons::ExplorerIcon;
pub use icons::ExplorerIconColor;
pub use icons::directory_icon;
pub use icons::file_icon;
mod recovery;
pub use recovery::DOCUMENT_BYTE_CEILING;
pub use recovery::RECOVERY_VERSION;
pub use recovery::RecoveryCleanup;
pub use recovery::RecoveryDiscovery;
pub use recovery::RecoveryRecord;
pub use recovery::cleanup_recovery;
pub use recovery::discover_recovery;
pub use recovery::recovery_size_allowed;
pub use recovery::write_recovery;
mod explorer_intent;
pub(crate) use explorer_intent::ExplorerIoReply;
pub(crate) use explorer_intent::ExplorerRenameSource;
pub(crate) use explorer_intent::PreparedExplorerMutation;
mod vfs;
pub use vfs::CheckedResourceIntent;
pub use vfs::ExplorerMutation;
pub use vfs::RequestId;
pub use vfs::ResourceIntentKind;
pub(crate) use vfs::VfsReply;
pub use vfs::VfsRequest;
pub use vfs::VfsResult;
pub use vfs::VfsWorker;
pub use vfs::WriteExpectation;
pub use vfs::atomic_write;
pub(crate) use vfs::atomic_write_with_executable;
pub use vfs::checked_atomic_write;
pub(crate) use vfs::lock_file;
pub(crate) use vfs::open_no_symlinks;
pub(crate) use vfs::open_regular_file;
pub use vfs::read_file;
pub(crate) use vfs::read_file_with_ceiling;
mod project_root;
pub use project_root::PROJECT_ROOT_ANCESTOR_CEILING;
pub use project_root::PROJECT_ROOT_MARKER_CEILING;
pub use project_root::ProjectRootResolution;
pub use project_root::ProjectRootWorker;
pub use project_root::ProjectRootStrategy;
mod directory;
pub use directory::DirectoryEntry;
pub use directory::DirectoryEntryKind;
pub use directory::DirectoryListing;
pub use directory::DirectoryWorker;
pub use directory::linked_worktree_repository;
mod watch;
pub use watch::ExplorerDirectoryFingerprint;
pub use watch::ExplorerWatchBatch;
pub use watch::ExplorerWatchEntryFingerprint;
pub use watch::ExplorerWatchEntryKind;
pub use watch::ExplorerWatchError;
pub use watch::ExplorerWatchEvent;
pub use watch::ExplorerWatchGeneration;
pub use watch::ExplorerWatchPolicy;
pub use watch::ExplorerWatchWorker;
pub use watch::diff_explorer_fingerprints;
pub use watch::explorer_directory_fingerprint;
mod workspace_search;
pub use workspace_search::WorkspaceSearchError;
pub use workspace_search::WorkspaceSearchEvent;
pub use workspace_search::WorkspaceSearchMatch;
pub use workspace_search::WorkspaceSearchPolicy;
pub use workspace_search::WorkspaceSearchWorker;
pub use workspace_search::WorkspaceSearchSummary;
mod error;
pub use error::VfsError;
mod workspace_files;
pub use workspace_files::WorkspaceFileEntry;
pub use workspace_files::WorkspaceFileError;
pub use workspace_files::WorkspaceFileEvent;
pub use workspace_files::WorkspaceFilePolicy;
pub use workspace_files::WorkspaceFileWorker;
pub use workspace_files::WorkspaceFileSummary;

#[cfg(test)]
mod tests;

pub(crate) use vfs::participating_roots;

pub(crate) use vfs::roots_current;

mod root_identity;
pub use root_identity::RootIdentity;
pub(crate) mod path_encoding;
