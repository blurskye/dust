use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use super::{error::VfsError, vfs::atomic_write};

pub const RECOVERY_VERSION: u32 = 1;
pub const DOCUMENT_BYTE_CEILING: u64 = 256 * 1024 * 1024;
pub(super) const RECOVERY_JOURNAL_BYTE_CEILING: u64 = 64 * 1024 * 1024;
pub(super) const RECOVERY_DIRECTORY_ENTRY_CEILING: usize = 10_000;
pub(super) const RECOVERY_TOML_OVERHEAD_ALLOWANCE: u64 = 1_024;
pub(super) const RECOVERY_TOML_MAX_ESCAPE_EXPANSION: u64 = 6;
pub(super) const RECOVERY_PAYLOAD_BYTE_CEILING: u64 = (RECOVERY_JOURNAL_BYTE_CEILING
    - RECOVERY_TOML_OVERHEAD_ALLOWANCE)
    / RECOVERY_TOML_MAX_ESCAPE_EXPANSION;

pub(super) const fn recovery_journal_size_allowed(bytes: u64) -> bool {
    bytes <= RECOVERY_JOURNAL_BYTE_CEILING
}

/// Whether a file this size fits in a recovery journal, leaving room for its path.
pub const fn recovery_size_allowed(content_bytes: u64) -> bool {
    recovery_payload_size_allowed(content_bytes, 4096)
}

pub(super) const fn recovery_payload_size_allowed(content_bytes: u64, path_bytes: u64) -> bool {
    content_bytes.saturating_add(path_bytes) <= RECOVERY_PAYLOAD_BYTE_CEILING
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

pub(super) fn read_bounded_utf8(
    reader: impl Read,
    maximum_bytes: u64,
    kind: &str,
) -> Result<String, String> {
    let bytes = read_bounded_bytes(reader, maximum_bytes)
        .map_err(|error| format!("cannot read {kind}: {error}"))?
        .ok_or_else(|| format!("{kind} exceeds {maximum_bytes} byte safety ceiling"))?;
    String::from_utf8(bytes).map_err(|error| format!("{kind} is not UTF-8: {error}"))
}

pub(super) fn read_recovery_source(path: &Path) -> Result<String, String> {
    let file =
        fs::File::open(path).map_err(|error| format!("cannot open recovery journal: {error}"))?;
    read_bounded_utf8(file, RECOVERY_JOURNAL_BYTE_CEILING, "recovery journal")
}

pub(super) fn bounded_recovery_entries<T>(entries: impl IntoIterator<Item = T>) -> (Vec<T>, bool) {
    let mut paths = entries
        .into_iter()
        .take(RECOVERY_DIRECTORY_ENTRY_CEILING.saturating_add(1))
        .collect::<Vec<_>>();
    let truncated = paths.len() > RECOVERY_DIRECTORY_ENTRY_CEILING;
    paths.truncate(RECOVERY_DIRECTORY_ENTRY_CEILING);
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
            original_path: original_path.map(|path| path.display().to_string()),
            revision,
            content,
        }
    }
}

pub fn write_recovery(path: &Path, record: &RecoveryRecord) -> Result<(), VfsError> {
    let path_bytes = record.original_path.as_ref().map_or(0, String::len) as u64;
    if !recovery_payload_size_allowed(record.content.len() as u64, path_bytes) {
        return Err(VfsError::Recovery {
            path: path.to_path_buf(),
            reason: format!(
                "payload exceeds {} byte pre-serialization recovery safety ceiling",
                RECOVERY_PAYLOAD_BYTE_CEILING
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
                "serialized journal exceeds {} byte recovery safety ceiling",
                RECOVERY_JOURNAL_BYTE_CEILING
            ),
        });
    }
    atomic_write(path, source.as_bytes())
}

#[must_use]
pub fn discover_recovery(directory: &Path) -> RecoveryDiscovery {
    let mut discovery = RecoveryDiscovery::default();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return discovery,
        Err(error) => {
            discovery.diagnostics.push(format!(
                "cannot inspect recovery directory {}: {error}",
                directory.display()
            ));
            return discovery;
        }
    };
    let (scanned, truncated) = bounded_recovery_entries(entries);
    if truncated {
        discovery.diagnostics.push(format!(
            "recovery directory scan stopped at safety ceiling of {RECOVERY_DIRECTORY_ENTRY_CEILING} raw entries"
        ));
    }
    let mut paths = scanned
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let result = read_recovery_source(&path).and_then(|source| {
            toml::from_str::<RecoveryRecord>(&source).map_err(|error| error.to_string())
        });
        match result {
            Ok(record) if record.version == RECOVERY_VERSION => {
                discovery.records.push((path, record));
            }
            Ok(record) => discovery.diagnostics.push(format!(
                "unsupported recovery version {} in {}",
                record.version,
                path.display()
            )),
            Err(error) => discovery.diagnostics.push(format!(
                "invalid recovery journal {}: {error}",
                path.display()
            )),
        }
    }
    discovery
}

/// Removes only valid, current-version journals selected by explicit retention
/// policy. Corrupt and unsupported records remain available for manual
/// inspection and are reported rather than silently discarded.
#[must_use]
pub fn cleanup_recovery(
    directory: &Path,
    maximum_records: usize,
    maximum_age_days: u64,
    now: SystemTime,
) -> RecoveryCleanup {
    let discovery = discover_recovery(directory);
    let mut cleanup = RecoveryCleanup {
        diagnostics: discovery.diagnostics,
        ..RecoveryCleanup::default()
    };
    let maximum_age = (maximum_age_days > 0)
        .then(|| Duration::from_secs(maximum_age_days.saturating_mul(86_400)));
    let mut records = discovery
        .records
        .into_iter()
        .map(|(path, _)| {
            let modified = fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            (path, modified)
        })
        .collect::<Vec<_>>();
    records.sort_by(|(left_path, left_modified), (right_path, right_modified)| {
        right_modified
            .cmp(left_modified)
            .then_with(|| left_path.cmp(right_path))
    });
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
