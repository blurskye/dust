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
                    .plugins
                    .statuses
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
                let load = self.plugins.load_id(&id);
                if let Err(error) = self.plugins.disable(&id) {
                    self.show_error(error)
                } else {
                    if let Some(load) = load {
                        self.remove_plugin_contribution_generation(&id, load);
                    }
                    self.commands.unregister_plugin(&id);
                    self.ui.notification = Some(self.message("plugin_disabled", &[("id", id)]));
                    self.ui.notification_error = false;
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
                    self.ui.dashboard = false;
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
                        self.ui.dashboard = false;
                        self.reconcile_editor_windows();
                        return Ok(());
                    }
                }
                let revision = self
                    .plugin_virtual_editor
                    .find("builtin.health", load, source, key)
                    .map_or(1, |entry| entry.revision.saturating_add(1));
                let plugin_rows = self.plugins.statuses.len();
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
                let mut state = self.state.clone();
                let mut bridge = self.plugin_virtual_editor.clone();
                match bridge.publish(&mut state, &snapshot) {
                    Ok(_) => {
                        let mut graph = self.windows.clone();
                        let mut editor = self.editor_windows.clone();
                        if let Err(error) = editor.reconcile(
                            &state,
                            &mut graph,
                            &self.configuration.settings.ui.buffer_line.scratch_label,
                        ) {
                            self.show_error(format!("health document graph rejected: {error}"));
                            return Ok(());
                        }
                        self.state = state;
                        self.plugin_virtual_editor = bridge;
                        self.windows = graph;
                        self.editor_windows = editor;
                        self.ui.dashboard = false;
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
                let Some(load) = self.plugins.load_id(&plugin) else {
                    self.show_error(crate::extensions::PluginError::NotActive);
                    return Ok(());
                };
                if !self.plugins.is_active_generation(&plugin, load) {
                    self.show_error(crate::extensions::PluginError::NotActive);
                    return Ok(());
                }
                self.refresh_plugin_host_events();
                let snapshot = self.state.active_buffer_id().and_then(|id| {
                    self.state.buffers.get(id).map(|buffer| PluginSnapshot {
                        buffer: id.data().as_ffi(),
                        revision: buffer.text.revision().0,
                        utf8: buffer.text.to_string(),
                    })
                });
                if let Err(error) =
                    self.plugins
                        .execute(crate::extensions::PluginExecutionRequest {
                            plugin: plugin.clone(),
                            command: command.clone(),
                            arguments: Vec::new(),
                            snapshot,
                            config: BTreeMap::new(),
                            ui_events: self
                                .pending_plugin_ui_events
                                .remove(&(plugin.clone(), load))
                                .unwrap_or_default(),
                            host_events: self.plugin_host_events.deliver(&plugin, load),
                            storage_results: self
                                .pending_plugin_storage_results
                                .remove(&(plugin.clone(), load))
                                .unwrap_or_default(),
                            provider_requests: self.plugin_providers.requests(&plugin, load),
                            virtual_document_requests: self
                                .plugin_virtual_documents
                                .requests_for_command(
                                    &plugin,
                                    load,
                                    &format!("plugin.{plugin}.{command}"),
                                ),
                            generation: self.configuration_generation,
                        })
                {
                    self.show_error(error)
                }
            }
            AppEffect::PackageInstall {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => {
                if self.safe_mode {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                } else if !self.execution_revalidated {
                    self.defer_trusted_execution(DeferredExecution::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    });
                } else {
                    self.install_package_after_validation(
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    )
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
                if self.safe_mode {
                    self.show_error(self.message("safe_mode_blocked", &[]))
                } else if !self.execution_revalidated {
                    self.defer_trusted_execution(DeferredExecution::InstallRegistry {
                        publisher,
                        name,
                        version,
                    })
                } else {
                    self.request_registry(
                        RegistryRequest::Show {
                            publisher,
                            name,
                            version,
                        },
                        true,
                    )
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
