use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn rebuild_matcher_cache(&mut self) {
        self.tasks.configure(self.configuration.settings());
    }

    pub(in crate::editor::session) fn plugin_limits(&self) -> PluginLimits {
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

    pub(in crate::editor::session) fn load_configured_plugins(&mut self) {
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
        for (id, entry) in policy.entries.iter().filter(|(_, entry)| entry.enabled) {
            if let Err(error) = self.plugins.index_from_store(
                id.clone(),
                store.clone(),
                entry.manifest.clone(),
                entry.component.clone(),
                limits.clone(),
            ) {
                self.show_error(self.message(
                    "plugin_load_failed",
                    &[("id", id.clone()), ("diagnostic", error.to_string())],
                ));
            }
        }
    }

    pub(in crate::editor::session) fn begin_package_activation_rollback(&mut self) {
        if self.package_startup_rollback.is_some() || self.package_startup_rollback_attempted {
            return;
        }
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
        if !failed.is_empty() {
            self.package_managed_plugins = failed;
            return;
        }
        self.package_startup_pending.clear();
        match self.packages.request(PackageOperation::Rollback) {
            Ok(id) => {
                self.package_startup_rollback_attempted = true;
                self.package_startup_rollback = Some(id)
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn handle_plugin_worker_stopped(
        &mut self,
        error: crate::extensions::PluginError,
    ) {
        if self.plugin_worker_reported {
            return;
        }
        self.plugin_worker_reported = true;
        self.pending_index_activation.clear();
        self.rejected_plugin_loads.clear();
        for plugin in self.plugins.statuses.keys().cloned().collect::<Vec<_>>() {
            self.commands.unregister_plugin(&plugin);
        }
        let owned_generations = self
            .plugin_contributions
            .ordered()
            .map(|index| (index.owner.clone(), PluginLoadId(index.generation)))
            .collect::<Vec<_>>();
        for (plugin, load) in owned_generations {
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        self.plugin_contributions = PluginContributionCatalog::default();
        self.plugin_host_events = PluginHostEventQueue::default();
        self.plugin_host_event_observation = None;
        if let Ok(candidate) = self.configuration.prepare_base_bindings(
            &self.commands,
            &self.plugin_contributions,
            true,
        ) && let Err(error) = self.commit_contribution_candidate(candidate)
        {
            self.show_error(error);
        }
        for (plugin, (_, token)) in self.package_prepared.drain() {
            self.commands.unregister_plugin(&plugin);
            let _ = self.packages.request(PackageOperation::Abort { token });
        }
        for plugin in self
            .package_startup_pending
            .keys()
            .chain(self.package_managed_plugins.keys())
            .cloned()
            .collect::<Vec<_>>()
        {
            self.commands.unregister_plugin(&plugin);
        }
        self.package_startup_pending.clear();
        self.package_managed_plugins.clear();
        for (plugin, _) in self.package_commits.values() {
            self.commands.unregister_plugin(plugin);
        }
        self.notices.record(
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

    pub(in crate::editor::session) fn register_indexed_plugin_commands(
        &mut self,
        index: &crate::extensions::PluginContributionIndex,
    ) -> Result<(), String> {
        let plugin = index.owner.clone();
        self.commands.unregister_plugin(&plugin);
        for (command, declaration) in &index.commands {
            let id = format!("plugin.{plugin}.{command}");
            let id = CommandId::new(&id).map_err(|error| error.to_string())?;
            let mut arguments = ArgumentSchema::default();
            for (name, field) in &declaration.arguments {
                arguments = arguments.field(
                    name,
                    match field.kind {
                        crate::extensions::PluginArgumentKind::String => {
                            crate::editor::ArgumentType::String
                        }
                        crate::extensions::PluginArgumentKind::Boolean => {
                            crate::editor::ArgumentType::Bool
                        }
                        crate::extensions::PluginArgumentKind::Integer => {
                            crate::editor::ArgumentType::Integer
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
                context: match declaration.context {
                    crate::extensions::PluginCommandContext::Application => {
                        ContextRequirement::Application
                    }
                    crate::extensions::PluginCommandContext::Workspace => {
                        ContextRequirement::Workspace
                    }
                    crate::extensions::PluginCommandContext::View => ContextRequirement::View,
                    crate::extensions::PluginCommandContext::Buffer => ContextRequirement::Buffer,
                    crate::extensions::PluginCommandContext::Selection => {
                        ContextRequirement::Selection
                    }
                },
                source: CommandSource::Plugin(plugin.clone()),
            };
            if let Err(error) = self.commands.register(spec, move |_, _| {
                Ok(CommandOutcome {
                    transaction: None,
                    effects: vec![AppEffect::PluginExecute {
                        plugin: effect_plugin.clone(),
                        command: effect_command.clone(),
                    }],
                    message: None,
                    follow_up: Vec::new(),
                })
            }) {
                self.commands.unregister_plugin(&plugin);
                return Err(error.to_string());
            }
        }
        Ok(())
    }

    pub(in crate::editor::session) fn recompile_plugin_contributions(
        &mut self,
    ) -> Result<(), String> {
        let candidate = self
            .configuration
            .prepare_plugin_contributions(&self.commands, &self.plugin_contributions)?;
        self.commit_contribution_candidate(candidate)
    }

    pub(in crate::editor::session) fn commit_contribution_candidate(
        &mut self,
        candidate: crate::editor::session::configuration::ContributionCandidate,
    ) -> Result<(), String> {
        // Both owners are checked before publication. There is no callback, wait,
        // or configuration mutation between this check and the two accepts.
        self.configuration
            .check_contributions(&candidate, &self.plugin_contributions)?;
        let mut plugin_windows = self.plugin_windows.clone();
        plugin_windows.set_configured_rules(candidate.windows())?;
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
        self.window_session
            .accept_current(windows, &self.state.window_version())?;
        self.configuration
            .commit_contributions(candidate, &self.plugin_contributions)?;
        self.plugin_windows = plugin_windows;
        self.cancel_pending_mapping();
        self.reconcile_editor_windows();
        Ok(())
    }

    pub(in crate::editor::session) fn remove_plugin_contribution_generation(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) {
        match self
            .window_session
            .remove_plugin_generation(plugin, load.0)
            .map(|()| self.plugin_windows.remove_generation(plugin, load.0))
        {
            Ok(removed) => {
                self.pending_plugin_ui_events
                    .remove(&(plugin.to_owned(), load));
                if removed {
                    self.ui.plugin_surfaces = self.plugin_windows.snapshots();
                    self.window_session.invalidate_native();
                    self.reconcile_editor_windows();
                }
            }
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        self.plugin_host_events.remove_generation(plugin, load);
        self.plugin_host_event_observation = None;
        self.plugin_storage.remove_generation(plugin, load);
        self.plugin_providers.remove_generation(plugin, load);
        self.plugin_virtual_documents
            .remove_generation(plugin, load);
        let mut virtual_state = self.state.clone();
        let mut virtual_editor = self.plugin_virtual_editor.clone();
        if virtual_editor.remove_generation(&mut virtual_state, plugin, load) > 0 {
            self.state = virtual_state;
            self.plugin_virtual_editor = virtual_editor;
            self.reconcile_editor_windows();
        }
        if self
            .active_plugin_picker
            .is_some_and(|request| self.plugin_providers.projection(request).is_none())
        {
            self.active_plugin_picker = None;
            self.close_picker();
        }
        self.pending_plugin_storage_results
            .remove(&(plugin.to_owned(), load));
        if self.plugin_contributions.remove_generation(plugin, load.0) {
            self.commands.unregister_plugin(plugin);
            if let Err(error) = self.recompile_plugin_contributions() {
                match self
                    .configuration
                    .prepare_builtin_contributions(&self.commands, &self.plugin_contributions, true)
                    .and_then(|candidate| self.commit_contribution_candidate(candidate))
                {
                    Ok(()) => self.show_error(error),
                    Err(fallback) => self.show_error(fallback),
                }
            }
        }
    }

    pub(in crate::editor::session) fn abort_pending_index_batch(
        &mut self,
        diagnostic: impl std::fmt::Display,
    ) {
        let pending = std::mem::take(&mut self.pending_index_activation);
        for (owner, (load, _)) in pending {
            self.rejected_plugin_loads.insert((owner.clone(), load));
            self.remove_plugin_contribution_generation(&owner, load);
            self.commands.unregister_plugin(&owner);
            let _ = self.plugins.disable_generation(&owner, load);
            if let Some((_, token)) = self
                .package_prepared
                .remove(&owner)
                .filter(|(candidate, _)| *candidate == load)
            {
                let _ = self.packages.request(PackageOperation::Abort { token });
            }
        }
        let candidate = self
            .configuration
            .prepare_builtin_contributions(
                &self.commands,
                &self.plugin_contributions,
                self.configuration.deferred_plugin_keymaps(),
            )
            .unwrap_or_else(|_| {
                self.configuration
                    .prepare_builtin_rules(&self.plugin_contributions)
            });
        if let Err(error) = self.commit_contribution_candidate(candidate) {
            self.show_error(error);
        }
        self.show_error(diagnostic);
    }

    pub(in crate::editor::session) fn abort_expected_index_batch(
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
        for owner in expected {
            let load = if owner == failed_plugin {
                Some(failed_load)
            } else {
                self.plugins.load_id(&owner)
            };
            if let Some(load) = load {
                self.rejected_plugin_loads.insert((owner.clone(), load));
                self.pending_index_activation
                    .entry(owner)
                    .or_insert_with(|| (load, String::new()));
            }
        }
        self.abort_pending_index_batch(diagnostic);
    }
}
