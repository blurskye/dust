//! Plugin manifests, capabilities and contribution declarations.

use crate::plugins::index::PluginError;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

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
    Commands,
    Input,
    Decorations,
    Completion,
    Timers,
    Http,
    HttpHost(String),
    Files,
    Process,
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
pub(super) const MAXIMUM_PRIVATE_CELLS_PER_CONTENT: u16 = 4_096;
pub(super) const MAXIMUM_PRIVATE_PATCH_CELLS: u16 = 2_048;
pub(super) const MAXIMUM_PRIVATE_DIRTY_RECTANGLES: u16 = 64;
pub(super) const MAXIMUM_PRIVATE_STYLES: u16 = 32;
pub(super) const MAXIMUM_PRIVATE_HIT_REGIONS: u16 = 64;
pub(super) const MAXIMUM_PRIVATE_BYTES: u32 = 64 * 1024;
pub(super) const MAXIMUM_PRIVATE_CELLS_PER_PLUGIN: usize = 8_192;
pub(super) const MAXIMUM_PLUGIN_SUBSCRIPTIONS: usize = 32;
pub(super) const MAXIMUM_PLUGIN_EVENT_QUEUE: usize = 128;
pub(super) const MAXIMUM_PLUGIN_PROVIDERS: usize = 32;
pub(super) const MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS: usize = 32;
pub(super) const MAXIMUM_VIRTUAL_DOCUMENT_BYTES: u32 = 1024 * 1024;
pub(super) const MAXIMUM_VIRTUAL_DOCUMENT_LINES: u32 = 100_000;
pub(super) const MAXIMUM_VIRTUAL_PATCH_BYTES: u32 = 256 * 1024;
pub(super) const MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const MAXIMUM_PROVIDER_ITEMS: u16 = 4_096;
pub(super) const MAXIMUM_PROVIDER_BYTES: u32 = 1024 * 1024;
pub(super) const MAXIMUM_STORAGE_ITEMS: u16 = 4_096;
pub(super) const MAXIMUM_STORAGE_VALUE_BYTES: u32 = 256 * 1024;
pub(super) const MAXIMUM_STORAGE_TOTAL_BYTES: u32 = 4 * 1024 * 1024;

pub(super) const PLUGIN_UI_HOST_COMMANDS: &[&str] = &[
    "surface.retained.focus.next",
    "surface.retained.focus.previous",
    "surface.retained.scroll.down",
    "surface.retained.scroll.up",
    "surface.retained.activate",
    "surface.retained.mouse.select",
    "surface.private.activate",
    "surface.private.mouse.activate",
];

pub(super) const fn plugin_contribution_schema() -> u32 {
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

pub(super) const fn default_subscription_coalescing() -> bool {
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
    pub plugin_load: u64,
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

pub(super) const fn default_plugin_content_focusable() -> bool {
    true
}

pub(super) const fn default_plugin_content_instances() -> u16 {
    1
}
#[derive(Clone, Debug)]
pub struct PluginContributionIndex {
    pub capabilities: BTreeSet<PluginCapability>,
    pub owner: String,
    pub version: String,
    pub checksum: String,
    pub plugin_load: u64,
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

/// Identity of one accepted catalogue revision; cloning a snapshot preserves it.
#[derive(Clone, Debug, Default)]
pub(crate) struct PluginCatalogVersion(std::sync::Arc<()>);
impl PartialEq for PluginCatalogVersion {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PluginCatalogVersion {}

#[derive(Clone, Debug, Default)]
pub struct PluginContributionCatalog {
    indexes: BTreeMap<String, PluginContributionIndex>,
    version: PluginCatalogVersion,
}

impl PluginContributionCatalog {
    pub(crate) fn version(&self) -> PluginCatalogVersion {
        self.version.clone()
    }
    pub fn install(&mut self, index: PluginContributionIndex) -> Result<(), PluginError> {
        if self.indexes.contains_key(&index.owner) {
            return Err(PluginError::AlreadyActive);
        }
        for existing in self.indexes.values() {
            let content_conflict = existing
                .content_types
                .iter()
                .any(|left| index.content_types.iter().any(|right| left.id == right.id));
            if content_conflict {
                return Err(PluginError::ContributionConflict);
            }
        }
        self.indexes.insert(index.owner.clone(), index);
        self.version = PluginCatalogVersion::default();
        Ok(())
    }

    pub fn remove_load(&mut self, owner: &str, plugin_load: u64) -> bool {
        if self
            .indexes
            .get(owner)
            .is_some_and(|index| index.plugin_load == plugin_load)
        {
            self.indexes.remove(owner);
            self.version = PluginCatalogVersion::default();
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

    pub fn context_snapshot(&self) -> Result<crate::editor::input::InputContexts, PluginError> {
        let mut contexts = crate::editor::input::InputContexts::bundled();
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
                        index.plugin_load,
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
    Number,
    List,
    Record,
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
