//! Recovery journals and bounded recovery reads.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use serde::Deserialize;

use serde::Serialize;

use crate::files::error::VfsError;

use crate::files::vfs::atomic_write;

use crate::files::vfs::lock_file;

pub const RECOVERY_VERSION: u32 = 2;
pub const DOCUMENT_BYTE_LIMIT: u64 = 256 * 1024 * 1024;
pub(super) const RECOVERY_JOURNAL_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) const RECOVERY_DIRECTORY_ENTRY_LIMIT: usize = 10_000;
pub(super) const RECOVERY_TOML_OVERHEAD_ALLOWANCE: u64 = 1_024;
pub(super) const RECOVERY_TOML_MAX_ESCAPE_EXPANSION: u64 = 6;
pub(super) const RECOVERY_PAYLOAD_BYTE_LIMIT: u64 = (RECOVERY_JOURNAL_BYTE_LIMIT
    - RECOVERY_TOML_OVERHEAD_ALLOWANCE)
    / RECOVERY_TOML_MAX_ESCAPE_EXPANSION;

pub(super) const fn recovery_journal_size_allowed(bytes: u64) -> bool {
    bytes <= RECOVERY_JOURNAL_BYTE_LIMIT
}

/// Whether a file this size fits in a recovery journal, leaving room for its path.
pub const fn recovery_size_allowed(content_bytes: u64) -> bool {
    recovery_payload_size_allowed(content_bytes, 2 * 4096)
}

pub(super) const fn recovery_payload_size_allowed(content_bytes: u64, path_bytes: u64) -> bool {
    content_bytes.saturating_add(path_bytes) <= RECOVERY_PAYLOAD_BYTE_LIMIT
}

pub(super) fn read_bounded_bytes(
    reader: impl Read,
    maximum_bytes: u64,
) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= maximum_bytes).then_some(bytes))
}

pub(super) fn read_recovery_source(path: &Path) -> Result<String, String> {
    let bytes = crate::files::vfs::read_file_bounded(path, RECOVERY_JOURNAL_BYTE_LIMIT)
        .map_err(|error| format!("cannot read recovery journal: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("recovery journal is not UTF-8: {error}"))
}

pub(super) fn bounded_recovery_entries<T>(entries: impl IntoIterator<Item = T>) -> (Vec<T>, bool) {
    let mut paths = entries
        .into_iter()
        .take(RECOVERY_DIRECTORY_ENTRY_LIMIT.saturating_add(1))
        .collect::<Vec<_>>();
    let truncated = paths.len() > RECOVERY_DIRECTORY_ENTRY_LIMIT;
    paths.truncate(RECOVERY_DIRECTORY_ENTRY_LIMIT);
    (paths, truncated)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryRecord {
    pub version: u32,
    pub original_path: Option<String>,
    pub revision: u64,
    pub content: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryDiscovery {
    pub records: Vec<(PathBuf, RecoveryRecord)>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryCleanup {
    pub deleted: Vec<PathBuf>,
    pub diagnostics: Vec<String>,
}

impl RecoveryRecord {
    #[must_use]
    pub fn new(original_path: Option<&Path>, revision: u64, content: String) -> Self {
        Self {
            version: RECOVERY_VERSION,
            original_path: original_path.map(|path| {
                path.as_os_str()
                    .as_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            }),
            revision,
            content,
        }
    }

    /// Version 1 used UTF-8 display paths; version 2 stores native Unix bytes as hex.
    pub fn path(&self) -> Option<PathBuf> {
        let path = self.original_path.as_ref()?;
        match self.version {
            // Old display paths with replacement characters are ambiguous. Recover
            // their text into a scratch buffer instead of risking a different file.
            1 if !path.contains('\u{fffd}') => Some(PathBuf::from(path)),
            RECOVERY_VERSION if path.len() % 2 == 0 => {
                let bytes = path
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| {
                        let high = (pair[0] as char).to_digit(16)?;
                        let low = (pair[1] as char).to_digit(16)?;
                        Some((high * 16 + low) as u8)
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(PathBuf::from(OsString::from_vec(bytes)))
            }
            _ => None,
        }
    }
}

pub fn write_recovery(path: &Path, record: &RecoveryRecord) -> Result<(), VfsError> {
    let path_bytes = record.original_path.as_ref().map_or(0, String::len) as u64;
    if !recovery_payload_size_allowed(record.content.len() as u64, path_bytes) {
        return Err(VfsError::Recovery {
            path: path.to_path_buf(),
            reason: format!(
                "payload exceeds {} byte pre-serialization recovery safety limit",
                RECOVERY_PAYLOAD_BYTE_LIMIT
            ),
        });
    }
    let source = toml::to_string(record).map_err(|source| VfsError::Recovery {
        path: path.to_path_buf(),
        reason: source.to_string(),
    })?;
    if !recovery_journal_size_allowed(source.len() as u64) {
        return Err(VfsError::Recovery {
            path: path.to_path_buf(),
            reason: format!(
                "serialized journal exceeds {} byte recovery safety limit",
                RECOVERY_JOURNAL_BYTE_LIMIT
            ),
        });
    }
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let _lock = lock_file(&directory.join(".recovery.lock")).map_err(|source| VfsError::Io {
        operation: "lock recovery directory",
        path: directory.to_path_buf(),
        source,
    })?;
    atomic_write(path, source.as_bytes())
}

#[must_use]
pub fn discover_recovery(directory: &Path) -> RecoveryDiscovery {
    discover_recovery_bounded(directory, 100, RECOVERY_JOURNAL_BYTE_LIMIT)
}

pub(super) fn discover_recovery_bounded(
    directory: &Path,
    maximum_records: usize,
    maximum_bytes: u64,
) -> RecoveryDiscovery {
    let mut records = Vec::new();
    let mut bytes = 0;
    if maximum_records == 0 {
        return RecoveryDiscovery::default();
    }
    let mut limited = false;
    let mut diagnostics = visit_recovery(directory, |path, record, _| {
        bytes += record.content.len() as u64
            + record.original_path.as_ref().map_or(0, String::len) as u64;
        if bytes > maximum_bytes {
            limited = true;
            return false;
        }
        records.push((path, record));
        records.len() < maximum_records
    });
    if limited {
        diagnostics.push(format!(
            "recovery discovery stopped at {maximum_bytes} byte safety limit"
        ));
    }
    RecoveryDiscovery {
        records,
        diagnostics,
    }
}

fn visit_recovery(
    directory: &Path,
    mut visit: impl FnMut(PathBuf, RecoveryRecord, Option<SystemTime>) -> bool,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return diagnostics,
        Err(error) => {
            diagnostics.push(format!(
                "cannot inspect recovery directory {}: {error}",
                directory.display()
            ));
            return diagnostics;
        }
    };
    let (scanned, truncated) = bounded_recovery_entries(entries);
    if truncated {
        diagnostics.push(format!(
            "recovery directory scan stopped at safety limit of {RECOVERY_DIRECTORY_ENTRY_LIMIT} raw entries"
        ));
    }
    let mut paths = scanned
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry.path()),
            Err(error) => {
                diagnostics.push(format!(
                    "cannot inspect entry in recovery directory {}: {error}",
                    directory.display()
                ));
                None
            }
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .map(|path| {
            let modified = fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            (path, modified)
        })
        .collect::<Vec<_>>();
    paths.sort_by(|(left_path, left_modified), (right_path, right_modified)| {
        right_modified
            .cmp(left_modified)
            .then_with(|| left_path.cmp(right_path))
    });
    for (path, modified) in paths {
        let result = read_recovery_source(&path).and_then(|source| {
            toml::from_str::<RecoveryRecord>(&source).map_err(|error| error.to_string())
        });
        match result {
            Ok(record)
                if record.version == 1
                    || (record.version == RECOVERY_VERSION
                        && (record.original_path.is_none() || record.path().is_some())) =>
            {
                if !visit(path, record, modified) {
                    break;
                }
            }
            Ok(record) => diagnostics.push(format!(
                "unsupported recovery version {} in {}",
                record.version,
                path.display()
            )),
            Err(error) => diagnostics.push(format!(
                "invalid recovery journal {}: {error}",
                path.display()
            )),
        }
    }
    diagnostics
}

/// Removes only valid, supported journals selected by explicit retention
/// policy. Corrupt and unsupported records remain available for manual
/// inspection and are reported rather than silently discarded.
#[must_use]
pub fn cleanup_recovery(
    directory: &Path,
    maximum_records: usize,
    maximum_age_days: u64,
    now: SystemTime,
    protected: &[PathBuf],
) -> RecoveryCleanup {
    // Keep retention decisions and deletion under the same lock as journal replacement.
    let _lock = match lock_file(&directory.join(".recovery.lock")) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return RecoveryCleanup::default(),
        Err(error) => {
            return RecoveryCleanup {
                diagnostics: vec![format!(
                    "cannot lock recovery directory {}: {error}",
                    directory.display()
                )],
                ..RecoveryCleanup::default()
            };
        }
    };
    let mut records = Vec::new();
    // Retention needs only paths and timestamps; drop each payload before reading the next.
    let diagnostics = visit_recovery(directory, |path, _, modified| {
        if protected.contains(&path) {
            return true;
        }
        records.push((path, modified));
        true
    });
    let mut cleanup = RecoveryCleanup {
        diagnostics,
        ..RecoveryCleanup::default()
    };
    let maximum_age = (maximum_age_days > 0)
        .then(|| Duration::from_secs(maximum_age_days.saturating_mul(86_400)));
    for (index, (path, modified)) in records.into_iter().enumerate() {
        let expired = maximum_age.is_some_and(|maximum| {
            modified
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > maximum)
        });
        if !expired && index < maximum_records {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => cleanup.deleted.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => cleanup.diagnostics.push(format!(
                "cannot prune recovery journal {}: {error}",
                path.display()
            )),
        }
    }
    cleanup
}
