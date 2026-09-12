//! Generation-scoped extension delivery and storage runtime.
use super::{
    plugin_edits::{PluginEditSources, PreparedPluginEdit, SourceExecution},
    workspace::{WorkspaceSession, WorkspaceVersion},
    workspace_edits::OriginKey,
};
use crate::extensions::{
    PluginContributionCatalog, PluginContributionIndex, PluginError, PluginHostEventQueue,
    PluginLoadId, PluginManager, PluginStorageContribution, PluginStorageRequest,
    PluginStorageResult, PluginStorageService, PluginSubscriptionKind, PluginUiEvent,
    STORAGE_REQUEST_CEILING, STORAGE_RESULT_QUEUE_BYTE_CEILING,
};
use crate::tools::{PackageService, RegistryPackageMetadata, RegistryService};
use crate::{
    editor::EditorState,
    extensions::{
        OwnedPluginEvent, PluginExecutionReply, PluginExecutionRequest, PluginLimits,
        PluginProviderBroker, PluginStatus, PluginVirtualDocumentBroker,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
};
#[path = "confirmations.rs"]
mod confirmations;
#[path = "hosting.rs"]
mod hosting;
pub(super) use confirmations::ConfirmationPresentation;
#[path = "integration.rs"]
mod integration;
#[path = "windows/mod.rs"]
mod windows;
pub(super) use hosting::WindowUpdate;
pub(super) use integration::{ExtensionUpdate, RegistryUpdate};
pub(super) use windows::PluginUiInteraction;
#[cfg(test)]
pub(in crate::editor::session) use windows::tests::fixture_manifest;
#[path = "content.rs"]
mod content;
#[path = "documents.rs"]
mod documents;
pub(super) use content::{VirtualContext, VirtualOpen, VirtualPublication};

pub(in crate::editor::session) struct InvocationContext<'a> {
    pub editor: &'a EditorState,
    pub workspace: &'a WorkspaceSession,
    pub configuration: &'a u64,
    pub generation: u64,
    pub ui_revision: Option<u64>,
    pub input_capture: std::sync::Arc<()>,
    pub navigation: super::windows::NavigationStamp,
    pub interaction_available: bool,
    pub maximum: usize,
}

type BufferObservation = (u64, u64, usize, usize, usize, u8);
pub(in crate::editor::session) type HostObservation = (u64, String, Option<BufferObservation>, u64);
enum ValidationIntent {
    InstallRegistry {
        publisher: String,
        name: String,
        version: String,
    },
    PrepareRegistryArtifact,
    InstallPackage {
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    },
}
pub(super) struct ExtensionSession {
    windows: windows::PluginWindowHost,
    providers: PluginProviderBroker,
    virtual_documents: PluginVirtualDocumentBroker,
    virtual_editor: documents::VirtualDocumentEditorBridge,
    confirmations: confirmations::PluginConfirmations,
    active_picker: Option<u64>,
    plugin_contributions: PluginContributionCatalog,
    pending_index_activation: BTreeMap<String, (PluginLoadId, String)>,
    rejected_plugin_loads: BTreeSet<(String, PluginLoadId)>,
    packages: PackageService,
    registry: RegistryService,
    registry_intents: HashMap<u64, bool>,
    pending_registry_artifact: Option<(RegistryPackageMetadata, Vec<u8>)>,
    package_prepared: HashMap<String, (PluginLoadId, u64)>,
    package_commits: HashMap<u64, (String, PluginLoadId)>,
    package_startup_pending: HashMap<String, PluginLoadId>,
    package_managed_plugins: HashMap<String, PluginLoadId>,
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
    storage: PluginStorageService,
    results: BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>>,
    validation: Option<(super::workspace::ValidationId, ValidationIntent)>,
}
impl ExtensionSession {
    pub(super) fn defer_registry_validation(
        &mut self,
        id: super::workspace::ValidationId,
        publisher: String,
        name: String,
        version: String,
    ) {
        debug_assert!(self.validation.is_none());
        self.validation = Some((
            id,
            ValidationIntent::InstallRegistry {
                publisher,
                name,
                version,
            },
        ));
    }
    pub(super) fn defer_artifact_validation(&mut self, id: super::workspace::ValidationId) {
        debug_assert!(self.validation.is_none());
        self.validation = Some((id, ValidationIntent::PrepareRegistryArtifact));
    }
    pub(super) fn defer_package_validation(
        &mut self,
        id: super::workspace::ValidationId,
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    ) {
        debug_assert!(self.validation.is_none());
        self.validation = Some((
            id,
            ValidationIntent::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            },
        ));
    }
    pub(super) fn take_validation(
        &mut self,
        id: super::workspace::ValidationId,
    ) -> Option<crate::editor::session::DeferredExecution> {
        let matches = self
            .validation
            .as_ref()
            .is_some_and(|(expected, _)| *expected == id);
        matches.then(|| {
            match self
                .validation
                .take()
                .expect("matched extension validation")
                .1
            {
                ValidationIntent::InstallRegistry {
                    publisher,
                    name,
                    version,
                } => crate::editor::session::DeferredExecution::InstallRegistry {
                    publisher,
                    name,
                    version,
                },
                ValidationIntent::PrepareRegistryArtifact => {
                    crate::editor::session::DeferredExecution::PrepareRegistryArtifact
                }
                ValidationIntent::InstallPackage {
                    package_manifest,
                    plugin_manifest,
                    component,
                    update,
                } => crate::editor::session::DeferredExecution::InstallPackage {
                    package_manifest,
                    plugin_manifest,
                    component,
                    update,
                },
            }
        })
    }
    pub(super) fn new(
        root: PathBuf,
        capacity: usize,
        package_root: PathBuf,
        registry: RegistryService,
    ) -> Self {
        Self {
            windows: windows::PluginWindowHost::default(),
            providers: PluginProviderBroker::default(),
            virtual_documents: PluginVirtualDocumentBroker::default(),
            virtual_editor: documents::VirtualDocumentEditorBridge::default(),
            confirmations: confirmations::PluginConfirmations::default(),
            active_picker: None,
            plugin_contributions: PluginContributionCatalog::default(),
            pending_index_activation: BTreeMap::new(),
            rejected_plugin_loads: BTreeSet::new(),
            registry_intents: HashMap::new(),
            pending_registry_artifact: None,
            package_prepared: HashMap::new(),
            package_commits: HashMap::new(),
            package_startup_pending: HashMap::new(),
            package_managed_plugins: HashMap::new(),
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
            storage: PluginStorageService::start(root, capacity),
            results: BTreeMap::new(),
            validation: None,
        }
    }
    pub(super) fn statuses(&self) -> &BTreeMap<String, PluginStatus> {
        &self.manager.statuses
    }
    pub(super) fn load_id(&self, id: &str) -> Option<PluginLoadId> {
        self.manager.load_id(id)
    }
    pub(super) fn is_active_generation(&self, id: &str, load: PluginLoadId) -> bool {
        self.manager.is_active_generation(id, load)
    }
    pub(super) fn index_from_store(
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
    pub(super) fn disable(&mut self, plugin: &str) -> Result<(), PluginError> {
        self.manager.disable(plugin)
    }
    pub(super) fn disable_generation(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<(), PluginError> {
        self.manager.disable_generation(plugin, load)
    }
    pub(super) fn poll_owned(&mut self) -> Result<Vec<OwnedPluginEvent>, PluginError> {
        self.manager.poll_owned()
    }
    pub(super) fn execute(
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
        let mut virtual_requests: crate::extensions::PluginVirtualRequestLane<'_> =
            self.virtual_documents.request_lane();
        // Real manager/source admission stays borrowed while delivery and snapshot
        // payloads are built and queued. Full/retired refusal leaves queues intact.
        reservation.enqueue(|| {
            loan.submit(PluginExecutionRequest {
                plugin: plugin.into(),
                command: command.into(),
                arguments: Vec::new(),
                snapshot: None,
                config: BTreeMap::new(),
                ui_revision: context.ui_revision,
                confirmation_available,
                ui_events: self.ui.remove(&(plugin.into(), load)).unwrap_or_default(),
                confirmation_results: Vec::new(),
                host_events: self.events.deliver(plugin, load),
                storage_results: self
                    .results
                    .remove(&(plugin.into(), load))
                    .unwrap_or_default(),
                provider_requests: self.providers.requests(plugin, load),
                virtual_document_requests: virtual_requests.deliver(
                    plugin,
                    load,
                    &format!("plugin.{plugin}.{command}"),
                ),
                generation: context.generation,
            })
        })
    }
    pub(super) fn receive(
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
        if received.is_err() {
            if let Some(origin) = origin {
                self.confirmations.release_terminal(&origin);
            }
        }
        received
    }
    pub(super) fn admit_confirmations(
        &mut self,
        source: super::plugin_edits::ConfirmationSource,
        requests: Vec<crate::extensions::PluginConfirmationRequest>,
    ) -> Result<(), String> {
        self.confirmations.admit(source, requests, &self.manager)
    }
    pub(super) fn next_confirmation(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &super::input::InputSession,
        windows: &super::windows::WindowSession,
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
    pub(super) fn answer_confirmation(
        &mut self,
        answer: crate::extensions::PluginConfirmationAnswer,
    ) -> bool {
        self.confirmations.answer(answer)
    }
    pub(super) fn dispatch_confirmation_results(
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
        )
    }
    pub(super) fn retire_stale_confirmations(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &super::input::InputSession,
        windows: &super::windows::WindowSession,
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
    pub(super) fn owns_edit(&self, key: &OriginKey) -> bool {
        self.edits.owns(key)
    }
    pub(super) fn check_edit(
        &self,
        key: &OriginKey,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        self.edits
            .check(key, &self.manager, editor, workspace, configuration)
    }
    pub(super) fn prepare_edit(
        &mut self,
        key: &OriginKey,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<PreparedPluginEdit, String> {
        self.edits
            .prepare_preview(key, &self.manager, editor, workspace, configuration)
    }
    pub(super) fn finish_edit(&mut self, key: &OriginKey) -> Result<(), String> {
        self.edits.finish(key)
    }
    pub(super) fn retire_edits(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> (usize, Option<OriginKey>) {
        self.edits
            .retire(&self.manager, editor, workspace, configuration)
    }
    pub(super) fn close_edits(&mut self) -> usize {
        self.edits.close()
    }
    pub(super) fn push_action(
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
    pub(super) fn queue_ui(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        events: impl IntoIterator<Item = PluginUiEvent>,
    ) {
        let pending = self.ui.entry((plugin, load)).or_default();
        for event in events {
            if pending.len() == 64 {
                pending.remove(0);
            }
            pending.push(event);
        }
    }
    #[cfg(test)]
    pub(super) fn take_ui(&mut self, plugin: &str, load: PluginLoadId) -> Vec<PluginUiEvent> {
        self.ui.remove(&(plugin.into(), load)).unwrap_or_default()
    }
    #[cfg(test)]
    pub(super) fn deliver(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Vec<crate::extensions::PluginHostEventEnvelope> {
        self.events.deliver(plugin, load)
    }
    pub(super) fn acknowledge(&mut self, plugin: &str, load: PluginLoadId, ids: &[u64]) {
        self.events.acknowledge(plugin, load, ids);
    }
    pub(super) fn install_events(
        &mut self,
        index: &PluginContributionIndex,
    ) -> Result<(), PluginError> {
        self.observation = None;
        self.events.install(index)
    }
    pub(super) fn install_storage(&mut self, plugin: &str, load: PluginLoadId) {
        self.storage.install_generation(plugin, load);
    }
    pub(super) fn reset_observation(&mut self) {
        self.observation = None;
    }
    pub(super) fn clear_events(&mut self) {
        self.events = PluginHostEventQueue::default();
        self.observation = None;
    }
    pub(super) fn remove_generation(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        let active_confirmation = self.confirmations.remove_generation(plugin, load);
        self.ui.remove(&(plugin.into(), load));
        self.results.remove(&(plugin.into(), load));
        self.events.remove_generation(plugin, load);
        self.storage.remove_generation(plugin, load);
        self.observation = None;
        active_confirmation
    }
    pub(super) fn reset_runtime(&mut self) -> bool {
        let active_confirmation = self.confirmations.reset();
        self.windows.clear_runtime();
        self.providers.reset_runtime();
        self.virtual_documents.reset_runtime();
        self.virtual_editor = documents::VirtualDocumentEditorBridge::default();
        self.active_picker = None;
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
    pub(super) fn queue_storage(&mut self, result: PluginStorageResult) {
        let pending = self
            .results
            .entry((result.plugin.clone(), result.load))
            .or_default();
        pending.push(result);
        while pending.len() > STORAGE_REQUEST_CEILING
            || pending
                .iter()
                .map(PluginStorageResult::bounded_bytes)
                .sum::<usize>()
                > STORAGE_RESULT_QUEUE_BYTE_CEILING
        {
            pending.remove(0);
        }
    }
    pub(super) fn request_storage(
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
    pub(super) fn poll_storage(&mut self) {
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
    pub(super) fn observe(
        &mut self,
        observation: HostObservation,
        roots: usize,
        retained: usize,
        private: usize,
    ) {
        if self.observation.as_ref() == Some(&observation) {
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
    }
}
#[cfg(test)]
impl ExtensionSession {
    pub(super) fn fixture_manager(&mut self, manager: PluginManager) {
        self.manager = manager;
        self.edits = PluginEditSources::default();
    }
    pub(super) fn events(&self) -> &PluginHostEventQueue {
        &self.events
    }
    pub(super) fn observation(&self) -> &Option<HostObservation> {
        &self.observation
    }
    pub(super) fn ui_events(&self) -> &BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>> {
        &self.ui
    }
    pub(super) fn storage_results(
        &self,
    ) -> &BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>> {
        &self.results
    }
    pub(super) fn confirmation_count(&self) -> usize {
        self.confirmations.pending_count()
    }
}
