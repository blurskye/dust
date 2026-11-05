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

    /// The files named at startup that do not exist yet.
    #[must_use]
    pub fn new_files(&self) -> Vec<PathBuf> {
        match self {
            Self::Dashboard { .. } => Vec::new(),
            Self::File {
                path,
                additional_files,
                ..
            } => std::iter::once(path)
                .chain(additional_files)
                .filter(|path| !path.exists())
                .cloned()
                .collect(),
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Dashboard { root } => format!("dashboard\t{}", root.to_string_lossy().escape_debug()),
            Self::File {
                workspace_root,
                path,
                additional_files,
            } => format!(
                "file\t{}\t{}\t{}",
                workspace_root.to_string_lossy().escape_debug(),
                path.to_string_lossy().escape_debug(),
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

    let first = canonical_or_new(&paths[0], current_directory)?;
    if first.is_dir() {
        if paths.len() != 1 {
            return Err(StartupError::DirectoryWithAdditionalPaths);
        }
        return Ok(StartupTarget::Dashboard { root: first });
    }
    if first.exists() && !first.is_file() {
        return Err(StartupError::UnsupportedPath(first));
    }

    let additional_files = paths[1..]
        .iter()
        .map(|path| {
            let path = canonical_or_new(path, current_directory)?;
            if !path.exists() || path.is_file() {
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

/// Where a new file at `path` lives: its folder resolved when that exists, as an open resolves
/// an existing file.
pub(crate) fn new_file_path(path: &Path) -> PathBuf {
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// An existing path resolved, or a new file in a folder that exists, which the first save creates.
fn canonical_or_new(path: &Path, current_directory: &Path) -> Result<PathBuf, StartupError> {
    let absolute = current_directory.join(path);
    if !absolute.exists() && absolute.parent().is_some_and(Path::is_dir) {
        return Ok(new_file_path(&absolute));
    }
    canonical_existing(&absolute)
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
    fn startup_description_escapes_path_controls() {
        let root = std::path::PathBuf::from("/workspace\x1b]0;title\x07");
        let dashboard = StartupTarget::Dashboard { root: root.clone() };
        let file = StartupTarget::File {
            workspace_root: root,
            path: "name\n\t.rs".into(),
            additional_files: Vec::new(),
        };
        assert_eq!(dashboard.describe(), "dashboard\t/workspace\\u{1b}]0;title\\u{7}");
        assert_eq!(file.describe(), "file\t/workspace\\u{1b}]0;title\\u{7}\tname\\n\\t.rs\t0");
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
    fn relative_existing_paths_use_the_supplied_directory() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.path().join("other.rs"), "// other\n").unwrap();
        let target = resolve_startup(
            &["main.rs".into(), "other.rs".into()],
            root.path(),
        ).unwrap();
        let canonical = root.path().canonicalize().unwrap();
        assert_eq!(target, StartupTarget::File {
            workspace_root: root.path().to_path_buf(),
            path: canonical.join("main.rs"),
            additional_files: vec![canonical.join("other.rs")],
        });
        fs::create_dir(root.path().join("nested")).unwrap();
        assert_eq!(
            resolve_startup(&["nested".into()], root.path()).unwrap(),
            StartupTarget::Dashboard { root: canonical.join("nested") },
        );
    }

    #[test]
    fn a_new_file_in_an_existing_folder_is_a_file_target() {
        let root = tempdir().expect("tempdir");
        let target =
            resolve_startup(&[std::path::PathBuf::from("new.txt")], root.path()).expect("startup");
        assert_eq!(
            target.new_files(),
            vec![root.path().canonicalize().unwrap().join("new.txt")]
        );
        assert!(
            resolve_startup(&[std::path::PathBuf::from("missing/new.txt")], root.path()).is_err(),
            "a folder that does not exist is still an error"
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
    fn a_path_in_a_missing_folder_fails_before_application_start() {
        let root = tempdir().expect("tempdir");
        assert!(resolve_startup(&[root.path().join("missing/file")], root.path()).is_err());
    }
}
