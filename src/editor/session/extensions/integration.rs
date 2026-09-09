//! Extension catalog and package/registry transitions; editor publication stays in dispatch.
use super::ExtensionSession;
use crate::{
    extensions::{PluginContributionCatalog, PluginContributionIndex, PluginLimits, PluginLoadId},
    tools::{
        PackageError, PackageLock, PackageOperation, PackageServiceEvent, RegistryEvent,
        RegistryRequest, RegistryResult,
    },
};
use std::collections::{BTreeMap, HashMap};

pub(in crate::editor::session) enum ExtensionUpdate {
    Remove(String, PluginLoadId),
    Error(String),
    Conflict(String),
    Message(&'static str, Vec<(&'static str, String)>),
    Lock(PackageLock),
    Failed { operation: String, error: String },
}
pub(in crate::editor::session) enum RegistryUpdate {
    Present(RegistryResult),
    ValidateArtifact,
    Error(String),
}
impl ExtensionSession {
    pub(in crate::editor::session) fn pending_packages(&self) -> usize {
        self.packages.pending_count()
    }
    pub(in crate::editor::session) fn contributions(&self) -> &PluginContributionCatalog {
        &self.plugin_contributions
    }
    pub(in crate::editor::session) fn install_index(
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
    pub(in crate::editor::session) fn remove_contribution(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> bool {
        self.plugin_contributions.remove_generation(plugin, load.0)
    }
    pub(in crate::editor::session) fn take_rejected(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> bool {
        self.rejected_plugin_loads.remove(&(plugin.into(), load))
    }
    pub(in crate::editor::session) fn index_pending(&self, plugin: &str) -> bool {
        self.pending_index_activation.contains_key(plugin)
    }
    pub(in crate::editor::session) fn queue_index(
        &mut self,
        plugin: String,
        load: PluginLoadId,
        checksum: String,
    ) {
        self.pending_index_activation
            .insert(plugin, (load, checksum));
    }
    pub(in crate::editor::session) fn activate_pending(
        &mut self,
        deferred: bool,
        plugin: &str,
        load: PluginLoadId,
    ) -> Result<(), (String, PluginLoadId, String)> {
        let activations = if deferred {
            std::mem::take(&mut self.pending_index_activation)
        } else {
            let Some((candidate, checksum)) = self
                .pending_index_activation
                .get(plugin)
                .filter(|(candidate, _)| *candidate == load)
                .cloned()
            else {
                return Ok(());
            };
            self.pending_index_activation.remove(plugin);
            BTreeMap::from([(plugin.into(), (candidate, checksum))])
        };
        for (plugin, (load, checksum)) in activations {
            self.manager
                .activate_indexed(&plugin, load, checksum)
                .map_err(|e| (plugin, load, e.to_string()))?;
        }
        Ok(())
    }
    pub(in crate::editor::session) fn reject_index(&mut self, plugin: String, load: PluginLoadId) {
        self.rejected_plugin_loads.insert((plugin, load));
    }
    pub(in crate::editor::session) fn reject_expected(
        &mut self,
        expected: impl Iterator<Item = String>,
        failed: (&str, PluginLoadId),
    ) {
        for plugin in expected {
            if let Some(load) = if plugin == failed.0 {
                Some(failed.1)
            } else {
                self.manager.load_id(&plugin)
            } {
                self.reject_index(plugin.clone(), load);
                self.pending_index_activation
                    .entry(plugin)
                    .or_insert_with(|| (load, String::new()));
            }
        }
    }
    pub(in crate::editor::session) fn abort_indexes(&mut self) -> Vec<(String, PluginLoadId)> {
        let pending = std::mem::take(&mut self.pending_index_activation);
        pending
            .into_iter()
            .map(|(plugin, (load, _))| {
                self.reject_index(plugin.clone(), load);
                let _ = self.manager.disable_generation(&plugin, load);
                self.abort_prepared(&plugin, load);
                (plugin, load)
            })
            .collect()
    }
    pub(in crate::editor::session) fn request_package(
        &mut self,
        operation: PackageOperation,
    ) -> Result<u64, PackageError> {
        if matches!(
            operation,
            PackageOperation::Rollback | PackageOperation::Remove(_)
        ) {
            self.package_startup_rollback_attempted = false;
        }
        self.packages.request(operation)
    }
    pub(in crate::editor::session) fn start_packages(&mut self) {
        self.package_startup_request = self.packages.request(PackageOperation::Activations).ok();
    }
    pub(in crate::editor::session) fn request_registry(
        &mut self,
        request: RegistryRequest,
        install: bool,
    ) -> Result<(), String> {
        let id = self.registry.request(request).map_err(|e| e.to_string())?;
        self.registry_intents.insert(id, install);
        Ok(())
    }
    fn accept_registry(&mut self, event: RegistryEvent) -> Option<RegistryUpdate> {
        // A retired workspace/request must not turn a late artifact into a new install.
        let install = self.registry_intents.remove(&event.id)?;
        Some(match event.result {
            Ok(RegistryResult::Show(metadata)) if install => match self.request_registry(
                RegistryRequest::Fetch {
                    metadata: Box::new(metadata),
                },
                true,
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
            Ok(result) => RegistryUpdate::Present(result),
            Err(error) => RegistryUpdate::Error(error.to_string()),
        })
    }
    pub(in crate::editor::session) fn poll_registry(&mut self) -> Vec<RegistryUpdate> {
        self.registry
            .poll()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| self.accept_registry(event))
            .collect()
    }
    pub(in crate::editor::session) fn discard_artifact(&mut self) {
        self.pending_registry_artifact = None;
    }
    pub(in crate::editor::session) fn prepare_artifact(
        &mut self,
        limits: PluginLimits,
    ) -> Result<(), PackageError> {
        if let Some((metadata, component)) = self.pending_registry_artifact.take() {
            self.packages.request(PackageOperation::PrepareRegistry {
                metadata: Box::new(metadata),
                component,
                limits,
            })?;
        }
        Ok(())
    }
    pub(in crate::editor::session) fn take_prepared(
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
    pub(in crate::editor::session) fn abort_prepared(&mut self, plugin: &str, load: PluginLoadId) {
        if let Some((_, token)) = self
            .package_prepared
            .get(plugin)
            .filter(|(candidate, _)| *candidate == load)
            .copied()
        {
            self.package_prepared.remove(plugin);
            let _ = self.packages.request(PackageOperation::Abort { token });
        }
    }
    pub(in crate::editor::session) fn commit_package(
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
                let _ = self.manager.disable_generation(&plugin, load);
                let _ = self.packages.request(PackageOperation::Abort { token });
                Err(error)
            }
        }
    }
    pub(in crate::editor::session) fn failed_package_load(
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
            match self.manager.unload_generation(&plugin, load) {
                Ok(()) => updates.push(ExtensionUpdate::Remove(plugin, load)),
                Err(error) => {
                    failed.insert(plugin, load);
                    updates.push(ExtensionUpdate::Error(error.to_string()));
                }
            }
        }
        self.package_managed_plugins = failed;
    }
    pub(in crate::editor::session) fn rollback_packages(&mut self) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        if self.package_startup_rollback.is_some() || self.package_startup_rollback_attempted {
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
    pub(in crate::editor::session) fn retire_packages(&mut self) -> Vec<(String, PluginLoadId)> {
        self.registry_intents.clear();
        self.pending_registry_artifact = None;
        let mut removed = Vec::new();
        for (plugin, (load, token)) in self.package_prepared.drain() {
            let _ = self.manager.unload_generation(&plugin, load);
            let _ = self.packages.request(PackageOperation::Abort { token });
            removed.push((plugin, load));
        }
        for (plugin, load) in self.package_commits.values() {
            let _ = self.manager.unload_generation(plugin, *load);
            removed.push((plugin.clone(), *load));
        }
        removed
    }
    pub(in crate::editor::session) fn worker_stopped(&mut self) -> Option<Vec<String>> {
        if self.plugin_worker_reported {
            return None;
        }
        self.plugin_worker_reported = true;
        self.pending_index_activation.clear();
        self.rejected_plugin_loads.clear();
        let mut unregister = self.manager.statuses.keys().cloned().collect::<Vec<_>>();
        for (plugin, (_, token)) in self.package_prepared.drain() {
            unregister.push(plugin);
            let _ = self.packages.request(PackageOperation::Abort { token });
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
    pub(in crate::editor::session) fn clear_contributions(&mut self) {
        self.plugin_contributions = PluginContributionCatalog::default();
        self.clear_events();
    }
    pub(in crate::editor::session) fn poll_packages(
        &mut self,
        limits: PluginLimits,
    ) -> Vec<ExtensionUpdate> {
        let mut updates = Vec::new();
        for event in self.packages.poll().unwrap_or_default() {
            match event {
                PackageServiceEvent::Completed(mut result) => {
                    let prepared_operation = result.prepared.is_some();
                    let activation_operation = result.operation == "activations";
                    if let Some(prepared) = result.prepared.take() {
                        let plugin = prepared.plugin.id.clone();
                        let token = prepared.token;
                        if self.manager.load_id(&plugin).is_some() {
                            let _ = self.packages.request(PackageOperation::Abort { token });
                            updates.push(ExtensionUpdate::Conflict(plugin));
                            continue;
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
                                let _ = self.packages.request(PackageOperation::Abort { token });
                                updates.push(ExtensionUpdate::Error(error.to_string()));
                            }
                        }
                    }
                    for (manifest, path) in std::mem::take(&mut result.activations) {
                        let plugin = manifest.id.clone();
                        if self.manager.load_id(&plugin).is_some() {
                            updates.push(ExtensionUpdate::Conflict(plugin));
                            updates.extend(self.rollback_packages());
                            break;
                        }
                        match self.manager.load(
                            self.package_store.clone(),
                            path,
                            manifest,
                            limits.clone(),
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
                        self.unload_managed(&mut updates);
                        self.package_startup_pending.clear();
                        if !self.package_managed_plugins.is_empty() {
                            updates.extend(self.rollback_packages());
                            continue;
                        }
                        match self.packages.request(PackageOperation::Activations) {
                            Ok(id) => self.package_startup_request = Some(id),
                            Err(error) => {
                                updates.push(ExtensionUpdate::Error(error.to_string()));
                                updates.extend(self.rollback_packages());
                            }
                        }
                    }
                    if let Some(lock) = result.lock {
                        updates.push(ExtensionUpdate::Lock(lock));
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
                PackageServiceEvent::Failed {
                    id,
                    operation,
                    error,
                } => {
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
                        let _ = self.manager.disable_generation(&plugin, load);
                        updates.push(ExtensionUpdate::Remove(plugin, load));
                    }
                    updates.push(ExtensionUpdate::Failed {
                        operation,
                        error: error.to_string(),
                    });
                }
            }
        }
        updates
    }
}
#[cfg(test)]
impl ExtensionSession {
    pub(in crate::editor::session) fn fixture_contribution(
        &mut self,
        index: PluginContributionIndex,
    ) -> Result<(), crate::extensions::PluginError> {
        self.plugin_contributions.install(index)
    }
    pub(in crate::editor::session) fn fixture_pending_index(
        &mut self,
        plugin: String,
        source: (PluginLoadId, String),
    ) {
        self.queue_index(plugin, source.0, source.1);
    }
    pub(in crate::editor::session) fn pending_indexes(
        &self,
    ) -> &BTreeMap<String, (PluginLoadId, String)> {
        &self.pending_index_activation
    }
    pub(in crate::editor::session) fn rejected_indexes(
        &self,
    ) -> &std::collections::BTreeSet<(String, PluginLoadId)> {
        &self.rejected_plugin_loads
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExtensionSession, ExtensionUpdate, PluginLimits, PluginLoadId, RegistryEvent,
        RegistryResult, RegistryUpdate,
    };
    use crate::{
        extensions::PluginManifest,
        tools::{PackageManifest, PackageSource, RegistryPackageMetadata, RegistryService},
    };
    use std::time::{Duration, Instant};
    #[test]
    fn retired_registry_and_replacement_package_obligations_are_not_adopted() {
        let root = tempfile::tempdir().unwrap();
        let registry =
            RegistryService::start(Default::default(), root.path().join("cache")).unwrap();
        let mut owner = ExtensionSession::new(
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
        owner.registry_intents.insert(1, false);
        assert!(matches!(
            owner.accept_registry(event(1, vec![1])),
            Some(RegistryUpdate::Error(_))
        ));
        assert!(owner.pending_registry_artifact.is_none());
        owner.registry_intents.insert(2, true);
        assert!(matches!(
            owner.accept_registry(event(2, vec![2])),
            Some(RegistryUpdate::ValidateArtifact)
        ));
        assert!(owner.accept_registry(event(2, vec![3])).is_none());
        owner.registry_intents.insert(3, true);
        assert!(matches!(
            owner.accept_registry(event(3, vec![3])),
            Some(RegistryUpdate::Error(_))
        ));
        assert_eq!(owner.pending_registry_artifact.as_ref().unwrap().1, vec![2]);
        owner.registry_intents.insert(4, true);
        owner.retire_packages();
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
            if owner.poll_packages(PluginLimits::default()).iter().any(|update| matches!(update, ExtensionUpdate::Failed { operation, .. } if operation == "rollback")) { break; }
            assert!(Instant::now() < deadline, "rollback terminal missing");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(owner.package_startup_rollback.is_none());
    }
}
