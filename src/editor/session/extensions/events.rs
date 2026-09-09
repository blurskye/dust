use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_plugin_storage_events(&mut self) {
        self.extension.poll_storage();
    }

    pub(in crate::editor::session) fn refresh_plugin_host_events(&mut self) {
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

    pub(in crate::editor::session) fn process_plugin_events(&mut self) {
        self.retire_plugin_edit_sources();
        let events = match self.extension.poll_owned() {
            Ok(events) => events,
            Err(error) => {
                self.retire_plugin_edit_sources();
                self.handle_plugin_worker_stopped(error);
                return;
            }
        };
        for event in events {
            let event = match event {
                crate::extensions::OwnedPluginEvent::Executed(reply) => {
                    self.apply_plugin_execution(reply);
                    continue;
                }
                crate::extensions::OwnedPluginEvent::Other(event) => event,
            };
            match event {
                PluginEvent::Indexed {
                    load,
                    plugin,
                    index,
                } => {
                    if self.extension.take_rejected(&plugin, load) {
                        let _ = self.extension.disable_generation(&plugin, load);
                        continue;
                    }
                    let checksum = index.checksum.clone();
                    let mut accepted = self
                        .extension
                        .install_index(index.clone())
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
                        self.remove_plugin_contribution_generation(&failed_plugin, failed_load);
                        let _ = self
                            .extension
                            .disable_generation(&failed_plugin, failed_load);
                        self.extension.abort_prepared(&failed_plugin, failed_load);
                        self.show_error(error);
                        continue;
                    }
                    self.notices.record(
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
                        let _ = self.extension.disable_generation(&plugin, load);
                        continue;
                    }
                    let prepared = self.extension.take_prepared(&plugin, load);
                    if let Some(index) = self
                        .extension
                        .contributions()
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
                                let _ = self
                                    .extension
                                    .request_package(PackageOperation::Abort { token });
                            }
                            self.remove_plugin_contribution_generation(&plugin, load);
                            let _ = self.extension.disable_generation(&plugin, load);
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
                        if let Err(error) =
                            self.extension.commit_package(plugin.clone(), load, token)
                        {
                            self.remove_plugin_contribution_generation(&plugin, load);
                            self.show_error(error);
                        }
                    }
                    self.notices.record(
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
                    self.remove_plugin_contribution_generation(&plugin, load);
                    self.commands.unregister_plugin(&plugin);
                    self.notices.record(
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
        self.retire_plugin_edit_sources();
    }
}
