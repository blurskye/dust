use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn queue_plugin_storage_result(
        &mut self,
        result: PluginStorageResult,
    ) {
        let key = (result.plugin.clone(), result.load);
        let pending = self.pending_plugin_storage_results.entry(key).or_default();
        pending.push(result);
        while pending.len() > crate::extensions::STORAGE_REQUEST_CEILING
            || pending
                .iter()
                .map(PluginStorageResult::bounded_bytes)
                .sum::<usize>()
                > crate::extensions::STORAGE_RESULT_QUEUE_BYTE_CEILING
        {
            pending.remove(0);
        }
    }

    pub(in crate::editor::session) fn process_plugin_storage_events(&mut self) {
        for result in self.plugin_storage.poll() {
            if self.plugins.load_id(&result.plugin) != Some(result.load)
                || !self
                    .plugins
                    .is_active_generation(&result.plugin, result.load)
            {
                continue;
            }
            self.queue_plugin_storage_result(result);
        }
    }

    pub(in crate::editor::session) fn refresh_plugin_host_events(&mut self) {
        let active_surface = if self.terminal_focused {
            "terminal"
        } else if self.ui.explorer_focused {
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
        let buffer = self.state.active_view.and_then(|view_id| {
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
        let graph_revision = self.windows.revision().unwrap_or(0);
        let observation = (
            self.workspace.root_revision(),
            active_surface.clone(),
            buffer,
            graph_revision,
        );
        if self.plugin_host_event_observation.as_ref() == Some(&observation) {
            return;
        }
        self.plugin_host_event_observation = Some(observation.clone());
        let generations = self
            .plugin_contributions
            .ordered()
            .map(|index| (index.owner.clone(), PluginLoadId(index.generation)))
            .collect::<Vec<_>>();
        for (plugin, load) in generations {
            let _ = self.plugin_host_events.observe(
                &plugin,
                load,
                PluginSubscriptionKind::WorkspaceMetadataChanged,
                observation.0,
                format!("roots={}", self.workspace.roots().len()),
            );
            let _ = self.plugin_host_events.observe(
                &plugin,
                load,
                PluginSubscriptionKind::ActiveSurfaceChanged,
                graph_revision,
                active_surface.clone(),
            );
            if let Some((id, revision, cursor, anchor, head, kind)) = buffer {
                let _ = self.plugin_host_events.observe(
                    &plugin,
                    load,
                    PluginSubscriptionKind::BufferSummaryChanged,
                    revision,
                    format!(
                        "buffer={id};revision={revision};cursor={cursor};selection={anchor}:{head}:{kind}"
                    ),
                );
            }
            let _ = self.plugin_host_events.observe(
                &plugin,
                load,
                PluginSubscriptionKind::PluginSurfaceChanged,
                graph_revision,
                format!(
                    "retained={};private={}",
                    self.ui.plugin_surfaces.len(),
                    self.ui.private_surfaces.len()
                ),
            );
        }
    }

    pub(in crate::editor::session) fn process_plugin_events(&mut self) {
        let events = match self.plugins.poll() {
            Ok(events) => events,
            Err(error) => {
                self.handle_plugin_worker_stopped(error);
                return;
            }
        };
        for event in events {
            match event {
                PluginEvent::Indexed {
                    load,
                    plugin,
                    index,
                } => {
                    if self.rejected_plugin_loads.remove(&(plugin.clone(), load)) {
                        let _ = self.plugins.disable_generation(&plugin, load);
                        continue;
                    }
                    let checksum = index.checksum.clone();
                    let mut accepted = self
                        .plugin_contributions
                        .install(index.clone())
                        .map_err(|error| error.to_string())
                        .and_then(|()| {
                            self.plugin_host_events
                                .install(&index)
                                .map_err(|error| error.to_string())
                        })
                        .and_then(|()| {
                            self.plugin_providers
                                .install(&index)
                                .map_err(|error| error.detail)
                        })
                        .and_then(|()| {
                            self.plugin_virtual_documents
                                .install(&index)
                                .map_err(|error| error.detail)
                        })
                        .and_then(|()| self.register_indexed_plugin_commands(&index));
                    self.plugin_host_event_observation = None;
                    if accepted.is_ok() && index.storage.is_some() {
                        self.plugin_storage.install_generation(&plugin, load);
                    }
                    self.pending_index_activation
                        .insert(plugin.clone(), (load, checksum.clone()));
                    let waiting_for_deferred = self.configuration.deferred_plugin_keymaps
                        && !self
                            .configuration
                            .expected_plugin_indexes
                            .iter()
                            .all(|owner| self.plugin_contributions.get(owner).is_some());
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
                    let activations = if self.configuration.deferred_plugin_keymaps {
                        std::mem::take(&mut self.pending_index_activation)
                    } else {
                        BTreeMap::from([(plugin.clone(), (load, checksum))])
                    };
                    let activation_error =
                        activations
                            .into_iter()
                            .find_map(|(owner, (candidate, checksum))| {
                                self.plugins
                                    .activate_indexed(&owner, candidate, checksum)
                                    .err()
                                    .map(|error| (owner, candidate, error))
                            });
                    if let Some((failed_plugin, failed_load, error)) = activation_error {
                        self.remove_plugin_contribution_generation(&failed_plugin, failed_load);
                        let _ = self.plugins.disable_generation(&failed_plugin, failed_load);
                        if let Some((_, token)) = self
                            .package_prepared
                            .remove(&failed_plugin)
                            .filter(|(candidate, _)| *candidate == failed_load)
                        {
                            let _ = self.packages.request(PackageOperation::Abort { token });
                        }
                        self.show_error(error);
                        continue;
                    }
                    self.logs.record(
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
                    if self.rejected_plugin_loads.remove(&(plugin.clone(), load)) {
                        let _ = self.plugins.disable_generation(&plugin, load);
                        continue;
                    }
                    let prepared = self
                        .package_prepared
                        .get(&plugin)
                        .filter(|(candidate, _)| *candidate == load)
                        .map(|(_, token)| *token);
                    if prepared.is_some() {
                        self.package_prepared.remove(&plugin);
                    }
                    if self.package_startup_pending.get(&plugin) == Some(&load) {
                        self.package_startup_pending.remove(&plugin);
                    }
                    if let Some(index) = self
                        .plugin_contributions
                        .get(&plugin)
                        .filter(|index| index.generation == load.0)
                    {
                        let declared = index.commands.keys().cloned().collect::<BTreeSet<_>>();
                        let activated = component
                            .activated_commands
                            .iter()
                            .cloned()
                            .collect::<BTreeSet<_>>();
                        if declared != activated {
                            if let Some(token) = prepared {
                                let _ = self.packages.request(PackageOperation::Abort { token });
                            }
                            self.remove_plugin_contribution_generation(&plugin, load);
                            let _ = self.plugins.disable_generation(&plugin, load);
                            self.show_error(crate::extensions::PluginError::UndeclaredCommand);
                            continue;
                        }
                    }
                    self.commands.unregister_plugin(&plugin);
                    for command in &component.activated_commands {
                        let id = format!("plugin.{plugin}.{command}");
                        let Ok(id) = CommandId::new(&id) else {
                            continue;
                        };
                        let Some(declaration) = component.manifest.commands.get(command) else {
                            continue;
                        };
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
                                crate::extensions::PluginCommandContext::View => {
                                    ContextRequirement::View
                                }
                                crate::extensions::PluginCommandContext::Buffer => {
                                    ContextRequirement::Buffer
                                }
                                crate::extensions::PluginCommandContext::Selection => {
                                    ContextRequirement::Selection
                                }
                            },
                            source: CommandSource::Plugin(plugin.clone()),
                        };
                        let _ = self.commands.register(spec, move |_, _| {
                            Ok(CommandOutcome {
                                transaction: None,
                                effects: vec![AppEffect::PluginExecute {
                                    plugin: effect_plugin.clone(),
                                    command: effect_command.clone(),
                                }],
                                message: None,
                                follow_up: Vec::new(),
                            })
                        });
                    }
                    if let Some(token) = prepared {
                        match self.packages.request(PackageOperation::Commit { token }) {
                            Ok(request) => {
                                self.package_commits.insert(request, (plugin.clone(), load));
                            }
                            Err(error) => {
                                let _ = self.plugins.disable_generation(&plugin, load);
                                self.remove_plugin_contribution_generation(&plugin, load);
                                let _ = self.packages.request(PackageOperation::Abort { token });
                                self.show_error(error)
                            }
                        }
                    }
                    self.logs.record(
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
                PluginEvent::Executed(execution) => self.apply_plugin_execution(execution),
                PluginEvent::Failed {
                    load,
                    plugin,
                    error,
                } => {
                    if self.rejected_plugin_loads.remove(&(plugin.clone(), load)) {
                        continue;
                    }
                    if self.configuration.deferred_plugin_keymaps
                        && self.configuration.expected_plugin_indexes.contains(&plugin)
                    {
                        self.abort_expected_index_batch(plugin, load, error);
                        continue;
                    }
                    if self.pending_index_activation.contains_key(&plugin) {
                        self.rejected_plugin_loads.insert((plugin, load));
                        self.abort_pending_index_batch(error);
                        continue;
                    }
                    let prepared = self
                        .package_prepared
                        .get(&plugin)
                        .filter(|(candidate, _)| *candidate == load)
                        .map(|(_, token)| *token);
                    if let Some(token) = prepared {
                        self.package_prepared.remove(&plugin);
                        let _ = self.packages.request(PackageOperation::Abort { token });
                    } else if self.package_startup_pending.get(&plugin) == Some(&load) {
                        self.package_startup_pending.remove(&plugin);
                        self.begin_package_activation_rollback();
                    }
                    self.remove_plugin_contribution_generation(&plugin, load);
                    self.commands.unregister_plugin(&plugin);
                    self.logs.record(
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
    }
}
