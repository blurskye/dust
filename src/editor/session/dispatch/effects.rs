use super::*;
use crate::editor::session::extensions::{VirtualContext, VirtualOpen, VirtualPublication};

#[path = "../effects/configuration.rs"]
mod configuration;
#[path = "../effects/core.rs"]
mod core;
pub(super) use core::{ExecutionAdmission, ExecutionAuthorization};
#[path = "../effects/debugger.rs"]
mod debugger;
#[path = "../effects/extensions.rs"]
mod extensions;
#[path = "../effects/language.rs"]
mod language;
#[path = "../effects/misc.rs"]
mod misc;
#[path = "../effects/project.rs"]
mod project;
#[path = "../effects/terminal.rs"]
mod terminal;

impl InteractiveApplication {
    pub(super) fn process_effect(&mut self, effect: AppEffect) {
        let effect = match self.process_core_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_configuration_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_terminal_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_extensions_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_project_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_language_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        let effect = match self.process_debugger_effect(effect) {
            Ok(()) => return,
            Err(effect) => effect,
        };
        if self.process_misc_effect(effect).is_err() {
            unreachable!("every AppEffect variant has one owning handler");
        }
    }
}

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
            if let Err(error) = self.extension.index_from_store(
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

    pub(in crate::editor::session) fn handle_plugin_worker_stopped(
        &mut self,
        error: crate::extensions::PluginError,
    ) {
        let Some(unregister) = self.extension.worker_stopped() else {
            return;
        };
        for plugin in unregister {
            self.commands.unregister_plugin(&plugin);
        }
        let owned_generations = self
            .extension
            .contributions()
            .ordered()
            .map(|index| (index.owner.clone(), PluginLoadId(index.generation)))
            .collect::<Vec<_>>();
        for (plugin, load) in owned_generations {
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        self.extension.clear_contributions();
        if let Ok(candidate) = self.configuration.prepare_base_bindings(
            &self.commands,
            self.extension.contributions(),
            true,
        ) && let Err(error) = self.commit_contribution_candidate(candidate)
        {
            self.show_error(error);
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
            .prepare_plugin_contributions(&self.commands, self.extension.contributions())?;
        self.commit_contribution_candidate(candidate)
    }

    pub(in crate::editor::session) fn commit_contribution_candidate(
        &mut self,
        candidate: crate::editor::session::configuration::ContributionCandidate,
    ) -> Result<(), String> {
        // Both owners are checked before publication. There is no callback, wait,
        // or configuration mutation between this check and the two accepts.
        self.configuration
            .check_contributions(&candidate, self.extension.contributions())?;
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
            .commit_contributions(candidate, self.extension.contributions())?;
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
        self.extension.remove_generation(plugin, load);
        let terminals = self.terminal_session.descriptions();
        let title = self.message("explorer_title", &[]);
        let descriptions = NativeDescriptions {
            signature: self.native_surface_signature(self.window_session.editor_ids()),
            explorer_title: &title,
            dashboard_title: &self.configuration.dashboard().identity,
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title),
            terminals: &terminals,
        };
        let result = self
            .extension
            .retire_virtual(
                plugin,
                load,
                VirtualContext {
                    editor: &self.state,
                    windows: &self.window_session,
                    scratch: &self.configuration.settings().ui.buffer_line.scratch_label,
                },
                descriptions,
            )
            .and_then(|staged| {
                staged
                    .map(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    })
                    .transpose()
            });
        match result {
            Ok(Some(state)) => {
                self.state = state;
                if self.state.active_view.is_none() {
                    self.set_dashboard_visibility(true);
                }
                self.reconcile_editor_windows();
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        if self.extension.retire_provider_generation(plugin, load) {
            self.close_picker();
        }

        if self.extension.remove_contribution(plugin, load) {
            self.commands.unregister_plugin(plugin);
            if let Err(error) = self.recompile_plugin_contributions() {
                match self
                    .configuration
                    .prepare_builtin_contributions(
                        &self.commands,
                        self.extension.contributions(),
                        true,
                    )
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
        for (owner, load) in self.extension.abort_indexes() {
            self.remove_plugin_contribution_generation(&owner, load);
            self.commands.unregister_plugin(&owner);
        }
        let candidate = self
            .configuration
            .prepare_builtin_contributions(
                &self.commands,
                self.extension.contributions(),
                self.configuration.deferred_plugin_keymaps(),
            )
            .unwrap_or_else(|_| {
                self.configuration
                    .prepare_builtin_rules(self.extension.contributions())
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
        self.extension
            .reject_expected(expected.into_iter(), (&failed_plugin, failed_load));
        self.abort_pending_index_batch(diagnostic);
    }
}

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

impl InteractiveApplication {
    pub(in crate::editor::session) fn apply_plugin_ui_request(
        &mut self,
        index: &crate::extensions::PluginContributionIndex,
        request: crate::extensions::PluginUiRequest,
    ) -> crate::extensions::PluginUiEvent {
        let Some(index) = self
            .extension
            .contributions()
            .get(&index.owner)
            .filter(|current| {
                current.generation == index.generation && current.checksum == index.checksum
            })
        else {
            return PluginWindowHost::rejected(
                request.token,
                self.window_session.graph().revision(),
                crate::extensions::PluginUiErrorCode::GenerationLost,
                "plugin UI request belongs to a retired catalog load".into(),
            );
        };
        if let Err(detail) = self
            .window_session
            .require_editor_source(&self.state.window_version())
        {
            return PluginWindowHost::rejected(
                request.token,
                self.window_session.graph().revision(),
                crate::extensions::PluginUiErrorCode::StaleGraph,
                detail,
            );
        }
        if PluginWindowHost::is_retained_update(&request.kind) {
            return self.plugin_windows.process_retained(
                self.window_session.graph(),
                index,
                request,
            );
        }
        if let Some(event) =
            self.plugin_windows
                .admit_window_request(self.window_session.graph(), index, &request)
        {
            return event;
        }
        let catalog = self.extension.contributions().version();
        let mut draft = match self.window_session.begin_plugin(
            &index.owner,
            index.generation,
            (&self.state.window_version(), &catalog),
        ) {
            Ok(draft) => draft,
            Err(detail) => {
                return PluginWindowHost::rejected(
                    request.token,
                    self.window_session.graph().revision(),
                    crate::extensions::PluginUiErrorCode::StaleGraph,
                    detail,
                );
            }
        };
        let mut host = self.plugin_windows.clone();
        let mut event = host.process(&mut draft, index, request);
        if event.error.is_none() {
            match self.window_session.commit_plugin(
                draft,
                &self.state.window_version(),
                &self.extension.contributions().version(),
            ) {
                Ok(()) => self.plugin_windows = host,
                Err(error) => {
                    event.handle = None;
                    event.tree_revision = None;
                    event.surface_revision = None;
                    event.error = Some(crate::extensions::PluginUiError {
                        code: crate::extensions::PluginUiErrorCode::StaleGraph,
                        detail: error,
                    });
                }
            }
        }
        event.graph_revision = self.window_session.graph().revision();
        event
    }

    pub(in crate::editor::session) fn apply_plugin_execution(
        &mut self,
        reply: crate::extensions::PluginExecutionReply,
    ) {
        let source = match self.extension.receive(
            reply,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(source) => source,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let mut execution = source.execution;
        let load = source.load;
        let plugin = execution.plugin.clone();
        let mut provider_continuations = Vec::new();
        if self.extension.is_active_generation(&plugin, load) {
            self.extension
                .acknowledge(&plugin, load, &execution.host_event_acknowledgements);
            if let Some(policy) = self
                .extension
                .contributions()
                .get(&plugin)
                .filter(|index| index.generation == load.0)
                .and_then(|index| index.storage.clone())
            {
                self.extension.request_storage(
                    &plugin,
                    load,
                    &policy,
                    std::mem::take(&mut execution.storage_requests),
                );
            }
            let workspace_revision = Some(self.workspace.root_revision());
            let buffer_revision = self
                .state
                .active_buffer_id()
                .and_then(|buffer| self.state.buffers.get(buffer))
                .map(|buffer| buffer.text.revision().0);
            for proposal in std::mem::take(&mut execution.virtual_document_proposals) {
                let result = self
                    .extension
                    .prepare_virtual(
                        &plugin,
                        load,
                        proposal,
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
            for chunk in std::mem::take(&mut execution.provider_chunks) {
                match self.extension.provider_chunk(
                    &plugin,
                    load,
                    (workspace_revision, buffer_revision),
                    chunk,
                ) {
                    Ok(update) => {
                        if let Some(request) = update.refresh {
                            self.present_plugin_picker(request);
                        }
                        if let Some(command) = update.command {
                            provider_continuations.push(command);
                        }
                    }
                    Err(error) => self.show_error(error.detail),
                }
            }
        }
        for command in provider_continuations {
            self.process_effect(AppEffect::PluginExecute {
                plugin: plugin.clone(),
                command,
            });
        }
        if !execution.ui_requests.is_empty()
            && self.extension.is_active_generation(&plugin, load)
            && let Some(index) = self
                .extension
                .contributions()
                .get(&plugin)
                .filter(|index| index.generation == load.0)
                .cloned()
        {
            let requests = std::mem::take(&mut execution.ui_requests);
            let events = requests
                .into_iter()
                .map(|request| self.apply_plugin_ui_request(&index, request))
                .collect::<Vec<_>>();
            self.extension.queue_ui(plugin.clone(), load, events);
            self.ui.plugin_surfaces = self.plugin_windows.snapshots();
            self.window_session.invalidate_native();
            self.reconcile_editor_windows();
        }
        for message in execution.messages {
            self.notify_message(message, false);
        }
        match source.preview {
            Ok(Some(key)) => self.begin_plugin_workspace_proposal(
                key,
                format!("plugin {}: {}", execution.plugin, execution.command),
            ),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        self.notices.record(
            LogLevel::Info,
            "plugins",
            "executed",
            BTreeMap::from([
                ("plugin".into(), execution.plugin),
                ("command".into(), execution.command),
                ("generation".into(), execution.generation.to_string()),
            ]),
        );
    }
    pub(in crate::editor::session) fn retire_plugin_edit_sources(&mut self) {
        let (retired, key) = self.extension.retire_edits(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        if retired != 0 {
            self.show_error(format!(
                "{retired} plugin invocations retired before their results were accepted"
            ));
        }
        if let Some(key) = key {
            if self
                .workspace_edits
                .origin()
                .is_some_and(|origin| origin.same(&key))
            {
                self.abort_workspace_edit("plugin proposal source retired".into());
            } else {
                let _ = self.extension.finish_edit(&key);
            }
        }
    }
    fn begin_plugin_workspace_proposal(
        &mut self,
        key: crate::editor::session::workspace_edits::OriginKey,
        label: String,
    ) {
        let result = (|| {
            self.workspace_edits.check_begin()?;
            let prepared = self.extension.prepare_edit(
                &key,
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            let path = prepared.draft.canonical_path.clone();
            let ticket = self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                prepared.sources,
            )?;
            let maximum = usize::try_from(
                self.configuration
                    .settings()
                    .workspace
                    .search
                    .maximum_file_bytes,
            )
            .unwrap_or(usize::MAX);
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect();
            self.workspace_edits.start_files(
                ticket,
                label,
                Some(key.clone()),
                false,
                vec![crate::editor::session::workspace_edits::InputFile::Read(
                    path.clone(),
                )],
                roots,
                maximum,
            )?;
            let snapshot = self
                .state
                .workspace_snapshot(
                    self.workspace_edits
                        .preparing_sources()
                        .ok_or("plugin workspace preparation missing")?,
                    &path,
                    maximum,
                )?
                .ok_or("plugin snapshot document closed")?;
            self.workspace_edits.open_snapshot(0, snapshot)?;
            self.workspace_edits.finish_preparation(
                vec![prepared.draft],
                self.workspace.edit_scope(),
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        match result {
            Ok(()) => self.show_workspace_proposal(),
            Err(error) => {
                if self
                    .workspace_edits
                    .origin()
                    .is_some_and(|origin| origin.same(&key))
                {
                    self.abort_workspace_edit(error);
                } else {
                    let _ = self.extension.finish_edit(&key);
                    self.show_error(error);
                }
            }
        }
    }
}

impl InteractiveApplication {
    fn accept_virtual_publication(
        windows: &mut super::super::windows::WindowSession,
        (source, transition): (
            VirtualPublication<'_>,
            super::super::windows::EditorWindowCandidate,
        ),
    ) -> Result<EditorState, String> {
        let accepted =
            windows.accept_editor_transition(transition, source.before(), source.catalog())?;
        source.finish(accepted)
    }
    fn route_virtual_open(&mut self, result: Result<VirtualOpen, String>) {
        match result {
            Ok(VirtualOpen::Execute { plugin, command }) => {
                self.process_effect(AppEffect::PluginExecute { plugin, command })
            }
            Ok(VirtualOpen::Existing(buffer)) => {
                let mut draft = self.state.draft();
                if let Err(error) = draft
                    .show_document(buffer)
                    .map_err(str::to_owned)
                    .and_then(|_| self.commit_editor_draft(draft, None, false))
                {
                    self.show_error(error);
                } else {
                    self.set_dashboard_visibility(false);
                    self.reconcile_editor_windows();
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}
