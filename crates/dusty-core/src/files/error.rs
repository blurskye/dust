//! Filesystem operation errors.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("cannot {operation} `{}`: {source}", path.display())]
    RootIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workspace root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("failed to {operation} `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("refusing to overwrite externally modified file `{}`; reload or force the write", .0.display())]
    ExternalModification(PathBuf),
    #[error(
        "`{}` already exists in `{}`",
        .0.file_name().unwrap_or_default().to_string_lossy(),
        .0.parent().unwrap_or(std::path::Path::new("")).display()
    )]
    DestinationExists(PathBuf),
    #[error("path is outside the permitted filesystem scope: `{}`", .0.display())]
    OutsideScope(PathBuf),
    #[error("invalid recovery journal `{}`: {reason}", path.display())]
    Recovery { path: PathBuf, reason: String },
    #[error("refusing to read `{}` because it exceeds the {maximum_bytes} byte safety limit", path.display())]
    DocumentTooLarge { path: PathBuf, maximum_bytes: u64 },
    #[error("resource source is not a regular bounded file: `{}`",.0.display())]
    NotRegularFile(PathBuf),
    #[error("resource changed after preview")]
    ChangedOnDisk,
    #[error("cross-filesystem rename is not supported atomically")]
    CrossFilesystem,
    #[error("directory entry limit is outside safe bounds")]
    DirectoryLimit,
    #[error("project-root markers must be bounded single path segments")]
    ProjectRootMarker,
    #[error("failed to walk directory `{}`: {reason}", path.display())]
    DirectoryWalk { path: PathBuf, reason: String },
    #[error("filesystem worker queue is full")]
    QueueFull,
    #[error("filesystem worker stopped")]
    WorkerStopped,
}
