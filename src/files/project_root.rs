//! Project-root discovery from files and configured markers.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::thread;

use serde::Deserialize;

use serde::Serialize;

use crate::files::error::VfsError;

use crate::files::vfs::canonical_path;

use crate::files::vfs::read_file_bounded;

pub const PROJECT_ROOT_MARKER_LIMIT: usize = 16;
pub const PROJECT_ROOT_ANCESTOR_LIMIT: usize = 64;
pub(super) const PROJECT_ROOT_MANIFEST_BYTE_LIMIT: u64 = 512 * 1024;

/// Adapter-defined project evidence. The filesystem worker never branches on a
/// document language; it validates the policy selected by the tool catalog.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRootStrategy {
    /// Any non-symlink marker is sufficient.
    #[default]
    Marker,
    /// A syntactically valid Cargo package or workspace manifest.
    Cargo,
    /// A declared Go module or workspace.
    Go,
    /// A syntactically valid TypeScript config or package manifest.
    Node,
    /// A syntactically valid pyproject, or one of Python's conventional build files.
    Python,
}

/// A bounded, workspace-scoped project-root decision for a saved document.
///
/// This is deliberately generic: tool definitions supply evidence markers and
/// a validator strategy, while the filesystem worker owns canonicalization and
/// ancestor inspection. `project_root = None` means no launchable project was
/// proven; it is never replaced with an arbitrary containing directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRootResolution {
    pub request_id: u64,
    pub root_revision: u64,
    pub document: PathBuf,
    pub workspace_root: PathBuf,
    pub project_root: Option<PathBuf>,
}

#[derive(Default)]
pub struct ProjectRootWorker {
    request_id: u64,
    receiver: Option<Receiver<Result<ProjectRootResolution, VfsError>>>,
}

impl ProjectRootWorker {
    pub fn resolve(
        &mut self,
        document: PathBuf,
        workspace_root: PathBuf,
        root_revision: u64,
        markers: Vec<String>,
        strategy: ProjectRootStrategy,
    ) -> Result<u64, VfsError> {
        if markers.len() > PROJECT_ROOT_MARKER_LIMIT
            || markers
                .iter()
                .any(|marker| !valid_project_root_marker(marker))
        {
            return Err(VfsError::ProjectRootMarker);
        }
        self.request_id = self.request_id.checked_add(1).ok_or_else(|| VfsError::Io {
            operation: "allocate project-root request_id",
            path: document.clone(),
            source: io::Error::other("project-root request_id exhausted"),
        })?;
        let request_id = self.request_id;
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_document = document.clone();
        thread::Builder::new()
            .name(format!("dust-project-root-{request_id}"))
            .spawn(move || {
                let result = resolve_project_root(
                    request_id,
                    root_revision,
                    &worker_document,
                    &workspace_root,
                    &markers,
                    strategy,
                );
                let _ = sender.send(result);
            })
            .map_err(|source| VfsError::Io {
                operation: "start project-root worker for",
                path: document,
                source,
            })?;
        self.receiver = Some(receiver);
        Ok(request_id)
    }

    /// Consuming terminal poll; unlike the compatibility query, disconnect is an outcome.
    pub fn poll_result(&mut self) -> Option<Result<ProjectRootResolution, VfsError>> {
        let receiver = self.receiver.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(VfsError::WorkerStopped),
        };
        self.receiver = None;
        Some(result)
    }
    pub fn try_result(&self) -> Option<Result<ProjectRootResolution, VfsError>> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }
}

pub(super) fn valid_project_root_marker(marker: &str) -> bool {
    !marker.is_empty()
        && marker.len() <= 255
        && marker != "."
        && marker != ".."
        && !marker.contains('/')
        && !marker.contains('\\')
        && !marker.contains('\0')
}

/// A document's real path. One not saved yet resolves through the nearest folder that exists.
fn canonical_document(document: &Path) -> Result<PathBuf, VfsError> {
    let existing = document
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or(document);
    let rest = document.strip_prefix(existing).unwrap_or(Path::new(""));
    let canonical = canonical_path(existing)?;
    Ok(if rest.as_os_str().is_empty() {
        canonical
    } else {
        canonical.join(rest)
    })
}

pub(super) fn resolve_project_root(
    request_id: u64,
    root_revision: u64,
    document: &Path,
    workspace_root: &Path,
    markers: &[String],
    strategy: ProjectRootStrategy,
) -> Result<ProjectRootResolution, VfsError> {
    let document = canonical_document(document)?;
    let workspace_root = canonical_path(workspace_root)?;
    if !document.starts_with(&workspace_root) {
        return Err(VfsError::OutsideScope(document));
    }
    let mut directory = document
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| VfsError::OutsideScope(document.clone()))?;
    let mut project_root = None;
    for _ in 0..PROJECT_ROOT_ANCESTOR_LIMIT {
        if !directory.starts_with(&workspace_root) {
            break;
        }
        if project_root_evidence(&directory, markers, strategy) {
            // A malformed nearest project boundary must not silently fall back
            // to an outer project and attach the file to the wrong server.
            if project_root_is_valid(&directory, markers, strategy) {
                project_root = Some(directory.clone());
            }
            break;
        }
        if directory == workspace_root {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent.to_path_buf();
    }
    Ok(ProjectRootResolution {
        request_id,
        root_revision,
        document,
        workspace_root,
        project_root,
    })
}

pub(super) fn regular_project_marker(directory: &Path, marker: &str) -> Option<fs::Metadata> {
    if marker.contains(['*', '?', '[']) {
        let pattern = globset::Glob::new(marker).ok()?.compile_matcher();
        return fs::read_dir(directory)
            .ok()?
            .take(4096)
            .filter_map(Result::ok)
            .filter(|entry| pattern.is_match(entry.file_name()))
            .filter_map(|entry| fs::symlink_metadata(entry.path()).ok())
            .find(|metadata| !metadata.file_type().is_symlink());
    }
    let metadata = fs::symlink_metadata(directory.join(marker)).ok()?;
    (!metadata.file_type().is_symlink()).then_some(metadata)
}

pub(super) fn bounded_project_marker(directory: &Path, marker: &str) -> Option<String> {
    let bytes =
        read_file_bounded(&directory.join(marker), PROJECT_ROOT_MANIFEST_BYTE_LIMIT).ok()?;
    String::from_utf8(bytes).ok()
}

pub(super) fn project_root_evidence(
    directory: &Path,
    markers: &[String],
    strategy: ProjectRootStrategy,
) -> bool {
    let supported = |marker: &str| match strategy {
        ProjectRootStrategy::Marker => true,
        ProjectRootStrategy::Cargo => marker == "Cargo.toml",
        ProjectRootStrategy::Go => matches!(marker, "go.mod" | "go.work"),
        ProjectRootStrategy::Node => {
            matches!(marker, "tsconfig.json" | "jsconfig.json" | "package.json")
        }
        ProjectRootStrategy::Python => {
            matches!(
                marker,
                "pyproject.toml" | "setup.py" | "setup.cfg" | "requirements.txt"
            )
        }
    };
    markers
        .iter()
        .filter(|marker| supported(marker))
        .any(|marker| regular_project_marker(directory, marker).is_some())
}

pub(super) fn project_root_is_valid(
    directory: &Path,
    markers: &[String],
    strategy: ProjectRootStrategy,
) -> bool {
    match strategy {
        ProjectRootStrategy::Marker => markers
            .iter()
            .any(|marker| regular_project_marker(directory, marker).is_some()),
        ProjectRootStrategy::Cargo => {
            markers.iter().any(|marker| marker == "Cargo.toml")
                && bounded_project_marker(directory, "Cargo.toml").is_some_and(|source| {
                    toml::from_str::<toml::Value>(&source).is_ok_and(|value| {
                        value.get("package").is_some() || value.get("workspace").is_some()
                    })
                })
        }
        ProjectRootStrategy::Go => {
            (markers.iter().any(|marker| marker == "go.mod")
                && bounded_project_marker(directory, "go.mod").is_some_and(|source| {
                    source.lines().any(|line| {
                        let mut words = line.split_ascii_whitespace();
                        words.next() == Some("module")
                            && words.next().is_some_and(|word| !word.starts_with("//"))
                    })
                }))
                || (markers.iter().any(|marker| marker == "go.work")
                    && bounded_project_marker(directory, "go.work").is_some_and(|source| {
                        source.lines().any(|line| {
                            let mut words = line.split_ascii_whitespace();
                            words.next() == Some("go")
                                && words.next().is_some_and(|word| !word.starts_with("//"))
                        })
                    }))
        }
        ProjectRootStrategy::Node => markers
            .iter()
            .filter(|marker| {
                matches!(
                    marker.as_str(),
                    "tsconfig.json" | "jsconfig.json" | "package.json"
                )
            })
            .any(|marker| {
                bounded_project_marker(directory, marker).is_some_and(|source| {
                    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
                    let source = if marker == "package.json" {
                        source.to_owned()
                    } else {
                        jsonc_source(source)
                    };
                    serde_json::from_str::<serde_json::Value>(&source)
                        .is_ok_and(|value| value.is_object())
                })
            }),
        ProjectRootStrategy::Python => {
            (markers.iter().any(|marker| marker == "pyproject.toml")
                && bounded_project_marker(directory, "pyproject.toml")
                    .is_some_and(|source| toml::from_str::<toml::Value>(&source).is_ok()))
                || markers
                    .iter()
                    .filter(|marker| {
                        matches!(
                            marker.as_str(),
                            "setup.py" | "setup.cfg" | "requirements.txt"
                        )
                    })
                    .any(|marker| regular_project_marker(directory, marker).is_some())
        }
    }
}

// Preserve string contents and offsets while removing JSONC comments and trailing commas.
fn jsonc_source(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => index += 2,
            b'"' => {
                quoted = !quoted;
                index += 1;
            }
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                    bytes[index] = b' ';
                    index += 1;
                }
            }
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'*') => {
                let start = index;
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                if index + 1 >= bytes.len() {
                    return source.to_owned();
                }
                index += 2;
                bytes[start..index].fill(b' ');
            }
            _ => index += 1,
        }
    }
    index = 0;
    quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => index += 2,
            b'"' => {
                quoted = !quoted;
                index += 1;
            }
            b',' if !quoted => {
                if bytes[..index]
                    .iter()
                    .rfind(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|byte| !matches!(byte, b'{' | b'[' | b',' | b':'))
                    && bytes[index + 1..]
                        .iter()
                        .find(|byte| !byte.is_ascii_whitespace())
                        .is_some_and(|byte| matches!(byte, b'}' | b']'))
                {
                    bytes[index] = b' ';
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    // Every modified byte is ASCII, and every byte of a comment was replaced.
    String::from_utf8(bytes).expect("JSONC comment removal preserves UTF-8")
}
