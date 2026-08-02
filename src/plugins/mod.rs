#![forbid(unsafe_code)]

mod providers;
mod storage;
mod virtual_documents;
pub use providers::{
    PROVIDER_ACTION_BYTE_CEILING, PROVIDER_CHUNK_ITEM_CEILING, PROVIDER_CONTEXT_BYTE_CEILING,
    PROVIDER_ITEM_ID_BYTE_CEILING, PROVIDER_PRIMARY_BYTE_CEILING, PROVIDER_QUERY_BYTE_CEILING,
    PROVIDER_REQUEST_QUEUE_CEILING, PROVIDER_SECONDARY_BYTE_CEILING, PluginPickerChunk,
    PluginPickerItem, PluginPickerProjection, PluginPickerRequestEnvelope, PluginProviderBroker,
    PluginProviderError, PluginProviderErrorCode, PluginProviderSessionStatus,
    PluginProviderTerminal,
};
pub use storage::{
    PluginStorageEntry, PluginStorageError, PluginStorageErrorCode, PluginStorageRequest,
    PluginStorageRequestKind, PluginStorageResult, PluginStorageService, STORAGE_KEY_BYTE_CEILING,
    STORAGE_LIST_BYTE_CEILING, STORAGE_LIST_ENTRY_CEILING, STORAGE_REQUEST_CEILING,
    STORAGE_RESULT_QUEUE_BYTE_CEILING, STORAGE_VALUE_BYTE_CEILING,
};
pub use virtual_documents::{
    PluginVirtualDocumentBroker, PluginVirtualDocumentError, PluginVirtualDocumentErrorCode,
    PluginVirtualDocumentFull, PluginVirtualDocumentPatch, PluginVirtualDocumentPatchSet,
    PluginVirtualDocumentProposal, PluginVirtualDocumentProposalKind, PluginVirtualDocumentRequest,
    PluginVirtualDocumentRequestKind, PluginVirtualDocumentSnapshot,
    VIRTUAL_DOCUMENT_KEY_BYTE_CEILING, VIRTUAL_DOCUMENT_LANGUAGE_BYTE_CEILING,
    VIRTUAL_DOCUMENT_REQUEST_QUEUE_CEILING, VIRTUAL_DOCUMENT_TITLE_BYTE_CEILING,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::Duration,
};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    ReadCurrentBuffer,
    ProposeEdits,
    ShowMessage,
    ReadConfig,
    PrivateCells,
    EventSubscriptions,
    PickerProviders,
    PluginStorage,
    VirtualDocuments,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContributions {
    #[serde(default = "plugin_contribution_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub input_contexts: Vec<PluginInputContextContribution>,
    #[serde(default)]
    pub keymaps: Vec<PluginKeymapContribution>,
    #[serde(default)]
    pub key_groups: Vec<PluginKeyGroupContribution>,
    #[serde(default)]
    pub content_types: Vec<PluginContentTypeContribution>,
    #[serde(default)]
    pub subscriptions: Vec<PluginSubscriptionContribution>,
    #[serde(default)]
    pub providers: Vec<PluginProviderContribution>,
    #[serde(default)]
    pub virtual_documents: Vec<PluginVirtualDocumentContribution>,
    #[serde(default)]
    pub storage: Option<PluginStorageContribution>,
}
const MAXIMUM_PRIVATE_CELLS_PER_CONTENT: u16 = 4_096;
const MAXIMUM_PRIVATE_PATCH_CELLS: u16 = 2_048;
const MAXIMUM_PRIVATE_DIRTY_RECTANGLES: u16 = 64;
const MAXIMUM_PRIVATE_STYLES: u16 = 32;
const MAXIMUM_PRIVATE_HIT_REGIONS: u16 = 64;
const MAXIMUM_PRIVATE_BYTES: u32 = 64 * 1024;
const MAXIMUM_PRIVATE_CELLS_PER_PLUGIN: usize = 8_192;
const MAXIMUM_PLUGIN_SUBSCRIPTIONS: usize = 32;
const MAXIMUM_PLUGIN_EVENT_QUEUE: usize = 128;
const MAXIMUM_PLUGIN_PROVIDERS: usize = 32;
const MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS: usize = 32;
const MAXIMUM_VIRTUAL_DOCUMENT_BYTES: u32 = 1024 * 1024;
const MAXIMUM_VIRTUAL_DOCUMENT_LINES: u32 = 100_000;
const MAXIMUM_VIRTUAL_PATCH_BYTES: u32 = 256 * 1024;
const MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_PROVIDER_ITEMS: u16 = 4_096;
const MAXIMUM_PROVIDER_BYTES: u32 = 1024 * 1024;
const MAXIMUM_STORAGE_ITEMS: u16 = 4_096;
const MAXIMUM_STORAGE_VALUE_BYTES: u32 = 256 * 1024;
const MAXIMUM_STORAGE_TOTAL_BYTES: u32 = 4 * 1024 * 1024;

const PLUGIN_UI_HOST_COMMANDS: &[&str] = &[
    "surface.retained.focus.next",
    "surface.retained.focus.previous",
    "surface.retained.scroll.down",
    "surface.retained.scroll.up",
    "surface.retained.activate",
    "surface.retained.mouse.select",
    "surface.private.activate",
    "surface.private.mouse.activate",
];

const fn plugin_contribution_schema() -> u32 {
    1
}
impl Default for PluginContributions {
    fn default() -> Self {
        Self {
            schema_version: plugin_contribution_schema(),
            input_contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInputContextContribution {
    pub id: String,
    pub title: String,
    pub parent_surface: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginKeymapContribution {
    pub id: String,
    pub context: String,
    #[serde(default)]
    pub mode: String,
    pub keys: String,
    pub command: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, toml::Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginKeyGroupContribution {
    pub id: String,
    pub context: String,
    #[serde(default)]
    pub mode: String,
    pub prefix: String,
    pub label: String,
    pub icon: Option<String>,
    #[serde(default)]
    pub order: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginWindowPlacementKind {
    Docked,
    Floating,
    Fullscreen,
    Hidden,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPresenterKind {
    RetainedDeclarative,
    PrivateCells,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPrivateCellPolicy {
    pub maximum_cells: u16,
    pub maximum_patch_cells: u16,
    pub maximum_dirty_rectangles: u16,
    pub maximum_styles: u16,
    pub maximum_hit_regions: u16,
    pub maximum_bytes: u32,
    #[serde(default)]
    pub cursor: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginSubscriptionKind {
    WorkspaceMetadataChanged,
    ActiveSurfaceChanged,
    BufferSummaryChanged,
    PluginSurfaceChanged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSubscriptionContribution {
    pub id: String,
    pub kind: PluginSubscriptionKind,
    pub maximum_queued: u16,
    pub maximum_rate_per_second: u16,
    #[serde(default = "default_subscription_coalescing")]
    pub coalesce: bool,
    #[serde(skip)]
    pub provenance: Option<PluginContributionProvenance>,
}

const fn default_subscription_coalescing() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginProviderKind {
    PickerSource,
    Preview,
    Action,
    Diagnostics,
    Completion,
    Hover,
    Decoration,
    Status,
    VirtualDocument,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginProviderContribution {
    pub id: String,
    pub kind: PluginProviderKind,
    pub command: String,
    pub maximum_concurrent_requests: u16,
    pub maximum_items: u16,
    pub maximum_bytes: u32,
    pub deadline_milliseconds: u32,
    #[serde(skip)]
    pub provenance: Option<PluginContributionProvenance>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub enum PluginVirtualDocumentMode {
    ReadOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginVirtualDocumentContribution {
    pub id: String,
    pub title: String,
    pub language: String,
    pub mode: PluginVirtualDocumentMode,
    pub maximum_concurrent_documents: u16,
    pub maximum_document_bytes: u32,
    pub maximum_document_lines: u32,
    pub maximum_patch_bytes: u32,
    pub maximum_patches_per_request: u16,
    pub deadline_milliseconds: u32,
    pub command: String,
    pub context: String,
    #[serde(skip)]
    pub provenance: Option<PluginContributionProvenance>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStorageContribution {
    pub maximum_items: u16,
    pub maximum_value_bytes: u32,
    pub maximum_total_bytes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginContributionProvenance {
    pub owner: String,
    pub generation: u64,
    pub source: String,
    pub declaration: usize,
    pub span: Option<std::ops::Range<usize>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContentTypeContribution {
    pub id: String,
    pub title: String,
    pub presenter: PluginPresenterKind,
    pub default_placement: PluginWindowPlacementKind,
    pub allowed_placements: BTreeSet<PluginWindowPlacementKind>,
    #[serde(default = "default_plugin_content_focusable")]
    pub focusable: bool,
    #[serde(default = "default_plugin_content_instances")]
    pub maximum_instances: u16,
    pub context: String,
    pub command: String,
    #[serde(default)]
    pub private_cells: Option<PluginPrivateCellPolicy>,
    #[serde(skip)]
    pub provenance: Option<PluginContributionProvenance>,
}

const fn default_plugin_content_focusable() -> bool {
    true
}

const fn default_plugin_content_instances() -> u16 {
    1
}
#[derive(Clone, Debug)]
pub struct PluginContributionIndex {
    pub owner: String,
    pub version: String,
    pub checksum: String,
    pub generation: u64,
    pub source: String,
    pub commands: BTreeMap<String, PluginCommandDeclaration>,
    pub contexts: Vec<PluginInputContextContribution>,
    pub keymaps: Vec<PluginKeymapContribution>,
    pub key_groups: Vec<PluginKeyGroupContribution>,
    pub content_types: Vec<PluginContentTypeContribution>,
    pub subscriptions: Vec<PluginSubscriptionContribution>,
    pub providers: Vec<PluginProviderContribution>,
    pub virtual_documents: Vec<PluginVirtualDocumentContribution>,
    pub storage: Option<PluginStorageContribution>,
}

#[derive(Clone, Debug, Default)]
pub struct PluginContributionCatalog {
    indexes: BTreeMap<String, PluginContributionIndex>,
}

impl PluginContributionCatalog {
    pub fn install(&mut self, index: PluginContributionIndex) -> Result<(), PluginError> {
        if self.indexes.contains_key(&index.owner) {
            return Err(PluginError::AlreadyActive);
        }
        for existing in self.indexes.values() {
            let mapping_conflict = existing.keymaps.iter().any(|left| {
                index.keymaps.iter().any(|right| {
                    left.context == right.context
                        && left.mode == right.mode
                        && left.keys == right.keys
                })
            });
            let group_conflict = existing.key_groups.iter().any(|left| {
                index.key_groups.iter().any(|right| {
                    left.context == right.context
                        && left.mode == right.mode
                        && left.prefix == right.prefix
                })
            });
            let content_conflict = existing
                .content_types
                .iter()
                .any(|left| index.content_types.iter().any(|right| left.id == right.id));
            if mapping_conflict || group_conflict || content_conflict {
                return Err(PluginError::ContributionConflict);
            }
        }
        self.indexes.insert(index.owner.clone(), index);
        Ok(())
    }

    pub fn remove_generation(&mut self, owner: &str, generation: u64) -> bool {
        if self
            .indexes
            .get(owner)
            .is_some_and(|index| index.generation == generation)
        {
            self.indexes.remove(owner);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn get(&self, owner: &str) -> Option<&PluginContributionIndex> {
        self.indexes.get(owner)
    }

    #[must_use]
    pub fn ordered(&self) -> impl ExactSizeIterator<Item = &PluginContributionIndex> {
        self.indexes.values()
    }

    pub fn context_snapshot(&self) -> Result<crate::input::InputContextRegistry, PluginError> {
        let mut contexts = crate::input::InputContextRegistry::bundled();
        for index in self.indexes.values() {
            let namespace = format!("plugin.{}.", index.owner);
            for context in &index.contexts {
                let local = context
                    .id
                    .strip_prefix(&namespace)
                    .ok_or(PluginError::InvalidManifest)?;
                contexts
                    .register_plugin(
                        &index.owner,
                        local,
                        &context.title,
                        context.parent_surface.clone(),
                        index.generation,
                        index.source.clone(),
                    )
                    .map_err(|_| PluginError::InvalidManifest)?;
            }
        }
        Ok(contexts)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginArgumentKind {
    String,
    Boolean,
    Integer,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginArgumentDeclaration {
    pub kind: PluginArgumentKind,
    #[serde(default)]
    pub required: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCommandContext {
    Application,
    Workspace,
    View,
    Buffer,
    Selection,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginActivationBehavior {
    ActiveOnly,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCommandDeclaration {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, PluginArgumentDeclaration>,
    pub context: PluginCommandContext,
    pub activation: PluginActivationBehavior,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub component_sha256: String,
    pub capabilities: BTreeSet<PluginCapability>,
    pub commands: BTreeMap<String, PluginCommandDeclaration>,
    pub contributions: PluginContributions,
}
#[derive(Clone, Debug)]
pub struct PluginLimits {
    pub maximum_component_bytes: usize,
    pub maximum_commands: usize,
    pub fuel: u64,
    pub epoch_deadline_ticks: u64,
    pub maximum_memory_bytes: usize,
    pub maximum_table_elements: u32,
    pub maximum_instances: u32,
}
impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            maximum_component_bytes: 64 * 1024 * 1024,
            maximum_commands: 256,
            fuel: 10_000_000,
            epoch_deadline_ticks: 10,
            maximum_memory_bytes: 64 * 1024 * 1024,
            maximum_table_elements: 10_000,
            maximum_instances: 16,
        }
    }
}
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
    store: wasmtime::Store<ComponentHostState>,
    bindings: component_api::EditorPlugin,
    commands: BTreeSet<String>,
    limits: PluginLimits,
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
    ForeignGeneration,
    UndeclaredContent,
    UndeclaredAction,
    DisallowedPlacement,
    HiddenFocus,
    CrossTab,
    InstanceCeiling,
    GraphCeiling,
    InvalidTree,
    InvalidPlacement,
    CloseOrder,
    GenerationLost,
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
    PatchCeiling,
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
struct PluginEventRegistration {
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
        let key = (index.owner.clone(), PluginLoadId(index.generation));
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
        if let Some(existing) = registration
            .events
            .iter_mut()
            .find(|event| event.kind == kind)
        {
            existing.revision = revision;
            existing.payload = payload;
            existing.coalesced = existing.coalesced.saturating_add(1);
            return Ok(true);
        }
        let capacity = usize::from(maximum).min(MAXIMUM_PLUGIN_EVENT_QUEUE);
        let dropped = if registration.events.len() >= capacity {
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

    pub fn remove_generation(&mut self, plugin: &str, load: PluginLoadId) -> bool {
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
    pub plugin: String,
    pub command: String,
    pub generation: u64,
    pub result: String,
    pub messages: Vec<String>,
    pub proposals: Vec<PluginEditProposal>,
    pub ui_requests: Vec<PluginUiRequest>,
    pub host_event_acknowledgements: Vec<u64>,
    pub storage_requests: Vec<PluginStorageRequest>,
    pub provider_chunks: Vec<PluginPickerChunk>,
    pub virtual_document_proposals: Vec<PluginVirtualDocumentProposal>,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginLoadId(pub u64);
#[derive(Debug)]
enum Request {
    IndexFromStore {
        load: PluginLoadId,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    },
    ActivateIndexed {
        load: PluginLoadId,
        plugin: String,
        checksum: String,
    },
    LoadFromStore {
        load: PluginLoadId,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    },
    Load {
        load: PluginLoadId,
        store: PathBuf,
        path: PathBuf,
        manifest: PluginManifest,
        limits: PluginLimits,
    },
    Execute {
        load: PluginLoadId,
        plugin: String,
        command: String,
        arguments: Vec<String>,
        snapshot: Option<PluginSnapshot>,
        config: BTreeMap<String, String>,
        ui_events: Vec<PluginUiEvent>,
        host_events: Vec<PluginHostEventEnvelope>,
        storage_results: Vec<PluginStorageResult>,
        provider_requests: Vec<PluginPickerRequestEnvelope>,
        virtual_document_requests: Vec<PluginVirtualDocumentRequest>,
        generation: u64,
    },
    Disable {
        load: PluginLoadId,
        plugin: String,
    },
}
#[derive(Debug)]
enum WorkerEvent {
    Indexed {
        load: PluginLoadId,
        plugin: String,
        index: PluginContributionIndex,
    },
    Loaded {
        load: PluginLoadId,
        component: LoadedComponent,
    },
    Executed {
        load: PluginLoadId,
        execution: PluginExecution,
    },
    Failed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
}
#[derive(Debug)]
pub enum PluginEvent {
    Indexed {
        load: PluginLoadId,
        plugin: String,
        index: PluginContributionIndex,
    },
    Loaded {
        load: PluginLoadId,
        component: LoadedComponent,
    },
    Executed(PluginExecution),
    Failed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
}
struct RuntimeSlot {
    load: PluginLoadId,
    runtime: ActiveRuntime,
}
struct IndexedCandidate {
    store: PathBuf,
    component_path: PathBuf,
    manifest: PluginManifest,
    limits: PluginLimits,
}
pub struct PluginManager {
    sender: SyncSender<Request>,
    receiver: Receiver<WorkerEvent>,
    pub statuses: BTreeMap<String, PluginStatus>,
    loads: BTreeMap<String, PluginLoadId>,
    next_load: u64,
    pending_disables: VecDeque<(String, PluginLoadId)>,
    maximum_loads: usize,
    stopped: bool,
}
impl PluginManager {
    pub fn start(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, 64);
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity);
        let (etx, erx) = mpsc::sync_channel(capacity);
        thread::Builder::new()
            .name("dust-components".into())
            .spawn(move || {
                let mut runtimes: BTreeMap<String, RuntimeSlot> = BTreeMap::new();
                let mut indexed: BTreeMap<(String, PluginLoadId), IndexedCandidate> =
                    BTreeMap::new();
                while let Ok(request) = rx.recv() {
                    let event = match request {
                        Request::IndexFromStore {
                            load,
                            expected_plugin,
                            store,
                            manifest_path,
                            component_path,
                            limits,
                        } => match index_manifest_from_store(
                            &store,
                            &manifest_path,
                            &component_path,
                            load,
                            &limits,
                        ) {
                            Ok((manifest, index, canonical_store, component_path))
                                if manifest.id == expected_plugin =>
                            {
                                indexed.insert(
                                    (expected_plugin.clone(), load),
                                    IndexedCandidate {
                                        store: canonical_store,
                                        component_path,
                                        manifest,
                                        limits,
                                    },
                                );
                                WorkerEvent::Indexed {
                                    load,
                                    plugin: expected_plugin,
                                    index,
                                }
                            }
                            Ok(_) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error: PluginError::InvalidManifest,
                            },
                            Err(error) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error,
                            },
                        },
                        Request::ActivateIndexed {
                            load,
                            plugin,
                            checksum,
                        } => match indexed.remove(&(plugin.clone(), load)) {
                            Some(candidate)
                                if candidate
                                    .manifest
                                    .component_sha256
                                    .eq_ignore_ascii_case(&checksum) =>
                            {
                                match load_component(
                                    &candidate.store,
                                    &candidate.store.join(&candidate.component_path),
                                    candidate.manifest,
                                    &candidate.limits,
                                ) {
                                    Ok(outcome) => {
                                        runtimes.insert(
                                            plugin,
                                            RuntimeSlot {
                                                load,
                                                runtime: outcome.runtime,
                                            },
                                        );
                                        WorkerEvent::Loaded {
                                            load,
                                            component: outcome.loaded,
                                        }
                                    }
                                    Err(error) => WorkerEvent::Failed {
                                        load,
                                        plugin,
                                        error,
                                    },
                                }
                            }
                            Some(_) => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::ChecksumMismatch,
                            },
                            None => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::IndexNotAccepted,
                            },
                        },
                        Request::Load {
                            load,
                            store,
                            path,
                            manifest,
                            limits,
                        } => {
                            let plugin = manifest.id.clone();
                            let prepared = (|| {
                                let store = fs::canonicalize(store).map_err(PluginError::Io)?;
                                if path.is_absolute() && !path.starts_with(&store) {
                                    return Err(PluginError::OutsideStore);
                                }
                                let index = index_manifest_contributions(
                                    &manifest,
                                    load.0,
                                    format!("<prepared-plugin:{plugin}>"),
                                )?;
                                Ok::<_, PluginError>((store, index))
                            })();
                            match prepared {
                                Ok((store, index)) => {
                                    indexed.insert(
                                        (plugin.clone(), load),
                                        IndexedCandidate {
                                            store,
                                            component_path: path,
                                            manifest,
                                            limits,
                                        },
                                    );
                                    WorkerEvent::Indexed {
                                        load,
                                        plugin,
                                        index,
                                    }
                                }
                                Err(error) => WorkerEvent::Failed {
                                    load,
                                    plugin,
                                    error,
                                },
                            }
                        }
                        Request::LoadFromStore {
                            load,
                            expected_plugin,
                            store,
                            manifest_path,
                            component_path,
                            limits,
                        } => match load_manifest_component(
                            &store,
                            &manifest_path,
                            &component_path,
                            &limits,
                        ) {
                            Ok(outcome) if outcome.loaded.manifest.id == expected_plugin => {
                                runtimes.insert(
                                    expected_plugin,
                                    RuntimeSlot {
                                        load,
                                        runtime: outcome.runtime,
                                    },
                                );
                                WorkerEvent::Loaded {
                                    load,
                                    component: outcome.loaded,
                                }
                            }
                            Ok(_) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error: PluginError::InvalidManifest,
                            },
                            Err((_, error)) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error,
                            },
                        },
                        Request::Execute {
                            load,
                            plugin,
                            command,
                            arguments,
                            snapshot,
                            config,
                            ui_events,
                            host_events,
                            storage_results,
                            provider_requests,
                            virtual_document_requests,
                            generation,
                        } => match runtimes.get_mut(&plugin).filter(|slot| slot.load == load) {
                            Some(slot) => execute_component(
                                &plugin,
                                &command,
                                arguments,
                                snapshot,
                                config,
                                ui_events,
                                host_events,
                                storage_results,
                                provider_requests,
                                virtual_document_requests,
                                generation,
                                &mut slot.runtime,
                            )
                            .map_or_else(
                                |error| {
                                    runtimes.remove(&plugin);
                                    WorkerEvent::Failed {
                                        load,
                                        plugin,
                                        error,
                                    }
                                },
                                |execution| WorkerEvent::Executed { load, execution },
                            ),
                            None => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::NotActive,
                            },
                        },
                        Request::Disable { load, plugin } => {
                            if runtimes.get(&plugin).is_some_and(|slot| slot.load == load) {
                                runtimes.remove(&plugin);
                            }
                            indexed.remove(&(plugin, load));
                            continue;
                        }
                    };
                    if etx.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("plugin loader");
        Self {
            sender: tx,
            receiver: erx,
            statuses: BTreeMap::new(),
            loads: BTreeMap::new(),
            next_load: 1,
            pending_disables: VecDeque::new(),
            maximum_loads: capacity,
            stopped: false,
        }
    }
    fn queue_disable(&mut self, plugin: String, load: PluginLoadId) {
        if !self
            .pending_disables
            .iter()
            .any(|(candidate, candidate_load)| candidate == &plugin && *candidate_load == load)
        {
            self.pending_disables.push_back((plugin, load));
        }
    }
    fn flush_pending_disables(&mut self) -> bool {
        while let Some((plugin, load)) = self.pending_disables.pop_front() {
            match self.sender.try_send(Request::Disable { load, plugin }) {
                Ok(()) => {}
                Err(TrySendError::Full(Request::Disable { load, plugin })) => {
                    self.pending_disables.push_front((plugin, load));
                    return false;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending_disables.clear();
                    self.stopped = true;
                    return true;
                }
                Err(TrySendError::Full(_)) => unreachable!("disable request changed variant"),
            }
        }
        true
    }
    fn require_disable_order(&mut self) -> Result<(), PluginError> {
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        self.flush_pending_disables();
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        if self.pending_disables.is_empty() {
            Ok(())
        } else {
            Err(PluginError::QueueFull)
        }
    }
    fn send_request(&mut self, request: Request) -> Result<(), PluginError> {
        match self.sender.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(PluginError::QueueFull),
            Err(TrySendError::Disconnected(_)) => {
                self.stopped = true;
                self.fail_stopped_loads();
                Err(PluginError::Stopped)
            }
        }
    }
    fn allocate_load(&mut self) -> Result<PluginLoadId, PluginError> {
        let load = PluginLoadId(self.next_load);
        self.next_load = self
            .next_load
            .checked_add(1)
            .ok_or(PluginError::InvalidInvocation)?;
        Ok(load)
    }
    pub fn index_from_store(
        &mut self,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.require_disable_order()?;
        if self.loads.contains_key(&expected_plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::IndexFromStore {
            load,
            expected_plugin: expected_plugin.clone(),
            store,
            manifest_path,
            component_path,
            limits,
        })?;
        self.loads.insert(expected_plugin.clone(), load);
        self.statuses.insert(expected_plugin, PluginStatus::Loading);
        Ok(load)
    }

    pub fn activate_indexed(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        checksum: String,
    ) -> Result<(), PluginError> {
        if self.loads.get(plugin) != Some(&load)
            || !matches!(self.statuses.get(plugin), Some(PluginStatus::Indexed))
        {
            return Err(PluginError::IndexNotAccepted);
        }
        self.send_request(Request::ActivateIndexed {
            load,
            plugin: plugin.into(),
            checksum,
        })?;
        self.statuses.insert(plugin.into(), PluginStatus::Loading);
        Ok(())
    }

    pub fn load_from_store(
        &mut self,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.require_disable_order()?;
        if self.loads.contains_key(&expected_plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::LoadFromStore {
            load,
            expected_plugin: expected_plugin.clone(),
            store,
            manifest_path,
            component_path,
            limits,
        })?;
        self.loads.insert(expected_plugin.clone(), load);
        self.statuses.insert(expected_plugin, PluginStatus::Loading);
        Ok(load)
    }
    pub fn load(
        &mut self,
        store: PathBuf,
        path: PathBuf,
        manifest: PluginManifest,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        validate_manifest(&manifest, &limits)?;
        let plugin = manifest.id.clone();
        self.require_disable_order()?;
        if self.loads.contains_key(&plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::Load {
            load,
            store,
            path,
            manifest,
            limits,
        })?;
        self.loads.insert(plugin.clone(), load);
        self.statuses.insert(plugin, PluginStatus::Loading);
        Ok(load)
    }
    pub fn execute(
        &mut self,
        plugin: &str,
        command: &str,
        arguments: Vec<String>,
        snapshot: Option<PluginSnapshot>,
        config: BTreeMap<String, String>,
        ui_events: Vec<PluginUiEvent>,
        host_events: Vec<PluginHostEventEnvelope>,
        storage_results: Vec<PluginStorageResult>,
        provider_requests: Vec<PluginPickerRequestEnvelope>,
        virtual_document_requests: Vec<PluginVirtualDocumentRequest>,
        generation: u64,
    ) -> Result<(), PluginError> {
        if arguments.len() > 256
            || arguments.iter().any(|argument| argument.len() > 65536)
            || config.len() > 1024
        {
            return Err(PluginError::InvalidInvocation);
        }
        self.require_disable_order()?;
        let load = self
            .loads
            .get(plugin)
            .copied()
            .ok_or(PluginError::NotActive)?;
        self.send_request(Request::Execute {
            load,
            plugin: plugin.into(),
            command: command.into(),
            arguments,
            snapshot,
            config,
            ui_events,
            host_events,
            storage_results,
            provider_requests,
            virtual_document_requests,
            generation,
        })
    }
    fn fail_stopped_loads(&mut self) {
        let ids = self.loads.keys().cloned().collect::<Vec<_>>();
        self.loads.clear();
        self.pending_disables.clear();
        for id in ids {
            self.statuses
                .insert(id, PluginStatus::Failed(PluginError::Stopped.to_string()));
        }
    }
    pub fn poll(&mut self) -> Result<Vec<PluginEvent>, PluginError> {
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        self.flush_pending_disables();
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    match event {
                        WorkerEvent::Indexed {
                            load,
                            plugin,
                            index,
                        } => {
                            if self.loads.get(&plugin) != Some(&load) {
                                self.queue_disable(plugin, load);
                                self.flush_pending_disables();
                                continue;
                            }
                            self.statuses.insert(plugin.clone(), PluginStatus::Indexed);
                            events.push(PluginEvent::Indexed {
                                load,
                                plugin,
                                index,
                            });
                        }
                        WorkerEvent::Loaded { load, component } => {
                            let plugin = component.manifest.id.clone();
                            if self.loads.get(&plugin) != Some(&load) {
                                self.queue_disable(plugin, load);
                                self.flush_pending_disables();
                                continue;
                            }
                            self.statuses.insert(plugin,if !component.activated_commands.is_empty(){PluginStatus::Active(component.activated_commands.clone())}else if component.instantiated{PluginStatus::Instantiated}else{PluginStatus::ValidatedUntilHost(component.unavailable_host.clone().unwrap_or_else(||"component imports require unavailable host bindings".into()))});
                            events.push(PluginEvent::Loaded { load, component })
                        }
                        WorkerEvent::Executed { load, execution } => {
                            if self.loads.get(&execution.plugin) == Some(&load) {
                                events.push(PluginEvent::Executed(execution));
                            }
                        }
                        WorkerEvent::Failed {
                            load,
                            plugin,
                            error,
                        } => {
                            if self.loads.get(&plugin) != Some(&load) {
                                continue;
                            }
                            self.loads.remove(&plugin);
                            self.statuses
                                .insert(plugin.clone(), PluginStatus::Failed(error.to_string()));
                            events.push(PluginEvent::Failed {
                                load,
                                plugin,
                                error,
                            });
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.flush_pending_disables();
                    return Ok(events);
                }
                Err(TryRecvError::Disconnected) => {
                    self.stopped = true;
                    self.fail_stopped_loads();
                    return Err(PluginError::Stopped);
                }
            }
        }
    }
    pub fn load_id(&self, id: &str) -> Option<PluginLoadId> {
        self.loads.get(id).copied()
    }
    #[must_use]
    pub fn is_active_generation(&self, id: &str, load: PluginLoadId) -> bool {
        self.loads.get(id) == Some(&load)
            && matches!(self.statuses.get(id), Some(PluginStatus::Active(_)))
    }
    pub fn unload_generation(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
        if self.loads.get(id) != Some(&load) {
            return Ok(());
        }
        self.loads.remove(id);
        self.statuses.remove(id);
        self.queue_disable(id.into(), load);
        self.flush_pending_disables();
        Ok(())
    }
    pub fn disable_generation(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
        if self.loads.get(id) != Some(&load) {
            return Ok(());
        }
        self.loads.remove(id);
        self.statuses.insert(id.into(), PluginStatus::Disabled);
        self.queue_disable(id.into(), load);
        self.flush_pending_disables();
        Ok(())
    }
    pub fn disable(&mut self, id: &str) -> Result<(), PluginError> {
        if let Some(load) = self.loads.remove(id) {
            self.queue_disable(id.into(), load);
            self.flush_pending_disables();
        }
        self.statuses.insert(id.into(), PluginStatus::Disabled);
        Ok(())
    }
    pub fn unload(&mut self, id: &str) -> Result<(), PluginError> {
        if let Some(load) = self.loads.remove(id) {
            self.queue_disable(id.into(), load);
            self.flush_pending_disables();
        }
        self.statuses.remove(id);
        Ok(())
    }
}
fn index_manifest_from_store(
    store: &Path,
    manifest_relative: &Path,
    component_relative: &Path,
    load: PluginLoadId,
    limits: &PluginLimits,
) -> Result<(PluginManifest, PluginContributionIndex, PathBuf, PathBuf), PluginError> {
    const MANIFEST_BYTE_CEILING: u64 = 1024 * 1024;
    if manifest_relative.is_absolute() || component_relative.is_absolute() {
        return Err(PluginError::OutsideStore);
    }
    let store = fs::canonicalize(store).map_err(PluginError::Io)?;
    let manifest_path = fs::canonicalize(store.join(manifest_relative)).map_err(PluginError::Io)?;
    if !manifest_path.starts_with(&store) {
        return Err(PluginError::OutsideStore);
    }
    let metadata = fs::symlink_metadata(&manifest_path).map_err(PluginError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > MANIFEST_BYTE_CEILING {
        return Err(PluginError::InvalidManifest);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&manifest_path)
        .map_err(PluginError::Io)?
        .take(MANIFEST_BYTE_CEILING + 1)
        .read_to_end(&mut bytes)
        .map_err(PluginError::Io)?;
    if bytes.len() as u64 > MANIFEST_BYTE_CEILING {
        return Err(PluginError::InvalidManifest);
    }
    let source = String::from_utf8(bytes).map_err(|_| PluginError::InvalidManifest)?;
    let manifest: PluginManifest =
        toml::from_str(&source).map_err(|_| PluginError::InvalidManifest)?;
    validate_manifest(&manifest, limits)?;
    let index = index_manifest_contributions_with_text(
        &manifest,
        load.0,
        manifest_path.display().to_string(),
        Some(&source),
    )?;
    Ok((manifest, index, store, component_relative.to_path_buf()))
}

fn load_manifest_component(
    store: &Path,
    manifest_relative: &Path,
    component_relative: &Path,
    limits: &PluginLimits,
) -> Result<LoadOutcome, (String, PluginError)> {
    let fallback = manifest_relative
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .chars()
        .take(256)
        .collect::<String>();
    let result = (|| {
        if manifest_relative.is_absolute() || component_relative.is_absolute() {
            return Err(PluginError::OutsideStore);
        }
        let store = fs::canonicalize(store).map_err(PluginError::Io)?;
        let manifest_path =
            fs::canonicalize(store.join(manifest_relative)).map_err(PluginError::Io)?;
        if !manifest_path.starts_with(&store) {
            return Err(PluginError::OutsideStore);
        }
        let metadata = fs::metadata(&manifest_path).map_err(PluginError::Io)?;
        if metadata.len() > 1024 * 1024 {
            return Err(PluginError::InvalidManifest);
        }
        let source = fs::read_to_string(manifest_path).map_err(PluginError::Io)?;
        let manifest: PluginManifest =
            toml::from_str(&source).map_err(|_| PluginError::InvalidManifest)?;
        validate_manifest(&manifest, limits)?;
        load_component(&store, &store.join(component_relative), manifest, limits)
    })();
    result.map_err(|error| (fallback, error))
}

fn valid_plugin_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_contribution_id(value: &str) -> bool {
    value.len() <= 128
        && value.split('.').count() >= 3
        && value.split('.').all(|segment| {
            segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_contributed_sequence(source: &str) -> bool {
    use std::str::FromStr;
    let tokens = source.split_whitespace().collect::<Vec<_>>();
    !tokens.is_empty()
        && source.len() <= 1_024
        && tokens.len() <= 64
        && !source.chars().any(char::is_control)
        && tokens.iter().all(|token| {
            matches!(*token, "<leader>" | "<localleader>")
                || crate::input::KeyEvent::from_str(token).is_ok()
        })
}

pub fn index_manifest_contributions(
    manifest: &PluginManifest,
    generation: u64,
    source: String,
) -> Result<PluginContributionIndex, PluginError> {
    index_manifest_contributions_with_text(manifest, generation, source, None)
}

fn index_manifest_contributions_with_text(
    manifest: &PluginManifest,
    generation: u64,
    source: String,
    source_text: Option<&str>,
) -> Result<PluginContributionIndex, PluginError> {
    if !valid_plugin_segment(&manifest.id)
        || manifest.version.is_empty()
        || manifest.version.len() > 128
        || manifest.version.chars().any(char::is_control)
        || manifest.component_sha256.len() != 64
        || !manifest
            .component_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || manifest.contributions.schema_version != 1
        || manifest.contributions.input_contexts.len() > 64
        || manifest.contributions.keymaps.len() > 256
        || manifest.contributions.key_groups.len() > 64
        || manifest.contributions.content_types.len() > 64
        || manifest.contributions.subscriptions.len() > MAXIMUM_PLUGIN_SUBSCRIPTIONS
        || manifest.contributions.providers.len() > MAXIMUM_PLUGIN_PROVIDERS
        || manifest.contributions.virtual_documents.len() > MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS
        || source.len() > 4_096
        || source.chars().any(char::is_control)
    {
        return Err(PluginError::InvalidManifest);
    }
    let namespace = format!("plugin.{}.", manifest.id);
    if manifest.commands.iter().any(|(id, declaration)| {
        !valid_plugin_segment(id)
            || declaration.title.trim().is_empty()
            || declaration.title.len() > 256
            || declaration.title.chars().any(char::is_control)
            || declaration.description.len() > 2_048
            || declaration.description.chars().any(char::is_control)
            || declaration.arguments.len() > 32
            || declaration
                .arguments
                .iter()
                .any(|(name, _)| !valid_plugin_segment(name))
    }) {
        return Err(PluginError::InvalidManifest);
    }
    let mut contexts = crate::input::InputContextRegistry::bundled();
    let mut ids = BTreeSet::new();
    let mut indexed_contexts = manifest.contributions.input_contexts.clone();
    indexed_contexts.sort_by(|left, right| left.id.cmp(&right.id));
    for context in &indexed_contexts {
        let local = context
            .id
            .strip_prefix(&namespace)
            .ok_or(PluginError::InvalidManifest)?;
        if !ids.insert(context.id.clone()) {
            return Err(PluginError::InvalidManifest);
        }
        contexts
            .register_plugin(
                &manifest.id,
                local,
                &context.title,
                context.parent_surface.clone(),
                generation,
                source.clone(),
            )
            .map_err(|_| PluginError::InvalidManifest)?;
    }
    let mut content_types = manifest.contributions.content_types.clone();
    content_types.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_private_cells = 0_usize;
    for (declaration, content) in content_types.iter_mut().enumerate() {
        content.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            generation,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&content.id)
                    .map(|start| start..start.saturating_add(content.id.len()))
            }),
        });
        let declared_command = content
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        let presenter_valid = match (&content.presenter, &content.private_cells) {
            (PluginPresenterKind::RetainedDeclarative, None) => true,
            (PluginPresenterKind::PrivateCells, Some(policy)) => {
                aggregate_private_cells =
                    aggregate_private_cells.saturating_add(usize::from(policy.maximum_cells));
                manifest
                    .capabilities
                    .contains(&PluginCapability::PrivateCells)
                    && (1..=MAXIMUM_PRIVATE_CELLS_PER_CONTENT).contains(&policy.maximum_cells)
                    && (1..=MAXIMUM_PRIVATE_PATCH_CELLS).contains(&policy.maximum_patch_cells)
                    && policy.maximum_patch_cells <= policy.maximum_cells
                    && (1..=MAXIMUM_PRIVATE_DIRTY_RECTANGLES)
                        .contains(&policy.maximum_dirty_rectangles)
                    && (1..=MAXIMUM_PRIVATE_STYLES).contains(&policy.maximum_styles)
                    && (1..=MAXIMUM_PRIVATE_HIT_REGIONS).contains(&policy.maximum_hit_regions)
                    && (1..=MAXIMUM_PRIVATE_BYTES).contains(&policy.maximum_bytes)
                    && aggregate_private_cells <= MAXIMUM_PRIVATE_CELLS_PER_PLUGIN
            }
            _ => false,
        };
        if !content.id.starts_with(&namespace)
            || !valid_contribution_id(&content.id)
            || !presenter_valid
            || !ids.insert(content.id.clone())
            || content.title.trim().is_empty()
            || content.title.len() > 256
            || content.title.chars().any(char::is_control)
            || content.allowed_placements.is_empty()
            || content.allowed_placements.len() > 4
            || !content
                .allowed_placements
                .contains(&content.default_placement)
            || !(1..=16).contains(&content.maximum_instances)
            || contexts.resolve(&content.context, "").is_err()
            || declared_command.is_none()
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    let mut subscriptions = manifest.contributions.subscriptions.clone();
    subscriptions.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_event_queue = 0_usize;
    for (declaration, subscription) in subscriptions.iter_mut().enumerate() {
        subscription.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            generation,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&subscription.id)
                    .map(|start| start..start.saturating_add(subscription.id.len()))
            }),
        });
        aggregate_event_queue = aggregate_event_queue
            .checked_add(usize::from(subscription.maximum_queued))
            .ok_or(PluginError::InvalidManifest)?;
        if !manifest
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
            || !subscription.id.starts_with(&namespace)
            || !valid_contribution_id(&subscription.id)
            || !ids.insert(subscription.id.clone())
            || !(1..=64).contains(&subscription.maximum_queued)
            || !(1..=120).contains(&subscription.maximum_rate_per_second)
            || !subscription.coalesce
            || aggregate_event_queue > MAXIMUM_PLUGIN_EVENT_QUEUE
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::EventSubscriptions)
        != !subscriptions.is_empty()
    {
        return Err(PluginError::InvalidManifest);
    }

    let mut providers = manifest.contributions.providers.clone();
    providers.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_provider_bytes = 0_u64;
    for (declaration, provider) in providers.iter_mut().enumerate() {
        provider.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            generation,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&provider.id)
                    .map(|start| start..start.saturating_add(provider.id.len()))
            }),
        });
        let command = provider
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        aggregate_provider_bytes = aggregate_provider_bytes
            .checked_add(u64::from(provider.maximum_bytes))
            .ok_or(PluginError::InvalidManifest)?;
        if provider.kind != PluginProviderKind::PickerSource
            || !manifest
                .capabilities
                .contains(&PluginCapability::PickerProviders)
            || !provider.id.starts_with(&namespace)
            || !valid_contribution_id(&provider.id)
            || !ids.insert(provider.id.clone())
            || command
                .is_none_or(|command| command.arguments.values().any(|argument| argument.required))
            || !(1..=8).contains(&provider.maximum_concurrent_requests)
            || !(1..=MAXIMUM_PROVIDER_ITEMS).contains(&provider.maximum_items)
            || !(1..=MAXIMUM_PROVIDER_BYTES).contains(&provider.maximum_bytes)
            || !(10..=30_000).contains(&provider.deadline_milliseconds)
            || aggregate_provider_bytes > u64::from(MAXIMUM_PROVIDER_BYTES) * 4
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::PickerProviders)
        != !providers.is_empty()
    {
        return Err(PluginError::InvalidManifest);
    }

    let mut virtual_documents = manifest.contributions.virtual_documents.clone();
    virtual_documents.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_virtual_bytes = 0_u64;
    for (declaration, document) in virtual_documents.iter_mut().enumerate() {
        document.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            generation,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&document.id)
                    .map(|start| start..start.saturating_add(document.id.len()))
            }),
        });
        let command = document
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        aggregate_virtual_bytes = aggregate_virtual_bytes
            .checked_add(u64::from(document.maximum_document_bytes))
            .ok_or(PluginError::InvalidManifest)?;
        if !manifest
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
            || !document.id.starts_with(&namespace)
            || !valid_contribution_id(&document.id)
            || !ids.insert(document.id.clone())
            || document.title.trim().is_empty()
            || document.title.len() > VIRTUAL_DOCUMENT_TITLE_BYTE_CEILING
            || document.title.chars().any(char::is_control)
            || document.language.is_empty()
            || document.language.len() > VIRTUAL_DOCUMENT_LANGUAGE_BYTE_CEILING
            || !document
                .language
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || document.mode != PluginVirtualDocumentMode::ReadOnly
            || !(1..=16).contains(&document.maximum_concurrent_documents)
            || !(1..=MAXIMUM_VIRTUAL_DOCUMENT_BYTES).contains(&document.maximum_document_bytes)
            || !(1..=MAXIMUM_VIRTUAL_DOCUMENT_LINES).contains(&document.maximum_document_lines)
            || !(1..=MAXIMUM_VIRTUAL_PATCH_BYTES).contains(&document.maximum_patch_bytes)
            || document.maximum_patch_bytes > document.maximum_document_bytes
            || !(1..=256).contains(&document.maximum_patches_per_request)
            || !(10..=30_000).contains(&document.deadline_milliseconds)
            || command
                .is_none_or(|command| command.arguments.values().any(|argument| argument.required))
            || contexts.resolve(&document.context, "").is_err()
            || aggregate_virtual_bytes > MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::VirtualDocuments)
        != !virtual_documents.is_empty()
    {
        return Err(PluginError::InvalidManifest);
    }

    let storage = manifest.contributions.storage.clone();
    let storage_valid = storage.as_ref().is_some_and(|policy| {
        manifest
            .capabilities
            .contains(&PluginCapability::PluginStorage)
            && (1..=MAXIMUM_STORAGE_ITEMS).contains(&policy.maximum_items)
            && (1..=MAXIMUM_STORAGE_VALUE_BYTES).contains(&policy.maximum_value_bytes)
            && policy.maximum_value_bytes <= policy.maximum_total_bytes
            && (1..=MAXIMUM_STORAGE_TOTAL_BYTES).contains(&policy.maximum_total_bytes)
    });
    if storage.is_some() != storage_valid
        || manifest
            .capabilities
            .contains(&PluginCapability::PluginStorage)
            != storage.is_some()
    {
        return Err(PluginError::InvalidManifest);
    }

    let mut keymaps = manifest.contributions.keymaps.clone();
    keymaps.sort_by(|left, right| left.id.cmp(&right.id));
    let mut map_sequences = BTreeSet::new();
    for keymap in &keymaps {
        let argument_bytes = keymap
            .arguments
            .iter()
            .try_fold(0usize, |total, (name, value)| {
                let value_bytes = match value {
                    toml::Value::String(value) if value.len() <= 1_024 => value.len(),
                    toml::Value::Integer(_) | toml::Value::Boolean(_) => 16,
                    _ => return None,
                };
                total.checked_add(name.len())?.checked_add(value_bytes)
            });
        let declared_command = keymap
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        let retained_host_command = PLUGIN_UI_HOST_COMMANDS.contains(&keymap.command.as_str());
        let arguments_match = (retained_host_command && keymap.arguments.is_empty())
            || declared_command.is_some_and(|command| {
                keymap.arguments.iter().all(|(name, value)| {
                    command.arguments.get(name).is_some_and(|field| {
                        matches!(
                            (field.kind, value),
                            (PluginArgumentKind::String, toml::Value::String(_))
                                | (PluginArgumentKind::Boolean, toml::Value::Boolean(_))
                                | (PluginArgumentKind::Integer, toml::Value::Integer(_))
                        )
                    })
                }) && command
                    .arguments
                    .iter()
                    .all(|(name, field)| !field.required || keymap.arguments.contains_key(name))
            });
        if !keymap.id.starts_with(&namespace)
            || !valid_contribution_id(&keymap.id)
            || !ids.insert(keymap.id.clone())
            || !arguments_match
            || !valid_contributed_sequence(&keymap.keys)
            || keymap.arguments.len() > 32
            || argument_bytes.is_none_or(|bytes| bytes > 4_096)
            || contexts.resolve(&keymap.context, &keymap.mode).is_err()
            || !map_sequences.insert((
                keymap.context.clone(),
                keymap.mode.clone(),
                keymap.keys.clone(),
            ))
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    let mut key_groups = manifest.contributions.key_groups.clone();
    key_groups.sort_by(|left, right| left.id.cmp(&right.id));
    let mut group_prefixes = BTreeSet::new();
    for group in &key_groups {
        if !group.id.starts_with(&namespace)
            || !valid_contribution_id(&group.id)
            || !ids.insert(group.id.clone())
            || !valid_contributed_sequence(&group.prefix)
            || group.label.is_empty()
            || group.label.len() > 256
            || group.label.chars().any(char::is_control)
            || group.icon.as_ref().is_some_and(|icon| {
                icon.is_empty()
                    || icon.len() > 32
                    || icon.chars().count() > 8
                    || icon.chars().any(char::is_control)
                    || unicode_width::UnicodeWidthStr::width(icon.as_str()) == 0
            })
            || contexts.resolve(&group.context, &group.mode).is_err()
            || !group_prefixes.insert((
                group.context.clone(),
                group.mode.clone(),
                group.prefix.clone(),
            ))
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    Ok(PluginContributionIndex {
        owner: manifest.id.clone(),
        version: manifest.version.clone(),
        checksum: manifest.component_sha256.clone(),
        generation,
        source,
        commands: manifest.commands.clone(),
        contexts: indexed_contexts,
        keymaps,
        key_groups,
        content_types,
        subscriptions,
        providers,
        virtual_documents,
        storage,
    })
}

fn validate_manifest(manifest: &PluginManifest, limits: &PluginLimits) -> Result<(), PluginError> {
    if manifest.id.is_empty()
        || manifest.id.len() > 256
        || manifest.version.is_empty()
        || manifest.component_sha256.len() != 64
        || manifest.commands.len() > limits.maximum_commands
        || manifest
            .commands
            .keys()
            .any(|command| command.is_empty() || command.len() > 256)
    {
        return Err(PluginError::InvalidManifest);
    }
    index_manifest_contributions(manifest, 0, "<manifest-validation>".into())?;
    Ok(())
}
pub fn activate_candidate(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<Vec<String>, PluginError> {
    let outcome = load_component(store, path, manifest, limits)?;
    Ok(outcome.loaded.activated_commands)
}

pub(crate) fn load_component(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<LoadOutcome, PluginError> {
    let store = fs::canonicalize(store).map_err(PluginError::Io)?;
    let path = fs::canonicalize(path).map_err(PluginError::Io)?;
    if !path.starts_with(&store) {
        return Err(PluginError::OutsideStore);
    }
    let metadata = fs::symlink_metadata(&path).map_err(PluginError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > limits.maximum_component_bytes as u64 {
        return Err(PluginError::InvalidComponent);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&path)
        .map_err(PluginError::Io)?
        .take(limits.maximum_component_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PluginError::Io)?;
    if bytes.len() > limits.maximum_component_bytes || !bytes.starts_with(b"\0asm") {
        return Err(PluginError::InvalidComponent);
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != manifest.component_sha256.to_ascii_lowercase() {
        return Err(PluginError::ChecksumMismatch);
    }
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let component = wasmtime::component::Component::from_binary(&engine, &bytes)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let store_limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(limits.maximum_memory_bytes)
        .table_elements(limits.maximum_table_elements as usize)
        .instances(limits.maximum_instances as usize)
        .build();
    let host = ComponentHostState {
        limits: store_limits,
        capabilities: manifest.capabilities.clone(),
        declared_content_types: manifest
            .contributions
            .content_types
            .iter()
            .map(|content| content.id.clone())
            .collect(),
        snapshot: None,
        proposals: Vec::new(),
        messages: Vec::new(),
        config: BTreeMap::new(),
        ui_requests: Vec::new(),
        ui_events: Vec::new(),
        next_ui_token: 1,
        host_events: Vec::new(),
        delivered_host_event_ids: BTreeSet::new(),
        host_event_acknowledgements: Vec::new(),
        storage_policy: manifest.contributions.storage.clone(),
        storage_requests: Vec::new(),
        storage_results: Vec::new(),
        next_storage_token: 1,
        provider_requests: Vec::new(),
        delivered_provider_requests: BTreeSet::new(),
        provider_chunks: Vec::new(),
        virtual_document_requests: Vec::new(),
        delivered_virtual_document_requests: BTreeSet::new(),
        virtual_document_proposals: Vec::new(),
    };
    let mut store = wasmtime::Store::new(&engine, host);
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.fuel)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    store.set_epoch_deadline(limits.epoch_deadline_ticks);
    let mut linker = wasmtime::component::Linker::new(&engine);
    let activation = (|| {
        component_api::EditorPlugin::add_to_linker(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;
        let bindings = component_api::EditorPlugin::instantiate(&mut store, &component, &linker)
            .map_err(|error| error.to_string())?;
        let commands = bindings
            .call_activate(&mut store)
            .map_err(|error| error.to_string())?
            .map_err(|error| error)?;
        Ok::<_, String>((bindings, commands))
    })();
    let (bindings, activated) = activation.map_err(PluginError::Activation)?;
    let activated_commands = activated
        .into_iter()
        .take(limits.maximum_commands)
        .map(|command| command.chars().take(256).collect::<String>())
        .collect::<Vec<_>>();
    if activated_commands
        .iter()
        .any(|command| !manifest.commands.contains_key(command))
    {
        return Err(PluginError::UndeclaredCommand);
    }
    let commands = activated_commands.iter().cloned().collect();
    let loaded = LoadedComponent {
        canonical_path: path,
        bytes,
        manifest,
        instantiated: true,
        unavailable_host: None,
        activated_commands,
    };
    Ok(LoadOutcome {
        loaded,
        runtime: ActiveRuntime {
            store,
            bindings,
            commands,
            limits: limits.clone(),
        },
    })
}
pub(crate) fn execute_component(
    plugin: &str,
    command: &str,
    arguments: Vec<String>,
    snapshot: Option<PluginSnapshot>,
    config: BTreeMap<String, String>,
    ui_events: Vec<PluginUiEvent>,
    host_events: Vec<PluginHostEventEnvelope>,
    storage_results: Vec<PluginStorageResult>,
    provider_requests: Vec<PluginPickerRequestEnvelope>,
    virtual_document_requests: Vec<PluginVirtualDocumentRequest>,
    generation: u64,
    runtime: &mut ActiveRuntime,
) -> Result<PluginExecution, PluginError> {
    if !runtime.commands.contains(command) {
        return Err(PluginError::UndeclaredCommand);
    }
    runtime
        .store
        .set_fuel(runtime.limits.fuel)
        .map_err(|error| PluginError::Execution(error.to_string()))?;
    runtime.store.set_epoch_deadline(1);
    let epoch_engine = runtime.store.engine().clone();
    let deadline = runtime
        .limits
        .epoch_deadline_ticks
        .max(1)
        .saturating_mul(10);
    let _ = thread::Builder::new()
        .name("dust-plugin-deadline".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(deadline));
            epoch_engine.increment_epoch();
        });
    {
        let state = runtime.store.data_mut();
        state.snapshot = snapshot.map(|snapshot| component_api::dust::plugin::host::Snapshot {
            buffer: snapshot.buffer,
            revision: snapshot.revision,
            utf8: snapshot.utf8,
        });
        state.config = config;
        state
            .ui_events
            .extend(ui_events.into_iter().take(64).map(|event| {
                component_api::dust::plugin::host::UiEvent {
                    token: event.token,
                    handle: event.handle,
                    graph_revision: event.graph_revision,
                    tree_revision: event.tree_revision,
                    surface_revision: event.surface_revision,
                    geometry: event.geometry.map(|geometry| {
                        component_api::dust::plugin::host::SurfaceGeometry {
                            handle: geometry.handle,
                            width: geometry.width,
                            height: geometry.height,
                            dimensions_revision: geometry.dimensions_revision,
                        }
                    }),
                    action: event.action,
                    error: event.error.map(component_ui_error),
                }
            }));
        if state.ui_events.len() > 64 {
            state
                .ui_events
                .drain(..state.ui_events.len().saturating_sub(64));
        }
        state.host_events = host_events
            .into_iter()
            .take(64)
            .map(|event| component_api::dust::plugin::host::HostEvent {
                id: event.id,
                sequence: event.sequence,
                kind: match event.kind {
                    PluginSubscriptionKind::WorkspaceMetadataChanged => {
                        component_api::dust::plugin::host::HostEventKind::WorkspaceMetadataChanged
                    }
                    PluginSubscriptionKind::ActiveSurfaceChanged => {
                        component_api::dust::plugin::host::HostEventKind::ActiveSurfaceChanged
                    }
                    PluginSubscriptionKind::BufferSummaryChanged => {
                        component_api::dust::plugin::host::HostEventKind::BufferSummaryChanged
                    }
                    PluginSubscriptionKind::PluginSurfaceChanged => {
                        component_api::dust::plugin::host::HostEventKind::PluginSurfaceChanged
                    }
                },
                revision: event.revision,
                payload: event.payload,
                coalesced: event.coalesced,
                dropped: event.dropped,
            })
            .collect();
        state.delivered_host_event_ids = state.host_events.iter().map(|event| event.id).collect();
        state.host_event_acknowledgements.clear();
        state.storage_results = storage_results
            .into_iter()
            .take(STORAGE_REQUEST_CEILING)
            .map(component_storage_result)
            .collect();
        state.storage_requests.clear();
        state.provider_requests = provider_requests
            .into_iter()
            .take(PROVIDER_REQUEST_QUEUE_CEILING)
            .map(component_provider_request)
            .collect();
        state.delivered_provider_requests = state
            .provider_requests
            .iter()
            .map(|request| request.id)
            .collect();
        state.provider_chunks.clear();
        state.virtual_document_requests = virtual_document_requests
            .into_iter()
            .take(VIRTUAL_DOCUMENT_REQUEST_QUEUE_CEILING)
            .map(component_virtual_document_request)
            .collect();
        state.delivered_virtual_document_requests = state
            .virtual_document_requests
            .iter()
            .map(|request| request.request)
            .collect();
        state.virtual_document_proposals.clear();
        state.proposals.clear();
        state.messages.clear();
        state.ui_requests.clear();
    }
    let result = runtime
        .bindings
        .call_execute_command(&mut runtime.store, command, &arguments)
        .map_err(|error| PluginError::Execution(error.to_string()))?
        .map_err(PluginError::Execution)?;
    if result.len() > 1024 * 1024 {
        return Err(PluginError::InvalidInvocation);
    }
    let state = runtime.store.data_mut();
    let messages = std::mem::take(&mut state.messages);
    let ui_requests = std::mem::take(&mut state.ui_requests);
    let host_event_acknowledgements = std::mem::take(&mut state.host_event_acknowledgements);
    let storage_requests = std::mem::take(&mut state.storage_requests);
    let provider_chunks = std::mem::take(&mut state.provider_chunks);
    let virtual_document_proposals = std::mem::take(&mut state.virtual_document_proposals);
    let proposals = std::mem::take(&mut state.proposals)
        .into_iter()
        .map(|(buffer, revision, edits)| PluginEditProposal {
            buffer,
            revision,
            edits: edits
                .into_iter()
                .map(|edit| PluginEdit {
                    start: edit.start,
                    end: edit.end,
                    replacement: edit.replacement,
                })
                .collect(),
        })
        .collect();
    Ok(PluginExecution {
        plugin: plugin.into(),
        command: command.into(),
        generation,
        result,
        messages,
        proposals,
        ui_requests,
        host_event_acknowledgements,
        storage_requests,
        provider_chunks,
        virtual_document_proposals,
    })
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("invalid plugin manifest")]
    InvalidManifest,
    #[error("invalid or oversized WebAssembly component")]
    InvalidComponent,
    #[error("component is outside immutable package store")]
    OutsideStore,
    #[error("component checksum does not match its manifest")]
    ChecksumMismatch,
    #[error("component validation failed: {0}")]
    Validation(String),
    #[error("plugin activation failed: {0}")]
    Activation(String),
    #[error("plugin execution failed: {0}")]
    Execution(String),
    #[error("plugin command was not declared")]
    UndeclaredCommand,
    #[error("plugin contribution conflicts with an installed plugin default")]
    ContributionConflict,
    #[error("plugin contribution index was not accepted for activation")]
    IndexNotAccepted,
    #[error("plugin invocation exceeded a bound")]
    InvalidInvocation,
    #[error("plugin already has a current load")]
    AlreadyActive,
    #[error("plugin active-load ceiling reached")]
    LoadLimit,
    #[error("plugin is not active")]
    NotActive,
    #[error("plugin loader queue is full")]
    QueueFull,
    #[error("plugin loader stopped")]
    Stopped,
    #[error("plugin I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    fn manager(sender: SyncSender<Request>, receiver: Receiver<WorkerEvent>) -> PluginManager {
        PluginManager {
            sender,
            receiver,
            statuses: BTreeMap::new(),
            loads: BTreeMap::new(),
            next_load: 1,
            pending_disables: VecDeque::new(),
            maximum_loads: 4,
            stopped: false,
        }
    }
    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "demo".into(),
            version: "1.0.0".into(),
            component_sha256: "0".repeat(64),
            capabilities: BTreeSet::new(),
            commands: [(
                "run".into(),
                PluginCommandDeclaration {
                    title: "Run".into(),
                    description: "Run demo".into(),
                    arguments: BTreeMap::new(),
                    context: PluginCommandContext::Buffer,
                    activation: PluginActivationBehavior::ActiveOnly,
                },
            )]
            .into_iter()
            .collect(),
            contributions: PluginContributions {
                schema_version: 1,
                input_contexts: vec![PluginInputContextContribution {
                    id: "plugin.demo.chat".into(),
                    title: "Chat".into(),
                    parent_surface: Some("panel".into()),
                }],
                keymaps: vec![PluginKeymapContribution {
                    id: "plugin.demo.keymap.open-chat".into(),
                    context: "plugin.demo.chat".into(),
                    mode: String::new(),
                    keys: "<leader> a".into(),
                    command: "plugin.demo.run".into(),
                    arguments: BTreeMap::new(),
                }],
                key_groups: vec![PluginKeyGroupContribution {
                    id: "plugin.demo.group.ai".into(),
                    context: "plugin.demo.chat".into(),
                    mode: String::new(),
                    prefix: "<leader>".into(),
                    label: "AI".into(),
                    icon: None,
                    order: 1,
                }],
                content_types: vec![PluginContentTypeContribution {
                    id: "plugin.demo.chat-panel".into(),
                    title: "Demo Chat".into(),
                    presenter: PluginPresenterKind::RetainedDeclarative,
                    default_placement: PluginWindowPlacementKind::Floating,
                    allowed_placements: [
                        PluginWindowPlacementKind::Floating,
                        PluginWindowPlacementKind::Docked,
                        PluginWindowPlacementKind::Hidden,
                    ]
                    .into_iter()
                    .collect(),
                    focusable: true,
                    maximum_instances: 1,
                    context: "plugin.demo.chat".into(),
                    command: "plugin.demo.run".into(),
                    private_cells: None,
                    provenance: None,
                }],
                subscriptions: Vec::new(),
                providers: Vec::new(),
                virtual_documents: Vec::new(),
                storage: None,
            },
        }
    }

    #[test]
    fn component_host_queues_declared_ui_requests_without_graph_authority() {
        use component_api::dust::plugin::host::{
            Host, OpenWindowRequest, UiNode, UiNodeKind, UiRequest, UiStyle, UiTree,
            WindowPlacement,
        };
        let mut host = ComponentHostState {
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            capabilities: BTreeSet::new(),
            declared_content_types: ["plugin.demo.chat-panel".into()].into_iter().collect(),
            snapshot: None,
            proposals: Vec::new(),
            messages: Vec::new(),
            config: BTreeMap::new(),
            ui_requests: Vec::new(),
            ui_events: Vec::new(),
            next_ui_token: 1,
            host_events: Vec::new(),
            delivered_host_event_ids: BTreeSet::new(),
            host_event_acknowledgements: Vec::new(),
            storage_policy: None,
            storage_requests: Vec::new(),
            storage_results: Vec::new(),
            next_storage_token: 1,
            provider_requests: Vec::new(),
            delivered_provider_requests: BTreeSet::new(),
            provider_chunks: Vec::new(),
            virtual_document_requests: Vec::new(),
            delivered_virtual_document_requests: BTreeSet::new(),
            virtual_document_proposals: Vec::new(),
        };
        let request = |content_type: &str| {
            UiRequest::OpenWindow(OpenWindowRequest {
                expected_graph_revision: 4,
                content_type: content_type.into(),
                title: "Demo".into(),
                placement: WindowPlacement::Floating,
                x: 2,
                y: 3,
                width: 30,
                height: 8,
                size: 1,
                tree: Some(UiTree {
                    revision: 1,
                    root: 1,
                    nodes: vec![UiNode {
                        id: 1,
                        kind: UiNodeKind::Text,
                        text: "hello".into(),
                        style: UiStyle::Accent,
                        action: None,
                        children: Vec::new(),
                    }],
                }),
            })
        };
        assert!(matches!(
            host.queue_ui_request(request("plugin.demo.chat-panel")),
            Ok(1)
        ));
        assert_eq!(host.ui_requests.len(), 1);
        assert!(
            host.queue_ui_request(request("plugin.other.stolen"))
                .is_err()
        );
        assert_eq!(host.ui_requests.len(), 1);
    }

    #[test]
    fn component_storage_queue_is_capability_key_and_count_bounded() {
        use component_api::dust::plugin::host::{Host, StorageRequest};
        let policy = PluginStorageContribution {
            maximum_items: 32,
            maximum_value_bytes: 1_024,
            maximum_total_bytes: 16_384,
        };
        let mut host = ComponentHostState {
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            capabilities: [
                PluginCapability::PluginStorage,
                PluginCapability::VirtualDocuments,
            ]
            .into_iter()
            .collect(),
            declared_content_types: BTreeSet::new(),
            snapshot: None,
            proposals: Vec::new(),
            messages: Vec::new(),
            config: BTreeMap::new(),
            ui_requests: Vec::new(),
            ui_events: Vec::new(),
            next_ui_token: 1,
            host_events: Vec::new(),
            delivered_host_event_ids: BTreeSet::new(),
            host_event_acknowledgements: Vec::new(),
            storage_policy: Some(policy),
            storage_requests: Vec::new(),
            storage_results: Vec::new(),
            next_storage_token: 1,
            provider_requests: Vec::new(),
            delivered_provider_requests: BTreeSet::new(),
            provider_chunks: Vec::new(),
            virtual_document_requests: Vec::new(),
            delivered_virtual_document_requests: BTreeSet::new(),
            virtual_document_proposals: Vec::new(),
        };
        assert_eq!(
            host.queue_storage_request(StorageRequest::Get("bad/key".into()))
                .unwrap_err()
                .code,
            component_api::dust::plugin::host::StorageErrorCode::InvalidKey
        );
        for _ in 0..STORAGE_REQUEST_CEILING {
            host.queue_storage_request(StorageRequest::Get("valid".into()))
                .expect("bounded storage request");
        }
        assert_eq!(
            host.queue_storage_request(StorageRequest::Get("valid".into()))
                .unwrap_err()
                .code,
            component_api::dust::plugin::host::StorageErrorCode::QueueFull
        );
        assert_eq!(host.storage_requests.len(), STORAGE_REQUEST_CEILING);
        use component_api::dust::plugin::host::{
            VirtualDocumentProposal, VirtualDocumentProposalKind,
        };
        assert_eq!(
            host.queue_virtual_document_proposal(VirtualDocumentProposal {
                request: 9,
                document: 4,
                cancellation_generation: 1,
                kind: VirtualDocumentProposalKind::CloseAcknowledged
            })
            .unwrap_err()
            .code,
            component_api::dust::plugin::host::VirtualDocumentErrorCode::UnknownRequest
        );
        assert!(host.virtual_document_proposals.is_empty());
    }

    #[test]
    fn static_manifest_contribution_index_is_namespaced_deterministic_and_pure() {
        let manifest = manifest();
        let index = index_manifest_contributions(&manifest, 9, "manifest.toml".into())
            .expect("static index");
        assert_eq!(index.owner, "demo");
        assert_eq!(index.generation, 9);
        assert_eq!(index.contexts[0].id, "plugin.demo.chat");
        assert_eq!(index.keymaps[0].id, "plugin.demo.keymap.open-chat");
        assert_eq!(index.content_types[0].id, "plugin.demo.chat-panel");
        let mut stolen = manifest;
        stolen.contributions.keymaps[0].id = "plugin.other.keymap.stolen".into();
        assert!(matches!(
            index_manifest_contributions(&stolen, 10, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
    }

    fn renamed_manifest(owner: &str) -> PluginManifest {
        let mut manifest = manifest();
        manifest.id = owner.into();
        for context in &mut manifest.contributions.input_contexts {
            context.id = context
                .id
                .replace("plugin.demo.", &format!("plugin.{owner}."));
        }
        for keymap in &mut manifest.contributions.keymaps {
            keymap.id = keymap
                .id
                .replace("plugin.demo.", &format!("plugin.{owner}."));
            keymap.context = keymap
                .context
                .replace("plugin.demo.", &format!("plugin.{owner}."));
            keymap.command = keymap
                .command
                .replace("plugin.demo.", &format!("plugin.{owner}."));
        }
        for group in &mut manifest.contributions.key_groups {
            group.id = group
                .id
                .replace("plugin.demo.", &format!("plugin.{owner}."));
            group.context = group
                .context
                .replace("plugin.demo.", &format!("plugin.{owner}."));
        }
        for content in &mut manifest.contributions.content_types {
            content.id = content
                .id
                .replace("plugin.demo.", &format!("plugin.{owner}."));
            content.context = content
                .context
                .replace("plugin.demo.", &format!("plugin.{owner}."));
            content.command = content
                .command
                .replace("plugin.demo.", &format!("plugin.{owner}."));
        }
        manifest
    }

    #[test]
    fn contribution_catalog_orders_plugins_and_removes_exact_generations() {
        let demo = index_manifest_contributions(&manifest(), 3, "demo.toml".into()).expect("demo");
        let mut other_manifest = renamed_manifest("other");
        other_manifest.contributions.keymaps[0].keys = "<leader> b".into();
        other_manifest.contributions.key_groups[0].prefix = "<leader>".into();
        let other =
            index_manifest_contributions(&other_manifest, 4, "other.toml".into()).expect("other");
        let mut catalog = PluginContributionCatalog::default();
        catalog.install(other).expect("other first");
        catalog.install(demo).expect("demo second");
        assert_eq!(
            catalog
                .ordered()
                .map(|index| index.owner.as_str())
                .collect::<Vec<_>>(),
            vec!["demo", "other"]
        );
        assert!(catalog.context_snapshot().is_ok());
        assert!(!catalog.remove_generation("demo", 2));
        assert!(catalog.get("demo").is_some());
        assert!(catalog.remove_generation("demo", 3));
        assert!(catalog.get("demo").is_none());

        let mut demo_manifest = manifest();
        demo_manifest.contributions.keymaps[0].context = "editor".into();
        demo_manifest.contributions.keymaps[0].mode = "normal".into();
        let demo = index_manifest_contributions(&demo_manifest, 5, "demo.toml".into())
            .expect("editor demo");
        let mut other_manifest = renamed_manifest("other");
        other_manifest.contributions.keymaps[0].context = "editor".into();
        other_manifest.contributions.keymaps[0].mode = "normal".into();
        let other = index_manifest_contributions(&other_manifest, 6, "other.toml".into())
            .expect("editor other");
        let mut conflicts = PluginContributionCatalog::default();
        conflicts.install(demo).expect("first default");
        assert!(matches!(
            conflicts.install(other),
            Err(PluginError::ContributionConflict)
        ));
    }

    #[test]
    fn index_worker_does_not_touch_component_and_requires_host_acceptance() {
        let root = tempfile::tempdir().expect("store");
        let manifest_source = toml::to_string(&manifest()).expect("serialize manifest");
        fs::write(root.path().join("manifest.toml"), manifest_source).expect("manifest");
        let mut manager = PluginManager::start(4);
        let load = manager
            .index_from_store(
                "demo".into(),
                root.path().to_path_buf(),
                PathBuf::from("manifest.toml"),
                PathBuf::from("missing-component.wasm"),
                PluginLimits::default(),
            )
            .expect("queue index");
        assert!(matches!(
            manager.activate_indexed("demo", load, "0".repeat(64)),
            Err(PluginError::IndexNotAccepted)
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let index = loop {
            let events = manager.poll().expect("poll");
            if let Some(index) = events.into_iter().find_map(|event| match event {
                PluginEvent::Indexed { index, .. } => Some(index),
                _ => None,
            }) {
                break index;
            }
            assert!(std::time::Instant::now() < deadline, "index timed out");
            std::thread::yield_now();
        };
        assert_eq!(index.owner, "demo");
        let provenance = index.content_types[0]
            .provenance
            .as_ref()
            .expect("content provenance");
        assert_eq!(provenance.owner, "demo");
        assert_eq!(provenance.generation, load.0);
        assert!(provenance.span.is_some());
        manager
            .activate_indexed("demo", load, index.checksum)
            .expect("accept index");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let events = manager.poll().expect("poll activation");
            if events.into_iter().any(|event| {
                matches!(event, PluginEvent::Failed { load: candidate, .. } if candidate == load)
            }) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "activation timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn hostile_static_contributions_are_rejected_before_component_access() {
        let absent = format!(
            "id='demo'\nversion='1'\ncomponent_sha256='{}'\ncapabilities=[]\n[commands]\n'plugin.demo.run'='Run'\n",
            "0".repeat(64)
        );
        assert!(toml::from_str::<PluginManifest>(&absent).is_err());

        let mut cases = Vec::new();
        let mut candidate = manifest();
        candidate.contributions.schema_version = 2;
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.id = "BAD".into();
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.commands = [("Stolen".into(), candidate.commands["run"].clone())]
            .into_iter()
            .collect();
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.keymaps[0].command = "plugin.demo.missing".into();
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.keymaps[0].context = "plugin.demo.missing".into();
        cases.push(candidate);
        let mut candidate = manifest();
        let mut duplicate = candidate.contributions.keymaps[0].clone();
        duplicate.id = "plugin.demo.keymap.duplicate".into();
        candidate.contributions.keymaps.push(duplicate);
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.keymaps[0]
            .arguments
            .insert("nested".into(), toml::Value::Array(vec![]));
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.key_groups[0].icon = Some("\u{0301}".into());
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.content_types[0].id = "plugin.other.stolen".into();
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.content_types[0].command = "plugin.demo.missing".into();
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.content_types[0].maximum_instances = 17;
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.content_types[0]
            .allowed_placements
            .remove(&PluginWindowPlacementKind::Floating);
        cases.push(candidate);
        let mut candidate = manifest();
        candidate.contributions.input_contexts = (0..65)
            .map(|index| PluginInputContextContribution {
                id: format!("plugin.demo.c{index}"),
                title: format!("Context {index}"),
                parent_surface: Some("panel".into()),
            })
            .collect();
        cases.push(candidate);
        let mut private = manifest();
        private.capabilities.insert(PluginCapability::PrivateCells);
        private.contributions.content_types[0].presenter = PluginPresenterKind::PrivateCells;
        private.contributions.content_types[0].private_cells = Some(PluginPrivateCellPolicy {
            maximum_cells: 256,
            maximum_patch_cells: 128,
            maximum_dirty_rectangles: 8,
            maximum_styles: 8,
            maximum_hit_regions: 8,
            maximum_bytes: 8_192,
            cursor: true,
        });
        assert!(index_manifest_contributions(&private, 1, "manifest.toml".into()).is_ok());
        let mut no_capability = private.clone();
        no_capability
            .capabilities
            .remove(&PluginCapability::PrivateCells);
        cases.push(no_capability);
        let mut excessive = private.clone();
        excessive.contributions.content_types[0]
            .private_cells
            .as_mut()
            .expect("policy")
            .maximum_cells = 4_097;
        cases.push(excessive);
        let mut mismatched = manifest();
        mismatched.contributions.content_types[0].private_cells =
            private.contributions.content_types[0].private_cells.clone();
        cases.push(mismatched);
        for candidate in cases {
            assert!(matches!(
                index_manifest_contributions(&candidate, 1, "manifest.toml".into()),
                Err(PluginError::InvalidManifest)
            ));
        }
        let mut retained = manifest();
        retained.contributions.keymaps[0].command = "surface.retained.activate".into();
        assert!(index_manifest_contributions(&retained, 1, "manifest.toml".into()).is_ok());
        retained.contributions.keymaps[0].command = "health.open".into();
        assert!(matches!(
            index_manifest_contributions(&retained, 1, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
        assert!(matches!(
            index_manifest_contributions(&manifest(), 1, "bad\nsource".into()),
            Err(PluginError::InvalidManifest)
        ));
    }

    #[test]
    fn static_events_providers_and_storage_are_capability_gated_and_bounded() {
        let mut candidate = manifest();
        candidate.capabilities.extend([
            PluginCapability::EventSubscriptions,
            PluginCapability::PickerProviders,
            PluginCapability::PluginStorage,
            PluginCapability::VirtualDocuments,
        ]);
        candidate.contributions.subscriptions = vec![PluginSubscriptionContribution {
            id: "plugin.demo.subscription.workspace".into(),
            kind: PluginSubscriptionKind::WorkspaceMetadataChanged,
            maximum_queued: 8,
            maximum_rate_per_second: 30,
            coalesce: true,
            provenance: None,
        }];
        candidate.contributions.providers = vec![PluginProviderContribution {
            id: "plugin.demo.provider.files".into(),
            kind: PluginProviderKind::PickerSource,
            command: "plugin.demo.run".into(),
            maximum_concurrent_requests: 2,
            maximum_items: 128,
            maximum_bytes: 16_384,
            deadline_milliseconds: 500,
            provenance: None,
        }];
        candidate.contributions.virtual_documents = vec![PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 2,
            maximum_document_bytes: 16_384,
            maximum_document_lines: 512,
            maximum_patch_bytes: 4_096,
            maximum_patches_per_request: 16,
            deadline_milliseconds: 500,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
        candidate.contributions.storage = Some(PluginStorageContribution {
            maximum_items: 32,
            maximum_value_bytes: 1_024,
            maximum_total_bytes: 16_384,
        });
        let indexed = index_manifest_contributions(&candidate, 7, "manifest.toml".into())
            .expect("bounded static extension declarations");
        assert_eq!(
            indexed.subscriptions[0]
                .provenance
                .as_ref()
                .unwrap()
                .generation,
            7
        );
        assert_eq!(indexed.providers[0].kind, PluginProviderKind::PickerSource);
        assert!(indexed.storage.is_some());
        assert_eq!(indexed.virtual_documents[0].language, "markdown");

        let mut unsupported = candidate.clone();
        unsupported.contributions.providers[0].kind = PluginProviderKind::VirtualDocument;
        assert!(matches!(
            index_manifest_contributions(&unsupported, 7, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
        let mut no_capability = candidate.clone();
        no_capability
            .capabilities
            .remove(&PluginCapability::EventSubscriptions);
        assert!(matches!(
            index_manifest_contributions(&no_capability, 7, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
        let mut excessive = candidate;
        excessive
            .contributions
            .storage
            .as_mut()
            .unwrap()
            .maximum_total_bytes = MAXIMUM_STORAGE_TOTAL_BYTES + 1;
        assert!(matches!(
            index_manifest_contributions(&excessive, 7, "manifest.toml".into()),
            Err(PluginError::InvalidManifest)
        ));
    }

    #[test]
    fn host_events_coalesce_acknowledge_and_remove_exact_generation() {
        let mut candidate = manifest();
        candidate
            .capabilities
            .insert(PluginCapability::EventSubscriptions);
        candidate.contributions.subscriptions = vec![
            PluginSubscriptionContribution {
                id: "plugin.demo.subscription.workspace".into(),
                kind: PluginSubscriptionKind::WorkspaceMetadataChanged,
                maximum_queued: 1,
                maximum_rate_per_second: 30,
                coalesce: true,
                provenance: None,
            },
            PluginSubscriptionContribution {
                id: "plugin.demo.subscription.surface".into(),
                kind: PluginSubscriptionKind::ActiveSurfaceChanged,
                maximum_queued: 1,
                maximum_rate_per_second: 30,
                coalesce: true,
                provenance: None,
            },
        ];
        let index = index_manifest_contributions(&candidate, 9, "manifest.toml".into())
            .expect("event index");
        let mut queue = PluginHostEventQueue::default();
        queue.install(&index).expect("install");
        assert!(
            queue
                .observe(
                    "demo",
                    PluginLoadId(9),
                    PluginSubscriptionKind::WorkspaceMetadataChanged,
                    1,
                    "roots=1"
                )
                .unwrap()
        );
        assert!(
            !queue
                .observe(
                    "demo",
                    PluginLoadId(9),
                    PluginSubscriptionKind::WorkspaceMetadataChanged,
                    1,
                    "roots=1"
                )
                .unwrap()
        );
        assert!(
            queue
                .observe(
                    "demo",
                    PluginLoadId(9),
                    PluginSubscriptionKind::WorkspaceMetadataChanged,
                    2,
                    "roots=2"
                )
                .unwrap()
        );
        let event = queue.deliver("demo", PluginLoadId(9)).pop().unwrap();
        assert_eq!((event.revision, event.coalesced), (2, 1));
        queue.acknowledge("demo", PluginLoadId(8), &[event.id]);
        assert_eq!(queue.pending("demo", PluginLoadId(9)), 1);
        queue.acknowledge("demo", PluginLoadId(9), &[event.id]);
        assert_eq!(queue.pending("demo", PluginLoadId(9)), 0);
        let stable = queue.clone();
        for _ in 0..256 {
            assert!(
                !queue
                    .observe(
                        "demo",
                        PluginLoadId(9),
                        PluginSubscriptionKind::WorkspaceMetadataChanged,
                        2,
                        "roots=2"
                    )
                    .unwrap()
            );
        }
        assert_eq!(queue, stable);
        assert!(!queue.remove_generation("demo", PluginLoadId(8)));
        assert!(queue.remove_generation("demo", PluginLoadId(9)));
    }

    #[test]
    fn failed_enqueue_does_not_publish_loading() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::Stopped)
        ));
        assert!(!manager.statuses.contains_key("demo"));
        assert!(manager.load_id("demo").is_none());
        assert!(manager.stopped);
    }
    #[test]
    fn stale_failure_and_unload_cannot_replace_current_generation() {
        let (tx, _rx) = mpsc::sync_channel(4);
        let (etx, erx) = mpsc::sync_channel(4);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(2));
        manager
            .statuses
            .insert("demo".into(), PluginStatus::Loading);
        etx.send(WorkerEvent::Failed {
            load: PluginLoadId(1),
            plugin: "demo".into(),
            error: PluginError::NotActive,
        })
        .unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
    }
    #[test]
    fn active_load_ceiling_bounds_disable_ownership() {
        let (tx, _rx) = mpsc::sync_channel(4);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        manager.maximum_loads = 1;
        manager.loads.insert("one".into(), PluginLoadId(1));
        assert!(matches!(
            manager.load_from_store(
                "two".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::LoadLimit)
        ));
        manager.unload_generation("one", PluginLoadId(1)).unwrap();
        assert!(manager.pending_disables.len() <= manager.maximum_loads);
    }
    #[test]
    fn queue_full_cancellation_stays_pending_and_suppresses_events() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(Request::Disable {
            load: PluginLoadId(9),
            plugin: "occupied".into(),
        })
        .unwrap();
        let (etx, erx) = mpsc::sync_channel(2);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(1));
        manager
            .statuses
            .insert("demo".into(), PluginStatus::Loading);
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        assert_eq!(manager.pending_disables.len(), 1);
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::QueueFull)
        ));
        etx.send(WorkerEvent::Failed {
            load: PluginLoadId(1),
            plugin: "demo".into(),
            error: PluginError::NotActive,
        })
        .unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert_eq!(manager.pending_disables.len(), 1);
        let _ = rx.recv().unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert!(manager.pending_disables.is_empty());
        assert!(
            matches!(rx.recv().unwrap(),Request::Disable{load:PluginLoadId(1),plugin}if plugin=="demo")
        );
    }
    #[test]
    fn disable_is_ordered_before_same_id_replacement_and_duplicates_are_rejected() {
        let (tx, rx) = mpsc::sync_channel(4);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(1));
        manager.next_load = 2;
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::AlreadyActive)
        ));
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        let replacement = manager
            .load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default(),
            )
            .unwrap();
        assert_ne!(replacement, PluginLoadId(1));
        assert!(matches!(
            rx.recv().unwrap(),
            Request::Disable {
                load: PluginLoadId(1),
                ..
            }
        ));
        assert!(matches!(rx.recv().unwrap(),Request::LoadFromStore{load,..}if load==replacement));
    }
}

pub mod component_api {
    wasmtime::component::bindgen!({ path: "wit", world: "editor-plugin" });
}

pub struct ComponentHostState {
    pub limits: wasmtime::StoreLimits,
    pub capabilities: BTreeSet<PluginCapability>,
    pub declared_content_types: BTreeSet<String>,
    pub snapshot: Option<component_api::dust::plugin::host::Snapshot>,
    pub proposals: Vec<(u64, u64, Vec<component_api::dust::plugin::host::Edit>)>,
    pub messages: Vec<String>,
    pub config: BTreeMap<String, String>,
    pub ui_requests: Vec<PluginUiRequest>,
    pub ui_events: Vec<component_api::dust::plugin::host::UiEvent>,
    pub next_ui_token: u64,
    pub host_events: Vec<component_api::dust::plugin::host::HostEvent>,
    pub delivered_host_event_ids: BTreeSet<u64>,
    pub host_event_acknowledgements: Vec<u64>,
    pub storage_policy: Option<PluginStorageContribution>,
    pub storage_requests: Vec<PluginStorageRequest>,
    pub storage_results: Vec<component_api::dust::plugin::host::StorageResult>,
    pub next_storage_token: u64,
    pub provider_requests: Vec<component_api::dust::plugin::host::PickerProviderRequest>,
    pub delivered_provider_requests: BTreeSet<u64>,
    pub provider_chunks: Vec<PluginPickerChunk>,
    pub virtual_document_requests: Vec<component_api::dust::plugin::host::VirtualDocumentRequest>,
    pub delivered_virtual_document_requests: BTreeSet<u64>,
    pub virtual_document_proposals: Vec<PluginVirtualDocumentProposal>,
}
impl component_api::dust::plugin::host::Host for ComponentHostState {
    fn current_buffer(&mut self) -> Result<component_api::dust::plugin::host::Snapshot, String> {
        if self
            .capabilities
            .contains(&PluginCapability::ReadCurrentBuffer)
        {
            self.snapshot
                .clone()
                .ok_or_else(|| "no current buffer snapshot".into())
        } else {
            Err("capability denied".into())
        }
    }
    fn propose_edits(
        &mut self,
        buffer: u64,
        revision: u64,
        edits: Vec<component_api::dust::plugin::host::Edit>,
    ) -> Result<u64, String> {
        if !self.capabilities.contains(&PluginCapability::ProposeEdits) {
            return Err("capability denied".into());
        }
        if edits.len() > 4096 {
            return Err("edit ceiling exceeded".into());
        }
        if edits
            .iter()
            .any(|edit| edit.start > edit.end || edit.replacement.len() > 1024 * 1024)
        {
            return Err("invalid edit proposal".into());
        }
        self.proposals.push((buffer, revision, edits));
        Ok(self.proposals.len() as u64)
    }
    fn show_message(&mut self, message: String) -> Result<(), String> {
        if !self.capabilities.contains(&PluginCapability::ShowMessage) {
            return Err("capability denied".into());
        }
        self.messages.push(
            message
                .chars()
                .filter(|character| !character.is_control())
                .take(4096)
                .collect(),
        );
        if self.messages.len() > 64 {
            self.messages.remove(0);
        }
        Ok(())
    }
    fn config_get(&mut self, key: String) -> Option<String> {
        self.capabilities
            .contains(&PluginCapability::ReadConfig)
            .then(|| self.config.get(&key).cloned())
            .flatten()
    }

    fn queue_ui_request(
        &mut self,
        request: component_api::dust::plugin::host::UiRequest,
    ) -> Result<u64, component_api::dust::plugin::host::UiError> {
        if self.ui_requests.len() >= 64 {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::QueueFull,
                "UI request ceiling exceeded",
            )));
        }
        let token = self.next_ui_token;
        self.next_ui_token = self.next_ui_token.checked_add(1).ok_or_else(|| {
            component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "UI request token exhausted",
            ))
        })?;
        let kind = convert_component_ui_request(request, &self.declared_content_types)
            .map_err(component_ui_error)?;
        self.ui_requests.push(PluginUiRequest { token, kind });
        Ok(token)
    }

    fn take_ui_events(&mut self) -> Vec<component_api::dust::plugin::host::UiEvent> {
        std::mem::take(&mut self.ui_events)
    }

    fn take_host_events(&mut self) -> Vec<component_api::dust::plugin::host::HostEvent> {
        if self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            std::mem::take(&mut self.host_events)
        } else {
            Vec::new()
        }
    }

    fn acknowledge_host_events(&mut self, ids: Vec<u64>) -> Result<(), String> {
        if !self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            return Err("capability denied".into());
        }
        if ids.len() > 64 {
            return Err("host event acknowledgement ceiling exceeded".into());
        }
        let mut unique = BTreeSet::new();
        for id in ids {
            if id == 0 || !self.delivered_host_event_ids.contains(&id) || !unique.insert(id) {
                return Err("invalid host event acknowledgement".into());
            }
        }
        self.host_event_acknowledgements.extend(unique);
        Ok(())
    }

    fn queue_storage_request(
        &mut self,
        request: component_api::dust::plugin::host::StorageRequest,
    ) -> Result<u64, component_api::dust::plugin::host::StorageError> {
        if !self.capabilities.contains(&PluginCapability::PluginStorage) {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.storage_requests.len() >= STORAGE_REQUEST_CEILING {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::QueueFull,
                "storage request ceiling exceeded",
            )));
        }
        let token = self.next_storage_token;
        self.next_storage_token = self.next_storage_token.checked_add(1).ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage token exhausted",
            ))
        })?;
        let kind = convert_component_storage_request(request);
        let candidate = PluginStorageRequest { token, kind };
        let policy = self.storage_policy.as_ref().ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage policy is unavailable",
            ))
        })?;
        storage::validate_request(&candidate, policy).map_err(component_storage_error)?;
        self.storage_requests.push(candidate);
        Ok(token)
    }

    fn take_storage_results(&mut self) -> Vec<component_api::dust::plugin::host::StorageResult> {
        if self.capabilities.contains(&PluginCapability::PluginStorage) {
            std::mem::take(&mut self.storage_results)
        } else {
            Vec::new()
        }
    }

    fn take_picker_provider_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::PickerProviderRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            std::mem::take(&mut self.provider_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_picker_provider_chunk(
        &mut self,
        chunk: component_api::dust::plugin::host::PickerProviderChunk,
    ) -> Result<(), component_api::dust::plugin::host::ProviderError> {
        if !self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.provider_chunks.len() >= PROVIDER_REQUEST_QUEUE_CEILING {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::QueueFull,
                "provider chunk queue is full",
            )));
        }
        if !self.delivered_provider_requests.contains(&chunk.request) {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::UnknownRequest,
                "provider request was not delivered in this execution",
            )));
        }
        let terminal_is_error = matches!(
            chunk.terminal.kind,
            component_api::dust::plugin::host::ProviderTerminalKind::Error
        );
        if terminal_is_error != chunk.terminal.error.is_some() {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Malformed,
                "provider terminal/error payload mismatch",
            )));
        }
        let chunk = convert_component_provider_chunk(chunk);
        providers::validate_chunk(&chunk).map_err(component_provider_error)?;
        self.provider_chunks.push(chunk);
        Ok(())
    }

    fn take_virtual_document_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::VirtualDocumentRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            std::mem::take(&mut self.virtual_document_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_virtual_document_proposal(
        &mut self,
        proposal: component_api::dust::plugin::host::VirtualDocumentProposal,
    ) -> Result<(), component_api::dust::plugin::host::VirtualDocumentError> {
        if !self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ReadOnly,
                    "capability denied",
                ),
            ));
        }
        if self.virtual_document_proposals.len() >= VIRTUAL_DOCUMENT_REQUEST_QUEUE_CEILING {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::QueueFull,
                    "proposal queue full",
                ),
            ));
        }
        if !self
            .delivered_virtual_document_requests
            .contains(&proposal.request)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::UnknownRequest,
                    "request was not delivered in this execution",
                ),
            ));
        }
        let proposal = convert_component_virtual_document_proposal(proposal)
            .map_err(component_virtual_document_error)?;
        self.virtual_document_proposals.push(proposal);
        Ok(())
    }
}

fn component_virtual_document_request(
    request: PluginVirtualDocumentRequest,
) -> component_api::dust::plugin::host::VirtualDocumentRequest {
    use component_api::dust::plugin::host::VirtualDocumentRequestKind as K;
    component_api::dust::plugin::host::VirtualDocumentRequest {
        request: request.request,
        document: request.document,
        source: request.source,
        key: request.key,
        kind: match request.kind {
            PluginVirtualDocumentRequestKind::Open => K::Open,
            PluginVirtualDocumentRequestKind::Refresh => K::Refresh,
            PluginVirtualDocumentRequestKind::CloseNotice => K::CloseNotice,
        },
        current_revision: request.current_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_generation: request.cancellation_generation,
        maximum_document_bytes: request.maximum_document_bytes,
        maximum_document_lines: request.maximum_document_lines,
        maximum_patch_bytes: request.maximum_patch_bytes,
        maximum_patches: request.maximum_patches,
    }
}
fn convert_component_virtual_document_proposal(
    p: component_api::dust::plugin::host::VirtualDocumentProposal,
) -> Result<PluginVirtualDocumentProposal, PluginVirtualDocumentError> {
    use component_api::dust::plugin::host::VirtualDocumentProposalKind as K;
    let kind = match p.kind {
        K::Full(f) => {
            if f.title.len() > VIRTUAL_DOCUMENT_TITLE_BYTE_CEILING
                || f.language.len() > VIRTUAL_DOCUMENT_LANGUAGE_BYTE_CEILING
                || f.text.len() > MAXIMUM_VIRTUAL_DOCUMENT_BYTES as usize
            {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ByteCeiling,
                    "virtual document full proposal exceeds absolute ceiling",
                ));
            }
            PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                expected_revision: f.expected_revision,
                revision: f.revision,
                title: f.title,
                language: f.language,
                text: f.text,
            })
        }
        K::Patches(s) => {
            if s.patches.len() > 256 {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::PatchCeiling,
                    "virtual document patch count exceeds absolute ceiling",
                ));
            }
            let mut bytes = 0usize;
            let mut patches = Vec::with_capacity(s.patches.len());
            for p in s.patches {
                bytes = bytes.checked_add(p.replacement.len()).ok_or_else(|| {
                    PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchCeiling,
                        "patch byte overflow",
                    )
                })?;
                if bytes > MAXIMUM_VIRTUAL_PATCH_BYTES as usize {
                    return Err(PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchCeiling,
                        "patch bytes exceed absolute ceiling",
                    ));
                }
                patches.push(PluginVirtualDocumentPatch {
                    start: p.start,
                    end: p.end,
                    replacement: p.replacement,
                });
            }
            PluginVirtualDocumentProposalKind::Patches(PluginVirtualDocumentPatchSet {
                expected_revision: s.expected_revision,
                revision: s.revision,
                patches,
            })
        }
        K::CloseAcknowledged => PluginVirtualDocumentProposalKind::CloseAcknowledged,
        K::Error(e) => PluginVirtualDocumentProposalKind::Error(internal_virtual_document_error(e)),
    };
    Ok(PluginVirtualDocumentProposal {
        request: p.request,
        document: p.document,
        cancellation_generation: p.cancellation_generation,
        kind,
    })
}
fn internal_virtual_document_error(
    e: component_api::dust::plugin::host::VirtualDocumentError,
) -> PluginVirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    PluginVirtualDocumentError::new(
        match e.code {
            C::QueueFull => PluginVirtualDocumentErrorCode::QueueFull,
            C::UnknownSource => PluginVirtualDocumentErrorCode::UnknownSource,
            C::UnknownRequest => PluginVirtualDocumentErrorCode::UnknownRequest,
            C::UnknownDocument => PluginVirtualDocumentErrorCode::UnknownDocument,
            C::StaleGeneration => PluginVirtualDocumentErrorCode::StaleGeneration,
            C::StaleRevision => PluginVirtualDocumentErrorCode::StaleRevision,
            C::Cancelled => PluginVirtualDocumentErrorCode::Cancelled,
            C::Expired => PluginVirtualDocumentErrorCode::Expired,
            C::InvalidTitle => PluginVirtualDocumentErrorCode::InvalidTitle,
            C::InvalidLanguage => PluginVirtualDocumentErrorCode::InvalidLanguage,
            C::InvalidUtf8Range => PluginVirtualDocumentErrorCode::InvalidUtf8Range,
            C::InvalidControl => PluginVirtualDocumentErrorCode::InvalidControl,
            C::ByteCeiling => PluginVirtualDocumentErrorCode::ByteCeiling,
            C::LineCeiling => PluginVirtualDocumentErrorCode::LineCeiling,
            C::PatchCeiling => PluginVirtualDocumentErrorCode::PatchCeiling,
            C::ReadOnly => PluginVirtualDocumentErrorCode::ReadOnly,
            C::Malformed => PluginVirtualDocumentErrorCode::Malformed,
            C::Internal => PluginVirtualDocumentErrorCode::Internal,
        },
        e.detail,
    )
}
fn component_virtual_document_error(
    e: PluginVirtualDocumentError,
) -> component_api::dust::plugin::host::VirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    component_api::dust::plugin::host::VirtualDocumentError {
        code: match e.code {
            PluginVirtualDocumentErrorCode::QueueFull => C::QueueFull,
            PluginVirtualDocumentErrorCode::UnknownSource => C::UnknownSource,
            PluginVirtualDocumentErrorCode::UnknownRequest => C::UnknownRequest,
            PluginVirtualDocumentErrorCode::UnknownDocument => C::UnknownDocument,
            PluginVirtualDocumentErrorCode::StaleGeneration => C::StaleGeneration,
            PluginVirtualDocumentErrorCode::StaleRevision => C::StaleRevision,
            PluginVirtualDocumentErrorCode::Cancelled => C::Cancelled,
            PluginVirtualDocumentErrorCode::Expired => C::Expired,
            PluginVirtualDocumentErrorCode::InvalidTitle => C::InvalidTitle,
            PluginVirtualDocumentErrorCode::InvalidLanguage => C::InvalidLanguage,
            PluginVirtualDocumentErrorCode::InvalidUtf8Range => C::InvalidUtf8Range,
            PluginVirtualDocumentErrorCode::InvalidControl => C::InvalidControl,
            PluginVirtualDocumentErrorCode::ByteCeiling => C::ByteCeiling,
            PluginVirtualDocumentErrorCode::LineCeiling => C::LineCeiling,
            PluginVirtualDocumentErrorCode::PatchCeiling => C::PatchCeiling,
            PluginVirtualDocumentErrorCode::ReadOnly => C::ReadOnly,
            PluginVirtualDocumentErrorCode::Malformed => C::Malformed,
            PluginVirtualDocumentErrorCode::Internal => C::Internal,
        },
        detail: e.detail,
    }
}

fn component_provider_request(
    request: PluginPickerRequestEnvelope,
) -> component_api::dust::plugin::host::PickerProviderRequest {
    component_api::dust::plugin::host::PickerProviderRequest {
        id: request.id,
        provider: request.provider,
        query: request.query,
        context: request.context,
        workspace_revision: request.workspace_revision,
        buffer_revision: request.buffer_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_generation: request.cancellation_generation,
        maximum_items: request.maximum_items,
        maximum_bytes: request.maximum_bytes,
    }
}

fn convert_component_provider_chunk(
    chunk: component_api::dust::plugin::host::PickerProviderChunk,
) -> PluginPickerChunk {
    use component_api::dust::plugin::host::ProviderTerminalKind;
    let terminal = match chunk.terminal.kind {
        ProviderTerminalKind::More => PluginProviderTerminal::More,
        ProviderTerminalKind::Complete => PluginProviderTerminal::Complete,
        ProviderTerminalKind::Truncated => PluginProviderTerminal::Truncated,
        ProviderTerminalKind::Error => PluginProviderTerminal::Error(
            chunk
                .terminal
                .error
                .map(internal_provider_error)
                .unwrap_or_else(|| {
                    PluginProviderError::new(
                        PluginProviderErrorCode::Malformed,
                        "provider error terminal omitted its error",
                    )
                }),
        ),
    };
    PluginPickerChunk {
        request: chunk.request,
        sequence: chunk.sequence,
        items: chunk
            .items
            .into_iter()
            .map(|item| PluginPickerItem {
                id: item.id,
                primary: item.primary,
                secondary: item.secondary,
                action_data: item.action_data,
            })
            .collect(),
        terminal,
    }
}

fn internal_provider_error(
    error: component_api::dust::plugin::host::ProviderError,
) -> PluginProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    PluginProviderError::new(
        match error.code {
            ProviderErrorCode::QueueFull => PluginProviderErrorCode::QueueFull,
            ProviderErrorCode::UnknownRequest => PluginProviderErrorCode::UnknownRequest,
            ProviderErrorCode::UnknownProvider => PluginProviderErrorCode::UnknownProvider,
            ProviderErrorCode::StaleGeneration => PluginProviderErrorCode::StaleGeneration,
            ProviderErrorCode::StaleRevision => PluginProviderErrorCode::StaleRevision,
            ProviderErrorCode::Cancelled => PluginProviderErrorCode::Cancelled,
            ProviderErrorCode::Expired => PluginProviderErrorCode::Expired,
            ProviderErrorCode::InvalidSequence => PluginProviderErrorCode::InvalidSequence,
            ProviderErrorCode::DuplicateItem => PluginProviderErrorCode::DuplicateItem,
            ProviderErrorCode::ItemCeiling => PluginProviderErrorCode::ItemCeiling,
            ProviderErrorCode::ByteCeiling => PluginProviderErrorCode::ByteCeiling,
            ProviderErrorCode::Malformed => PluginProviderErrorCode::Malformed,
            ProviderErrorCode::Unavailable => PluginProviderErrorCode::Unavailable,
            ProviderErrorCode::Internal => PluginProviderErrorCode::Internal,
        },
        error.detail,
    )
}

fn component_provider_error(
    error: PluginProviderError,
) -> component_api::dust::plugin::host::ProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    component_api::dust::plugin::host::ProviderError {
        code: match error.code {
            PluginProviderErrorCode::QueueFull => ProviderErrorCode::QueueFull,
            PluginProviderErrorCode::UnknownRequest => ProviderErrorCode::UnknownRequest,
            PluginProviderErrorCode::UnknownProvider => ProviderErrorCode::UnknownProvider,
            PluginProviderErrorCode::StaleGeneration => ProviderErrorCode::StaleGeneration,
            PluginProviderErrorCode::StaleRevision => ProviderErrorCode::StaleRevision,
            PluginProviderErrorCode::Cancelled => ProviderErrorCode::Cancelled,
            PluginProviderErrorCode::Expired => ProviderErrorCode::Expired,
            PluginProviderErrorCode::InvalidSequence => ProviderErrorCode::InvalidSequence,
            PluginProviderErrorCode::DuplicateItem => ProviderErrorCode::DuplicateItem,
            PluginProviderErrorCode::ItemCeiling => ProviderErrorCode::ItemCeiling,
            PluginProviderErrorCode::ByteCeiling => ProviderErrorCode::ByteCeiling,
            PluginProviderErrorCode::Malformed => ProviderErrorCode::Malformed,
            PluginProviderErrorCode::Unavailable => ProviderErrorCode::Unavailable,
            PluginProviderErrorCode::Internal => ProviderErrorCode::Internal,
        },
        detail: error.detail,
    }
}

fn convert_component_storage_request(
    request: component_api::dust::plugin::host::StorageRequest,
) -> PluginStorageRequestKind {
    use component_api::dust::plugin::host::StorageRequest;
    match request {
        StorageRequest::Get(key) => PluginStorageRequestKind::Get { key },
        StorageRequest::CompareAndSet(request) => PluginStorageRequestKind::CompareAndSet {
            key: request.key,
            expected_revision: request.expected_revision,
            value: request.value,
        },
        StorageRequest::Delete(request) => PluginStorageRequestKind::Delete {
            key: request.key,
            expected_revision: request.expected_revision,
        },
        StorageRequest::PrefixList(request) => PluginStorageRequestKind::List {
            prefix: request.prefix,
            maximum: request.maximum,
        },
    }
}

fn component_storage_error(
    error: PluginStorageError,
) -> component_api::dust::plugin::host::StorageError {
    use component_api::dust::plugin::host::StorageErrorCode;
    component_api::dust::plugin::host::StorageError {
        code: match error.code {
            PluginStorageErrorCode::QueueFull => StorageErrorCode::QueueFull,
            PluginStorageErrorCode::InvalidKey => StorageErrorCode::InvalidKey,
            PluginStorageErrorCode::InvalidValue => StorageErrorCode::InvalidValue,
            PluginStorageErrorCode::StaleRevision => StorageErrorCode::StaleRevision,
            PluginStorageErrorCode::QuotaExceeded => StorageErrorCode::QuotaExceeded,
            PluginStorageErrorCode::Unavailable => StorageErrorCode::Unavailable,
            PluginStorageErrorCode::Corrupt => StorageErrorCode::Corrupt,
            PluginStorageErrorCode::GenerationLost => StorageErrorCode::GenerationLost,
        },
        detail: error.detail,
    }
}

fn component_storage_result(
    result: PluginStorageResult,
) -> component_api::dust::plugin::host::StorageResult {
    component_api::dust::plugin::host::StorageResult {
        token: result.token,
        value: result.value,
        revision: result.revision,
        entries: result
            .entries
            .into_iter()
            .map(|entry| component_api::dust::plugin::host::StorageEntry {
                key: entry.key,
                revision: entry.revision,
                value: entry.value,
            })
            .collect(),
        truncated: result.truncated,
        error: result.error.map(component_storage_error),
    }
}

fn component_ui_error(error: PluginUiError) -> component_api::dust::plugin::host::UiError {
    use component_api::dust::plugin::host::UiErrorCode;
    let code = match error.code {
        PluginUiErrorCode::QueueFull => UiErrorCode::QueueFull,
        PluginUiErrorCode::StaleGraph => UiErrorCode::StaleGraph,
        PluginUiErrorCode::StaleTree => UiErrorCode::StaleTree,
        PluginUiErrorCode::StaleHandle => UiErrorCode::StaleHandle,
        PluginUiErrorCode::ForeignGeneration => UiErrorCode::ForeignGeneration,
        PluginUiErrorCode::UndeclaredContent => UiErrorCode::UndeclaredContent,
        PluginUiErrorCode::UndeclaredAction => UiErrorCode::UndeclaredAction,
        PluginUiErrorCode::DisallowedPlacement => UiErrorCode::DisallowedPlacement,
        PluginUiErrorCode::HiddenFocus => UiErrorCode::HiddenFocus,
        PluginUiErrorCode::CrossTab => UiErrorCode::CrossTab,
        PluginUiErrorCode::InstanceCeiling => UiErrorCode::InstanceCeiling,
        PluginUiErrorCode::GraphCeiling => UiErrorCode::GraphCeiling,
        PluginUiErrorCode::InvalidTree => UiErrorCode::InvalidTree,
        PluginUiErrorCode::InvalidPlacement => UiErrorCode::InvalidPlacement,
        PluginUiErrorCode::CloseOrder => UiErrorCode::CloseOrder,
        PluginUiErrorCode::GenerationLost => UiErrorCode::GenerationLost,
        PluginUiErrorCode::InternalRejected => UiErrorCode::InternalRejected,
        PluginUiErrorCode::WrongPresenter => UiErrorCode::WrongPresenter,
        PluginUiErrorCode::StaleSurface => UiErrorCode::StaleSurface,
        PluginUiErrorCode::StaleDimensions => UiErrorCode::StaleDimensions,
        PluginUiErrorCode::InvalidLocalBounds => UiErrorCode::InvalidLocalBounds,
        PluginUiErrorCode::CellOverlap => UiErrorCode::CellOverlap,
        PluginUiErrorCode::SplitWideGrapheme => UiErrorCode::SplitWideGrapheme,
        PluginUiErrorCode::InvalidGrapheme => UiErrorCode::InvalidGrapheme,
        PluginUiErrorCode::InvalidStyle => UiErrorCode::InvalidStyle,
        PluginUiErrorCode::InvalidCursor => UiErrorCode::InvalidCursor,
        PluginUiErrorCode::InvalidHitRegion => UiErrorCode::InvalidHitRegion,
        PluginUiErrorCode::PatchCeiling => UiErrorCode::PatchCeiling,
    };
    component_api::dust::plugin::host::UiError {
        code,
        detail: error.detail,
    }
}

fn convert_component_tree(
    tree: component_api::dust::plugin::host::UiTree,
) -> Result<PluginUiTree, PluginUiError> {
    if tree.nodes.len() > 256 {
        return Err(PluginUiError::new(
            PluginUiErrorCode::InvalidTree,
            "retained UI node ceiling exceeded",
        ));
    }
    let nodes = tree
        .nodes
        .into_iter()
        .map(|node| PluginUiNode {
            id: node.id,
            kind: match node.kind {
                component_api::dust::plugin::host::UiNodeKind::Container => {
                    PluginUiNodeKind::Container
                }
                component_api::dust::plugin::host::UiNodeKind::Text => PluginUiNodeKind::Text,
                component_api::dust::plugin::host::UiNodeKind::ListNode => PluginUiNodeKind::List,
                component_api::dust::plugin::host::UiNodeKind::Button => PluginUiNodeKind::Button,
            },
            text: node.text,
            style: match node.style {
                component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
                component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
                component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
                component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
                component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
                component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
                component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
            },
            action: node.action,
            children: node.children,
        })
        .collect();
    Ok(PluginUiTree {
        revision: tree.revision,
        root: tree.root,
        nodes,
    })
}

fn convert_component_placement(
    placement: component_api::dust::plugin::host::WindowPlacement,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    size: u16,
) -> PluginRequestedPlacement {
    use component_api::dust::plugin::host::WindowPlacement;
    match placement {
        WindowPlacement::DockedLeft => PluginRequestedPlacement::DockedLeft { size },
        WindowPlacement::DockedRight => PluginRequestedPlacement::DockedRight { size },
        WindowPlacement::DockedTop => PluginRequestedPlacement::DockedTop { size },
        WindowPlacement::DockedBottom => PluginRequestedPlacement::DockedBottom { size },
        WindowPlacement::Floating => PluginRequestedPlacement::Floating {
            x,
            y,
            width,
            height,
        },
        WindowPlacement::Fullscreen => PluginRequestedPlacement::Fullscreen,
        WindowPlacement::Hidden => PluginRequestedPlacement::Hidden,
    }
}

fn convert_private_style(
    style: component_api::dust::plugin::host::PrivateStyle,
) -> PluginPrivateStyle {
    PluginPrivateStyle {
        id: style.id,
        style: match style.style {
            component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
            component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
            component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
            component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
            component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
            component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
            component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
        },
    }
}

fn convert_private_cell(cell: component_api::dust::plugin::host::PrivateCell) -> PluginPrivateCell {
    PluginPrivateCell {
        x: cell.x,
        y: cell.y,
        grapheme: cell.grapheme,
        continuation: cell.continuation,
        style: cell.style,
    }
}

fn convert_private_hit(
    hit: component_api::dust::plugin::host::PrivateHitRegion,
) -> PluginPrivateHitRegion {
    PluginPrivateHitRegion {
        x: hit.x,
        y: hit.y,
        width: hit.width,
        height: hit.height,
        action: hit.action,
    }
}

fn convert_private_snapshot(
    snapshot: component_api::dust::plugin::host::PrivateSnapshot,
) -> PluginPrivateSnapshot {
    PluginPrivateSnapshot {
        revision: snapshot.revision,
        dimensions_revision: snapshot.dimensions_revision,
        styles: snapshot
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: snapshot
            .cells
            .into_iter()
            .map(convert_private_cell)
            .collect(),
        cursor: snapshot.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        hit_regions: snapshot
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

fn convert_private_patch(
    patch: component_api::dust::plugin::host::PrivatePatch,
) -> PluginPrivatePatch {
    PluginPrivatePatch {
        revision: patch.revision,
        expected_surface_revision: patch.expected_surface_revision,
        dimensions_revision: patch.dimensions_revision,
        dirty: patch
            .dirty
            .into_iter()
            .map(|rect| PluginPrivateRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
        styles: patch
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: patch.cells.into_iter().map(convert_private_cell).collect(),
        replace_cursor: patch.replace_cursor,
        cursor: patch.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        replace_hit_regions: patch.replace_hit_regions,
        hit_regions: patch
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

fn convert_component_ui_request(
    request: component_api::dust::plugin::host::UiRequest,
    declared_content_types: &BTreeSet<String>,
) -> Result<PluginUiRequestKind, PluginUiError> {
    use component_api::dust::plugin::host::UiRequest;
    Ok(match request {
        UiRequest::OpenWindow(request) => {
            if !declared_content_types.contains(&request.content_type) {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::UndeclaredContent,
                    "content type was not statically declared",
                ));
            }
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision: request.expected_graph_revision,
                content_type: request.content_type,
                title: request.title,
                placement: convert_component_placement(
                    request.placement,
                    request.x,
                    request.y,
                    request.width,
                    request.height,
                    request.size,
                ),
                tree: request.tree.map(convert_component_tree).transpose()?,
            }
        }
        UiRequest::ReplaceUi(request) => PluginUiRequestKind::ReplaceUi {
            handle: request.handle,
            expected_tree_revision: request.expected_tree_revision,
            tree: convert_component_tree(request.tree)?,
        },
        UiRequest::PlaceWindow(request) => PluginUiRequestKind::PlaceWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
            placement: convert_component_placement(
                request.placement,
                request.x,
                request.y,
                request.width,
                request.height,
                request.size,
            ),
        },
        UiRequest::FocusWindow(request) => PluginUiRequestKind::FocusWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::CloseWindow(request) => PluginUiRequestKind::CloseWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::ReplacePrivate(request) => {
            if request.snapshot.cells.len() > usize::from(MAXIMUM_PRIVATE_CELLS_PER_CONTENT)
                || request.snapshot.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.snapshot.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .snapshot
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(
                        request
                            .snapshot
                            .hit_regions
                            .iter()
                            .map(|hit| hit.action.len()),
                    )
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchCeiling,
                    "private snapshot exceeds absolute host ceilings",
                ));
            }
            PluginUiRequestKind::ReplacePrivate {
                handle: request.handle,
                snapshot: convert_private_snapshot(request.snapshot),
            }
        }
        UiRequest::PatchPrivate(request) => {
            if request.patch.cells.len() > usize::from(MAXIMUM_PRIVATE_PATCH_CELLS)
                || request.patch.dirty.len() > usize::from(MAXIMUM_PRIVATE_DIRTY_RECTANGLES)
                || request.patch.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.patch.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .patch
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(request.patch.hit_regions.iter().map(|hit| hit.action.len()))
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchCeiling,
                    "private patch exceeds absolute host ceilings",
                ));
            }
            PluginUiRequestKind::PatchPrivate {
                handle: request.handle,
                patch: convert_private_patch(request.patch),
            }
        }
    })
}
