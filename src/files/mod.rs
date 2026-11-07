//! Filesystem reads, writes and directory traversal.

#![forbid(unsafe_code)]

mod icons;
pub use icons::ExplorerIcon;
pub use icons::ExplorerIconColor;
pub use icons::directory_icon;
pub use icons::file_icon;
mod recovery;
pub use recovery::DOCUMENT_BYTE_LIMIT;
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
pub(crate) use vfs::FileLock;
pub use vfs::RequestId;
pub use vfs::ResourceIntentKind;
pub(crate) use vfs::VfsReply;
pub use vfs::VfsRequest;
pub use vfs::VfsResult;
pub use vfs::VfsWorker;
pub use vfs::WriteExpectation;
pub use vfs::atomic_write;
pub(crate) use vfs::atomic_write_with_executable;
pub use vfs::canonicalize;
pub use vfs::checked_atomic_write;
#[cfg(unix)]
pub(crate) use vfs::directory_open_flags;
pub(crate) use vfs::is_symlink_error;
pub(crate) use vfs::lock_exclusive;
pub(crate) use vfs::lock_file;
pub(crate) use vfs::open_lock_file;
#[cfg(unix)]
pub(crate) use vfs::open_no_symlinks;
pub(crate) use vfs::open_path;
pub(crate) use vfs::open_read_no_symlinks;
pub(crate) use vfs::open_regular_file;
pub use vfs::read_file;
pub(crate) use vfs::read_file_bounded;
pub(crate) use vfs::sync_directory;
mod project_root;
pub use project_root::PROJECT_ROOT_ANCESTOR_LIMIT;
pub use project_root::PROJECT_ROOT_MARKER_LIMIT;
pub use project_root::ProjectRootResolution;
pub use project_root::ProjectRootStrategy;
pub use project_root::ProjectRootWorker;
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
pub use watch::ExplorerWatchId;
pub use watch::ExplorerWatchPolicy;
pub use watch::ExplorerWatchWorker;
pub use watch::diff_explorer_fingerprints;
pub use watch::explorer_directory_fingerprint;
mod workspace_search;
pub use workspace_search::WorkspaceSearchError;
pub use workspace_search::WorkspaceSearchEvent;
pub use workspace_search::WorkspaceSearchMatch;
pub use workspace_search::WorkspaceSearchPolicy;
pub use workspace_search::WorkspaceSearchSummary;
pub use workspace_search::WorkspaceSearchWorker;
mod error;
pub use error::VfsError;
pub(crate) mod workspace_access;
mod workspace_files;
pub use workspace_files::WorkspaceFileEntry;
pub use workspace_files::WorkspaceFileError;
pub use workspace_files::WorkspaceFileEvent;
pub use workspace_files::WorkspaceFilePolicy;
pub use workspace_files::WorkspaceFileSummary;
pub use workspace_files::WorkspaceFileWorker;

#[cfg(test)]
mod tests;

pub(crate) use vfs::participating_roots;

pub(crate) use vfs::roots_current;

mod root_identity;
pub use root_identity::RootIdentity;
#[cfg(windows)]
pub(crate) use root_identity::file_identity;
pub(crate) mod path_encoding;

/// A Unix-style absolute test path such as `/workspace/a.rs`, made absolute on Windows too.
#[cfg(test)]
pub(crate) fn absolute_test_path(path: &str) -> std::path::PathBuf {
    if cfg!(windows) {
        std::path::PathBuf::from(format!(r"C:{}", path.replace('/', r"\")))
    } else {
        std::path::PathBuf::from(path)
    }
}
