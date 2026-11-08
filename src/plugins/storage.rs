//! Private plugin storage requests and atomic persistence.

#![forbid(unsafe_code)]

use crate::plugins::PluginLoadId;

use crate::plugins::PluginStorageContribution;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;

pub const STORAGE_REQUEST_LIMIT: usize = 64;
pub const STORAGE_KEY_BYTE_LIMIT: usize = 128;
pub const STORAGE_VALUE_BYTE_LIMIT: usize = 256 * 1024;
pub const STORAGE_FILE_BYTE_LIMIT: u64 = 8 * 1024 * 1024;
pub const STORAGE_LIST_ENTRY_LIMIT: u16 = 1_024;
pub const STORAGE_LIST_BYTE_LIMIT: usize = 512 * 1024;
pub const STORAGE_RESULT_QUEUE_BYTE_LIMIT: usize = 4 * 1024 * 1024;
const STORAGE_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginStorageErrorCode {
    QueueFull,
    InvalidKey,
    InvalidValue,
    StaleRevision,
    QuotaExceeded,
    Unavailable,
    Corrupt,
    LoadUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStorageError {
    pub code: PluginStorageErrorCode,
    pub detail: String,
}

impl PluginStorageError {
    pub fn new(code: PluginStorageErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: detail
                .to_string()
                .chars()
                .filter(|character| !character.is_control())
                .take(1_024)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginStorageRequestKind {
    Get {
        key: String,
    },
    CompareAndSet {
        key: String,
        expected_revision: Option<u64>,
        value: Vec<u8>,
    },
    Delete {
        key: String,
        expected_revision: u64,
    },
    List {
        prefix: String,
        maximum: u16,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStorageRequest {
    pub token: u64,
    pub kind: PluginStorageRequestKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStorageEntry {
    pub key: String,
    pub revision: u64,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStorageResult {
    pub plugin: String,
    pub load: PluginLoadId,
    pub token: u64,
    pub value: Option<Vec<u8>>,
    pub revision: Option<u64>,
    pub entries: Vec<PluginStorageEntry>,
    pub truncated: bool,
    pub error: Option<PluginStorageError>,
}

impl PluginStorageResult {
    #[must_use]
    pub fn bounded_bytes(&self) -> usize {
        self.value
            .as_ref()
            .map_or(0, Vec::len)
            .saturating_add(self.entries.iter().fold(0_usize, |total, entry| {
                total
                    .saturating_add(entry.key.len())
                    .saturating_add(entry.value.len())
            }))
            .saturating_add(self.error.as_ref().map_or(0, |error| error.detail.len()))
    }

    pub fn rejected(
        plugin: String,
        load: PluginLoadId,
        token: u64,
        error: PluginStorageError,
    ) -> Self {
        Self {
            plugin,
            load,
            token,
            value: None,
            revision: None,
            entries: Vec::new(),
            truncated: false,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredRecord {
    revision: u64,
    value: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    key: String,
    revision: u64,
    value: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPayload {
    schema: u32,
    next_revision: u64,
    records: Vec<StoredEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StorageState {
    next_revision: u64,
    records: BTreeMap<String, StoredRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelope {
    schema: u32,
    payload: StoredPayload,
    sha256: String,
}

#[derive(Debug)]
struct Work {
    plugin: String,
    load: PluginLoadId,
    epoch: u64,
    policy: PluginStorageContribution,
    request: PluginStorageRequest,
}

pub struct PluginStorageWorker {
    sender: SyncSender<Work>,
    receiver: Receiver<(u64, PluginStorageResult)>,
    loads: Arc<Mutex<BTreeMap<String, (PluginLoadId, u64)>>>,
    stopped: bool,
}

impl PluginStorageWorker {
    #[must_use]
    pub fn start(root: PathBuf, capacity: usize) -> Self {
        let capacity = capacity.clamp(1, STORAGE_REQUEST_LIMIT);
        let (sender, requests) = mpsc::sync_channel::<Work>(capacity);
        let (results, receiver) = mpsc::sync_channel::<(u64, PluginStorageResult)>(capacity);
        let loads = Arc::new(Mutex::new(BTreeMap::new()));
        let worker_loads = Arc::clone(&loads);
        thread::Builder::new()
            .name("dust-plugin-storage".into())
            .spawn(move || {
                let mut temporary = 1_u64;
                while let Ok(work) = requests.recv() {
                    let current = worker_loads
                        .lock()
                        .ok()
                        .and_then(|owners| owners.get(&work.plugin).copied());
                    let epoch = work.epoch;
                    let result = if current != Some((work.load, work.epoch)) {
                        PluginStorageResult::rejected(
                            work.plugin,
                            work.load,
                            work.request.token,
                            PluginStorageError::new(
                                PluginStorageErrorCode::LoadUnavailable,
                                "storage load is no longer current",
                            ),
                        )
                    } else {
                        process(&root, work, &mut temporary, &worker_loads)
                    };
                    if results.send((epoch, result)).is_err() {
                        break;
                    }
                }
            })
            .expect("plugin storage worker");
        Self {
            sender,
            receiver,
            loads,
            stopped: false,
        }
    }

    pub fn install_load(&mut self, plugin: &str, load: PluginLoadId) {
        if let Ok(mut loads) = self.loads.lock() {
            let epoch = loads
                .get(plugin)
                .map_or(1, |(_, epoch)| epoch.saturating_add(1).max(1));
            loads.insert(plugin.to_owned(), (load, epoch));
        }
    }

    pub fn reset_load(&mut self, plugin: &str, load: PluginLoadId) {
        if let Ok(mut loads) = self.loads.lock()
            && let Some((current, epoch)) = loads.get_mut(plugin)
            && *current == load
        {
            *epoch = epoch.saturating_add(1).max(1);
        }
    }

    pub fn remove_load(&mut self, plugin: &str, load: PluginLoadId) {
        if let Ok(mut loads) = self.loads.lock()
            && loads
                .get(plugin)
                .is_some_and(|(current, _)| *current == load)
        {
            loads.remove(plugin);
        }
    }

    pub fn request(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        policy: PluginStorageContribution,
        request: PluginStorageRequest,
    ) -> Result<(), PluginStorageError> {
        if self.stopped {
            return Err(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "plugin storage worker stopped",
            ));
        }
        validate_request(&request, &policy)?;
        let epoch = self
            .loads
            .lock()
            .ok()
            .and_then(|owners| owners.get(&plugin).copied())
            .filter(|(current, _)| *current == load)
            .map(|(_, epoch)| epoch)
            .ok_or_else(|| {
                PluginStorageError::new(
                    PluginStorageErrorCode::LoadUnavailable,
                    "storage load is no longer current",
                )
            })?;
        match self.sender.try_send(Work {
            plugin,
            load,
            epoch,
            policy,
            request,
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(PluginStorageError::new(
                PluginStorageErrorCode::QueueFull,
                "plugin storage queue is full",
            )),
            Err(TrySendError::Disconnected(_)) => {
                self.stopped = true;
                Err(PluginStorageError::new(
                    PluginStorageErrorCode::Unavailable,
                    "plugin storage worker stopped",
                ))
            }
        }
    }

    pub fn poll(&mut self) -> Vec<PluginStorageResult> {
        if self.stopped {
            return Vec::new();
        }
        let mut results = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok((epoch, result)) => {
                    if self
                        .loads
                        .lock()
                        .ok()
                        .and_then(|owners| owners.get(&result.plugin).copied())
                        == Some((result.load, epoch))
                    {
                        results.push(result);
                    }
                }
                Err(TryRecvError::Empty) => return results,
                Err(TryRecvError::Disconnected) => {
                    self.stopped = true;
                    return results;
                }
            }
        }
    }

    #[must_use]
    pub const fn stopped(&self) -> bool {
        self.stopped
    }
}

pub fn validate_key(key: &str, allow_empty: bool) -> Result<(), PluginStorageError> {
    if (key.is_empty() && !allow_empty)
        || key.len() > STORAGE_KEY_BYTE_LIMIT
        || key == "."
        || key == ".."
        || key.starts_with('.')
        || key.ends_with('.')
        || key.contains("..")
        || !key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::InvalidKey,
            "storage keys use lowercase ASCII, digits, dot, underscore, and dash only",
        ));
    }
    Ok(())
}

pub fn validate_request(
    request: &PluginStorageRequest,
    policy: &PluginStorageContribution,
) -> Result<(), PluginStorageError> {
    if request.token == 0 {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Unavailable,
            "storage token is invalid",
        ));
    }
    match &request.kind {
        PluginStorageRequestKind::Get { key } => validate_key(key, false),
        PluginStorageRequestKind::CompareAndSet { key, value, .. } => {
            validate_key(key, false)?;
            if value.len() > STORAGE_VALUE_BYTE_LIMIT
                || value.len() > policy.maximum_value_bytes as usize
            {
                return Err(PluginStorageError::new(
                    PluginStorageErrorCode::InvalidValue,
                    "storage value exceeds the accepted bound",
                ));
            }
            Ok(())
        }
        PluginStorageRequestKind::Delete {
            key,
            expected_revision,
        } => {
            validate_key(key, false)?;
            if *expected_revision == 0 {
                return Err(PluginStorageError::new(
                    PluginStorageErrorCode::StaleRevision,
                    "storage revision must be nonzero",
                ));
            }
            Ok(())
        }
        PluginStorageRequestKind::List { prefix, maximum } => {
            validate_key(prefix, true)?;
            if *maximum == 0 || *maximum > STORAGE_LIST_ENTRY_LIMIT {
                return Err(PluginStorageError::new(
                    PluginStorageErrorCode::QuotaExceeded,
                    "storage list count exceeds the host bound",
                ));
            }
            Ok(())
        }
    }
}

fn process(
    root: &Path,
    work: Work,
    temporary: &mut u64,
    loads: &Arc<Mutex<BTreeMap<String, (PluginLoadId, u64)>>>,
) -> PluginStorageResult {
    let token = work.request.token;
    let plugin = work.plugin;
    let load = work.load;
    let path = root.join(format!("{:x}.json", Sha256::digest(plugin.as_bytes())));
    let result = (|| {
        fs::create_dir_all(root)
            .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
        let metadata = fs::symlink_metadata(root)
            .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
        if !metadata.file_type().is_dir() {
            return Err(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage root is not a regular directory",
            ));
        }
        let _lock = crate::files::lock_file(&path.with_extension("lock"))
            .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
        let current = load_state(&path)?;
        match work.request.kind {
            PluginStorageRequestKind::Get { key } => {
                let record = current.records.get(&key);
                Ok(PluginStorageResult {
                    plugin: plugin.clone(),
                    load,
                    token,
                    value: record.map(|record| record.value.clone()),
                    revision: record.map(|record| record.revision),
                    entries: Vec::new(),
                    truncated: false,
                    error: None,
                })
            }
            PluginStorageRequestKind::CompareAndSet {
                key,
                expected_revision,
                value,
            } => {
                let actual = current.records.get(&key).map(|record| record.revision);
                if actual != expected_revision {
                    return Err(PluginStorageError::new(
                        PluginStorageErrorCode::StaleRevision,
                        "storage compare-and-set revision is stale",
                    ));
                }
                let revision = current.next_revision;
                let next_revision = revision.checked_add(1).ok_or_else(|| {
                    PluginStorageError::new(
                        PluginStorageErrorCode::QuotaExceeded,
                        "storage revision exhausted",
                    )
                })?;
                let mut candidate = current.clone();
                candidate.next_revision = next_revision;
                candidate
                    .records
                    .insert(key, StoredRecord { revision, value });
                validate_state(&candidate, &work.policy)?;
                persist(PersistContext {
                    root,
                    path: &path,
                    state: &candidate,
                    temporary,
                    plugin: &plugin,
                    load,
                    epoch: work.epoch,
                    loads,
                })?;
                Ok(PluginStorageResult {
                    plugin: plugin.clone(),
                    load,
                    token,
                    value: None,
                    revision: Some(revision),
                    entries: Vec::new(),
                    truncated: false,
                    error: None,
                })
            }
            PluginStorageRequestKind::Delete {
                key,
                expected_revision,
            } => {
                if current.records.get(&key).map(|record| record.revision)
                    != Some(expected_revision)
                {
                    return Err(PluginStorageError::new(
                        PluginStorageErrorCode::StaleRevision,
                        "storage delete revision is stale",
                    ));
                }
                let mut candidate = current.clone();
                candidate.records.remove(&key);
                persist(PersistContext {
                    root,
                    path: &path,
                    state: &candidate,
                    temporary,
                    plugin: &plugin,
                    load,
                    epoch: work.epoch,
                    loads,
                })?;
                Ok(PluginStorageResult {
                    plugin: plugin.clone(),
                    load,
                    token,
                    value: None,
                    revision: Some(expected_revision),
                    entries: Vec::new(),
                    truncated: false,
                    error: None,
                })
            }
            PluginStorageRequestKind::List { prefix, maximum } => {
                let mut entries = Vec::new();
                let mut bytes = 0_usize;
                let mut truncated = false;
                for (key, record) in current.records.range(prefix.clone()..) {
                    if !key.starts_with(&prefix) {
                        break;
                    }
                    let entry_bytes = key.len().saturating_add(record.value.len());
                    if entries.len() >= usize::from(maximum)
                        || entries.len() >= usize::from(STORAGE_LIST_ENTRY_LIMIT)
                        || bytes.saturating_add(entry_bytes) > STORAGE_LIST_BYTE_LIMIT
                    {
                        truncated = true;
                        break;
                    }
                    bytes += entry_bytes;
                    entries.push(PluginStorageEntry {
                        key: key.clone(),
                        revision: record.revision,
                        value: record.value.clone(),
                    });
                }
                Ok(PluginStorageResult {
                    plugin: plugin.clone(),
                    load,
                    token,
                    value: None,
                    revision: None,
                    entries,
                    truncated,
                    error: None,
                })
            }
        }
    })();
    result.unwrap_or_else(|error| PluginStorageResult::rejected(plugin, load, token, error))
}

fn empty_state() -> StorageState {
    StorageState {
        next_revision: 1,
        records: BTreeMap::new(),
    }
}

fn load_state(path: &Path) -> Result<StorageState, PluginStorageError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(empty_state()),
        Err(error) => {
            return Err(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                error,
            ));
        }
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > STORAGE_FILE_BYTE_LIMIT
    {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Corrupt,
            "storage state is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?
        .take(STORAGE_FILE_BYTE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    if bytes.len() as u64 > STORAGE_FILE_BYTE_LIMIT {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Corrupt,
            "storage state exceeds the file bound",
        ));
    }
    let envelope: StoredEnvelope = serde_json::from_slice(&bytes).map_err(|_| {
        PluginStorageError::new(
            PluginStorageErrorCode::Corrupt,
            "storage state is malformed",
        )
    })?;
    if envelope.schema != STORAGE_SCHEMA || envelope.payload.schema != STORAGE_SCHEMA {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Corrupt,
            "storage schema is unsupported",
        ));
    }
    let payload = serde_json::to_vec(&envelope.payload)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Corrupt, error))?;
    let checksum = format!("{:x}", Sha256::digest(payload));
    let mut records = BTreeMap::new();
    let mut revisions = BTreeSet::new();
    let invalid = checksum != envelope.sha256
        || envelope.payload.next_revision == 0
        || envelope.payload.records.iter().any(|record| {
            validate_key(&record.key, false).is_err()
                || record.revision == 0
                || record.revision >= envelope.payload.next_revision
                || record.value.len() > STORAGE_VALUE_BYTE_LIMIT
                || !revisions.insert(record.revision)
                || records
                    .insert(
                        record.key.clone(),
                        StoredRecord {
                            revision: record.revision,
                            value: record.value.clone(),
                        },
                    )
                    .is_some()
        });
    if invalid {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Corrupt,
            "storage state integrity check failed",
        ));
    }
    Ok(StorageState {
        next_revision: envelope.payload.next_revision,
        records,
    })
}

fn validate_state(
    state: &StorageState,
    policy: &PluginStorageContribution,
) -> Result<(), PluginStorageError> {
    let bytes = state
        .records
        .iter()
        .try_fold(0_usize, |total, (key, record)| {
            total
                .checked_add(key.len())?
                .checked_add(record.value.len())
        });
    if state.records.len() > usize::from(policy.maximum_items)
        || bytes.is_none_or(|bytes| bytes > policy.maximum_total_bytes as usize)
        || state
            .records
            .values()
            .any(|record| record.value.len() > policy.maximum_value_bytes as usize)
    {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::QuotaExceeded,
            "storage candidate exceeds its accepted quota",
        ));
    }
    Ok(())
}

struct PersistContext<'a> {
    root: &'a Path,
    path: &'a Path,
    state: &'a StorageState,
    temporary: &'a mut u64,
    plugin: &'a str,
    load: PluginLoadId,
    epoch: u64,
    loads: &'a Arc<Mutex<BTreeMap<String, (PluginLoadId, u64)>>>,
}

fn persist(context: PersistContext<'_>) -> Result<(), PluginStorageError> {
    let PersistContext {
        root,
        path,
        state,
        temporary,
        plugin,
        load,
        epoch,
        loads,
    } = context;
    fs::create_dir_all(root)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::Unavailable,
            "storage root is not a regular directory",
        ));
    }
    let payload = StoredPayload {
        schema: STORAGE_SCHEMA,
        next_revision: state.next_revision,
        records: state
            .records
            .iter()
            .map(|(key, record)| StoredEntry {
                key: key.clone(),
                revision: record.revision,
                value: record.value.clone(),
            })
            .collect(),
    };
    let payload_bytes = serde_json::to_vec(&payload)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    let envelope = StoredEnvelope {
        schema: STORAGE_SCHEMA,
        payload,
        sha256: format!("{:x}", Sha256::digest(&payload_bytes)),
    };
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    if bytes.len() as u64 > STORAGE_FILE_BYTE_LIMIT {
        return Err(PluginStorageError::new(
            PluginStorageErrorCode::QuotaExceeded,
            "serialized storage exceeds the file bound",
        ));
    }
    let temp = root.join(format!(
        ".storage-{}-{:08x}-{:016x}.tmp",
        path.file_stem().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        *temporary
    ));
    *temporary = temporary.checked_add(1).ok_or_else(|| {
        PluginStorageError::new(
            PluginStorageErrorCode::Unavailable,
            "storage temporary ID exhausted",
        )
    })?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temp)
        .map_err(|error| PluginStorageError::new(PluginStorageErrorCode::Unavailable, error))?;
    let mut cancelled = false;
    let result = (|| {
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        let owners = loads
            .lock()
            .map_err(|_| std::io::Error::other("storage load lock poisoned"))?;
        if owners.get(plugin).copied() != Some((load, epoch)) {
            cancelled = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "storage load was cancelled",
            ));
        }
        fs::rename(&temp, path)?;
        drop(owners);
        crate::files::sync_directory(root)?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(PluginStorageError::new(
            if cancelled {
                PluginStorageErrorCode::LoadUnavailable
            } else {
                PluginStorageErrorCode::Unavailable
            },
            error,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::plugins::storage::*;
    use std::time::Duration;
    use std::time::Instant;

    fn policy() -> PluginStorageContribution {
        PluginStorageContribution {
            maximum_items: 2,
            maximum_value_bytes: 8,
            maximum_total_bytes: 32,
        }
    }

    fn wait(service: &mut PluginStorageWorker) -> PluginStorageResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = service.poll().pop() {
                return result;
            }
            assert!(Instant::now() < deadline, "storage result timed out");
            thread::yield_now();
        }
    }

    fn request(token: u64, kind: PluginStorageRequestKind) -> PluginStorageRequest {
        PluginStorageRequest { token, kind }
    }

    fn write_payload(path: &Path, payload: StoredPayload) -> Vec<u8> {
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let envelope = StoredEnvelope {
            schema: STORAGE_SCHEMA,
            payload,
            sha256: format!("{:x}", Sha256::digest(payload_bytes)),
        };
        let bytes = serde_json::to_vec(&envelope).unwrap();
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn storage_is_atomic_revisioned_persistent_and_load_owned() {
        let root = tempfile::tempdir().expect("root");
        let mut service = PluginStorageWorker::start(root.path().join("data"), 8);
        service.install_load("demo", PluginLoadId(7));
        service
            .request(
                "demo".into(),
                PluginLoadId(7),
                policy(),
                request(
                    1,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "item.one".into(),
                        expected_revision: None,
                        value: b"value".to_vec(),
                    },
                ),
            )
            .expect("CAS");
        let stored = wait(&mut service);
        assert_eq!(stored.revision, Some(1));
        assert!(stored.error.is_none());
        // Permission bits are Unix-specific; the storage behavior is shared.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root
                .path()
                .join("data")
                .join(format!("{:x}.json", Sha256::digest(b"demo")));
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        service
            .request(
                "demo".into(),
                PluginLoadId(7),
                policy(),
                request(
                    2,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "item.one".into(),
                        expected_revision: None,
                        value: b"other".to_vec(),
                    },
                ),
            )
            .expect("stale request queued");
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::StaleRevision
        );
        service
            .request(
                "demo".into(),
                PluginLoadId(7),
                policy(),
                request(
                    3,
                    PluginStorageRequestKind::Get {
                        key: "item.one".into(),
                    },
                ),
            )
            .expect("get");
        assert_eq!(
            wait(&mut service).value.as_deref(),
            Some(b"value".as_slice())
        );
        service.remove_load("demo", PluginLoadId(6));
        service.install_load("demo", PluginLoadId(8));
        assert_eq!(
            service
                .request(
                    "demo".into(),
                    PluginLoadId(7),
                    policy(),
                    request(
                        4,
                        PluginStorageRequestKind::Get {
                            key: "item.one".into(),
                        },
                    ),
                )
                .unwrap_err()
                .code,
            PluginStorageErrorCode::LoadUnavailable
        );
        service
            .request(
                "demo".into(),
                PluginLoadId(8),
                policy(),
                request(
                    5,
                    PluginStorageRequestKind::List {
                        prefix: "item".into(),
                        maximum: 8,
                    },
                ),
            )
            .expect("list after upgrade");
        assert_eq!(wait(&mut service).entries.len(), 1);
    }

    #[test]
    fn hostile_keys_values_quota_and_corruption_fail_closed() {
        for key in [
            "", ".", "..", ".hidden", "bad/key", "bad\\key", "UPPER", "é",
        ] {
            assert_eq!(
                validate_key(key, false).unwrap_err().code,
                PluginStorageErrorCode::InvalidKey
            );
        }
        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("data");
        let mut service = PluginStorageWorker::start(data.clone(), 8);
        service.install_load("demo", PluginLoadId(1));
        assert_eq!(
            service
                .request(
                    "demo".into(),
                    PluginLoadId(1),
                    policy(),
                    request(
                        1,
                        PluginStorageRequestKind::CompareAndSet {
                            key: "valid".into(),
                            expected_revision: None,
                            value: vec![0; 9],
                        },
                    ),
                )
                .unwrap_err()
                .code,
            PluginStorageErrorCode::InvalidValue
        );
        fs::create_dir_all(&data).unwrap();
        let path = data.join(format!("{:x}.json", Sha256::digest(b"demo")));
        fs::write(&path, b"not-json").unwrap();
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(
                    2,
                    PluginStorageRequestKind::Get {
                        key: "valid".into(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::Corrupt
        );
        assert_eq!(fs::read(path).unwrap(), b"not-json");
    }

    #[test]
    fn item_and_total_quota_rejection_preserves_the_previous_map() {
        let root = tempfile::tempdir().expect("root");
        let mut service = PluginStorageWorker::start(root.path().join("data"), 8);
        service.install_load("demo", PluginLoadId(1));
        let policy = PluginStorageContribution {
            maximum_items: 1,
            maximum_value_bytes: 4,
            maximum_total_bytes: 8,
        };
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy.clone(),
                request(
                    1,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "one".into(),
                        expected_revision: None,
                        value: b"ok".to_vec(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(wait(&mut service).revision, Some(1));
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy.clone(),
                request(
                    2,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "two".into(),
                        expected_revision: None,
                        value: b"ok".to_vec(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::QuotaExceeded
        );
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy,
                request(
                    3,
                    PluginStorageRequestKind::List {
                        prefix: String::new(),
                        maximum: 8,
                    },
                ),
            )
            .unwrap();
        let listed = wait(&mut service);
        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].key, "one");
    }

    #[test]
    fn duplicate_records_and_revision_exhaustion_fail_without_replacement() {
        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let path = data.join(format!("{:x}.json", Sha256::digest(b"demo")));
        let duplicate = StoredPayload {
            schema: STORAGE_SCHEMA,
            next_revision: 3,
            records: vec![
                StoredEntry {
                    key: "same".into(),
                    revision: 1,
                    value: vec![1],
                },
                StoredEntry {
                    key: "same".into(),
                    revision: 2,
                    value: vec![2],
                },
            ],
        };
        let duplicate_bytes = write_payload(&path, duplicate);
        let mut service = PluginStorageWorker::start(data.clone(), 4);
        service.install_load("demo", PluginLoadId(1));
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(1, PluginStorageRequestKind::Get { key: "same".into() }),
            )
            .unwrap();
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::Corrupt
        );
        assert_eq!(fs::read(&path).unwrap(), duplicate_bytes);

        let exhausted = StoredPayload {
            schema: STORAGE_SCHEMA,
            next_revision: u64::MAX,
            records: Vec::new(),
        };
        let exhausted_bytes = write_payload(&path, exhausted);
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(
                    2,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "valid".into(),
                        expected_revision: None,
                        value: vec![1],
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::QuotaExceeded
        );
        assert_eq!(fs::read(path).unwrap(), exhausted_bytes);
    }

    #[test]
    fn idle_polling_performs_no_storage_io() {
        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("not-created");
        let mut service = PluginStorageWorker::start(data.clone(), 4);
        for _ in 0..256 {
            assert!(service.poll().is_empty());
        }
        assert!(!data.exists());
    }

    #[test]
    fn failed_atomic_write_preserves_the_previous_file() {
        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("data");
        let mut service = PluginStorageWorker::start(data.clone(), 4);
        service.install_load("demo", PluginLoadId(1));
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(
                    1,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "valid".into(),
                        expected_revision: None,
                        value: b"old".to_vec(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(wait(&mut service).revision, Some(1));
        let path = data.join(format!("{:x}.json", Sha256::digest(b"demo")));
        let previous = fs::read(&path).unwrap();
        // Occupy the next temporary path so create_new fails even as root.
        let temporary = data.join(format!(
            ".storage-{}-{:08x}-{:016x}.tmp",
            path.file_stem().unwrap().to_string_lossy(),
            std::process::id(),
            2_u64,
        ));
        fs::write(&temporary, b"occupied").unwrap();
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(
                    2,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "valid".into(),
                        expected_revision: Some(1),
                        value: b"new".to_vec(),
                    },
                ),
            )
            .unwrap();
        let failed = wait(&mut service);
        assert_eq!(fs::read(temporary).unwrap(), b"occupied");
        assert_eq!(
            failed.error.unwrap().code,
            PluginStorageErrorCode::Unavailable
        );
        assert_eq!(fs::read(path).unwrap(), previous);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_state_is_never_followed() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(
            &outside,
            data.join(format!("{:x}.json", Sha256::digest(b"demo"))),
        )
        .unwrap();
        let mut service = PluginStorageWorker::start(data, 4);
        service.install_load("demo", PluginLoadId(1));
        service
            .request(
                "demo".into(),
                PluginLoadId(1),
                policy(),
                request(
                    1,
                    PluginStorageRequestKind::Get {
                        key: "valid".into(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            wait(&mut service).error.unwrap().code,
            PluginStorageErrorCode::Corrupt
        );
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    fn result(service: &mut PluginStorageWorker) -> PluginStorageResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = service.poll().pop() {
                return result;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn storage_transactions_share_a_stable_lock_and_deletions_reduce_quota() {
        let root = tempfile::tempdir().unwrap();
        let mut first = PluginStorageWorker::start(root.path().into(), 8);
        let mut second = PluginStorageWorker::start(root.path().into(), 8);
        let load = PluginLoadId(1);
        first.install_load("demo", load);
        second.install_load("demo", load);
        let policy = PluginStorageContribution {
            maximum_items: 4,
            maximum_value_bytes: 8,
            maximum_total_bytes: 128,
        };
        let path = root
            .path()
            .join(format!("{:x}.lock", Sha256::digest(b"demo")));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let lock = crate::files::lock_exclusive(file, false).unwrap();
        for (service, key) in [(&mut first, "one"), (&mut second, "two")] {
            service
                .request(
                    "demo".into(),
                    load,
                    policy.clone(),
                    PluginStorageRequest {
                        token: 1,
                        kind: PluginStorageRequestKind::CompareAndSet {
                            key: key.into(),
                            expected_revision: None,
                            value: vec![1],
                        },
                    },
                )
                .unwrap();
        }
        assert!(first.poll().is_empty());
        assert!(second.poll().is_empty());
        drop(lock);
        let a = result(&mut first);
        let b = result(&mut second);
        assert!(a.error.is_none() && b.error.is_none());
        assert_ne!(a.revision, b.revision);
        first
            .request(
                "demo".into(),
                load,
                policy.clone(),
                PluginStorageRequest {
                    token: 3,
                    kind: PluginStorageRequestKind::CompareAndSet {
                        key: "three".into(),
                        expected_revision: None,
                        value: vec![1],
                    },
                },
            )
            .unwrap();
        assert!(result(&mut first).error.is_none());
        let state = load_state(
            &root
                .path()
                .join(format!("{:x}.json", Sha256::digest(b"demo"))),
        )
        .unwrap();
        assert_eq!(state.records.len(), 3);
        let lowered = PluginStorageContribution {
            maximum_items: 1,
            ..policy
        };
        first
            .request(
                "demo".into(),
                load,
                lowered,
                PluginStorageRequest {
                    token: 2,
                    kind: PluginStorageRequestKind::Delete {
                        key: "one".into(),
                        expected_revision: a.revision.unwrap(),
                    },
                },
            )
            .unwrap();
        assert!(result(&mut first).error.is_none());
    }

    #[test]
    fn buffered_storage_results_are_discarded_after_reset() {
        let root = tempfile::tempdir().unwrap();
        let mut service = PluginStorageWorker::start(root.path().into(), 8);
        let load = PluginLoadId(1);
        service.install_load("demo", load);
        service
            .request(
                "demo".into(),
                load,
                PluginStorageContribution {
                    maximum_items: 1,
                    maximum_value_bytes: 8,
                    maximum_total_bytes: 32,
                },
                PluginStorageRequest {
                    token: 1,
                    kind: PluginStorageRequestKind::Get { key: "one".into() },
                },
            )
            .unwrap();
        let completion = service
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        service.reset_load("demo", load);
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(completion).unwrap();
        service.receiver = receiver;
        assert!(service.poll().is_empty());
    }

    #[test]
    fn storage_revocation_prevents_a_waiting_transaction_from_committing() {
        let root = tempfile::tempdir().unwrap();
        let mut service = PluginStorageWorker::start(root.path().into(), 8);
        let load = PluginLoadId(1);
        service.install_load("demo", load);
        let lock_path = root
            .path()
            .join(format!("{:x}.lock", Sha256::digest(b"demo")));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
            .unwrap();
        let lock = crate::files::lock_exclusive(file, false).unwrap();
        service
            .request(
                "demo".into(),
                load,
                policy(),
                request(
                    1,
                    PluginStorageRequestKind::CompareAndSet {
                        key: "one".into(),
                        expected_revision: None,
                        value: vec![1],
                    },
                ),
            )
            .unwrap();
        service.reset_load("demo", load);
        drop(lock);
        let (_, result) = service
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            result.error.unwrap().code,
            PluginStorageErrorCode::LoadUnavailable
        );
        assert!(
            !root
                .path()
                .join(format!("{:x}.json", Sha256::digest(b"demo")))
                .exists()
        );
    }
}
