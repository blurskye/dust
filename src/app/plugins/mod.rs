//! Generation-scoped extension delivery and storage runtime.
use crate::app::plugins::edits::PluginEditSources;
use crate::app::plugins::edits::PreparedPluginEdit;
use crate::app::plugins::edits::SourceExecution;
use crate::app::workspace::WorkspaceSession;
use crate::app::workspace::WorkspaceVersion;
use crate::app::workspace::edits::OriginKey;
use crate::plugins::PluginContributionCatalog;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginError;
use crate::plugins::PluginHostEventQueue;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginManager;
use crate::plugins::PluginStorageContribution;
use crate::plugins::PluginStorageRequest;
use crate::plugins::PluginStorageResult;
use crate::plugins::PluginStorageService;
use crate::plugins::PluginSubscriptionKind;
use crate::plugins::PluginUiEvent;
use crate::tools::PackageService;
use crate::tools::RegistryPackageMetadata;
use crate::tools::RegistryService;
use crate::editor::EditorState;
use crate::plugins::OwnedPluginEvent;
use crate::plugins::PluginExecutionReply;
use crate::plugins::PluginExecutionRequest;
use crate::plugins::PluginLimits;
use crate::plugins::PluginProviderBroker;
use crate::plugins::PluginStatus;
use crate::plugins::PluginVirtualDocumentBroker;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;
mod confirmations;
mod hosting;
pub(super) use confirmations::ConfirmationPresentation;
pub(in crate::app) mod integration;
mod panels;
pub(super) use hosting::WindowUpdate;
pub(super) use integration::ExtensionUpdate;
pub(super) use integration::RegistryUpdate;
pub(super) use windows::PluginUiInteraction;
#[cfg(test)]
pub(in crate::app) use windows::tests::fixture_manifest;
pub(in crate::app) mod content;
mod documents;
pub(super) use content::VirtualContext;
pub(super) use content::VirtualOpen;
pub(super) use content::VirtualPublication;

pub(in crate::app) struct InvocationContext<'a> {
    pub editor: &'a EditorState,
    pub workspace: &'a WorkspaceSession,
    pub configuration: &'a u64,
    pub generation: u64,
    pub ui_revision: Option<u64>,
    pub input_capture: std::sync::Arc<()>,
    pub navigation: crate::app::windows::NavigationStamp,
    pub interaction_available: bool,
    pub maximum: usize,
    pub arguments: Vec<String>,
}

type BufferObservation = (u64, u64, usize, usize, usize, u8);
pub(in crate::app) type HostObservation = (u64, String, Option<BufferObservation>, u64);
pub(super) struct ExtensionSession {
    runtime_epoch: u64,
    windows: windows::PluginWindowHost,
    providers: PluginProviderBroker,
    virtual_documents: PluginVirtualDocumentBroker,
    virtual_editor: documents::VirtualDocumentEditorBridge,
    confirmations: confirmations::PluginConfirmations,
    active_picker: Option<u64>,
    provider_sources:
        HashMap<u64, Option<(crate::editor::BufferId, crate::editor::text::TextVersion)>>,
    plugin_contributions: PluginContributionCatalog,
    pending_index_activation: BTreeMap<String, (PluginLoadId, String)>,
    rejected_plugin_loads: BTreeSet<(String, PluginLoadId)>,
    packages: PackageService,
    registry: RegistryService,
    registry_intents: HashMap<u64, (bool, Option<crate::app::picker::PickerEpoch>)>,
    pending_registry_artifact: Option<(RegistryPackageMetadata, Vec<u8>)>,
    package_preparing: BTreeSet<u64>,
    package_aborts: BTreeSet<u64>,
    package_refresh: bool,
    package_prepared: HashMap<String, (PluginLoadId, u64)>,
    package_commits: HashMap<u64, (String, PluginLoadId)>,
    package_startup_pending: HashMap<String, PluginLoadId>,
    package_managed_plugins: HashMap<String, PluginLoadId>,
    disabled_plugins: BTreeSet<String>,
    package_startup_request: Option<u64>,
    package_startup_rollback: Option<u64>,
    package_startup_rollback_attempted: bool,
    plugin_worker_reported: bool,
    package_store: PathBuf,
    manager: PluginManager,
    edits: PluginEditSources,
    ui: BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>>,
    events: PluginHostEventQueue,
    observation: Option<HostObservation>,
    observation_counts: Option<(usize, usize, usize)>,
    storage: PluginStorageService,
    results: BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>>,
}
impl ExtensionSession {
    pub(in crate::app) fn new(
        root: PathBuf,
        capacity: usize,
        package_root: PathBuf,
        registry: RegistryService,
    ) -> Self {
        Self {
            runtime_epoch: 0,
            windows: windows::PluginWindowHost::default(),
            providers: PluginProviderBroker::default(),
            virtual_documents: PluginVirtualDocumentBroker::default(),
            virtual_editor: documents::VirtualDocumentEditorBridge::default(),
            confirmations: confirmations::PluginConfirmations::default(),
            active_picker: None,
            provider_sources: HashMap::new(),
            plugin_contributions: PluginContributionCatalog::default(),
            pending_index_activation: BTreeMap::new(),
            rejected_plugin_loads: BTreeSet::new(),
            registry_intents: HashMap::new(),
            pending_registry_artifact: None,
            package_preparing: BTreeSet::new(),
            package_aborts: BTreeSet::new(),
            package_refresh: false,
            package_prepared: HashMap::new(),
            package_commits: HashMap::new(),
            package_startup_pending: HashMap::new(),
            package_managed_plugins: HashMap::new(),
            disabled_plugins: BTreeSet::new(),
            package_startup_request: None,
            package_startup_rollback: None,
            package_startup_rollback_attempted: false,
            plugin_worker_reported: false,
            registry,
            package_store: package_root.join("store"),
            packages: PackageService::start(package_root, 8, 16 * 1024 * 1024, 64 * 1024 * 1024),
            manager: PluginManager::start(capacity),
            edits: PluginEditSources::default(),
            ui: BTreeMap::new(),
            events: PluginHostEventQueue::default(),
            observation: None,
            observation_counts: None,
            storage: PluginStorageService::start(root, capacity),
            results: BTreeMap::new(),
        }
    }
    pub(in crate::app) fn statuses(&self) -> &BTreeMap<String, PluginStatus> {
        &self.manager.statuses
    }
    pub(in crate::app) fn load_id(&self, id: &str) -> Option<PluginLoadId> {
        self.manager.load_id(id)
    }
    pub(in crate::app) fn is_active_generation(&self, id: &str, load: PluginLoadId) -> bool {
        self.manager.is_active_generation(id, load)
    }
    pub(in crate::app) fn index_from_store(
        &mut self,
        plugin: String,
        store: PathBuf,
        manifest: PathBuf,
        component: PathBuf,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.manager
            .index_from_store(plugin, store, manifest, component, limits)
    }
    pub(in crate::app) fn disable(&mut self, plugin: &str) -> Result<(), PluginError> {
        let load = self.manager.load_id(plugin);
        self.manager.disable(plugin)?;
        self.disabled_plugins.insert(plugin.into());
        // The manager discards late events for disabled generations, so they cannot
        // finish a pending activation or release its prepared package token.
        if let Some(load) = load {
            if self.package_startup_pending.get(plugin) == Some(&load) {
                self.package_startup_pending.remove(plugin);
            }
            self.abort_prepared(plugin, load);
        }
        Ok(())
    }
    pub(in crate::app) fn disable_generation(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<(), PluginError> {
        self.manager.disable_generation(plugin, load)
    }
    pub(in crate::app) fn poll_owned(&mut self) -> Result<Vec<OwnedPluginEvent>, PluginError> {
        self.manager.poll_owned()
    }
    pub(in crate::app) fn execute(
        &mut self,
        plugin: &str,
        command: &str,
        context: InvocationContext<'_>,
    ) -> Result<(), String> {
        let load = self
            .manager
            .load_id(plugin)
            .ok_or_else(|| PluginError::NotActive.to_string())?;
        if !self.manager.is_active_generation(plugin, load) {
            return Err(PluginError::NotActive.to_string());
        }
        let loan = self.edits.prepare_interactive(
            &mut self.manager,
            (plugin, command),
            context.editor,
            context.workspace,
            context.configuration,
            (
                context.maximum,
                Some(context.input_capture),
                Some(context.navigation),
                context.interaction_available,
            ),
        )?;
        let origin = loan.origin().clone();
        let reservation = self.confirmations.reserve(&origin);
        let confirmation_available = reservation.available();
        let mut request = PluginExecutionRequest {
            runtime_epoch: self.runtime_epoch,
            plugin: plugin.into(),
            command: command.into(),
            arguments: context.arguments,
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: context.ui_revision,
            confirmation_available,
            ui_events: Vec::new(),
            confirmation_results: Vec::new(),
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            generation: context.generation,
        };
        PluginManager::validate_invocation(&request).map_err(|error| error.to_string())?;
        let mut virtual_requests: crate::plugins::PluginVirtualRequestLane<'_> =
            self.virtual_documents.request_lane();
        // Validate before consuming deliveries; real queue admission stays reserved
        // while the remaining payloads are built and queued.
        reservation.enqueue(|| {
            request.ui_events = self.ui.remove(&(plugin.into(), load)).unwrap_or_default();
            request.host_events = self.events.deliver(plugin, load);
            request.storage_results = self
                .results
                .remove(&(plugin.into(), load))
                .unwrap_or_default();
            request.provider_requests =
                self.providers
                    .requests(plugin, load, &format!("plugin.{plugin}.{command}"));
            request.virtual_document_requests =
                virtual_requests.deliver(plugin, load, &format!("plugin.{plugin}.{command}"));
            loan.submit(request)
        })
    }
    pub(in crate::app) fn receive(
        &mut self,
        reply: PluginExecutionReply,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<SourceExecution, String> {
        if reply
            .origin()
            .is_some_and(|origin| self.confirmations.owns(origin))
        {
            let (execution, confirmation_source) =
                self.confirmations.receive(reply, &self.manager)?;
            return Ok(SourceExecution {
                load: confirmation_source.origin().load(),
                execution,
                confirmation_source,
                confirmation_result: true,
                source_actions_admitted: true,
                preview: Ok(None),
            });
        }
        let origin = reply.origin().cloned();
        let received = self
            .edits
            .receive(reply, &self.manager, workspace, configuration);
        if received.is_err()
            && let Some(origin) = origin
        {
            self.confirmations.release_terminal(&origin);
        }
        received
    }
    pub(in crate::app) fn admit_confirmations(
        &mut self,
        source: crate::app::plugins::edits::ConfirmationSource,
        requests: Vec<crate::plugins::PluginConfirmationRequest>,
    ) -> Result<(), String> {
        self.confirmations.admit(source, requests, &self.manager)
    }
    pub(in crate::app) fn next_confirmation(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &crate::app::input::InputSession,
        windows: &crate::app::windows::WindowSession,
    ) -> Option<ConfirmationPresentation> {
        self.confirmations
            .activate_next(confirmations::ConfirmationContext {
                manager: &self.manager,
                editor,
                workspace,
                configuration,
                input,
                windows,
            })
    }
    pub(in crate::app) fn answer_confirmation(
        &mut self,
        answer: crate::plugins::PluginConfirmationAnswer,
    ) -> bool {
        self.confirmations.answer(answer)
    }
    pub(in crate::app) fn dispatch_confirmation_results(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<bool, String> {
        self.confirmations.dispatch(
            &mut self.manager,
            editor,
            workspace,
            configuration,
            &mut self.ui,
            self.runtime_epoch,
        )
    }
    pub(in crate::app) fn retire_stale_confirmations(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &crate::app::input::InputSession,
        windows: &crate::app::windows::WindowSession,
    ) -> bool {
        self.confirmations
            .retire_stale(confirmations::ConfirmationContext {
                manager: &self.manager,
                editor,
                workspace,
                configuration,
                input,
                windows,
            })
    }
    pub(in crate::app) fn owns_edit(&self, key: &OriginKey) -> bool {
        self.edits.owns(key)
    }
    pub(in crate::app) fn check_edit(
        &self,
        key: &OriginKey,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        self.edits
            .check(key, &self.manager, editor, workspace, configuration)
    }
    pub(in crate::app) fn prepare_edit(
        &mut self,
        key: &OriginKey,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<PreparedPluginEdit, String> {
        self.edits
            .prepare_preview(key, &self.manager, editor, workspace, configuration)
    }
    pub(in crate::app) fn finish_edit(&mut self, key: &OriginKey) -> Result<(), String> {
        self.edits.finish(key)
    }
    pub(in crate::app) fn retire_edits(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> (usize, Option<OriginKey>) {
        self.edits
            .retire(&self.manager, editor, workspace, configuration)
    }
    pub(in crate::app) fn close_edits(&mut self) -> usize {
        self.edits.close()
    }
    pub(in crate::app) fn push_action(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        event: PluginUiEvent,
    ) -> Result<(), &'static str> {
        let pending = self.ui.entry((plugin, load)).or_default();
        if pending.len() >= 64 {
            return Err("private surface action queue is full");
        }
        pending.push(event);
        Ok(())
    }
    pub(in crate::app) fn queue_ui(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        events: impl IntoIterator<Item = PluginUiEvent>,
    ) {
        let pending = self.ui.entry((plugin, load)).or_default();
        for event in events {
            if event.token == 0 && event.action.is_none() {
                pending.retain(|previous| {
                    previous.token != 0
                        || previous.action.is_some()
                        || previous.handle != event.handle
                });
            }
            pending.push(event);
        }
    }
    #[cfg(test)]
    pub(in crate::app) fn take_ui(&mut self, plugin: &str, load: PluginLoadId) -> Vec<PluginUiEvent> {
        self.ui.remove(&(plugin.into(), load)).unwrap_or_default()
    }
    #[cfg(test)]
    pub(in crate::app) fn deliver(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Vec<crate::plugins::PluginHostEventEnvelope> {
        self.events.deliver(plugin, load)
    }
    pub(in crate::app) fn acknowledge(&mut self, plugin: &str, load: PluginLoadId, ids: &[u64]) {
        self.events.acknowledge(plugin, load, ids);
    }
    pub(in crate::app) fn install_events(
        &mut self,
        index: &PluginContributionIndex,
    ) -> Result<(), PluginError> {
        self.observation = None;
        self.events.install(index)
    }
    pub(in crate::app) fn install_storage(&mut self, plugin: &str, load: PluginLoadId) {
        self.storage.install_generation(plugin, load);
    }
    pub(in crate::app) fn reset_observation(&mut self) {
        self.observation = None;
    }
    pub(in crate::app) fn clear_events(&mut self) {
        self.events = PluginHostEventQueue::default();
        self.observation = None;
    }
    pub(in crate::app) fn remove_generation(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        let active_confirmation = self.confirmations.remove_generation(plugin, load);
        self.ui.remove(&(plugin.into(), load));
        self.results.remove(&(plugin.into(), load));
        self.events.remove_generation(plugin, load);
        self.storage.remove_generation(plugin, load);
        self.observation = None;
        active_confirmation
    }
    pub(in crate::app) fn reset_runtime(&mut self) -> bool {
        self.runtime_epoch = self.runtime_epoch.saturating_add(1);
        self.edits = PluginEditSources::default();
        let active_confirmation = self.confirmations.reset();
        self.edits = PluginEditSources::default();
        self.windows.clear_runtime();
        self.providers.reset_runtime();
        self.virtual_documents.reset_runtime();
        self.virtual_editor = documents::VirtualDocumentEditorBridge::default();
        self.active_picker = None;
        self.provider_sources.clear();
        self.ui.clear();
        self.results.clear();
        self.events.reset_runtime();
        self.observation = None;
        for index in self
            .plugin_contributions
            .ordered()
            .filter(|index| index.storage.is_some())
        {
            self.storage
                .reset_generation(&index.owner, PluginLoadId(index.generation));
        }
        active_confirmation
    }
    pub(in crate::app) fn queue_storage(&mut self, result: PluginStorageResult) {
        let pending = self
            .results
            .entry((result.plugin.clone(), result.load))
            .or_default();
        pending.push(result);
    }
    pub(in crate::app) fn request_storage(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        policy: &PluginStorageContribution,
        requests: impl IntoIterator<Item = PluginStorageRequest>,
    ) {
        for request in requests {
            let token = request.token;
            if let Err(error) = self
                .storage
                .request(plugin.into(), load, policy.clone(), request)
            {
                self.queue_storage(PluginStorageResult::rejected(
                    plugin.into(),
                    load,
                    token,
                    error,
                ));
            }
        }
    }
    pub(in crate::app) fn poll_storage(&mut self) {
        for result in self.storage.poll() {
            if self.manager.load_id(&result.plugin) == Some(result.load)
                && self
                    .manager
                    .is_active_generation(&result.plugin, result.load)
            {
                self.queue_storage(result);
            }
        }
    }
    pub(in crate::app) fn observe(
        &mut self,
        observation: HostObservation,
        roots: usize,
        retained: usize,
        private: usize,
    ) {
        if self.observation.as_ref() == Some(&observation)
            && self.observation_counts == Some((roots, retained, private))
        {
            return;
        }
        for index in self.plugin_contributions.ordered() {
            let plugin = &index.owner;
            let load = PluginLoadId(index.generation);
            let _ = self.events.observe(
                plugin,
                load,
                PluginSubscriptionKind::WorkspaceMetadataChanged,
                observation.0,
                format!("roots={roots}"),
            );
            let _ = self.events.observe(
                plugin,
                load,
                PluginSubscriptionKind::ActiveSurfaceChanged,
                observation.3,
                observation.1.clone(),
            );
            if let Some((id, revision, cursor, anchor, head, kind)) = observation.2 {
                let _ = self.events.observe(plugin, load, PluginSubscriptionKind::BufferSummaryChanged, revision, format!("buffer={id};revision={revision};cursor={cursor};selection={anchor}:{head}:{kind}"));
            } else {
                let _ = self.events.observe(
                    plugin,
                    load,
                    PluginSubscriptionKind::BufferSummaryChanged,
                    0,
                    "buffer=none",
                );
            }
            let _ = self.events.observe(
                plugin,
                load,
                PluginSubscriptionKind::PluginSurfaceChanged,
                observation.3,
                format!("retained={retained};private={private}"),
            );
        }
        self.observation = Some(observation);
        self.observation_counts = Some((roots, retained, private));
    }
}
#[cfg(test)]
impl ExtensionSession {
    pub(in crate::app) fn fixture_manager(&mut self, manager: PluginManager) {
        self.manager = manager;
        self.edits = PluginEditSources::default();
    }
    pub(in crate::app) fn events(&self) -> &PluginHostEventQueue {
        &self.events
    }
    pub(in crate::app) fn observation(&self) -> &Option<HostObservation> {
        &self.observation
    }
    pub(in crate::app) fn ui_events(&self) -> &BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>> {
        &self.ui
    }
    pub(in crate::app) fn storage_results(
        &self,
    ) -> &BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>> {
        &self.results
    }
    pub(in crate::app) fn confirmation_count(&self) -> usize {
        self.confirmations.pending_count()
    }
}

pub(in crate::app) mod edits;

pub(in crate::app) mod interaction;

pub(in crate::app) mod commands;

#[cfg(test)]
mod extensions_tests;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod surfaces_tests;
