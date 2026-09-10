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
                let result = self.extension.open_virtual(&source, key, &self.state);
                self.route_virtual_open(result);
            }
            kind @ (AppEffect::HealthVirtualOpen | AppEffect::HealthVirtualRefresh) => {
                let result = self
                    .extension
                    .prepare_health(
                        matches!(kind, AppEffect::HealthVirtualRefresh),
                        self.commands.specs().count(),
                        VirtualContext {
                            editor: &self.state,
                            windows: &self.window_session,
                            scratch: &self.configuration.settings().ui.buffer_line.scratch_label,
                        },
                    )
                    .and_then(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    });
                match result {
                    Ok(state) => {
                        self.state = state;
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::VirtualDocumentRefresh => {
                let result = self
                    .state
                    .active_buffer_id()
                    .ok_or_else(|| "no active virtual document".to_owned())
                    .and_then(|buffer| self.extension.refresh_virtual(buffer, &self.state));
                self.route_virtual_open(result);
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
                let input_capture = self.input.capture_epoch();
                let interaction_available = self.input.prompt().is_none()
                    && self.ui.picker.is_none()
                    && self.ui.tool_menu.is_none()
                    && self.input.documentation().is_none()
                    && !self.input.mapping_pending()
                    && !self.input.grammar_pending();
                let navigation = self
                    .window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view);
                let result = self.extension.execute(
                    &plugin,
                    &command,
                    crate::editor::session::extensions::InvocationContext {
                        editor: &self.state,
                        workspace: &self.workspace,
                        configuration: self.configuration.binding_revision_ref(),
                        generation: self.configuration.generation(),
                        ui_revision: self.window_session.graph().revision(),
                        input_capture,
                        navigation,
                        interaction_available,
                        maximum,
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
                if let Err(error) = self.extension.request_package(PackageOperation::List) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageVerify => {
                if let Err(error) = self.extension.request_package(PackageOperation::Verify) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRollback => {
                if let Err(error) = self.extension.request_package(PackageOperation::Rollback) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRemove(name) => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::Remove(name))
                {
                    self.show_error(error)
                }
            }
            AppEffect::PackageGarbageCollect => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::GarbageCollect { maximum: 256 })
                {
                    self.show_error(error)
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
