use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_extensions_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::PluginValidate => self.load_configured_plugins(),
            AppEffect::PluginList => {
                let items = self
                    .extension
                    .statuses()
                    .iter()
                    .take(4096)
                    .enumerate()
                    .map(|(index, (id, status))| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("plugin:{index}:{id}")),
                                label: id.clone(),
                                detail: format!("{status:?}"),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("plugin_picker_title", &[]), items);
            }
            AppEffect::PluginDisable(id) => {
                let load = self.extension.load_id(&id);
                if let Err(error) = self.extension.disable(&id) {
                    self.show_error(error)
                } else {
                    if let Some(load) = load {
                        self.remove_plugin_contribution_generation(&id, load);
                    }
                    self.commands.unregister_plugin(&id);
                    self.notify_message(self.message("plugin_disabled", &[("id", id)]), false);
                }
            }
            AppEffect::PluginPickerOpen { provider, query } => {
                self.open_plugin_picker(provider, query)
            }
            AppEffect::VirtualDocumentOpen { source, key } => {
                if key.len() > crate::extensions::VIRTUAL_DOCUMENT_KEY_BYTE_CEILING
                    || key.chars().any(char::is_control)
                {
                    self.show_error("invalid virtual document key");
                    return Ok(());
                }
                let Some((plugin, load, command)) =
                    self.plugin_contributions.ordered().find_map(|index| {
                        index
                            .virtual_documents
                            .iter()
                            .find(|declaration| declaration.id == source)
                            .map(|declaration| {
                                (
                                    index.owner.clone(),
                                    PluginLoadId(index.generation),
                                    declaration.command.clone(),
                                )
                            })
                    })
                else {
                    self.show_error("unknown virtual document source");
                    return Ok(());
                };
                if let Some(entry) = self
                    .plugin_virtual_editor
                    .find(&plugin, load, &source, &key)
                {
                    let _ = self.state.show_buffer(entry.buffer);
                    self.set_dashboard_visibility(false);
                    self.reconcile_editor_windows();
                    return Ok(());
                }
                match self
                    .plugin_virtual_documents
                    .start(&plugin, load, &source, key)
                {
                    Ok(_) => {
                        let prefix = format!("plugin.{plugin}.");
                        let Some(local) = command.strip_prefix(&prefix).map(str::to_owned) else {
                            self.show_error("invalid virtual document command");
                            return Ok(());
                        };
                        self.process_effect(AppEffect::PluginExecute {
                            plugin,
                            command: local,
                        });
                    }
                    Err(error) => self.show_error(error.detail),
                }
            }
            kind @ (AppEffect::HealthVirtualOpen | AppEffect::HealthVirtualRefresh) => {
                let load = PluginLoadId(1);
                let source = "builtin.health.virtual";
                let key = "health";
                if matches!(kind, AppEffect::HealthVirtualOpen) {
                    if let Some(entry) =
                        self.plugin_virtual_editor
                            .find("builtin.health", load, source, key)
                    {
                        let buffer = entry.buffer;
                        let _ = self.state.show_buffer(buffer);
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                        return Ok(());
                    }
                }
                let revision = self
                    .plugin_virtual_editor
                    .find("builtin.health", load, source, key)
                    .map_or(1, |entry| entry.revision.saturating_add(1));
                let plugin_rows = self.extension.statuses().len();
                let text = format!(
                    "# Dust health\n\n- editor: ready\n- plugins indexed: {plugin_rows}\n- API commands: {}\n- persistence schema: {}\n",
                    self.commands.specs().count(),
                    crate::persistence::STATE_VERSION
                );
                let snapshot = crate::extensions::PluginVirtualDocumentSnapshot {
                    plugin: "builtin.health".into(),
                    load,
                    source: source.into(),
                    document: 1,
                    key: key.into(),
                    title: "Dust health".into(),
                    language: "markdown".into(),
                    text,
                    revision,
                    closed: false,
                };
                let current = self.state.version();
                let mut state = self.state.draft();
                let mut bridge = self.plugin_virtual_editor.clone();
                match bridge.publish(&mut state, &snapshot) {
                    Ok(_) => {
                        let accepted = self
                            .window_session
                            .prepare_editor_transition(
                                state,
                                &current,
                                self.plugin_contributions.version(),
                                &self.configuration.settings().ui.buffer_line.scratch_label,
                                None,
                            )
                            .and_then(|transition| {
                                self.window_session.accept_editor_transition(
                                    transition,
                                    &current,
                                    &self.plugin_contributions.version(),
                                )
                            });
                        let state = match accepted {
                            Ok(state) => state,
                            Err(error) => {
                                self.show_error(format!("health document graph rejected: {error}"));
                                return Ok(());
                            }
                        };
                        self.state = state;
                        self.plugin_virtual_editor = bridge;
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(format!("health document rejected: {error:?}")),
                }
            }
            AppEffect::VirtualDocumentRefresh => {
                let Some(buffer) = self.state.active_buffer_id() else {
                    self.show_error("no active virtual document");
                    return Ok(());
                };
                let Some(entry) = self.plugin_virtual_editor.entry_for_buffer(buffer).cloned()
                else {
                    self.show_error("active buffer is not a virtual document");
                    return Ok(());
                };
                if let Err(error) =
                    self.plugin_virtual_documents
                        .refresh(&entry.plugin, entry.load, entry.document)
                {
                    self.show_error(error.detail);
                    return Ok(());
                }
                let command = self
                    .plugin_contributions
                    .get(&entry.plugin)
                    .and_then(|index| {
                        index
                            .virtual_documents
                            .iter()
                            .find(|d| d.id == entry.source)
                    })
                    .map(|d| d.command.clone());
                let Some(command) = command else {
                    self.show_error("virtual document source unavailable");
                    return Ok(());
                };
                let prefix = format!("plugin.{}.", entry.plugin);
                let Some(local) = command.strip_prefix(&prefix).map(str::to_owned) else {
                    return Ok(());
                };
                self.process_effect(AppEffect::PluginExecute {
                    plugin: entry.plugin,
                    command: local,
                });
            }
            AppEffect::PluginExecute { plugin, command } => {
                let Some(load) = self.extension.load_id(&plugin) else {
                    self.show_error(crate::extensions::PluginError::NotActive);
                    return Ok(());
                };
                if !self.extension.is_active_generation(&plugin, load) {
                    self.show_error(crate::extensions::PluginError::NotActive);
                    return Ok(());
                }
                self.refresh_plugin_host_events();
                self.retire_plugin_edit_sources();
                let maximum = usize::try_from(
                    self.configuration
                        .settings()
                        .workspace
                        .search
                        .maximum_file_bytes,
                )
                .unwrap_or(usize::MAX);
                let result = self.extension.execute(
                    &plugin,
                    &command,
                    crate::editor::session::extensions::InvocationContext {
                        editor: &self.state,
                        workspace: &self.workspace,
                        configuration: self.configuration.binding_revision_ref(),
                        generation: self.configuration.generation(),
                        maximum,
                        providers: &self.plugin_providers,
                        virtual_documents: self.plugin_virtual_documents.request_lane(),
                    },
                );
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::PackageInstall {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                } else {
                    self.defer_trusted_execution(DeferredExecution::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    });
                }
            }
            AppEffect::PackageRegistrySearch(query) => {
                self.request_registry(RegistryRequest::Search { query }, false)
            }
            AppEffect::PackageRegistryShow {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                false,
            ),
            AppEffect::PackageRegistryInstall {
                publisher,
                name,
                version,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]))
                } else {
                    self.defer_trusted_execution(DeferredExecution::InstallRegistry {
                        publisher,
                        name,
                        version,
                    })
                }
            }
            AppEffect::PackageList => {
                if let Err(error) = self.packages.request(PackageOperation::List) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageVerify => {
                if let Err(error) = self.packages.request(PackageOperation::Verify) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRollback => {
                self.package_startup_rollback_attempted = false;
                if let Err(error) = self.packages.request(PackageOperation::Rollback) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRemove(name) => {
                self.package_startup_rollback_attempted = false;
                if let Err(error) = self.packages.request(PackageOperation::Remove(name)) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageGarbageCollect => {
                if let Err(error) = self
                    .packages
                    .request(PackageOperation::GarbageCollect { maximum: 256 })
                {
                    self.show_error(error)
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
