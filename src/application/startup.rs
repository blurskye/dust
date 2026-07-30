use std::{
    fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupTarget {
    Dashboard {
        root: PathBuf,
    },
    File {
        workspace_root: PathBuf,
        path: PathBuf,
        additional_files: Vec<PathBuf>,
    },
}

impl StartupTarget {
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        match self {
            Self::Dashboard { root } => root,
            Self::File { workspace_root, .. } => workspace_root,
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Dashboard { root } => format!("dashboard\t{}", root.display()),
            Self::File {
                workspace_root,
                path,
                additional_files,
            } => format!(
                "file\t{}\t{}\t{}",
                workspace_root.display(),
                path.display(),
                additional_files.len()
            ),
        }
    }
}

pub fn resolve_startup(
    paths: &[PathBuf],
    current_directory: &Path,
) -> Result<StartupTarget, StartupError> {
    validate_current_directory(current_directory)?;
    if paths.is_empty() {
        return Ok(StartupTarget::Dashboard {
            root: current_directory.to_path_buf(),
        });
    }

    let first = canonical_existing(&paths[0])?;
    if first.is_dir() {
        if paths.len() != 1 {
            return Err(StartupError::DirectoryWithAdditionalPaths);
        }
        return Ok(StartupTarget::Dashboard { root: first });
    }
    if !first.is_file() {
        return Err(StartupError::UnsupportedPath(first));
    }

    let additional_files = paths[1..]
        .iter()
        .map(|path| {
            let path = canonical_existing(path)?;
            if path.is_file() {
                Ok(path)
            } else {
                Err(StartupError::UnsupportedPath(path))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StartupTarget::File {
        workspace_root: current_directory.to_path_buf(),
        path: first,
        additional_files,
    })
}

fn validate_current_directory(path: &Path) -> Result<(), StartupError> {
    let metadata = fs::metadata(path).map_err(|source| StartupError::CurrentDirectory {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(StartupError::CurrentDirectoryNotDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn canonical_existing(path: &Path) -> Result<PathBuf, StartupError> {
    path.canonicalize().map_err(|source| StartupError::Path {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("cannot access current directory `{path}`: {source}")]
    CurrentDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("current directory `{0}` is not a directory")]
    CurrentDirectoryNotDirectory(PathBuf),
    #[error("cannot access startup path `{path}`: {source}")]
    Path {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("a directory startup target cannot be combined with additional paths")]
    DirectoryWithAdditionalPaths,
    #[error("startup path `{0}` is neither a regular file nor a directory")]
    UnsupportedPath(PathBuf),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{StartupTarget, resolve_startup};

    #[test]
    fn no_argument_preserves_exact_cwd_without_parent_discovery() {
        let root = tempdir().expect("tempdir");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested");
        assert_eq!(
            resolve_startup(&[], &nested).expect("startup"),
            StartupTarget::Dashboard { root: nested }
        );
    }

    #[test]
    fn directory_is_canonicalized_to_dashboard() {
        let root = tempdir().expect("tempdir");
        let target = resolve_startup(&[root.path().join(".")], root.path()).expect("startup");
        assert_eq!(
            target,
            StartupTarget::Dashboard {
                root: root.path().canonicalize().expect("canonical")
            }
        );
    }

    #[test]
    fn file_uses_current_directory_as_workspace() {
        let root = tempdir().expect("tempdir");
        let file = root.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        let target = resolve_startup(std::slice::from_ref(&file), root.path()).expect("startup");
        assert_eq!(
            target,
            StartupTarget::File {
                workspace_root: root.path().to_path_buf(),
                path: file.canonicalize().expect("canonical"),
                additional_files: Vec::new(),
            }
        );
    }

    #[test]
    fn missing_path_fails_before_application_start() {
        let root = tempdir().expect("tempdir");
        assert!(resolve_startup(&[root.path().join("missing")], root.path()).is_err());
    }
}
