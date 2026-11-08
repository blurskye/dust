//! Directory listing requests and filtered entries.

use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;

use ignore::WalkBuilder;

use crate::files::error::VfsError;

use crate::files::vfs::canonical_path;

pub(super) const WORKTREE_DIRECTORY: &str = ".worktree";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryEntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryListing {
    pub request_id: u64,
    pub root_revision: u64,
    pub directory: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

type DirectoryResult = Result<(DirectoryListing, Vec<crate::files::RootIdentity>), VfsError>;

#[derive(Default)]
pub struct DirectoryWorker {
    request_id: u64,
    receiver: Option<Receiver<DirectoryResult>>,
}

impl DirectoryWorker {
    pub fn list(
        &mut self,
        directory: PathBuf,
        canonical_roots: Vec<PathBuf>,
        root_revision: u64,
        include_hidden: bool,
        include_ignored: bool,
        maximum_entries: usize,
    ) -> Result<u64, VfsError> {
        if !(1..=100_000).contains(&maximum_entries) {
            return Err(VfsError::DirectoryLimit);
        }
        self.request_id = self.request_id.checked_add(1).ok_or_else(|| VfsError::Io {
            operation: "allocate directory request identity",
            path: directory.clone(),
            source: std::io::Error::other("directory request identity exhausted"),
        })?;
        let request_id = self.request_id;
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_directory = directory.clone();
        thread::Builder::new()
            .name(format!("dust-directory-{request_id}"))
            .spawn(move || {
                let result = (|| {
                    let canonical = canonical_path(&worker_directory)?;
                    let allowed = canonical_roots
                        .iter()
                        .map(|path| crate::files::RootIdentity {
                            canonical_path: path.clone(),
                            filesystem_identity: None,
                        })
                        .collect::<Vec<_>>();
                    let relevant = crate::files::vfs::participating_roots(
                        &[&worker_directory],
                        &[&canonical],
                        &allowed,
                    )?;
                    let capture =
                        || {
                            relevant
                                .iter()
                                .map(|root| &root.canonical_path)
                                .map(|path| {
                                    let observed = crate::files::RootIdentity::capture(path)
                                        .map_err(|error| VfsError::Io {
                                            operation: "inspect directory scope",
                                            path: path.clone(),
                                            source: std::io::Error::other(error.to_string()),
                                        })?;
                                    if &observed.canonical_path != path {
                                        return Err(VfsError::OutsideScope(path.clone()));
                                    }
                                    Ok(observed)
                                })
                                .collect::<Result<Vec<_>, _>>()
                        };
                    let roots = capture()?;
                    let listing = list_scoped_directory(
                        request_id,
                        root_revision,
                        &worker_directory,
                        &canonical_roots,
                        include_hidden,
                        include_ignored,
                        maximum_entries,
                    )?;
                    if capture()? != roots {
                        return Err(VfsError::Io {
                            operation: "check directory scope",
                            path: worker_directory.clone(),
                            source: std::io::Error::other(
                                "workspace filesystem identity changed during listing",
                            ),
                        });
                    }
                    Ok((listing, roots))
                })();
                let _ = sender.send(result);
            })
            .map_err(|source| VfsError::Io {
                operation: "start directory worker for",
                path: directory,
                source,
            })?;
        self.receiver = Some(receiver);
        Ok(request_id)
    }

    pub(crate) fn poll_result(
        &mut self,
    ) -> Option<Result<(DirectoryListing, Vec<crate::files::RootIdentity>), VfsError>> {
        match self.receiver.as_ref()?.try_recv() {
            Ok(result) => {
                self.receiver = None;
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.receiver = None;
                Some(Err(VfsError::Io {
                    operation: "receive directory result",
                    path: PathBuf::new(),
                    source: std::io::Error::other("directory worker stopped"),
                }))
            }
        }
    }
    pub fn try_result(&self) -> Option<Result<DirectoryListing, VfsError>> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
            .map(|result| result.map(|(listing, _)| listing))
    }
}

pub(super) fn natural_name_key(value: &str) -> String {
    let lowercase = value.to_lowercase();
    let mut characters = lowercase.chars().peekable();
    let mut key = String::with_capacity(lowercase.len());
    while let Some(character) = characters.next() {
        if character.is_ascii_digit() {
            let mut digits = String::from(character);
            while characters
                .peek()
                .is_some_and(|digit| digit.is_ascii_digit())
            {
                if let Some(digit) = characters.next() {
                    digits.push(digit);
                }
            }
            let significant = digits.trim_start_matches('0');
            let significant = if significant.is_empty() {
                "0"
            } else {
                significant
            };
            key.push('\u{1}');
            key.push_str(&format!("{:09}", significant.len()));
            key.push_str(significant);
            key.push('\u{1}');
            key.push_str(&format!("{:09}", digits.len()));
        } else {
            key.push(character);
        }
    }
    key
}

/// Git's linked worktrees for a repository root, kept in `.worktree/`. Git ignores that folder,
/// but from the repository's root the tree still shows what's in it.
pub(super) fn root_worktrees(
    root: &Path,
    mut keep_scanning: impl FnMut() -> bool,
) -> impl Iterator<Item = PathBuf> {
    // Do not follow either the special directory or its children through symlinks.
    let canonical_root = crate::files::canonicalize(root).ok();
    let directory = root.join(WORKTREE_DIRECTORY);
    let entries = canonical_root.as_ref().and_then(|_| {
        std::fs::symlink_metadata(&directory)
            .ok()
            .filter(|metadata| metadata.is_dir())
            .and_then(|_| std::fs::read_dir(&directory).ok())
    });
    entries
        .into_iter()
        .flatten()
        .take_while(move |_| keep_scanning())
        .filter_map(move |entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            let canonical = crate::files::canonicalize(&path).ok()?;
            if !canonical.starts_with(canonical_root.as_ref()?) || !path.join(".git").is_file() {
                return None;
            }
            Some(path)
        })
}

/// The repository a folder is a linked worktree of: Git leaves a `.git` file there pointing at
/// `<repository>/.git/worktrees/<name>`.
#[must_use]
pub fn linked_worktree_repository(root: &Path) -> Option<String> {
    let bytes = crate::files::vfs::read_file_bounded(&root.join(".git"), 16 * 1024).ok()?;
    let link = std::str::from_utf8(&bytes).ok()?;
    let git_directory = Path::new(link.strip_prefix("gitdir:")?.trim());
    let git_directory = std::path::absolute(root.join(git_directory)).ok()?;
    let mut normalized = PathBuf::new();
    for component in git_directory.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            component => normalized.push(component.as_os_str()),
        }
    }
    let repository = normalized.ancestors().nth(3)?;
    Some(repository.file_name()?.to_string_lossy().into_owned())
}

pub(super) fn list_scoped_directory(
    request_id: u64,
    root_revision: u64,
    directory: &Path,
    roots: &[PathBuf],
    include_hidden: bool,
    include_ignored: bool,
    maximum_entries: usize,
) -> Result<DirectoryListing, VfsError> {
    let directory = canonical_path(directory)?;
    if !roots.iter().any(|root| directory.starts_with(root)) {
        return Err(VfsError::OutsideScope(directory));
    }
    let mut builder = WalkBuilder::new(&directory);
    builder
        .max_depth(Some(1))
        .follow_links(false)
        .hidden(!include_hidden)
        .ignore(!include_ignored)
        .git_ignore(!include_ignored)
        .git_global(!include_ignored)
        .git_exclude(!include_ignored)
        .parents(!include_ignored);
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in builder.build().skip(1) {
        let entry = entry.map_err(|error| VfsError::DirectoryWalk {
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        if entries.len() >= maximum_entries {
            truncated = true;
            break;
        }
        let kind = match entry.file_type() {
            Some(kind) if kind.is_dir() => DirectoryEntryKind::Directory,
            Some(kind) if kind.is_file() => DirectoryEntryKind::File,
            _ => DirectoryEntryKind::Symlink,
        };
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.into_path(),
            kind,
        });
    }
    let worktrees = directory.join(WORKTREE_DIRECTORY);
    if !truncated
        && roots.contains(&directory)
        && !entries.iter().any(|entry| entry.path == worktrees)
    {
        let mut scanned = 0;
        let found = root_worktrees(&directory, || {
            if scanned >= maximum_entries {
                truncated = true;
                return false;
            }
            scanned += 1;
            true
        })
        .next()
        .is_some();
        if found {
            if entries.len() >= maximum_entries {
                truncated = true;
            } else {
                entries.push(DirectoryEntry {
                    name: WORKTREE_DIRECTORY.to_owned(),
                    path: worktrees,
                    kind: DirectoryEntryKind::Directory,
                });
            }
        }
    }
    let mut keyed_entries = entries
        .into_iter()
        .map(|entry| (natural_name_key(&entry.name), entry))
        .collect::<Vec<_>>();
    keyed_entries.sort_by(|(left_key, left), (right_key, right)| {
        let left_rank = usize::from(left.kind != DirectoryEntryKind::Directory);
        let right_rank = usize::from(right.kind != DirectoryEntryKind::Directory);
        left_rank
            .cmp(&right_rank)
            .then_with(|| left_key.cmp(right_key))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
    });
    let entries = keyed_entries.into_iter().map(|(_, entry)| entry).collect();
    Ok(DirectoryListing {
        request_id,
        root_revision,
        directory,
        entries,
        truncated,
    })
}
