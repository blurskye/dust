//! Plugin execution requests and owned worker results.

use crate::plugins::component::ComponentHostState;
use crate::plugins::component::component_api;
use crate::plugins::index::PluginError;
use crate::plugins::manifest::MAXIMUM_PLUGIN_EVENT_QUEUE;
use crate::plugins::manifest::PluginContributionIndex;
use crate::plugins::manifest::PluginLimits;
use crate::plugins::manifest::PluginManifest;
use crate::plugins::manifest::PluginSubscriptionKind;
use crate::plugins::providers::PluginPickerChunk;
use crate::plugins::providers::PluginPickerRequestEnvelope;
use crate::plugins::storage::PluginStorageRequest;
use crate::plugins::storage::PluginStorageResult;
use crate::plugins::virtual_documents::PluginVirtualDocumentProposal;
use crate::plugins::virtual_documents::PluginVirtualDocumentRequest;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum PluginStatus {
    Loading,
    Indexed,
    ValidatedUntilHost(String),
    Instantiated,
    Active(Vec<String>),
    Disabled,
    Failed(String),
}
#[derive(Clone, Debug)]
pub struct LoadedComponent {
    pub canonical_path: PathBuf,
    pub bytes: Vec<u8>,
    pub manifest: PluginManifest,
    pub instantiated: bool,
    pub unavailable_host: Option<String>,
    pub activated_commands: Vec<String>,
}
pub(crate) struct ActiveRuntime {
    pub(crate) store: wasmtime::Store<ComponentHostState>,
    pub(crate) bindings: component_api::EditorPlugin,
    pub(crate) commands: BTreeSet<String>,
    pub(crate) limits: PluginLimits,
}
pub(crate) struct LoadOutcome {
    pub(crate) loaded: LoadedComponent,
    pub(crate) runtime: ActiveRuntime,
}
#[derive(Clone, Debug)]
pub struct PluginSnapshot {
    pub buffer: u64,
    pub revision: u64,
    pub utf8: String,
}

#[derive(Debug)]
pub struct PluginExecutionRequest {
    pub call: crate::plugins::component::component_api::dust::plugin::host::CommandCall,
    pub delivery: crate::plugins::PluginDelivery,
    pub runtime_epoch: u64,
    pub plugin: String,
    pub command: String,
    pub arguments: Vec<String>,
    pub snapshot: Option<PluginSnapshot>,
    pub config: BTreeMap<String, String>,
    pub ui_revision: Option<u64>,
    pub confirmation_available: bool,
    pub ui_events: Vec<PluginUiEvent>,
    pub confirmation_results: Vec<PluginConfirmationResult>,
    pub host_events: Vec<PluginHostEventEnvelope>,
    pub storage_results: Vec<PluginStorageResult>,
    pub provider_requests: Vec<PluginPickerRequestEnvelope>,
    pub virtual_document_requests: Vec<PluginVirtualDocumentRequest>,
    pub request_id: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginConfirmationAnswer {
    Yes,
    No,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginConfirmationErrorCode {
    QueueFull,
    InvalidRequest,
    LoadUnavailable,
    StaleSource,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConfirmationError {
    pub code: PluginConfirmationErrorCode,
    pub detail: String,
}

impl PluginConfirmationError {
    pub fn new(code: PluginConfirmationErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: detail
                .to_string()
                .chars()
                .filter(|character| !character.is_control())
                .take(4_096)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConfirmationRequest {
    pub token: u64,
    pub title: String,
    pub message: String,
    pub default_answer: PluginConfirmationAnswer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginConfirmationOutcome {
    Answered(PluginConfirmationAnswer),
    Failed(PluginConfirmationError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConfirmationResult {
    pub request: u64,
    pub outcome: PluginConfirmationOutcome,
}

#[derive(Clone, Debug)]
pub struct PluginEditProposal {
    pub buffer: u64,
    pub revision: u64,
    pub edits: Vec<PluginEdit>,
}
#[derive(Clone, Debug)]
pub struct PluginEdit {
    pub start: u64,
    pub end: u64,
    pub replacement: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginUiNodeKind {
    Container,
    Text,
    List,
    Button,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginUiStyle {
    Default,
    Muted,
    Accent,
    Warning,
    Error,
    Selected,
    Border,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiNode {
    pub id: u64,
    pub kind: PluginUiNodeKind,
    pub text: String,
    pub style: PluginUiStyle,
    pub action: Option<String>,
    pub children: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiTree {
    pub revision: u64,
    pub root: u64,
    pub nodes: Vec<PluginUiNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPrivateStyle {
    pub id: u16,
    pub style: PluginUiStyle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPrivateCell {
    pub x: u16,
    pub y: u16,
    pub grapheme: String,
    pub continuation: bool,
    pub style: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginPrivateRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginPrivateCursor {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPrivateHitRegion {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub action: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPrivateSnapshot {
    pub revision: u64,
    pub dimensions_revision: u64,
    pub styles: Vec<PluginPrivateStyle>,
    pub cells: Vec<PluginPrivateCell>,
    pub cursor: Option<PluginPrivateCursor>,
    pub hit_regions: Vec<PluginPrivateHitRegion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPrivatePatch {
    pub revision: u64,
    pub expected_surface_revision: u64,
    pub dimensions_revision: u64,
    pub dirty: Vec<PluginPrivateRect>,
    pub styles: Vec<PluginPrivateStyle>,
    pub cells: Vec<PluginPrivateCell>,
    pub replace_cursor: bool,
    pub cursor: Option<PluginPrivateCursor>,
    pub replace_hit_regions: bool,
    pub hit_regions: Vec<PluginPrivateHitRegion>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginRequestedPlacement {
    DockedLeft {
        size: u16,
    },
    DockedRight {
        size: u16,
    },
    DockedTop {
        size: u16,
    },
    DockedBottom {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginUiRequestKind {
    OpenWindow {
        expected_graph_revision: u64,
        content_type: String,
        title: String,
        placement: PluginRequestedPlacement,
        tree: Option<PluginUiTree>,
    },
    ReplaceUi {
        handle: u64,
        expected_tree_revision: u64,
        tree: PluginUiTree,
    },
    PlaceWindow {
        handle: u64,
        expected_graph_revision: u64,
        placement: PluginRequestedPlacement,
    },
    FocusWindow {
        handle: u64,
        expected_graph_revision: u64,
    },
    CloseWindow {
        handle: u64,
        expected_graph_revision: u64,
    },
    ReplacePrivate {
        handle: u64,
        snapshot: PluginPrivateSnapshot,
    },
    PatchPrivate {
        handle: u64,
        patch: PluginPrivatePatch,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiRequest {
    pub token: u64,
    pub kind: PluginUiRequestKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginUiErrorCode {
    QueueFull,
    StaleGraph,
    StaleTree,
    StaleHandle,
    ForeignLoad,
    UndeclaredContent,
    UndeclaredAction,
    DisallowedPlacement,
    HiddenFocus,
    CrossTab,
    InstanceLimit,
    GraphLimit,
    InvalidTree,
    InvalidPlacement,
    CloseOrder,
    LoadUnavailable,
    InternalRejected,
    WrongPresenter,
    StaleSurface,
    StaleDimensions,
    InvalidLocalBounds,
    CellOverlap,
    SplitWideGrapheme,
    InvalidGrapheme,
    InvalidStyle,
    InvalidCursor,
    InvalidHitRegion,
    PatchLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiError {
    pub code: PluginUiErrorCode,
    pub detail: String,
}

impl From<String> for PluginUiError {
    fn from(detail: String) -> Self {
        Self::new(PluginUiErrorCode::InternalRejected, detail)
    }
}

impl From<&str> for PluginUiError {
    fn from(detail: &str) -> Self {
        Self::new(PluginUiErrorCode::InternalRejected, detail)
    }
}

impl PluginUiError {
    pub fn new(code: PluginUiErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: detail
                .to_string()
                .chars()
                .filter(|character| !character.is_control())
                .take(4_096)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSurfaceGeometry {
    pub handle: u64,
    pub width: u16,
    pub height: u16,
    pub dimensions_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiEvent {
    pub token: u64,
    pub handle: Option<u64>,
    pub graph_revision: Option<u64>,
    pub tree_revision: Option<u64>,
    pub surface_revision: Option<u64>,
    pub geometry: Option<PluginSurfaceGeometry>,
    pub action: Option<String>,
    pub error: Option<PluginUiError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginHostEventEnvelope {
    pub id: u64,
    pub sequence: u64,
    pub kind: PluginSubscriptionKind,
    pub revision: u64,
    pub payload: String,
    pub coalesced: u16,
    pub dropped: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PluginEventRegistration {
    subscriptions: BTreeMap<PluginSubscriptionKind, (u16, u16)>,
    observed: BTreeMap<PluginSubscriptionKind, (u64, String)>,
    events: VecDeque<PluginHostEventEnvelope>,
    last_delivery: BTreeMap<PluginSubscriptionKind, std::time::Instant>,
    next_id: u64,
    next_sequence: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginHostEventQueue {
    registrations: BTreeMap<(String, PluginLoadId), PluginEventRegistration>,
}

impl PluginHostEventQueue {
    pub fn install(&mut self, index: &PluginContributionIndex) -> Result<(), PluginError> {
        if index.subscriptions.is_empty() {
            return Ok(());
        }
        let key = (index.owner.clone(), PluginLoadId(index.plugin_load));
        if self.registrations.contains_key(&key) {
            return Err(PluginError::AlreadyActive);
        }
        let subscriptions = index
            .subscriptions
            .iter()
            .map(|subscription| {
                (
                    subscription.kind,
                    (
                        subscription.maximum_queued,
                        subscription.maximum_rate_per_second,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.registrations.insert(
            key,
            PluginEventRegistration {
                subscriptions,
                observed: BTreeMap::new(),
                events: VecDeque::new(),
                last_delivery: BTreeMap::new(),
                next_id: 1,
                next_sequence: 1,
            },
        );
        Ok(())
    }

    pub fn observe(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        kind: PluginSubscriptionKind,
        revision: u64,
        payload: impl Into<String>,
    ) -> Result<bool, PluginError> {
        let Some(registration) = self.registrations.get_mut(&(plugin.to_string(), load)) else {
            return Ok(false);
        };
        let Some((maximum, _)) = registration.subscriptions.get(&kind).copied() else {
            return Ok(false);
        };
        let payload = payload.into();
        if payload.len() > 4_096 || payload.chars().any(char::is_control) {
            return Err(PluginError::InvalidInvocation);
        }
        if registration
            .observed
            .get(&kind)
            .is_some_and(|current| current.0 == revision && current.1 == payload)
        {
            return Ok(false);
        }
        registration
            .observed
            .insert(kind, (revision, payload.clone()));
        if let Some(index) = registration
            .events
            .iter()
            .position(|event| event.kind == kind)
        {
            let mut existing = registration.events.remove(index).expect("queued event");
            existing.id = registration.next_id;
            registration.next_id = registration
                .next_id
                .checked_add(1)
                .ok_or(PluginError::InvalidInvocation)?;
            existing.sequence = registration.next_sequence;
            registration.next_sequence = registration
                .next_sequence
                .checked_add(1)
                .ok_or(PluginError::InvalidInvocation)?;
            existing.revision = revision;
            existing.payload = payload;
            existing.coalesced = existing.coalesced.saturating_add(1);
            registration.events.push_back(existing);
            return Ok(true);
        }
        let capacity = usize::from(maximum).min(MAXIMUM_PLUGIN_EVENT_QUEUE);
        let dropped = if registration
            .events
            .iter()
            .filter(|event| event.kind == kind)
            .count()
            >= capacity
        {
            if let Some(index) = registration
                .events
                .iter()
                .position(|event| event.kind == kind)
            {
                registration.events.remove(index);
            }
            1
        } else if registration.events.len() >= MAXIMUM_PLUGIN_EVENT_QUEUE {
            registration.events.pop_front();
            1
        } else {
            0
        };
        let id = registration.next_id;
        registration.next_id = registration
            .next_id
            .checked_add(1)
            .ok_or(PluginError::InvalidInvocation)?;
        let sequence = registration.next_sequence;
        registration.next_sequence = registration
            .next_sequence
            .checked_add(1)
            .ok_or(PluginError::InvalidInvocation)?;
        registration.events.push_back(PluginHostEventEnvelope {
            id,
            sequence,
            kind,
            revision,
            payload,
            coalesced: 0,
            dropped,
        });
        Ok(true)
    }

    #[must_use]
    pub fn deliver(&mut self, plugin: &str, load: PluginLoadId) -> Vec<PluginHostEventEnvelope> {
        let Some(registration) = self.registrations.get_mut(&(plugin.to_string(), load)) else {
            return Vec::new();
        };
        let now = crate::clock::monotonic();
        let events = registration
            .events
            .iter()
            .filter(|event| {
                let rate = registration
                    .subscriptions
                    .get(&event.kind)
                    .map_or(1, |(_, rate)| *rate);
                let interval = Duration::from_secs_f64(1.0 / f64::from(rate));
                registration
                    .last_delivery
                    .get(&event.kind)
                    .is_none_or(|last| now.saturating_duration_since(*last) >= interval)
            })
            .take(64)
            .cloned()
            .collect::<Vec<_>>();
        for event in &events {
            registration.last_delivery.insert(event.kind, now);
        }
        events
    }

    pub fn acknowledge(&mut self, plugin: &str, load: PluginLoadId, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let accepted = ids.iter().copied().take(64).collect::<BTreeSet<_>>();
        if let Some(registration) = self.registrations.get_mut(&(plugin.to_string(), load)) {
            registration
                .events
                .retain(|event| !accepted.contains(&event.id));
        }
    }

    pub fn remove_load(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        self.registrations
            .remove(&(plugin.to_string(), load))
            .is_some()
    }

    pub fn reset_runtime(&mut self) {
        for registration in self.registrations.values_mut() {
            registration.events.clear();
            registration.observed.clear();
            registration.last_delivery.clear();
        }
    }

    #[must_use]
    pub fn pending(&self, plugin: &str, load: PluginLoadId) -> usize {
        self.registrations
            .get(&(plugin.to_string(), load))
            .map_or(0, |registration| registration.events.len())
    }
}

#[derive(Clone, Debug)]
pub struct PluginExecution {
    pub(crate) error: Option<String>,
    pub(crate) editor_requests: Vec<crate::plugins::requests::EditorRequest>,
    pub(crate) value:
        Option<crate::plugins::component::component_api::dust::plugin::host::CommandValue>,
    pub(crate) call_next: Option<u64>,
    pub plugin: String,
    pub command: String,
    pub request_id: u64,
    pub result: String,
    pub messages: Vec<String>,
    pub proposals: Vec<PluginEditProposal>,
    pub ui_requests: Vec<PluginUiRequest>,
    pub confirmation_requests: Vec<PluginConfirmationRequest>,
    pub host_event_acknowledgements: Vec<u64>,
    pub storage_requests: Vec<PluginStorageRequest>,
    pub provider_chunks: Vec<PluginPickerChunk>,
    pub virtual_document_proposals: Vec<PluginVirtualDocumentProposal>,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginLoadId(pub u64);
