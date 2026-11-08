//! Extension catalog and package/registry transitions; editor publication stays in dispatch.
use crate::app::plugins::Plugins;
use crate::plugins::PluginContributionCatalog;
use crate::plugins::PluginContributionIndex;
use crate::plugins::PluginLimits;
use crate::plugins::PluginLoadId;
use crate::tools::PackageError;
use crate::tools::PackageEvent;
use crate::tools::PackageLock;
use crate::tools::PackageOperation;
use crate::tools::RegistryEvent;
use crate::tools::RegistryRequest;
use crate::tools::RegistryResult;
use std::collections::BTreeMap;
use std::collections::HashMap;

pub(in crate::app) enum ExtensionUpdate {
    Remove(String, PluginLoadId),
    Error(String),
    Conflict(String),
    Message(&'static str, Vec<(&'static str, String)>),
    Lock(PackageLock),
    Failed { operation: String, error: String },
}
pub(in crate::app) enum RegistryUpdate {
    Present(Box<RegistryResult>, Option<crate::app::picker::PickerEpoch>),
    ValidateArtifact,
    Error(String),
}
impl Plugins {
    pub(in crate::app) fn pending_packages(&self) -> usize {
        self.packages.pending_count()
            + self.package_aborts.len()
            + usize::from(self.package_refresh)
    }
    pub(in crate::app) fn contributions(&self) -> &PluginContributionCatalog {
        &self.plugin_contributions
    }
    pub(in crate::app) fn install_index(
        &mut self,
        index: PluginContributionIndex,
    ) -> Result<(), String> {
        self.plugin_contributions
            .install(index.clone())
            .map_err(|e| e.to_string())?;
        self.install_events(&index).map_err(|e| e.to_string())?;
        self.providers.install(&index).map_err(|e| e.detail)?;
        self.virtual_documents.install(&index).map_err(|e| e.detail)
    }
    pub(in crate::app) fn remove_contribution(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        self.plugin_contributions.remove_load(plugin, load.0)
    }
    pub(in crate::app) fn take_rejected(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        self.rejected_plugin_loads.remove(&(plugin.into(), load))
    }
    pub(in crate::app) fn index_pending(&self, plugin: &str) -> bool {
        self.pending_index_activation.contains_key(plugin)
    }
    pub(in crate::app) fn queue_index(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        checksum: String,
    ) {
        self.pending_index_activation
            .insert(plugin, (load, checksum));
    }
    pub(in crate::app) fn activate_pending(
        &mut self,
        deferred: bool,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<(), (String, PluginLoadId, String)> {
        let activations = if deferred {
            self.pending_index_activation.clone()
        } else {
            let Some((candidate, checksum)) = self
                .pending_index_activation
                .get(plugin)
                .filter(|(candidate, _)| *candidate == load)
                .cloned()
            else {
                return Ok(());
            };
            BTreeMap::from([(plugin.into(), (candidate, checksum))])
        };
        for (plugin, (load, checksum)) in activations {
            let result = self.manager.activate_indexed(&plugin, load, checksum);
            self.pending_index_activation.remove(&plugin);
            result.map_err(|e| (plugin, load, e.to_string()))?;
        }
        Ok(())
    }
    pub(in crate::app) fn reject_index(&mut self, plugin: String, load: PluginLoadId) {
        self.rejected_plugin_loads.insert((plugin, load));
    }
    pub(in crate::app) fn reject_expected(
        &mut self,
        expected: impl Iterator<Item = String>,
        failed: Option<(&str, PluginLoadId)>,
    ) {
        for plugin in expected {
            if let Some(load) = failed
                .filter(|(owner, _)| plugin == *owner)
                .map(|(_, load)| load)
                .or_else(|| self.manager.load_id(&plugin))
            {
                self.reject_index(plugin.clone(), load);
                self.pending_index_activation
                    .entry(plugin)
                    .or_insert_with(|| (load, String::new()));
            }
        }
    }
    pub(in crate::app) fn abort_indexes(&mut self) -> Vec<(String, PluginLoadId)> {
        let pending = std::mem::take(&mut self.pending_index_activation);
        pending
            .into_iter()
            .map(|(plugin, (load, _))| {
                self.reject_index(plugin.clone(), load);
                let _ = self.manager.disable_load(&plugin, load);
                self.abort_prepared(&plugin, load);
                (plugin, load)
            })
            .collect()
    }
    pub(in crate::app) fn request_package(
        &mut self,
        operation: PackageOperation,
    ) -> Result<u64, PackageError> {
        // Serialize mutations through preparation, plugin loading, and commit acknowledgement.
        if matches!(
            operation,
            PackageOperation::Install { .. }
                | PackageOperation::PrepareRegistry { .. }
                | PackageOperation::Rollback
                | PackageOperation::Remove(_)
        ) && (self.pending_packages() != 0
            || !self.package_preparing.is_empty()
            || !self.package_prepared.is_empty()
            || !self.package_commits.is_empty()
            || !self.package_startup_pending.is_empty())
        {
            return Err(PackageError::Busy);
        }
        if matches!(
            operation,
            PackageOperation::Rollback | PackageOperation::Remove(_)
        ) {
            self.package_startup_rollback_attempted = false;
        }
        if let PackageOperation::Abort { token } = operation {
            // Keep rejected aborts for the next poll; enqueueing transfers the token to the worker.
            self.package_aborts.insert(token);
            let id = self.packages.request(PackageOperation::Abort { token })?;
            self.package_aborts.remove(&token);
            return Ok(id);
        }
        let preparing = matches!(
            operation,
            PackageOperation::Install { .. } | PackageOperation::PrepareRegistry { .. }
        );
        let id = self.packages.request(operation)?;
        if preparing {
            self.package_preparing.insert(id);
        }
        Ok(id)
    }
    pub(in crate::app) fn start_packages(&mut self) {
        self.package_startup_request = self.packages.request(PackageOperation::Activations).ok();
    }
    pub(in crate::app) fn request_registry(
        &mut self,
        request: RegistryRequest,
        install: bool,
        picker: Option<crate::app::picker::PickerEpoch>,
    ) -> Result<(), String> {
        let id = self.registry.request(request).map_err(|e| e.to_string())?;
        self.registry_intents.insert(id, (install, picker));
        Ok(())
    }
    fn accept_registry(&mut self, event: RegistryEvent) -> Option<RegistryUpdate> {
        // A stale workspace/request must not turn a late artifact into a new install.
        let (install, picker) = self.registry_intents.remove(&event.id)?;
        Some(match event.result {
            Ok(RegistryResult::Show(metadata)) if install => match self.request_registry(
                RegistryRequest::Fetch {
                    metadata: Box::new(metadata),
                },
                true,
                None,
            ) {
                Ok(()) => return None,
                Err(error) => RegistryUpdate::Error(error),
            },
            Ok(RegistryResult::Artifact { metadata, bytes }) => {
                if !install {
                    RegistryUpdate::Error(
                        "registry artifact was not requested for installation".into(),
                    )
                } else if self.pending_registry_artifact.is_some() {
                    RegistryUpdate::Error("another registry artifact is awaiting validation".into())
                } else {
                    self.pending_registry_artifact = Some((metadata, bytes));
                    RegistryUpdate::ValidateArtifact
                }
            }
            Ok(result) => RegistryUpdate::Present(Box::new(result), picker),
            Err(error) => RegistryUpdate::Error(error.to_string()),
        })
    }
    pub(in crate::app) fn poll_registry(&mut self) -> Vec<RegistryUpdate> {
        self.registry
            .poll()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| self.accept_registry(event))
            .collect()
    }
    pub(in crate::app) fn discard_artifact(&mut self) {
        self.pending_registry_artifact = None;
    }
    pub(in crate::app) fn prepare_artifact(
        &mut self,
        limits: PluginLimits,
    ) -> Result<(), PackageError> {
        if let Some((metadata, component)) = self.pending_registry_artifact.take() {
            self.request_package(PackageOperation::PrepareRegistry {
                metadata: Box::new(metadata),
                component,
                limits,
            })?;
        }
        Ok(())
    }
    pub(in crate::app) fn take_prepared(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Option<u64> {
        if self.package_startup_pending.get(plugin) == Some(&load) {
            self.package_startup_pending.remove(plugin);
        }
        let (_, token) = self
            .package_prepared
            .get(plugin)
            .filter(|(candidate, _)| *candidate == load)?;
        let token = *token;
        self.package_prepared.remove(plugin);
        Some(token)
    }
    fn abort_token(&mut self, token: u64) {
        self.package_refresh = true;
        let _ = self.request_package(PackageOperation::Abort { token });
    }
    pub(in crate::app) fn abort_prepared(&mut self, plugin: &str, load: PluginLoadId) {
        if let Some((_, token)) = self
            .package_prepared
            .get(plugin)
            .filter(|(candidate, _)| *candidate == load)
            .copied()
        {
            self.package_prepared.remove(plugin);
            self.abort_token(token);
        }
    }
    pub(in crate::app) fn commit_package(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        token: u64,
    ) -> Result<(), PackageError> {
        match self.packages.request(PackageOperation::Commit { token }) {
            Ok(id) => {
                self.package_commits.insert(id, (plugin, load));
                Ok(())
            }
            Err(error) => {
                let _ = self.manager.disable_load(&plugin, load);
                self.abort_token(token);
                Err(error)
            }
        }
    }
    pub(in crate::app) fn failed_package_load(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> Vec<ExtensionUpdate> {
        if self
            .package_prepared
            .get(plugin)
            .is_some_and(|(candidate, _)| *candidate == load)
        {
            self.abort_prepared(plugin, load);
            Vec::new()
        } else if self.package_startup_pending.get(plugin) == Some(&load) {
            self.package_startup_pending.remove(plugin);
            self.rollback_packages()
        } else {
            Vec::new()
        }
    }
    fn unload_managed(&mut self, updates: &mut Vec<ExtensionUpdate>) {
        let mut failed = HashMap::new();
        for (plugin, load) in std::mem::take(&mut self.package_managed_plugins) {
            match self.manager.unload_load(&plugin, load) {
                Ok(()) => updates.push(ExtensionUpdate::Remove(plugin, load)),
                Err(error) => {
                    failed.insert(plugin, load);
                    updates.push(ExtensionUpdate::Error(error.to_string()));
                }
            }
        }
        self.package_managed_plugins = failed;
    }
    pub(in crate::app) fn rollback_packages(&mut self) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        if self.package_startup_rollback.is_some() || self.package_startup_rollback_attempted {
            return updates;
        }
        if self.pending_packages() != 0
            || !self.package_preparing.is_empty()
            || !self.package_prepared.is_empty()
            || !self.package_commits.is_empty()
        {
            updates.push(ExtensionUpdate::Error(PackageError::Busy.to_string()));
            return updates;
        }
        self.unload_managed(&mut updates);
        if !self.package_managed_plugins.is_empty() {
            return updates;
        }
        self.package_startup_pending.clear();
        match self.packages.request(PackageOperation::Rollback) {
            Ok(id) => {
                self.package_startup_rollback_attempted = true;
                self.package_startup_rollback = Some(id);
            }
            Err(error) => updates.push(ExtensionUpdate::Error(error.to_string())),
        }
        updates
    }
    pub(in crate::app) fn cancel_pending_packages(&mut self) -> Vec<(String, PluginLoadId)> {
        self.registry_intents.clear();
        self.pending_registry_artifact = None;
        self.package_preparing.clear();
        let mut removed = Vec::new();
        for (plugin, (load, token)) in std::mem::take(&mut self.package_prepared) {
            let _ = self.manager.unload_load(&plugin, load);
            self.abort_token(token);
            removed.push((plugin, load));
        }
        for (plugin, load) in self.package_commits.values() {
            let _ = self.manager.unload_load(plugin, *load);
            removed.push((plugin.clone(), *load));
        }
        removed
    }
    pub(in crate::app) fn worker_stopped(&mut self) -> Option<Vec<String>> {
        if self.plugin_worker_reported {
            return None;
        }
        self.plugin_worker_reported = true;
        self.pending_index_activation.clear();
        self.rejected_plugin_loads.clear();
        let mut unregister = self.manager.statuses.keys().cloned().collect::<Vec<_>>();
        for (plugin, (_, token)) in std::mem::take(&mut self.package_prepared) {
            unregister.push(plugin);
            self.abort_token(token);
        }
        unregister.extend(self.package_startup_pending.keys().cloned());
        unregister.extend(self.package_managed_plugins.keys().cloned());
        unregister.extend(
            self.package_commits
                .values()
                .map(|(plugin, _)| plugin.clone()),
        );
        self.package_startup_pending.clear();
        self.package_managed_plugins.clear();
        Some(unregister)
    }
    pub(in crate::app) fn clear_contributions(&mut self) {
        self.plugin_contributions = PluginContributionCatalog::default();
        self.clear_events();
    }
    fn load_prepared(
        &mut self,
        request: u64,
        prepared: crate::tools::PreparedPackage,
        limits: &PluginLimits,
    ) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        let plugin = prepared.plugin.id.clone();
        let token = prepared.token;
        if !self.package_preparing.remove(&request) {
            self.abort_token(token);
            return updates;
        }
        if let Some(load) = self.manager.load_id(&plugin) {
            if self.package_managed_plugins.get(&plugin) != Some(&load) {
                self.abort_token(token);
                updates.push(ExtensionUpdate::Conflict(plugin));
                return updates;
            }
            // The installed lock retains the previous version until Commit succeeds.
            if let Err(error) = self.manager.unload_load(&plugin, load) {
                self.abort_token(token);
                updates.push(ExtensionUpdate::Error(error.to_string()));
                return updates;
            }
            self.package_managed_plugins.remove(&plugin);
            updates.push(ExtensionUpdate::Remove(plugin.clone(), load));
        }
        match self.manager.load(
            self.package_store.clone(),
            prepared.object,
            prepared.plugin,
            limits.clone(),
        ) {
            Ok(load) => {
                self.package_prepared.insert(plugin, (load, token));
                updates.push(ExtensionUpdate::Message(
                    "package_prepared",
                    vec![("package", prepared.manifest.name)],
                ));
            }
            Err(error) => {
                self.abort_token(token);
                updates.push(ExtensionUpdate::Error(error.to_string()));
            }
        }
        updates
    }
    fn load_activations(
        &mut self,
        activations: Vec<(
            crate::plugins::PluginManifest,
            std::path::PathBuf,
            std::sync::Arc<std::fs::File>,
        )>,
        limits: &PluginLimits,
        config: &crate::config::PluginsConfig,
        safe_mode: bool,
    ) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        self.browser_dirty = true;
        self.browser_packages = activations
            .iter()
            .map(|(manifest, path, _)| {
                (
                    manifest.id.clone(),
                    crate::app::plugins::browser::BrowserPlugin {
                        version: manifest.version.clone(),
                        capabilities: manifest.capabilities.clone(),
                        folder: path.parent().unwrap_or(&self.package_store).to_owned(),
                        error: None,
                    },
                )
            })
            .collect();
        if safe_mode {
            return updates;
        }
        for (manifest, path, object_lock) in activations {
            let plugin = manifest.id.clone();
            // A failed load may disable a load too; only explicit disablement persists.
            if (config
                .entries
                .get(&plugin)
                .is_some_and(|entry| !entry.enabled)
                && !self.enabled_plugins.contains(&plugin))
                || self.disabled_plugins.contains(&plugin)
            {
                continue;
            }
            if let Some(load) = self.manager.load_id(&plugin) {
                if self.package_managed_plugins.get(&plugin) == Some(&load) {
                    continue;
                }
                updates.push(ExtensionUpdate::Conflict(plugin));
                continue;
            }
            match self.manager.load_protected(
                self.package_store.clone(),
                path,
                manifest,
                limits.clone(),
                Some(object_lock),
            ) {
                Ok(load) => {
                    self.package_startup_pending.insert(plugin.clone(), load);
                    self.package_managed_plugins.insert(plugin, load);
                }
                Err(error) => {
                    updates.push(ExtensionUpdate::Error(error.to_string()));
                    updates.extend(self.rollback_packages());
                    break;
                }
            }
        }
        updates
    }
    pub(in crate::app) fn poll_packages(
        &mut self,
        limits: PluginLimits,
        config: &crate::config::PluginsConfig,
        safe_mode: bool,
    ) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        for event in self.packages.poll().unwrap_or_default() {
            match event {
                PackageEvent::Completed(mut result) => {
                    if let Some(error) = result.cleanup_error.take() {
                        updates.push(ExtensionUpdate::Error(format!(
                            "package cleanup failed: {error}"
                        )));
                    }
                    let prepared_operation = result.prepared.is_some();
                    let activation_operation = result.operation == "activations";
                    if let Some(prepared) = result.prepared.take() {
                        if !safe_mode
                            && config
                                .entries
                                .get(&prepared.plugin.id)
                                .is_none_or(|entry| entry.enabled)
                            && !self.disabled_plugins.contains(&prepared.plugin.id)
                        {
                            updates.extend(self.load_prepared(result.id, prepared, &limits));
                        } else {
                            self.package_preparing.remove(&result.id);
                            let _ = self.request_package(PackageOperation::Abort {
                                token: prepared.token,
                            });
                        }
                    }
                    updates.extend(self.load_activations(
                        std::mem::take(&mut result.activations),
                        &limits,
                        config,
                        safe_mode,
                    ));
                    if let Some((plugin, load)) = self.package_commits.remove(&result.id) {
                        if self.manager.load_id(&plugin) == Some(load) {
                            self.package_managed_plugins.insert(plugin, load);
                        } else {
                            self.package_startup_rollback_attempted = false;
                            updates.extend(self.rollback_packages());
                        }
                    }
                    if self.package_startup_request == Some(result.id) {
                        self.package_startup_request = None;
                    }
                    if self.package_startup_rollback == Some(result.id) {
                        self.package_startup_rollback = None;
                    }
                    if matches!(result.operation.as_str(), "rollback" | "remove") {
                        self.browser_packages.clear();
                        self.browser_dirty = true;
                        self.unload_managed(&mut updates);
                        self.package_startup_pending.clear();
                        if !self.package_managed_plugins.is_empty() {
                            updates.extend(self.rollback_packages());
                            continue;
                        }
                        match (!safe_mode)
                            .then(|| self.packages.request(PackageOperation::Activations))
                        {
                            Some(Ok(id)) => self.package_startup_request = Some(id),
                            None => {}
                            Some(Err(error)) => {
                                updates.push(ExtensionUpdate::Error(error.to_string()));
                                updates.extend(self.rollback_packages());
                            }
                        }
                    }
                    if let Some(lock) = result.lock {
                        updates.push(ExtensionUpdate::Lock(lock));
                    } else if result.operation == "verify" && result.verified == Some(0) {
                        updates.push(ExtensionUpdate::Message(
                            "package_nothing_to_verify",
                            Vec::new(),
                        ));
                    } else if !prepared_operation && !activation_operation {
                        updates.push(ExtensionUpdate::Message(
                            "package_operation_complete",
                            vec![
                                ("operation", result.operation),
                                (
                                    "count",
                                    result.verified.or(result.removed).unwrap_or(0).to_string(),
                                ),
                            ],
                        ));
                    }
                }
                PackageEvent::Failed {
                    id,
                    operation,
                    error,
                } => {
                    self.package_preparing.remove(&id);
                    if self.package_startup_rollback == Some(id) {
                        self.package_startup_rollback = None;
                    }
                    if self.package_startup_request == Some(id) {
                        self.package_startup_request = None;
                        updates.extend(self.rollback_packages());
                    }
                    if let Some((plugin, load)) = self.package_commits.remove(&id) {
                        if self.package_managed_plugins.get(&plugin) == Some(&load) {
                            self.package_managed_plugins.remove(&plugin);
                        }
                        let _ = self.manager.disable_load(&plugin, load);
                        updates.push(ExtensionUpdate::Remove(plugin, load));
                        self.package_startup_request =
                            self.packages.request(PackageOperation::Activations).ok();
                    }
                    updates.push(ExtensionUpdate::Failed {
                        operation,
                        error: error.to_string(),
                    });
                }
            }
        }
        for token in self.package_aborts.clone() {
            if self
                .request_package(PackageOperation::Abort { token })
                .is_err()
            {
                break;
            }
        }
        if self.package_refresh
            && self.package_aborts.is_empty()
            && self.package_startup_request.is_none()
            && let Ok(id) = self.packages.request(PackageOperation::Activations)
        {
            self.package_startup_request = Some(id);
            self.package_refresh = false;
        }
        updates
    }
}
#[cfg(test)]
impl Plugins {
    pub(in crate::app) fn fixture_contribution(
        &mut self,
        index: PluginContributionIndex,
    ) -> Result<(), crate::plugins::PluginError> {
        self.plugin_contributions.install(index)
    }
    pub(in crate::app) fn fixture_prepared(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        token: u64,
    ) {
        self.package_prepared.insert(plugin.into(), (load, token));
    }
    pub(in crate::app) fn fixture_package_reload_pending(&self) -> bool {
        self.package_startup_request.is_some()
    }
    pub(in crate::app) fn fixture_pending_index(
        &mut self,
        plugin: String,
        source: (PluginLoadId, String),
    ) {
        self.queue_index(plugin, source.0, source.1);
    }
    pub(in crate::app) fn pending_indexes(&self) -> &BTreeMap<String, (PluginLoadId, String)> {
        &self.pending_index_activation
    }
    pub(in crate::app) fn rejected_indexes(
        &self,
    ) -> &std::collections::BTreeSet<(String, PluginLoadId)> {
        &self.rejected_plugin_loads
    }
}

use crate::app::commands::AppEffect;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::editor::ArgumentSchema;
use crate::ui::render::SCRATCH_LABEL;

use crate::app::App;
use crate::app::picker::PickerPayload;
use crate::app::windows::NativeDescriptions;
use crate::editor::CommandId;
use crate::editor::CommandOutcome;
use crate::editor::CommandSource;
use crate::editor::CommandSpec;
use crate::editor::ContextRequirement;
use crate::editor::EditorState;
use crate::plugins::PluginEvent;
use crate::telemetry::LogLevel;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use std::collections::BTreeSet;

use crate::app::plugins::VirtualContext;
use crate::app::plugins::VirtualOpen;
use crate::app::plugins::VirtualPublication;
use crate::app::plugins::WindowUpdate;
use crate::app::search::workflow::ABS_MAX_BYTES;
use crate::app::search::workflow::SearchMatcherKind;
use crate::app::search::workflow::SearchPresenterKind;
use crate::app::search::workflow::SearchRequestId;
use crate::app::search::workflow::SearchSourceKind;
use crate::app::search::workflow::SearchStreamId;
use crate::app::search::workflow::SearchWorkflowBroker;
use crate::app::search::workflow::SearchWorkflowChunk;
use crate::app::search::workflow::SearchWorkflowError;
use crate::app::search::workflow::SearchWorkflowId;
use crate::app::search::workflow::SearchWorkflowSpec;
use crate::app::search::workflow::picker_item_adapter;
use crate::app::search::workflow::plugin_item_adapter;
use crate::ui::PromptKind;

pub(in crate::app) fn compose_plugin_picker_items(
    projection: &crate::plugins::PluginPickerProjection,
) -> Result<Vec<(PickerItem, PickerPayload)>, SearchWorkflowError> {
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(projection.request),
        source: SearchSourceKind::PluginProvider {
            provider: projection.provider.clone(),
        },
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: projection.items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(
        SearchRequestId(projection.request),
        SearchStreamId(projection.revision),
        &projection.query,
    )?;
    let workflow = projection
        .items
        .iter()
        .map(|item| plugin_item_adapter(&item.id, &item.primary, item.secondary.as_deref()))
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(projection.request),
        stream_id: SearchStreamId(projection.revision),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let snapshot = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?;
    let mut source = projection
        .items
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    Ok(snapshot
        .items
        .into_iter()
        .filter_map(|ranked| {
            source.remove(&ranked.stable_key).map(|item| {
                let id = PickerItemId(format!(
                    "plugin-provider:{}:{}",
                    projection.request, item.id
                ));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: item.primary.clone(),
                        detail: item.secondary.clone().unwrap_or_default(),
                        source: projection.provider.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::PluginProviderItem {
                        plugin: projection.plugin.clone(),
                        load: projection.load,
                        request: projection.request,
                        item: item.id.clone(),
                        action_command: projection.action_command.clone(),
                    },
                )
            })
        })
        .collect())
}

pub(in crate::app) fn compose_command_picker_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, SearchWorkflowError> {
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Commands,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(SearchRequestId(1), SearchStreamId(1), "")?;
    let workflow = items
        .iter()
        .map(|(item, payload)| {
            let mut item = item.clone();
            if let PickerPayload::Command(command) = payload {
                item.action = PickerAction::InvokeCommand {
                    command: command.clone(),
                }
            }
            picker_item_adapter(&item)
        })
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        stream_id: SearchStreamId(1),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut by_id = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect())
}
impl App {
    pub(in crate::app) fn plugin_limits(&self) -> PluginLimits {
        let policy = &self.configuration.settings().plugins;
        PluginLimits {
            maximum_component_bytes: policy.maximum_component_bytes,
            maximum_commands: policy.maximum_commands,
            fuel: policy.fuel,
            epoch_deadline_ticks: policy.epoch_deadline_ticks,
            maximum_memory_bytes: policy.maximum_memory_bytes,
            maximum_table_elements: policy.maximum_table_elements,
            maximum_instances: policy.maximum_instances,
        }
    }
}

impl App {
    pub(in crate::app) fn load_configured_plugins(&mut self) {
        if self.workspace.safe_mode() {
            self.show_error(self.message("safe_mode_blocked", &[]));
            return;
        }
        let policy = self.configuration.settings().plugins.clone();
        let store = if policy.store.is_absolute() {
            policy.store
        } else {
            crate::config::user_state_directory().join(policy.store)
        };
        let limits = self.plugin_limits();
        for (id, entry) in &policy.entries {
            if self.extension.disabled_plugins.contains(id)
                || (!entry.enabled && !self.extension.enabled_plugins.contains(id))
            {
                continue;
            }
            if let Err(error) = self.extension.index_from_store(
                id.clone(),
                store.clone(),
                entry.manifest.clone(),
                entry.component.clone(),
                limits.clone(),
            ) {
                if self.configuration.deferred_plugin_keymaps() {
                    let expected = self
                        .configuration
                        .expected_plugin_indexes()
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.extension.reject_expected(expected.into_iter(), None);
                    self.abort_pending_index_batch(error);
                    break;
                }
                self.show_error(self.message(
                    "plugin_load_failed",
                    &[("id", id.clone()), ("diagnostic", error.to_string())],
                ));
            }
        }
    }
}

impl App {
    pub(in crate::app) fn handle_plugin_worker_stopped(
        &mut self,
        error: crate::plugins::PluginError,
    ) {
        let Some(unregister) = self.extension.worker_stopped() else {
            return;
        };
        for plugin in unregister {
            self.commands.unregister_plugin(&plugin);
        }
        let owned_loads = self
            .extension
            .contributions()
            .ordered()
            .map(|index| (index.owner.clone(), PluginLoadId(index.plugin_load)))
            .collect::<Vec<_>>();
        for (plugin, load) in owned_loads {
            self.remove_plugin_contributions(&plugin, load);
        }
        self.extension.clear_contributions();
        if let Ok(candidate) = self.configuration.prepare_base_bindings(
            &self.commands,
            self.extension.contributions(),
            true,
        ) && let Err(error) = self.commit_contribution_candidate(candidate)
        {
            self.show_error(error);
        }
        self.health.record(
            LogLevel::Error,
            "plugins",
            "worker-stopped",
            BTreeMap::from([("error".into(), error.to_string())]),
        );
        self.show_error(self.message(
            "plugin_worker_stopped",
            &[("diagnostic", error.to_string())],
        ));
    }
}

impl App {
    pub(in crate::app) fn register_indexed_plugin_commands(
        &mut self,
        index: &crate::plugins::PluginContributionIndex,
    ) -> Result<(), String> {
        self.register_plugin_commands(&index.owner, &index.commands)
    }
}

impl App {
    fn register_plugin_commands(
        &mut self,
        plugin: &str,
        declarations: &BTreeMap<String, crate::plugins::PluginCommandDeclaration>,
    ) -> Result<(), String> {
        let plugin = plugin.to_owned();
        self.commands.unregister_plugin(&plugin);
        for (command, declaration) in declarations {
            let id = format!("plugin.{plugin}.{command}");
            let id = CommandId::new(&id).map_err(|error| error.to_string())?;
            let mut arguments = ArgumentSchema::default();
            for (name, field) in &declaration.arguments {
                arguments = arguments.field(
                    name,
                    match field.kind {
                        crate::plugins::PluginArgumentKind::String => {
                            crate::editor::ArgumentType::String
                        }
                        crate::plugins::PluginArgumentKind::Boolean => {
                            crate::editor::ArgumentType::Bool
                        }
                        crate::plugins::PluginArgumentKind::Integer => {
                            crate::editor::ArgumentType::Integer
                        }
                        crate::plugins::PluginArgumentKind::Number => {
                            crate::editor::ArgumentType::Number
                        }
                        crate::plugins::PluginArgumentKind::List => {
                            crate::editor::ArgumentType::List
                        }
                        crate::plugins::PluginArgumentKind::Record => {
                            crate::editor::ArgumentType::Record
                        }
                    },
                    field.required,
                );
            }
            let effect_plugin = plugin.clone();
            let effect_command = command.clone();
            let spec = CommandSpec {
                id,
                title: declaration.title.clone(),
                description: declaration.description.clone(),
                arguments,
                result: None,
                access: std::collections::BTreeSet::new(),
                context: match declaration.context {
                    crate::plugins::PluginCommandContext::Application => {
                        ContextRequirement::Application
                    }
                    crate::plugins::PluginCommandContext::Workspace => {
                        ContextRequirement::Workspace
                    }
                    crate::plugins::PluginCommandContext::View => ContextRequirement::View,
                    crate::plugins::PluginCommandContext::Buffer => ContextRequirement::Buffer,
                    crate::plugins::PluginCommandContext::Selection => {
                        ContextRequirement::Selection
                    }
                },
                source: CommandSource::Plugin(plugin.clone()),
            };
            let command_id = spec.id.clone();
            if let Err(error) = self
                .commands
                .register(spec, |_, _| Ok(CommandOutcome::empty()))
            {
                self.commands.unregister_plugin(&plugin);
                return Err(error.to_string());
            }
            let authority = self.plugin_authority(&plugin);
            if let Err(error) = self.commands.register_wrapper(
                &command_id,
                plugin.clone(),
                0,
                authority,
                move |_, invocation, pending, previous| {
                    Ok(crate::editor::CommandWrapperAction::Pending(vec![
                        AppEffect::PluginCall {
                            plugin: effect_plugin.clone(),
                            handler: effect_command.clone(),
                            call: Box::new(crate::app::plugins::abi::command_call(
                                invocation,
                                previous.as_ref().and_then(|outcome| outcome.value.as_ref()),
                            )),
                            pending: Some(pending),
                            previous: previous.map(Box::new),
                        },
                    ]))
                },
            ) {
                self.commands.unregister_plugin(&plugin);
                return Err(error.to_string());
            }
        }
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn recompile_plugin_contributions(&mut self) -> Result<(), String> {
        let candidate = self
            .configuration
            .prepare_plugin_contributions(&self.commands, self.extension.contributions())?;
        self.commit_contribution_candidate(candidate)
    }
}

impl App {
    pub(in crate::app) fn commit_contribution_candidate(
        &mut self,
        candidate: crate::app::configuration::ContributionCandidate,
    ) -> Result<(), String> {
        // Both owners are checked before publication. There is no callback, wait,
        // or configuration mutation between this check and the two accepts.
        self.configuration
            .check_contributions(&candidate, self.extension.contributions())?;
        let native_windows = crate::config::WindowsConfig {
            rules: candidate
                .windows()
                .rules
                .iter()
                .filter(|rule| !rule.content_type.starts_with("plugin."))
                .cloned()
                .collect(),
        };
        let windows = self.prepare_configured_window_rules(&native_windows)?;
        let (rules, catalog) = self.extension.prepare_window_rules(&candidate)?;
        self.window_session.accept_current(
            windows,
            &self.state.window_version(),
            &mut self.state,
        )?;
        let accepted = self
            .configuration
            .commit_contributions(candidate, catalog)?;
        rules.finish(accepted)?;
        self.cancel_pending_mapping();
        self.reconcile_editor_windows();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn remove_plugin_contributions(&mut self, plugin: &str, load: PluginLoadId) {
        match self
            .window_session
            .remove_plugin_load(plugin, load.0, &mut self.state)
            .map(|()| self.extension.remove_windows(plugin, load.0))
        {
            Ok(removed) => {
                if removed {
                    self.ui.plugin_surfaces = self.extension.window_snapshots();
                    self.window_session.invalidate_native();
                    self.reconcile_editor_windows();
                }
            }
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        let removed_active_confirmation = self.extension.remove_load(plugin, load);
        if removed_active_confirmation
            && self
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.close_prompt();
        }
        let terminals = self.terminal_session.descriptions();
        let title = self.message("explorer_title", &[]);
        let descriptions = NativeDescriptions {
            signature: self.native_surface_signature(),
            explorer_title: &title,
            dashboard_title: &self.configuration.dashboard().identity,
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title),
            terminals: &terminals,
        };
        let result = self
            .extension
            .remove_virtual_documents(
                plugin,
                load,
                VirtualContext {
                    editor: &self.state,
                    windows: &self.window_session,
                    scratch: SCRATCH_LABEL,
                },
                descriptions,
            )
            .and_then(|staged| {
                staged
                    .map(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    })
                    .transpose()
            });
        match result {
            Ok(Some(state)) => {
                *self.state = state;
                if self.state.active_view().is_none() {
                    self.set_dashboard_visibility(true);
                }
                self.reconcile_editor_windows();
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        if self.extension.remove_provider_load(plugin, load) {
            self.close_picker();
        }

        if self.extension.remove_contribution(plugin, load) {
            self.commands.unregister_plugin(plugin);
            self.remove_plugin_abi(plugin);
            if let Err(error) = self.recompile_plugin_contributions() {
                match self
                    .configuration
                    .prepare_builtin_contributions(
                        &self.commands,
                        self.extension.contributions(),
                        true,
                    )
                    .and_then(|candidate| self.commit_contribution_candidate(candidate))
                {
                    Ok(()) => self.show_error(error),
                    Err(fallback) => self.show_error(fallback),
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn abort_pending_index_batch(&mut self, diagnostic: impl std::fmt::Display) {
        for (owner, load) in self.extension.abort_indexes() {
            self.remove_plugin_contributions(&owner, load);
            self.commands.unregister_plugin(&owner);
        }
        let candidate = self
            .configuration
            .prepare_builtin_contributions(
                &self.commands,
                self.extension.contributions(),
                self.configuration.deferred_plugin_keymaps(),
            )
            .unwrap_or_else(|_| {
                self.configuration
                    .prepare_builtin_rules(self.extension.contributions())
            });
        if let Err(error) = self.commit_contribution_candidate(candidate) {
            self.show_error(error);
        }
        self.show_error(diagnostic);
    }
}

impl App {
    pub(in crate::app) fn abort_expected_index_batch(
        &mut self,
        failed_plugin: String,
        failed_load: PluginLoadId,
        diagnostic: impl std::fmt::Display,
    ) {
        let expected = self
            .configuration
            .expected_plugin_indexes()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        self.extension
            .reject_expected(expected.into_iter(), Some((&failed_plugin, failed_load)));
        self.abort_pending_index_batch(diagnostic);
    }
}

impl App {
    pub(in crate::app) fn process_plugin_storage_events(&mut self) {
        self.extension.poll_storage();
    }
}

impl App {
    pub(in crate::app) fn refresh_plugin_host_events(&mut self) {
        let active_surface = if self.window_session.terminal_focused() {
            "terminal"
        } else if self.window_session.explorer_focused() {
            "explorer"
        } else if self
            .ui
            .private_surfaces
            .iter()
            .any(|surface| surface.focused)
        {
            "plugin"
        } else {
            "editor"
        }
        .to_string();
        let buffer = self.state.active_view().and_then(|view_id| {
            let view = self.state.views.get(view_id)?;
            let buffer = self.state.buffers.get(view.buffer)?;
            let (anchor, head, kind) =
                view.selection
                    .map_or((view.cursor.0, view.cursor.0, 0), |selection| {
                        (
                            selection.anchor.0,
                            selection.head.0,
                            match selection.kind {
                                crate::editor::SelectionKind::Character => 1,
                                crate::editor::SelectionKind::Line => 2,
                                crate::editor::SelectionKind::Block => 3,
                            },
                        )
                    });
            Some((
                view.buffer.data().as_ffi(),
                buffer.text.revision().0,
                view.cursor.0,
                anchor,
                head,
                kind,
            ))
        });
        let graph_revision = self.window_session.graph().revision().unwrap_or(0);
        let observation = (
            self.workspace.root_revision(),
            active_surface.clone(),
            buffer,
            graph_revision,
        );
        self.extension.observe(
            observation,
            self.workspace.roots().len(),
            self.ui.plugin_surfaces.len(),
            self.ui.private_surfaces.len(),
        );
    }
}

impl App {
    pub(in crate::app) fn process_plugin_events(&mut self) {
        self.finish_plugin_folder_open();
        let browser_view = self
            .search
            .plugin_browser_open()
            .then(|| self.search.picker_bookmark())
            .flatten();
        self.poll_plugin_callbacks();
        if let Some(request) = self.extension.expire_providers() {
            self.present_plugin_picker(request);
        }
        if self.persistence.restoring() {
            return;
        }
        if self.extension.invalidate_stale_confirmations(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) && self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.close_prompt();
        }
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
        self.discard_stale_plugin_edits();
        let events = match self.extension.poll_owned() {
            Ok(events) => events,
            Err(error) => {
                self.discard_stale_plugin_edits();
                self.handle_plugin_worker_stopped(error);
                return;
            }
        };
        let refresh_browser =
            std::mem::take(&mut self.extension.browser_dirty) || !events.is_empty();
        let mut failure_diagnostic = None;
        for event in events {
            let event = match event {
                crate::plugins::OwnedPluginEvent::Executed(reply) => {
                    self.apply_plugin_execution(reply);
                    continue;
                }
                crate::plugins::OwnedPluginEvent::Other(event) => event,
            };
            match event {
                PluginEvent::Indexed {
                    load,
                    plugin,
                    index,
                } => {
                    if self.extension.take_rejected(&plugin, load) {
                        let _ = self.extension.disable_load(&plugin, load);
                        continue;
                    }
                    let checksum = index.checksum.clone();
                    let mut accepted = self
                        .extension
                        .install_index(index.clone())
                        .and_then(|()| self.register_indexed_plugin_commands(&index));
                    self.extension.reset_observation();
                    if accepted.is_ok() && index.storage.is_some() {
                        self.extension.install_storage(&plugin, load);
                    }
                    self.extension.queue_index(plugin.clone(), load, checksum);
                    let waiting_for_deferred = self.configuration.deferred_plugin_keymaps()
                        && !self
                            .configuration
                            .expected_plugin_indexes()
                            .iter()
                            .all(|owner| self.extension.contributions().get(owner).is_some());
                    if accepted.is_ok() && waiting_for_deferred {
                        continue;
                    }
                    if accepted.is_ok() {
                        accepted = self.recompile_plugin_contributions();
                    }
                    if let Err(error) = accepted {
                        self.abort_pending_index_batch(error);
                        continue;
                    }
                    if let Err((failed_plugin, failed_load, error)) =
                        self.extension.activate_pending(
                            self.configuration.deferred_plugin_keymaps(),
                            &plugin,
                            load,
                        )
                    {
                        self.remove_plugin_contributions(&failed_plugin, failed_load);
                        let _ = self.extension.disable_load(&failed_plugin, failed_load);
                        self.extension.abort_prepared(&failed_plugin, failed_load);
                        self.abort_pending_index_batch(error);
                        continue;
                    }
                    self.configuration.complete_plugin_indexes();
                    self.health.record(
                        LogLevel::Info,
                        "plugins",
                        "indexed-and-accepted",
                        BTreeMap::from([
                            ("plugin".into(), plugin),
                            ("load".into(), load.0.to_string()),
                            ("contexts".into(), index.contexts.len().to_string()),
                        ]),
                    );
                }
                PluginEvent::Loaded { load, component } => {
                    let plugin = component.manifest.id.clone();
                    if self.extension.take_rejected(&plugin, load) {
                        let _ = self.extension.disable_load(&plugin, load);
                        continue;
                    }
                    if let Some(index) = self
                        .extension
                        .contributions()
                        .get(&plugin)
                        .filter(|index| index.plugin_load == load.0)
                    {
                        let declared = index.commands.keys().cloned().collect::<BTreeSet<_>>();
                        let activated = component
                            .activated_commands
                            .iter()
                            .cloned()
                            .collect::<BTreeSet<_>>();
                        if declared != activated {
                            let updates = self.extension.failed_package_load(&plugin, load);
                            self.apply_extension_updates(updates);
                            self.remove_plugin_contributions(&plugin, load);
                            let _ = self.extension.disable_load(&plugin, load);
                            self.show_error(crate::plugins::PluginError::UndeclaredCommand);
                            continue;
                        }
                    }
                    let prepared = self.extension.take_prepared(&plugin, load);
                    let declarations = component
                        .manifest
                        .commands
                        .iter()
                        .filter(|(command, _)| component.activated_commands.contains(command))
                        .map(|(command, declaration)| (command.clone(), declaration.clone()))
                        .collect();
                    if let Err(error) = self.register_plugin_commands(&plugin, &declarations) {
                        self.show_error(error);
                        continue;
                    }
                    if let Some(token) = prepared
                        && let Err(error) =
                            self.extension.commit_package(plugin.clone(), load, token)
                    {
                        self.remove_plugin_contributions(&plugin, load);
                        self.show_error(error);
                    }
                    self.health.record(
                        LogLevel::Info,
                        "plugins",
                        "active",
                        BTreeMap::from([
                            ("plugin".into(), plugin),
                            (
                                "commands".into(),
                                component.activated_commands.len().to_string(),
                            ),
                        ]),
                    );
                }
                PluginEvent::CommandFailed { plugin, error, .. } => {
                    failure_diagnostic = Some(format!("Plugin {plugin}: {error}"));
                }
                PluginEvent::Executed(_) => self.show_error("unowned plugin execution event"),
                PluginEvent::Failed {
                    load,
                    plugin,
                    error,
                } => {
                    if self.extension.take_rejected(&plugin, load) {
                        continue;
                    }
                    if self.configuration.deferred_plugin_keymaps()
                        && !self.extension.is_active_load(&plugin, load)
                        && self
                            .configuration
                            .expected_plugin_indexes()
                            .contains(&plugin)
                    {
                        self.abort_expected_index_batch(plugin, load, error);
                        continue;
                    }
                    if self.extension.index_pending(&plugin) {
                        self.extension.reject_index(plugin, load);
                        self.abort_pending_index_batch(error);
                        continue;
                    }
                    let updates = self.extension.failed_package_load(&plugin, load);
                    self.apply_extension_updates(updates);
                    self.remove_plugin_contributions(&plugin, load);
                    self.commands.unregister_plugin(&plugin);
                    failure_diagnostic = Some(format!("Plugin {plugin}: {error}"));
                    self.health.record(
                        LogLevel::Error,
                        "plugins",
                        "failed",
                        BTreeMap::from([
                            ("plugin".into(), plugin),
                            ("error".into(), error.to_string()),
                        ]),
                    );
                }
            }
        }
        if refresh_browser {
            if browser_view.is_some()
                && (self.search.picker().is_none() || self.search.plugin_browser_open())
            {
                // Contribution changes invalidate picker bindings; keep the browser's query and selection.
                self.present_plugin_browser();
                self.search.restore_picker_view(browser_view, false);
                self.sync_picker_ui();
            } else {
                self.refresh_plugin_browser();
            }
        }
        self.discard_stale_plugin_edits();
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
        if let Some(diagnostic) = failure_diagnostic {
            self.show_error(diagnostic);
        }
    }
}

impl App {
    pub(in crate::app) fn apply_plugin_ui_request(
        &mut self,
        index: &crate::plugins::PluginContributionIndex,
        request: crate::plugins::PluginUiRequest,
    ) -> crate::plugins::PluginUiEvent {
        match self.extension.prepare_window(
            index,
            request,
            &self.state,
            &self.window_session,
            self.configuration.settings().editor.ambiguous_width
                == crate::config::AmbiguousWidth::Wide,
        ) {
            WindowUpdate::Ready(event) => event,
            WindowUpdate::Publish(source, transition) => {
                let accepted = self.window_session.accept_plugin_transition(
                    *transition,
                    &self.state.window_version(),
                    source.catalog(),
                    &mut self.state,
                );
                source.finish(accepted, self.window_session.graph().revision())
            }
        }
    }
}

impl App {
    pub(in crate::app) fn apply_plugin_execution(
        &mut self,
        reply: crate::plugins::PluginExecutionReply,
    ) {
        let mut source = match self.extension.receive(
            reply,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(source) => source,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.apply_plugin_call_source(&mut source);
        if let Some(error) = &source.execution.error {
            self.show_error(error);
        }
        let confirmation_source = source.confirmation_source.clone();
        let navigation_current =
            confirmation_source.navigation_current(&self.state, &self.window_session);
        let confirmation_result = source.confirmation_result;
        let source_actions_allowed = source.source_actions_allowed;
        let mut execution = source.execution;
        let load = source.load;
        let confirmation_requests = std::mem::take(&mut execution.confirmation_requests);
        if let Err(error) = self
            .extension
            .queue_confirmations(confirmation_source, confirmation_requests)
        {
            self.show_error(error);
        }
        if !source_actions_allowed {
            execution.messages.clear();
            execution.ui_requests.clear();
            execution.host_event_acknowledgements.clear();
            execution.storage_requests.clear();
            execution.provider_chunks.clear();
            execution.virtual_document_proposals.clear();
        }
        let plugin = execution.plugin.clone();
        if confirmation_result {
            let disallowed_ui = execution.ui_requests.iter().any(|request| {
                !matches!(
                    request.kind,
                    crate::plugins::PluginUiRequestKind::ReplaceUi { .. }
                        | crate::plugins::PluginUiRequestKind::ReplacePrivate { .. }
                        | crate::plugins::PluginUiRequestKind::PatchPrivate { .. }
                )
            });
            if disallowed_ui
                || !execution.storage_requests.is_empty()
                || !execution.provider_chunks.is_empty()
                || !execution.virtual_document_proposals.is_empty()
            {
                execution.ui_requests.clear();
                execution.storage_requests.clear();
                execution.provider_chunks.clear();
                execution.virtual_document_proposals.clear();
                self.show_error(
                    "plugin confirmation result may update existing plugin UI only".to_owned(),
                );
            }
        }
        let mut provider_continuations = Vec::new();
        if self.extension.is_active_load(&plugin, load) {
            self.extension
                .acknowledge(&plugin, load, &execution.host_event_acknowledgements);
            if let Some(policy) = self
                .extension
                .contributions()
                .get(&plugin)
                .filter(|index| index.plugin_load == load.0)
                .and_then(|index| index.storage.clone())
            {
                self.extension.request_storage(
                    &plugin,
                    load,
                    &policy,
                    std::mem::take(&mut execution.storage_requests),
                );
            }
            let workspace_revision = Some(self.workspace.root_revision());
            for proposal in std::mem::take(&mut execution.virtual_document_proposals) {
                let closing = matches!(
                    proposal.kind,
                    crate::plugins::PluginVirtualDocumentProposalKind::CloseAcknowledged
                );
                let result = self
                    .extension
                    .prepare_virtual(
                        &plugin,
                        load,
                        proposal,
                        navigation_current,
                        VirtualContext {
                            editor: &self.state,
                            windows: &self.window_session,
                            scratch: SCRATCH_LABEL,
                        },
                    )
                    .and_then(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    });
                match result {
                    Ok(state) => {
                        *self.state = state;
                        if !closing {
                            self.set_dashboard_visibility(false);
                        }
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            let buffer_revision = self
                .state
                .active_buffer_id()
                .and_then(|buffer| self.state.buffers.get(buffer))
                .map(|buffer| buffer.text.revision().0);
            for chunk in std::mem::take(&mut execution.provider_chunks) {
                match self.extension.provider_chunk(
                    &plugin,
                    load,
                    (workspace_revision, buffer_revision),
                    chunk,
                ) {
                    Ok(update) => {
                        if let Some(request) = update.refresh {
                            self.present_plugin_picker(request);
                        }
                        if let Some(command) = update.command {
                            provider_continuations.push(command);
                        }
                    }
                    Err(error) => self.show_error(error.detail),
                }
            }
        }
        for command in provider_continuations {
            self.process_effect(AppEffect::PluginExecute {
                plugin: plugin.clone(),
                command,
                arguments: Vec::new(),
            });
        }
        if !execution.ui_requests.is_empty()
            && self.extension.is_active_load(&plugin, load)
            && let Some(index) = self
                .extension
                .contributions()
                .get(&plugin)
                .filter(|index| index.plugin_load == load.0)
                .cloned()
        {
            let requests = std::mem::take(&mut execution.ui_requests);
            let events = requests
                .into_iter()
                .map(|request| self.apply_plugin_ui_request(&index, request))
                .collect::<Vec<_>>();
            self.extension.queue_ui(plugin.clone(), load, events);
            self.ui.plugin_surfaces = self.extension.window_snapshots();
            self.window_session.invalidate_native();
            self.reconcile_editor_windows();
        }
        for message in execution.messages {
            self.notify_message(message, false);
        }
        match source.preview {
            Ok(Some(key)) => self.begin_plugin_workspace_proposal(
                key,
                format!("plugin {}: {}", execution.plugin, execution.command),
            ),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        self.health.record(
            LogLevel::Info,
            "plugins",
            "executed",
            BTreeMap::from([
                ("plugin".into(), execution.plugin),
                ("command".into(), execution.command),
                ("request_id".into(), execution.request_id.to_string()),
            ]),
        );
    }
}

impl App {
    pub(in crate::app) fn discard_stale_plugin_edits(&mut self) {
        let (discarded, key) = self.extension.discard_stale_edits(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        if discarded != 0 {
            self.show_error(format!(
                "{discarded} plugin invocations discarded before their results were accepted"
            ));
        }
        if let Some(key) = key {
            if self
                .workspace_edits
                .origin()
                .is_some_and(|origin| origin.same(&key))
            {
                self.abort_workspace_edit("plugin proposal source changed".into());
            } else {
                let _ = self.extension.finish_edit(&key);
            }
        }
    }
}

impl App {
    fn begin_plugin_workspace_proposal(
        &mut self,
        key: crate::app::workspace::edits::OriginKey,
        label: String,
    ) {
        let result = (|| {
            self.workspace_edits.check_begin()?;
            let prepared = self.extension.prepare_edit(
                &key,
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            let path = prepared.draft.canonical_path.clone();
            let ticket = self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                prepared.sources,
            )?;
            let maximum = SEARCH_MAXIMUM_FILE_BYTES;
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect();
            self.workspace_edits.start_files(
                ticket,
                label,
                Some(key.clone()),
                false,
                vec![crate::app::workspace::edits::InputFile::Read(path.clone())],
                (roots, maximum),
            )?;
            let snapshot = self
                .state
                .workspace_snapshot(
                    self.workspace_edits
                        .preparing_sources()
                        .ok_or("plugin workspace preparation missing")?,
                    &path,
                    maximum,
                )?
                .ok_or("plugin snapshot document closed")?;
            self.workspace_edits.open_snapshot(0, snapshot)?;
            self.workspace_edits.finish_preparation(
                vec![prepared.draft],
                self.workspace.edit_scope(),
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        match result {
            Ok(()) => self.show_workspace_proposal(),
            Err(error) => {
                if self
                    .workspace_edits
                    .origin()
                    .is_some_and(|origin| origin.same(&key))
                {
                    self.abort_workspace_edit(error);
                } else {
                    let _ = self.extension.finish_edit(&key);
                    self.show_error(error);
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn accept_virtual_publication(
        windows: &mut crate::app::windows::Windows,
        (source, transition): (
            VirtualPublication<'_>,
            crate::app::windows::EditorWindowCandidate,
        ),
    ) -> Result<EditorState, String> {
        let accepted =
            windows.accept_editor_transition(transition, source.before(), source.catalog())?;
        source.finish(accepted)
    }
}

impl App {
    pub(in crate::app) fn route_virtual_open(&mut self, result: Result<VirtualOpen, String>) {
        match result {
            Ok(VirtualOpen::Execute {
                plugin,
                load,
                command,
                document,
            }) => {
                if !self.execute_plugin_command(plugin.clone(), command, Vec::new()) {
                    self.extension
                        .cancel_virtual_request(&plugin, load, document);
                }
            }
            Ok(VirtualOpen::Existing(buffer)) => {
                let mut draft = self.state.draft();
                if let Err(error) = draft
                    .show_document(buffer)
                    .map_err(str::to_owned)
                    .and_then(|_| self.commit_editor_draft(draft, None, false))
                {
                    self.show_error(error);
                } else {
                    self.set_dashboard_visibility(false);
                    self.reconcile_editor_windows();
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}

use slotmap::Key;

#[cfg(test)]
mod tests {
    use crate::app::plugins::Plugins;
    use crate::app::plugins::integration::ExtensionUpdate;
    use crate::app::plugins::integration::RegistryUpdate;
    use crate::plugins::PluginLimits;
    use crate::plugins::PluginLoadId;
    use crate::plugins::PluginManifest;
    use crate::tools::PackageManifest;
    use crate::tools::PackageSource;
    use crate::tools::RegistryEvent;
    use crate::tools::RegistryPackageMetadata;
    use crate::tools::RegistryResult;
    use crate::tools::RegistryWorker;
    use std::time::Duration;
    use std::time::Instant;
    #[test]
    fn package_update_replaces_managed_plugin_and_commits_after_load() {
        use crate::plugins::PluginEvent;
        use crate::tools::PackageOperation;
        use crate::tools::PackageStore;
        use sha2::Digest;
        use sha2::Sha256;
        use std::fs;
        use std::thread;
        let root = tempfile::tempdir().unwrap();
        let packages = root.path().join("packages");
        let registry = RegistryWorker::start(
            crate::tools::RegistryPolicy {
                offline: true,
                ..Default::default()
            },
            root.path().join("cache"),
        )
        .unwrap();
        let mut owner = Plugins::new(root.path().join("storage"), 8, packages.clone(), registry);
        let core = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/retained-guest/retained-guest.core.wasm"
        ));
        let bytes = wit_component::ComponentEncoder::default()
            .module(core)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let mut plugin = crate::app::plugins::fixture_manifest(digest.clone());
        let mut package = PackageManifest {
            name: "demo".into(),
            version: "1.0.0".into(),
            component: "plugin.wasm".into(),
            source: PackageSource::Registry {
                registry: "https://example.invalid".into(),
                version: "1.0.0".into(),
                archive_sha256: digest,
            },
            dependencies: Vec::new(),
        };
        fs::write(root.path().join("plugin.wasm"), bytes).unwrap();
        let mut old_load = None;
        for update in [false, true] {
            if update {
                package.version = "2.0.0".into();
                plugin.version = "2.0.0".into();
            }
            fs::write(
                root.path().join("package.json"),
                serde_json::to_vec(&package).unwrap(),
            )
            .unwrap();
            fs::write(
                root.path().join("plugin.toml"),
                toml::to_string(&plugin).unwrap(),
            )
            .unwrap();
            owner
                .request_package(PackageOperation::Install {
                    package_manifest: root.path().join("package.json"),
                    plugin_manifest: root.path().join("plugin.toml"),
                    component: root.path().join("plugin.wasm"),
                    roots: vec![root.path().into()],
                    limits: PluginLimits::default(),
                    update,
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let load = loop {
                for update in
                    owner.poll_packages(PluginLimits::default(), &Default::default(), false)
                {
                    assert!(
                        !matches!(
                            update,
                            ExtensionUpdate::Conflict(_)
                                | ExtensionUpdate::Error(_)
                                | ExtensionUpdate::Failed { .. }
                        ),
                        "package preparation failed"
                    );
                }
                let mut loaded = None;
                for event in owner.manager.poll().unwrap() {
                    match event {
                        PluginEvent::Indexed {
                            load, plugin: id, ..
                        } => {
                            owner
                                .manager
                                .activate_indexed(&id, load, plugin.component_sha256.clone())
                                .unwrap();
                        }
                        PluginEvent::Loaded { load, .. } => loaded = Some(load),
                        PluginEvent::Failed { error, .. } => panic!("plugin load: {error}"),
                        _ => {}
                    }
                }
                if let Some(load) = loaded {
                    break load;
                }
                assert!(
                    Instant::now() < deadline,
                    "replacement load timed out: update={update}, pending={}, statuses={:?}",
                    owner.pending_packages(),
                    owner.statuses(),
                );
                thread::sleep(Duration::from_millis(2));
            };
            assert_ne!(Some(load), old_load);
            let token = owner.take_prepared("demo", load).unwrap();
            let manager =
                PackageStore::new(packages.clone(), 1024 * 1024, 64 * 1024 * 1024).unwrap();
            assert_eq!(
                manager
                    .read_lock()
                    .unwrap()
                    .packages
                    .first()
                    .map(|package| package.version.as_str()),
                if update { Some("1.0.0") } else { None }
            );
            owner.commit_package("demo".into(), load, token).unwrap();
            while owner.pending_packages() != 0 {
                owner.poll_packages(PluginLimits::default(), &Default::default(), false);
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(
                manager.read_lock().unwrap().packages[0].version,
                package.version
            );
            old_load = Some(load);
        }
        // Neither a new ID for this package nor another package claiming its ID may load.
        for (package_name, plugin_id) in [("demo", "renamed"), ("unrelated", "demo")] {
            package.name = package_name.into();
            plugin.id = plugin_id.into();
            fs::write(
                root.path().join("package.json"),
                serde_json::to_vec(&package).unwrap(),
            )
            .unwrap();
            fs::write(
                root.path().join("plugin.toml"),
                toml::to_string(&plugin).unwrap(),
            )
            .unwrap();
            owner
                .request_package(PackageOperation::Install {
                    package_manifest: root.path().join("package.json"),
                    plugin_manifest: root.path().join("plugin.toml"),
                    component: root.path().join("plugin.wasm"),
                    roots: vec![root.path().into()],
                    limits: PluginLimits::default(),
                    update: package_name == "demo",
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let updates =
                    owner.poll_packages(PluginLimits::default(), &Default::default(), false);
                if updates.iter().any(|update| matches!(update, ExtensionUpdate::Failed { error, .. } if error.contains("belongs to package demo"))) {
                    break;
                }
                assert!(updates.is_empty(), "unexpected package update");
                assert!(Instant::now() < deadline, "ownership rejection timed out");
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(owner.manager.load_id("demo"), old_load);
            assert!(owner.manager.load_id("renamed").is_none());
            assert!(owner.package_prepared.is_empty());
            let manager =
                PackageStore::new(packages.clone(), 1024 * 1024, 64 * 1024 * 1024).unwrap();
            assert_eq!(manager.read_lock().unwrap().packages.len(), 1);
            assert_eq!(manager.read_lock().unwrap().packages[0].name, "demo");
        }
        // Package maintenance is allowed in safe mode, but must not activate installed code.
        for operation in [
            PackageOperation::Rollback,
            PackageOperation::Remove("demo".into()),
        ] {
            owner.request_package(operation).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while owner.pending_packages() != 0 {
                for update in
                    owner.poll_packages(PluginLimits::default(), &Default::default(), true)
                {
                    assert!(!matches!(
                        update,
                        ExtensionUpdate::Error(_) | ExtensionUpdate::Failed { .. }
                    ));
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            assert!(owner.manager.load_id("demo").is_none());
            assert!(owner.package_startup_request.is_none());
        }
    }

    #[test]
    fn registry_presentations_keep_the_requesting_picker_epoch() {
        use crate::app::picker::PickerContent;
        use crate::app::picker::PickerOrigin;
        use crate::app::search::Search;
        use crate::app::workspace::Workspace;
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let identity = crate::files::RootIdentity::capture(root.path()).unwrap();
        let workspace = Workspace::new(
            crate::workspace::WorkspaceState::new(
                crate::workspace::WorkspaceId(1),
                [("root".into(), identity)],
            )
            .unwrap(),
            root.path().join("trust"),
            1,
            false,
            false,
            1,
        );
        let mut search = Search::default();
        let content = |title: &str| PickerContent {
            surface: crate::ui::PickerSurfaceKind::Generic,
            origin: PickerOrigin::General,
            title: title.into(),
            items: Vec::new(),
            query: String::new(),
            complete: true,
        };
        let epoch = search
            .install_picker(content("registry"), 100, workspace.version(), 1)
            .unwrap();
        owner.registry_intents.insert(1, (false, Some(epoch)));
        let Some(RegistryUpdate::Present(_, Some(epoch))) = owner.accept_registry(RegistryEvent {
            id: 1,
            result: Ok(RegistryResult::Search(
                crate::tools::RegistrySearchResponse {
                    schema: 1,
                    packages: Vec::new(),
                },
            )),
        }) else {
            panic!("search must preserve its picker owner")
        };
        assert!(search.owns_picker_epoch(&epoch));
        search
            .install_picker(content("files"), 100, workspace.version(), 1)
            .unwrap();
        search.picker_insert("my query");
        assert!(
            !search.owns_picker_epoch(&epoch),
            "a late registry result cannot replace the file picker"
        );
        assert_eq!(search.picker().unwrap().query(), "my query");
    }

    #[test]
    fn package_activation_respects_safe_mode_and_disabled_plugins() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let (manager, _fixture) = crate::plugins::ExecutionFixture::new(4);
        owner.fixture_manager(manager);
        let plugin = PluginManifest {
            id: "other".into(),
            version: "1.0.0".into(),
            component_sha256: "0".repeat(64),
            capabilities: Default::default(),
            commands: Default::default(),
            contributions: Default::default(),
        };
        let lock =
            std::sync::Arc::new(std::fs::File::create(root.path().join("object-lock")).unwrap());
        let activation = vec![(plugin, root.path().join("other.wasm"), lock)];
        assert!(
            owner
                .load_activations(
                    activation.clone(),
                    &PluginLimits::default(),
                    &Default::default(),
                    true
                )
                .is_empty()
        );
        assert!(owner.load_id("other").is_none());
        let mut config = crate::config::PluginsConfig::default();
        config.entries.insert(
            "other".into(),
            crate::config::PluginEntryConfig {
                manifest: "other.toml".into(),
                component: "other.wasm".into(),
                enabled: false,
            },
        );
        assert!(
            owner
                .load_activations(activation.clone(), &PluginLimits::default(), &config, false)
                .is_empty()
        );
        assert!(owner.load_id("other").is_none());
        owner.disable("other").unwrap();
        assert!(
            owner
                .load_activations(
                    activation.clone(),
                    &PluginLimits::default(),
                    &Default::default(),
                    false
                )
                .is_empty()
        );
        assert!(owner.load_id("other").is_none());
        assert!(matches!(
            owner.statuses().get("other"),
            Some(crate::plugins::PluginStatus::Disabled)
        ));
        // A normal enabled activation still starts loading.
        owner.disabled_plugins.remove("other");
        if let Some(update) = owner
            .load_activations(
                activation,
                &PluginLimits::default(),
                &Default::default(),
                false,
            )
            .into_iter()
            .next()
        {
            match update {
                ExtensionUpdate::Error(error) => panic!("enabled activation failed: {error}"),
                _ => panic!("unexpected activation update"),
            }
        }
        assert!(owner.load_id("other").is_some());
        // Disabling while activation is pending suppresses its late Loaded event.
        // It must also release the pending-load blocker on subsequent package changes.
        owner.disable("other").unwrap();
        assert!(!owner.package_startup_pending.contains_key("other"));
        owner
            .request_package(crate::tools::PackageOperation::Remove("other".into()))
            .unwrap();
    }

    #[test]
    fn package_activation_conflicts_do_not_request_persistent_rollback() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let (manager, _fixture) = crate::plugins::ExecutionFixture::new(4);
        owner.fixture_manager(manager);
        let manifest = crate::app::plugins::fixture_manifest("0".repeat(64));
        let updates = owner.load_activations(
            vec![(
                manifest,
                root.path().join("demo.wasm"),
                std::sync::Arc::new(
                    std::fs::File::create(root.path().join("object-lock")).unwrap(),
                ),
            )],
            &PluginLimits::default(),
            &Default::default(),
            false,
        );
        assert!(
            matches!(updates.as_slice(), [ExtensionUpdate::Conflict(plugin)] if plugin == "demo")
        );
        assert!(owner.package_startup_rollback.is_none());
        assert!(!owner.package_startup_rollback_attempted);
        assert_eq!(owner.pending_packages(), 0);
        assert_eq!(owner.load_id("demo"), Some(PluginLoadId(1)));
    }

    #[test]
    fn package_changes_wait_for_prepare_load_and_commit() {
        use crate::tools::PackageError;
        use crate::tools::PackageOperation;
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        for stage in 0..3 {
            match stage {
                0 => {
                    owner.package_preparing.insert(1);
                }
                1 => {
                    owner
                        .package_prepared
                        .insert("demo".into(), (PluginLoadId(1), 1));
                }
                _ => {
                    owner
                        .package_commits
                        .insert(1, ("demo".into(), PluginLoadId(1)));
                }
            }
            for operation in [
                PackageOperation::Remove("demo".into()),
                PackageOperation::Rollback,
            ] {
                assert!(matches!(
                    owner.request_package(operation),
                    Err(PackageError::Busy)
                ));
            }
            assert!(
                matches!(owner.rollback_packages().as_slice(), [ExtensionUpdate::Error(error)] if error == &PackageError::Busy.to_string())
            );
            assert_eq!(owner.packages.pending_count(), 0);
            owner.package_preparing.clear();
            owner.package_prepared.clear();
            owner.package_commits.clear();
        }
        owner
            .request_package(PackageOperation::Remove("demo".into()))
            .unwrap();
        // A new install cannot start while removal is waiting for the worker either.
        assert!(matches!(
            owner.request_package(PackageOperation::Install {
                package_manifest: root.path().join("package.json"),
                plugin_manifest: root.path().join("plugin.toml"),
                component: root.path().join("plugin.wasm"),
                roots: vec![root.path().into()],
                limits: PluginLimits::default(),
                update: true,
            }),
            Err(PackageError::Busy)
        ));
    }

    #[test]
    fn package_abort_retries_after_queue_saturation() {
        use crate::tools::PackageError;
        use crate::tools::PackageOperation;
        use crate::tools::PackageWorker;
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        owner.packages = PackageWorker::start(root.path().join("packages"), 1, 1024 * 1024, 1024);
        let deadline = Instant::now() + Duration::from_secs(5);
        // One result fills the event queue, the next blocks its worker, and a third fills requests.
        while owner.packages.pending_count() < 3 {
            match owner.packages.request(PackageOperation::List) {
                Ok(_) | Err(PackageError::QueueFull) => {}
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(matches!(
            owner.packages.request(PackageOperation::List),
            Err(PackageError::QueueFull)
        ));
        let (manager, _fixture) = crate::plugins::ExecutionFixture::new(4);
        owner.fixture_manager(manager);
        owner
            .package_prepared
            .insert("demo".into(), (PluginLoadId(1), 41));
        owner.disable("demo").unwrap();
        assert!(owner.package_prepared.is_empty());
        assert!(owner.package_aborts.contains(&41));
        let mut aborted = 0;
        while owner.pending_packages() != 0 {
            for update in owner.poll_packages(PluginLimits::default(), &Default::default(), false) {
                if let ExtensionUpdate::Failed { operation, error } = update {
                    assert_eq!(operation, "abort");
                    assert_eq!(error, PackageError::MissingPrepared.to_string());
                    aborted += 1;
                }
            }
            assert!(Instant::now() < deadline, "abort was not retried");
            std::thread::yield_now();
        }
        assert_eq!(aborted, 1);
        assert!(owner.package_aborts.is_empty());
    }

    #[test]
    fn cancelled_registry_and_replacement_package_requests_are_not_adopted() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let metadata = RegistryPackageMetadata {
            schema: 1,
            publisher: "owned".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            yanked: false,
            artifact_path: "demo.wasm".into(),
            artifact_sha256: "0".repeat(64),
            package: PackageManifest {
                name: "demo".into(),
                version: "1.0.0".into(),
                component: "demo.wasm".into(),
                source: PackageSource::Path {
                    path: root.path().into(),
                },
                dependencies: Vec::new(),
            },
            plugin: PluginManifest {
                id: "demo".into(),
                version: "1.0.0".into(),
                component_sha256: "0".repeat(64),
                capabilities: Default::default(),
                commands: Default::default(),
                contributions: Default::default(),
            },
        };
        let event = |id, bytes| RegistryEvent {
            id,
            result: Ok(RegistryResult::Artifact {
                metadata: metadata.clone(),
                bytes,
            }),
        };
        // Synthetic registry responses; no network or installation is performed.
        assert!(owner.accept_registry(event(1, vec![1])).is_none());
        owner.registry_intents.insert(1, (false, None));
        assert!(matches!(
            owner.accept_registry(event(1, vec![1])),
            Some(RegistryUpdate::Error(_))
        ));
        assert!(owner.pending_registry_artifact.is_none());
        owner.registry_intents.insert(2, (true, None));
        assert!(matches!(
            owner.accept_registry(event(2, vec![2])),
            Some(RegistryUpdate::ValidateArtifact)
        ));
        assert!(owner.accept_registry(event(2, vec![3])).is_none());
        owner.registry_intents.insert(3, (true, None));
        assert!(matches!(
            owner.accept_registry(event(3, vec![3])),
            Some(RegistryUpdate::Error(_))
        ));
        assert_eq!(owner.pending_registry_artifact.as_ref().unwrap().1, vec![2]);
        owner.registry_intents.insert(4, (true, None));
        owner.cancel_pending_packages();
        assert!(owner.accept_registry(event(4, vec![4])).is_none());
        assert!(owner.pending_registry_artifact.is_none());
        owner
            .package_prepared
            .insert("demo".into(), (PluginLoadId(9), 41));
        owner.abort_prepared("demo", PluginLoadId(8));
        assert_eq!(
            owner.package_prepared.get("demo"),
            Some(&(PluginLoadId(9), 41))
        );
        owner.package_prepared.clear();
        // Actual package worker/VFS failure: no previous lock exists in this owned root.
        owner.rollback_packages();
        assert!(owner.package_startup_rollback.is_some());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if owner.poll_packages(PluginLimits::default(), &Default::default(), false).iter().any(|update| matches!(update, ExtensionUpdate::Failed { operation, .. } if operation == "rollback")) { break; }
            assert!(Instant::now() < deadline, "rollback terminal missing");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(owner.package_startup_rollback.is_none());
    }

    #[test]
    fn deferred_activation_failure_keeps_unsubmitted_indexes() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let (mut manager, _fixture) = crate::plugins::ExecutionFixture::new(1);
        manager
            .execute(crate::plugins::empty_execution_request())
            .unwrap();
        manager
            .statuses
            .insert("demo".into(), crate::plugins::PluginStatus::Indexed);
        owner.fixture_manager(manager);
        owner.queue_index("demo".into(), PluginLoadId(1), "a".into());
        owner.queue_index("zeta".into(), PluginLoadId(2), "b".into());
        assert!(
            owner
                .activate_pending(true, "demo", PluginLoadId(1))
                .is_err()
        );
        assert!(!owner.index_pending("demo"));
        assert!(owner.index_pending("zeta"));
    }

    #[test]
    fn prepared_updates_replace_only_managed_loads_and_ignore_cancelled_requests() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryWorker::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = Plugins::new(
            root.path().join("storage"),
            4,
            root.path().join("packages"),
            registry,
        );
        let (manager, _fixture) = crate::plugins::ExecutionFixture::new(4);
        owner.fixture_manager(manager);
        let prepared = crate::tools::PreparedPackage {
            token: 41,
            manifest: PackageManifest {
                name: "demo".into(),
                version: "2.0.0".into(),
                component: "demo.wasm".into(),
                source: PackageSource::Path {
                    path: root.path().into(),
                },
                dependencies: Vec::new(),
            },
            plugin: PluginManifest {
                id: "demo".into(),
                version: "2.0.0".into(),
                component_sha256: "0".repeat(64),
                capabilities: Default::default(),
                commands: Default::default(),
                contributions: Default::default(),
            },
            object: root.path().join("demo.wasm"),
        };
        owner.package_preparing.insert(41);
        let updates = owner.load_prepared(41, prepared.clone(), &PluginLimits::default());
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, ExtensionUpdate::Conflict(_)))
        );
        assert_eq!(owner.manager.load_id("demo"), Some(PluginLoadId(1)));
        owner.package_preparing.insert(42);
        owner.cancel_pending_packages();
        assert!(
            owner
                .load_prepared(42, prepared.clone(), &PluginLimits::default())
                .is_empty()
        );
        assert_eq!(owner.manager.load_id("demo"), Some(PluginLoadId(1)));
        owner
            .package_managed_plugins
            .insert("demo".into(), PluginLoadId(1));
        owner.package_preparing.insert(43);
        let updates = owner.load_prepared(43, prepared, &PluginLimits::default());
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, ExtensionUpdate::Remove(_, PluginLoadId(1))))
        );
        assert!(
            !updates
                .iter()
                .any(|update| matches!(update, ExtensionUpdate::Conflict(_)))
        );
        assert_eq!(
            owner.package_prepared.get("demo"),
            Some(&(PluginLoadId(2), 41))
        );
    }
}
