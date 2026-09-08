use super::*;

impl InteractiveApplication {
    pub(super) fn install_package_after_validation(
        &mut self,
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    ) {
        let root = self.state.workspace_root.clone();
        let absolute = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        };
        let operation = PackageOperation::Install {
            package_manifest: absolute(package_manifest),
            plugin_manifest: absolute(plugin_manifest),
            component: absolute(component),
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            limits: self.plugin_limits(),
            update,
        };
        if let Err(error) = self.packages.request(operation) {
            self.show_error(error)
        }
    }

    pub(super) fn request_registry(&mut self, request: RegistryRequest, install: bool) {
        match self.registry.request(request) {
            Ok(id) => {
                self.registry_intents.insert(id, install);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn process_registry_events(&mut self) {
        let Ok(events) = self.registry.poll() else {
            return;
        };
        for event in events {
            let install = self.registry_intents.remove(&event.id).unwrap_or(false);
            match event.result {
                Ok(RegistryResult::Search(result)) => {
                    let items = result
                        .packages
                        .into_iter()
                        .enumerate()
                        .map(|(index, package)| {
                            (
                                PickerItem {
                                    id: PickerItemId(format!(
                                        "registry:{index}:{}/{}",
                                        package.publisher, package.name
                                    )),
                                    label: format!("{}/{}", package.publisher, package.name),
                                    detail: format!(
                                        "{}{}",
                                        package.version,
                                        if package.yanked { " · yanked" } else { "" }
                                    ),
                                    source: package.description.clone(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::RegistrySummary(package),
                            )
                        })
                        .collect();
                    self.install_picker(self.message("package_registry_search_title", &[]), items);
                }
                Ok(RegistryResult::Show(metadata)) => {
                    if install {
                        self.request_registry(
                            RegistryRequest::Fetch {
                                metadata: Box::new(metadata),
                            },
                            true,
                        )
                    } else {
                        let label = format!("{}/{}", metadata.publisher, metadata.name);
                        let detail = format!("{} · {}", metadata.version, metadata.artifact_sha256);
                        self.install_picker(
                            self.message("package_registry_show_title", &[]),
                            vec![(
                                PickerItem {
                                    id: PickerItemId(format!("registry-show:{label}")),
                                    label,
                                    detail,
                                    source: metadata.artifact_path.clone(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::RegistryMetadata(Box::new(metadata)),
                            )],
                        );
                    }
                }
                Ok(RegistryResult::Artifact { metadata, bytes }) => {
                    if self.pending_registry_artifact.is_some() {
                        self.show_error("another registry artifact is awaiting validation")
                    } else {
                        self.pending_registry_artifact = Some((metadata, bytes));
                        self.defer_trusted_execution(DeferredExecution::PrepareRegistryArtifact);
                        if self.execution_validation.is_none() {
                            self.pending_registry_artifact = None;
                        }
                    }
                }
                Err(error) => self.show_error(self.message(
                    "package_registry_failed",
                    &[("diagnostic", error.to_string())],
                )),
            }
        }
    }

    pub(super) fn process_package_events(&mut self) {
        if let Ok(events) = self.packages.poll() {
            for event in events {
                match event {
                    PackageServiceEvent::Completed(mut result) => {
                        let prepared_operation = result.prepared.is_some();
                        let activation_operation = result.operation == "activations";
                        if let Some(prepared) = result.prepared.take() {
                            let id = prepared.plugin.id.clone();
                            let token = prepared.token;
                            let store =
                                crate::config::user_state_directory().join("packages/store");
                            if self.plugins.load_id(&id).is_some() {
                                let _ = self.packages.request(PackageOperation::Abort { token });
                                self.show_error(
                                    self.message("package_plugin_conflict", &[("plugin", id)]),
                                );
                                continue;
                            }
                            match self.plugins.load(
                                store,
                                prepared.object,
                                prepared.plugin,
                                self.plugin_limits(),
                            ) {
                                Ok(load) => {
                                    self.package_prepared.insert(id.clone(), (load, token));
                                    self.notify_message(
                                        self.message(
                                            "package_prepared",
                                            &[("package", prepared.manifest.name)],
                                        ),
                                        false,
                                    );
                                }
                                Err(error) => {
                                    let _ =
                                        self.packages.request(PackageOperation::Abort { token });
                                    self.show_error(error)
                                }
                            }
                        }
                        for (manifest, path) in std::mem::take(&mut result.activations) {
                            let id = manifest.id.clone();
                            if self.plugins.load_id(&id).is_some() {
                                self.show_error(
                                    self.message("package_plugin_conflict", &[("plugin", id)]),
                                );
                                self.begin_package_activation_rollback();
                                break;
                            }
                            let store =
                                crate::config::user_state_directory().join("packages/store");
                            match self
                                .plugins
                                .load(store, path, manifest, self.plugin_limits())
                            {
                                Ok(load) => {
                                    self.package_startup_pending.insert(id.clone(), load);
                                    self.package_managed_plugins.insert(id, load);
                                }
                                Err(error) => {
                                    self.show_error(error);
                                    self.begin_package_activation_rollback();
                                    break;
                                }
                            }
                        }
                        if let Some((plugin, load)) = self.package_commits.remove(&result.id) {
                            if self.plugins.load_id(&plugin) == Some(load) {
                                self.package_managed_plugins.insert(plugin, load);
                            } else {
                                self.package_startup_rollback_attempted = false;
                                self.begin_package_activation_rollback();
                            }
                        }
                        if self.package_startup_request == Some(result.id) {
                            self.package_startup_request = None;
                        }
                        let reconcile = matches!(result.operation.as_str(), "rollback" | "remove");
                        if self.package_startup_rollback == Some(result.id) {
                            self.package_startup_rollback = None;
                        }
                        if reconcile {
                            let managed = std::mem::take(&mut self.package_managed_plugins);
                            let mut failed = HashMap::new();
                            for (id, load) in managed {
                                match self.plugins.unload_generation(&id, load) {
                                    Ok(()) => self.remove_plugin_contribution_generation(&id, load),
                                    Err(error) => {
                                        failed.insert(id, load);
                                        self.show_error(error);
                                    }
                                }
                            }
                            self.package_managed_plugins = failed;
                            self.package_startup_pending.clear();
                            if !self.package_managed_plugins.is_empty() {
                                self.begin_package_activation_rollback();
                                continue;
                            }
                            match self.packages.request(PackageOperation::Activations) {
                                Ok(id) => self.package_startup_request = Some(id),
                                Err(error) => {
                                    self.show_error(error);
                                    self.begin_package_activation_rollback();
                                }
                            }
                        }
                        if let Some(lock) = result.lock {
                            let items = lock
                                .packages
                                .into_iter()
                                .enumerate()
                                .map(|(index, package)| {
                                    (
                                        PickerItem {
                                            id: PickerItemId(format!(
                                                "package:{index}:{}",
                                                package.name
                                            )),
                                            label: package.name,
                                            detail: package.version,
                                            source: "package".into(),
                                            action: PickerAction::None,
                                        },
                                        PickerPayload::Command(String::new()),
                                    )
                                })
                                .collect();
                            self.install_picker(self.message("package_picker_title", &[]), items);
                        } else if !prepared_operation && !activation_operation {
                            self.notify_message(
                                self.message(
                                    "package_operation_complete",
                                    &[
                                        ("operation", result.operation),
                                        (
                                            "count",
                                            result
                                                .verified
                                                .or(result.removed)
                                                .unwrap_or(0)
                                                .to_string(),
                                        ),
                                    ],
                                ),
                                false,
                            );
                        }
                    }
                    PackageServiceEvent::Failed {
                        id,
                        operation,
                        error,
                    } => {
                        if self.package_startup_request == Some(id) {
                            self.package_startup_request = None;
                            self.begin_package_activation_rollback();
                        }
                        if let Some((plugin, load)) = self.package_commits.remove(&id) {
                            if self.package_managed_plugins.get(&plugin) == Some(&load) {
                                self.package_managed_plugins.remove(&plugin);
                            }
                            let _ = self.plugins.disable_generation(&plugin, load);
                            self.remove_plugin_contribution_generation(&plugin, load);
                        }
                        self.notices.record(
                            LogLevel::Error,
                            "packages",
                            "operation-failed",
                            BTreeMap::from([
                                ("operation".into(), operation.clone()),
                                ("error".into(), error.to_string()),
                            ]),
                        );
                        self.show_error(self.message(
                            "package_operation_failed",
                            &[("operation", operation), ("diagnostic", error.to_string())],
                        ));
                    }
                }
            }
        }
    }
}
