#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
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

pub const STATE_VERSION: u32 = 2;
const MAXIMUM_SCROLL_COLUMN: usize = 1_000_000_000;
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
    pub scroll_column: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedPane {
    pub path: PathBuf,
    pub cursor: usize,
    pub anchor: usize,
    pub scroll_line: usize,
    pub scroll_column: usize,
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PersistedWindowPlacement {
    Docked {
        edge: String,
        size: u16,
    },
    Floating {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    Fullscreen,
    Hidden,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedWindowRule {
    /// Stable content-type identity, never a runtime ContentId/ViewId/WindowId.
    pub content_type: String,
    pub placement: PersistedWindowPlacement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedSession {
    pub roots: Vec<PathBuf>,
    pub tabs: Vec<PersistedTab>,
    pub active_tab: usize,
    /// Semantic placement defaults reconstructed with fresh runtime IDs.
    pub window_rules: Vec<PersistedWindowRule>,
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
            self.session.as_ref().map_or(0, |s| s.window_rules.len()),
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
            || self
                .marks
                .iter()
                .map(|mark| &mark.location)
                .chain(&self.jumps)
                .chain(&self.changes)
                .any(|location| location.scroll_column > MAXIMUM_SCROLL_COLUMN)
            || self.session.as_ref().is_some_and(|session| {
                session
                    .tabs
                    .iter()
                    .any(|tab| layout_has_oversized_scroll_column(&tab.layout))
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
        if let Some(session) = &self.session {
            validate_window_rules(&session.window_rules)?;
        }
        Ok(())
    }
}

pub fn validate_window_rules(rules: &[PersistedWindowRule]) -> Result<(), PersistenceError> {
    let mut content_types = BTreeSet::new();
    let fullscreen = rules
        .iter()
        .filter(|rule| matches!(rule.placement, PersistedWindowPlacement::Fullscreen))
        .count();
    if rules.len() > 2
        || fullscreen > 1
        || rules.iter().any(|rule| {
            !valid_content_type(&rule.content_type)
                || !matches!(
                    rule.content_type.as_str(),
                    "builtin.explorer" | "builtin.terminal"
                )
                || !content_types.insert(rule.content_type.as_str())
                || !valid_persisted_window_placement(&rule.placement)
        })
    {
        return Err(PersistenceError::InvalidWindowRule);
    }
    Ok(())
}

fn valid_content_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.split('.').count() >= 2
        && value.split('.').all(|segment| {
            let mut characters = segment.chars();
            characters
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
                && characters.all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
        })
}

fn valid_persisted_window_placement(placement: &PersistedWindowPlacement) -> bool {
    match placement {
        PersistedWindowPlacement::Docked { edge, size } => {
            *size > 0 && matches!(edge.as_str(), "left" | "right" | "top" | "bottom")
        }
        PersistedWindowPlacement::Floating { width, height, .. } => *width > 0 && *height > 0,
        PersistedWindowPlacement::Fullscreen | PersistedWindowPlacement::Hidden => true,
    }
}

fn layout_has_oversized_scroll_column(layout: &PersistedLayout) -> bool {
    match layout {
        PersistedLayout::Pane { pane } => pane.scroll_column > MAXIMUM_SCROLL_COLUMN,
        PersistedLayout::Split { first, second, .. } => {
            layout_has_oversized_scroll_column(first) || layout_has_oversized_scroll_column(second)
        }
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
        state: Box<PersistedState>,
        policy: PersistencePolicy,
    },
}
#[derive(Debug)]
pub enum PersistenceEvent {
    Loaded {
        generation: PersistenceId,
        state: Box<PersistedState>,
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
                            Ok(state) => PersistenceEvent::Loaded {
                                generation,
                                state: Box::new(state),
                            },
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
                state: Box::new(state),
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
    #[error("persistent window rule is invalid or duplicated")]
    InvalidWindowRule,
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{
        PersistedLayout, PersistedLocation, PersistedPane, PersistedSession, PersistedState,
        PersistedTab, PersistedWindowPlacement, PersistedWindowRule, PersistenceError,
        PersistencePolicy, STATE_VERSION, read_state, write_state,
    };

    fn state_with_horizontal_offsets() -> PersistedState {
        PersistedState {
            version: STATE_VERSION,
            session: Some(PersistedSession {
                roots: vec![PathBuf::from("/workspace")],
                tabs: vec![PersistedTab {
                    layout: PersistedLayout::Pane {
                        pane: PersistedPane {
                            path: PathBuf::from("/workspace/main.rs"),
                            cursor: 27,
                            anchor: 27,
                            scroll_line: 3,
                            scroll_column: 19,
                        },
                    },
                    focused_path: Some(PathBuf::from("/workspace/main.rs")),
                }],
                active_tab: 0,
                window_rules: vec![PersistedWindowRule {
                    content_type: "builtin.explorer".into(),
                    placement: PersistedWindowPlacement::Docked {
                        edge: "left".into(),
                        size: 41,
                    },
                }],
            }),
            jumps: vec![PersistedLocation {
                path: PathBuf::from("/workspace/main.rs"),
                byte: 27,
                anchor: 27,
                scroll_line: 3,
                scroll_column: 17,
            }],
            metadata: BTreeMap::new(),
            ..PersistedState::default()
        }
    }

    #[test]
    fn pane_and_navigation_scroll_columns_round_trip_strictly() {
        let root = tempdir().expect("temporary persistence directory");
        let path = root.path().join("state.json");
        let policy = PersistencePolicy::default();
        write_state(&path, &state_with_horizontal_offsets(), &policy).expect("write state");
        let restored = read_state(&path, &policy).expect("read state");
        let pane = match &restored.session.as_ref().expect("session").tabs[0].layout {
            PersistedLayout::Pane { pane } => pane,
            PersistedLayout::Split { .. } => panic!("expected pane"),
        };
        assert_eq!(pane.scroll_column, 19);
        assert_eq!(restored.jumps[0].scroll_column, 17);
    }

    #[test]
    fn semantic_window_rules_round_trip_and_reject_missing_duplicate_or_invalid_data() {
        let state = state_with_horizontal_offsets();
        let encoded = serde_json::to_value(&state).expect("serialize state");
        let decoded: PersistedState = serde_json::from_value(encoded.clone()).expect("strict rule");
        assert_eq!(
            decoded.session.expect("session").window_rules[0].content_type,
            "builtin.explorer"
        );

        let mut missing = encoded.clone();
        missing["session"]
            .as_object_mut()
            .expect("session")
            .remove("window_rules");
        assert!(serde_json::from_value::<PersistedState>(missing).is_err());

        let mut unknown_field = encoded.clone();
        unknown_field["session"]["window_rules"][0]["runtime_window_id"] = serde_json::json!(42);
        assert!(serde_json::from_value::<PersistedState>(unknown_field).is_err());

        let mut duplicated: PersistedState =
            serde_json::from_value(encoded.clone()).expect("decode duplicate candidate");
        let duplicate = duplicated.session.as_ref().expect("session").window_rules[0].clone();
        duplicated
            .session
            .as_mut()
            .expect("session")
            .window_rules
            .push(duplicate);
        assert!(matches!(
            duplicated.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidWindowRule)
        ));

        let mut invalid: PersistedState =
            serde_json::from_value(encoded.clone()).expect("decode invalid");
        invalid.session.as_mut().expect("session").window_rules[0].placement =
            PersistedWindowPlacement::Docked {
                edge: "diagonal".into(),
                size: 0,
            };
        assert!(matches!(
            invalid.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidWindowRule)
        ));

        let mut unsupported: PersistedState =
            serde_json::from_value(encoded.clone()).expect("decode unsupported");
        unsupported.session.as_mut().expect("session").window_rules[0].content_type =
            "plugin.unknown".into();
        assert!(matches!(
            unsupported.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidWindowRule)
        ));

        let mut two_fullscreen: PersistedState =
            serde_json::from_value(encoded).expect("decode fullscreen");
        let rules = &mut two_fullscreen
            .session
            .as_mut()
            .expect("session")
            .window_rules;
        rules[0].placement = PersistedWindowPlacement::Fullscreen;
        rules.push(PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: PersistedWindowPlacement::Fullscreen,
        });
        assert!(matches!(
            two_fullscreen.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidWindowRule)
        ));
    }

    #[test]
    fn pre_v1_missing_unknown_and_oversized_scroll_columns_are_rejected() {
        let state = state_with_horizontal_offsets();
        let mut value = serde_json::to_value(&state).expect("serialize state");
        value["jumps"][0]
            .as_object_mut()
            .expect("jump object")
            .remove("scroll_column");
        assert!(serde_json::from_value::<PersistedState>(value).is_err());

        let mut value = serde_json::to_value(&state).expect("serialize state");
        value["jumps"][0]["legacy_scroll_x"] = serde_json::json!(4);
        assert!(serde_json::from_value::<PersistedState>(value).is_err());

        let root = tempdir().expect("temporary persistence directory");
        let path = root.path().join("hostile.json");
        let mut value = serde_json::to_value(&state).expect("serialize state");
        value["jumps"][0]["scroll_column"] = serde_json::json!(1_000_000_001_u64);
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("encode hostile state"),
        )
        .expect("write hostile state");
        assert!(matches!(
            read_state(&path, &PersistencePolicy::default()),
            Err(PersistenceError::EntryTooLarge)
        ));
    }
}
