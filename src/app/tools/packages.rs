//! Package installation and activation in the application.

use crate::app::App;
use crate::app::TrustedAction;
use crate::app::picker::PickerPayload;
use crate::telemetry::LogLevel;
use crate::tools::PackageOperation;
use crate::tools::RegistryRequest;
use crate::tools::RegistryResult;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use std::collections::BTreeMap;
use std::path::PathBuf;

impl App {
    pub(in crate::app) fn install_package_after_validation(
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
}

impl App {
    pub(in crate::app) fn request_registry(&mut self, request: RegistryRequest, install: bool) {
        let picker = if install {
            None
        } else {
            let title = match &request {
                RegistryRequest::Search { .. } => "package_registry_search_title",
                _ => "package_registry_show_title",
            };
            self.install_picker(self.message(title, &[]), Vec::new());
            self.search.picker_epoch().cloned()
        };
        if let Err(error) = self.extension.request_registry(request, install, picker) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_registry_events(&mut self) {
        use crate::app::plugins::RegistryUpdate;
        for update in self.extension.poll_registry() {
            match update {
                RegistryUpdate::Present(_, ref picker)
                    if !picker
                        .as_ref()
                        .is_some_and(|epoch| self.search.owns_picker_epoch(epoch)) => {}
                RegistryUpdate::Present(result, _) => match *result {
                    RegistryResult::Search(result) => {
                        let items = result
                            .packages
                            .into_iter()
                            .enumerate()
                            .map(|(index, package)| {
                                (
                                    PickerItem {
                                        badge: String::new(),
                                        kind: crate::workspace::PickerItemKind::Entry,
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
                                    badge: String::new(),
                                    kind: crate::workspace::PickerItemKind::Entry,
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
                RegistryUpdate::ValidateArtifact => match self.workspace.execution_allowed() {
                    Ok(()) => self.run_trusted(TrustedAction::PrepareRegistryArtifact),
                    Err(failure) => {
                        self.show_workspace_failure(failure);
                        self.extension.discard_artifact();
                    }
                },
                RegistryUpdate::Error(error) => self
                    .show_error(self.message("package_registry_failed", &[("diagnostic", error)])),
            }
        }
    }
}

impl App {
    pub(in crate::app) fn process_package_events(&mut self) {
        let updates = self.extension.poll_packages(
            self.plugin_limits(),
            &self.configuration.settings().plugins,
            self.workspace.safe_mode(),
        );
        self.apply_extension_updates(updates);
    }
}

impl App {
    pub(in crate::app) fn apply_extension_updates(
        &mut self,
        updates: Vec<crate::app::plugins::ExtensionUpdate>,
    ) {
        use crate::app::plugins::ExtensionUpdate;
        for update in updates {
            match update {
                ExtensionUpdate::Remove(plugin, load) => {
                    self.remove_plugin_contributions(&plugin, load)
                }
                ExtensionUpdate::Error(error) => self.show_error(error),
                ExtensionUpdate::Conflict(plugin) => {
                    self.show_error(self.message("package_plugin_conflict", &[("plugin", plugin)]))
                }
                ExtensionUpdate::Message(key, arguments) => {
                    self.notify_message(self.message(key, &arguments), false)
                }
                ExtensionUpdate::Lock(lock) => {
                    let items: Vec<_> = lock
                        .packages
                        .into_iter()
                        .enumerate()
                        .map(|(index, package)| {
                            (
                                PickerItem {
                                    badge: String::new(),
                                    kind: crate::workspace::PickerItemKind::Entry,
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
                    let empty = items.is_empty();
                    self.install_picker(self.message("package_picker_title", &[]), items);
                    if empty {
                        self.search
                            .set_picker_footer(self.message("package_picker_empty", &[]));
                        self.sync_picker_ui();
                    }
                }
                ExtensionUpdate::Failed { operation, error } => {
                    self.health.record(
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
