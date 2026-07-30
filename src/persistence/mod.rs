#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};
use thiserror::Error;

pub const STATE_VERSION: u32 = 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub struct PersistencePolicy {
    pub maximum_file_bytes: usize,
    pub maximum_entries: usize,
}
impl Default for PersistencePolicy {
    fn default() -> Self {
        Self {
            maximum_file_bytes: 4 * 1024 * 1024,
            maximum_entries: 10_000,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedLocation {
    pub path: PathBuf,
    pub byte: usize,
    pub anchor: usize,
    pub scroll_line: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedPane {
    pub path: PathBuf,
    pub cursor: usize,
    pub anchor: usize,
    pub scroll_line: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PersistedLayout {
    Pane {
        pane: PersistedPane,
    },
    Split {
        axis: String,
        ratio: u16,
        first: Box<PersistedLayout>,
        second: Box<PersistedLayout>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedTab {
    pub layout: PersistedLayout,
    pub focused_path: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedSession {
    pub roots: Vec<PathBuf>,
    pub tabs: Vec<PersistedTab>,
    pub active_tab: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedMark {
    pub name: String,
    pub location: PersistedLocation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedBreakpoint {
    pub path: PathBuf,
    pub line: u64,
    pub column: Option<u64>,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedWatch {
    pub expression: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedLargeFileOverride {
    pub path: PathBuf,
    pub mode: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedState {
    pub version: u32,
    pub session: Option<PersistedSession>,
    pub recent_files: Vec<PathBuf>,
    pub marks: Vec<PersistedMark>,
    pub jumps: Vec<PersistedLocation>,
    pub changes: Vec<PersistedLocation>,
    pub command_history: Vec<String>,
    pub search_history: Vec<String>,
    #[serde(default)]
    pub breakpoints: Vec<PersistedBreakpoint>,
    #[serde(default)]
    pub watches: Vec<PersistedWatch>,
    #[serde(default)]
    pub large_file_overrides: Vec<PersistedLargeFileOverride>,
    pub metadata: BTreeMap<String, String>,
}
impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            session: None,
            recent_files: Vec::new(),
            marks: Vec::new(),
            jumps: Vec::new(),
            changes: Vec::new(),
            command_history: Vec::new(),
            search_history: Vec::new(),
            breakpoints: Vec::new(),
            watches: Vec::new(),
            large_file_overrides: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}
impl PersistedState {
    pub fn validate(&self, policy: &PersistencePolicy) -> Result<(), PersistenceError> {
        if self.version != STATE_VERSION {
            return Err(PersistenceError::UnsupportedVersion(self.version));
        }
        for length in [
            self.recent_files.len(),
            self.marks.len(),
            self.jumps.len(),
            self.changes.len(),
            self.command_history.len(),
            self.search_history.len(),
            self.breakpoints.len(),
            self.watches.len(),
            self.large_file_overrides.len(),
            self.session.as_ref().map_or(0, |s| s.tabs.len()),
        ] {
            if length > policy.maximum_entries {
                return Err(PersistenceError::TooManyEntries);
            }
        }
        if self
            .command_history
            .iter()
            .chain(&self.search_history)
            .chain(self.watches.iter().map(|watch| &watch.expression))
            .any(|entry| entry.len() > 64 * 1024)
            || self.large_file_overrides.iter().any(|entry| {
                !entry.path.is_absolute() || !matches!(entry.mode.as_str(), "auto" | "on" | "off")
            })
            || self.breakpoints.iter().any(|breakpoint| {
                breakpoint.line == 0
                    || [
                        breakpoint.condition.as_ref(),
                        breakpoint.hit_condition.as_ref(),
                        breakpoint.log_message.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|value| value.len() > 64 * 1024)
            })
        {
            return Err(PersistenceError::EntryTooLarge);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PersistenceId(pub u64);
#[derive(Debug)]
enum Request {
    Read {
        generation: PersistenceId,
        path: PathBuf,
        policy: PersistencePolicy,
    },
    Write {
        generation: PersistenceId,
        path: PathBuf,
        state: PersistedState,
        policy: PersistencePolicy,
    },
}
#[derive(Debug)]
pub enum PersistenceEvent {
    Loaded {
        generation: PersistenceId,
        state: PersistedState,
    },
    Saved {
        generation: PersistenceId,
    },
    Failed {
        generation: PersistenceId,
        error: PersistenceError,
    },
}
pub struct PersistenceService {
    ids: Arc<dyn crate::clock::IdSource>,
    sender: SyncSender<Request>,
    receiver: Receiver<PersistenceEvent>,
}
impl PersistenceService {
    pub fn start(capacity: usize) -> Self {
        Self::start_with_ids(capacity, Arc::new(crate::clock::AtomicIds::new(1)))
    }
    pub fn start_with_ids(capacity: usize, ids: Arc<dyn crate::clock::IdSource>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity.clamp(1, 64));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 64));
        thread::Builder::new()
            .name("dust-persistence".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    let event = match request {
                        Request::Read {
                            generation,
                            path,
                            policy,
                        } => match read_state(&path, &policy) {
                            Ok(state) => PersistenceEvent::Loaded { generation, state },
                            Err(error) => PersistenceEvent::Failed { generation, error },
                        },
                        Request::Write {
                            generation,
                            path,
                            state,
                            policy,
                        } => match write_state(&path, &state, &policy) {
                            Ok(()) => PersistenceEvent::Saved { generation },
                            Err(error) => PersistenceEvent::Failed { generation, error },
                        },
                    };
                    let _ = etx.try_send(event);
                }
            })
            .expect("persistence worker");
        Self {
            ids,
            sender: tx,
            receiver: erx,
        }
    }
    fn id(&self) -> PersistenceId {
        PersistenceId(self.ids.next())
    }
    pub fn read(
        &mut self,
        path: PathBuf,
        policy: PersistencePolicy,
    ) -> Result<PersistenceId, PersistenceError> {
        let generation = self.id();
        self.sender
            .try_send(Request::Read {
                generation,
                path,
                policy,
            })
            .map_err(queue_error)?;
        Ok(generation)
    }
    pub fn write(
        &mut self,
        path: PathBuf,
        state: PersistedState,
        policy: PersistencePolicy,
    ) -> Result<PersistenceId, PersistenceError> {
        state.validate(&policy)?;
        let generation = self.id();
        self.sender
            .try_send(Request::Write {
                generation,
                path,
                state,
                policy,
            })
            .map_err(queue_error)?;
        Ok(generation)
    }
    pub fn try_event(&self) -> Result<Option<PersistenceEvent>, PersistenceError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(PersistenceError::Stopped),
        }
    }
}
fn queue_error(error: TrySendError<Request>) -> PersistenceError {
    match error {
        TrySendError::Full(_) => PersistenceError::QueueFull,
        TrySendError::Disconnected(_) => PersistenceError::Stopped,
    }
}
fn read_state(path: &Path, policy: &PersistencePolicy) -> Result<PersistedState, PersistenceError> {
    let metadata = fs::symlink_metadata(path).map_err(PersistenceError::Io)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(PersistenceError::InvalidFileType);
    }
    if metadata.len() > policy.maximum_file_bytes as u64 {
        return Err(PersistenceError::TooLarge);
    }
    let file = fs::File::open(path).map_err(PersistenceError::Io)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(policy.maximum_file_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PersistenceError::Io)?;
    if bytes.len() > policy.maximum_file_bytes {
        return Err(PersistenceError::TooLarge);
    }
    let state: PersistedState = serde_json::from_slice(&bytes).map_err(PersistenceError::Json)?;
    state.validate(policy)?;
    Ok(state)
}
fn write_state(
    path: &Path,
    state: &PersistedState,
    policy: &PersistencePolicy,
) -> Result<(), PersistenceError> {
    let bytes = serde_json::to_vec(state).map_err(PersistenceError::Json)?;
    if bytes.len() > policy.maximum_file_bytes {
        return Err(PersistenceError::TooLarge);
    }
    let parent = path.parent().ok_or(PersistenceError::InvalidPath)?;
    fs::create_dir_all(parent).map_err(PersistenceError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(PersistenceError::Io)?;
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.file_type().is_file() || metadata.file_type().is_symlink())
    {
        return Err(PersistenceError::InvalidFileType);
    }
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(PersistenceError::Io)?;
    file.write_all(&bytes).map_err(PersistenceError::Io)?;
    file.sync_all().map_err(PersistenceError::Io)?;
    fs::rename(&temp, path).map_err(PersistenceError::Io)?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(PersistenceError::Io)
}
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("persistence I/O failed: {0}")]
    Io(#[source] io::Error),
    #[error("persistence JSON is invalid: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported persistence version {0}")]
    UnsupportedVersion(u32),
    #[error("persistence file exceeds its byte ceiling")]
    TooLarge,
    #[error("persistence collection exceeds its entry ceiling")]
    TooManyEntries,
    #[error("persistence entry exceeds its byte ceiling")]
    EntryTooLarge,
    #[error("persistence path has no parent")]
    InvalidPath,
    #[error("persistence target is not a regular file")]
    InvalidFileType,
    #[error("persistence queue is full")]
    QueueFull,
    #[error("persistence worker stopped")]
    Stopped,
}
