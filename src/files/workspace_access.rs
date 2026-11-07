//! Workspace-scoped filesystem operations using the VFS's checked opens and mutations.

use super::vfs;
use super::{RootIdentity, VfsError};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum FileOperation {
    Read(PathBuf),
    Write(PathBuf, Vec<u8>),
    CreateDirectory(PathBuf),
    ListDirectory(PathBuf),
    Stat(PathBuf),
    Rename(PathBuf, PathBuf),
    Delete(PathBuf),
}

#[derive(Debug)]
pub struct FileInfo {
    pub name: String,
    pub directory: bool,
    pub file: bool,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum FileValue {
    Done,
    Bytes(Vec<u8>),
    Entries(Vec<FileInfo>),
    Info(FileInfo),
}

impl FileOperation {
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Rename(source, target) => vec![source.clone(), target.clone()],
            Self::Read(path)
            | Self::Write(path, _)
            | Self::CreateDirectory(path)
            | Self::ListDirectory(path)
            | Self::Stat(path)
            | Self::Delete(path) => vec![path.clone()],
        }
    }

    pub(crate) fn resolve(&mut self, paths: &[PathBuf]) {
        match self {
            Self::Rename(source, target) => {
                *source = paths[0].clone();
                *target = paths[1].clone();
            }
            Self::Read(path)
            | Self::Write(path, _)
            | Self::CreateDirectory(path)
            | Self::ListDirectory(path)
            | Self::Stat(path)
            | Self::Delete(path) => *path = paths[0].clone(),
        }
    }

    pub(crate) fn changes(&self, path: &Path) -> bool {
        match self {
            Self::Write(target, _) => path == target,
            Self::Rename(source, target) => path.starts_with(source) || path.starts_with(target),
            Self::Delete(target) => path.starts_with(target),
            _ => false,
        }
    }
}

fn info(path: &Path, metadata: std::fs::Metadata) -> Result<FileInfo, VfsError> {
    Ok(FileInfo {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_str()
            .ok_or_else(|| VfsError::OutsideScope(path.into()))?
            .into(),
        directory: metadata.is_dir(),
        file: metadata.is_file(),
        bytes: metadata.len(),
    })
}

pub(crate) fn execute(
    root: &RootIdentity,
    operation: FileOperation,
) -> Result<FileValue, VfsError> {
    vfs::roots_current(std::slice::from_ref(root))?;
    let roots = vec![root.canonical_path.clone()];
    for path in operation.paths() {
        if !path.starts_with(&root.canonical_path)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(VfsError::OutsideScope(path));
        }
    }
    let io = |path: &Path, source| VfsError::Io {
        operation: "access workspace file",
        path: path.into(),
        source,
    };
    match operation {
        FileOperation::Read(path) => {
            vfs::read_scoped_file(&path, &roots, 1024 * 1024).map(FileValue::Bytes)
        }
        FileOperation::Write(path, bytes) => {
            vfs::agent_atomic_write(&root.canonical_path, &path, &bytes)?;
            Ok(FileValue::Done)
        }
        FileOperation::Stat(path) => {
            let file = vfs::open_scoped(&path, &roots, rustix::fs::OFlags::PATH)?;
            Ok(FileValue::Info(info(
                &path,
                file.metadata().map_err(|e| io(&path, e))?,
            )?))
        }
        FileOperation::ListDirectory(path) => {
            let directory = vfs::open_scoped(
                &path,
                &roots,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )?;
            let mut entries = Vec::new();
            let iter = rustix::fs::Dir::read_from(&directory).map_err(|e| io(&path, e.into()))?;
            for entry in iter {
                let entry = entry.map_err(|e| io(&path, e.into()))?;
                let name = entry
                    .file_name()
                    .to_str()
                    .map_err(|e| io(&path, std::io::Error::other(e)))?;
                if matches!(name, "." | "..") {
                    continue;
                }
                if entries.len() >= 4096 {
                    return Err(io(
                        &path,
                        std::io::Error::other("directory entry limit exceeded"),
                    ));
                }
                let child = path.join(name);
                // Symlinks are reported as other entries, never followed by a listing.
                let metadata = rustix::fs::statat(
                    &directory,
                    entry.file_name(),
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|e| io(&child, e.into()))?;
                let kind = rustix::fs::FileType::from_raw_mode(metadata.st_mode);
                entries.push(FileInfo {
                    name: name.into(),
                    directory: kind == rustix::fs::FileType::Directory,
                    file: kind == rustix::fs::FileType::RegularFile,
                    bytes: metadata.st_size as u64,
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(FileValue::Entries(entries))
        }
        operation => {
            let mutation = match operation {
                FileOperation::CreateDirectory(path) => vfs::ExplorerMutation::Create {
                    path,
                    directory: true,
                },
                FileOperation::Rename(source, target) => {
                    vfs::ExplorerMutation::Rename { source, target }
                }
                FileOperation::Delete(path) => vfs::ExplorerMutation::Delete { path },
                _ => unreachable!(),
            };
            vfs::apply_explorer_mutation(&mutation, &roots, 1024 * 1024)?;
            Ok(FileValue::Done)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_operations_reject_parent_paths_and_symlink_escapes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(root.path()).unwrap();
        std::fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        for path in [
            root.path().join("link/secret"),
            root.path().join("../secret"),
        ] {
            assert!(execute(&identity, FileOperation::Read(path.clone())).is_err());
            assert!(execute(&identity, FileOperation::Write(path, b"changed".to_vec())).is_err());
        }
        assert_eq!(
            std::fs::read(outside.path().join("secret")).unwrap(),
            b"secret"
        );
    }
}
