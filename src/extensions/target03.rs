//! Test-only execution lane for the normative target ABI. Production remains 0.2.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) mod api {
    wasmtime::component::bindgen!({
        path: "docs/specs/api-next.wit",
        world: "editor-plugin",
    });
}

use api::dust::plugin::host as wire;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetManifest {
    target_api: String,
    clone_view: Vec<String>,
    tile_view: Vec<String>,
    plugin: super::PluginManifest,
}

#[derive(Clone, Debug)]
pub(crate) struct TargetAdmission {
    pub index: super::PluginContributionIndex,
    clone_view: BTreeSet<String>,
    tile_view: BTreeSet<String>,
    activation: Arc<()>,
}

impl TargetAdmission {
    pub(crate) fn parse(source: &str, generation: u64) -> Result<Self, String> {
        let manifest: TargetManifest = toml::from_str(source).map_err(|error| error.to_string())?;
        if manifest.target_api != "0.3.0" {
            return Err("target manifest requires target_api = 0.3.0".into());
        }
        let index = super::index_manifest_contributions(
            &manifest.plugin,
            generation,
            "target03-fixture-manifest.toml".into(),
        )
        .map_err(|error| error.to_string())?;
        let clone_view = manifest.clone_view.iter().cloned().collect::<BTreeSet<_>>();
        let tile_view = manifest.tile_view.iter().cloned().collect::<BTreeSet<_>>();
        let declared = index
            .content_types
            .iter()
            .map(|content| content.id.as_str())
            .collect::<BTreeSet<_>>();
        if clone_view.is_empty()
            || clone_view.len() != manifest.clone_view.len()
            || clone_view
                .iter()
                .any(|content| !declared.contains(content.as_str()))
        {
            return Err("target clone_view is empty, duplicated, or undeclared".into());
        }
        if tile_view.is_empty()
            || tile_view.len() != manifest.tile_view.len()
            || tile_view
                .iter()
                .any(|content| !declared.contains(content.as_str()))
        {
            return Err("target tile_view is empty, duplicated, or undeclared".into());
        }
        Ok(Self {
            index,
            clone_view,
            tile_view,
            activation: Arc::new(()),
        })
    }

    pub(crate) fn can_clone(&self, content_type: &str) -> bool {
        self.clone_view.contains(content_type)
    }

    pub(crate) fn can_tile(&self, content_type: &str) -> bool {
        self.tile_view.contains(content_type)
    }

    pub(crate) fn without_clone(&self, content_type: &str) -> Self {
        let mut candidate = self.clone();
        candidate.clone_view.remove(content_type);
        candidate
    }
}

#[derive(Clone, Debug)]
struct ContentAdmission {
    private: bool,
    clone_view: bool,
}

#[derive(Debug)]
struct LoadLease {
    owner: String,
    generation: u64,
    checksum: String,
    activation: Arc<()>,
    active: AtomicBool,
}

impl LoadLease {
    fn catalogue_current(&self, catalogue: &super::PluginContributionCatalog) -> bool {
        self.active.load(Ordering::Acquire)
            && catalogue.get(&self.owner).is_some_and(|index| {
                index.generation == self.generation && index.checksum == self.checksum
            })
    }

    fn current(
        &self,
        admission: &TargetAdmission,
        catalogue: &super::PluginContributionCatalog,
        source: &super::PluginCatalogVersion,
    ) -> bool {
        self.catalogue_current(catalogue)
            && *source == catalogue.version()
            && Arc::ptr_eq(&self.activation, &admission.activation)
            && self.generation == admission.index.generation
            && self.checksum == admission.index.checksum
            && self.owner == admission.index.owner
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StagedUiRequest {
    pub token: u64,
    pub invocation: wire::Invocation,
    pub request: wire::UiRequest,
    lease: Arc<LoadLease>,
    catalogue: super::PluginCatalogVersion,
}

impl StagedUiRequest {
    pub(crate) fn current(
        &self,
        admission: &TargetAdmission,
        catalogue: &super::PluginContributionCatalog,
    ) -> bool {
        self.lease.current(admission, catalogue, &self.catalogue)
    }
}

struct HostState {
    limits: wasmtime::StoreLimits,
    invocation: Option<wire::Invocation>,
    invocation_catalogue: Option<super::PluginCatalogVersion>,
    lease: Arc<LoadLease>,
    next_token: u64,
    pending_tokens: BTreeSet<u64>,
    requests: Vec<StagedUiRequest>,
    notices: Vec<String>,
    contents: BTreeMap<String, ContentAdmission>,
    callback_dimensions: BTreeMap<u64, u64>,
    delivered_events: BTreeSet<u64>,
    active: bool,
}

fn failure(code: wire::ErrorCode, message: impl Into<String>, retryable: bool) -> wire::Failure {
    wire::Failure {
        code,
        message: message.into(),
        retryable,
    }
}

fn unsupported(operation: &str) -> wire::Failure {
    failure(
        wire::ErrorCode::Unsupported,
        format!("target 0.3 {operation} is not implemented"),
        false,
    )
}

struct StageCheckpoint {
    requests: usize,
    notices: usize,
    pending: BTreeSet<u64>,
}

impl HostState {
    fn checkpoint(&self) -> StageCheckpoint {
        StageCheckpoint {
            requests: self.requests.len(),
            notices: self.notices.len(),
            pending: self.pending_tokens.clone(),
        }
    }

    fn rollback(&mut self, checkpoint: StageCheckpoint) {
        self.requests.truncate(checkpoint.requests);
        self.notices.truncate(checkpoint.notices);
        self.pending_tokens = checkpoint.pending;
    }

    fn reserve(
        &mut self,
        invocation: wire::Invocation,
        request: wire::UiRequest,
    ) -> Result<u64, wire::Failure> {
        let catalogue = self.invocation_catalogue.clone().ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target invocation has no catalogue source",
                false,
            )
        })?;
        if self.pending_tokens.len() >= 64 {
            return Err(failure(
                wire::ErrorCode::QueueFull,
                "target UI request queue is full",
                true,
            ));
        }
        let token = self.next_token;
        self.next_token = token.checked_add(1).ok_or_else(|| {
            failure(
                wire::ErrorCode::Exhausted,
                "target UI token space is exhausted",
                false,
            )
        })?;
        self.pending_tokens.insert(token);
        self.requests.push(StagedUiRequest {
            token,
            invocation,
            request,
            lease: self.lease.clone(),
            catalogue,
        });
        Ok(token)
    }
}

impl wire::Host for HostState {
    fn features(&mut self) -> Vec<wire::Feature> {
        vec![wire::Feature {
            id: "ui".into(),
            version: 1,
            available: true,
        }]
    }

    fn limits(&mut self) -> Vec<wire::Limit> {
        vec![
            wire::Limit {
                name: "per-load-requests".into(),
                value: 64,
            },
            wire::Limit {
                name: "event-batch".into(),
                value: 64,
            },
        ]
    }

    fn notify(&mut self, message: wire::Notice) -> Result<(), wire::Failure> {
        if message.message.is_empty() || message.message.len() > 4096 {
            return Err(failure(
                wire::ErrorCode::InvalidArgument,
                "notice is empty or oversized",
                false,
            ));
        }
        self.notices.push(message.message);
        Ok(())
    }

    fn queue_ui(&mut self, request: wire::UiRequest) -> Result<u64, wire::Failure> {
        if !self.active {
            return Err(failure(
                wire::ErrorCode::WorkerStopped,
                "target load is retired",
                false,
            ));
        }
        let invocation = self.invocation.clone().ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target UI call has no active invocation",
                false,
            )
        })?;
        match &request {
            wire::UiRequest::Open(open) => {
                if invocation.kind != wire::ExecutionKind::Command {
                    return Err(failure(
                        wire::ErrorCode::PermissionDenied,
                        "callback cannot open or clone a view",
                        false,
                    ));
                }
                let ui = invocation.ui.as_ref().ok_or_else(|| {
                    failure(
                        wire::ErrorCode::StaleSource,
                        "target UI call has no admitted UI source",
                        false,
                    )
                })?;
                let content = self.contents.get(&open.content_type).ok_or_else(|| {
                    failure(
                        wire::ErrorCode::PermissionDenied,
                        "target content type is not admitted",
                        false,
                    )
                })?;
                if open
                    .tree
                    .as_ref()
                    .is_some_and(|tree| tree.nodes.len() > 256)
                {
                    return Err(failure(
                        wire::ErrorCode::LimitExceeded,
                        "target retained tree exceeds the node ceiling",
                        false,
                    ));
                }
                let tree_valid = if content.private {
                    open.tree.is_none()
                } else {
                    open.tree
                        .as_ref()
                        .is_some_and(|tree| tree.revision == 1 && !tree.nodes.is_empty())
                };
                if open.content.is_some() && !content.clone_view {
                    return Err(failure(
                        wire::ErrorCode::PermissionDenied,
                        "target clone-view is not admitted",
                        false,
                    ));
                }
                if open.title.is_empty()
                    || open.title.len() > 128
                    || open.graph_revision != ui.graph_revision
                    || open.tab != ui.tab
                    || open.content.is_some_and(|source| source != ui.content)
                    || !tree_valid
                {
                    return Err(failure(
                        wire::ErrorCode::InvalidArgument,
                        "target open-view does not match its admitted source or declaration",
                        false,
                    ));
                }
            }
            wire::UiRequest::Place(place) => {
                let ui = invocation.ui.as_ref().ok_or_else(|| {
                    failure(
                        wire::ErrorCode::StaleSource,
                        "target placement has no admitted UI source",
                        false,
                    )
                })?;
                let placement_valid = match &place.placement {
                    wire::Placement::Tiled(tile) => (1..=999).contains(&tile.ratio),
                    wire::Placement::Docked(dock) => dock.size > 0,
                    wire::Placement::Floating(rect) => rect.width > 0 && rect.height > 0,
                    wire::Placement::Fullscreen | wire::Placement::Hidden => true,
                };
                if invocation.kind != wire::ExecutionKind::Command
                    || place.window != ui.window
                    || place.graph_revision != ui.graph_revision
                    || place.tab != ui.tab
                    || !placement_valid
                {
                    return Err(failure(
                        wire::ErrorCode::InvalidArgument,
                        "target placement does not match its admitted source",
                        false,
                    ));
                }
            }
            wire::UiRequest::Focus(focus) => {
                let ui = invocation.ui.as_ref().ok_or_else(|| {
                    failure(
                        wire::ErrorCode::StaleSource,
                        "target focus has no admitted UI source",
                        false,
                    )
                })?;
                if invocation.kind != wire::ExecutionKind::Command
                    || focus.window != ui.window
                    || focus.graph_revision != ui.graph_revision
                {
                    return Err(failure(
                        wire::ErrorCode::InvalidArgument,
                        "target focus does not match its admitted source",
                        false,
                    ));
                }
            }
            wire::UiRequest::Close(close) => {
                let ui = invocation.ui.as_ref().ok_or_else(|| {
                    failure(
                        wire::ErrorCode::StaleSource,
                        "target close has no admitted UI source",
                        false,
                    )
                })?;
                if invocation.kind != wire::ExecutionKind::Command
                    || close.view != ui.view
                    || close.graph_revision != ui.graph_revision
                {
                    return Err(failure(
                        wire::ErrorCode::InvalidArgument,
                        "target close does not match its admitted source",
                        false,
                    ));
                }
            }
            wire::UiRequest::Cells(update) => {
                if update.cells.len() > 4_096 {
                    return Err(failure(
                        wire::ErrorCode::LimitExceeded,
                        "target private cells exceed the per-view ceiling",
                        false,
                    ));
                }
                if invocation.kind != wire::ExecutionKind::Lifecycle
                    || invocation.ui.is_some()
                    || self.callback_dimensions.get(&update.view)
                        != Some(&update.dimensions_revision)
                    || update.expected.checked_add(1) != Some(update.revision)
                    || update.revision == 0
                    || update.dirty.len() > 64
                    || update.hits.len() > 64
                {
                    return Err(failure(
                        wire::ErrorCode::InvalidArgument,
                        "target cells do not match the delivered dimensions callback",
                        false,
                    ));
                }
            }
            _ => return Err(unsupported("queue-ui request")),
        }
        self.reserve(invocation, request)
    }

    fn queue_interaction(
        &mut self,
        _request: wire::InteractionRequest,
    ) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-interaction"))
    }
    fn update_interaction(
        &mut self,
        _update: wire::InteractionUpdate,
    ) -> Result<(), wire::Failure> {
        Err(unsupported("update-interaction"))
    }
    fn validate_submission(&mut self, _validation: wire::Validation) -> Result<(), wire::Failure> {
        Err(unsupported("validate-submission"))
    }
    fn queue_snapshot(&mut self, _request: wire::SnapshotRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-snapshot"))
    }
    fn propose_edits(&mut self, _proposal: wire::Proposal) -> Result<u64, wire::Failure> {
        Err(unsupported("propose-edits"))
    }
    fn publish_virtual(&mut self, _update: wire::VirtualUpdate) -> Result<(), wire::Failure> {
        Err(unsupported("publish-virtual"))
    }
    fn publish_provider(&mut self, _chunk: wire::ProviderChunk) -> Result<(), wire::Failure> {
        Err(unsupported("publish-provider"))
    }
    fn queue_storage(&mut self, _request: wire::StorageRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-storage"))
    }
    fn request_grant(&mut self, _request: wire::GrantRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("request-grant"))
    }
    fn queue_service(&mut self, _request: wire::ServiceRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-service"))
    }
    fn cancel(&mut self, _request: u64) -> Result<(), wire::Failure> {
        Err(unsupported("cancel"))
    }
    fn release_handle(&mut self, _value: u64) -> Result<(), wire::Failure> {
        Err(unsupported("release-handle"))
    }
}

pub(crate) struct Runtime {
    store: wasmtime::Store<HostState>,
    bindings: api::EditorPlugin,
    commands: BTreeSet<String>,
    lease: Arc<LoadLease>,
}

impl Runtime {
    pub(crate) fn load(core: &[u8], admission: &TargetAdmission) -> Result<Self, String> {
        let checksum = format!("{:x}", Sha256::digest(core));
        if checksum != admission.index.checksum.to_ascii_lowercase() {
            return Err("target Component checksum differs from its manifest".into());
        }
        let component = wit_component::ComponentEncoder::default()
            .module(core)
            .map_err(|error| error.to_string())?
            .validate(true)
            .encode()
            .map_err(|error| error.to_string())?;
        let mut config = wasmtime::Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config).map_err(|error| error.to_string())?;
        let component = wasmtime::component::Component::from_binary(&engine, &component)
            .map_err(|error| error.to_string())?;
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(64 * 1024 * 1024)
            .instances(64)
            .build();
        let lease = Arc::new(LoadLease {
            owner: admission.index.owner.clone(),
            generation: admission.index.generation,
            checksum: admission.index.checksum.clone(),
            activation: admission.activation.clone(),
            active: AtomicBool::new(true),
        });
        let mut store = wasmtime::Store::new(
            &engine,
            HostState {
                limits,
                invocation: None,
                invocation_catalogue: None,
                lease: lease.clone(),
                next_token: 1,
                pending_tokens: BTreeSet::new(),
                requests: Vec::new(),
                notices: Vec::new(),
                contents: admission
                    .index
                    .content_types
                    .iter()
                    .map(|content| {
                        (
                            content.id.clone(),
                            ContentAdmission {
                                private: content.presenter
                                    == super::PluginPresenterKind::PrivateCells,
                                clone_view: admission.can_clone(&content.id),
                            },
                        )
                    })
                    .collect(),
                callback_dimensions: BTreeMap::new(),
                delivered_events: BTreeSet::new(),
                active: true,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(10_000_000)
            .map_err(|error| error.to_string())?;
        store.set_epoch_deadline(1);
        let mut linker = wasmtime::component::Linker::new(&engine);
        api::EditorPlugin::add_to_linker(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;
        let bindings = api::EditorPlugin::instantiate(&mut store, &component, &linker)
            .map_err(|error| error.to_string())?;
        let commands = bindings
            .call_activate(&mut store)
            .map_err(|error| error.to_string())?
            .map_err(|failure| format!("{:?}: {}", failure.code, failure.message))?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let declared = admission
            .index
            .commands
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if commands != declared {
            return Err("target Component activation differs from its manifest commands".into());
        }
        Ok(Self {
            store,
            bindings,
            commands,
            lease,
        })
    }

    pub(crate) fn execute(
        &mut self,
        invocation: wire::Invocation,
        catalogue: &super::PluginContributionCatalog,
    ) -> Result<(), String> {
        if !self.store.data().active || !self.lease.catalogue_current(catalogue) {
            return Err("target load or catalogue source is retired".into());
        }
        if !self.commands.contains(&invocation.command) {
            return Err("target command is not activated".into());
        }
        let checkpoint = self.store.data().checkpoint();
        self.store.data_mut().invocation = Some(invocation.clone());
        self.store.data_mut().invocation_catalogue = Some(catalogue.version());
        let result = self
            .bindings
            .call_execute_command(&mut self.store, &invocation)
            .map_err(|error| error.to_string())
            .and_then(|result| {
                result.map_err(|failure| format!("{:?}: {}", failure.code, failure.message))
            });
        self.store.data_mut().invocation = None;
        self.store.data_mut().invocation_catalogue = None;
        if result.is_err() {
            self.store.data_mut().rollback(checkpoint);
        }
        result
    }

    pub(crate) fn handle_events(
        &mut self,
        events: Vec<wire::Event>,
        catalogue: &super::PluginContributionCatalog,
    ) -> Result<(), String> {
        if !self.store.data().active || !self.lease.catalogue_current(catalogue) {
            return Err("target load or catalogue source is retired".into());
        }
        if events.is_empty() || events.len() > 64 {
            return Err("target event batch is empty or exceeds its ceiling".into());
        }
        let mut delivered_tokens = BTreeSet::new();
        let mut delivered_ids = BTreeSet::new();
        let mut dimensions = BTreeMap::new();
        for event in &events {
            if event.id == 0
                || self.store.data().delivered_events.contains(&event.id)
                || !delivered_ids.insert(event.id)
                || event.context.ui.is_some()
            {
                return Err("target event is stale, duplicated, or has invalid context".into());
            }
            match &event.value {
                wire::EventValue::UiResult((token, _)) => {
                    if event.context.kind != wire::ExecutionKind::ResultOnly {
                        return Err("target result callback authority is invalid".into());
                    }
                    if !self.store.data().pending_tokens.contains(token)
                        || !delivered_tokens.insert(*token)
                    {
                        return Err("target UI result token is stale or duplicated".into());
                    }
                }
                wire::EventValue::Dimensions(value) => {
                    if event.context.kind != wire::ExecutionKind::Lifecycle
                        || value.view == 0
                        || value.revision == 0
                        || value.width == 0
                        || value.height == 0
                    {
                        return Err("target dimensions callback is invalid".into());
                    }
                    if dimensions.insert(value.view, value.revision).is_some() {
                        return Err("target dimensions event is duplicated".into());
                    }
                }
                _ => return Err("target event kind or callback authority is invalid".into()),
            }
        }
        self.store.data_mut().delivered_events.extend(delivered_ids);
        for token in &delivered_tokens {
            self.store.data_mut().pending_tokens.remove(token);
        }
        let checkpoint = self.store.data().checkpoint();
        self.store.data_mut().callback_dimensions = dimensions;
        self.store.data_mut().invocation = events.first().map(|event| event.context.clone());
        self.store.data_mut().invocation_catalogue = Some(catalogue.version());
        let result = self
            .bindings
            .call_handle_events(&mut self.store, &events)
            .map_err(|error| error.to_string())
            .and_then(|result| {
                result.map_err(|failure| format!("{:?}: {}", failure.code, failure.message))
            });
        self.store.data_mut().invocation = None;
        self.store.data_mut().invocation_catalogue = None;
        self.store.data_mut().callback_dimensions.clear();
        if result.is_err() {
            self.store.data_mut().rollback(checkpoint);
        }
        result
    }

    pub(crate) fn deactivate(&mut self) -> Result<(), String> {
        if !self.store.data().active {
            return Err("target load is retired".into());
        }
        let result = self
            .bindings
            .call_deactivate(&mut self.store)
            .map_err(|error| error.to_string())
            .and_then(|result| {
                result.map_err(|failure| format!("{:?}: {}", failure.code, failure.message))
            });
        self.lease.active.store(false, Ordering::Release);
        let state = self.store.data_mut();
        state.active = false;
        state.invocation = None;
        state.invocation_catalogue = None;
        state.requests.clear();
        state.pending_tokens.clear();
        state.callback_dimensions.clear();
        result
    }

    pub(crate) fn activated_commands(&self) -> BTreeSet<String> {
        self.commands.clone()
    }

    pub(crate) fn take_requests(&mut self) -> Vec<StagedUiRequest> {
        std::mem::take(&mut self.store.data_mut().requests)
    }

    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().notices)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.lease.active.store(false, Ordering::Release);
    }
}
