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
        if let Err(error) = self.extension.request_package(operation) {
            self.show_error(error)
        }
    }

    pub(super) fn request_registry(&mut self, request: RegistryRequest, install: bool) {
        if let Err(error) = self.extension.request_registry(request, install) {
            self.show_error(error);
        }
    }
    pub(super) fn process_registry_events(&mut self) {
        use crate::editor::session::extensions::RegistryUpdate;
        for update in self.extension.poll_registry() {
            match update {
                RegistryUpdate::Present(result) => match *result {
                    RegistryResult::Search(result) => {
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
                        self.install_picker(
                            self.message("package_registry_search_title", &[]),
                            items,
                        );
                    }
                    RegistryResult::Show(metadata) => {
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
                    RegistryResult::Artifact { .. } => self.show_error("unowned registry artifact"),
                },
                RegistryUpdate::ValidateArtifact => {
                    self.defer_trusted_execution(DeferredExecution::PrepareRegistryArtifact);
                    if self.execution_validation.is_none() {
                        self.extension.discard_artifact();
                    }
                }
                RegistryUpdate::Error(error) => self
                    .show_error(self.message("package_registry_failed", &[("diagnostic", error)])),
            }
        }
    }
    pub(super) fn process_package_events(&mut self) {
        let updates = self.extension.poll_packages(self.plugin_limits());
        self.apply_extension_updates(updates);
    }
    pub(in crate::editor::session) fn apply_extension_updates(
        &mut self,
        updates: Vec<crate::editor::session::extensions::ExtensionUpdate>,
    ) {
        use crate::editor::session::extensions::ExtensionUpdate;
        for update in updates {
            match update {
                ExtensionUpdate::Remove(plugin, load) => {
                    self.remove_plugin_contribution_generation(&plugin, load)
                }
                ExtensionUpdate::Error(error) => self.show_error(error),
                ExtensionUpdate::Conflict(plugin) => {
                    self.show_error(self.message("package_plugin_conflict", &[("plugin", plugin)]))
                }
                ExtensionUpdate::Message(key, arguments) => {
                    self.notify_message(self.message(key, &arguments), false)
                }
                ExtensionUpdate::Lock(lock) => {
                    let items = lock
                        .packages
                        .into_iter()
                        .enumerate()
                        .map(|(index, package)| {
                            (
                                PickerItem {
                                    id: PickerItemId(format!("package:{index}:{}", package.name)),
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
                }
                ExtensionUpdate::Failed { operation, error } => {
                    self.notices.record(
                        LogLevel::Error,
                        "packages",
                        "operation-failed",
                        BTreeMap::from([
                            ("operation".into(), operation.clone()),
                            ("error".into(), error.clone()),
                        ]),
                    );
                    self.show_error(self.message(
                        "package_operation_failed",
                        &[("operation", operation), ("diagnostic", error)],
                    ));
                }
            }
        }
    }
}
