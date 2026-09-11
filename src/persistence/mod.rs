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
const MAXIMUM_DESCRIPTOR_ID_BYTES: usize = 128;
const MAXIMUM_PROVIDER_ID_BYTES: usize = 256;
const MAXIMUM_VIRTUAL_KEY_BYTES: usize = 1024;
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
    Tiled,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedContentDescriptor {
    /// Inert location in the persisted semantic layout, never a runtime ID.
    pub semantic_id: String,
    pub content_type: String,
    pub adapter: String,
    pub path: Option<PathBuf>,
    pub provider: Option<String>,
    pub source: Option<String>,
    pub key: Option<String>,
    pub desired_placement: PersistedWindowPlacement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedSession {
    pub roots: Vec<PathBuf>,
    pub tabs: Vec<PersistedTab>,
    pub active_tab: usize,
    /// Semantic placement defaults reconstructed with fresh runtime IDs.
    pub window_rules: Vec<PersistedWindowRule>,
    /// This backward-compatible schema-2 extension is absent from older files.
    #[serde(default)]
    pub content_descriptors: Vec<PersistedContentDescriptor>,
}
impl PersistedSession {
    #[must_use]
    pub fn unavailable_virtual_descriptors(&self) -> BTreeSet<String> {
        self.content_descriptors
            .iter()
            .filter(|descriptor| descriptor.adapter == "virtual-document")
            .filter_map(|descriptor| {
                Some(format!(
                    "{}:{}:{}",
                    descriptor.provider.as_deref()?,
                    descriptor.source.as_deref()?,
                    descriptor.key.as_deref()?
                ))
            })
            .collect()
    }
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
            self.session.as_ref().map_or(0, |s| s.roots.len()),
            self.session.as_ref().map_or(0, |s| s.tabs.len()),
            self.session.as_ref().map_or(0, |s| s.window_rules.len()),
            self.session
                .as_ref()
                .map_or(0, |s| s.content_descriptors.len()),
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
            validate_content_descriptors(session, policy.maximum_entries)?;
        }
        Ok(())
    }
}

pub fn validate_content_descriptors(
    session: &PersistedSession,
    maximum_entries: usize,
) -> Result<(), PersistenceError> {
    if (session.tabs.is_empty() && session.active_tab != 0)
        || (!session.tabs.is_empty() && session.active_tab >= session.tabs.len())
        || session
            .roots
            .iter()
            .any(|root| !valid_descriptor_path(root))
    {
        return Err(PersistenceError::InvalidSession);
    }
    let mut identities = BTreeSet::new();
    let mut virtual_identities = BTreeSet::new();
    let mut text = BTreeMap::new();
    for descriptor in &session.content_descriptors {
        if descriptor.semantic_id.is_empty()
            || descriptor.semantic_id.len() > MAXIMUM_DESCRIPTOR_ID_BYTES
            || descriptor.semantic_id.chars().any(char::is_control)
            || !identities.insert(descriptor.semantic_id.as_str())
            || descriptor.adapter.is_empty()
            || descriptor.adapter.len() > 64
            || descriptor.adapter.chars().any(char::is_control)
            || !valid_content_type(&descriptor.content_type)
            || !valid_persisted_window_placement(&descriptor.desired_placement)
        {
            return Err(PersistenceError::InvalidContentDescriptor);
        }
        match descriptor.adapter.as_str() {
            "text-buffer" => {
                if descriptor.content_type != "builtin.text-buffer" {
                    return Err(PersistenceError::InvalidContentDescriptor);
                }
                let (tab, pane) = parse_text_semantic_id(&descriptor.semantic_id)
                    .ok_or(PersistenceError::InvalidContentDescriptor)?;
                let path = descriptor
                    .path
                    .as_ref()
                    .ok_or(PersistenceError::InvalidContentDescriptor)?;
                if descriptor.provider.is_some()
                    || descriptor.source.is_some()
                    || descriptor.key.is_some()
                    || !valid_descriptor_path(path)
                {
                    return Err(PersistenceError::InvalidContentDescriptor);
                }
                if descriptor.semantic_id != format!("text:{tab}:{pane}")
                    || text.insert((tab, pane), path).is_some()
                {
                    return Err(PersistenceError::InvalidContentDescriptor);
                }
            }
            "virtual-document" => {
                if descriptor.content_type != "builtin.virtual-document"
                    || descriptor.path.is_some()
                    || !valid_descriptor_part(
                        descriptor.provider.as_deref(),
                        MAXIMUM_PROVIDER_ID_BYTES,
                    )
                    || !valid_descriptor_part(
                        descriptor.source.as_deref(),
                        MAXIMUM_PROVIDER_ID_BYTES,
                    )
                    || !valid_descriptor_part(descriptor.key.as_deref(), MAXIMUM_VIRTUAL_KEY_BYTES)
                {
                    return Err(PersistenceError::InvalidContentDescriptor);
                }
                let index = descriptor
                    .semantic_id
                    .strip_prefix("virtual:")
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or(PersistenceError::InvalidContentDescriptor)?;
                if descriptor.semantic_id != format!("virtual:{index}")
                    || !virtual_identities.insert(index)
                {
                    return Err(PersistenceError::InvalidContentDescriptor);
                }
            }
            "terminal-session" | "external-tui" | "declarative-ui" | "private-cell-surface" => {
                return Err(PersistenceError::ForbiddenContentDescriptor);
            }
            _ => return Err(PersistenceError::UnknownContentDescriptor),
        }
    }

    let mut pane_count = 0usize;
    for (tab_index, tab) in session.tabs.iter().enumerate() {
        let mut panes = Vec::new();
        collect_layout_paths(&tab.layout, &mut panes, maximum_entries)?;
        if tab
            .focused_path
            .as_ref()
            .is_some_and(|focused| !valid_descriptor_path(focused) || !panes.contains(&focused))
        {
            return Err(PersistenceError::StaleContentDescriptor);
        }
        for (pane_index, path) in panes.into_iter().enumerate() {
            pane_count = pane_count
                .checked_add(1)
                .ok_or(PersistenceError::TooManyEntries)?;
            if !session.content_descriptors.is_empty()
                && text.get(&(tab_index, pane_index)).copied() != Some(path)
            {
                return Err(PersistenceError::StaleContentDescriptor);
            }
        }
    }
    if pane_count > maximum_entries {
        return Err(PersistenceError::TooManyEntries);
    }
    // An empty list denotes a legacy schema-2 session. New descriptor-bearing
    // sessions must describe every and only persisted text pane.
    if !session.content_descriptors.is_empty() && text.len() != pane_count {
        return Err(PersistenceError::StaleContentDescriptor);
    }
    if session.tabs.is_empty()
        && !session
            .content_descriptors
            .iter()
            .any(|descriptor| descriptor.adapter == "virtual-document")
    {
        return Err(PersistenceError::InvalidContentDescriptor);
    }
    Ok(())
}

fn parse_text_semantic_id(value: &str) -> Option<(usize, usize)> {
    let mut parts = value.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("text"), Some(tab), Some(pane), None) => {
            Some((tab.parse().ok()?, pane.parse().ok()?))
        }
        _ => None,
    }
}

fn valid_descriptor_part(value: Option<&str>, maximum: usize) -> bool {
    value.is_some_and(|value| {
        !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
    })
}

fn collect_layout_paths<'a>(
    layout: &'a PersistedLayout,
    panes: &mut Vec<&'a PathBuf>,
    maximum: usize,
) -> Result<(), PersistenceError> {
    if panes.len() >= maximum {
        return Err(PersistenceError::TooManyEntries);
    }
    match layout {
        PersistedLayout::Pane { pane } => {
            if !valid_descriptor_path(&pane.path) {
                return Err(PersistenceError::InvalidContentDescriptor);
            }
            panes.push(&pane.path);
        }
        PersistedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !matches!(axis.as_str(), "horizontal" | "vertical") || !(100..=900).contains(ratio) {
                return Err(PersistenceError::InvalidContentDescriptor);
            }
            collect_layout_paths(first, panes, maximum)?;
            collect_layout_paths(second, panes, maximum)?;
        }
    }
    Ok(())
}

fn valid_descriptor_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
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
                || matches!(rule.placement, PersistedWindowPlacement::Tiled)
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
        PersistedWindowPlacement::Tiled
        | PersistedWindowPlacement::Fullscreen
        | PersistedWindowPlacement::Hidden => true,
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
    last_id: Option<u64>,
    ids: Arc<dyn crate::clock::IdSource>,
    sender: SyncSender<Request>,
    receiver: Receiver<PersistenceEvent>,
}
impl PersistenceService {
    pub fn start(capacity: usize) -> Self {
        Self::start_with_ids(capacity, Arc::new(crate::clock::AtomicIds::new(1)))
    }
    pub fn start_with_ids(capacity: usize, ids: Arc<dyn crate::clock::IdSource>) -> Self {
        Self::start_gated(capacity, ids, None)
    }
    #[cfg(test)]
    pub(crate) fn gated_for_test(capacity: usize) -> (Self, mpsc::Sender<()>) {
        let (send, receive) = mpsc::channel();
        (
            Self::start_gated(
                capacity,
                Arc::new(crate::clock::AtomicIds::new(1)),
                Some(receive),
            ),
            send,
        )
    }
    #[cfg(test)]
    pub(crate) fn stopped_for_test() -> Self {
        let (sender, _) = mpsc::sync_channel(1);
        let (_, receiver) = mpsc::sync_channel(1);
        Self {
            ids: Arc::new(crate::clock::AtomicIds::new(1)),
            last_id: None,
            sender,
            receiver,
        }
    }
    fn start_gated(
        capacity: usize,
        ids: Arc<dyn crate::clock::IdSource>,
        gate: Option<Receiver<()>>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity.clamp(1, 64));
        let (etx, erx) = mpsc::sync_channel(capacity.clamp(1, 64));
        thread::Builder::new()
            .name("dust-persistence".into())
            .spawn(move || {
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
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
                    if etx.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("persistence worker");
        Self {
            last_id: None,
            ids,
            sender: tx,
            receiver: erx,
        }
    }
    fn id(&mut self) -> Result<PersistenceId, PersistenceError> {
        let id = self.ids.next();
        if self.last_id.is_some_and(|last| id <= last) {
            return Err(PersistenceError::Io(io::Error::other(
                "persistence request identity exhausted or reused",
            )));
        }
        self.last_id = Some(id);
        Ok(PersistenceId(id))
    }
    pub fn read(
        &mut self,
        path: PathBuf,
        policy: PersistencePolicy,
    ) -> Result<PersistenceId, PersistenceError> {
        let generation = self.id()?;
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
        let generation = self.id()?;
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
    #[error("persistent session structure is invalid")]
    InvalidSession,
    #[error("persistent content descriptor is invalid or duplicated")]
    InvalidContentDescriptor,
    #[error("persistent content descriptor names an unknown content type")]
    UnknownContentDescriptor,
    #[error("persistent content descriptor names a forbidden content type")]
    ForbiddenContentDescriptor,
    #[error("persistent content descriptor is stale for its semantic layout")]
    StaleContentDescriptor,
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::PathBuf,
    };

    use tempfile::tempdir;

    use super::{
        MAXIMUM_DESCRIPTOR_ID_BYTES, MAXIMUM_VIRTUAL_KEY_BYTES, PersistedContentDescriptor,
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
                content_descriptors: Vec::new(),
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
    fn content_descriptors_are_bounded_strict_semantic_and_authority_free() {
        let mut state = state_with_horizontal_offsets();
        let descriptor = PersistedContentDescriptor {
            semantic_id: "text:0:0".into(),
            content_type: "builtin.text-buffer".into(),
            adapter: "text-buffer".into(),
            path: Some(PathBuf::from("/workspace/main.rs")),
            provider: None,
            source: None,
            key: None,
            desired_placement: PersistedWindowPlacement::Floating {
                x: 80,
                y: 12,
                width: 91,
                height: 37,
            },
        };
        state.session.as_mut().expect("session").content_descriptors = vec![descriptor.clone()];
        state
            .validate(&PersistencePolicy::default())
            .expect("valid semantic descriptor");
        let encoded = serde_json::to_string(&state).expect("encode descriptor");
        for forbidden in [
            "content_id",
            "view_id",
            "window_id",
            "generation",
            "capabilities",
            "callback",
            "pty",
            "process",
            "cells",
        ] {
            assert!(!encoded.contains(forbidden), "serialized {forbidden}");
        }

        let mut unknown_field = serde_json::to_value(&state).expect("value");
        unknown_field["session"]["content_descriptors"][0]["runtime_id"] = serde_json::json!(7);
        assert!(serde_json::from_value::<PersistedState>(unknown_field).is_err());

        let mut duplicate = state.clone();
        duplicate
            .session
            .as_mut()
            .expect("session")
            .content_descriptors
            .push(descriptor.clone());
        assert!(matches!(
            duplicate.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidContentDescriptor)
        ));

        let mut stale = state.clone();
        stale.session.as_mut().expect("session").content_descriptors[0].path =
            Some(PathBuf::from("/workspace/other.rs"));
        assert!(matches!(
            stale.validate(&PersistencePolicy::default()),
            Err(PersistenceError::StaleContentDescriptor)
        ));

        let mut oversized = state.clone();
        oversized
            .session
            .as_mut()
            .expect("session")
            .content_descriptors[0]
            .semantic_id = "x".repeat(MAXIMUM_DESCRIPTOR_ID_BYTES + 1);
        assert!(matches!(
            oversized.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidContentDescriptor)
        ));

        let mut invalid_placement = state.clone();
        invalid_placement
            .session
            .as_mut()
            .expect("session")
            .content_descriptors[0]
            .desired_placement = PersistedWindowPlacement::Floating {
            x: 0,
            y: 0,
            width: 0,
            height: 1,
        };
        assert!(matches!(
            invalid_placement.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidContentDescriptor)
        ));

        let mut unknown = state;
        unknown
            .session
            .as_mut()
            .expect("session")
            .content_descriptors[0]
            .adapter = "future-surface".into();
        assert!(matches!(
            unknown.validate(&PersistencePolicy::default()),
            Err(PersistenceError::UnknownContentDescriptor)
        ));
    }

    #[test]
    fn forbidden_and_unavailable_descriptors_are_distinguished_without_live_authority() {
        let descriptor = PersistedContentDescriptor {
            semantic_id: "virtual:0".into(),
            content_type: "builtin.virtual-document".into(),
            adapter: "virtual-document".into(),
            path: None,
            provider: Some("plugin.demo".into()),
            source: Some("plugin.demo.help".into()),
            key: Some("index".into()),
            desired_placement: PersistedWindowPlacement::Hidden,
        };
        let mut state = state_with_horizontal_offsets();
        state.session.as_mut().expect("session").tabs.clear();
        state
            .session
            .as_mut()
            .expect("session")
            .window_rules
            .clear();
        state.session.as_mut().expect("session").content_descriptors = vec![descriptor.clone()];
        state
            .validate(&PersistencePolicy::default())
            .expect("bounded unavailable provider descriptor");
        assert_eq!(
            state
                .session
                .as_ref()
                .expect("session")
                .unavailable_virtual_descriptors(),
            BTreeSet::from(["plugin.demo:plugin.demo.help:index".into()])
        );

        for (adapter, content_type) in [
            ("terminal-session", "builtin.terminal"),
            ("external-tui", "builtin.external-tui"),
            ("declarative-ui", "builtin.explorer"),
            ("declarative-ui", "plugin.demo.retained"),
            ("private-cell-surface", "plugin.demo.private"),
        ] {
            let mut forbidden = state.clone();
            let target = &mut forbidden
                .session
                .as_mut()
                .expect("session")
                .content_descriptors[0];
            target.adapter = adapter.into();
            target.content_type = content_type.into();
            target.path = None;
            assert!(matches!(
                forbidden.validate(&PersistencePolicy::default()),
                Err(PersistenceError::ForbiddenContentDescriptor)
            ));
        }

        let mut oversized_key = state.clone();
        oversized_key
            .session
            .as_mut()
            .expect("session")
            .content_descriptors[0]
            .key = Some("k".repeat(MAXIMUM_VIRTUAL_KEY_BYTES + 1));
        assert!(matches!(
            oversized_key.validate(&PersistencePolicy::default()),
            Err(PersistenceError::InvalidContentDescriptor)
        ));

        let mut too_many = state;
        too_many
            .session
            .as_mut()
            .expect("session")
            .content_descriptors
            .push(PersistedContentDescriptor {
                semantic_id: "virtual:1".into(),
                ..descriptor
            });
        assert!(matches!(
            too_many.validate(&PersistencePolicy {
                maximum_file_bytes: usize::MAX,
                maximum_entries: 1,
            }),
            Err(PersistenceError::TooManyEntries)
        ));
    }

    #[test]
    fn pre_v1_missing_unknown_and_oversized_scroll_columns_are_rejected() {
        let state = state_with_horizontal_offsets();
        let mut legacy_schema_2 = serde_json::to_value(&state).expect("serialize state");
        legacy_schema_2["session"]
            .as_object_mut()
            .expect("session")
            .remove("content_descriptors");
        let legacy: PersistedState =
            serde_json::from_value(legacy_schema_2).expect("legacy schema-2 state");
        legacy
            .validate(&PersistencePolicy::default())
            .expect("legacy descriptor omission");

        let mut unsupported = state.clone();
        unsupported.version = STATE_VERSION + 1;
        assert!(matches!(
            unsupported.validate(&PersistencePolicy::default()),
            Err(PersistenceError::UnsupportedVersion(_))
        ));

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
