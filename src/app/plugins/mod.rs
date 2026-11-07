//! Plugin delivery, storage and load ownership.
use crate::app::plugins::edits::PluginEditSources;
use crate::app::plugins::edits::PreparedPluginEdit;
use crate::app::plugins::edits::SourceExecution;
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::app::workspace::edits::OriginKey;
use crate::editor::EditorState;
use crate::plugins::OwnedPluginEvent;
use crate::plugins::PluginContributionCatalog;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginError;
use crate::plugins::PluginExecutionReply;
use crate::plugins::PluginExecutionRequest;
use crate::plugins::PluginHost;
use crate::plugins::PluginHostEventQueue;
use crate::plugins::PluginLimits;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginProviderBroker;
use crate::plugins::PluginStatus;
use crate::plugins::PluginStorageContribution;
use crate::plugins::PluginStorageRequest;
use crate::plugins::PluginStorageResult;
use crate::plugins::PluginStorageWorker;
use crate::plugins::PluginSubscriptionKind;
use crate::plugins::PluginUiEvent;
use crate::plugins::PluginVirtualDocumentBroker;
use crate::tools::PackageWorker;
use crate::tools::RegistryPackageMetadata;
use crate::tools::RegistryWorker;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;
pub(in crate::app) mod abi;
mod confirmations;
mod hosting;
pub(super) use confirmations::ConfirmationPresentation;
pub(in crate::app) mod integration;
mod panels;
pub(super) use hosting::WindowUpdate;
pub(super) use integration::ExtensionUpdate;
pub(super) use integration::RegistryUpdate;
pub(super) use panels::PluginUiInteraction;
#[cfg(test)]
pub(in crate::app) use panels::tests::fixture_manifest;
pub(in crate::app) mod content;
mod documents;
pub(super) use content::VirtualContext;
pub(super) use content::VirtualOpen;
pub(super) use content::VirtualPublication;

pub(in crate::app) struct InvocationContext<'a> {
    pub editor: &'a EditorState,
    pub workspace: &'a Workspace,
    pub configuration: &'a u64,
    pub request_id: u64,
    pub ui_revision: Option<u64>,
    pub input_capture: std::sync::Arc<()>,
    pub navigation: crate::app::windows::NavigationStamp,
    pub interaction_available: bool,
    pub maximum: usize,
    pub arguments: Vec<String>,
}

type BufferObservation = (u64, u64, usize, usize, usize, u8);
pub(in crate::app) type HostObservation = (u64, String, Option<BufferObservation>, u64);
pub(super) struct Plugins {
    abi: abi::PluginCalls,
    runtime_epoch: u64,
    windows: panels::PluginWindowHost,
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
    packages: PackageWorker,
    registry: RegistryWorker,
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
    manager: PluginHost,
    edits: PluginEditSources,
    ui: BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>>,
    events: PluginHostEventQueue,
    observation: Option<HostObservation>,
    observation_counts: Option<(usize, usize, usize)>,
    storage: PluginStorageWorker,
    results: BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>>,
}
impl Plugins {
    pub(in crate::app) fn new(
        root: PathBuf,
        capacity: usize,
        package_root: PathBuf,
        registry: RegistryWorker,
    ) -> Self {
        Self {
            abi: abi::PluginCalls::default(),
            runtime_epoch: 0,
            windows: panels::PluginWindowHost::default(),
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
            packages: PackageWorker::start(package_root, 8, 16 * 1024 * 1024, 64 * 1024 * 1024),
            manager: PluginHost::start(capacity),
            edits: PluginEditSources::default(),
            ui: BTreeMap::new(),
            events: PluginHostEventQueue::default(),
            observation: None,
            observation_counts: None,
            storage: PluginStorageWorker::start(root, capacity),
            results: BTreeMap::new(),
        }
    }
    pub(in crate::app) fn statuses(&self) -> &BTreeMap<String, PluginStatus> {
        &self.manager.statuses
    }
    pub(in crate::app) fn load_id(&self, id: &str) -> Option<PluginLoadId> {
        self.manager.load_id(id)
    }
    pub(in crate::app) fn is_active_load(&self, id: &str, load: PluginLoadId) -> bool {
        self.manager.is_active_load(id, load)
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
        // The manager discards late events for disabled loads, so they cannot
        // finish a pending activation or release its prepared package token.
        if let Some(load) = load {
            if self.package_startup_pending.get(plugin) == Some(&load) {
                self.package_startup_pending.remove(plugin);
            }
            self.abort_prepared(plugin, load);
        }
        Ok(())
    }
    pub(in crate::app) fn disable_load(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<(), PluginError> {
        self.manager.disable_load(plugin, load)
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
        if !self.manager.is_active_load(plugin, load) {
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
            call: Default::default(),
            delivery: Default::default(),
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
            request_id: context.request_id,
        };
        PluginHost::validate_invocation(&request).map_err(|error| error.to_string())?;
        let mut virtual_requests: crate::plugins::PluginVirtualRequestLane<'_> =
            self.virtual_documents.request_lane();
        // Validate before consuming deliveries; queue capacity stays reserved
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
    fn receive(
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
                source_actions_allowed: true,
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
    fn queue_confirmations(
        &mut self,
        source: crate::app::plugins::edits::ConfirmationSource,
        requests: Vec<crate::plugins::PluginConfirmationRequest>,
    ) -> Result<(), String> {
        self.confirmations.queue(source, requests, &self.manager)
    }
    pub(in crate::app) fn next_confirmation(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &crate::app::input::InputState,
        windows: &crate::app::windows::Windows,
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
    pub(in crate::app) fn invalidate_stale_confirmations(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        input: &crate::app::input::InputState,
        windows: &crate::app::windows::Windows,
    ) -> bool {
        self.confirmations
            .invalidate_stale(confirmations::ConfirmationContext {
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
    fn prepare_edit(
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
    pub(in crate::app) fn discard_stale_edits(
        &mut self,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> (usize, Option<OriginKey>) {
        self.edits
            .discard_stale(&self.manager, editor, workspace, configuration)
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
    pub(in crate::app) fn take_ui(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Vec<PluginUiEvent> {
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
        self.storage.install_load(plugin, load);
    }
    pub(in crate::app) fn reset_observation(&mut self) {
        self.observation = None;
    }
    pub(in crate::app) fn clear_events(&mut self) {
        self.events = PluginHostEventQueue::default();
        self.observation = None;
    }
    pub(in crate::app) fn remove_load(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        let active_confirmation = self.confirmations.remove_load(plugin, load);
        self.ui.remove(&(plugin.into(), load));
        self.results.remove(&(plugin.into(), load));
        self.events.remove_load(plugin, load);
        self.storage.remove_load(plugin, load);
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
                .reset_load(&index.owner, PluginLoadId(index.plugin_load));
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
                && self.manager.is_active_load(&result.plugin, result.load)
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
            let load = PluginLoadId(index.plugin_load);
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
impl Plugins {
    pub(in crate::app) fn fixture_manager(&mut self, manager: PluginHost) {
        self.manager = manager;
        self.edits = PluginEditSources::default();
    }
    pub(in crate::app) fn events(&self) -> &PluginHostEventQueue {
        &self.events
    }
    pub(in crate::app) fn observation(&self) -> &Option<HostObservation> {
        &self.observation
    }
    pub(in crate::app) fn ui_events(
        &self,
    ) -> &BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>> {
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

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerOrigin;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::ui::PickerSurfaceKind;
use crate::ui::PromptKind;

use crate::app::plugins::integration::compose_plugin_picker_items;

impl App {
    pub(in crate::app) fn apply_retained_ui_interaction(
        &mut self,
        interaction: PluginUiInteraction,
    ) {
        match self
            .extension
            .window_interaction(self.window_session.graph(), interaction)
        {
            Ok(action) => {
                self.ui.plugin_surfaces = self.extension.window_snapshots();
                if let Some(action) = action {
                    self.invoke_mapped_command(&action);
                }
            }
            Err(error) => self.show_error(error.detail),
        }
    }
}

impl App {
    pub(in crate::app) fn select_retained_ui_at_mouse(&mut self) {
        let Some((column, row)) = self.input.mouse_position() else {
            return;
        };
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let Some(window) = projection.hit_test(column, row) else {
            return;
        };
        let Some(rect) = projection.window(window).map(|projected| projected.rect) else {
            return;
        };
        if let Err(error) = self
            .extension
            .focus_window_row(window, usize::from(row.saturating_sub(rect.y)))
        {
            self.show_error(error.detail);
            return;
        }
        self.ui.plugin_surfaces = self.extension.window_snapshots();
    }
}

impl App {
    pub(in crate::app) fn activate_private_surface_action(&mut self, at_mouse: bool) {
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let target = if at_mouse {
            self.input.mouse_position().and_then(|(x, y)| {
                projection.hit_test(x, y).and_then(|window| {
                    projection.window(window).map(|projected| {
                        (
                            window,
                            x.saturating_sub(projected.rect.x),
                            y.saturating_sub(projected.rect.y),
                        )
                    })
                })
            })
        } else {
            self.window_session.graph().focused().and_then(|window| {
                self.ui
                    .private_surfaces
                    .iter()
                    .find(|surface| surface.window == window)
                    .and_then(|surface| surface.cursor.map(|(x, y)| (window, x, y)))
            })
        };
        let Some((window, x, y)) = target else { return };
        match self.extension.private_action(window, x, y) {
            Ok(Some(action)) => self.invoke_mapped_command(&action),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn present_next_plugin_confirmation(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        let Some(request) = self.extension.next_confirmation(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) else {
            let _ = self.extension.dispatch_confirmation_results(
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
            return;
        };
        let selected = match request.default_answer {
            crate::plugins::PluginConfirmationAnswer::Yes => crate::ui::ConfirmationChoice::Yes,
            crate::plugins::PluginConfirmationAnswer::No
            | crate::plugins::PluginConfirmationAnswer::Cancelled => {
                crate::ui::ConfirmationChoice::No
            }
        };
        self.open_confirmation(
            PromptKind::PluginConfirmation,
            format!("Plugin {} · {}", request.plugin, request.title),
            request.message,
            selected,
        );
    }
}

impl App {
    pub(in crate::app) fn submit_plugin_confirmation(
        &mut self,
        selected: crate::ui::ConfirmationChoice,
    ) {
        let answer = match selected {
            crate::ui::ConfirmationChoice::Yes => crate::plugins::PluginConfirmationAnswer::Yes,
            crate::ui::ConfirmationChoice::No => crate::plugins::PluginConfirmationAnswer::No,
        };
        self.extension.answer_confirmation(answer);
        self.close_prompt();
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
    }
}

impl App {
    pub(in crate::app) fn open_plugin_picker(&mut self, provider: String, query: String) {
        let invocation = match self.extension.start_provider(
            &provider,
            query,
            self.workspace.root_revision(),
            &self.state,
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {provider}"),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::PluginProvider(invocation.request),
        );
        if self.extension.provider_visible(invocation.request) {
            self.process_effect(AppEffect::PluginExecute {
                plugin: invocation.plugin,
                command: invocation.command,
                arguments: Vec::new(),
            });
        } else {
            self.extension
                .discard_unpresented_provider(invocation.request);
        }
    }
}

impl App {
    pub(in crate::app) fn execute_plugin_command(
        &mut self,
        plugin: String,
        command: String,
        arguments: Vec<String>,
    ) -> bool {
        let Some(load) = self.extension.load_id(&plugin) else {
            self.show_error(crate::plugins::PluginError::NotActive);
            return false;
        };
        if !self.extension.is_active_load(&plugin, load) {
            self.show_error(crate::plugins::PluginError::NotActive);
            return false;
        }
        self.refresh_plugin_host_events();
        self.discard_stale_plugin_edits();
        let maximum = SEARCH_MAXIMUM_FILE_BYTES;
        let input_capture = self.input.capture_epoch();
        let interaction_available = self.input.prompt().is_none()
            && self.ui.picker.is_none()
            && self.ui.tool_menu.is_none()
            && self.ui.settings.is_none()
            && self.input.documentation().is_none()
            && !self.input.mapping_pending()
            && !self.input.grammar_pending();
        let navigation = self
            .window_session
            .navigation_stamp(self.state.window_version(), self.state.active_view());
        let result = self.extension.execute(
            &plugin,
            &command,
            crate::app::plugins::InvocationContext {
                editor: &self.state,
                workspace: &self.workspace,
                configuration: self.configuration.binding_revision_ref(),
                request_id: self.configuration.reload_id(),
                ui_revision: self.window_session.graph().revision(),
                input_capture,
                navigation,
                interaction_available,
                maximum,
                arguments,
            },
        );
        if let Err(error) = result {
            self.show_error(error);
            return false;
        }
        true
    }
}

impl App {
    pub(in crate::app) fn present_plugin_picker(&mut self, request: u64) {
        if !self.extension.provider_visible(request) {
            return;
        }
        let Some(projection) = self.extension.provider_projection(request) else {
            return;
        };
        if let Some(error) = &projection.error {
            self.show_error(&error.detail);
        }
        let items = match compose_plugin_picker_items(&projection) {
            Ok(items) => items,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {}", projection.provider),
            items,
            String::new(),
            true,
            PickerOrigin::PluginProvider(request),
        );
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
