#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::fs::File;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
mod icons;
pub use icons::{ExplorerIcon, ExplorerIconColor, directory_icon, file_icon};
mod recovery;
pub use recovery::{
    DOCUMENT_BYTE_CEILING, RECOVERY_VERSION, RecoveryCleanup, RecoveryDiscovery, RecoveryRecord,
    cleanup_recovery, discover_recovery, write_recovery,
};
use recovery::{read_bounded_bytes, read_bounded_utf8};
mod explorer_intent;
pub(crate) use explorer_intent::{
    ExplorerIo, ExplorerIoReply, ExplorerRenameSource, PreparedExplorerMutation,
};
mod vfs;
pub(crate) use vfs::VfsReply;
use vfs::canonical_path;
#[cfg(not(unix))]
use vfs::sync_parent;
pub use vfs::{
    CheckedResourceIntent, ExplorerMutation, RequestId, ResourceIntentKind, VfsRequest, VfsResult,
    VfsWorker, WriteExpectation, atomic_write, read_file,
};
mod project_root;
pub use project_root::{
    PROJECT_ROOT_ANCESTOR_CEILING, PROJECT_ROOT_MARKER_CEILING, ProjectRootResolution,
    ProjectRootService, ProjectRootStrategy,
};
mod directory;
pub use directory::{DirectoryEntry, DirectoryEntryKind, DirectoryListing, DirectoryService};
mod watch;
#[cfg(not(unix))]
use watch::watch_name_bytes;
pub use watch::{
    ExplorerDirectoryFingerprint, ExplorerWatchBatch, ExplorerWatchEntryFingerprint,
    ExplorerWatchEntryKind, ExplorerWatchError, ExplorerWatchEvent, ExplorerWatchGeneration,
    ExplorerWatchPolicy, ExplorerWatchService, diff_explorer_fingerprints,
    explorer_directory_fingerprint,
};
mod workspace_search;
use workspace_search::{ABSOLUTE_WORKSPACE_SEARCH_FILES, owner_root};
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
