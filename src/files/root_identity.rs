//! Canonical paths and filesystem identities for workspace roots.

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
        let canonical_path = fs::canonicalize(path).map_err(|source| VfsError::RootIo {
            operation: "canonicalize workspace root",
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&canonical_path).map_err(|source| VfsError::RootIo {
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

#[cfg(test)]
mod tests {
    use super::RootIdentity;
    use crate::files::VfsError;

    #[test]
    fn missing_root_reports_the_operation_and_original_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing");
        let error = RootIdentity::capture(&path).unwrap_err();
        let VfsError::RootIo {
            operation,
            path: failed_path,
            source,
        } = &error
        else {
            panic!("unexpected root error: {error}");
        };
        assert_eq!(*operation, "canonicalize workspace root");
        assert_eq!(failed_path, &path);
        assert_eq!(
            error.to_string(),
            format!(
                "cannot canonicalize workspace root `{}`: {source}",
                path.display()
            )
        );
    }

    #[test]
    fn a_file_is_reported_as_a_non_directory_root() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        std::fs::write(&path, "text").unwrap();
        let error = RootIdentity::capture(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "workspace root is not a directory: {}",
                path.canonicalize().unwrap().display()
            )
        );
    }
}
