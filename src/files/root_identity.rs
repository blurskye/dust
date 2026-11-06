use crate::files::VfsError;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootIdentity {
    #[serde(with = "crate::files::path_encoding")]
    pub canonical_path: PathBuf,
    pub filesystem_identity: Option<(u64, u64)>,
}

impl RootIdentity {
    pub fn capture(path: &Path) -> Result<Self, VfsError> {
        let canonical_path = fs::canonicalize(path).map_err(|source| VfsError::Io {
            operation: "canonicalize workspace root",
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&canonical_path).map_err(|source| VfsError::Io {
            operation: "inspect workspace root",
            path: canonical_path.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(VfsError::RootNotDirectory(canonical_path));
        }
        #[cfg(unix)]
        let filesystem_identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let filesystem_identity = None;
        Ok(Self {
            canonical_path,
            filesystem_identity,
        })
    }
}
