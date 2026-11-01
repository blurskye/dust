use super::super::configuration::{configured_grapheme_width, configured_text_width};
use super::{
    AdapterContext, AppEffect, ArgumentSchema, BTreeMap, BTreeSet, ByteOffset, ByteRange,
    CommandId, CommandInvocation, CommandOutcome, CommandSource, CommandSpec,
    ContextRequirement, DapControl, DapEvent, DapResponse, DapSourceBreakpoint, TrustedAction,
    DiagnosticFilter, DiagnosticOwner, DisplayWidthPolicy, DockEdge, DocumentationPopupKind,
    EditorCloseSave, EditorCloseStep, EditorCloseTarget, EditorMode, EditorOperation, EditorState,
    EditorTransaction, ExecutionPreflightEvent, ExecutionSource, ExplorerKind, ExplorerMutation,
    FileSnapshot, FocusDirection, HashSet, InputAction, Instant, InteractiveApplication,
    LargeFileOverride, LayoutAction, LayoutPlanError, LogLevel,
    LspAutostartObservation, LspDocument, LspFeature, LspLaunchRequest, LspLaunchSource,
    LspMarkupContent, LspMarkupKind, LspRuntimeSource, LspWorkspaceEdit, MatcherSource,
    NativeDescriptions, NativeSurfaceTarget, PackageOperation, Path,
    PathBuf, PendingLspProjectRoot, PersistedLargeFileOverride, PersistedLocation,
    PersistedSession, PersistedState, PersistedWindowRule, PersistencePolicy, PickerAction,
    PickerContent, PickerItem, PickerItemId, PickerKind, PickerOrigin, PickerPayload, PickerState,
    PickerSurfaceKind, PluginEvent, PluginLimits, PluginLoadId, PluginUiInteraction, PositionCodec,
    PositionEncoding, Precondition, PromptKind, PromptMove, RegistryRequest, RegistryResult,
    RestoreContext, RestorePrepared, RestoreReady, RestoreWindowPolicy, RunningTask, STATE_VERSION,
    SpatialDirection, TerminalInput, TerminalUpdate, TextBuffer, TextEdit, ToolDefinition,
    ToolInstall, ToolKind, ToolMenuRow, ToolMenuState, ToolOperation, ToolRuntimeOption,
    ToolRuntimePickerState, ToolStatus, TransactionId, UndoPolicy, VisualRowAnchor, VisualRowModel,
    WindowCandidate, WindowPlacement, WindowRect, WindowUpdate, WorkspaceEditDraft,
    WorkspaceFilePolicy, WorkspaceSearchMatch, WorkspaceSearchPolicy, WorkspaceTrust,
    active_editor_pane_viewport, builtin_languages, discover_executable, editor_pane_viewport_at,
    editor_state_content_width, editor_state_gutter_width, file_uri, is_nixos_host,
    language_id_for_path, lsp_runtime_source_allowed_on_host,
    managed_formatter, managed_tool_executable, mode_name_large,
    nix_lsp_launch_environment, nix_lsp_profile_installed,
    nix_workspace_shell_for_host, parse_ex, path_from_file_uri, tool_kind_key,
};
use crate::editor::session::extensions::{VirtualContext, VirtualOpen, VirtualPublication};
use crate::editor::session::git::GitHunkAction;
use crate::workspace::search_workflow::{
    ABS_MAX_BYTES, SearchMatcherKind, SearchPresenterKind, SearchRequestId, SearchSourceKind,
    SearchStreamGeneration, SearchWorkflowBroker, SearchWorkflowChunk, SearchWorkflowError,
    SearchWorkflowId, SearchWorkflowSpec, picker_item_adapter, plugin_item_adapter,
};
use super::commands::SyntaxFoldAction;
use crate::ui::{CompletionItem, LanguageCompletionState};
use slotmap::Key;
use unicode_segmentation::UnicodeSegmentation;

fn compose_workspace_file_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, SearchWorkflowError> {
    let mut broker = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Files,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    })?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: items
            .iter()
            .map(|(item, _)| picker_item_adapter(item))
            .collect(),
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut map = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect())
}

fn compose_plugin_picker_items(
    projection: &crate::extensions::PluginPickerProjection,
) -> Result<Vec<(PickerItem, PickerPayload)>, SearchWorkflowError> {
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(projection.request),
        source: SearchSourceKind::PluginProvider {
            provider: projection.provider.clone(),
        },
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: projection.items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(
        SearchRequestId(projection.request),
        SearchStreamGeneration(projection.revision),
        &projection.query,
    )?;
    let workflow = projection
        .items
        .iter()
        .map(|item| plugin_item_adapter(&item.id, &item.primary, item.secondary.as_deref()))
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(projection.request),
        generation: SearchStreamGeneration(projection.revision),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let snapshot = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?;
    let mut source = projection
        .items
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    Ok(snapshot
        .items
        .into_iter()
        .filter_map(|ranked| {
            source.remove(&ranked.stable_key).map(|item| {
                let id = PickerItemId(format!(
                    "plugin-provider:{}:{}:{}",
                    projection.request, projection.revision, item.id
                ));
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: item.primary.clone(),
                        detail: item.secondary.clone().unwrap_or_default(),
                        source: projection.provider.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::PluginProviderItem {
                        plugin: projection.plugin.clone(),
                        load: projection.load,
                        request: projection.request,
                        item: item.id.clone(),
                        action_command: projection.action_command.clone(),
                    },
                )
            })
        })
        .collect())
}

pub(super) fn compose_command_picker_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, SearchWorkflowError> {
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Commands,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    let workflow = items
        .iter()
        .map(|(item, payload)| {
            let mut item = item.clone();
            if let PickerPayload::Command(command) = payload {
                item.action = PickerAction::InvokeCommand {
                    command: command.clone(),
                }
            }
            picker_item_adapter(&item)
        })
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut by_id = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect())
}

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
        ) {
            if let Err(error) = self.commit_contribution_candidate(candidate) {
                self.show_error(error);
            }
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
        let (rules, catalog) = self.extension.prepare_window_rules(&candidate)?;
        self.window_session
            .accept_current(windows, &self.state.window_version())?;
        let receipt = self
            .configuration
            .commit_contributions(candidate, catalog)?;
        rules.finish(receipt)?;
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
            .map(|()| self.extension.remove_windows(plugin, load.0))
        {
            Ok(removed) => {
                if removed {
                    self.ui.plugin_surfaces = self.extension.window_snapshots();
                    self.window_session.invalidate_native();
                    self.reconcile_editor_windows();
                }
            }
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        let removed_active_confirmation = self.extension.remove_generation(plugin, load);
        if removed_active_confirmation
            && self
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.close_prompt();
        }
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
                *self.state = state;
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
        if self.extension.retire_stale_confirmations(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) && self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.close_prompt();
        }
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
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
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn apply_plugin_ui_request(
        &mut self,
        index: &crate::extensions::PluginContributionIndex,
        request: crate::extensions::PluginUiRequest,
    ) -> crate::extensions::PluginUiEvent {
        match self
            .extension
            .prepare_window(index, request, &self.state, &self.window_session)
        {
            WindowUpdate::Ready(event) => event,
            WindowUpdate::Publish(source, transition) => {
                let receipt = self.window_session.accept_plugin_transition(
                    *transition,
                    &self.state.window_version(),
                    source.catalog(),
                );
                source.finish(receipt, self.window_session.graph().revision())
            }
        }
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
        let confirmation_source = source.confirmation_source.clone();
        let confirmation_result = source.confirmation_result;
        let source_actions_admitted = source.source_actions_admitted;
        let mut execution = source.execution;
        let load = source.load;
        let confirmation_requests = std::mem::take(&mut execution.confirmation_requests);
        if let Err(error) = self
            .extension
            .admit_confirmations(confirmation_source, confirmation_requests)
        {
            self.show_error(error);
        }
        if !source_actions_admitted {
            execution.messages.clear();
            execution.ui_requests.clear();
            execution.host_event_acknowledgements.clear();
            execution.storage_requests.clear();
            execution.provider_chunks.clear();
            execution.virtual_document_proposals.clear();
        }
        let plugin = execution.plugin.clone();
        if confirmation_result {
            let disallowed_ui = execution.ui_requests.iter().any(|request| {
                !matches!(
                    request.kind,
                    crate::extensions::PluginUiRequestKind::ReplaceUi { .. }
                        | crate::extensions::PluginUiRequestKind::ReplacePrivate { .. }
                        | crate::extensions::PluginUiRequestKind::PatchPrivate { .. }
                )
            });
            if disallowed_ui
                || !execution.storage_requests.is_empty()
                || !execution.provider_chunks.is_empty()
                || !execution.virtual_document_proposals.is_empty()
            {
                execution.ui_requests.clear();
                execution.storage_requests.clear();
                execution.provider_chunks.clear();
                execution.virtual_document_proposals.clear();
                self.show_error(
                    "plugin confirmation result may update existing plugin UI only".to_owned(),
                );
            }
        }
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
                        *self.state = state;
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
        if !execution.ui_requests.is_empty() && self.extension.is_active_generation(&plugin, load) {
            if let Some(index) = self
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
                self.ui.plugin_surfaces = self.extension.window_snapshots();
                self.window_session.invalidate_native();
                self.reconcile_editor_windows();
            }
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
                (roots, maximum),
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

impl InteractiveApplication {
    pub(super) fn effective_large_file(&self, buffer: crate::editor::BufferId) -> bool {
        self.documents.effective_large_file(
            buffer,
            self.state.buffers.get(buffer).map(|buffer| &buffer.text),
            &self.configuration.settings().editor.large_file,
        )
    }
    pub(super) fn process_large_file_modes(&mut self) {
        let large = self.documents.reconcile_policy(
            &self.state.window_version(),
            self.state
                .buffers
                .iter()
                .map(|(id, buffer)| (id, buffer.path.as_deref(), &buffer.text)),
            &self.configuration.settings().editor.large_file,
        );
        self.project_large_file_modes(large);
    }

    pub(super) fn persistence_policy(&self) -> PersistencePolicy {
        PersistencePolicy {
            maximum_file_bytes: self.configuration.settings().persistence.maximum_file_bytes,
            maximum_entries: self.configuration.settings().persistence.maximum_entries,
        }
    }

    /// Asked for by the user, a saved session replaces what is open unless that would lose work.
    pub(super) fn load_session_on_request(&mut self) {
        if !self.configuration.settings().persistence.enabled {
            self.show_error(self.message("session_disabled", &[]));
            return;
        }
        if self.state.buffers.values().any(|buffer| buffer.dirty) {
            self.show_error(self.message("session_load_dirty", &[]));
            return;
        }
        let result = self.persistence.load_session(
            self.persistence_policy(),
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            true,
        );
        match result {
            Ok(()) => {
                self.session_load_requested = true;
                self.configuration.begin_theme_restore();
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn load_persistence(&mut self) {
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled || !configured.restore_on_startup {
            return;
        }
        if let Err(error) = self.persistence.load_session(
            self.persistence_policy(),
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            !self.documents.has_startup_work() && self.state.buffers.is_empty(),
        ) {
            self.ui.config_diagnostic = Some(error);
        } else {
            self.configuration.begin_theme_restore();
        }
    }

    pub(super) fn capture_window_rules(&self) -> Vec<PersistedWindowRule> {
        self.window_session
            .persisted_rules(self.terminal_session.selected().map(|id| id.0))
    }

    pub(super) fn prepare_configured_window_rules(
        &self,
        windows: &crate::config::WindowsConfig,
    ) -> Result<WindowCandidate, String> {
        let terminals = self.terminal_session.descriptions();
        self.window_session.prepare_rules(
            windows,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )
    }

    pub(super) fn apply_persisted_window_rules(
        &mut self,
        rules: &[PersistedWindowRule],
    ) -> Result<(), String> {
        let terminals = self.terminal_session.descriptions();
        let candidate = self.window_session.prepare_persisted_rules(
            rules,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )?;
        self.window_session
            .accept_current(candidate, &self.state.window_version())?;
        let requires_projection = self.window_session.graph().active_tab().is_some();
        self.reconcile_editor_windows();
        if requires_projection && self.compositor_projection().is_none() {
            return Err("persisted window projection could not be published".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn capture_session(&self) -> Option<PersistedSession> {
        self.try_capture_session().ok().flatten()
    }

    fn try_capture_session(&self) -> Result<Option<PersistedSession>, String> {
        self.persistence.capture_session(
            &self.state,
            &self.workspace,
            &self.window_session,
            self.capture_window_rules(),
            &self.extension.virtual_descriptor_sources(),
        )
    }

    pub(super) fn save_persistence(&mut self) {
        if self.configuration.settings().persistence.save_on_exit {
            self.write_persistence();
        }
    }

    fn write_persistence(&mut self) {
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled || self.persistence.pending() {
            return;
        }
        let mut metadata = self.tooling.metadata();
        match self.configuration.theme_state() {
            Ok(Some(theme)) => {
                metadata.insert("ui.theme.snapshot".into(), theme);
            }
            Ok(None) => {}
            Err(error) => {
                self.ui.config_diagnostic = Some(error);
                return;
            }
        }
        let session = match self.try_capture_session() {
            Ok(session) => session,
            Err(error) => {
                self.ui.config_diagnostic = Some(error);
                return;
            }
        };
        let state = PersistedState {
            version: STATE_VERSION,
            session,
            recent_files: self.persistence.recent_files().to_vec(),
            marks: self.persistence.marks().to_vec(),
            jumps: self.persistence.jumps().to_vec(),
            changes: self.persistence.changes().to_vec(),
            command_history: if configured.persist_command_history {
                self.input.command_history().to_vec()
            } else {
                Vec::new()
            },
            search_history: if configured.persist_search_history {
                self.input.search_history().to_vec()
            } else {
                Vec::new()
            },
            breakpoints: self.debugger_session.persisted_breakpoints(),
            large_file_overrides: self
                .documents
                .policy_overrides()
                .filter_map(|(id, mode)| {
                    let path = self.state.buffers.get(id)?.path.clone()?;
                    Some(PersistedLargeFileOverride {
                        path,
                        mode: mode_name_large(mode).into(),
                    })
                })
                .take(configured.maximum_entries)
                .collect(),
            watches: self.debugger_session.persisted_watches(),
            metadata,
        };
        if let Err(error) = self.persistence.save(state, self.persistence_policy()) {
            self.ui.config_diagnostic = Some(error);
        }
    }

    pub(super) fn process_persistence_events(&mut self) {
        self.read_persistence_events();
        // A project with no saved session file finishes loading without any event.
        if self.session_load_requested && !self.persistence.pending() {
            self.session_load_requested = false;
            self.notify_message(self.message("session_load_none", &[]), false);
        }
    }

    fn read_persistence_events(&mut self) {
        while let Some(event) = self.persistence.poll() {
            match event {
                Ok(mut state) => {
                    let session = state.session.clone();
                    self.tooling.restore(&state.metadata);
                    if let Err(error) = self
                        .configuration
                        .restore_theme(state.metadata.get("ui.theme.snapshot").map(String::as_str))
                    {
                        self.ui.config_diagnostic = Some(error);
                    }
                    if let Some(session) = &session {
                        self.report_unavailable_persisted_content(session);
                    }
                    let configured = &self.configuration.settings().persistence;
                    self.persistence.restore_history(&mut state, configured);
                    self.debugger_session.restore(
                        state.breakpoints,
                        state.watches,
                        &self.workspace,
                    );
                    self.documents.restore_policy(
                        state
                            .large_file_overrides
                            .into_iter()
                            .map(|entry| (entry.path, entry.mode)),
                    );
                    self.input.restore_history(
                        state.command_history,
                        state.search_history,
                        configured,
                    );
                    let requested = std::mem::take(&mut self.session_load_requested);
                    let found = session.is_some();
                    if let Some(session) = session {
                        self.restore_session(session);
                    }
                    if requested {
                        let message = if found { "session_restored" } else { "session_load_none" };
                        self.notify_message(self.message(message, &[]), false);
                    }
                }
                Err(error) => {
                    self.session_load_requested = false;
                    self.ui.config_diagnostic = Some(error);
                }
            }
        }
        if self.persistence.take_save_request() {
            self.save_persistence();
        }
    }

    pub(super) fn report_unavailable_persisted_content(&mut self, session: &PersistedSession) {
        // ponytail: production 0.2 has no source-owned restore callback; report
        // unavailable descriptors rather than inventing host-side provider authority.
        let unavailable = session.unavailable_virtual_descriptors();
        if !unavailable.is_empty() {
            self.ui.config_diagnostic = Some(format!(
                "persistent virtual content unavailable under production API 0.2; continuing without: {}",
                unavailable.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    pub(super) fn restore_session(&mut self, session: PersistedSession) {
        let empty_rules = session.window_rules.clone();
        let paths = match self.persistence.begin_restore(
            session,
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            self.configuration.settings().persistence.maximum_entries,
        ) {
            Ok(Some(paths)) => paths,
            Ok(None) => return,
            Err(error) => {
                self.ui.config_diagnostic = Some(error);
                return;
            }
        };
        if paths.is_empty() {
            if let Err(error) = self.apply_persisted_window_rules(&empty_rules) {
                self.ui.config_diagnostic = Some(format!(
                    "persistent window layout rejected transactionally: {error}"
                ));
            }
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        for path in paths {
            let Some(read) = self.persistence.restore_read(
                &path,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) else {
                self.cancel_session_restore();
                return;
            };
            match self.documents.read_scoped(path, roots.clone()) {
                Ok(id) => {
                    if let Err(error) = read.register(id) {
                        self.cancel_session_restore();
                        self.show_error(error);
                        return;
                    }
                }
                Err(error) => {
                    drop(read);
                    self.cancel_session_restore();
                    self.show_error(error);
                    return;
                }
            }
        }
    }
    pub(super) fn cancel_session_restore(&mut self) {
        for id in self.persistence.cancel_restore() {
            self.documents.cancel_request(id);
        }
    }
    pub(super) fn finish_session_restore(&mut self, ready: RestoreReady) {
        let result = ready
            .prepare(RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            })
            .and_then(
                |RestorePrepared {
                     draft,
                     paths,
                     window_rules,
                     content_descriptors,
                 }| {
                    let terminals = self.terminal_session.descriptions();
                    let descriptions = NativeDescriptions {
                        signature: self.native_surface_signature(self.window_session.editor_ids()),
                        explorer_title: &self.message("explorer_title", &[]),
                        dashboard_title: &self.configuration.dashboard().identity,
                        picker_title: self
                            .ui
                            .picker
                            .as_ref()
                            .map_or("Picker", |picker| &picker.title),
                        terminals: &terminals,
                    };
                    let current = self.state.version();
                    let catalog = self.extension.contributions().version();
                    let transition = self.window_session.prepare_restore_transition(
                        draft,
                        (&current, catalog.clone()),
                        &self.configuration.settings().ui.buffer_line.scratch_label,
                        descriptions,
                        &self.extension.window_generations(),
                        RestoreWindowPolicy {
                            rules: &window_rules,
                            descriptors: &content_descriptors,
                        },
                    )?;
                    let state = self
                        .window_session
                        .accept_editor_transition(transition, &current, &catalog)?;
                    Ok((state, paths))
                },
            );
        match result {
            Ok((state, paths)) => {
                *self.state = state;
                let retired_confirmation = self.extension.reset_runtime();
                if retired_confirmation
                    && self
                        .input
                        .prompt()
                        .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
                {
                    self.close_prompt();
                }
                self.ui.plugin_surfaces.clear();
                self.ui.private_surfaces.clear();
                self.set_dashboard_visibility(false);
                for path in paths {
                    self.record_recent_file(path);
                }
                self.ensure_cursor_visible();
                self.reconcile_editor_windows();
            }
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn record_recent_file(&mut self, path: PathBuf) {
        self.persistence
            .record_recent(path, self.configuration.settings().persistence.recent_files);
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_theme_results(&mut self) {
        if let Some(Err(error)) = self.configuration.poll_theme_restore() {
            self.show_error(error);
        }
        self.retire_stale_picker();
        if let Some(rows) = self
            .configuration
            .poll_theme_picker(self.search.theme_picker_key())
        {
            let previous = self.search.picker_bookmark();
            self.present_theme_picker(rows);
            self.search.restore_picker_view(previous, false);
            self.sync_picker_ui();
        }
    }
    fn present_theme_picker(&mut self, rows: crate::editor::session::configuration::ThemeRows) {
        let items = rows
            .items
            .into_iter()
            .enumerate()
            .map(|(index, (key, name, origin))| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("theme:{index}")),
                        label: name,
                        detail: if index == 0 {
                            "Current".into()
                        } else {
                            String::new()
                        },
                        source: origin,
                        action: PickerAction::None,
                    },
                    PickerPayload::Theme(key),
                )
            })
            .collect();
        self.install_surface_picker_state(
            PickerSurfaceKind::Theme,
            "Dust themes".into(),
            items,
            String::new(),
            !rows.loading,
            PickerOrigin::General,
        );
        if !rows.diagnostics.is_empty() {
            self.show_error(rows.diagnostics.join("; "));
        }
    }
    pub(in crate::editor::session) fn process_configuration_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::SelectTheme => {
                self.close_picker();
                match self.configuration.begin_theme_picker() {
                    Ok(rows) => self.present_theme_picker(rows),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::ReloadConfiguration => match self.configuration.request_reload() {
                Ok(()) => {
                    self.notify_message(self.message("config_reload_queued", &[]), false);
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ExplainConfig(key) => {
                if let Some(explanation) = self.configuration.explain(&key) {
                    self.notify_message(
                        self.message(
                            "config_explanation",
                            &[
                                ("key", explanation.key),
                                ("value", explanation.value),
                                ("source", explanation.source),
                            ],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("unknown_config_key", &[("key", key)]));
                }
            }
            AppEffect::InspectKeymap {
                id,
                context,
                key,
                command,
            } => {
                let context = context.unwrap_or_else(|| self.active_input_context());
                let inspection = id
                    .as_deref()
                    .and_then(|id| self.configuration.keymaps().inspect_id(id))
                    .or_else(|| {
                        key.as_deref().and_then(|keys| {
                            self.configuration.keymaps().inspect_key(&context, keys)
                        })
                    })
                    .or_else(|| {
                        command.as_deref().and_then(|command| {
                            self.configuration.keymaps().inspect(&context, command)
                        })
                    });
                if let Some(inspection) = inspection {
                    self.notify_message(
                        self.message(
                            "keymap_inspection",
                            &[
                                ("id", inspection.id),
                                ("context", inspection.context),
                                ("source_keys", inspection.source_keys),
                                ("keys", inspection.keys),
                                ("operation", inspection.operation.as_str().into()),
                                ("command", inspection.command.unwrap_or_default()),
                                ("arguments", format!("{:?}", inspection.arguments)),
                                ("source", inspection.source),
                            ],
                        ),
                        false,
                    );
                } else if id.is_none() && command.is_none() && key.is_none() {
                    let count = self
                        .configuration
                        .keymaps()
                        .reverse
                        .keys()
                        .filter(|(mapped_context, _)| mapped_context == &context)
                        .count();
                    self.notify_message(
                        self.message(
                            "keymap_context_summary",
                            &[("context", context), ("count", count.to_string())],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_keymap_binding", &[("context", context)]));
                }
            }
            AppEffect::ShowKeymapConflicts => {
                let conflicts = self
                    .configuration
                    .rejected_keymaps()
                    .into_iter()
                    .chain(self.configuration.keymaps().conflicts.iter().map(String::as_str))
                    .collect::<Vec<_>>();
                if conflicts.is_empty() {
                    self.notify_message(self.message("no_keymap_conflicts", &[]), false);
                } else {
                    self.show_error(conflicts.join("; "));
                }
            }
            AppEffect::RecoveryInspect => {
                self.notify_message(
                    self.message(
                        "recovery_summary",
                        &[("count", self.recovery.records().len().to_string())],
                    ),
                    false,
                );
            }
            AppEffect::RecoveryRecoverLatest => {
                if let Some((_, record)) = self.recovery.records().last() {
                    self.state
                        .open_text(TextBuffer::from_text(&record.content), None);
                    self.set_dashboard_visibility(false);
                    self.notify_message(self.message("recovery_opened", &[]), false);
                } else {
                    self.show_error(self.message("recovery_none", &[]));
                }
            }
            AppEffect::RecoveryDiscardLatest => {
                match self.recovery.discard_latest(self.documents.recovery_io()) {
                    Ok(true) => {}
                    Ok(false) => self.show_error(self.message("recovery_none", &[])),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::SessionSave => {
                if self.configuration.settings().persistence.enabled {
                    self.write_persistence();
                    self.notify_message(self.message("session_saved", &[]), false);
                } else {
                    self.show_error(self.message("session_disabled", &[]));
                }
            }
            AppEffect::SessionLoad => self.load_session_on_request(),
            AppEffect::HistoryClear => {
                self.input.clear_history();
                self.notify_message(self.message("history_cleared", &[]), false);
            }
            AppEffect::LargeFileSet(mode) => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let mode = match mode.as_str() {
                    "auto" => LargeFileOverride::Auto,
                    "on" => LargeFileOverride::On,
                    "off" => LargeFileOverride::Off,
                    _ => {
                        self.show_error(self.message("large_file_invalid", &[]));
                        return Ok(());
                    }
                };
                self.documents.set_large_file_mode(
                    buffer,
                    self.state.buffers[buffer].path.as_deref(),
                    mode,
                );
                self.process_large_file_modes();
                self.notify_message(
                    self.message(
                        "large_file_changed",
                        &[("mode", mode_name_large(mode).into())],
                    ),
                    false,
                );
            }
            AppEffect::LargeFileStatus => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let configured = self.documents.configured_mode(buffer);
                let effective = if self.effective_large_file(buffer) {
                    "on"
                } else {
                    "off"
                };
                self.notify_message(
                    self.message(
                        "large_file_status",
                        &[
                            ("mode", mode_name_large(configured).into()),
                            ("effective", effective.into()),
                        ],
                    ),
                    false,
                );
            }
            AppEffect::SyntaxFold(action) => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    let folds = self.ui.syntax_folds.iter_mut().filter(|fold| fold.view == view);
                    match action {
                        SyntaxFoldAction::OpenAll => folds.for_each(|fold| fold.collapsed = false),
                        SyntaxFoldAction::CloseAll => folds.for_each(|fold| fold.collapsed = true),
                        SyntaxFoldAction::Toggle | SyntaxFoldAction::Open | SyntaxFoldAction::Close => {
                            // The innermost fold around the cursor, the one Vim's `za` acts on.
                            if let Some(fold) = folds
                                .filter(|fold| {
                                    line.is_some_and(|line| {
                                        line >= fold.start_line && line <= fold.end_line
                                    })
                                })
                                .max_by_key(|fold| fold.start_line)
                            {
                                fold.collapsed = match action {
                                    SyntaxFoldAction::Open => false,
                                    SyntaxFoldAction::Close => true,
                                    _ => !fold.collapsed,
                                };
                            }
                        }
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| {
                        crate::language::syntax::language_for_document(
                            buffer.path.as_deref(),
                            buffer.text.chunks().next().unwrap_or_default(),
                        )
                    });
                let items = builtin_languages()
                    .enumerate()
                    .map(|(index, info)| {
                        let state = if Some(info.language) == active_language {
                            if let Some(buffer) = active {
                                if let Some(error) = self.syntax.failure(buffer) {
                                    self.message(
                                        "syntax_state_failed",
                                        &[("error", error.to_owned())],
                                    )
                                } else if self.syntax.result(buffer).is_some_and(|result| {
                                    self.state.buffers.get(buffer).is_some_and(|state| {
                                        state.text.revision().0 == result.revision
                                    }) && result.language == info.language
                                }) {
                                    self.message(
                                        if self
                                            .syntax
                                            .result(buffer)
                                            .is_some_and(|result| result.truncated)
                                        {
                                            "syntax_state_active_truncated"
                                        } else {
                                            "syntax_state_active"
                                        },
                                        &[],
                                    )
                                } else {
                                    self.message("syntax_state_pending", &[])
                                }
                            } else {
                                self.message("syntax_state_available", &[])
                            }
                        } else {
                            self.message("syntax_state_available", &[])
                        };
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: info.language.name().to_owned(),
                                detail: self.message(
                                    "syntax_capabilities",
                                    &[
                                        (
                                            "highlights",
                                            self.syntax_support_label(info.capabilities.highlights),
                                        ),
                                        (
                                            "folds",
                                            self.syntax_support_label(info.capabilities.folds),
                                        ),
                                        (
                                            "indents",
                                            self.syntax_support_label(info.capabilities.indents),
                                        ),
                                        ("state", state),
                                    ],
                                ),
                                source: self.message("syntax_builtin_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("syntax_status_title", &[]), items);
            }
            AppEffect::TestRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                // Without a matcher there are no per-test rows, but the run and its exit code stand.
                match task.matcher.clone() {
                    Some(matcher) => self.run_trusted(TrustedAction::RunTest { name, matcher }),
                    None => self.run_trusted(TrustedAction::RunTask(name)),
                }
            }
            AppEffect::TestStop(name) => self.stop_task(&name),
            AppEffect::TestList(name) => self.show_test_picker(&name),
            AppEffect::TaskList => self.show_task_picker(),
            AppEffect::TaskRestart(name) => {
                self.stop_task(&name);
                self.run_trusted(TrustedAction::RunTask(name));
            }
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return Ok(());
                };
                self.run_trusted(TrustedAction::RunLinter { name, matcher });
            }
            AppEffect::LintClear(name) => {
                if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                    self.diagnostics.clear_owner(&owner);
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::FormatRun(name) => {
                let Some(view_id) = self.state.active_view else {
                    return Ok(());
                };
                let Some(view) = self.state.views.get(view_id) else {
                    return Ok(());
                };
                let buffer = view.buffer;
                match crate::editor::session::tasks::FormatterSource::capture(&self.state, buffer) {
                    Ok(source) => self
                        .run_trusted(TrustedAction::RunFormatter { name, source }),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::TaskRun(name) => {
                self.run_trusted(TrustedAction::RunTask(name))
            }
            AppEffect::TaskStop(name) => self.stop_task(&name),
            AppEffect::TaskOutput(name) => self.show_output_picker(&name),
            AppEffect::CrashInspect => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                self.notify_message(
                    self.message("crash_location", &[("path", path.display().to_string())]),
                    path.is_file(),
                );
            }
            AppEffect::CrashClear => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                if let Err(error) = self
                    .recovery
                    .clear_crash(path, self.documents.recovery_io())
                {
                    self.show_error(error);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_debugger_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        match effect {
            AppEffect::DapStart(program) => self.start_dap(None, program),
            AppEffect::DapConfigurationStart(name) => self.start_dap_configuration(&name),
            AppEffect::DapWatchAdd(expression) => self.debugger_session.add_watch(expression),
            AppEffect::DapWatchRemove(expression) => {
                self.debugger_session.remove_watch(&expression)
            }
            AppEffect::DapWatchList => self.open_dap_picker(
                "watches",
                self.debugger_session
                    .watches()
                    .iter()
                    .cloned()
                    .map(|expression| {
                        (
                            expression.clone(),
                            String::new(),
                            DapControl::Evaluate {
                                expression,
                                frame_id: self.debugger_session.current_frame(),
                                context: "watch".into(),
                            },
                        )
                    })
                    .collect(),
            ),
            AppEffect::DapStop => {
                if let Err(error) = self.debugger_session.stop() {
                    self.show_error(error);
                }
                self.close_debug_terminal();
            }
            AppEffect::DapContinue => {
                // With nothing to continue, start one, as F5 does in VS Code.
                match self.debugger_session.control(DapControl::Continue) {
                    Err(crate::debugger::DapError::NotRunning) => self.start_dap(None, None),
                    Err(error) => self.show_error(error),
                    Ok(_) => {}
                }
            }
            AppEffect::DapPause => {
                if let Err(error) = self.debugger_session.control(DapControl::Pause) {
                    self.show_error(error);
                }
            }
            AppEffect::DapNext => {
                if let Err(error) = self.debugger_session.control(DapControl::Next) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepIn => {
                if let Err(error) = self.debugger_session.control(DapControl::StepIn) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepOut => {
                if let Err(error) = self.debugger_session.control(DapControl::StepOut) {
                    self.show_error(error);
                }
            }
            AppEffect::DapConsole => {
                let items = self
                    .debugger_session
                    .console()
                    .enumerate()
                    .map(|(index, entry)| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("dap-console:{index}")),
                                label: entry.text.clone(),
                                // The ORIGIN and the source are both shown, so a REPL result is
                                // never read as program output.
                                detail: format!("{} · {}", entry.origin.label(), entry.source),
                                source: entry.origin.label().to_owned(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("dap_console_title", &[]), items);
            }
            AppEffect::DapSessions => {
                let adapter = self
                    .debugger_session
                    .active_adapter()
                    .unwrap_or("none")
                    .to_owned();
                let configuration = self
                    .debugger_session
                    .configuration()
                    .map(str::to_owned)
                    .unwrap_or_default();
                let items = vec![(
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId("dap-session:active".into()),
                        label: adapter,
                        detail: configuration,
                        source: if self.debugger_session.is_running() {
                            self.message("dap_session_running", &[])
                        } else {
                            self.message("dap_session_stopped", &[])
                        },
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )];
                self.install_picker(self.message("dap_sessions_title", &[]), items);
            }
            AppEffect::DapThreads => {
                if let Err(error) = self.debugger_session.control(DapControl::Threads) {
                    self.show_error(error)
                }
            }
            AppEffect::DapStack(thread) => {
                let Some(thread_id) = thread.or(self.debugger_session.stopped_thread()) else {
                    self.show_error(self.message("dap_not_paused", &[]));
                    return Ok(());
                };
                if let Err(error) = self.debugger_session.control(DapControl::StackTrace {
                    thread_id,
                    start: 0,
                    levels: 256,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapScopes(frame) | AppEffect::DapVariables(frame @ None) => {
                let Some(frame_id) = frame.or(self.debugger_session.current_frame()) else {
                    self.show_error(self.message("dap_not_paused", &[]));
                    return Ok(());
                };
                if let Err(error) = self.debugger_session.control(DapControl::Scopes { frame_id }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapVariables(Some(reference)) => {
                if let Err(error) = self.debugger_session.control(DapControl::Variables {
                    reference,
                    start: 0,
                    count: 1000,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapEvaluate(expression) => {
                // What the user typed is recorded as a COMMAND, distinct from what comes back.
                self.debugger_session
                    .record_console_command(expression.clone());
                let frame_id = self.debugger_session.current_frame();
                if let Err(error) = self.debugger_session.control(DapControl::Evaluate {
                    expression,
                    frame_id,
                    context: "repl".into(),
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapRestart => {
                if let Some(name) = self.debugger_session.configuration().map(str::to_owned) {
                    let _ = self.debugger_session.stop();
                    self.start_dap_configuration(&name)
                } else if let Some(target) = self.debugger_session.target().cloned() {
                    // A session started from the active file restarts what it was running.
                    let _ = self.debugger_session.stop();
                    self.start_dap(Some(target.adapter), target.program);
                } else {
                    self.show_error(self.message("dap_no_configuration", &[]));
                }
            }
            AppEffect::DapBreakpointConfigure {
                condition: None,
                hit_condition: None,
                log_message: None,
            } => {
                let current = self.cursor_breakpoint_place().and_then(|(path, line)| {
                    self.debugger_session
                        .persisted_breakpoints()
                        .into_iter()
                        .find(|row| row.path == path && row.line == line)
                        .and_then(|row| row.condition)
                });
                self.open_prompt(PromptKind::BreakpointCondition, current.unwrap_or_default());
            }
            AppEffect::DapBreakpointConfigure {
                condition,
                hit_condition,
                log_message,
            } => self.configure_breakpoint(condition, hit_condition, log_message),
            AppEffect::DapBreakpointToggle => {
                let Some(view) = self.state.active_view else {
                    return Ok(());
                };
                let buffer = &self.state.buffers[self.state.views[view].buffer];
                let Some(path) = buffer.path.clone() else {
                    return Ok(());
                };
                let Ok(line) = buffer
                    .text
                    .byte_to_line(self.state.views[view].cursor)
                    .map(|line| line as u64 + 1)
                else {
                    return Ok(());
                };
                match self.debugger_session.breakpoint(
                    DapSourceBreakpoint {
                        path,
                        line,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    },
                    true,
                ) {
                    // The sign in the gutter is the answer.
                    Ok(_) => self.project_debug_panes(),
                    Err(error) => self.show_error(error),
                }
            }

            AppEffect::DapInspectorNext => {
                // Narrow terminals replace panes with SELECTABLE inspectors, one at a time: this
                // is that selection. Without a session there are no panes to select.
                if let Some(panes) = self.ui.debug_panes.as_mut() {
                    let all = crate::ui::DebugPane::ALL;
                    let index = all
                        .iter()
                        .position(|pane| *pane == panes.selected)
                        .unwrap_or(0);
                    panes.selected = all[(index + 1) % all.len()];
                }
            }
            AppEffect::DapStatus => {
                let adapter = self
                    .debugger_session
                    .active_adapter()
                    .unwrap_or("none")
                    .to_owned();
                // An ATTACHED session names the TARGET it was given: the editor did not start that
                // process, so it does not describe it as a program it launched.
                let attached = self.debugger_session.target().and_then(|target| {
                    match target.request {
                        crate::config::DebugRequestKind::Attach => target.target.clone(),
                        crate::config::DebugRequestKind::Launch => None,
                    }
                });
                match attached {
                    Some(target) => self.notify_message(
                        self.message(
                            "dap_status_attached",
                            &[("adapter", adapter), ("target", target)],
                        ),
                        false,
                    ),
                    None => self.notify_message(
                        self.message("dap_status", &[("adapter", adapter)]),
                        false,
                    ),
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

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
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
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
                if !self.extension.statuses().contains_key(&id) {
                    self.show_error(self.message("plugin_unknown", &[("id", id)]));
                    return Ok(());
                }
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
                        *self.state = state;
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
                    self.run_trusted(TrustedAction::InstallPackage {
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
                    self.run_trusted(TrustedAction::InstallRegistry {
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

impl InteractiveApplication {
    pub(in crate::editor::session) fn show_lsp_logs(&mut self) {
        let previous = if self.search.picker_surface() == PickerSurfaceKind::Logs {
            self.search.picker_bookmark()
        } else {
            None
        };
        let normal = previous.is_some() && self.input.picker_normal();
        let follow_tail = previous.is_none() || self.search.picker_follows_tail();
        let items = self
            .language
            .logs()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("lsp-log:{index}")),
                        label: line.clone(),
                        detail: String::new(),
                        source: "lsp".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Logs,
            self.message("lsp_logs", &[("logs", String::new())]),
            items,
        );
        self.search.restore_picker_view(previous, follow_tail);
        self.input.set_picker_normal(normal);
        self.sync_picker_ui();
    }

    pub(in crate::editor::session) fn process_language_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::OpenToolMenu(category) => self.open_tool_menu(category),
            // Esc on a filtered list clears the search before it closes the manager.
            AppEffect::CloseToolMenu
                if self.ui.tool_menu.as_ref().is_some_and(|menu| !menu.query.is_empty()) =>
            {
                self.search_tool_menu(None, false);
            }
            AppEffect::CloseToolMenu => self.close_tool_menu(),
            AppEffect::ToolSearch => self.search_tool_menu(None, true),
            AppEffect::ToolSearchInsert(text) => {
                let query = self.ui.tool_menu.as_ref().map(|menu| format!("{}{text}", menu.query));
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchDelete => {
                let query = self.ui.tool_menu.as_ref().map(|menu| {
                    let mut query = menu.query.clone();
                    query.pop();
                    query
                });
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchStop { keep } => {
                let query = self.ui.tool_menu.as_ref().map(|menu| menu.query.clone());
                self.search_tool_menu(query.filter(|_| keep), false);
            }
            AppEffect::MoveToolMenu(direction) => self.move_tool_menu(direction),
            AppEffect::CycleToolCategory(direction) => self.cycle_tool_category(direction),
            AppEffect::RefreshTools => self.refresh_tools(),
            AppEffect::ActivateTool => self.activate_selected_tool(),
            AppEffect::InstallSelectedTool => self.install_selected_tool(),
            AppEffect::UninstallSelectedTool => self.uninstall_selected_tool(),
            AppEffect::StopSelectedLsp => self.stop_selected_lsp(),
            AppEffect::RestartSelectedLsp => self.restart_selected_lsp(),
            AppEffect::ToggleToolRuntime => self.toggle_selected_lsp_runtime(),
            AppEffect::LspStart => {
                if let Some(key) = self.language.active().cloned() {
                    self.language.request_start(&key, None);
                }
                self.start_lsp(None);
            }
            AppEffect::LspStop => {
                if let Some(server) = self.language.active().cloned() {
                    self.language.request_stop(&server);
                    if let Err(error) = self.language.stop(&server) {
                        self.show_error(error)
                    }
                    self.clear_lsp_progress(&server);
                    self.language.remove(&server);
                    self.clear_lsp_session_state();
                }
            }
            AppEffect::LspRestart => {
                if let Some(server) = self.language.active().cloned() {
                    self.language.request_start(&server, Some(Instant::now()));
                    let _ = self.language.stop(&server);
                    self.clear_lsp_progress(&server);
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspStatus => {
                let mut servers = self
                    .language
                    .running_servers()
                    .iter()
                    .map(|server| self.lsp_server_label(server))
                    .collect::<Vec<_>>();
                servers.sort();
                servers.dedup();
                self.notify_message(
                    self.message(
                        "lsp_status",
                        &[(
                            "servers",
                            if servers.is_empty() {
                                "none".into()
                            } else {
                                servers.join(", ")
                            },
                        )],
                    ),
                    false,
                );
                if let Some(server) = self.language.active().map(String::as_str) {
                    let _ = self.language.health(server);
                }
            }
            AppEffect::LspAttach => {
                if let Some(server) = self.language.active().cloned() {
                    if let Some((buffer, document)) = self.active_lsp_document() {
                        let source = self.state.buffers[buffer].text.source_version();
                        if let Err(error) =
                            self.language
                                .sync_owned(&server, document, (buffer, source))
                        {
                            self.show_error(error);
                        }
                    }
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspDetach => {
                if let (Some(server), Some((buffer, document))) =
                    (self.language.active().cloned(), self.active_lsp_document())
                {
                    let uri = document.uri();
                    let _ = self.language.close(&server, uri.clone());
                    self.language.detach(&server, buffer);
                    if let Ok(owner) = DiagnosticOwner::new(format!("lsp.{server}")) {
                        self.diagnostics.clear_owner_uri(&owner, &uri);
                    }
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::LspSessions => self.show_lsp_sessions(),
            AppEffect::LspSessionSelect(key) => self.select_lsp_session(&key),
            AppEffect::LspLogs => self.show_lsp_logs(),
            AppEffect::LspInlayHintsToggle => {
                let enabled = self.configuration.toggle_inlay_hints();
                self.schedule_lsp_inlay_refresh(true);
                self.notify_message(
                    format!(
                        "inlay hints {}",
                        if enabled { "enabled" } else { "disabled" }
                    ),
                    false,
                );
            }
            AppEffect::LspRequest(feature) => {
                if matches!(feature, LspFeature::Completion) {
                    self.mark_lsp_completion_start(false);
                }
                self.request_lsp(feature);
            }
            AppEffect::LspDocumentationScroll(direction) => {
                self.reconcile_editor_windows();
                let policy = &self.configuration.settings().ui.documentation_popup;
                self.input.scroll_documentation(
                    direction,
                    policy.scroll_step,
                    self.window_session.content().documentation_rows.max(1),
                );
                self.project_input_documentation();
            }
            AppEffect::LspDocumentationClose => self.close_documentation(),
            AppEffect::LspCompletionDocumentationOpen => self.open_lsp_completion_documentation(true),
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::DiagnosticNavigate { direction, filter } => {
                self.navigate_diagnostic(direction, filter)
            }
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspRenamePrompt => self.open_rename_prompt(),
            AppEffect::Substitute {
                range,
                pattern,
                replacement,
                flags,
            } => self.run_substitute(&range, &pattern, &replacement, &flags),
            AppEffect::LspCompletionLeave => {
                self.cancel_lsp_completion();
                self.invoke_mapped_command("input.cancel");
            }
            AppEffect::LspCompletionCancel => self.cancel_lsp_completion(),
            AppEffect::SnippetNavigate(direction) => self.navigate_snippet(direction),
            AppEffect::SnippetLeave => {
                self.input.clear_snippet();
                self.invoke_mapped_command("input.cancel");
            }
            AppEffect::SnippetCancel => {
                self.input.clear_snippet();
                if let Some(view) = self.state.active_view {
                    self.state.views[view].selection = None;
                    self.state.mark_editor_changed();
                }
            }
            AppEffect::LspFoldToggle => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    if let Some(fold) = self.ui.lsp_folds.iter_mut().find(|fold| {
                        fold.view == view
                            && line.is_some_and(|line| {
                                line >= fold.start_line && line <= fold.end_line
                            })
                    }) {
                        fold.collapsed = !fold.collapsed;
                    }
                }
            }
            AppEffect::LspSelectionExpand => {
                if let (Some(view), Some(range)) = (
                    self.state.active_view,
                    self.language
                        .selection_ranges()
                        .iter()
                        .filter(|range| {
                            range.contains(self.state.views[self.state.active_view.unwrap()].cursor)
                        })
                        .min_by_key(|range| range.len())
                        .copied(),
                ) {
                    let transaction = EditorTransaction::new(self.transaction_id()).with_operation(
                        EditorOperation::SetSelection {
                            view,
                            selection: Some(crate::editor::Selection {
                                anchor: range.start,
                                head: range.end,
                                kind: crate::editor::SelectionKind::Character,
                            }),
                        },
                    );
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error)
                    }
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_misc_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::MarkJump(name) => {
                let location = self
                    .persistence
                    .marks()
                    .iter()
                    .find(|mark| mark.name == name)
                    .map(|mark| mark.location.clone());
                match location {
                    Some(location) => self.navigate_persisted_location(location),
                    None => self.show_error(self.message("mark_unset", &[("name", name)])),
                }
            }
            AppEffect::MarkSet(name) => {
                if let Some(location) = self.current_persisted_location() {
                    self.persistence.set_mark(
                        name,
                        location,
                        self.configuration.settings().persistence.marks,
                    );
                }
            }

            AppEffect::SafeModeStatus => {
                self.notify_message(
                    self.message(
                        "safe_mode_status",
                        &[(
                            "state",
                            if self.workspace.safe_mode() {
                                "active".into()
                            } else {
                                "inactive".into()
                            },
                        )],
                    ),
                    false,
                );
            }
            AppEffect::ShowHealth => {
                self.notify_message(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.notices.terminal_capabilities().to_owned()),
                            ("jobs", self.tasks.running_count().to_string()),
                            ("ptys", self.terminal_session.running_count().to_string()),
                            ("preflights", self.pending_preflight_count().to_string()),
                            ("tasks", self.tasks.status_count().to_string()),
                            ("plugins", self.extension.statuses().len().to_string()),
                            (
                                "plugin_failures",
                                self.extension
                                    .statuses()
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::extensions::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.extension.pending_packages().to_string()),
                            ("lsp", self.language.sessions().len().to_string()),
                            ("persistence", self.persistence.pending().to_string()),
                            ("safe", self.workspace.safe_mode().to_string()),
                            ("dap", self.debugger_session.is_running().to_string()),
                        ],
                    ),
                    false,
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_project_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceTrust => self.request_workspace_trust(),
            AppEffect::WorkspaceUntrust => self.revoke_workspace_trust(),
            AppEffect::WorkspaceStatus => {
                let state = if self.workspace.trust() == WorkspaceTrust::Trusted {
                    self.message("workspace_trust_trusted", &[])
                } else {
                    self.message("workspace_trust_untrusted", &[])
                };
                self.notify_message(
                    self.message("workspace_trust_status", &[("state", state)]),
                    false,
                );
            }
            AppEffect::ExplorerPanelToggle => {
                return self.process_project_effect(if self.ui.explorer.is_some() {
                    AppEffect::ExplorerClose
                } else {
                    AppEffect::ExplorerOpen
                });
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerSearch => {
                self.explorer.start_search();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerSearchInsert(text) => {
                let query = format!("{}{text}", self.explorer.tree().filter());
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchDelete => {
                let mut query = self.explorer.tree().filter().to_owned();
                query.pop();
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchStop { keep } => {
                self.explorer.stop_search(keep);
                self.sync_explorer_ui();
            }
            // Esc on a filtered tree clears the filter before it closes the explorer.
            AppEffect::ExplorerClose if !self.explorer.tree().filter().is_empty() => {
                self.explorer.stop_search(false);
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerClose => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
                self.ensure_cursor_visible();
            }
            AppEffect::ExplorerMove(delta) => {
                self.move_explorer_selection(isize::from(delta));
            }
            AppEffect::ExplorerPage(direction) => self.page_explorer_selection(direction),
            AppEffect::ExplorerParent => self.explorer_collapse_or_parent(),
            AppEffect::ExplorerChild => {
                self.explorer.select_first_child();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerToggle => self.explorer_toggle(),
            AppEffect::ExplorerRefresh => self.refresh_explorer(),
            AppEffect::ExplorerFollow => self.follow_active_file_in_explorer(),
            AppEffect::ExplorerToggleHidden => {
                self.explorer.toggle_hidden();
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer.toggle_ignored();
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
            AppEffect::ExplorerCreate(path) => self.create_explorer_path(path),
            AppEffect::ExplorerRename(target) => self.rename_explorer_path(target),
            AppEffect::ExplorerRenamePrompt => self.open_explorer_rename_prompt(),
            AppEffect::ExplorerDeleteRequest => self.request_explorer_delete(),
            AppEffect::ExplorerClipboard { cut } => self.set_explorer_clipboard(cut),
            AppEffect::ExplorerPaste => self.paste_explorer_clipboard(),
            AppEffect::ExplorerYankPath => self.yank_explorer_path(),
            AppEffect::ExplorerMouseSelect => self.explorer_mouse_select(),
            AppEffect::WorkspaceRootAdd(path) => self.add_workspace_root(path),
            AppEffect::WorkspaceRootActivate(id) => {
                self.activate_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootRemove(id) => {
                self.remove_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootStatus => {
                if let Some(root) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.notify_message(
                        self.message(
                            "workspace_root_status",
                            &[
                                ("id", root.id.0.to_string()),
                                ("path", root.identity.canonical_path.display().to_string()),
                            ],
                        ),
                        false,
                    );
                }
            }
            AppEffect::WorkspaceSearchPrompt => self.open_live_workspace_search(),
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.search.cancel_grep();
                self.notify_message(self.message("workspace_search_cancelled", &[]), false);
            }
            AppEffect::WorkspaceReplace { query, replacement } => {
                self.start_workspace_search(query, Some(replacement));
            }
            AppEffect::OpenPicker(kind) => self.open_builtin_picker(kind),
            AppEffect::PickerInsert(text) => self.picker_insert(&text),
            AppEffect::PickerBackspace => self.picker_backspace(),
            AppEffect::PickerMove(direction) => self.picker_move(direction),
            AppEffect::PickerResults => {
                if self.search.set_picker_surface(PickerSurfaceKind::Problems) {
                    self.sync_picker_ui();
                }
            }
            AppEffect::PickerAccept => self.accept_picker(),
            AppEffect::PickerCancel => self.close_picker(),
            AppEffect::PickerNormal if self.palette_is_open() => self.close_picker(),
            AppEffect::PickerNormal | AppEffect::PickerInsertMode => {
                if self.search.picker().is_some() && self.search.workspace_picker().is_none() {
                    self.input
                        .set_picker_normal(matches!(effect, AppEffect::PickerNormal));
                    self.sync_picker_ui();
                }
            }
            AppEffect::WorkspaceEditToggle => {
                self.retire_stale_picker();
                let selected = self
                    .selected_picker_payload()
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected {
                    if self.workspace_edits.toggle(index).is_ok() {
                        self.show_workspace_proposal();
                    }
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => loop {
                let result = match self.workspace_edits.prepare_resource(
                    &self.workspace,
                    self.configuration.binding_revision_ref(),
                    &self.state,
                ) {
                    Ok(Some(permit)) => self.documents.submit_workspace_resource(permit),
                    Ok(None) => break,
                    Err(error) => {
                        self.show_error(error);
                        break;
                    }
                };
                if let Err(error) = result {
                    self.show_error(error);
                    break;
                }
            },
            effect => return Err(effect),
        }
        Ok(())
    }
}

use crate::editor::session::search::{Intent, SearchUpdate};
impl InteractiveApplication {
    pub(in crate::editor::session) fn open_live_workspace_search(&mut self) {
        self.close_picker();
        self.search.open_live();
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            self.message("workspace_search_title", &[]),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::Grep,
        );
    }
    pub(in crate::editor::session) fn restart_live_workspace_search(&mut self, query: String) {
        self.search.set_query(query.clone());
        if query.is_empty() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                Vec::new(),
                query,
                true,
                PickerOrigin::Grep,
            );
        } else {
            self.start_workspace_search(query, None);
        }
    }
    pub(in crate::editor::session) fn start_workspace_file_picker(&mut self) {
        self.close_picker();
        let configured = &self.configuration.settings().workspace.search;
        let root_revision = self.workspace.root_revision();
        let policy = WorkspaceFilePolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect(),
            root_revision,
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_path_bytes: 16 * 1024 * 1024,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.search.start_files(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                "Files".into(),
                Vec::new(),
                String::new(),
                false,
                PickerOrigin::Files,
            ),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn process_workspace_file_events(&mut self) {
        let Some(result) = self.search.poll_files(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.close_picker();
                self.show_error(error);
                return;
            }
        };
        let multi_root = self.workspace.roots().len() > 1;
        let items = entries
            .into_iter()
            .map(|entry| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(entry.id),
                        label: entry.relative_path.display().to_string(),
                        detail: String::new(),
                        source: if multi_root {
                            entry.root.file_name().map_or_else(String::new, |name| {
                                name.to_string_lossy().into_owned()
                            })
                        } else {
                            String::new()
                        },
                        action: PickerAction::None,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: entry.canonical_path,
                        line: 0,
                        column: 0,
                    }),
                )
            })
            .collect();
        match compose_workspace_file_items(items) {
            Ok(items) => self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                "Files".into(),
                items,
                String::new(),
                !self.search.files_pending(),
                PickerOrigin::Files,
            ),
            Err(error) => self.show_error(error.to_string()),
        }
    }
    /// `<leader>st`: every TODO-style comment keyword in the workspace, as todo-comments.nvim.
    fn start_todo_search(&mut self) {
        self.start_search(
            r"\b(TODO|FIXME|FIX|HACK|BUG|PERF|WARN|WARNING|NOTE|XXX)\b".to_owned(),
            None,
            true,
        );
    }

    pub(in crate::editor::session) fn start_workspace_search(
        &mut self,
        query: String,
        replacement: Option<String>,
    ) {
        let regex = self.configuration.settings().workspace.search.regex;
        self.start_search(query, replacement, regex);
    }

    fn start_search(&mut self, query: String, replacement: Option<String>, regex: bool) {
        let configured = &self.configuration.settings().workspace.search;
        let policy = WorkspaceSearchPolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            query,
            regex,
            smart_case: configured.smart_case,
            whole_word: configured.whole_word,
            include_globs: configured.include_globs.clone(),
            exclude_globs: configured.exclude_globs.clone(),
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_results: configured.maximum_results,
            maximum_file_bytes: configured.maximum_file_bytes,
            maximum_total_bytes: configured.maximum_total_bytes,
            maximum_preview_bytes: configured.maximum_preview_bytes,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.search.start(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
            replacement,
        ) {
            Ok(()) => {
                if self.search.live() {
                    self.install_surface_picker_state(
                        PickerSurfaceKind::Generic,
                        self.message("workspace_search_title", &[]),
                        Vec::new(),
                        self.search.query().into(),
                        false,
                        PickerOrigin::Grep,
                    );
                    self.clear_notice();
                } else {
                    self.close_picker();
                    self.notify_message(self.message("workspace_search_started", &[]), false);
                }
            }
            Err(error) => {
                self.show_error(self.message("workspace_search_failed", &[("diagnostic", error)]))
            }
        }
    }
    pub(in crate::editor::session) fn process_workspace_search_events(&mut self) {
        for update in self.search.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            match update {
                SearchUpdate::Batch => {
                    if self.search.live() {
                        self.open_workspace_search_picker();
                    }
                }
                SearchUpdate::Failed(error) => {
                    self.close_picker();
                    self.show_error(
                        self.message("workspace_search_failed", &[("diagnostic", error)]),
                    );
                }
                SearchUpdate::Complete {
                    intent: Intent::Find,
                    files,
                } => {
                    if self.search.live() || !self.search.matches().is_empty() {
                        self.open_workspace_search_picker();
                        if self.search.live() {
                            self.clear_notice();
                        } else {
                            self.notify_message(
                                self.message(
                                    "workspace_search_complete",
                                    &[
                                        ("count", self.search.matches().len().to_string()),
                                        ("files", files.to_string()),
                                    ],
                                ),
                                false,
                            );
                        }
                    } else {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    }
                }
                SearchUpdate::Complete {
                    intent: Intent::Replace(replacement),
                    ..
                } => {
                    if self.search.matches().is_empty() {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    } else {
                        self.begin_workspace_search_replacement(replacement);
                    }
                }
            }
        }
    }
    pub(in crate::editor::session) fn open_workspace_search_picker(&mut self) {
        let Some(order) = self
            .search
            .workflow()
            .and_then(|broker| broker.ordered_items())
        else {
            self.show_error("workspace search workflow projection is unavailable");
            return;
        };
        let matches = self
            .search
            .matches()
            .iter()
            .map(|found| (found.id.as_str(), found))
            .collect::<BTreeMap<_, _>>();
        let multi_root = self.workspace.roots().len() > 1;
        let items = order
            .into_iter()
            .filter_map(|item| matches.get(item.stable_key.as_str()).copied())
            .map(|found| {
                let item = PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(found.id.clone()),
                    label: self.message(
                        "workspace_search_row",
                        &[
                            ("path", found.relative_path.display().to_string()),
                            ("line", found.line.saturating_add(1).to_string()),
                            ("column", found.column.saturating_add(1).to_string()),
                        ],
                    ),
                    detail: found.line_text.trim().to_owned(),
                    source: if multi_root {
                        found
                            .root
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                    } else {
                        String::new()
                    },
                    action: PickerAction::None,
                };
                (item, PickerPayload::WorkspaceSearch(found.id.clone()))
            })
            .collect();
        if self.search.live() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                items,
                self.search.query().into(),
                !self.search.pending(),
                PickerOrigin::Grep,
            );
        } else {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                items,
                String::new(),
                true,
                PickerOrigin::Grep,
            );
        }
    }
    pub(in crate::editor::session) fn begin_workspace_search_replacement(
        &mut self,
        replacement: String,
    ) {
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let mut grouped = BTreeMap::<PathBuf, (Vec<u8>, Vec<TextEdit>)>::new();
        for found in self.search.matches() {
            let entry = grouped
                .entry(found.canonical_path.clone())
                .or_insert_with(|| (found.snapshot.to_vec(), Vec::new()));
            if entry.0.as_slice() != found.snapshot.as_ref() {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            }
            let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            else {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            };
            entry.1.push(TextEdit::new(range, replacement.clone()));
        }
        let mut snapshots = Vec::with_capacity(grouped.len());
        let mut drafts = Vec::with_capacity(grouped.len());
        for (path, (searched_bytes, edits)) in grouped {
            let revision = if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                if buffer.text.to_bytes() != searched_bytes {
                    self.show_error(self.message("workspace_search_stale_result", &[]));
                    return;
                }
                Some(buffer.text.revision())
            } else {
                None
            };
            snapshots.push(FileSnapshot {
                canonical_path: path.clone(),
                revision,
                bytes: searched_bytes,
            });
            drafts.push(WorkspaceEditDraft {
                canonical_path: path,
                expected_revision: revision,
                edits,
            });
        }
        let id = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(id) => id,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match self.workspace_edits.build(
            crate::editor::session::workspace_edits::PreparedEdits {
                ticket: id,
                label: self.message("workspace_replace_label", &[]),
                drafts,
                snapshots,
                resources: Vec::new(),
            },
            self.workspace.edit_scope(),
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => {
                self.show_workspace_proposal();
            }
            Err(error) => {
                self.workspace_edits.reject();
                self.show_error(self.message("workspace_edit_failed", &[("diagnostic", error)]));
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn explorer_open_file(&mut self, split: bool) {
        if self
            .explorer
            .tree()
            .selected_node()
            .is_some_and(|node| matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory))
        {
            self.explorer_toggle();
            return;
        }
        if let Err(error) = self.documents.check_admission() {
            self.show_error(error);
            return;
        }
        let request = self.explorer.prepare_open(
            split,
            (
                self.workspace.version(),
                self.configuration.binding_revision(),
            ),
            self.state.window_version(),
            self.workspace.roots(),
        );
        let (retired, request) = match request {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Some(id) = retired {
            self.documents.cancel_request(id);
        }
        match self.documents.open_explorer(request.path, request.roots) {
            Ok(id) => {
                self.set_explorer_focus(false);
                // Finish the requested focus change before capturing its async
                // navigation guard; a redraw must not look like new navigation.
                self.reconcile_editor_windows();
                let navigation = self
                    .window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view);
                if !self.explorer.admit_open(request.ticket, id, navigation) {
                    self.documents.cancel_request(id);
                }
            }
            Err(error) => {
                self.explorer.fail_open(request.ticket);
                self.show_error(error);
            }
        }
    }
    pub(in crate::editor::session) fn cancel_explorer_confirmation(&mut self) {
        if let Some(id) = self.explorer.cancel_confirmation() {
            self.documents.cancel_request(id);
        }
    }
    pub(in crate::editor::session) fn retire_stale_explorer_intents(&mut self) {
        let (ids, may_write) = self.explorer.retire_stale_intents(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        let mut all_cancelled = true;
        for id in ids {
            all_cancelled &= self.documents.cancel_request(id);
        }
        if may_write && !all_cancelled {
            self.notify_message(
                "explorer source changed; a running filesystem write may still finish".into(),
                true,
            );
        }
        if self
            .input
            .prompt()
            .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
            && !self.explorer.rename_current(
                &self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            )
        {
            self.close_prompt();
        }
        if !self.explorer.confirmation_pending()
            && self
                .input
                .prompt()
                .is_some_and(|p| p.kind == PromptKind::ExplorerDelete)
        {
            self.close_prompt();
        }
    }
    pub(in crate::editor::session) fn retire_explorer_intents(&mut self) {
        let (ids, may_write) = self.explorer.retire_intents();
        let mut all_cancelled = true;
        for id in ids {
            all_cancelled &= self.documents.cancel_request(id);
        }
        if may_write && !all_cancelled {
            self.notify_message(
                "explorer operation retired; a running filesystem write may still finish".into(),
                true,
            );
        }
    }
    fn queue_explorer_mutation(
        &mut self,
        operation: Result<ExplorerMutation, String>,
        confirmation: bool,
        rename_prompt: bool,
    ) -> Result<(), String> {
        let operation = operation?;
        if self.explorer.mutation_pending() {
            return Err("an explorer operation is already pending".into());
        }
        self.documents
            .check_admission()
            .map_err(|error| error.to_string())?;
        let change = match &operation {
            ExplorerMutation::Rename { source, .. } | ExplorerMutation::Delete { path: source } => {
                Some(self.state.capture_path_change(source)?)
            }
            _ => None,
        };
        let context = crate::editor::session::explorer::MutationContext {
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
            editor: self.state.window_version(),
            change,
        };
        let request = self.explorer.prepare_mutation(
            operation,
            context,
            self.workspace.roots(),
            self.configuration
                .settings()
                .workspace
                .search
                .maximum_file_bytes,
            confirmation,
        )?;
        let result = if rename_prompt {
            let source = self.explorer.submitted_rename_source();
            let target = request
                .operation
                .target()
                .expect("validated rename target")
                .into();
            self.documents.prepare_explorer_rename(source, target)
        } else {
            self.documents
                .prepare_explorer(request.operation, request.roots, request.maximum)
        }
        .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_mutation(request.ticket, result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            return Err(error);
        }
        self.notify_message("checking explorer operation…".into(), false);
        Ok(())
    }
    pub(in crate::editor::session) fn create_explorer_path(&mut self, input: String) {
        if let Err(error) = self.queue_explorer_mutation(self.explorer.create(&input), false, false)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn rename_explorer_path(&mut self, target: String) {
        if let Err(error) =
            self.queue_explorer_mutation(self.explorer.rename(&target), false, false)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn open_explorer_rename_prompt(&mut self) {
        self.close_prompt();
        let request = match self.explorer.begin_rename(
            self.workspace.version(),
            self.configuration.binding_revision(),
            self.state.window_version(),
            self.workspace.roots(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let result = self.explorer.rename_inspection().and_then(|inspection| {
            let id = self
                .documents
                .inspect_explorer_rename(
                    request.path,
                    request.roots,
                    self.configuration
                        .settings()
                        .workspace
                        .search
                        .maximum_file_bytes,
                )
                .map_err(|error| error.to_string())?;
            inspection.register(id);
            Ok(())
        });
        if let Err(error) = result {
            self.explorer.cancel_rename();
            self.show_error(error);
            return;
        }
        self.open_prompt(PromptKind::ExplorerRename, request.initial);
        self.prompt_help(format!(
            "Relative to `{}`. Checking source… Enter renames; Esc cancels.",
            request.root.display()
        ));
    }
    pub(in crate::editor::session) fn submit_explorer_rename_prompt(&mut self, value: &str) {
        let operation = self.explorer.rename_value(
            value,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        if let Err(error) = self.queue_explorer_mutation(operation, false, true) {
            self.prompt_error(error);
        } else {
            self.prompt_help("Checking rename target… Esc cancels before application.".into());
        }
    }
    pub(in crate::editor::session) fn cancel_explorer_rename_prompt(&mut self) {
        for id in self.explorer.cancel_rename() {
            self.documents.cancel_request(id);
        }
    }
    pub(in crate::editor::session) fn request_explorer_delete(&mut self) {
        self.close_prompt();
        if let Err(error) = self.queue_explorer_mutation(self.explorer.delete(), true, false) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn confirm_explorer_delete(&mut self) {
        match self.explorer.confirm_delete(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) {
            Ok(prepared) => self.apply_prepared_explorer(prepared),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn apply_prepared_explorer(
        &mut self,
        prepared: crate::files::PreparedExplorerMutation,
    ) {
        let result = self
            .documents
            .apply_explorer(prepared)
            .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_apply(result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn set_explorer_clipboard(&mut self, cut: bool) {
        match self.explorer.clipboard(cut, self.workspace.version()) {
            Ok(message) => self.notify_message(message, false),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn paste_explorer_clipboard(&mut self) {
        if let Err(error) = self.queue_explorer_mutation(
            self.explorer.paste(&self.workspace.version()),
            false,
            false,
        ) {
            self.show_error(error);
        }
    }
}

impl InteractiveApplication {
    /// Shows a terminal and gives it the keys, bringing its window back if it was hidden.
    fn reveal_terminal(&mut self, id: PtyId) {
        self.terminal_session.display(id);
        if self.terminal_window_rect().is_none() {
            if let Err(error) = self.show_native_window(NativeSurfaceTarget::Terminal(id.0)) {
                self.show_error(error);
                return;
            }
        }
        self.set_terminal_focus(true);
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
    }

    pub(in crate::editor::session) fn process_terminal_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::TerminalOpen(profile) => {
                let profile = profile.unwrap_or_else(|| {
                    self.configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone()
                });
                self.request_terminal_open(&profile, None);
            }
            AppEffect::TerminalToggle => {
                if self.window_session.terminal_focused() {
                    self.place_active_terminal(WindowPlacement::Hidden);
                } else if let Some(id) = self.terminal_session.selected() {
                    self.reveal_terminal(id);
                } else {
                    let profile = self
                        .configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone();
                    self.request_terminal_open(&profile, None);
                }
            }
            AppEffect::TerminalList => {
                let items = self
                    .terminal_session
                    .choices()
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, label, detail))| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("terminal:{index}")),
                                label,
                                detail,
                                source: "terminal".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Terminal(key),
                        )
                    })
                    .collect();
                self.install_picker("Terminals".into(), items);
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminal_session.select(id) {
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalFocusActive => {
                if self.terminal_session.selected().is_some() {
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalCycle(direction) => {
                let focused = self.window_session.terminal_focused();
                let hidden = self.terminal_window_rect().is_none();
                if !self.terminal_session.cycle(i64::from(direction)) {
                    self.show_error(self.message("terminal_missing", &[]));
                } else if let Some(id) = self.terminal_session.selected().filter(|_| focused || hidden) {
                    self.reveal_terminal(id);
                } else {
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                }
            }
            AppEffect::TerminalResize(rows) => {
                if let Some(id) = self.displayed_terminal() {
                    let result = if self.compositor_projection().is_none() {
                        Err("universal compositor projection is missing or stale".into())
                    } else {
                        self.window_session.resize_terminal(
                            id.0,
                            rows,
                            &self.configuration.settings().ui.terminal_panel,
                        )
                    };
                    if let Err(error) = result {
                        self.show_error(error);
                    }
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                    self.sync_explorer_ui();
                    self.ensure_cursor_visible();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalDock { edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("terminal dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                self.place_active_terminal(WindowPlacement::Docked { edge, size });
            }
            AppEffect::TerminalFloat(rect) => {
                let rect = rect.unwrap_or_else(|| {
                    let (columns, rows) = (self.window_session.width(), self.window_session.height());
                    let (width, height) = (columns * 4 / 5, rows * 4 / 5);
                    WindowRect { x: (columns - width) / 2, y: (rows - height) / 2, width, height }
                });
                self.place_active_terminal(WindowPlacement::Floating(rect));
            }
            AppEffect::TerminalFullscreen => {
                self.place_active_terminal(WindowPlacement::Fullscreen)
            }
            AppEffect::TerminalHide => self.place_active_terminal(WindowPlacement::Hidden),
            AppEffect::TerminalUnfocus => {
                self.set_terminal_focus(false);
                self.sync_terminal_panel();
                self.ensure_cursor_visible();
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.terminal_session.selected() {
                    if let Err(error) =
                        self.terminal_session
                            .send(id, TerminalInput::Text(&text), &self.workspace)
                    {
                        self.show_error(error);
                    }
                }
            }
            AppEffect::TerminalModeNormal => self.enter_terminal_normal_mode(),
            AppEffect::TerminalModeInput => self.enter_terminal_input_mode(),
            AppEffect::TerminalNormalMove(motion) => {
                self.move_terminal_normal_cursor(&motion);
            }
            AppEffect::TerminalVisualStart(linewise) => {
                self.start_terminal_visual(linewise);
            }
            AppEffect::TerminalYank { line } => self.yank_terminal_text(line),
            AppEffect::TerminalClose => {
                if let Some(id) = self.displayed_terminal() {
                    if self.terminal_session.stopping(id) {
                        self.show_error("terminal stop is already pending");
                    } else if self.terminal_session.requires_close_confirmation(id) {
                        if self.input.prompt().is_some() {
                            self.show_error(
                                "close the active prompt before closing a running terminal",
                            );
                        } else if self.terminal_session.begin_close(id) {
                            self.open_confirmation_actions(
                                PromptKind::TerminalClose,
                                format!("Terminal #{} is active", id.0),
                                "Hide this terminal or stop its process?".into(),
                                crate::ui::ConfirmationChoice::No,
                                "Stop",
                                "Hide",
                            );
                        }
                    } else {
                        self.terminal_session.cancel_open();
                        self.retire_terminal(id, true);
                    }
                } else {
                    self.terminal_session.cancel_open();
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl InteractiveApplication {
    pub(super) fn process_grammar_input(&mut self, input: crate::editor::input::GrammarInput) {
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        match self.input.feed(mode, input) {
            Ok(InputAction::Invoke(invocation)) => self.invoke_mapped(invocation),
            Ok(InputAction::Pending) => {
                if self.input.effective_mode(mode) == EditorMode::OperatorPending
                    && mode != EditorMode::OperatorPending
                {
                    self.set_mode(EditorMode::OperatorPending);
                }
            }
            Ok(InputAction::Cancelled) => {
                if mode == EditorMode::OperatorPending {
                    self.set_mode(EditorMode::Normal);
                }
            }
            Ok(InputAction::Text(text)) => {
                let context = self.active_input_context();
                if let Some(command_id) = self
                    .configuration
                    .settings()
                    .input
                    .text_commands
                    .get(&context)
                    .cloned()
                {
                    self.invoke_text_command(&command_id, text);
                }
            }
            Ok(InputAction::Unmapped) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<CommandInvocation> {
        let view = self
            .state
            .views
            .get(invocation.target.view.or(self.state.active_view)?)?;
        self.input.normalized_visual_repeat(
            invocation,
            &self.state.buffers.get(view.buffer)?.text,
            view.selection?,
            DisplayWidthPolicy {
                tab_width: self.state.policy.tab_width,
                ambiguous_wide: self.state.policy.ambiguous_width_wide,
            },
        )
    }

    pub(super) fn repeat_last_change(&mut self) {
        let limit = self.configuration.settings().runtime.repeat_step_limit;
        match self.input.repeat_plan(limit) {
            Ok(invocations) => {
                for invocation in invocations {
                    self.invoke(invocation);
                }
            }
            Err(super::super::input::ReplayError::NoChange) => {
                self.show_error(self.message("no_previous_change", &[]));
            }
            Err(_) => {
                self.show_error(self.message("repeat_step_limit", &[("limit", limit.to_string())]))
            }
        }
    }

    pub(super) fn replay_macro(&mut self, requested: char) {
        let Ok(register) = self.input.macro_register(requested) else {
            self.show_error(self.message("no_previous_macro", &[]));
            return;
        };
        let Some(invocations) = self.state.registers.get_macro(register).map(<[_]>::to_vec) else {
            self.show_error(self.message("empty_macro", &[("register", register.to_string())]));
            return;
        };
        if self
            .input
            .begin_macro_replay(
                register,
                self.configuration.settings().runtime.macro_recursion_limit,
            )
            .is_err()
        {
            self.show_error(
                self.message(
                    "macro_recursion_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings()
                            .runtime
                            .macro_recursion_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        for invocation in invocations {
            let Ok(invocation) = self.input.macro_step(
                invocation,
                self.configuration.settings().runtime.macro_step_limit,
            ) else {
                self.show_error(
                    self.message(
                        "macro_step_limit",
                        &[(
                            "limit",
                            self.configuration
                                .settings()
                                .runtime
                                .macro_step_limit
                                .to_string(),
                        )],
                    ),
                );
                break;
            };
            self.invoke(invocation);
        }
        self.input.finish_macro_replay();
    }

    pub(super) fn search_word_under_cursor(&mut self, forward: bool) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let source = self.state.buffers[view.buffer].text.to_string();
        let cursor = view.cursor.get();
        let Some(character) = source[cursor..].chars().next() else {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        };
        if !(character.is_alphanumeric() || character == '_') {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        }
        let start = source[..cursor]
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
            .last()
            .map_or(cursor, |(index, _)| index);
        let end = source[cursor..]
            .char_indices()
            .find(|(_, character)| !(character.is_alphanumeric() || *character == '_'))
            .map_or(source.len(), |(relative, _)| cursor + relative);
        self.ui.search_query = Some(source[start..end].to_owned());
        self.input.remember_search(forward);
        self.perform_search(forward);
    }

    pub(super) fn apply_authoritative_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
    ) -> Result<(), String> {
        self.commit_editor_transaction(transaction, None)
    }
    pub(super) fn apply_window_plan(
        &mut self,
        plan: super::super::windows::WindowPlan,
    ) -> Result<(), String> {
        self.window_session.check_plan(
            &plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
        )?;
        if plan.operations().is_empty() {
            self.window_session.accept_native_plan(
                plan,
                &self.state.window_version(),
                &self.extension.contributions().version(),
            )?;
            if let Some(id) = self
                .window_session
                .focused_terminal()
                .map(PtyId)
                .filter(|id| self.terminal_session.contains(*id))
            {
                self.terminal_session.select(id);
            }
            self.project_window_activation();
            self.reconcile_editor_windows();
            return Ok(());
        }
        let transaction = plan
            .operations()
            .iter()
            .cloned()
            .fold(
                EditorTransaction::new(self.transaction_id()),
                |transaction, operation| transaction.with_operation(operation),
            )
            .with_undo_policy(UndoPolicy::Ignore);
        self.commit_editor_transaction(&transaction, Some(plan))
    }
    fn commit_editor_transaction(
        &mut self,
        transaction: &EditorTransaction,
        plan: Option<super::super::windows::WindowPlan>,
    ) -> Result<(), String> {
        if let Some(plan) = &plan {
            self.window_session.check_plan(
                plan,
                &self.state.window_version(),
                &self.extension.contributions().version(),
            )?;
        }
        self.window_session
            .check_editor_admission(&(&*self.state).into())?;
        let candidate = self
            .state
            .prepare_transaction(transaction)
            .map_err(|error| error.to_string())?;
        self.commit_editor_draft(candidate, plan, false)
    }
    pub(super) fn commit_editor_draft(
        &mut self,
        candidate: crate::editor::EditorDraft,
        plan: Option<super::super::windows::WindowPlan>,
        activate_document: bool,
    ) -> Result<(), String> {
        self.window_session
            .check_editor_admission(&(&*self.state).into())?;
        let current = self.state.version();
        candidate.check_current(&current)?;
        if !activate_document
            && plan.is_none()
            && candidate.window_version() == self.state.window_version()
        {
            *self.state = candidate.accept(&current)?;
            return Ok(());
        }

        let terminals = self.terminal_session.descriptions();
        let mut signature = self.native_surface_signature(self.window_session.editor_ids());
        signature.dashboard_visible =
            self.window_session.dashboard_visible() || candidate.topology().active_tab().is_none();
        let title = self.message("explorer_title", &[]);
        let descriptions = NativeDescriptions {
            signature,
            explorer_title: &title,
            dashboard_title: &self.configuration.dashboard().identity,
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title),
            terminals: &terminals,
        };
        let catalog = self.extension.contributions().version();
        let transition = if let Some(plan) = plan {
            self.window_session.prepare_editor_plan(
                plan,
                (&current, &catalog),
                candidate,
                &self.configuration.settings().ui.buffer_line.scratch_label,
                descriptions,
            )?
        } else if activate_document {
            self.window_session.prepare_document_transition(
                candidate,
                &current,
                catalog.clone(),
                &self.configuration.settings().ui.buffer_line.scratch_label,
                descriptions,
            )?
        } else {
            self.window_session.prepare_editor_transition(
                candidate,
                &current,
                catalog.clone(),
                &self.configuration.settings().ui.buffer_line.scratch_label,
                Some(descriptions),
            )?
        };
        *self.state = self
            .window_session
            .accept_editor_transition(transition, &current, &catalog)?;
        self.reconcile_editor_windows();
        Ok(())
    }

    pub(super) fn focus_surface(&mut self, direction: FocusDirection) {
        self.reconcile_editor_windows();
        let direction = match direction {
            FocusDirection::Left => SpatialDirection::Left,
            FocusDirection::Down => SpatialDirection::Down,
            FocusDirection::Up => SpatialDirection::Up,
            FocusDirection::Right => SpatialDirection::Right,
        };
        let Some(target) = self.window_session.spatial_target(direction) else {
            return;
        };
        let result = self
            .window_session
            .plan_focus(
                target,
                &self.state.window_version(),
                self.extension.contributions().version(),
            )
            .and_then(|plan| self.apply_window_plan(plan));
        if let Err(error) = result {
            self.show_error(error);
            return;
        }
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.ensure_cursor_visible();
        self.sync_terminal_panel();
    }

    pub(super) fn apply_layout_action(&mut self, action: LayoutAction) {
        self.reconcile_editor_windows();
        if matches!(action, LayoutAction::ClosePane | LayoutAction::CloseTab) {
            match self
                .window_session
                .begin_layout_close(action, &(&*self.state).into())
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
            return;
        }
        let plan = match self.window_session.plan_layout(
            action,
            &(&*self.state).into(),
            self.configuration.settings(),
            self.extension.contributions().version(),
        ) {
            Ok(plan) => plan,
            Err(super::super::windows::LayoutPlanError::Dirty) => {
                self.show_error(self.message("dirty_close", &[]));
                return;
            }
            Err(super::super::windows::LayoutPlanError::Rejected(error)) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_window_plan(plan) {
            self.show_error(error);
        } else {
            self.cancel_pending_mapping();
            self.input.cancel_pending();
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn apply_retained_ui_interaction(&mut self, interaction: PluginUiInteraction) {
        match self
            .extension
            .window_interaction(self.window_session.graph(), interaction)
        {
            Ok(action) => {
                self.ui.plugin_surfaces = self.extension.window_snapshots();
                if let Some(action) = action {
                    self.invoke_mapped_command(&action);
                }
            }
            Err(error) => self.show_error(error.detail),
        }
    }

    pub(super) fn select_retained_ui_at_mouse(&mut self) {
        let Some((column, row)) = self.input.mouse_position() else {
            return;
        };
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let Some(window) = projection.hit_test(column, row) else {
            return;
        };
        let Some(rect) = projection.window(window).map(|projected| projected.rect) else {
            return;
        };
        if let Err(error) = self
            .extension
            .focus_window_row(window, usize::from(row.saturating_sub(rect.y)))
        {
            self.show_error(error.detail);
            return;
        }
        self.ui.plugin_surfaces = self.extension.window_snapshots();
    }

    pub(super) fn activate_private_surface_action(&mut self, at_mouse: bool) {
        let Some(projection) = self.compositor_projection() else {
            return;
        };
        let target = if at_mouse {
            self.input.mouse_position().and_then(|(x, y)| {
                projection.hit_test(x, y).and_then(|window| {
                    projection.window(window).map(|projected| {
                        (
                            window,
                            x.saturating_sub(projected.rect.x),
                            y.saturating_sub(projected.rect.y),
                        )
                    })
                })
            })
        } else {
            self.window_session.graph().focused().and_then(|window| {
                self.ui
                    .private_surfaces
                    .iter()
                    .find(|surface| surface.window == window)
                    .and_then(|surface| surface.cursor.map(|(x, y)| (window, x, y)))
            })
        };
        let Some((window, x, y)) = target else { return };
        match self.extension.private_action(window, x, y) {
            Ok(Some(action)) => self.invoke_mapped_command(&action),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn mouse_pane_geometry(
        &self,
    ) -> Option<(crate::editor::PaneId, u16, u16, u16, u16)> {
        let (global_column, global_row) = self.input.mouse_position()?;
        editor_pane_viewport_at(&self.state, &self.ui, global_column, global_row).map(|viewport| {
            (
                viewport.pane,
                global_column.saturating_sub(viewport.x),
                global_row.saturating_sub(viewport.y),
                viewport.width,
                viewport.height,
            )
        })
    }

    pub(super) fn focus_mouse_pane(&mut self) {
        self.reconcile_editor_windows();
        let Some((pane, ..)) = self.mouse_pane_geometry() else {
            return;
        };
        let Some(window) = self.window_session.editor_ids().window_for(pane) else {
            return;
        };
        let result = self
            .window_session
            .plan_focus(
                window,
                &self.state.window_version(),
                self.extension.contributions().version(),
            )
            .and_then(|plan| self.apply_window_plan(plan));
        if let Err(error) = result {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn apply_mouse_cursor(&mut self, extend_selection: bool) {
        self.reconcile_editor_windows();
        let Some((pane_id, column, row, pane_width, pane_height)) = self.mouse_pane_geometry()
        else {
            return;
        };
        let Some(view_id) = self.state.panes.get(pane_id).map(|pane| pane.view) else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let body_height = pane_height.saturating_sub(u16::from(
            self.configuration.settings().ui.status_line.enabled,
        ));
        if row >= body_height {
            return;
        }
        let gutter = editor_state_gutter_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            pane_width,
        );
        if available == 0 {
            return;
        }
        let wrap_enabled = self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&buffer_id)
                && self.configuration.settings().editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            pane_width,
            wrap_enabled,
        );
        let source = self.state.version();
        let transaction_id = TransactionId(self.next_transaction);
        let plan = self.input.mouse_plan(
            crate::editor::session::input::MouseDocument {
                editor: &source,
                view_id,
                pane_id,
                view,
                text: &buffer.text,
            },
            crate::editor::session::input::MouseGeometry {
                column,
                row,
                body_height,
                gutter,
                wrap: wrap_enabled,
                rows: &visual_rows,
                inlays: &self.ui.lsp_inlay_hints,
            },
            &self.configuration.settings().editor,
            transaction_id,
            extend_selection,
        );
        let Some(plan) = plan else {
            return;
        };
        self.transaction_id();
        let transaction = match self.input.mouse_transaction(&plan, &self.state.version()) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_authoritative_editor_transaction(transaction) {
            self.show_error(error);
        } else {
            let source = self.state.window_version();
            let current = self.state.active_view.and_then(|id| {
                self.state.views.get(id).and_then(|view| {
                    self.state
                        .buffers
                        .get(view.buffer)
                        .map(|buffer| (id, view.buffer, buffer.text.revision()))
                })
            });
            self.input.accept_mouse_plan(plan, &source, current);
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn scroll_view(&mut self, kind: &str, count: u64) {
        self.reconcile_editor_windows();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let Some(viewport) = active_editor_pane_viewport(&self.state, &self.ui)
            .filter(|viewport| viewport.view == view_id)
        else {
            return;
        };
        let body = usize::from(
            viewport
                .height
                .saturating_sub(u16::from(
                    self.configuration.settings().ui.status_line.enabled,
                ))
                .max(1),
        );
        let percent = if kind.starts_with("half_") {
            self.configuration.settings().editor.half_page_percent
        } else {
            self.configuration.settings().editor.full_page_percent
        };
        let amount = body.saturating_mul(usize::from(percent)) / 100;
        let amount = amount
            .max(1)
            .saturating_mul(usize::try_from(count).unwrap_or(usize::MAX));
        let available = editor_state_content_width(
            &self.state,
            &buffer.text,
            &self.configuration.settings().editor,
            viewport.width,
        );
        let wrap_enabled = available > 0
            && self.configuration.settings().editor.wrap
            && !(self.ui.large_file_buffers.contains(&view.buffer)
                && self.configuration.settings().editor.large_file.disable_wrap);
        let visual_rows = VisualRowModel::new_for_state(
            &self.state,
            &buffer.text,
            view_id,
            &self.ui,
            self.configuration.settings(),
            viewport.width,
            wrap_enabled,
        );
        let old_start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let first_anchor = if kind.ends_with("up") {
            visual_rows.backward(old_start_anchor, amount)
        } else {
            visual_rows.forward(old_start_anchor, amount)
        };
        let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
        let last_anchor = visual_rows.forward(first_anchor, body.saturating_sub(1));
        let target_anchor = cursor_anchor.clamp(first_anchor, last_anchor);
        let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let old_line_start = buffer
            .text
            .line_to_byte(current_line)
            .unwrap_or(ByteOffset(0));
        let old_prefix = buffer
            .text
            .slice(ByteRange::new(old_line_start, view.cursor).expect("cursor line range"))
            .unwrap_or_default();
        let current_column = configured_text_width(
            &old_prefix,
            self.configuration.settings().editor.tab_width,
            self.configuration.settings().editor.ambiguous_width,
        );
        let target_line = target_anchor.line;
        let target_start = buffer
            .text
            .line_to_byte(target_line)
            .unwrap_or(ByteOffset(0));
        let target_byte = if wrap_enabled {
            let current_cell = visual_rows.cursor_projection(view.cursor).1;
            visual_rows.byte_at_cell(target_anchor, current_cell)
        } else {
            let target_text = buffer.text.line(target_line).unwrap_or_default();
            let target_content = target_text.strip_suffix('\n').unwrap_or(&target_text);
            let mut display_column = 0_usize;
            let mut target_byte = 0_usize;
            for (relative, grapheme) in target_content.grapheme_indices(true) {
                let width = if grapheme == "\t" {
                    self.configuration.settings().editor.tab_width
                        - (display_column % self.configuration.settings().editor.tab_width)
                } else {
                    configured_grapheme_width(
                        grapheme,
                        self.configuration.settings().editor.ambiguous_width,
                    )
                };
                if display_column.saturating_add(width) > current_column {
                    target_byte = relative;
                    break;
                }
                display_column = display_column.saturating_add(width);
                target_byte = relative.saturating_add(grapheme.len());
            }
            target_byte
        };
        let target = ByteOffset(target_start.get().saturating_add(target_byte));
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first_anchor.line,
            })
            .with_operation(EditorOperation::SetScrollWrapRow {
                view: view_id,
                first_wrap_row: if wrap_enabled {
                    first_anchor.wrap_row
                } else {
                    0
                },
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: target,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl InteractiveApplication {
    pub(super) fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        if kind != PromptKind::ExplorerRename {
            self.cancel_explorer_rename_prompt();
        }
        if kind != PromptKind::ExplorerDelete {
            self.cancel_explorer_confirmation();
        }
        if kind != PromptKind::TerminalClose {
            self.terminal_session.cancel_close();
        }
        if kind != PromptKind::DirtyClose {
            self.window_session.cancel_editor_close();
        }
        self.reconcile_editor_windows();
        self.input.hold_focus(self.window_session.capture_focus());
        let active = self.state.active_view.map(|view| {
            (
                self.state.window_version(),
                view,
                self.state.views[view].mode,
            )
        });
        if let Some(mode) = self.input.open_prompt(kind, initial, active) {
            self.set_mode(mode);
        }
        self.project_input_hints();
        self.refresh_completions();
    }
    pub(super) fn open_confirmation(
        &mut self,
        kind: PromptKind,
        title: String,
        message: String,
        selected: crate::ui::ConfirmationChoice,
    ) {
        self.open_confirmation_actions(kind, title, message, selected, "Yes", "No");
    }
    pub(super) fn open_confirmation_actions(
        &mut self,
        kind: PromptKind,
        title: String,
        message: String,
        selected: crate::ui::ConfirmationChoice,
        positive_label: &str,
        negative_label: &str,
    ) {
        self.open_prompt(kind, String::new());
        let binding = |command: &str| {
            self.configuration
                .keymaps()
                .binding_for("confirmation_prompt", command)
        };
        // Arrows and tab also move the selection; the footer names only y/n so it fits.
        let selections = [
            "surface.confirmation.select_yes",
            "surface.confirmation.select_no",
        ]
        .into_iter()
        .filter_map(binding)
        .collect::<Vec<_>>()
        .join("/");
        let mut hints = Vec::new();
        if !selections.is_empty() {
            hints.push(format!("{selections} answer"));
        }
        if let Some(key) = binding("command_line.submit") {
            hints.push(format!("{key} confirm"));
        }
        if let Some(key) = binding("command_line.cancel") {
            hints.push(format!("{key} cancel"));
        }
        // Help is a focusable action of the confirmation itself, not a global key: it is
        // advertised here through the same keymap lookup as every other action.
        if let Some(key) = binding("overlay.help.form") {
            hints.push(format!("{key} help"));
        }
        let hint = hints.join(" · ");
        self.input.configure_confirmation_actions(
            title,
            message,
            selected,
            positive_label,
            negative_label,
            hint,
        );
        self.project_input_prompt();
        self.reconcile_editor_windows();
    }
    pub(super) fn close_prompt(&mut self) {
        self.close_prompt_with_editor_close(true);
    }
    fn close_prompt_with_editor_close(&mut self, cancel_editor_close: bool) {
        if let Some((view, cursor)) = self.input.take_search_origin() {
            self.move_search_cursor(view, cursor);
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
        {
            self.extension
                .answer_confirmation(crate::extensions::PluginConfirmationAnswer::Cancelled);
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::AgentPermission)
        {
            self.cancel_agent_permission();
        }
        self.cancel_explorer_rename_prompt();
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::TerminalClose)
        {
            self.terminal_session.cancel_close();
        }
        if cancel_editor_close
            && self
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == PromptKind::DirtyClose)
        {
            self.window_session.cancel_editor_close();
        }
        if self
            .input
            .prompt()
            .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust)
        {
            self.workspace.cancel_confirmation();
        }
        self.cancel_explorer_confirmation();
        if let Some((view, mode)) = self.input.close_prompt(&self.state.window_version()) {
            if self.state.views.contains_key(view) {
                self.set_specific_mode(view, mode);
            }
        }
        self.restore_input_focus();
        self.project_input_prompt();
    }
    pub(super) fn present_next_plugin_confirmation(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        let Some(request) = self.extension.next_confirmation(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
            &self.input,
            &self.window_session,
        ) else {
            let _ = self.extension.dispatch_confirmation_results(
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
            return;
        };
        let selected = match request.default_answer {
            crate::extensions::PluginConfirmationAnswer::Yes => crate::ui::ConfirmationChoice::Yes,
            crate::extensions::PluginConfirmationAnswer::No
            | crate::extensions::PluginConfirmationAnswer::Cancelled => {
                crate::ui::ConfirmationChoice::No
            }
        };
        self.open_confirmation(
            PromptKind::PluginConfirmation,
            format!("Plugin {} · {}", request.plugin, request.title),
            request.message,
            selected,
        );
    }
    pub(super) fn submit_terminal_close(&mut self, selected: crate::ui::ConfirmationChoice) {
        let source = self.terminal_session.take_close();
        // Restore the pre-prompt focus first; Hide or Stop then performs the
        // accepted final focus transition without the prompt lease undoing it.
        self.close_prompt();
        let id = match source {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match selected {
            crate::ui::ConfirmationChoice::Yes => {
                match self
                    .window_session
                    .require_native_stop(NativeSurfaceTarget::Terminal(id.0))
                {
                    Ok(()) => {
                        self.terminal_session.cancel_open();
                        self.retire_terminal(id, true);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            crate::ui::ConfirmationChoice::No => {
                if let Err(error) = self
                    .window_session
                    .place_native(NativeSurfaceTarget::Terminal(id.0), WindowPlacement::Hidden)
                {
                    self.show_error(error);
                }
            }
        }
    }
    pub(super) fn handle_editor_close_step(&mut self, step: EditorCloseStep) {
        match step {
            EditorCloseStep::Prompt { title, message } => self.open_confirmation_actions(
                PromptKind::DirtyClose,
                title,
                message,
                crate::ui::ConfirmationChoice::No,
                "Save",
                "Discard",
            ),
            EditorCloseStep::Save(token) => {
                let buffer = token.buffer();
                let retained = token.clone();
                match self.request_save_buffer(buffer, false, None, false, Some(token)) {
                    Ok(()) => self.notify_message(self.message("saving", &[]), false),
                    Err(error) => {
                        self.window_session.fail_editor_close_save(&retained);
                        self.show_document_failure(error);
                    }
                }
            }
            EditorCloseStep::Ready { target, discarded } => {
                self.apply_accepted_editor_close(target, discarded);
            }
        }
    }

    fn submit_dirty_close(&mut self, selected: crate::ui::ConfirmationChoice) {
        let step = self.window_session.choose_editor_close(
            selected == crate::ui::ConfirmationChoice::Yes,
            &(&*self.state).into(),
        );
        self.close_prompt_with_editor_close(false);
        match step {
            Ok(step) => self.handle_editor_close_step(step),
            Err(error) => {
                self.window_session.cancel_editor_close();
                self.show_error(error);
            }
        }
    }

    fn apply_accepted_editor_close(
        &mut self,
        target: EditorCloseTarget,
        discarded: Vec<crate::editor::BufferId>,
    ) {
        match target {
            EditorCloseTarget::Buffer { view, buffer } => {
                self.close_buffer_target(view, buffer);
            }
            EditorCloseTarget::Layout(_) | EditorCloseTarget::Window { .. } => {
                let plan = self.window_session.plan_accepted_editor_close(
                    target,
                    discarded,
                    &(&*self.state).into(),
                    self.configuration.settings(),
                    self.extension.contributions().version(),
                );
                match plan {
                    Ok(plan) => {
                        if let Err(error) = self.apply_window_plan(plan) {
                            self.show_error(error);
                        } else {
                            self.cancel_pending_mapping();
                            self.input.cancel_pending();
                            self.ensure_cursor_visible();
                        }
                    }
                    Err(LayoutPlanError::Dirty) => unreachable!("accepted close bypasses review"),
                    Err(LayoutPlanError::Rejected(error)) => self.show_error(error),
                }
            }
        }
    }

    pub(super) fn submit_plugin_confirmation(&mut self, selected: crate::ui::ConfirmationChoice) {
        let answer = match selected {
            crate::ui::ConfirmationChoice::Yes => crate::extensions::PluginConfirmationAnswer::Yes,
            crate::ui::ConfirmationChoice::No => crate::extensions::PluginConfirmationAnswer::No,
        };
        self.extension.answer_confirmation(answer);
        self.close_prompt();
        let _ = self.extension.dispatch_confirmation_results(
            &self.state,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        );
        self.present_next_plugin_confirmation();
    }
    pub(super) fn prompt_insert(&mut self, text: &str) {
        self.input.prompt_insert(text);
        self.refresh_completions();
        self.preview_search();
    }
    pub(super) fn prompt_backspace(&mut self) {
        self.input.prompt_backspace();
        self.refresh_completions();
        self.preview_search();
    }
    pub(super) fn prompt_move(&mut self, movement: PromptMove) {
        self.input.prompt_move(movement);
        self.project_input_prompt();
    }
    pub(super) fn prompt_history(&mut self, direction: i8) {
        self.input
            .prompt_history(direction, self.configuration.settings());
        self.refresh_completions();
        self.preview_search();
    }
    pub(super) fn prompt_complete(&mut self, direction: i8) {
        self.input
            .prompt_complete(direction, self.configuration.settings());
        self.project_input_prompt();
    }
    pub(super) fn accept_completion(&mut self) -> bool {
        let accepted = self.input.accept_completion(self.configuration.settings());
        if accepted {
            self.refresh_completions();
        }
        accepted
    }
    pub(super) fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        self.input
            .record_prompt_history(kind, input, self.configuration.settings());
    }
    pub(super) fn refresh_completions(&mut self) {
        if let Some(request) = self.input.refresh_completions(
            self.configuration.settings(),
            &self.state.workspace_root,
            self.state
                .buffers
                .values()
                .filter_map(|buffer| buffer.path.as_deref()),
            self.commands.specs().map(|spec| spec.id.as_str()),
        ) && !self.input.completion_read_pending()
        {
            if let Ok(id) = self
                .documents
                .list_directory(request.directory, request.limit)
            {
                self.input.register_completion_read(id, request.key);
            }
        }
        self.project_input_prompt();
    }

    pub(super) fn submit_prompt(&mut self) {
        if self
            .configuration
            .settings()
            .input
            .completion_accept_on_submit
            && self.accept_completion()
        {
            // A chosen command runs at once, as Enter does in Vim's wildmenu; a chosen path
            // waits, so a directory can be completed further.
            let command = self.input.prompt().is_some_and(|prompt| {
                prompt.kind == PromptKind::Command && !prompt.input.contains(char::is_whitespace)
            });
            if !command {
                return;
            }
        }
        let Some(prompt) = self.input.prompt().cloned() else {
            return;
        };
        if prompt
            .confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.selected == crate::ui::ConfirmationChoice::Yes)
        {
            let (width, height) = crate::ui::prompt_surface_dimensions(
                &self.state,
                &self.ui,
                self.window_session.width(),
                self.window_session.height(),
            );
            if !crate::ui::confirmation_fully_visible(
                width,
                height,
                self.configuration.settings(),
                &prompt,
            ) {
                return;
            }
        }
        if !matches!(
            prompt.kind,
            PromptKind::WorkspaceTrust
                | PromptKind::GitReset
                | PromptKind::ExplorerDelete
                | PromptKind::ExplorerRename
                | PromptKind::PluginConfirmation
                | PromptKind::TerminalClose
                | PromptKind::DirtyClose
                | PromptKind::BreakpointCondition
                | PromptKind::ReplaceFind
                | PromptKind::ReplaceWith
        ) {
            self.record_prompt_history(prompt.kind, &prompt.input);
        }
        match prompt.kind {
            PromptKind::Command => match parse_ex(
                &prompt.input,
                &self.configuration.settings().input.command_aliases,
            ) {
                Ok(invocation) => {
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    self.prompt_error(error.to_string());
                }
            },
            PromptKind::WorkspaceTrust => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                if selected == crate::ui::ConfirmationChoice::Yes {
                    // Confirm before closing: closing cancels a still-pending confirmation.
                    self.confirm_workspace_trust();
                    self.close_prompt();
                } else {
                    self.close_prompt();
                    self.notify_message(self.message("workspace_trust_declined", &[]), false);
                }
            }
            PromptKind::GitReset => {
                self.close_prompt();
                self.confirm_reset_selected_git_hunk(prompt.input);
            }
            PromptKind::GitDestructive => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                self.close_prompt();
                // A previewed whole-file replacement is answered by this same confirmation, and
                // it is answered through the REBUILT plan rather than the previewed one.
                if self.git.has_pending_replacement() {
                    self.apply_git_replacement(selected);
                    return;
                }
                let action = self.git.take_pending_destructive();
                if selected == crate::ui::ConfirmationChoice::Yes {
                    self.confirm_git_destructive(action);
                } else if action.is_some() {
                    self.notify_message("Nothing was removed".into(), false);
                }
            }
            PromptKind::GitStageUnsaved => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                self.close_prompt();
                if selected == crate::ui::ConfirmationChoice::Yes {
                    self.confirm_stage_unsaved();
                } else {
                    self.input.clear_pending_stage();
                    self.notify_message("Nothing was staged".into(), false);
                }
            }
            PromptKind::ExplorerDelete => {
                let selected = prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected);
                if selected == crate::ui::ConfirmationChoice::Yes {
                    self.confirm_explorer_delete();
                }
                self.close_prompt();
            }
            PromptKind::PluginConfirmation => {
                // The extension owner asynchronously consumes this one-shot result.
                self.submit_plugin_confirmation(
                    prompt
                        .confirmation
                        .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
                );
            }
            PromptKind::TerminalClose => self.submit_terminal_close(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::DirtyClose => self.submit_dirty_close(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::ExplorerRename => self.submit_explorer_rename_prompt(&prompt.input),
            PromptKind::ReviewComment => {
                self.close_prompt();
                self.submit_review_comment(prompt.input);
            }
            PromptKind::ReplaceFind => {
                self.close_prompt();
                if !prompt.input.is_empty() {
                    self.replace_query = Some(prompt.input);
                    self.open_prompt(PromptKind::ReplaceWith, String::new());
                }
            }
            PromptKind::ReplaceWith => {
                self.close_prompt();
                if let Some(query) = self.replace_query.take() {
                    self.start_workspace_search(query, Some(prompt.input));
                }
            }
            PromptKind::BreakpointCondition => {
                self.close_prompt();
                let condition = prompt.input.trim();
                self.configure_breakpoint(
                    (!condition.is_empty()).then(|| condition.to_owned()),
                    None,
                    None,
                );
            }
            PromptKind::AgentPermission => self.answer_agent_permission(
                prompt
                    .confirmation
                    .map_or(crate::ui::ConfirmationChoice::No, |state| state.selected),
            ),
            PromptKind::SearchForward | PromptKind::SearchBackward => {
                if prompt.input.is_empty() {
                    let message = self.message("search_empty", &[]);
                    self.prompt_error(message);
                    return;
                }
                if self.state.active_view.is_none() {
                    let message = self.message("search_no_buffer", &[]);
                    self.prompt_error(message);
                    return;
                }
                if self.configuration.settings().search.regex {
                    let source = self
                        .state
                        .active_buffer_id()
                        .and_then(|buffer| self.state.buffers.get(buffer))
                        .map(|buffer| buffer.text.to_string())
                        .unwrap_or_default();
                    if let Err(error) = crate::editor::input::search_matches(
                        &source,
                        &prompt.input,
                        self.configuration.settings().search.smart_case,
                        true,
                    ) {
                        self.prompt_error(error.to_string());
                        return;
                    }
                }
                let forward = prompt.kind == PromptKind::SearchForward;
                self.ui.search_query = Some(prompt.input);
                self.input.remember_search(forward);
                self.close_prompt();
                self.perform_search(forward);
            }
        }
    }

    /// As with Vim's incsearch: while a `/` pattern is typed the cursor sits on its next match,
    /// and it goes back when the prompt closes.
    fn preview_search(&mut self) {
        let Some(prompt) = self.input.prompt().filter(|prompt| {
            matches!(prompt.kind, PromptKind::SearchForward | PromptKind::SearchBackward)
        }) else {
            return;
        };
        let forward = prompt.kind == PromptKind::SearchForward;
        let query = prompt.input.clone();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let buffer_id = self.state.views[view_id].buffer;
        if self.ui.large_file_buffers.contains(&buffer_id) {
            return;
        }
        let origin = self.input.search_origin(view_id, self.state.views[view_id].cursor);
        let search = &self.configuration.settings().search;
        let buffer = &self.state.buffers[buffer_id];
        let matches = crate::editor::input::search_matches(
            &buffer.text.to_string(),
            &query,
            search.smart_case,
            search.regex,
        )
        .unwrap_or_default();
        let target = search_target(&buffer.text, &matches, origin, forward, search.wrap);
        self.move_search_cursor(view_id, target.map_or(origin, ByteOffset));
    }

    fn move_search_cursor(&mut self, view: crate::editor::ViewId, position: ByteOffset) {
        if !self.state.views.contains_key(view) {
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetCursor { view, position })
            .with_undo_policy(UndoPolicy::Ignore);
        if self.state.apply_transaction(&transaction).is_ok() {
            self.ensure_cursor_visible();
        }
    }

    pub(super) fn perform_search(&mut self, forward: bool) {
        let Some(query) = self.ui.search_query.clone() else {
            self.show_error(self.message("no_previous_search", &[]));
            return;
        };
        self.record_jump();
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor_offset = view.cursor;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let source = buffer.text.to_string();
        let matches = match crate::editor::input::search_matches(
            &source,
            &query,
            self.configuration.settings().search.smart_case,
            self.configuration.settings().search.regex,
        ) {
            Ok(matches) => matches,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let wrap = self.configuration.settings().search.wrap;
        let Some(target) = search_target(&buffer.text, &matches, cursor_offset, forward, wrap) else {
            self.show_error(self.message("pattern_not_found", &[("query", query)]));
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(target),
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl InteractiveApplication {
    pub(super) fn start_dap_configuration(&mut self, name: &str) {
        match self.debugger_session.request_debug(
            (Some(name), None),
            None,
            None,
            (
                &self.configuration.settings().debug,
                &self.configuration.settings().terminals,
                self.configuration.binding_revision(),
            ),
            &self.workspace,
            self.tooling.entries(),
        ) {
            Ok(intent) => self.run_trusted(TrustedAction::StartDebug(intent)),
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn start_dap(&mut self, requested: Option<String>, program: Option<PathBuf>) {
        let document = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers.get(id))
            .and_then(|buffer| buffer.path.as_deref())
            .and_then(|path| language_id_for_path(path).map(|language| (path, language)));
        let Some(document) = document else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        match self.debugger_session.request_debug(
            (None, requested.as_deref()),
            program,
            Some(document),
            (
                &self.configuration.settings().debug,
                &self.configuration.settings().terminals,
                self.configuration.binding_revision(),
            ),
            &self.workspace,
            self.tooling.entries(),
        ) {
            Ok(intent) => self.run_trusted(TrustedAction::StartDebug(intent)),
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn authorize_debug_launch(
        &mut self,
        intent: crate::editor::session::debugger::DebugIntent,
    ) {
        if let Err(error) = self
            .debugger_session
            .authorize_debug(intent, &self.workspace)
        {
            self.show_error(error);
            return;
        }
        self.advance_debug_launch();
    }
    pub(super) fn advance_debug_launch(&mut self) {
        match self.debugger_session.advance_debug(
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Debugger),
            &self.configuration.settings().debug,
            self.configuration.binding_revision(),
            self.tooling.entries(),
        ) {
            Ok(Some(name)) => {
                self.notify_message(self.message("dap_started", &[("adapter", name)]), false);
                self.notices.mark_current_passing();
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn process_dap_project_root(&mut self) {
        if let Some(result) = self
            .workspace
            .project_result(crate::editor::session::workspace::DiscoveryKind::Debugger)
        {
            if let Err(error) = self.debugger_session.debug_root_result(result) {
                let _ = self.debugger_session.stop();
                self.show_error(error);
            }
        }
        self.advance_debug_launch();
    }
    pub(super) fn process_dap_events(&mut self) {
        for error in self.debugger_session.reply_failures() {
            self.show_error(error);
        }
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        for _ in 0..256 {
            let Some(event) = self.debugger_session.try_event() else {
                break;
            };
            match event {
                DapEvent::Starting { .. } => {}
                DapEvent::RequestFailed { command, message, .. } => {
                    if !self.debugger_session.watch_request_failed(&command) {
                        self.show_error(message);
                    }
                }
                // "Starting" is over, and it would cover the variables the stop just filled in.
                // Keys go back to the source, or F10 would go to the program's terminal.
                DapEvent::Stopped { .. } => {
                    self.notices.clear_passing();
                    self.project_notice();
                    self.set_terminal_focus(false);
                }
                // The panes and the execution sign show these.
                DapEvent::Running { .. } | DapEvent::Output { .. } => {}
                DapEvent::Response { response, .. } => match self
                    .debugger_session
                    .response(response)
                {
                    crate::editor::session::debugger::DebugResponse::Retired
                    | crate::editor::session::debugger::DebugResponse::Absorbed => {}
                    crate::editor::session::debugger::DebugResponse::StoppedAt(location) => {
                        // A library frame outside the workspace stays in the stack pane.
                        if self
                            .workspace
                            .roots()
                            .iter()
                            .any(|root| location.path.starts_with(&root.identity.canonical_path))
                        {
                            self.navigate_matcher_source(MatcherSource {
                                path: location.path,
                                line: location.line.saturating_sub(1) as usize,
                                column: location.column.saturating_sub(1) as usize,
                            });
                        }
                    }
                    crate::editor::session::debugger::DebugResponse::Picker { kind, rows } => {
                        self.install_debug_rows(kind, rows)
                    }
                    crate::editor::session::debugger::DebugResponse::Result(response) => {
                        match response {
                            DapResponse::Evaluate(value) => {
                                // The answer is recorded as an EVALUATION RESULT, which is a
                                // different origin from the command that asked for it.
                                self.debugger_session.record_console_evaluation(format!(
                                    "{} = {}",
                                    value.name, value.value
                                ));
                                self.notify_message(
                                    self.message(
                                        "dap_evaluate_result",
                                        &[("name", value.name), ("value", value.value)],
                                    ),
                                    false,
                                );
                            }
                            DapResponse::Breakpoints(_) => {}
                            DapResponse::Threads(_)
                            | DapResponse::Stack(_)
                            | DapResponse::Scopes(_)
                            | DapResponse::Variables(_) => {
                                unreachable!("owner converts response rows")
                            }
                            DapResponse::Source {
                                name,
                                content,
                                mime_type,
                            } => {
                                let Some(target) = self.debugger_session.take_source() else {
                                    continue;
                                };
                                if !self.window_session.navigation_current(
                                    &target.navigation,
                                    &self.state.window_version(),
                                    self.state.active_view,
                                ) || !self.input.capture_current(&target.capture)
                                {
                                    continue;
                                }
                                let mut draft = self.state.draft();
                                draft.open_virtual_read_only(
                                    TextBuffer::from_text(&content),
                                    "dap".into(),
                                    0,
                                    0,
                                    0,
                                );
                                if let Err(error) = self.commit_editor_draft(draft, None, false) {
                                    self.show_error(error);
                                    continue;
                                }
                                self.notify_message(
                                    self.message(
                                        "dap_source_loaded",
                                        &[("name", name), ("mime", mime_type.unwrap_or_default())],
                                    ),
                                    false,
                                );
                            }
                        }
                    }
                },
                DapEvent::Terminated { adapter } => {
                    let _ = self.debugger_session.stop();
                    self.notify_message(
                        self.message(
                            "dap_stopped",
                            &[("adapter", adapter), ("reason", "terminated".into())],
                        ),
                        false,
                    );
                }
                DapEvent::RunInTerminal {
                    reply,
                    cwd,
                    arguments,
                    environment,
                    ..
                } => {
                    let reply = reply.claim();
                    if let Some(program) =
                        self.debugger_session.adapter_terminal(&cwd, &arguments, &environment)
                    {
                        self.open_adapter_terminal(program, reply);
                        continue;
                    }
                    let profile = match self.debugger_session.terminal_request(
                        &cwd,
                        &arguments,
                        &environment,
                        &self.configuration.settings().terminals,
                    ) {
                        Ok(profile) => profile.to_owned(),
                        Err(error) => {
                            let _ = reply.respond(false, Some(error));
                            continue;
                        }
                    };
                    if !reply.current() {
                        continue;
                    }
                    self.request_terminal_open(&profile, Some(reply));
                }
                DapEvent::Failed { adapter, message } => {
                    let _ = self.debugger_session.stop();
                    self.show_error(self.message(
                        "dap_failed",
                        &[("adapter", adapter), ("diagnostic", message)],
                    ));
                }
            }
        }
    
        // The arrangement APPEARS when the session does: the panes are projected on every pump, so
        // starting, stopping and reporting all refresh what the layout shows.
        self.project_debug_panes();
    }
}

impl InteractiveApplication {
    pub(super) fn stop_task(&mut self, name: &str) {
        match self.tasks.stop(name) {
            Ok(Some(source)) => {
                self.diagnostics.clear_source(&source);
                self.sync_ui_diagnostics();
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn start_task_plan(
        &mut self,
        name: &str,
        adapter: AdapterContext,
    ) {
        let managed = match &adapter {
            AdapterContext::Formatter { source }
                if !self.configuration.settings().tasks.contains_key(name) =>
            {
                let path = self.state.buffers[source.buffer()].path.clone().unwrap_or_default();
                let Some(tool) = managed_formatter(|tool| tool.id == name, &path) else {
                    self.show_error(self.message("formatter_missing", &[("name", name.into())]));
                    return;
                };
                let mut config = self.configuration.settings().clone();
                config.tasks.insert(
                    name.into(),
                    crate::config::TaskConfig {
                        kind: crate::config::TaskKind::Formatter,
                        executable: tool.executable,
                        arguments: tool.arguments,
                        cwd: None,
                        environment: tool.environment,
                        dependencies: Vec::new(),
                        timeout_ms: Some(10_000),
                        stdin: true,
                        matcher: None,
                    },
                );
                Some(config)
            }
            _ => None,
        };
        match self.tasks.plan(
            name,
            &adapter,
            managed.as_ref().unwrap_or(self.configuration.settings()),
            &self.workspace,
            self.configuration.binding_revision(),
        ) {
            Ok(()) => {}
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
            self.diagnostics.clear_owner(&owner);
            self.sync_ui_diagnostics();
        }
        self.start_task_step(name.into());
    }
    pub(super) fn start_task_step(&mut self, requested: String) {
        let (step, _) = match self.tasks.step(
            &requested,
            &self.workspace,
            self.configuration.binding_revision(),
        ) {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.tasks.queue_preflight(
            step,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }

    pub(super) fn apply_formatter_result(&mut self, task: &RunningTask) {
        let transaction = self.transaction_id();
        match self.tasks.formatter_transaction(
            task,
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            transaction,
        ) {
            Ok(Some(transaction)) => match self
                .state
                .prepare_transaction(&transaction)
                .map_err(|e| e.to_string())
                .and_then(|draft| self.commit_editor_draft(draft, None, false))
            {
                Ok(()) => self.notify_message(self.message("formatter_applied", &[]), false),
                Err(error) => self.show_error(error),
            },
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn drain_linter_sources(&mut self) {
        self.tasks.retire_linter_sources(
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            &self.diagnostics,
        );
        for key in self.tasks.linter_cancellations() {
            if let Some(cancel) = self.documents.cancel_linter_read(&key) {
                self.tasks.linter_cancelled(cancel);
            }
        }
        for _ in 0..64 {
            let Some(loan) = self.tasks.linter_read(
                &self.state,
                &self.workspace,
                self.configuration.binding_revision_ref(),
                &self.diagnostics,
            ) else {
                break;
            };
            // Full capacity preserves the unread path for retry. Permanent failure retires
            // only this build; already-admitted reads stay owned until a proven terminal.
            if !matches!(self.documents.submit_linter_read(loan), Ok(true)) {
                break;
            }
        }
        while let Some(update) = self.tasks.next_linter_update(
            &self.state,
            &self.workspace,
            self.configuration.binding_revision(),
            &self.diagnostics,
        ) {
            match update {
                crate::editor::session::tasks::LinterUpdate::Publish { producer, batches } => {
                    if let Err(error) =
                        self.diagnostics
                            .replace_source(&producer, batches, &self.state)
                    {
                        self.show_error(error);
                    }
                    self.sync_ui_diagnostics();
                }
                crate::editor::session::tasks::LinterUpdate::Failed(error) => {
                    self.show_error(error)
                }
            }
        }
    }

    pub(super) fn show_task_picker(&mut self) {
        let items = self
            .tasks
            .task_items()
            .into_iter()
            .map(|item| (item, PickerPayload::Command(String::new())))
            .collect();
        self.install_picker(self.message("picker_tasks_title", &[]), items);
    }

    pub(super) fn show_output_picker(&mut self, name: &str) {
        let previous = if self.tasks.active_output() == Some(name)
            && self.search.picker_surface() == PickerSurfaceKind::Output
        {
            self.search.picker_bookmark()
        } else {
            None
        };
        let normal = previous.is_some() && self.input.picker_normal();
        let follow_tail = previous.is_none() || self.search.picker_follows_tail();
        self.tasks.select_output(Some(name));
        let items = self
            .tasks
            .output_items(name)
            .into_iter()
            .map(|item| (item, PickerPayload::Command(String::new())))
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Output,
            self.message("picker_output_title", &[("name", name.into())]),
            items,
        );
        self.search.restore_picker_view(previous, follow_tail);
        self.input.set_picker_normal(normal);
        self.sync_picker_ui();
    }

    pub(super) fn show_test_picker(&mut self, name: &str) {
        let items = self
            .tasks
            .test_items(name)
            .into_iter()
            .map(|(item, key)| (item, PickerPayload::TaskTest(key)))
            .collect();
        self.install_picker(
            self.message("picker_tests_title", &[("name", name.into())]),
            items,
        );
    }

    pub(super) fn process_job_events(&mut self) {
        self.drain_job_events(None);
    }
    pub(super) fn drain_job_events(&mut self, deadline: Option<Instant>) {
        for _ in 0..256 {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            let Some(update) = self
                .tasks
                .next_update(&self.workspace, self.configuration.binding_revision())
            else {
                break;
            };
            match update {
                crate::editor::session::tasks::TaskUpdate::Output => {
                    if let Some(name) = self.tasks.active_output().map(str::to_owned) {
                        if self.search.picker_surface() == PickerSurfaceKind::Output {
                            self.show_output_picker(&name);
                        }
                    }
                }
                crate::editor::session::tasks::TaskUpdate::Finished {
                    task,
                    code,
                    truncated,
                    next,
                } => {
                    if !self.tasks.current(
                        &task,
                        &self.workspace,
                        self.configuration.binding_revision(),
                    ) {
                        continue;
                    }
                    if next {
                        self.start_task_step(task.requested().clone());
                    } else {
                        if code == Some(0) && !truncated {
                            self.apply_formatter_result(&task);
                        } else if truncated
                            && matches!(task.adapter(), AdapterContext::Formatter { .. })
                        {
                            self.show_error(self.message("formatter_truncated", &[]));
                        }
                        // Whatever the formatter did, the save it was holding back goes on.
                        if matches!(task.adapter(), AdapterContext::Formatter { .. }) {
                            self.finish_format_on_save();
                        }
                        // Linters exit 1 when they have findings; that is not a failure.
                        let linter_ran = matches!(task.adapter(), AdapterContext::Linter { .. })
                            && matches!(code, Some(0 | 1));
                        if !linter_ran
                            && (!matches!(task.adapter(), AdapterContext::Formatter { .. })
                                || code != Some(0))
                        {
                            self.notify_message(
                                self.message(
                                    "task_exited",
                                    &[
                                        ("name", task.name().clone()),
                                        (
                                            "code",
                                            code.map_or_else(|| "signal".into(), |v| v.to_string()),
                                        ),
                                    ],
                                ),
                                code != Some(0),
                            );
                        }
                        if truncated && matches!(task.adapter(), AdapterContext::Linter { .. }) {
                            self.show_error("linter output truncated; diagnostics not published");
                        }
                        if linter_ran && !truncated {
                            if let Err(error) = self.tasks.begin_linter(
                                *task,
                                &self.state,
                                &self.workspace,
                                self.configuration.binding_revision(),
                                &self.diagnostics,
                                (
                                    self.configuration.settings().workspace.maximum_diagnostics,
                                    self.configuration
                                        .settings()
                                        .workspace
                                        .search
                                        .maximum_file_bytes,
                                ),
                            ) {
                                self.show_error(error);
                            }
                        }
                    }
                }
                crate::editor::session::tasks::TaskUpdate::Failed { source, message } => {
                    if let Some(source) = source {
                        self.diagnostics.clear_source(&source);
                        self.sync_ui_diagnostics();
                    }
                    self.show_error(message);
                }
            }
        }
        if deadline.is_none_or(|deadline| Instant::now() < deadline) {
            self.drain_linter_sources();
        }
    }
}

// Remaining editor/window routing; terminal data and algorithms live in the owner.
use super::{PtyId, RegisterShape, RegisterValue};
impl InteractiveApplication {
    fn terminal_command_target(&self) -> Option<PtyId> {
        self.keyboard_terminal()
            .or(self.terminal_session.selected())
    }
    pub(super) fn reject_terminal_responses(&mut self, reason: &str) {
        self.debugger_session.reject_terminals(reason);
    }

    /// Shows the terminal as a centered float covering most of the screen, like LazyVim's
    /// lazygit window.
    fn float_terminal(&mut self, id: PtyId) {
        self.terminal_session.display(id);
        let (width, height) = (self.window_session.width(), self.window_session.height());
        let rect = WindowRect {
            x: width / 20,
            y: height / 20,
            width: width - width / 10,
            height: height - height / 10,
        };
        self.window_session
            .prefer_terminal_placement(id.0, WindowPlacement::Floating(rect));
    }

    pub(super) fn retire_terminal(&mut self, id: PtyId, request_close: bool) {
        let focused = self.window_session.focused_terminal() == Some(id.0);
        if request_close {
            match self.terminal_session.request_stop(id) {
                Ok(true) => {
                    if focused {
                        self.set_terminal_focus(false);
                    }
                    self.sync_terminal_panel();
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        } else {
            self.terminal_session.close(id);
        }
        if let Some(reply) = self.debugger_session.take_terminal(&id) {
            let _ = reply.respond(
                false,
                Some("terminal closed before startup response".into()),
            );
        }
        if focused {
            self.set_terminal_focus(self.terminal_session.selected().is_some());
        }
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.sync_explorer_ui();
        self.ensure_cursor_visible();
    }
    pub(super) fn enter_terminal_normal_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_normal_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
    pub(super) fn enter_terminal_input_mode(&mut self) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.enter_terminal_input_mode(id);
        }
        self.cancel_pending_mapping();
        self.sync_terminal_panel();
    }
    pub(super) fn move_terminal_normal_cursor(&mut self, motion: &str) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        if let Err(error) = self
            .terminal_session
            .move_terminal_normal_cursor(id, motion)
        {
            self.show_error(error);
        }
        self.sync_terminal_panel();
    }
    pub(super) fn start_terminal_visual(&mut self, linewise: bool) {
        if let Some(id) = self.terminal_command_target() {
            self.terminal_session.start_terminal_visual(id, linewise);
        }
        self.sync_terminal_panel();
    }
    pub(super) fn yank_terminal_text(&mut self, line: bool) {
        let Some(id) = self.terminal_command_target() else {
            return;
        };
        let Some((text, linewise)) = self.terminal_session.yank_terminal_text(id, line) else {
            return;
        };
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text,
                shape: if linewise {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                },
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        self.notify_message(self.message("terminal_yanked", &[]), false);
        self.sync_terminal_panel();
    }
    pub(super) fn sync_terminal_panel(&mut self) {
        self.ui.terminal_panel = self.terminal_session.projection(
            self.window_session.focused_terminal(),
            self.window_session.terminal_height(),
        );
        if self.ui.terminal_panel.is_none() {
            self.set_terminal_focus(false);
        }
        self.reconcile_editor_windows();
    }
}

impl InteractiveApplication {
    pub(super) fn request_terminal_open(
        &mut self,
        profile: &str,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("requesting debugger retired");
            return;
        }
        let file = self.state.active_view.and_then(|view| {
            let view = &self.state.views[view];
            let buffer = &self.state.buffers[view.buffer];
            let line = buffer.text.byte_to_line(view.cursor).map_or(1, |line| line + 1);
            buffer.path.as_deref().map(|path| (path, line))
        });
        match self.terminal_session.request_open(
            profile,
            &self.workspace,
            file,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
            self.input.capture_epoch(),
        ) {
            Ok(intent) => self.run_trusted(if let Some(reply) = reply {
                TrustedAction::OpenDapTerminal { intent, reply }
            } else {
                TrustedAction::OpenTerminal(intent)
            }),
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }

    /// The program's terminal from the last run. It stays after a run ends so its output can be
    /// read, and goes when the user stops the program or runs it again.
    fn close_debug_terminal(&mut self) {
        let Some(id) = self.debug_terminal.take() else {
            return;
        };
        if self.terminal_session.request_stop(id).is_ok() {
            self.sync_terminal_panel();
        }
    }

    fn open_adapter_terminal(
        &mut self,
        program: crate::config::TerminalProfileConfig,
        reply: crate::debugger::DapTerminalReply,
    ) {
        match self.terminal_session.open_program(
            super::super::terminal::DEBUG_TERMINAL_PROFILE,
            program,
            &self.workspace,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
            self.input.capture_epoch(),
        ) {
            Ok(intent) => self.run_trusted(TrustedAction::OpenDapTerminal { intent, reply }),
            Err(error) => {
                let _ = reply.respond(false, Some(error.clone()));
                self.show_error(error);
            }
        }
    }

    pub(super) fn open_terminal_with_response(
        &mut self,
        intent: super::super::terminal::TerminalIntent,
        reply: Option<crate::debugger::DapTerminalReply>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("the debugger that asked for this terminal has stopped");
            return;
        }
        let for_debugger = reply.is_some();
        match self.terminal_session.prepare(intent, &self.workspace)
        {
            Ok(open) => {
                let id = self
                    .terminal_session
                    .resolve(&open.key)
                    .expect("new terminal");
                match self.terminal_session.queue_preflight(
                    open.key.clone(),
                    open.request,
                    self.workspace
                        .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
                ) {
                    Ok(()) => {
                        if let Some(reply) = reply {
                            self.debugger_session.register_terminal(id, reply);
                            self.close_debug_terminal();
                            self.debug_terminal = Some(id);
                        }
                        if self.window_session.navigation_current(
                            &open.navigation,
                            &self.state.window_version(),
                            self.state.active_view,
                        ) && self.input.capture_current(&open.capture)
                            && self.input.capture_context().is_none()
                            && self.ui.picker.is_none()
                            && self.ui.tool_menu.is_none()
                        {
                            self.terminal_session.select(id);
                            if open.float {
                                self.float_terminal(id);
                            }
                            // A debugged program's terminal shows, and the user stays on the source.
                            if !for_debugger {
                                self.set_terminal_focus(true);
                            }
                        }
                        self.resize_active_terminal_to_panel();
                        self.sync_terminal_panel();
                        self.sync_explorer_ui();
                        self.ensure_cursor_visible();
                    }
                    Err(error) => {
                        self.terminal_session.reject(&open.key);
                        if let Some(reply) = reply {
                            let _ = reply.respond(false, Some(error.to_string()));
                        }
                        self.show_error(error);
                    }
                }
            }
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }

    pub(in crate::editor::session) fn pending_preflight_count(&self) -> usize {
        self.tasks.preflight_count()
            + self.terminal_session.preflight_count()
            + self.language.preflight_count()
    }
    pub(super) fn process_execution_preflight(&mut self) {
        let events = self.workspace.preflight_events();
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(ready) => {
                    if self
                        .debugger_session
                        .owns_debug_preflight(ready.result().id)
                    {
                        let id = ready.result().id;
                        if let Err(error) = self.debugger_session.debug_preflight(ready) {
                            self.debugger_session.fail_debug_preflight(id);
                            self.show_error(error);
                        }
                        self.advance_debug_launch();
                        continue;
                    }
                    if self.language.owns_preflight(ready.result().id) {
                        match self.language.finish_preflight(
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                            &self.configuration.settings().lsp,
                        ) {
                            Ok(Some(key)) => {
                                for prefix in ["lsp.", "lsp-pull:"] {
                                    if let Some(producer) =
                                        self.language.diagnostic_producer(&key, prefix)
                                    {
                                        self.diagnostics.activate(producer);
                                    }
                                }
                                self.sync_ui_diagnostics();
                            }
                            Ok(None) => {}
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    let result = ready.result();
                    let id = result.id;
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        if let Some(step) = self.tasks.take_preflight(id) {
                            self.tasks.reject_step(&step);
                        }
                        if let Some(key) = self.terminal_session.take_preflight(id) {
                            if let Some(id) = self.terminal_session.resolve(&key) {
                                if let Some(reply) = self.debugger_session.take_terminal(&id) {
                                    let _ = reply
                                        .respond(false, Some("terminal workspace retired".into()));
                                }
                            }
                            self.terminal_session.reject(&key);
                            self.sync_terminal_panel();
                        }
                        continue;
                    }
                    if let Some(step) = self.tasks.take_preflight(id) {
                        match self.tasks.launch(
                            step,
                            ready,
                            &self.configuration.settings().processes,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                        ) {
                            Ok(source) => {
                                if let Some(source) = source {
                                    self.diagnostics.activate(source);
                                    self.sync_ui_diagnostics();
                                }
                            }
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        let id = self.terminal_session.resolve(&key);
                        if id.is_some_and(|id| !self.debugger_session.terminal_current(&id)) {
                            self.terminal_session.reject(&key);
                            if let Some(id) = id {
                                self.debugger_session.take_terminal(&id);
                            }
                            continue;
                        }
                        if let Err(error) =
                            self.terminal_session
                                .launch(key.clone(), ready, &self.workspace)
                        {
                            self.terminal_session.reject(&key);
                            if let Some(reply) =
                                id.and_then(|id| self.debugger_session.take_terminal(&id))
                            {
                                let _ = reply.respond(false, Some(error.clone()));
                            }
                            self.show_error(error);
                        }
                        self.sync_terminal_panel();
                        continue;
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    if self.debugger_session.owns_debug_preflight(id) {
                        self.debugger_session.fail_debug_preflight(id);
                        self.show_error(message);
                        continue;
                    }
                    if self.language.fail_preflight(id) {
                        self.show_error(message);
                        continue;
                    }
                    let mut owned = false;
                    if let Some(step) = self.tasks.take_preflight(id) {
                        self.tasks.reject_step(&step);
                        owned = true;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        if let Some(id) = self.terminal_session.resolve(&key) {
                            if let Some(reply) = self.debugger_session.take_terminal(&id) {
                                let _ = reply.respond(false, Some(message.clone()));
                            }
                        }
                        self.terminal_session.reject(&key);
                        self.sync_terminal_panel();
                        owned = true;
                    }
                    if owned && root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }

    pub(super) fn process_pty_events(&mut self) {
        let windows = &self.window_session;
        let updates = self
            .terminal_session
            .drain(&self.workspace, |id| windows.terminal_geometry_source(id.0));
        if updates.is_empty() {
            return;
        }
        for update in updates {
            match update {
                TerminalUpdate::Started(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        if let Err(error) = reply.respond(true, None) {
                            self.show_error(error);
                        }
                    }
                }
                TerminalUpdate::Exited(id, Some(0)) if self.terminal_session.closes_on_exit(id) => {
                    self.retire_terminal(id, false);
                    self.set_terminal_focus(false);
                }
                // Nothing is left to type to; keys go back to the source.
                TerminalUpdate::Exited(id, _) if self.debug_terminal == Some(id) => {
                    self.set_terminal_focus(false);
                }
                TerminalUpdate::Exited(_, _) | TerminalUpdate::IoComplete(_) => {}
                TerminalUpdate::StopUnknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ =
                            reply.respond(false, Some("terminal stop outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal stop outcome is unknown: {message}"));
                }
                TerminalUpdate::Failed(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some(message.clone()));
                    }
                    self.show_error(message);
                }
                TerminalUpdate::Unknown(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal outcome is unknown".into()));
                    }
                    self.show_error(format!("terminal #{} outcome is unknown: {message}", id.0));
                }
                TerminalUpdate::Closed(id) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some("terminal source retired".into()));
                    }
                    if self.window_session.focused_terminal() == Some(id.0) {
                        self.set_terminal_focus(false);
                    }
                }
                TerminalUpdate::Changed => {}
            }
        }
        self.sync_terminal_panel();
        self.resize_active_terminal_to_panel();
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_completion_detail(
        item: &crate::language::lsp::LspCompletionItem,
        server: &str,
    ) -> String {
        let kind = match item.kind {
            Some(1) => "Text",
            Some(2) => "Method",
            Some(3) => "Function",
            Some(4) => "Constructor",
            Some(5) => "Field",
            Some(6) => "Variable",
            Some(7) => "Class",
            Some(8) => "Interface",
            Some(9) => "Module",
            Some(10) => "Property",
            Some(11) => "Unit",
            Some(12) => "Value",
            Some(13) => "Enum",
            Some(14) => "Keyword",
            Some(15) => "Snippet",
            Some(16) => "Color",
            Some(17) => "File",
            Some(18) => "Reference",
            Some(19) => "Folder",
            Some(20) => "Enum member",
            Some(21) => "Constant",
            Some(22) => "Struct",
            Some(23) => "Event",
            Some(24) => "Operator",
            Some(25) => "Type parameter",
            _ => "Item",
        };
        [kind, item.detail.as_str(), server]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    pub(in crate::editor::session) fn refresh_lsp_completion_documentation(&mut self) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        if self
            .language
            .completion_items()
            .get(selected)
            .is_some_and(|item| item.documentation.is_some())
        {
            self.open_lsp_completion_documentation(false);
        } else if self
            .input
            .documentation()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Completion)
        {
            self.close_documentation();
        }
    }

    pub(in crate::editor::session) fn move_lsp_completion(&mut self, direction: i8) {
        let rows = usize::from(
            self.configuration
                .settings()
                .ui
                .command_line
                .completion_max_rows
                .max(1),
        );
        if let Some((selected, scroll)) = self.language.move_completion(direction, rows) {
            if let Some(completion) = self.ui.language_completion.as_mut() {
                completion.selected = selected;
                completion.scroll = scroll;
            }
        }
        self.refresh_lsp_completion_documentation();
    }

    pub(in crate::editor::session) fn accept_lsp_completion(&mut self) {
        let Some(completion) = &self.ui.language_completion else {
            return;
        };
        let Some(item) = self
            .language
            .completion_items()
            .get(completion.selected)
            .cloned()
        else {
            return;
        };
        let Some((buffer_id, document)) = self.active_lsp_document() else {
            return;
        };
        // The item belongs to the server that offered it: resolve there and read its positions in that server's encoding.
        let server = self
            .language
            .completion_server(completion.selected)
            .map(str::to_owned)
            .or_else(|| self.language.active().cloned())
            .unwrap_or_default();
        if !item.resolved
            && self
                .language
                .sessions()
                .get(&server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && item.resolve_item.is_some()
        {
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            if let Err(error) =
                self.language
                    .resolve_completion(completion.selected, document, offset)
            {
                self.show_error(error);
            }
            return;
        }
        let encoding = self.language.server_encoding(&server);
        let snippet = item.snippet;
        let session_allowed = item.additional_edits.is_empty();
        let offset = self
            .state
            .active_view
            .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
        let buffer = TextBuffer::from_text(&document.text);
        let codec = PositionCodec::new(&buffer);
        let (line, character) = codec.byte_to_lsp(offset, encoding).unwrap_or((0, 0));
        let here = crate::language::lsp::LspPosition { line, character };
        let mut edit = match item.text_edit {
            // The range was the server's at request time; what was typed since goes too.
            Some(mut edit) => {
                if edit.range.end.line == line && edit.range.end.character < character {
                    edit.range.end = here;
                }
                edit
            }
            // No range from the server: replace the word typed so far.
            None => {
                let start = self
                    .language
                    .completion_start()
                    .filter(|start| {
                        *start <= offset
                            && buffer.byte_to_line(*start).is_ok_and(|start| start == line)
                    })
                    .and_then(|start| codec.byte_to_lsp(start, encoding).ok())
                    .map_or(here.clone(), |(line, character)| {
                        crate::language::lsp::LspPosition { line, character }
                    });
                crate::language::lsp::LspTextEdit {
                    range: crate::language::lsp::LspRange { start, end: here },
                    new_text: item.insert_text,
                }
            }
        };
        let mut tabstops = Vec::new();
        if snippet {
            match crate::language::lsp::parse_snippet(&edit.new_text, 1024 * 1024) {
                Ok(parsed) => {
                    edit.new_text = parsed.text;
                    tabstops = parsed.tabstops;
                }
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        }
        let insertion_start = TextBuffer::from_text(&document.text);
        let insertion_start = PositionCodec::new(&insertion_start)
            .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
            .ok();
        let mut edits = vec![edit];
        edits.extend(item.additional_edits);
        let workspace_edit = LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![crate::language::lsp::LspDocumentEdit {
                uri: document.uri(),
                version: Some(document.version),
                edits,
            }],
        };
        if let Err(error) = self.apply_lsp_workspace_edit(workspace_edit, encoding) {
            self.show_error(error);
        } else if session_allowed {
            if let Some(start) = insertion_start {
                if !tabstops.is_empty() {
                    let source = self.state.window_version();
                    self.input.install_snippet(
                        crate::editor::session::input::SnippetDocument {
                            editor: &source,
                            buffer: buffer_id,
                            text: &self.state.buffers[buffer_id].text,
                            view: self.state.active_view,
                        },
                        start,
                        tabstops
                            .into_iter()
                            .map(|stop| (stop.index, stop.start, stop.end)),
                    );
                    self.select_snippet_current();
                }
            }
        }
        self.language.clear_completion();
        self.ui.language_completion = None;
    }

    pub(in crate::editor::session) fn select_snippet_current(&mut self) {
        let source = self.state.window_version();
        let document = self.input.snippet_buffer().and_then(|id| {
            self.state.buffers.get(id).map(|buffer| {
                crate::editor::session::input::SnippetDocument {
                    editor: &source,
                    buffer: id,
                    text: &buffer.text,
                    view: self.state.active_view.filter(|view| {
                        self.state
                            .views
                            .get(*view)
                            .is_some_and(|view| view.buffer == id)
                    }),
                }
            })
        });
        let Some(operations) = self.input.snippet_selection(document) else {
            return;
        };
        let mut transaction =
            EditorTransaction::new(self.transaction_id()).with_undo_policy(UndoPolicy::Ignore);
        transaction.operations.extend(operations);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
    pub(in crate::editor::session) fn navigate_snippet(&mut self, direction: i8) {
        self.input.navigate_snippet(direction);
        self.select_snippet_current();
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn lsp_position_encoding(&self) -> PositionEncoding {
        self.language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |c| c.position_encoding)
    }

    pub(in crate::editor::session) fn clear_git_status(&mut self) {
        self.git.clear();
        self.ui.git_signs.clear();
    }

    pub(in crate::editor::session) fn reconcile_git_signs(&mut self) {
        if self.git.status().is_none()
            && self
                .state
                .active_buffer_id()
                .and_then(|id| self.state.buffers[id].path.as_ref())
                .is_some()
        {
            self.start_git_status_refresh_silent();
        }
        if self.git.reconcile_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.ui.git_signs.clear();
        }
    }

    pub(in crate::editor::session) fn process_git_sign_events(&mut self) {
        if let Some(signs) = self.git.poll_signs(
            &self.state,
            self.workspace.root_revision(),
            self.configuration.settings().editor.git_signs.enabled,
        ) {
            self.ui.git_signs = signs;
        }
    }

    pub(in crate::editor::session) fn start_git_hunks(&mut self) {
        self.start_git_hunks_with_action(None);
    }
    pub(in crate::editor::session) fn start_git_hunks_with_action(
        &mut self,
        action: Option<GitHunkAction>,
    ) {
        self.start_git_hunks_with_layer(action, crate::workspace::git::GitDiffLayer::Unstaged);
    }
    pub(in crate::editor::session) fn start_git_hunks_with_layer(
        &mut self,
        action: Option<GitHunkAction>,
        layer: crate::workspace::git::GitDiffLayer,
    ) {
        let path = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.as_deref());
        if let Err(error) = self
            .git
            .start_hunks(path, self.workspace.root_revision(), action, layer)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn start_git_worktrees(&mut self) {
        let Some(status) = self.git.status().cloned() else {
            if self.git.status_pending() {
                self.git.show_worktrees_after_change();
            } else {
                self.show_error("Git status is not ready; is this a Git repository?");
            }
            return;
        };
        if let Err(error) = self.git.start_branches(&status.repository) {
            self.show_error(error);
        }
    }
    /// Remove the selected worktree and its directory. This never deletes a branch.
    pub(in crate::editor::session) fn request_git_worktree_remove(&mut self) {
        let Some(crate::editor::session::search::PickerPayload::GitWorktree { name, .. }) =
            self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a worktree first");
            return;
        };
        self.git.set_pending_destructive(
            crate::editor::session::git::GitDestructiveAction::RemoveWorktree(name.clone()),
        );
        self.open_confirmation(
            PromptKind::GitDestructive,
            "Remove worktree".into(),
            format!("Remove worktree `{name}` and its directory? No branch is deleted."),
            crate::ui::ConfirmationChoice::No,
        );
    }
    fn git_repository(&mut self) -> Option<std::path::PathBuf> {
        match self.git.status() {
            Some(status) => Some(status.repository.clone()),
            None => {
                self.show_error("Git status is not ready; is this a Git repository?");
                None
            }
        }
    }
    fn report_branch_request(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn create_git_worktree(
        &mut self,
        branch: String,
        path: Option<std::path::PathBuf>,
    ) {
        let Some(repository) = self.git_repository() else {
            return;
        };
        // Git names a worktree with one path segment; branches may have several.
        let name = branch.replace('/', "-");
        let path = path.unwrap_or_else(|| std::path::Path::new(".worktree").join(&name));
        let result = self
            .git
            .start_worktree_create(&repository, &name, &path, &branch);
        self.git.show_worktrees_after_change();
        self.report_branch_request(result);
    }
    fn confirm_git_destructive(&mut self, action: Option<crate::editor::session::git::GitDestructiveAction>) {
        let Some(action) = action else {
            return;
        };
        let Some(status) = self.git.status().cloned() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let result = match action {
            crate::editor::session::git::GitDestructiveAction::RemoveWorktree(name) => {
                self.git.show_worktrees_after_change();
                self.git.start_worktree_remove(&status.repository, &name)
            }
        };
        if let Err(error) = result {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_branch_events(&mut self) {
        // One poll per handler call: the service is one shot.
        let outcome = match self.git.poll_branch_outcome() {
            Ok(Some(outcome)) => Some(outcome),
            Ok(None) => None,
            Err(error) => {
                self.show_error(error);
                None
            }
        };
        let list = match outcome {
            Some(crate::editor::session::git::GitBranchOutcome::List(list)) => list,
            Some(crate::editor::session::git::GitBranchOutcome::Changed) => {
                self.start_git_status_refresh();
                self.notify_message("Git branch or worktree updated".into(), false);
                if self.git.take_show_worktrees() {
                    self.start_git_worktrees();
                }
                return;
            }
            None => return,
        };
        let new_worktree = (
            PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: PickerItemId("git-worktree-new".into()),
                label: "New worktree…".into(),
                detail: "check out a branch in its own directory".into(),
                source: String::new(),
                action: PickerAction::None,
            },
            crate::editor::session::search::PickerPayload::Command(
                "workspace.git.worktree.create".into(),
            ),
        );
        let items = std::iter::once(new_worktree)
            .chain(list.worktrees.iter().map(|worktree| {
                let mut detail = match &worktree.branch {
                    Some(branch) => format!("branch {branch}"),
                    None => "detached".to_string(),
                };
                if worktree.main {
                    detail.push_str(" · main");
                }
                if worktree.dirty {
                    detail.push_str(" · dirty");
                }
                if let Some(operation) = &worktree.operation {
                    detail.push_str(&format!(" · {operation} in progress"));
                }
                if worktree.locked {
                    detail.push_str(" · locked");
                }
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("git-worktree:{}", worktree.name)),
                        label: worktree.name.clone(),
                        detail,
                        source: "worktrees".into(),
                        action: PickerAction::None,
                    },
                    crate::editor::session::search::PickerPayload::GitWorktree {
                        name: worktree.name.clone(),
                        path: worktree.path.clone(),
                    },
                )
            }))
            .collect::<Vec<_>>();
        let title = format!("Git worktrees · {}", list.worktrees.len());
        self.install_picker(title, items);
        self.search
            .set_picker_footer("Enter Switch · ctrl-x Remove · Esc Esc Close".to_owned());
        self.sync_picker_ui();
    }
    pub(in crate::editor::session) fn unstage_selected_git_hunk(&mut self) {
        if let Err(error) = self.git.start_plan(
            &self.state,
            crate::workspace::git::GitMutationKind::UnstageHunk,
            self.workspace.root_revision(),
            self.workspace.trust() == WorkspaceTrust::Trusted,
            Vec::new(),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_hunk_events(&mut self) {
        match self.git.poll_hunks() {
            Ok(Some(ready)) => {
                if let Some(action) = ready.action {
                    match action {
                        GitHunkAction::Move(direction) => self.navigate_git_hunk(direction),
                        GitHunkAction::Preview => self.preview_selected_git_hunk(),
                        GitHunkAction::Stage => self.stage_selected_git_hunk(),
                        GitHunkAction::Reset => self.request_reset_selected_git_hunk(),
                    }
                    return;
                }
                let Some(snapshot) = self.git.hunks() else {
                    return;
                };
                let items = snapshot
                    .hunks
                    .iter()
                    .map(|hunk| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("git-hunk:{}", hunk.id.0)),
                                label: hunk.header.trim().to_owned(),
                                // The layer is part of the identity: a staged hunk must never
                                // be presented as an unstaged one.
                                detail: format!(
                                    "-{},{} +{},{} · {}",
                                    hunk.old_start,
                                    hunk.old_lines,
                                    hunk.new_start,
                                    hunk.new_lines,
                                    snapshot.source().source_label()
                                ),
                                source: "git-hunk".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::GitHunk {
                                path: ready.path.clone(),
                                status_revision: snapshot.status_revision,
                                repository: snapshot.repository_id,
                                hunk: hunk.id,
                                new_line: hunk
                                    .change_lines()
                                    .first()
                                    .map_or(hunk.new_start, |line| *line)
                                    .saturating_sub(1)
                                    as usize,
                            },
                        )
                    })
                    .collect();
                self.install_picker("Git hunks".into(), items);
            }
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
    }
    /// Conflict markers are in the text itself, so this works in any file, staged or not.
    fn navigate_git_conflict(&mut self, direction: i8) {
        let Some(view) = self.state.active_view.map(|id| &self.state.views[id]) else {
            return;
        };
        let buffer = &self.state.buffers[view.buffer];
        let current = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let source = buffer.text.to_string();
        let markers: Vec<usize> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with("<<<<<<< "))
            .map(|(index, _)| index)
            .collect();
        let target = if direction > 0 {
            markers.iter().find(|line| **line > current).or(markers.first())
        } else {
            markers.iter().rev().find(|line| **line < current).or(markers.last())
        };
        match (target, buffer.path.clone()) {
            (Some(line), Some(path)) => self.navigate_matcher_source(MatcherSource {
                path,
                line: *line,
                column: 0,
            }),
            _ => self.notify_message(self.message("conflict_none", &[]), false),
        }
    }
    pub(in crate::editor::session) fn navigate_git_hunk(&mut self, direction: i8) {
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(GitHunkAction::Move(direction.signum())));
            return;
        }
        match self.git.navigate(direction, self.cursor_line_number()) {
            Ok((path, line)) => self.navigate_matcher_source(MatcherSource {
                path,
                line,
                column: 0,
            }),
            Err(error) => self.show_error(error),
        }
    }
    /// The one-based line under the cursor in the active window.
    fn cursor_line_number(&self) -> u32 {
        self.state
            .active_view
            .and_then(|id| {
                let view = &self.state.views[id];
                self.state.buffers[view.buffer]
                    .text
                    .byte_to_line(view.cursor)
                    .ok()
            })
            .unwrap_or(0) as u32
            + 1
    }
    pub(in crate::editor::session) fn preview_selected_git_hunk(&mut self) {
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(GitHunkAction::Preview));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        match self.git.preview() {
            Ok((header, text)) => self.present_documentation_popup(
                DocumentationPopupKind::Git,
                header,
                "Git".into(),
                None,
                vec![LspMarkupContent {
                    kind: LspMarkupKind::PlainText,
                    value: text,
                }],
            ),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn request_reset_selected_git_hunk(&mut self) {
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(GitHunkAction::Reset));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        if !self.git.has_selection() {
            self.show_error("select a current Git hunk first");
            return;
        }
        self.open_prompt(PromptKind::GitReset, String::new());
        self.prompt_help("Discards worktree changes. Type `reset this hunk`.".into());
    }
    pub(in crate::editor::session) fn confirm_reset_selected_git_hunk(&mut self, phrase: String) {
        if phrase != "reset this hunk" {
            self.show_error("Git hunk reset confirmation did not match");
            return;
        }
        if let Err(error) = self.git.mutate(
            &self.state,
            true,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn stage_selected_git_hunk(&mut self) {
        if self.git.hunks().is_none() {
            self.start_git_hunks_with_action(Some(GitHunkAction::Stage));
            return;
        }
        self.git.select_at(self.cursor_line_number());
        if let Err(error) = self.git.mutate(
            &self.state,
            false,
            self.workspace.trust() == WorkspaceTrust::Trusted,
            self.workspace.root_revision(),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_mutation_events(&mut self) {
        match self.git.poll_mutation() {
            Ok(Some(crate::editor::session::git::GitMutationOutcome::Reset)) => {
                self.input.clear_pending_stage();
                self.ui.git_signs.clear();
                self.start_git_status_refresh();
                self.notify_message("Git hunk reset".into(), false);
            }
            Ok(Some(crate::editor::session::git::GitMutationOutcome::Staged(message))) => {
                self.input.clear_pending_stage();
                self.ui.git_signs.clear();
                self.start_git_status_refresh();
                self.notify_message(message.into(), false);
            }
            Ok(Some(crate::editor::session::git::GitMutationOutcome::UnsavedDocument)) => {
                // A refusal, not a failure: the plan never wrote the index, so offer the
                // explicit choice instead of reporting an error the user cannot act on.
                let path = self
                    .git
                    .unsaved_refusal()
                    .map_or_else(|| "the path".into(), |path| path.display().to_string());
                self.open_confirmation(
                    PromptKind::GitStageUnsaved,
                    "Stage unsaved changes".into(),
                    format!(
                        "{path} has unsaved changes, so the index would record the version on disk rather than the version in the buffer. Save the buffer, then stage the saved revision?"
                    ),
                    crate::ui::ConfirmationChoice::No,
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.input.clear_pending_stage();
                self.show_error(error);
            }
        }
    }
    /// Answer the save-and-stage choice. The staging effect is replayed only after the save
    /// completes, so the plan is rebuilt against the saved bytes.
    pub(in crate::editor::session) fn confirm_stage_unsaved(&mut self) {
        self.start_save(false, None, false);
    }
    pub(in crate::editor::session) fn start_git_status_refresh(&mut self) {
        self.start_git_status_refresh_with_feedback(true);
    }
    pub(in crate::editor::session) fn start_git_status_refresh_silent(&mut self) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        if self.git.needs_status(self.workspace.root_revision(), root) {
            self.start_git_status_refresh_with_feedback(false);
        }
    }
    pub(in crate::editor::session) fn start_git_status_refresh_with_feedback(
        &mut self,
        report_errors: bool,
    ) {
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| r.identity.canonical_path.as_path());
        self.ui.git_signs.clear();
        if let Err(error) = self
            .git
            .refresh(
                &self.state,
                root,
                self.workspace.root_revision(),
                report_errors,
            )
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_git_status_events(&mut self) {
        match self.git.poll_status(self.workspace.root_revision()) {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => self.show_error(error),
        }
        if self.git.status().is_none() {
            self.ui.git_signs.clear();
            self.git.open_changes_when_ready = false;
        } else if self.git.open_changes_when_ready {
            self.open_git_status_picker();
        } else if self.git.take_show_worktrees() {
            self.start_git_worktrees();
        }
        if self.ui.explorer.is_some() {
            self.sync_explorer_ui();
        }
        // A refreshed status is the bounded moment to re-read the operation bar, so a paused
        // operation appears and disappears with the repository rather than with a guess.
        self.project_operation_bar();
        self.project_debug_panes();
    }
    pub(in crate::editor::session) fn open_git_status_picker(&mut self) {
        self.git.open_changes_when_ready = false;
        let root = self
            .workspace
            .active_root()
            .and_then(|id| self.workspace.roots().iter().find(|r| r.id == id))
            .map(|r| &r.identity.canonical_path);
        let Some(snapshot) = self.git.status().filter(|s| {
            s.root_revision == self.workspace.root_revision() && root == Some(&s.scope_root)
        }) else {
            // Status loads in the background; open the picker as soon as it arrives.
            self.git.open_changes_when_ready = true;
            self.start_git_status_refresh();
            return;
        };
        // The three sections, in the order the surface draws them. A conflicted path is listed
        // only under Conflicts, so it never appears twice.
        let conflicted: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| entry.status == crate::workspace::git::GitFileStatus::Conflicted)
            .collect();
        // The Unstaged section holds what is not staged. A path whose buffer has unsaved
        // edits but nothing on disk is listed here too, because the document says such a path
        // shows a badge and a path that is not listed cannot show one; its OWN provenance
        // says `buffer -> disk`, so it is never read as an unstaged change.
        let unstaged: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| {
                entry.status != crate::workspace::git::GitFileStatus::Conflicted
                    && (entry.sources.unstaged
                        || entry.sources.unsaved_document_revision.is_some())
            })
            .collect();
        let staged: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| entry.status != crate::workspace::git::GitFileStatus::Conflicted && entry.sources.staged)
            .collect();
        let total = conflicted.len() + unstaged.len() + staged.len();
        let mut items: Vec<(PickerItem, PickerPayload)> = Vec::new();
        for (title, entries, provenance) in [
            ("Conflicts", &conflicted, "both changed"),
            ("Unstaged", &unstaged, "index -> disk"),
            ("Staged", &staged, "HEAD -> index"),
        ] {
            if entries.is_empty() {
                continue;
            }
            items.push((
                PickerItem {
                    // Each header needs its own id: duplicate ids are rejected, so two
                    // headers sharing one would silently drop the second section.
                    id: PickerItemId(format!("section:{title}")),
                    label: format!("{title} {}", entries.len()),
                    detail: String::new(),
                    source: String::new(),
                    badge: String::new(),
                    action: PickerAction::None,
                    kind: crate::workspace::PickerItemKind::Section,
                },
                PickerPayload::Section,
            ));
            for entry in entries {
                items.push((
                    PickerItem {
                        // The id names the section as well, because one path can be both
                        // staged and further modified and the two rows are different changes.
                        id: PickerItemId(format!(
                            "git:{title}:{}",
                            entry.relative_path.display()
                        )),
                        label: entry.relative_path.display().to_string(),
                        // The provenance column names the comparison this row belongs to, so
                        // an unstaged row is never read as unsaved and an unsaved buffer is
                        // never read as an unstaged change.
                        detail: if title == "Unstaged" && !entry.sources.unstaged {
                            "buffer -> disk".to_owned()
                        } else {
                            provenance.to_owned()
                        },
                        // The status column carries the code: the glyph that keeps the meaning
                        // readable when colour is unavailable.
                        source: Self::change_code(entry),
                        // A path whose buffer has unsaved edits carries a badge, and staging it
                        // is blocked with an explicit choice rather than silently saving.
                        badge: if entry.sources.unsaved_document_revision.is_some() {
                            "unsaved".to_owned()
                        } else {
                            String::new()
                        },
                        action: PickerAction::None,
                        kind: crate::workspace::PickerItemKind::Entry,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: snapshot.scope_root.join(&entry.relative_path),
                        line: 0,
                        column: 0,
                    }),
                ));
            }
        }
        // Re-listing must not move the cursor: the row the user was on is remembered by its
        // identity and restored, so a refresh never reorders the list under them.
        let previous = self.search.picker_bookmark();
        // The title names the branch the changes are against, as the document draws it. A
        // detached HEAD has no branch name, so the segment is simply absent rather than
        // filled with something invented.
        let branch = crate::workspace::git::git_branch_name(&snapshot.repository)
            .ok()
            .flatten();
        let title = match branch {
            Some(branch) => format!("Changes · {branch} ±{total}"),
            None => format!("Changes · ±{total}"),
        };
        self.install_surface_picker(PickerSurfaceKind::Changes, title, items);
        self.search.set_picker_footer(
            "s Stage file · u Unstage file · Tab Switch diff · Enter Open · Esc Esc Close".to_owned(),
        );
        self.search.restore_picker_view(previous, false);
        self.sync_picker_ui();
    }

    /// Open the current line on the forge, or copy its URL, as LazyVim's `<leader>gB`/`gY`.
    fn browse_git_line(&mut self, copy: bool) {
        let file = self.state.active_view.and_then(|view| {
            let view = &self.state.views[view];
            let buffer = &self.state.buffers[view.buffer];
            let line = buffer.text.byte_to_line(view.cursor).map_or(1, |line| line + 1);
            buffer.path.clone().map(|path| (path, line))
        });
        let Some((path, line)) = file else {
            self.show_error("this needs a file buffer");
            return;
        };
        let url = match crate::workspace::git::git_browse_url(&path, line) {
            Ok(url) => url,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if copy {
            let policy = self.state.policy;
            self.state.registers.set(
                '+',
                RegisterValue {
                    text: url.clone(),
                    shape: RegisterShape::Character,
                },
                policy.registers_enabled,
                true,
            );
            self.notify_message(format!("copied {url}"), false);
            return;
        }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        match std::process::Command::new(opener)
            .arg(&url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notify_message(format!("opened {url}"), false);
            }
            Err(error) => self.show_error(format!("{opener}: {error}")),
        }
    }

    /// The two-character index/worktree code a change row shows. Unmerged and untracked have
    /// their own codes; everything else names which side differs.
    fn change_code(entry: &crate::workspace::git::GitStatusEntry) -> String {
        use crate::workspace::git::GitFileStatus;
        if entry.status == crate::workspace::git::GitFileStatus::Conflicted {
            return "UU".to_owned();
        }
        if entry.status == GitFileStatus::Untracked {
            return "??".to_owned();
        }
        let letter = match entry.status {
            GitFileStatus::Added => 'A',
            GitFileStatus::Modified => 'M',
            GitFileStatus::Deleted => 'D',
            GitFileStatus::Renamed => 'R',
            GitFileStatus::Conflicted | GitFileStatus::Untracked => unreachable!(),
        };
        let index = if entry.sources.staged { letter } else { ' ' };
        let worktree = if entry.sources.unstaged { letter } else { ' ' };
        format!("{index}{worktree}")
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn navigate_workspace_search_match(
        &mut self,
        found: WorkspaceSearchMatch,
    ) {
        if self
            .state
            .buffers
            .iter()
            .any(|(_, buffer)| buffer.path.as_ref() == Some(&found.canonical_path))
        {
            if let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            {
                self.navigate_byte_location(&file_uri(&found.canonical_path), range);
            }
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.documents.read_scoped(found.canonical_path, roots) {
            Ok(id) => {
                self.search.register_workspace_navigation(id, found.range);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_byte_location(
        &mut self,
        uri: &str,
        range: ByteRange,
    ) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer, _)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        let transaction = self.transaction_id();
        match self
            .state
            .prepare_document_location(buffer, range.start, transaction)
            .and_then(|draft| self.commit_editor_draft(draft, None, false))
        {
            Ok(()) => self.ensure_cursor_visible(),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_lsp_location(
        &mut self,
        location: &crate::language::lsp::LspLocation,
    ) {
        let Some(path) = path_from_file_uri(&location.uri) else {
            self.show_error(self.message("lsp_invalid_uri", &[("uri", location.uri.clone())]));
            return;
        };
        self.record_jump();
        let encoding = self
            .language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            let mut roots: Vec<_> = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            // A definition in a library or the toolchain opens too, read-only: the reader may
            // look at it, and nothing here can write it.
            let mut path = path;
            if self.workspace.require_path(&path).is_err() {
                if let Ok(real) = path.canonicalize() {
                    path = real;
                }
                roots.extend(path.parent().map(std::path::Path::to_owned));
            }
            match self.documents.read_scoped(path, roots) {
                Ok(id) => {
                    self.language
                        .register_navigation_read(id, location.range.clone(), encoding);
                    self.notify_message(self.message("opening_file", &[]), false);
                }
                Err(error) => self.show_error(error),
            }
            return;
        };
        let Ok(position) = PositionCodec::new(&buffer.text).lsp_to_byte(
            location.range.start.line,
            location.range.start.character,
            encoding,
        ) else {
            self.show_error(self.message("lsp_invalid_position", &[]));
            return;
        };
        if let Ok(view) = self.state.show_buffer(buffer_id) {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_precondition(Precondition::ViewDisplays {
                    view,
                    buffer: buffer_id,
                })
                .with_operation(EditorOperation::SetCursor { view, position })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            } else {
                self.ensure_cursor_visible();
            }
        }
    }

    pub(in crate::editor::session) fn current_persisted_location(
        &self,
    ) -> Option<PersistedLocation> {
        let view = self.state.views.get(self.state.active_view?)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        Some(PersistedLocation {
            path: buffer.path.clone()?,
            byte: view.cursor.get(),
            anchor: view
                .selection
                .map_or(view.cursor.get(), |selection| selection.anchor.get()),
            scroll_line: view.scroll_line,
            scroll_column: view.scroll_column,
        })
    }

    /// Remember where the cursor is before it jumps, for `ctrl-o`.
    pub(in crate::editor::session) fn record_jump(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            self.persistence.record_location(
                location,
                false,
                self.configuration.settings().persistence.jumps,
            );
        }
    }

    /// `ctrl-o` (back) and `ctrl-i` (forward) through the jump list.
    pub(in crate::editor::session) fn walk_jumps(&mut self, back: bool) {
        let current = self.current_persisted_location();
        let maximum = self.configuration.settings().persistence.jumps;
        if let Some(location) = self.persistence.walk_jumps(current, back, maximum) {
            self.go_to_persisted_location(location);
        }
    }

    pub(in crate::editor::session) fn push_change_location(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            self.persistence.record_location(
                location,
                true,
                self.configuration.settings().persistence.changes,
            );
        }
    }

    pub(in crate::editor::session) fn navigate_matcher_source(&mut self, mut source: MatcherSource) {
        if !source.path.is_absolute() {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        let mut roots: Vec<_> = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        // A debugger frame in a library or the toolchain opens too, read-only, as definitions do.
        if !roots.iter().any(|root| source.path.starts_with(root)) {
            if let Ok(real) = source.path.canonicalize() {
                source.path = real;
            }
            roots.extend(source.path.parent().map(std::path::Path::to_owned));
        }
        match self.documents.read_scoped(source.path.clone(), roots) {
            Ok(id) => {
                self.search.register_matcher_navigation(id, source);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_persisted_location(
        &mut self,
        location: PersistedLocation,
    ) {
        if !location.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| location.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        self.record_jump();
        self.go_to_persisted_location(location);
    }

    fn go_to_persisted_location(&mut self, location: PersistedLocation) {
        let mut roots: Vec<_> = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        // An open file was let in already, like the tutor's practice file outside the project.
        if self.state.buffers.values().any(|buffer| buffer.path.as_ref() == Some(&location.path)) {
            roots.extend(location.path.parent().map(Path::to_path_buf));
        }
        match self.documents.read_scoped(location.path.clone(), roots) {
            Ok(id) => {
                self.persistence.register_navigation_read(id, location);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn navigate_diagnostic(
        &mut self,
        direction: i8,
        filter: DiagnosticFilter,
    ) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(path) = self.state.buffers[buffer_id].path.clone() else {
            return;
        };
        let uri = crate::language::lsp::file_uri(&path);
        self.diagnostics.retire_documents(&self.state);
        let Some(key) = self
            .diagnostics
            .adjacent(&uri, cursor, direction, filter.severities())
        else {
            self.show_error(self.message("diagnostic_none", &[]));
            return;
        };
        let Some(target) = self.diagnostics.resolve(&key, &self.state) else {
            self.sync_ui_diagnostics();
            return;
        };
        let position = target.range.start;
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        let result = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())
            .and_then(|draft| self.commit_editor_draft(draft, None, false));
        if let Err(error) = result {
            self.show_error(error);
        } else {
            self.diagnostics
                .expand(key, filter.severities(), &self.state);
            self.sync_ui_diagnostics();
            self.clear_notice();
            self.ensure_cursor_visible();
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn install_picker(
        &mut self,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker(PickerSurfaceKind::Generic, title, items);
    }

    /// Show a help page without losing the surface the user was looking at. Help replaces the
    /// picker while it is open, and closing it returns the suspended surface with its query,
    /// its rows and its selected row exactly as they were.
    fn install_help_page(&mut self, title: String, items: Vec<(PickerItem, PickerPayload)>) {
        self.search.suspend_for_help();
        self.install_surface_picker(PickerSurfaceKind::Generic, title, items);
        self.search.mark_help_open();
    }

    pub(in crate::editor::session) fn install_surface_picker(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker_state(
            surface,
            title,
            items,
            String::new(),
            true,
            PickerOrigin::General,
        );
    }

    pub(in crate::editor::session) fn install_surface_picker_state(
        &mut self,
        surface: PickerSurfaceKind,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
        query: String,
        complete: bool,
        origin: PickerOrigin,
    ) {
        if let PickerOrigin::PluginProvider(request) = origin {
            if !self.extension.provider_presentable(request) {
                self.show_error("plugin picker source retired");
                return;
            }
        }
        let retired_preview = self
            .search
            .workspace_picker()
            .filter(|id| !matches!(origin, PickerOrigin::WorkspaceEdit(current) if current == *id));
        let previous_epoch = self.search.picker_epoch().cloned();
        let epoch = match self.search.install_picker(
            PickerContent {
                surface,
                origin,
                title,
                items,
                query,
                complete,
            },
            self.configuration.settings().workspace.maximum_picker_items,
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if self.search.theme_picker_key().is_none() {
            self.configuration.cancel_theme_picker();
        }
        self.extension.picker_installed(match origin {
            PickerOrigin::PluginProvider(request) => Some(request),
            _ => None,
        });
        if !previous_epoch.is_some_and(|epoch| self.search.owns_picker_epoch(&epoch)) {
            self.input.begin_picker();
        }
        // A refused replacement must not destroy the still-valid old preview.
        if let Some(id) = retired_preview {
            self.retire_workspace_picker(id);
        }
        self.language
            .retain_actions(self.search.picker_code_actions(&epoch));
        self.input.cancel_prompt_completions();
        self.clear_picker_preview();
        self.sync_picker_ui();
    }

    pub(in crate::editor::session) fn retire_stale_picker(&mut self) {
        if self.search.picker_is_stale(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            self.close_picker();
        }
    }

    pub(in crate::editor::session) fn selected_picker_payload(&self) -> Option<&PickerPayload> {
        self.search.selected_picker_payload()
    }

    pub(in crate::editor::session) fn open_lsp_locations_picker(
        &mut self,
        locations: Vec<crate::language::lsp::LspLocation>,
    ) {
        let mut files = std::collections::HashMap::new();
        let items = locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| {
                let line = location.range.start.line as usize;
                let path = path_from_file_uri(&location.uri);
                let code = path
                    .as_deref()
                    .map(|path| self.source_line_at(path, line, &mut files))
                    .unwrap_or_default();
                let path = path.map_or_else(
                    || location.uri.clone(),
                    |path| self.workspace_relative_label(&path),
                );
                let id = PickerItemId(format!("location:{index}:{}", location.uri));
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: format!("{path}:{}", line + 1),
                        detail: code,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspLocation(location),
                )
            })
            .collect();
        self.install_picker(self.message("picker_references_title", &[]), items);
    }

    /// The line a location points at, from its open buffer or a file small enough to read here.
    fn source_line_at(
        &self,
        path: &std::path::Path,
        line: usize,
        files: &mut std::collections::HashMap<std::path::PathBuf, Option<String>>,
    ) -> String {
        const MAXIMUM_FILE: u64 = 1024 * 1024;
        if let Some((_, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_deref() == Some(path))
        {
            return buffer.text.line(line).map_or_else(|_| String::new(), |line| line.trim().to_owned());
        }
        let contents = files.entry(path.to_owned()).or_insert_with(|| {
            std::fs::metadata(path)
                .is_ok_and(|metadata| metadata.len() <= MAXIMUM_FILE)
                .then(|| std::fs::read_to_string(path).ok())
                .flatten()
        });
        contents
            .as_deref()
            .and_then(|contents| contents.lines().nth(line))
            .map_or_else(String::new, |line| line.trim().to_owned())
    }

    pub(in crate::editor::session) fn open_lsp_symbols_picker(
        &mut self,
        server: &str,
        symbols: Vec<crate::language::lsp::LspSymbol>,
    ) {
        let server = self.lsp_server_label(server);
        let items = symbols
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| {
                let id = PickerItemId(format!("symbol:{index}:{}", symbol.name));
                let payload = symbol.location.map_or_else(
                    || PickerPayload::Command(String::new()),
                    PickerPayload::LspLocation,
                );
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: symbol.name,
                        detail: symbol.kind.map_or_else(String::new, |kind| {
                            self.message(
                                "picker_symbol_kind",
                                &[("kind", crate::language::lsp::lsp_symbol_kind_name(kind).to_owned())],
                            )
                        }),
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    payload,
                )
            })
            .collect();
        self.install_picker(self.message("picker_symbols_title", &[]), items);
    }

    pub(in crate::editor::session) fn open_lsp_code_actions_picker(
        &mut self,
        server: &str,
        actions: Vec<crate::language::lsp::LspCodeAction>,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        let choices = match self.language.install_actions(
            actions,
            context,
            self.configuration.settings().workspace.maximum_picker_items,
        ) {
            Ok(choices) => choices,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let server = self.lsp_server_label(server);
        let items = choices
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("code-action:{index}:{}", choice.title)),
                        label: choice.title,
                        detail: choice.detail,
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::CodeAction(choice.key),
                )
            })
            .collect();
        self.install_picker(self.message("picker_code_actions_title", &[]), items);
    }

    pub(in crate::editor::session) fn open_dap_picker(
        &mut self,
        kind: &str,
        rows: Vec<(String, String, DapControl)>,
    ) {
        let rows = self.debugger_session.controls(rows);
        self.install_debug_rows(kind, rows);
    }
    pub(in crate::editor::session) fn install_debug_rows(
        &mut self,
        kind: &str,
        rows: Vec<crate::editor::session::debugger::DebugRow>,
    ) {
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("dap:{kind}:{index}")),
                        label: row.label,
                        detail: row.detail,
                        source: "dap".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Debug(row.key),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Debug,
            self.message("dap_picker_title", &[("kind", kind.into())]),
            items,
        );
    }

    pub(in crate::editor::session) fn open_builtin_picker(&mut self, kind: PickerKind) {
        match kind {
            PickerKind::Diagnostics => {
                let items = self
                    .diagnostics
                    .choices()
                    .enumerate()
                    .map(|(index, (key, problem, validity))| {
                        let id = PickerItemId(format!("diagnostic:{index}:{}", problem.uri));
                        let path = path_from_file_uri(&problem.uri)
                            .map_or_else(|| problem.uri.clone(), |path| path.display().to_string());
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: problem.message.clone(),
                                detail: format!("{path} · {}", validity.label()),
                                source: problem.source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Diagnostic(key),
                        )
                    })
                    .collect();
                self.install_surface_picker(
                    PickerSurfaceKind::Problems,
                    self.message("picker_diagnostics_title", &[]),
                    items,
                );
            }
            PickerKind::Buffers => {
                let items = self
                    .state
                    .buffers
                    .iter()
                    .enumerate()
                    .map(|(index, (buffer, state))| {
                        let id = PickerItemId(format!("buffer:{index}"));
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id,
                                label: state.path.as_ref().map_or_else(
                                    || {
                                        self.configuration
                                            .settings()
                                            .ui
                                            .status_line
                                            .scratch_label
                                            .clone()
                                    },
                                    |path| self.workspace_relative_label(path),
                                ),
                                detail: if state.dirty {
                                    self.configuration
                                        .settings()
                                        .ui
                                        .status_line
                                        .dirty_marker
                                        .clone()
                                } else {
                                    String::new()
                                },
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Buffer(buffer),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_buffers_title", &[]), items);
            }
            PickerKind::Roots => {
                // Every workspace Dust has opened on this machine; choosing one moves the
                // editor there like a worktree switch.
                let current = self.state.workspace_root.clone();
                let mut projects = self
                    .workspace
                    .seen_roots()
                    .into_iter()
                    .chain(self.workspace.roots().iter().map(|root| root.identity.canonical_path.clone()))
                    .filter(|path| path.is_dir())
                    .collect::<Vec<_>>();
                projects.sort();
                projects.dedup();
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let items = projects
                    .into_iter()
                    .map(|path| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        let shown = match home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
                            Some(rest) => format!("~/{}", rest.display()),
                            None => path.display().to_string(),
                        };
                        (
                            PickerItem {
                                badge: if path == current { "current".into() } else { String::new() },
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("project:{}", path.display())),
                                label: name,
                                detail: shown,
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Project(path),
                        )
                    })
                    .collect();
                self.install_picker("Projects".to_owned(), items);
                self.search
                    .set_picker_footer("Enter Switch · Esc Esc Close".to_owned());
                self.sync_picker_ui();
            }
            PickerKind::Recents => {
                let items = self
                    .persistence
                    .recent_files()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, path)| {
                        let location = PersistedLocation {
                            path: path.clone(),
                            byte: 0,
                            anchor: 0,
                            scroll_line: 0,
                            scroll_column: 0,
                        };
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("recent:{index}")),
                                label: self.workspace_relative_label(&path),
                                detail: String::new(),
                                source: self.message("navigation_recent_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_recents_title", &[]), items);
            }
            PickerKind::Marks => {
                let items = self
                    .persistence
                    .marks()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, mark)| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("mark:{index}:{}", mark.name)),
                                label: mark.name,
                                detail: format!(
                                    "{}:{}",
                                    self.workspace_relative_label(&mark.location.path),
                                    mark.location.byte
                                ),
                                source: self.message("navigation_mark_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(mark.location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_marks_title", &[]), items);
            }
            PickerKind::Jumps | PickerKind::Changes => {
                let (source, title, rows) = if matches!(kind, PickerKind::Jumps) {
                    (
                        self.message("navigation_jump_source", &[]),
                        self.message("navigation_jumps_title", &[]),
                        self.persistence.jumps().to_vec(),
                    )
                } else {
                    (
                        self.message("navigation_change_source", &[]),
                        self.message("navigation_changes_title", &[]),
                        self.persistence.changes().to_vec(),
                    )
                };
                let items = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, location)| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("navigation:{index}")),
                                label: self.workspace_relative_label(&location.path),
                                detail: location.byte.to_string(),
                                source: source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(title, items);
            }
            PickerKind::Commands => self.open_command_palette(),
        }
    }

    pub(in crate::editor::session) fn sync_picker_ui(&mut self) {
        self.retire_stale_picker();
        self.ui.picker = self.search.picker_projection();
        let theme = match self.search.selected_picker_payload() {
            Some(PickerPayload::Theme(key)) => Some(key),
            _ => None,
        };
        self.configuration.preview_theme(theme);
        let context = self.active_input_context();
        let theme_picker = self.search.theme_picker_key().is_some();
        if let Some(picker) = &mut self.ui.picker {
            let (mode, commands): (&str, &[(&str, &str)]) =
                if self.search.workspace_picker().is_some() {
                    (
                        "Review",
                        &[
                            ("workspace.edit.toggle", "toggle"),
                            ("workspace.edit.accept", "apply"),
                            ("workspace.edit.reject", "reject"),
                        ],
                    )
                } else if theme_picker {
                    ("", &[("picker.cancel", "cancel")])
                } else if self.input.picker_normal() {
                    (
                        "Normal",
                        &[
                            ("picker.mode.insert", "edit"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                } else {
                    (
                        "Insert",
                        &[
                            ("picker.mode.normal", "normal"),
                            ("picker.accept", "accept"),
                            ("picker.cancel", "close"),
                        ],
                    )
                };
            let hints = commands
                .iter()
                .filter_map(|(command, action)| {
                    self.configuration
                        .keymaps()
                        .inspect_key(&context, "escape")
                        .filter(|binding| {
                            *command == "picker.cancel"
                                && binding.command.as_deref() == Some(*command)
                        })
                        .or_else(|| self.configuration.keymaps().inspect(&context, command))
                        .map(|binding| {
                            if theme_picker {
                                let key = if binding.keys == "escape" {
                                    "Escape"
                                } else {
                                    &binding.keys
                                };
                                format!("Press {key} to {action}")
                            } else {
                                format!("{} {action}", binding.keys)
                            }
                        })
                })
                .collect::<Vec<_>>()
                .join(" · ");
            picker.input_hint = if mode.is_empty() {
                hints
            } else {
                format!("{mode} · {hints}")
            };
        }
        self.sync_picker_preview();
        if let Some(picker) = &mut self.ui.picker {
            picker.preview.clone_from(self.search.preview());
        }
        self.refresh_window_content();
    }

    pub(in crate::editor::session) fn picker_insert(&mut self, text: &str) {
        self.retire_stale_picker();
        let query = self.search.picker_insert(text);
        self.sync_picker_ui();
        if self.search.live() {
            if let Some(query) = query {
                self.restart_live_workspace_search(query);
            }
        }
    }

    pub(in crate::editor::session) fn picker_backspace(&mut self) {
        self.retire_stale_picker();
        let query = self.search.picker_backspace();
        self.sync_picker_ui();
        if self.search.live() {
            if let Some(query) = query {
                self.restart_live_workspace_search(query);
            }
        }
    }

    pub(in crate::editor::session) fn picker_move(&mut self, direction: i8) {
        self.retire_stale_picker();
        self.search.picker_move(
            direction,
            self.configuration.settings().workspace.picker_wrap,
        );
        self.sync_picker_ui();
    }

    /// Show the diff of the focused change row in the pane.
    ///
    /// The read is SYNCHRONOUS AND BOUNDED: `read_hunks` carries a deadline and maximum hunk,
    /// line and byte counts, so it cannot hang the UI thread or read an unbounded file. It is
    /// done here rather than through the hunk service because the service tracks the buffer's
    /// own hunks and a second consumer would fight it for the accepted snapshot.
    fn sync_changes_diff_preview(&mut self) {
        let Some(crate::editor::session::search::PickerPayload::MatcherSource(source)) =
            self.selected_picker_payload().cloned()
        else {
            self.clear_picker_preview();
            return;
        };
        let Some(status) = self.git.status().cloned() else {
            self.clear_picker_preview();
            return;
        };
        let layer = self.search.changes_layer();
        // A hunk policy takes a path RELATIVE to the scope root: it rejects an absolute one,
        // and the status row carries an absolute path.
        let Ok(relative) = source.path.strip_prefix(&status.scope_root) else {
            self.clear_picker_preview();
            return;
        };
        let policy =
            crate::editor::session::git::GitSession::policy(&status, relative.to_owned(), layer);
        match crate::workspace::git::read_hunks(&policy) {
            Ok(snapshot) => {
                // The pane names the comparison it is showing, so a diff is never read as
                // another one.
                let provenance = match layer {
                    crate::workspace::git::GitDiffLayer::Staged => "HEAD -> index",
                    crate::workspace::git::GitDiffLayer::Unstaged => "index -> disk",
                };
                let preview = crate::editor::session::git::diff_preview_from_hunks(
                    &snapshot.hunks,
                    provenance,
                    false,
                    0,
                );
                let retired = self.search.set_preview(crate::ui::PickerPreview {
                    title: source
                        .path
                        .strip_prefix(&status.scope_root)
                        .unwrap_or(&source.path)
                        .display()
                        .to_string(),
                    body: crate::ui::PickerPreviewBody::Diff(preview),
                });
                if let Some(id) = retired {
                    self.documents.cancel_request(id);
                }
            }
            // A file whose diff cannot be read shows no diff rather than a stale one.
            Err(_) => self.clear_picker_preview(),
        }
    }

    fn clear_picker_preview(&mut self) {
        if let Some(id) = self.search.clear_preview() {
            self.documents.cancel_request(id);
        }
    }

    /// The change surface's focused row as a path RELATIVE to the scope root, when that
    /// surface is open.
    ///
    /// `None` means the caller should act on the active buffer, which is what every other
    /// surface wants. The change surface labels `s` and `u` by focus, so it stages the row
    /// under the cursor rather than whatever buffer happens to be active.
    fn changes_focused_relative(&self) -> Option<std::path::PathBuf> {
        if self.search.picker_surface() != PickerSurfaceKind::Changes {
            return None;
        }
        let crate::editor::session::search::PickerPayload::MatcherSource(source) =
            self.selected_picker_payload()?
        else {
            return None;
        };
        let status = self.git.status()?;
        source
            .path
            .strip_prefix(&status.scope_root)
            .ok()
            .map(std::path::Path::to_owned)
    }

    fn sync_picker_preview(&mut self) {
        if self.search.picker_surface() == PickerSurfaceKind::Changes {
            self.sync_changes_diff_preview();
            return;
        }
        if let Some(PickerPayload::ProposalFile(index)) = self.selected_picker_payload() {
            let index = *index;
            let preview = self.workspace_edits.proposal().and_then(|proposal| {
                let file = proposal.files.get(index)?;
                Some(crate::ui::PickerPreview {
                    title: file
                        .canonical_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    body: crate::ui::PickerPreviewBody::Diff(
                        crate::editor::session::git::text_diff_preview(
                            &file.original_bytes,
                            &file.proposed_bytes,
                            "current -> proposed",
                        ),
                    ),
                })
            });
            match preview {
                Some(preview) => {
                    if let Some(id) = self.search.set_preview(preview) {
                        self.documents.cancel_request(id);
                    }
                }
                None => self.clear_picker_preview(),
            }
            return;
        }
        let Some((label, payload)) = self
            .search
            .picker()
            .and_then(PickerState::selected_item)
            .and_then(|item| {
                self.selected_picker_payload()
                    .cloned()
                    .map(|payload| (item.label.clone(), payload))
            })
        else {
            self.clear_picker_preview();
            return;
        };
        let payload = if let PickerPayload::TaskTest(key) = payload {
            let Some((path, line, column)) = self.tasks.resolve_test(&key) else {
                self.clear_picker_preview();
                return;
            };
            PickerPayload::MatcherSource(MatcherSource { path, line, column })
        } else {
            payload
        };
        match payload {
            PickerPayload::WorkspaceSearch(key) => {
                if let Some(id) = self.search.preview_match(
                    &key,
                    self.configuration
                        .settings()
                        .ui
                        .picker
                        .maximum_preview_bytes,
                ) {
                    self.documents.cancel_request(id);
                }
            }
            PickerPayload::MatcherSource(source) => {
                let (retired, ticket) = self.search.prepare_preview(
                    source.path.clone(),
                    label,
                    self.configuration
                        .settings()
                        .ui
                        .picker
                        .loading_label
                        .clone(),
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                );
                if let Some(id) = retired {
                    self.documents.cancel_request(id);
                }
                let Some(ticket) = ticket else {
                    return;
                };
                let roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect();
                let maximum = self
                    .configuration
                    .settings()
                    .ui
                    .picker
                    .maximum_preview_bytes;
                match self
                    .documents
                    .read_scoped_bounded(source.path, roots, maximum as u64)
                {
                    Ok(id) => {
                        if !self.search.admit_preview(ticket, id, maximum) {
                            self.documents.cancel_request(id);
                        }
                    }
                    Err(error) => self.search.preview_failed(ticket, error.to_string()),
                }
            }
            _ => self.clear_picker_preview(),
        }
    }
    pub(in crate::editor::session) fn open_plugin_picker(
        &mut self,
        provider: String,
        query: String,
    ) {
        let buffer_revision = self
            .state
            .active_buffer_id()
            .and_then(|buffer| self.state.buffers.get(buffer))
            .map(|buffer| buffer.text.revision().0);
        let invocation = match self.extension.start_provider(
            &provider,
            query,
            (self.workspace.root_revision(), buffer_revision),
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {provider}"),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::PluginProvider(invocation.request),
        );
        if self.extension.provider_visible(invocation.request) {
            self.process_effect(AppEffect::PluginExecute {
                plugin: invocation.plugin,
                command: invocation.command,
            });
        } else {
            self.extension
                .discard_unpresented_provider(invocation.request);
        }
    }

    pub(in crate::editor::session) fn present_plugin_picker(&mut self, request: u64) {
        if !self.extension.provider_visible(request) {
            return;
        }
        let Some(projection) = self.extension.provider_projection(request) else {
            return;
        };
        let items = match compose_plugin_picker_items(&projection) {
            Ok(items) => items,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            format!("Plugin · {}", projection.provider),
            items,
            String::new(),
            true,
            PickerOrigin::PluginProvider(request),
        );
    }

    pub(in crate::editor::session) fn close_picker(&mut self) {
        self.configuration.cancel_theme_picker();
        self.debugger_session.dismiss_choices();
        if self.ui.picker.is_some() {
            self.input.capture_changed();
        }
        if let Some(id) = self.search.workspace_picker() {
            self.retire_workspace_picker(id);
        }
        self.language.clear_actions();
        if let Some(id) = self.search.close_picker() {
            self.documents.cancel_request(id);
        }
        self.extension.picker_installed(None);
        self.tasks.select_output(None);
        self.ui.picker = None;
        self.refresh_window_content();
    }

    pub(in crate::editor::session) fn accept_picker(&mut self) {
        // A help page documents; it does not act. Accepting inside help closes the page and
        // returns the surface it was opened over, exactly like Escape, instead of running the
        // row's command and resurrecting a surface underneath it.
        if self.search.help_is_open() {
            self.close_picker();
            return;
        }
        self.retire_stale_picker();
        if let Some(PickerPayload::Theme(key)) = self.search.selected_picker_payload() {
            match self.configuration.apply_theme(key) {
                Ok(name) => {
                    self.close_picker();
                    let saving = self.configuration.settings().persistence.enabled
                        && self.configuration.settings().persistence.save_on_exit;
                    if saving {
                        self.persistence.request_save();
                    }
                    self.notify_message(
                        format!(
                            "Theme {} applied; {}",
                            crate::config::theme_display_name(&name),
                            if saving {
                                "editor-state save requested"
                            } else {
                                "state saving is disabled"
                            }
                        ),
                        false,
                    );
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let payload = self.search.take_picker_payload();
        if let Some(PickerPayload::ProposalFile(index)) = &payload {
            if let Err(error) = self.workspace_edits.toggle(*index) {
                self.show_error(error);
            }
            self.show_workspace_proposal();
            return;
        }
        // Retain only the accepted source match before close retires live search data.
        let search_match = match &payload {
            Some(PickerPayload::WorkspaceSearch(id)) => self
                .search
                .matches()
                .iter()
                .find(|found| &found.id == id)
                .cloned(),
            _ => None,
        };
        let code_action = match &payload {
            Some(PickerPayload::CodeAction(key)) => Some(self.language.take_action(key)),
            _ => None,
        };
        let debug_selection = match &payload {
            Some(PickerPayload::Debug(key)) => Some(self.debugger_session.take_selection(key)),
            _ => None,
        };
        let provider_selection = match &payload {
            Some(PickerPayload::PluginProviderItem {
                plugin,
                load,
                request,
                item,
                action_command,
            }) => self
                .extension
                .provider_selection(plugin, *load, *request, item, action_command),
            _ => None,
        };
        self.close_picker();
        match payload {
            Some(PickerPayload::Theme(_)) => {}
            Some(PickerPayload::WorkspaceSearch(_)) => {
                if let Some(found) = search_match {
                    self.navigate_workspace_search_match(found);
                }
            }
            Some(PickerPayload::LspLocation(location)) => self.navigate_lsp_location(&location),
            Some(PickerPayload::Diagnostic(key)) => {
                if let Some(row) = self.diagnostics.resolve(&key, &self.state) {
                    let (uri, range) = (row.uri.clone(), row.range);
                    self.navigate_byte_location(&uri, range);
                } else {
                    self.show_error("diagnostic source retired");
                }
            }
            Some(PickerPayload::CodeAction(_)) => {
                let (action, context) = match code_action.expect("selected code action") {
                    Ok(action) => action,
                    Err(error) => {
                        self.show_error(error);
                        return;
                    }
                };
                if let Some(edit) = action.edit {
                    self.begin_workspace_proposal_owned(
                        action.title,
                        edit,
                        None,
                        self.lsp_position_encoding(),
                        context,
                    );
                } else if let Some(command) = action.command {
                    self.request_lsp(LspFeature::ExecuteCommand(command));
                }
            }
            Some(PickerPayload::LspCommand(command)) => {
                self.request_lsp(LspFeature::ExecuteCommand(command));
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::GitWorktree { path, .. } | PickerPayload::Project(path)) => {
                self.switch_workspace_root(path)
            }
            Some(PickerPayload::Agent(choice)) => self.accept_agent_choice(choice),
            Some(PickerPayload::PullRequest(number)) => self.checkout_pull_request(number),
            Some(PickerPayload::Terminal(key)) => {
                if let Some(id) = self.terminal_session.resolve(&key) {
                    self.terminal_session.select(id);
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            Some(PickerPayload::Window(window)) => {
                self.reconcile_editor_windows();
                let result = self
                    .window_session
                    .plan_focus(
                        window,
                        &self.state.window_version(),
                        self.extension.contributions().version(),
                    )
                    .and_then(|plan| self.apply_window_plan(plan));
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::LspSession(key)) => self.select_lsp_session(&key),
            Some(PickerPayload::Debug(_)) => {
                self.debugger_session.refresh(
                    &self.configuration.settings().debug,
                    self.workspace.version(),
                );
                let target = crate::editor::session::debugger::SourceTarget {
                    navigation: self
                        .window_session
                        .navigation_stamp(self.state.window_version(), self.state.active_view),
                    capture: self.input.capture_epoch(),
                };
                let debug_selection = debug_selection.expect("debug selection routed");
                // A value with nothing inside it opens to the whole of itself, unclipped.
                if let Some(value) = debug_selection.as_ref().ok().and_then(|s| s.value()) {
                    let value = value.to_owned();
                    self.debugger_session.record_console_evaluation(value.clone());
                    self.notify_message(value, false);
                }
                match debug_selection
                    .and_then(|selection| self.debugger_session.activate(selection, target))
                {
                    Ok(Some(location)) => self.navigate_matcher_source(MatcherSource {
                        path: location.path,
                        line: location.line.saturating_sub(1) as usize,
                        column: location.column.saturating_sub(1) as usize,
                    }),
                    Ok(None) => {}
                    Err(error) => self.show_error(error),
                }
            }
            Some(PickerPayload::GitHunk {
                path,
                status_revision,
                repository,
                hunk,
                new_line,
            }) => {
                if self.git.select(
                    status_revision,
                    repository,
                    self.workspace.root_revision(),
                    &path,
                    hunk,
                ) {
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: new_line,
                        column: 0,
                    })
                } else {
                    self.show_error("Git hunk is stale")
                }
            }
            Some(PickerPayload::TaskTest(key)) => {
                if let Some((path, line, column)) = self.tasks.resolve_test(&key) {
                    self.navigate_matcher_source(MatcherSource { path, line, column });
                } else {
                    self.show_error("test result source retired");
                }
            }
            Some(PickerPayload::MatcherSource(source)) => self.navigate_matcher_source(source),
            Some(PickerPayload::PersistedLocation(location)) => {
                self.navigate_persisted_location(location)
            }
            Some(PickerPayload::RegistrySummary(summary)) => self.request_registry(
                RegistryRequest::Show {
                    publisher: summary.publisher,
                    name: summary.name,
                    version: summary.version,
                },
                false,
            ),
            Some(PickerPayload::RegistryMetadata(metadata)) => {
                self.request_registry(RegistryRequest::Fetch { metadata }, true)
            }
            Some(PickerPayload::Command(command)) if !command.is_empty() => {
                self.run_palette_command(&command);
            }
            Some(PickerPayload::PluginProviderItem { plugin, .. }) => {
                if let Some(command) = provider_selection {
                    self.process_effect(AppEffect::PluginExecute { plugin, command });
                }
            }
            _ => {}
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn add_workspace_root(&mut self, path: PathBuf) {
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.workspace.add_root_request(requested_path, None) {
            Ok(()) => self.notify_message(self.message("workspace_root_loading", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }

    /// Accept one side of the active conflict into the RESULT BUFFER, through an ordinary
    /// transaction so the change is undoable by the editor's own undo.
    ///
    /// Saving and staging are deliberately NOT part of this: the buffer is left dirty and the
    /// file on disk and the index are untouched until their own explicit actions run.
    pub(in crate::editor::session) fn choose_git_conflict_side(&mut self, side: &str) {
        let Some(choice) = crate::workspace::git::GitConflictChoice::parse(side) else {
            self.show_error("choose base, ours or theirs");
            return;
        };
        let Some(status) = self.git.status() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let repository = status.repository.clone();
        let Some(view_id) = self.state.active_view else {
            self.show_error("no view is active");
            return;
        };
        let Some(view) = self.state.views.get(view_id) else {
            self.show_error("no view is active");
            return;
        };
        let buffer_id = view.buffer;
        let Some(path) = self
            .state
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        // The content comes from the repository, and every refusal is decided before the edit.
        let content = match crate::workspace::git::read_side_content(&repository, &path, choice) {
            Ok(content) => content,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            self.show_error("the buffer is gone");
            return;
        };
        let revision = buffer.text.revision();
        let length = ByteOffset(buffer.text.len_bytes());
        let edits = crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                crate::editor::text::ByteRange::new(ByteOffset(0), length).expect("whole buffer"),
                content,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error.to_string());
            return;
        }
        self.notify_message(
            format!(
                "Accepted the {} side of {}; save and stage are separate steps",
                choice.name(),
                path.display()
            ),
            false,
        );
    }
    /// Preview replacing whole files with one side, then ask. Nothing is written until the
    /// confirmation is answered, and the plan is REBUILT when it is, so a preview taken against
    /// one repository state is never applied to another.
    pub(in crate::editor::session) fn preview_git_replacement(&mut self, side: &str, files: &str) {
        let Some(choice) = crate::workspace::git::GitConflictChoice::parse(side) else {
            self.show_error("choose base, ours or theirs");
            return;
        };
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        let paths = files
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| {
                let path = std::path::Path::new(name);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    repository.join(path)
                }
            })
            .collect::<Vec<_>>();
        if paths.is_empty() {
            self.show_error("name at least one file to replace");
            return;
        }
        let plan = match crate::workspace::git::plan_replacement(&repository, &paths, choice) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let preview = plan.preview();
        self.git.set_pending_replacement(plan);
        self.open_confirmation(
            crate::ui::PromptKind::GitDestructive,
            "Replace whole files".into(),
            preview,
            crate::ui::ConfirmationChoice::No,
        );
        self.project_input_prompt();
    }
    /// Apply the previewed replacement once it is confirmed. The plan is rebuilt from the same
    /// list and side, so the write is decided by current repository state rather than by a plan
    /// captured before the question was asked.
    pub(in crate::editor::session) fn apply_git_replacement(
        &mut self,
        answer: crate::ui::ConfirmationChoice,
    ) {
        let Some(previewed) = self.git.take_pending_replacement() else {
            return;
        };
        if answer != crate::ui::ConfirmationChoice::Yes {
            self.notify_message("Replacement cancelled; nothing was written".into(), false);
            return;
        }
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        let paths = previewed
            .files
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let plan = match crate::workspace::git::plan_replacement(&repository, &paths, previewed.choice)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        match crate::workspace::git::apply_replacement(&plan) {
            Ok(written) => self.notify_message(
                format!("Replaced {} file(s); saving and staging are separate steps", written.len()),
                false,
            ),
            Err(error) => self.show_error(error.to_string()),
        }
    }
    /// Show what is TRUE about each breakpoint. The rows carry the adapter's verdict, so a
    /// request is never presented as a fact.
    fn cursor_breakpoint_place(&self) -> Option<(PathBuf, u64)> {
        let view = &self.state.views[self.state.active_view?];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).ok()?;
        Some((buffer.path.clone()?, line as u64 + 1))
    }
    fn configure_breakpoint(
        &mut self,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
    ) {
        let Some((path, line)) = self.cursor_breakpoint_place() else {
            return;
        };
        if let Err(error) = self.debugger_session.breakpoint(
            DapSourceBreakpoint {
                path,
                line,
                column: None,
                condition,
                hit_condition,
                log_message,
            },
            false,
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn open_breakpoint_states(&mut self) {
        let rows = self.debugger_session.persisted_breakpoints();
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let state = self
                    .debugger_session
                    .breakpoint_state(&row.path, row.line)
                    .map(|state| state.label())
                    .unwrap_or("not requested");
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("breakpoint:{index}")),
                        label: format!(
                            "{}:{}",
                            self.workspace_relative_label(&row.path),
                            row.line
                        ),
                        detail: match &row.condition {
                            Some(condition) => format!("{state}, if {condition}"),
                            None => state.to_owned(),
                        },
                        source: "debugger".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command("dap.breakpoints.states".into()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Breakpoints · {} (what is true, not what was asked)", items.len());
        self.install_picker(title, items);
    }
    /// Enable or disable the breakpoint on the cursor line. Disabling is SESSION state, so it
    /// survives whatever the adapter later says about that breakpoint.
    pub(in crate::editor::session) fn toggle_breakpoint_disabled(&mut self) {
        let Some(view) = self.state.active_view else {
            self.show_error("no view is active");
            return;
        };
        let Some(view_state) = self.state.views.get(view) else {
            self.show_error("no view is active");
            return;
        };
        let buffer = view_state.buffer;
        let cursor = view_state.cursor;
        let Some(path) = self
            .state
            .buffers
            .get(buffer)
            .and_then(|buffer| buffer.path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        let Some(buffer) = self.state.buffers.get(buffer) else {
            return;
        };
        let line = buffer.text.byte_to_line(cursor).unwrap_or(0) as u64 + 1;
        let disabled = self
            .debugger_session
            .breakpoint_state(&path, line)
            .is_some_and(|state| state == crate::debugger::DapBreakpointState::Disabled);
        self.debugger_session
            .set_breakpoint_disabled(&path, line, !disabled);
        self.notify_message(
            format!(
                "breakpoint {}:{} is now {}",
                path.display(),
                line,
                if disabled { "enabled" } else { "disabled" }
            ),
            false,
        );
    }
    /// TERMINATE: ask for the debuggee to end with the session.
    pub(in crate::editor::session) fn terminate_debug_session(&mut self) {
        match self.debugger_session.terminate_debuggee() {
            Ok(()) => {
                let state = self.debugger_session.end_state();
                self.notify_message(
                    format!("debug session terminated ({state:?})"),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
    /// DETACH: leave the session without asking the debuggee to stop. The report says the program
    /// may still be running, because the adapter's side of that contract is not something this
    /// editor can claim on the adapter's behalf.
    pub(in crate::editor::session) fn detach_debug_session(&mut self) {
        match self.debugger_session.detach_debuggee() {
            Ok(()) => {
                let state = self.debugger_session.end_state();
                self.notify_message(
                    format!(
                        "debug session detached ({state:?}); the editor is no longer attached and \
                         the program was NOT asked to stop"
                    ),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
    /// Stage the resolved conflict in the active buffer, through the SAME file-staging plan the
    /// ordinary staging surface uses rather than a second staging path.
    pub(in crate::editor::session) fn stage_git_conflict(&mut self) {
        self.input.set_pending_stage(AppEffect::GitConflictStage);
        if let Err(error) = self.git.start_plan(
            &self.state,
            crate::workspace::git::GitMutationKind::StageFile,
            self.workspace.root_revision(),
            self.workspace.trust() == WorkspaceTrust::Trusted,
            Vec::new(),
        ) {
            self.show_error(error);
        }
    }
    /// Continue the paused operation. Refuses while any conflict remains, and refuses when no
    /// operation is paused, both before any write.
    pub(in crate::editor::session) fn continue_git_conflict(&mut self) {
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        match crate::workspace::git::continue_operation(&repository, "") {
            Ok(outcome) => {
                self.notify_message(
                    format!(
                        "{} completed as {}",
                        outcome.operation,
                        outcome
                            .commit
                            .map(|oid| oid.to_string()[..8].to_owned())
                            .unwrap_or_else(|| "a commit".to_owned())
                    ),
                    false,
                );
                self.after_conflict_recovery();
            }
            Err(refusal) => self.show_error(refusal.to_string()),
        }
    }
    /// Abandon the paused operation. There is no hard-reset fallback: when the working tree holds
    /// changes the abort would destroy, it is refused and names them.
    pub(in crate::editor::session) fn abort_git_conflict(&mut self) {
        let Some(repository) = self.conflict_repository() else {
            return;
        };
        match crate::workspace::git::abort_operation(&repository) {
            Ok(outcome) => {
                self.notify_message(
                    format!(
                        "{} abandoned; the branch is back at {}",
                        outcome.operation,
                        outcome
                            .restored_to
                            .map(|oid| oid.to_string()[..8].to_owned())
                            .unwrap_or_else(|| "where it started".to_owned())
                    ),
                    false,
                );
                self.after_conflict_recovery();
            }
            Err(refusal) => self.show_error(refusal.to_string()),
        }
    }
    fn conflict_repository(&mut self) -> Option<std::path::PathBuf> {
        match self.git.status() {
            Some(status) => Some(status.repository.clone()),
            None => {
                self.show_error("Git status is not ready; is this a Git repository?");
                None
            }
        }
    }
    /// After a recovery the repository state changed underneath every open buffer, so the
    /// status is refreshed rather than left stale.
    fn after_conflict_recovery(&mut self) {
        self.start_git_status_refresh();
        // The operation may have finished, so the bar is re-read from the repository rather than
        // left showing a state that no longer exists.
        self.project_operation_bar();
        self.project_debug_panes();
    }
    /// The conflict surface. The operation bar names the paused operation and its phase, and
    /// each conflicted path reports its three sides with a side that does not exist shown as
    /// ABSENT rather than as content.
    pub(in crate::editor::session) fn open_git_conflicts(&mut self) {
        let Some(status) = self.git.status() else {
            self.show_error("Git status is not ready; is this a Git repository?");
            return;
        };
        let repository = status.repository.clone();
        let scope_root = status.scope_root.clone();
        let state = match crate::workspace::git::read_conflict_state(&repository) {
            Ok(state) => state,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let title = format!("Conflicts · {}", state.phase());
        let mut items = Vec::new();
        for (index, conflict) in state.conflicts.iter().enumerate() {
            items.push((
                PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(format!("conflict:{index}")),
                    label: conflict.path.display().to_string(),
                    detail: conflict.summary(),
                    source: "conflict".into(),
                    action: PickerAction::None,
                },
                PickerPayload::MatcherSource(MatcherSource {
                    line: first_conflict_line(&scope_root.join(&conflict.path)),
                    path: scope_root.join(&conflict.path),
                    column: 0,
                }),
            ));
        }
        self.install_picker(title, items);
        self.search.set_picker_footer(
            "Enter Open · then Space g m o/t/b takes ours, theirs or both · Esc Esc Close".to_owned(),
        );
        self.sync_picker_ui();
    }
    /// Open the tutorial, a fresh copy of `assets/defaults/tutor.md` in the state directory, so
    /// the user practises on it and can save without touching the original, as in vimtutor.
    /// Contextual help: what is available on the focused surface, read from the command
    /// registry through the dispatcher's own context check, so help can never advertise a
    /// command the dispatcher would refuse and can never drift from a renamed command.
    pub(in crate::editor::session) fn open_contextual_help(&mut self) {
        let invocation =
            crate::editor::CommandInvocation::new("overlay.help.contextual", crate::editor::InvocationSource::Keymap)
                .expect("static command id");
        let items = self
            .commands
            .specs()
            .filter(|spec| spec.context.satisfied(&self.state, &invocation))
            .enumerate()
            .map(|(index, spec)| {
                let id = PickerItemId(format!("help:{index}:{}", spec.id.as_str()));
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: spec.id.as_str().to_owned(),
                        detail: spec.title.clone(),
                        source: format!("{:?}", spec.context).to_lowercase(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(spec.id.as_str().to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {} commands available here", items.len());
        self.install_help_page(title, items);
    }
    /// Help for the form or confirmation in front of the user. It names the surface by its own
    /// title and message, so the help is about the question actually being asked, and it leaves
    /// that question untouched: opening help is not answering it.
    pub(in crate::editor::session) fn open_form_help(&mut self) {
        let Some(prompt) = self.input.prompt() else {
            self.show_error("no form or confirmation is open");
            return;
        };
        let kind = format!("{:?}", prompt.kind);
        let mut rows = vec![
            ("surface".to_owned(), kind.clone()),
        ];
        if let Some(confirmation) = prompt.confirmation.as_ref() {
            rows.push(("asks".to_owned(), confirmation.title.clone()));
            rows.push(("detail".to_owned(), confirmation.message.clone()));
            rows.push((
                "choices".to_owned(),
                format!(
                    "{} / {}",
                    confirmation.positive_label, confirmation.negative_label
                ),
            ));
            if !confirmation.hint.is_empty() {
                rows.push(("keys".to_owned(), confirmation.hint.clone()));
            }
        } else if !prompt.input.is_empty() {
            rows.push(("draft".to_owned(), prompt.input.clone()));
        }
        // The commands available on this surface come from the registry through the dispatcher's
        // own context check, so the help can never list a command this form would refuse.
        let invocation = crate::editor::CommandInvocation::new(
            "overlay.help.form",
            crate::editor::InvocationSource::Keymap,
        )
        .expect("static command id");
        for spec in self
            .commands
            .specs()
            .filter(|spec| spec.context.satisfied(&self.state, &invocation))
        {
            if spec.id.as_str() == "overlay.help.form" {
                continue;
            }
            rows.push(("available".to_owned(), spec.id.as_str().to_owned()));
        }
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (kind, value))| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("form-help:{index}:{kind}")),
                        label: value,
                        detail: kind,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command("overlay.help.form".to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {kind}");
        self.install_help_page(title, items);
    }
    /// The reference for the selected command, assembled from the registry entry itself: its
    /// title, its description, where it is available and every argument the schema declares,
    /// marked required or optional. Nothing here is a second copy of the truth.
    pub(in crate::editor::session) fn open_command_reference(&mut self) {
        let Some(PickerPayload::Command(command)) = self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a command first");
            return;
        };
        let Some(spec) = self
            .commands
            .specs()
            .find(|spec| spec.id.as_str() == command)
        else {
            self.show_error("that command is no longer registered");
            return;
        };
        let mut rows = vec![
            ("command".to_owned(), spec.id.as_str().to_owned()),
            ("does".to_owned(), spec.title.clone()),
        ];
        if !spec.description.is_empty() {
            rows.push(("detail".to_owned(), spec.description.clone()));
        }
        rows.push(("available".to_owned(), format!("{:?}", spec.context).to_lowercase()));
        // The effective binding, read from the same resolved table the dispatcher uses, so the
        // reference can never show a key the user cannot press: a remapped key is the key shown,
        // an unmapped one is not offered, and a command with none here says so.
        for (label, context) in [("binding", "editor.normal"), ("binding here", "picker")] {
            let effective = self
                .configuration
                .keymaps()
                .effective_binding(context, spec.id.as_str());
            if effective.is_bound() || label == "binding" {
                rows.push((label.to_owned(), effective.display()));
            }
        }
        for (name, field) in &spec.arguments.fields {
            rows.push((
                if field.required { "argument" } else { "optional" }.to_owned(),
                name.clone(),
            ));
        }
        if spec.arguments.fields.is_empty() {
            rows.push(("takes".to_owned(), "no arguments".to_owned()));
        }
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (kind, value))| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("reference:{index}:{kind}")),
                        label: value,
                        detail: kind,
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(spec.id.as_str().to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let title = format!("Help · {}", spec.id.as_str());
        self.install_help_page(title, items);
    }
    /// Move the editor into the selected worktree: it replaces the current root, so the file
    /// tree, pickers, LSP servers and git signs follow. The worktree gets its own trust
    /// decision. Files with unsaved changes in the current root block the switch.
    pub(in crate::editor::session) fn open_git_worktree(&mut self) {
        let Some(crate::editor::session::search::PickerPayload::GitWorktree { path, .. }) =
            self.search.selected_picker_payload().cloned()
        else {
            self.show_error("select a worktree first");
            return;
        };
        self.switch_workspace_root(path);
    }

    pub(super) fn switch_workspace_root(&mut self, path: PathBuf) {
        let current = self.workspace.active_root().and_then(|id| {
            self.workspace
                .roots()
                .iter()
                .find(|root| root.id == id)
                .map(|root| (id, root.identity.canonical_path.clone()))
        });
        let Some((id, current_path)) = current else {
            self.add_workspace_root(path);
            return;
        };
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        if target == current_path {
            self.notify_message(format!("already in {}", target.display()), false);
            return;
        }
        let unsaved = self
            .state
            .buffers
            .values()
            .filter(|buffer| {
                buffer.dirty
                    && buffer.path.as_ref().is_some_and(|file| {
                        file.starts_with(&current_path) && !file.starts_with(&target)
                    })
            })
            .count();
        if unsaved > 0 {
            self.show_error(format!(
                "{unsaved} file(s) in {} have unsaved changes; save or close them first",
                current_path.display()
            ));
            return;
        }
        self.close_picker();
        match self.workspace.add_root_request(target, Some(id)) {
            Ok(()) => self.notify_message("switching…".to_owned(), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }

    /// After a worktree switch: close the old root's clean buffers, move the editor's root and
    /// reopen the file that was in front at the same place in the new worktree.
    pub(in crate::editor::session) fn finish_root_switch(&mut self, path: PathBuf, old: &Path) {
        let leaving = |file: &Path| file.starts_with(old) && !file.starts_with(&path);
        let reopen = self.current_persisted_location().and_then(|mut location| {
            let relative = location.path.strip_prefix(old).ok()?;
            if !leaving(&location.path) {
                return None;
            }
            location.path = path.join(relative);
            location.path.is_file().then_some(location)
        });
        // Buffers in no window go first, or closing a window's buffer would put one of them on
        // screen, and a later save would land in the old checkout.
        let hidden = self
            .state
            .buffers
            .iter()
            .filter(|(id, buffer)| {
                !buffer.dirty
                    && buffer.path.as_deref().is_some_and(leaving)
                    && !self.state.views.values().any(|view| view.buffer == *id)
            })
            .map(|(id, buffer)| (id, buffer.path.clone()))
            .collect::<Vec<_>>();
        for (buffer, closed_path) in hidden {
            let mut candidate = self.state.draft();
            candidate.remove_hidden_document(buffer);
            match self.commit_editor_draft(candidate, None, false) {
                Ok(()) => self.retire_closed_buffer(buffer, closed_path.as_deref()),
                Err(error) => self.show_error(error),
            }
        }
        // A window can fall back to another window's buffer from the old tree, so go until none
        // shows one. Each round closes a buffer or a window.
        for _ in 0..self.state.views.len() + self.state.buffers.len() {
            let showing = self.state.views.iter().find_map(|(view, state)| {
                let buffer = self.state.buffers.get(state.buffer)?;
                (!buffer.dirty && buffer.path.as_deref().is_some_and(leaving))
                    .then_some((view, state.buffer))
            });
            let Some((view, buffer)) = showing else {
                break;
            };
            // Only the active window's buffer can be closed.
            if let Err(error) = self.state.focus_view(view) {
                self.show_error(error);
                break;
            }
            self.close_buffer_target(view, buffer);
        }
        // Windows left with nothing to show go too, down to the one the file reopens in.
        let front = self.state.active_view;
        let empty = self
            .state
            .views
            .iter()
            .filter(|(view, state)| {
                Some(*view) != front
                    && self.state.buffers.get(state.buffer).is_some_and(|buffer| {
                        buffer.path.is_none() && !buffer.dirty && buffer.text.len_bytes() == 0
                    })
                    && Some(state.buffer) != self.state.kept_buffer
            })
            .map(|(view, state)| (view, state.buffer))
            .collect::<Vec<_>>();
        for (view, buffer) in empty {
            if self.state.close_view(view).is_ok() {
                let mut candidate = self.state.draft();
                candidate.remove_hidden_document(buffer);
                if let Err(error) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(error);
                }
            }
        }
        if let Some(front) = front {
            let _ = self.state.focus_view(front);
        }
        self.state.workspace_root = path.clone();
        self.invalidate_root_scoped_operations();
        self.start_git_status_refresh_silent();
        if let Some(location) = reopen {
            self.navigate_persisted_location(location);
        }
        let name = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
        self.notify_message(format!("switched to {name}"), false);
    }
    pub(in crate::editor::session) fn root_removal_blocker(
        &self,
        root: crate::workspace::RootId,
    ) -> Option<String> {
        let root = self
            .workspace
            .roots()
            .iter()
            .find(|candidate| candidate.id == root)?;
        if self.state.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root.identity.canonical_path))
        }) {
            return Some(self.message("workspace_root_dirty_blocker", &[]));
        }
        let busy = self.workspace.root_operation_pending()
            || self.language.active().is_some()
            || self
                .language
                .sessions()
                .values()
                .any(|state| state.restart_due.is_some())
            || self.debugger_session.is_running()
            || self
                .tooling
                .entries()
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing { .. }))
            || self.search.pending()
            || self.workspace_edits.preparing()
            || self.workspace_edits.proposal().is_some()
            || self.documents.legacy_pending()
            || self.workspace.persist_pending()
            || self.documents.has_startup_work()
            || self.documents.file_work_pending();
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }

    pub(in crate::editor::session) fn invalidate_root_scoped_operations(&mut self) {
        self.documents.retire_startup();
        self.documents.retire_workspace(&self.workspace.version());
        self.clear_git_status();
        self.terminal_session.reset();
        self.reject_terminal_responses("workspace changed");
        self.set_terminal_focus(false);
        self.ui.terminal_panel = None;
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.language.clear_active();

                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.language.retire_launches();
        for (plugin, load) in self.extension.retire_packages() {
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        self.debugger_session.reset();
        self.input.clear_snippet();
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.tasks.retire_workspace();
        self.clear_git_status();
        self.search.cancel();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("workspace changed");
        self.retire_plugin_edit_sources();
        self.retire_explorer_intents();
        self.clear_explorer_preview();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.set_explorer_focus(false);
        self.reconcile_explorer_watch();
        self.close_picker();
        self.ensure_cursor_visible();
    }

    pub(in crate::editor::session) fn remove_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                self.invalidate_root_scoped_operations();
                self.notify_message(
                    self.message(
                        "workspace_root_removed",
                        &[("path", root.identity.canonical_path.display().to_string())],
                    ),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn activate_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        let before = self.workspace.version();
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                if before != self.workspace.version() {
                    self.documents.retire_startup();
                }
                self.documents.retire_workspace(&self.workspace.version());
                self.clear_git_status();
                let path = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.display().to_string())
                    .unwrap_or_default();
                self.notify_message(
                    self.message("workspace_root_activated", &[("path", path)]),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn nix_workspace_available_for(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.tooling.nix_workspace_available_for(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.tooling.selected_lsp_runtime_source(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn selected_managed_definition(
        &self,
        definition: &ToolDefinition,
    ) -> ToolDefinition {
        self.tooling.selected_managed_definition(definition)
    }

    pub(in crate::editor::session) fn managed_variant_installed(
        &self,
        base: &ToolDefinition,
        variant: &ToolDefinition,
    ) -> bool {
        self.tooling.managed_variant_installed(base, variant)
    }

    pub(in crate::editor::session) fn selected_managed_executable(
        &self,
        definition: &ToolDefinition,
    ) -> Option<PathBuf> {
        self.tooling.selected_managed_executable(definition)
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_available(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.tooling.selected_lsp_runtime_available(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn tool_version_key(
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> String {
        crate::editor::session::tooling::ToolingSession::tool_version_key(
            definition, source, executable,
        )
    }

    pub(in crate::editor::session) fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
        crate::editor::session::tooling::ToolingSession::rust_analyzer_is_rustup_proxy(path)
    }

    pub(in crate::editor::session) fn unavailable_rust_analyzer_message() -> String {
        crate::editor::session::tooling::ToolingSession::unavailable_rust_analyzer_message()
    }

    pub(in crate::editor::session) fn rust_analyzer_path_version(
        version: &str,
        rustup_bundled: bool,
    ) -> (String, String) {
        crate::editor::session::tooling::ToolingSession::rust_analyzer_path_version(
            version,
            rustup_bundled,
        )
    }

    pub(in crate::editor::session) fn observed_tool_version(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> (String, Option<String>) {
        self.tooling
            .observed_tool_version(definition, source, executable)
    }

    pub(in crate::editor::session) fn queue_tool_version_probe(
        &mut self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: PathBuf,
    ) {
        self.tooling
            .queue_tool_version_probe(definition, source, executable)
    }

    pub(in crate::editor::session) fn queue_tool_version_probes(&mut self, category: &str) {
        self.tooling.queue_tool_version_probes(category)
    }
}

impl InteractiveApplication {
    fn tool_menu_height(&self) -> u16 {
        let configured = &self.configuration.settings().ui.tool_manager;
        let height =
            (u32::from(self.window_session.height()) * u32::from(configured.height_percent) / 100)
                .try_into()
                .unwrap_or(self.window_session.height());
        height
            .max(configured.minimum_height.min(self.window_session.height()))
            .min(self.window_session.height())
    }

    fn tool_menu_list_visible_rows(&self) -> usize {
        // Rendering reserves the border, up to two wrapped help rows, tabs,
        // heading, separator, and ten detail rows. Using the two-help-row
        // bound keeps navigation safe when the help text wraps after resize.
        usize::from(self.tool_menu_height().saturating_sub(18)).max(1)
    }

    fn tool_runtime_picker_visible_rows(&self) -> usize {
        let option_count = self
            .ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .map_or(1, |picker| picker.options.len());
        let compact_height = u16::try_from(option_count)
            .unwrap_or(u16::MAX)
            .saturating_mul(2)
            .saturating_add(11)
            .max(self.configuration.settings().ui.tool_manager.minimum_height);
        usize::from(
            self.tool_menu_height()
                .min(compact_height)
                .saturating_sub(9)
                / 2,
        )
        .max(1)
    }

    pub(in crate::editor::session) fn open_tool_menu(&mut self, category: String) {
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.close_prompt();
        self.input.capture_changed();
        // `:lsp` is normally opened from a source buffer. Put the matching
        // server under the cursor so Enter and runtime selection act on the
        // server the user is actually editing, not an alphabetically earlier
        // unrelated adapter.
        let selected = (category == "lsp")
            .then(|| {
                self.active_lsp_document_without_text()
                    .map(|(_, document)| document.language_id)
            })
            .flatten()
            .and_then(|language| {
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry.definition.automatically_supports_language(&language)
                            && matches!(entry.status, ToolStatus::Installed(_))
                    })
                    .or_else(|| {
                        self.tooling.entries().iter().find(|entry| {
                            entry.definition.kind == ToolKind::Lsp
                                && entry.definition.automatically_supports_language(&language)
                        })
                    })
                    .map(|entry| entry.definition.id.clone())
            });
        self.queue_tool_version_probes(&category);
        self.rebuild_tool_menu(&category, selected.as_deref());
    }

    pub(in crate::editor::session) fn lsp_menu_lifecycle(&self, id: &str) -> (String, Vec<String>) {
        let running_servers = self.language.running_servers();
        let mut running = 0usize;
        let mut starting = false;
        let mut backoff = false;
        let mut failed = false;
        let mut sessions = self
            .language
            .sessions()
            .iter()
            .filter(|(_, session)| session.server_id == id)
            .map(|(key, session)| {
                let state = if session.capabilities.is_some() {
                    running = running.saturating_add(1);
                    "running"
                } else if session.circuit_open {
                    failed = true;
                    "crashed"
                } else if session.restart_due.is_some() {
                    backoff = true;
                    "restarting"
                } else if running_servers.contains(key) {
                    starting = true;
                    "starting"
                } else {
                    "stopped"
                };
                format!("{} ({state})", session.root_path.display())
            })
            .collect::<Vec<_>>();
        sessions.sort();
        starting = starting || self.language.launch_pending(id);
        let lifecycle = if running > 0 {
            if running == 1 {
                "running".into()
            } else {
                format!("running ×{running}")
            }
        } else if starting {
            "starting".into()
        } else if backoff {
            "restarting".into()
        } else if failed || self.language.errors().contains_key(id) {
            "crashed".into()
        } else {
            "stopped".into()
        };
        (lifecycle, sessions)
    }

    pub(in crate::editor::session) fn runtime_options_for(
        &self,
        definition: &ToolDefinition,
    ) -> Vec<ToolRuntimeOption> {
        let nixos_host = is_nixos_host();
        let selected_source = self.selected_lsp_runtime_source(definition);
        let selected_managed = self.selected_managed_definition(definition);
        let path = discover_executable(&definition.executable);
        let managed_path = managed_tool_executable(definition);
        let nix_available = self.nix_workspace_available_for(definition);
        let nix_profile_installed = nix_lsp_profile_installed(definition);
        let (path_version, path_error) =
            self.observed_tool_version(definition, LspRuntimeSource::Path, path.as_ref());
        let path_version_reported = path_error.is_none()
            && !matches!(
                path_version.as_str(),
                "probing…" | "detecting…" | "version unavailable" | "probe failed"
            );
        let rustup_bundled = definition.id == "rust-analyzer"
            && path
                .as_deref()
                .is_some_and(Self::rust_analyzer_is_rustup_proxy);
        let rustup_installable = rustup_bundled
            && path_error.is_some()
            && path
                .as_ref()
                .is_some_and(|proxy| proxy.with_file_name("rustup").is_file());
        let path_usable = path.is_some() && path_error.is_none();
        let (path_display_version, path_release) = if rustup_bundled {
            if path_version_reported {
                Self::rust_analyzer_path_version(&path_version, true)
            } else {
                ("rustup bundled".into(), "toolchain build".into())
            }
        } else if definition.id == "rust-analyzer" && path_version_reported {
            Self::rust_analyzer_path_version(&path_version, false)
        } else {
            (
                path_version.clone(),
                if path_version_reported {
                    "detected PATH build".into()
                } else {
                    "—".into()
                },
            )
        };
        let mut options = vec![ToolRuntimeOption {
            source: LspRuntimeSource::Path.key().into(),
            version_pin: None,
            label: LspRuntimeSource::Path.label().into(),
            version: if path.is_some() {
                path_display_version
            } else {
                "—".into()
            },
            release: if path.is_some() {
                path_release
            } else {
                "—".into()
            },
            location: path.as_ref().map_or_else(
                || "not on PATH".into(),
                |path| path.display().to_string(),
            ),
            status: if path.is_none() {
                "not installed"
            } else if path_error.is_some() {
                "not working"
            } else if selected_source == LspRuntimeSource::Path {
                "in use"
            } else {
                "installed"
            }
            .into(),
            note: if path.is_none() {
                "Nothing on PATH. Install it with your package manager to use this build.".into()
            } else if rustup_installable {
                "rustup has no rust-analyzer for this toolchain; i installs it with rustup.".into()
            } else if let Some(error) = path_error {
                error
            } else if path_version_reported {
                format!("Found on PATH ({path_version}). Dust never changes or removes it.")
            } else {
                "Found on PATH. Dust never changes or removes it.".into()
            },
            selected: selected_source == LspRuntimeSource::Path,
            installed: path_usable,
            installable: rustup_installable,
            uninstallable: false,
        }];

        if lsp_runtime_source_allowed_on_host(LspRuntimeSource::Managed, nixos_host) {
            for variant in definition.managed_versions() {
                let version = variant.version.clone().unwrap_or_else(|| "unknown".into());
                let installed = self.managed_variant_installed(definition, &variant);
                let (_, probe_error) = if installed {
                    {
                        self.observed_tool_version(
                            &variant,
                            LspRuntimeSource::Managed,
                            managed_path.as_ref(),
                        )
                    }
                } else {
                    (String::new(), None)
                };
                options.push(ToolRuntimeOption {
                source: LspRuntimeSource::Managed.key().into(),
                version_pin: Some(version.clone()),
                label: LspRuntimeSource::Managed.label().into(),
                version: version.clone(),
                release: variant.release.clone().unwrap_or_else(|| "catalog build".into()),
                location: if installed {
                    managed_path.as_ref().map_or_else(
                        || "installed path unavailable".into(),
                        |path| path.display().to_string(),
                    )
                } else {
                    "not installed".into()
                },
                status: if installed && selected_source == LspRuntimeSource::Managed
                    && selected_managed.version.as_deref() == Some(version.as_str())
                {
                    "in use"
                } else if installed {
                    "installed"
                } else {
                    "not installed"
                }
                .into(),
                note: probe_error.unwrap_or_else(|| {
                    if installed {
                        format!("{} {version}, installed by Dust.", definition.name)
                    } else {
                        "A verified download from the Dust catalog.".to_owned()
                    }
                }),
                selected: selected_source == LspRuntimeSource::Managed
                    && selected_managed.version.as_deref() == Some(version.as_str()),
                installed,
                installable: variant.install.is_some(),
                uninstallable: installed,
            });
            }

            if definition.managed_versions().is_empty() {
                options.push(ToolRuntimeOption {
                    source: LspRuntimeSource::Managed.key().into(),
                    version_pin: None,
                    label: LspRuntimeSource::Managed.label().into(),
                    version: "none".into(),
                    release: "none".into(),
                    location: "—".into(),
                    status: "unavailable".into(),
                    note: "Dust has no download for this server.".into(),
                    selected: selected_source == LspRuntimeSource::Managed,
                    installed: false,
                    installable: false,
                    uninstallable: false,
                });
            }
        }

        if lsp_runtime_source_allowed_on_host(LspRuntimeSource::Nix, nixos_host) {
            for variant in definition.managed_versions() {
                let version = variant.version.clone().unwrap_or_else(|| "unknown".into());
                let installed =
                    nix_profile_installed && self.managed_variant_installed(definition, &variant);
                let (_, probe_error) = if installed {
                    {
                        self.observed_tool_version(
                            &variant,
                            LspRuntimeSource::Nix,
                            managed_path.as_ref(),
                        )
                    }
                } else {
                    (String::new(), None)
                };
                options.push(ToolRuntimeOption {
                    source: LspRuntimeSource::Nix.key().into(),
                    version_pin: Some(version.clone()),
                    label: LspRuntimeSource::Nix.label().into(),
                    version: version.clone(),
                    release: variant.release.clone().unwrap_or_else(|| "catalog build".into()),
                    location: if installed {
                        managed_path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string())
                    } else {
                        "not installed".into()
                    },
                    status: if installed
                        && selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str())
                    {
                        "in use"
                    } else if installed {
                        "installed"
                    } else if nix_available {
                        "not installed"
                    } else {
                        "unavailable"
                    }
                    .into(),
                    note: probe_error.unwrap_or_else(|| {
                        if installed {
                            format!("{} {version}, installed by Dust through Nix.", definition.name)
                        } else {
                            "Built through Nix from the Dust catalog.".to_owned()
                        }
                    }),
                    selected: selected_source == LspRuntimeSource::Nix
                        && selected_managed.version.as_deref() == Some(version.as_str()),
                    installed,
                    installable: nix_available && variant.install.is_some(),
                    uninstallable: installed,
                });
            }
        }
        options
    }

    pub(in crate::editor::session) fn rebuild_tool_menu(
        &mut self,
        category: &str,
        selected_id: Option<&str>,
    ) {
        let nixos_host = is_nixos_host();
        let configured = self.configuration.settings().ui.tool_manager.clone();
        let previous_picker = self.ui.tool_menu.as_ref().and_then(|menu| {
            menu.runtime_picker.as_ref().map(|picker| {
                (
                    picker.server_id.clone(),
                    picker
                        .options
                        .get(picker.selected)
                        .map(|option| (option.source.clone(), option.version_pin.clone())),
                    picker.scroll,
                )
            })
        });
        let previous_scroll = self
            .ui
            .tool_menu
            .as_ref()
            .filter(|menu| menu.category == category)
            .map_or(0, |menu| menu.scroll);
        let (query, searching) = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(Default::default, |menu| (menu.query.clone(), menu.searching));
        let needle = query.to_lowercase();
        let category = if configured.categories.contains_key(category) {
            category.to_owned()
        } else {
            "all".into()
        };
        let active_language = self
            .active_lsp_document_without_text()
            .map(|(_, document)| document.language_id);
        let mut entries = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| category == "all" || tool_kind_key(entry.definition.kind) == category)
            .filter(|entry| {
                let definition = &entry.definition;
                needle.is_empty()
                    || definition.name.to_lowercase().contains(&needle)
                    || definition.id.to_lowercase().contains(&needle)
                    || definition.languages.iter().any(|language| language.contains(&needle))
            })
            // A search reaches every known server, as Mason lists them all; without one the
            // list keeps to servers that are installed or that Dust can install.
            .filter(|entry| {
                !needle.is_empty()
                    || entry.definition.kind != ToolKind::Lsp
                    || discover_executable(&entry.definition.executable).is_some()
                    || managed_tool_executable(&entry.definition).is_some()
                    || !entry.definition.managed_versions().is_empty()
                    || !self.lsp_menu_lifecycle(&entry.definition.id).1.is_empty()
            })
            .collect::<Vec<_>>();
        // Stable ordering is deliberate: installation, runtime selection, and
        // lifecycle changes must never make the focused row jump elsewhere.
        entries.sort_by_key(|entry| entry.definition.name.to_lowercase());
        let rows = entries
            .into_iter()
            .map(|entry| {
                let definition = &entry.definition;
                let is_lsp = definition.kind == ToolKind::Lsp;
                let source = if is_lsp {
                    self.selected_lsp_runtime_source(definition)
                } else if discover_executable(&definition.executable).is_some() {
                    LspRuntimeSource::Path
                } else {
                    LspRuntimeSource::Managed
                };
                let path = discover_executable(&definition.executable);
                let managed_definition = if is_lsp {
                    self.selected_managed_definition(definition)
                } else {
                    definition.clone()
                };
                let managed = if is_lsp {
                    self.selected_managed_executable(definition)
                } else {
                    managed_tool_executable(definition)
                };
                let source_executable = match source {
                    LspRuntimeSource::Path => path.as_ref(),
                    LspRuntimeSource::Managed | LspRuntimeSource::Nix => managed.as_ref(),
                };
                let observed_definition = if matches!(
                    source,
                    LspRuntimeSource::Managed | LspRuntimeSource::Nix
                ) {
                    &managed_definition
                } else {
                    definition
                };
                let (reported_version, probe_error) =
                    self.observed_tool_version(observed_definition, source, source_executable);
                let rustup_bundled = source == LspRuntimeSource::Path
                    && definition.id == "rust-analyzer"
                    && path
                        .as_deref()
                        .is_some_and(Self::rust_analyzer_is_rustup_proxy);
                let path_version_reported = probe_error.is_none()
                    && !matches!(
                        reported_version.as_str(),
                        "probing…" | "detecting…" | "version unavailable" | "probe failed"
                    );
                let path_presentation = (source == LspRuntimeSource::Path
                    && definition.id == "rust-analyzer"
                    && (rustup_bundled || path_version_reported))
                    .then(|| {
                        Self::rust_analyzer_path_version(&reported_version, rustup_bundled)
                    });
                let mut version = match source {
                    LspRuntimeSource::Path => path_presentation
                        .as_ref()
                        .map_or_else(|| reported_version.clone(), |(version, _)| version.clone()),
                    LspRuntimeSource::Managed => managed_definition
                        .version
                        .clone()
                        .unwrap_or_else(|| reported_version.clone()),
                    LspRuntimeSource::Nix => managed_definition
                        .version
                        .clone()
                        .unwrap_or_else(|| reported_version.clone()),
                };
                let mut release = match source {
                    LspRuntimeSource::Path => path_presentation.map_or_else(
                        || {
                            if path_version_reported {
                                "reported by executable".into()
                            } else {
                                "—".into()
                            }
                        },
                        |(_, release)| release,
                    ),
                    LspRuntimeSource::Managed => managed_definition
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                    LspRuntimeSource::Nix => managed_definition
                        .release
                        .clone()
                        .unwrap_or_else(|| "catalog build".into()),
                };
                let (session_state, sessions) = if is_lsp {
                    self.lsp_menu_lifecycle(&definition.id)
                } else {
                    (String::new(), Vec::new())
                };
                let installed = match source {
                    LspRuntimeSource::Path => path.is_some(),
                    LspRuntimeSource::Managed => managed.is_some(),
                    LspRuntimeSource::Nix => {
                        nix_lsp_profile_installed(definition)
                            && self.managed_variant_installed(definition, &managed_definition)
                    },
                };
                let can_install = if is_lsp && nixos_host {
                    self.nix_workspace_available_for(definition)
                } else {
                    !definition.managed_versions().is_empty()
                };
                let can_uninstall = if is_lsp && nixos_host {
                    installed
                } else {
                    managed.is_some()
                };
                let error = self
                    .language
                    .errors()
                    .get(&definition.id)
                    .cloned()
                    .or_else(|| probe_error.clone())
                    .or_else(|| match &entry.status {
                        ToolStatus::Failed(error) | ToolStatus::Unavailable(error) => {
                            Some(error.clone())
                        }
                        _ => None,
                    });
                // One state per row, from a fixed set, so the column always reads the same way.
                let lifecycle = if let ToolStatus::Installing {
                    phase,
                    completed,
                    total,
                } = &entry.status
                {
                    tool_install_progress_label(phase, *completed, *total)
                } else if !installed {
                    if matches!(entry.status, ToolStatus::Failed(_)) {
                        "install failed".into()
                    } else {
                        "not installed".into()
                    }
                } else if source == LspRuntimeSource::Path && probe_error.is_some() {
                    "not working".into()
                } else if is_lsp && session_state != "stopped" {
                    session_state.clone()
                } else {
                    "installed".into()
                };
                if !installed && source == LspRuntimeSource::Path {
                    version = "—".into();
                    release = "—".into();
                }
                let version = short_tool_version(&version);
                let runtime = if installed || can_install {
                    source.label().into()
                } else {
                    "—".into()
                };
                let executable = match (&source, installed) {
                    (_, false) => String::new(),
                    (LspRuntimeSource::Path, true) => path
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string()),
                    (LspRuntimeSource::Managed | LspRuntimeSource::Nix, true) => managed
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string()),
                };
                let note = if let Some(error) = error {
                    error
                } else if matches!(entry.status, ToolStatus::Installing { .. }) {
                    format!("Installing {} {version}.", definition.name)
                } else if installed {
                    match source {
                        LspRuntimeSource::Path => {
                            "Found on PATH. Dust uses it as is and never removes it.".into()
                        }
                        LspRuntimeSource::Managed => "Installed by Dust.".into(),
                        LspRuntimeSource::Nix => "Installed by Dust through Nix.".into(),
                    }
                } else if can_install {
                    format!("Not installed. Press i to install {version}.")
                } else {
                    "Dust has no installer for this tool. Install it with your package manager \
                     so it is on PATH."
                        .into()
                };
                ToolMenuRow {
                    id: definition.id.clone(),
                    name: definition.name.clone(),
                    kind: configured
                        .categories
                        .get(tool_kind_key(definition.kind))
                        .cloned()
                        .unwrap_or_else(|| definition.kind.label().into()),
                    languages: definition.languages.join(&configured.language_separator),
                    version,
                    release,
                    runtime,
                    lifecycle,
                    session_state,
                    executable,
                    note,
                    sessions,
                    lsp: is_lsp,
                    installed,
                    can_start: installed
                        && (!is_lsp
                            || active_language.as_ref().is_none_or(|language| {
                                definition.supports_language(language)
                            })),
                    can_install,
                    can_uninstall,
                }
            })
            .collect::<Vec<_>>();
        let selected = selected_id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .or_else(|| {
                self.ui
                    .tool_menu
                    .as_ref()
                    .and_then(|menu| menu.rows.get(menu.selected))
                    .and_then(|previous| rows.iter().position(|row| row.id == previous.id))
            })
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1));
        let runtime_picker = previous_picker.and_then(|(server_id, focused, scroll)| {
            let definition = self
                .tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.id == server_id && entry.definition.kind == ToolKind::Lsp
                })
                .map(|entry| entry.definition.clone())?;
            let options = self.runtime_options_for(&definition);
            let selected = focused
                .and_then(|(source, version_pin)| {
                    options.iter().position(|option| {
                        option.source == source && option.version_pin == version_pin
                    })
                })
                .unwrap_or_else(|| {
                    options
                        .iter()
                        .position(|option| option.selected)
                        .unwrap_or(0)
                });
            Some(ToolRuntimePickerState {
                server_id,
                selected,
                scroll: scroll.min(options.len().saturating_sub(1)),
                options,
            })
        });
        let visible_rows = self.tool_menu_list_visible_rows();
        let mut scroll = previous_scroll.min(rows.len().saturating_sub(1));
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll.saturating_add(visible_rows) {
            scroll = selected.saturating_add(1).saturating_sub(visible_rows);
        }
        self.ui.tool_menu = Some(ToolMenuState {
            category,
            selected,
            scroll,
            rows,
            runtime_picker,
            query,
            searching,
        });
    }

    /// Set the tool search and whether keys still type into it, then filter the list again.
    pub(in crate::editor::session) fn search_tool_menu(&mut self, query: Option<String>, searching: bool) {
        let Some(menu) = self.ui.tool_menu.as_mut() else {
            return;
        };
        menu.query = query.unwrap_or_default();
        menu.searching = searching;
        let category = menu.category.clone();
        self.rebuild_tool_menu(&category, None);
    }

    pub(in crate::editor::session) fn move_tool_menu(&mut self, direction: i8) {
        let list_visible = self.tool_menu_list_visible_rows();
        let picker_visible = self.tool_runtime_picker_visible_rows();
        let Some(menu) = &mut self.ui.tool_menu else {
            return;
        };
        if let Some(picker) = &mut menu.runtime_picker {
            if picker.options.is_empty() {
                return;
            }
            picker.selected = if direction < 0 {
                picker.selected.saturating_sub(1)
            } else {
                picker
                    .selected
                    .saturating_add(1)
                    .min(picker.options.len() - 1)
            };
            let visible = picker_visible;
            if picker.selected < picker.scroll {
                picker.scroll = picker.selected;
            } else if picker.selected >= picker.scroll.saturating_add(visible) {
                picker.scroll = picker.selected.saturating_add(1).saturating_sub(visible);
            }
            return;
        }
        if menu.rows.is_empty() {
            return;
        }
        menu.selected = if direction < 0 {
            menu.selected.saturating_sub(1)
        } else {
            menu.selected.saturating_add(1).min(menu.rows.len() - 1)
        };
        let visible = list_visible;
        if menu.selected < menu.scroll {
            menu.scroll = menu.selected;
        } else if menu.selected >= menu.scroll.saturating_add(visible) {
            menu.scroll = menu.selected.saturating_add(1).saturating_sub(visible);
        }
    }

    pub(in crate::editor::session) fn cycle_tool_category(&mut self, direction: i8) {
        let Some(menu) = &self.ui.tool_menu else {
            return;
        };
        if menu.runtime_picker.is_some() {
            return;
        }
        let order = &self.configuration.settings().ui.tool_manager.category_order;
        let current = order
            .iter()
            .position(|category| category == &menu.category)
            .unwrap_or(0);
        let next = if direction < 0 {
            if current == 0 {
                order.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % order.len()
        };
        let category = order[next].clone();
        self.rebuild_tool_menu(&category, None);
    }

    pub(in crate::editor::session) fn selected_tool_id(&self) -> Option<String> {
        let menu = self.ui.tool_menu.as_ref()?;
        menu.runtime_picker
            .as_ref()
            .map(|picker| picker.server_id.clone())
            .or_else(|| menu.rows.get(menu.selected).map(|row| row.id.clone()))
    }

    pub(in crate::editor::session) fn focused_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .filter(|picker| picker.server_id == definition.id)
            .and_then(|picker| picker.options.get(picker.selected))
            .and_then(|option| match option.source.as_str() {
                "path" => Some(LspRuntimeSource::Path),
                "managed" => Some(LspRuntimeSource::Managed),
                "nix" => Some(LspRuntimeSource::Nix),
                _ => None,
            })
            .unwrap_or_else(|| self.selected_lsp_runtime_source(definition))
    }

    pub(in crate::editor::session) fn focused_managed_version(
        &self,
        definition: &ToolDefinition,
    ) -> Option<String> {
        self.ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.runtime_picker.as_ref())
            .filter(|picker| picker.server_id == definition.id)
            .and_then(|picker| picker.options.get(picker.selected))
            .filter(|option| {
                matches!(
                    option.source.as_str(),
                    source if source == LspRuntimeSource::Managed.key()
                        || source == LspRuntimeSource::Nix.key()
                )
            })
            .and_then(|option| option.version_pin.clone())
            .or_else(|| self.selected_managed_definition(definition).version)
    }

    pub(in crate::editor::session) fn toggle_selected_lsp_runtime(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let Some(definition) = self
            .tooling
            .entries()
            .iter()
            .find(|entry| entry.definition.id == id)
            .map(|entry| entry.definition.clone())
        else {
            return;
        };
        if definition.kind != ToolKind::Lsp {
            self.show_error("runtime selection is available only for language servers");
            return;
        }
        let options = self.runtime_options_for(&definition);
        let selected = options
            .iter()
            .position(|option| option.selected)
            .unwrap_or(0);
        if let Some(menu) = &mut self.ui.tool_menu {
            menu.runtime_picker = Some(ToolRuntimePickerState {
                server_id: id,
                selected,
                scroll: selected.saturating_sub(2),
                options,
            });
        }
    }

    pub(in crate::editor::session) fn accept_selected_lsp_runtime(&mut self) {
        let Some((id, source, version_pin, installed, installable)) =
            self.ui.tool_menu.as_ref().and_then(|menu| {
                let picker = menu.runtime_picker.as_ref()?;
                let option = picker.options.get(picker.selected)?;
                Some((
                    picker.server_id.clone(),
                    option.source.clone(),
                    option.version_pin.clone(),
                    option.installed,
                    option.installable,
                ))
            })
        else {
            return;
        };
        let source = match source.as_str() {
            "path" => LspRuntimeSource::Path,
            "managed" => LspRuntimeSource::Managed,
            "nix" => LspRuntimeSource::Nix,
            _ => return,
        };
        if !installed && !installable {
            self.show_error("this runtime is not installed or installable on the current host");
            return;
        }
        if let Err(error) = self.tooling.select_runtime(&id, source, version_pin) {
            self.show_error(error);
            return;
        }
        self.stop_lsp_sessions_for_server(&id);
        self.language.reset_autostart();
        if let Some(menu) = &mut self.ui.tool_menu {
            menu.runtime_picker = None;
        }
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        if !installed && installable {
            self.rebuild_tool_menu(&category, Some(&id));
            self.install_tool_by_id(&id, Some(source));
            return;
        }
        self.notify_message(
            match source {
                LspRuntimeSource::Managed => format!(
                    "{id} now uses Dust {}",
                    self.tooling
                        .managed_version(&id)
                        .map(String::as_str)
                        .unwrap_or("unknown")
                ),
                _ => format!("{id} now uses {}", source.label()),
            },
            false,
        );
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn close_tool_menu(&mut self) {
        let Some(menu) = &mut self.ui.tool_menu else {
            return;
        };
        if menu.runtime_picker.take().is_some() {
            let category = menu.category.clone();
            let selected = menu.rows.get(menu.selected).map(|row| row.id.clone());
            self.rebuild_tool_menu(&category, selected.as_deref());
        } else {
            self.ui.tool_menu = None;
        }
    }

    pub(in crate::editor::session) fn stop_lsp_sessions_for_server(&mut self, id: &str) {
        let sessions = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, session)| (session.server_id == id).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in sessions {
            self.language.request_stop(&key);
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.clear_lsp_session_state();
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn refresh_tools(&mut self) {
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "all".into(), |menu| menu.category.clone());
        let selected = self.selected_tool_id();
        self.tooling.refresh();
        self.queue_tool_version_probes(&category);
        self.rebuild_tool_menu(&category, selected.as_deref());
    }

    pub(in crate::editor::session) fn process_tool_version_results(&mut self) {
        let (changed, unavailable_rustup) = self.tooling.poll_versions();
        if changed {
            self.language.reset_autostart();
            let category = self.ui.tool_menu.as_ref().map(|menu| menu.category.clone());
            let selected = self.selected_tool_id();
            if let Some(category) = category {
                self.rebuild_tool_menu(&category, selected.as_deref());
            }
        }
        if unavailable_rustup {
            self.show_error(Self::unavailable_rust_analyzer_message());
        }
    }

    pub(in crate::editor::session) fn activate_selected_tool(&mut self) {
        if self
            .ui
            .tool_menu
            .as_ref()
            .is_some_and(|menu| menu.runtime_picker.is_some())
        {
            self.accept_selected_lsp_runtime();
            return;
        }
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.activate_tool_by_id(&id);
    }

    pub(in crate::editor::session) fn activate_tool_by_id(&mut self, id: &str) {
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let definition = self.tooling.entries()[index].definition.clone();
        let path_available = matches!(
            self.tooling.entries()[index].status,
            ToolStatus::Installed(_)
        );
        if definition.kind == ToolKind::Lsp {
            if self.selected_lsp_runtime_available(&definition) {
                if self.active_lsp_document_without_text().is_some_and(|(_, document)| {
                    !definition.supports_language(&document.language_id)
                }) {
                    self.notify_message(
                        format!(
                            "{} is installed. Open a supported file to start it.",
                            definition.name
                        ),
                        false,
                    );
                } else {
                    self.launch_lsp(Some(id.into()), true);
                }
            } else if definition.managed_versions().is_empty() {
                self.show_error(format!(
                    "{} is not installed and Dust has no installer for it",
                    definition.name
                ));
            } else {
                let source = self.selected_lsp_runtime_source(&definition);
                if source == LspRuntimeSource::Path && definition.id == "rust-analyzer" {
                    if let Some(path) = discover_executable(&definition.executable) {
                        if Self::rust_analyzer_is_rustup_proxy(&path) {
                            let key = Self::tool_version_key(&definition, source, Some(&path));
                            if matches!(self.tooling.version(&key), Some(Err(_))) {
                                self.show_error(Self::unavailable_rust_analyzer_message());
                                return;
                            }
                        }
                    }
                }
                self.install_tool_by_id(id, Some(source));
            }
            return;
        }
        if path_available {
            match definition.kind {
                ToolKind::Lsp => unreachable!("handled before installed-tool dispatch"),
                ToolKind::Dap => self.start_dap(Some(id.into()), None),
                ToolKind::Agent => self.start_agent_for_program(&definition.executable),
                ToolKind::Formatter | ToolKind::Linter => {
                    self.notify_message(
                        self.message(
                            "tool_install_succeeded",
                            &[("tool", definition.name.clone())],
                        ),
                        false,
                    );
                }
            }
            return;
        }
        if let ToolStatus::Unavailable(diagnostic) = &self.tooling.entries()[index].status {
            self.show_error(diagnostic.clone());
            return;
        }
        if definition.managed_versions().is_empty() {
            self.show_error(format!(
                "{} is not installed and Dust has no installer for it",
                definition.name
            ));
            return;
        }
        self.install_tool_by_id(id, None);
    }

    pub(in crate::editor::session) fn install_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let focused = self
            .tooling
            .entries()
            .iter()
            .find(|entry| entry.definition.id == id)
            .filter(|entry| entry.definition.kind == ToolKind::Lsp)
            .map(|entry| {
                (
                    self.focused_runtime_source(&entry.definition),
                    self.focused_managed_version(&entry.definition),
                )
            });
        let source = focused.as_ref().map(|(source, _)| *source);
        if let Some((source, Some(version))) = focused {
            if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
                if let Err(error) = self.tooling.select_version(&id, version) {
                    self.show_error(error);
                    return;
                }
            }
        }
        self.install_tool_by_id(&id, source);
    }

    pub(in crate::editor::session) fn install_tool_by_id(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        self.install_tool(id, source);
    }
    pub(in crate::editor::session) fn install_tool(
        &mut self,
        id: &str,
        source: Option<LspRuntimeSource>,
    ) {
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tooling.entries()[index].definition.clone();
        let source = source.unwrap_or(LspRuntimeSource::Managed);
        if base_definition.kind == ToolKind::Lsp
            && !lsp_runtime_source_allowed_on_host(source, is_nixos_host())
        {
            self.show_error(if is_nixos_host() {
                "NixOS language servers are installed only through Dust's private Nix compatibility runtime"
            } else {
                "non-NixOS language servers are installed only as verified Dust downloads"
            });
            return;
        }
        let definition = if base_definition.kind == ToolKind::Lsp
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
        {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition
        };
        let rustup_install = if definition.id == "rust-analyzer" && source == LspRuntimeSource::Path
        {
            discover_executable(&definition.executable).and_then(|proxy| {
                if !Self::rust_analyzer_is_rustup_proxy(&proxy) {
                    return None;
                }
                let key = Self::tool_version_key(&definition, source, Some(&proxy));
                let rustup = proxy.with_file_name("rustup");
                (matches!(self.tooling.version(&key), Some(Err(_))) && rustup.is_file())
                    .then_some((rustup, proxy))
            })
        } else {
            None
        };
        if definition.kind == ToolKind::Lsp {
            match source {
                LspRuntimeSource::Path if rustup_install.is_none() => {
                    if discover_executable(&definition.executable).is_some() {
                        self.notify_message(format!(
                            "{} is already installed on PATH; Dust leaves external software unchanged",
                            definition.name
                        ), false);
                    } else {
                        self.show_error(format!(
                            "{} is not installed and has no Dust-managed build",
                            definition.name
                        ));
                    }
                    return;
                }
                LspRuntimeSource::Path => {}
                LspRuntimeSource::Managed
                    if self.managed_variant_installed(
                        &self.tooling.entries()[index].definition,
                        &definition,
                    ) =>
                {
                    self.notify_message(
                        format!(
                            "{} {} is already installed",
                            definition.name,
                            definition.version.as_deref().unwrap_or("unknown")
                        ),
                        false,
                    );
                    return;
                }
                LspRuntimeSource::Nix
                    if nix_lsp_profile_installed(&definition)
                        && self.managed_variant_installed(
                            &self.tooling.entries()[index].definition,
                            &definition,
                        ) =>
                {
                    self.notify_message(format!(
                        "{} {} is already installed through Dust's Nix compatibility environment",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ), false);
                    return;
                }
                LspRuntimeSource::Managed | LspRuntimeSource::Nix => {}
            }
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ToolInstall)
        {
            self.show_error(error);
            return;
        }
        let operation = if let Some((rustup, proxy)) = rustup_install {
            let Some(cwd) = self.workspace.active_root().and_then(|root| {
                self.workspace
                    .roots()
                    .iter()
                    .find(|candidate| candidate.id == root)
                    .map(|root| root.identity.canonical_path.clone())
            }) else {
                self.show_error("rustup installation requires an active workspace root");
                return;
            };
            self.tooling.install(ToolInstall::Rustup {
                definition: definition.clone(),
                rustup,
                proxy,
                cwd,
            })
        } else if definition.kind == ToolKind::Lsp && source == LspRuntimeSource::Nix {
            let Some(nix) = discover_executable("nix") else {
                self.show_error("Dust via Nix is unavailable: nix was not found");
                return;
            };
            if !self.nix_workspace_available_for(&definition) {
                self.show_error("no verified upstream artifact or Dust Nix runtime is available for this language server");
                return;
            }
            self.tooling
                .install(ToolInstall::Nix(definition.clone(), nix))
        } else {
            if let ToolStatus::Unavailable(diagnostic) = &self.tooling.entries()[index].status {
                self.show_error(diagnostic.clone());
                return;
            }
            if definition.install.is_none() {
                self.show_error(self.message("tool_no_installer", &[("tool", definition.name)]));
                return;
            }
            self.tooling
                .install(ToolInstall::Managed(definition.clone()))
        };
        match operation {
            Ok(()) => {
                self.notify_message(
                    match (source, definition.version.as_deref()) {
                        (LspRuntimeSource::Path, _) => {
                            format!("installing {} with rustup…", definition.name)
                        }
                        (_, Some(version)) => format!("installing {} {version}…", definition.name),
                        (_, None) => format!("installing {}…", definition.name),
                    },
                    false,
                );
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(id));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn uninstall_selected_tool(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        let Some(index) = self
            .tooling
            .entries()
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        let base_definition = self.tooling.entries()[index].definition.clone();
        let source = if base_definition.kind == ToolKind::Lsp {
            self.focused_runtime_source(&base_definition)
        } else {
            LspRuntimeSource::Managed
        };
        if base_definition.kind == ToolKind::Lsp
            && !lsp_runtime_source_allowed_on_host(source, is_nixos_host())
        {
            self.show_error("that installation mechanism is not used on this host");
            return;
        }
        let definition = if base_definition.kind == ToolKind::Lsp
            && matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
        {
            self.focused_managed_version(&base_definition)
                .as_deref()
                .and_then(|version| base_definition.managed_version(version))
                .unwrap_or_else(|| base_definition.clone())
        } else {
            base_definition.clone()
        };
        let operation =
            match source {
                LspRuntimeSource::Path => {
                    self.notify_message(
                    "This server is owned by PATH, not Dust; remove it with its package manager"
                        .into(), false);
                    return;
                }
                LspRuntimeSource::Managed => {
                    if !self.managed_variant_installed(&base_definition, &definition) {
                        self.show_error(format!(
                            "Dust-download version {} is not installed",
                            definition.version.as_deref().unwrap_or("unknown")
                        ));
                        return;
                    }
                    self.stop_lsp_sessions_for_server(&id);
                    self.tooling
                        .install(ToolInstall::Uninstall(definition.clone(), false))
                }
                LspRuntimeSource::Nix => {
                    if !nix_lsp_profile_installed(&definition)
                        || !self.managed_variant_installed(&base_definition, &definition)
                    {
                        self.show_error(format!(
                            "Dust via Nix version {} is not installed",
                            definition.version.as_deref().unwrap_or("unknown")
                        ));
                        return;
                    }
                    self.stop_lsp_sessions_for_server(&id);
                    self.tooling
                        .install(ToolInstall::Uninstall(definition.clone(), true))
                }
            };
        match operation {
            Ok(()) => {
                self.notify_message(
                    format!(
                        "uninstalling {} {} runtime…",
                        definition.name,
                        source.label()
                    ),
                    false,
                );
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(&id));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn stop_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.stop_lsp_sessions_for_server(&id);
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn restart_selected_lsp(&mut self) {
        let Some(id) = self.selected_tool_id() else {
            return;
        };
        self.stop_lsp_sessions_for_server(&id);
        self.launch_lsp(Some(id.clone()), true);
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
        self.rebuild_tool_menu(&category, Some(&id));
    }

    pub(in crate::editor::session) fn process_tool_progress(&mut self) {
        let changed = self.tooling.poll_progress();
        if let (Some(id), Some(menu)) = (changed, &self.ui.tool_menu) {
            self.rebuild_tool_menu(&menu.category.clone(), Some(&id));
        }
    }

    pub(in crate::editor::session) fn process_tool_results(&mut self) {
        while let Some((name, result)) = self.tooling.poll_install() {
            if let Err(diagnostic) = result.result {
                self.show_error(format!(
                    "{} {} failed: {diagnostic}",
                    match result.operation {
                        ToolOperation::InstallManaged
                        | ToolOperation::InstallRustup
                        | ToolOperation::InstallNix => "installing",
                        ToolOperation::UninstallManaged | ToolOperation::UninstallNix => {
                            "uninstalling"
                        }
                    },
                    name
                ));
            } else {
                self.notify_message(
                    match result.operation {
                        ToolOperation::InstallRustup => {
                            format!("installed {name} in the active rustup toolchain")
                        }
                        ToolOperation::InstallManaged => {
                            format!("installed {name} Dust download runtime")
                        }
                        ToolOperation::InstallNix => {
                            format!("installed {name} Dust via Nix runtime")
                        }
                        ToolOperation::UninstallManaged => {
                            format!("uninstalled {name} Dust download runtime")
                        }
                        ToolOperation::UninstallNix => {
                            format!("uninstalled {name} Dust via Nix runtime")
                        }
                    },
                    false,
                );
                self.language.clear_error(&result.id);
            }
            self.language.reset_autostart();
            if let Some(menu) = &self.ui.tool_menu {
                let category = menu.category.clone();
                self.queue_tool_version_probes(&category);
                self.rebuild_tool_menu(&category, Some(&result.id));
            }
        }
    }
}

/// The first word of a reported version, so `0.1.99 (58cf8f9 2025-01-01)` reads `0.1.99`.
fn short_tool_version(version: &str) -> String {
    match version {
        "version unavailable" | "probe failed" | "unknown" | "" => "—".into(),
        _ => version.split_whitespace().next().unwrap_or("—").to_owned(),
    }
}

pub(in crate::editor::session) fn tool_install_progress_label(
    phase: &str,
    completed: u64,
    total: Option<u64>,
) -> String {
    let bytes = |value: u64| {
        if value >= 1024 * 1024 {
            format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} KiB", value / 1024)
        }
    };
    // The percentage comes first so a narrow column still shows progress.
    match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "{phase} {}% ({} / {})",
            completed.saturating_mul(100).min(total.saturating_mul(100)) / total,
            bytes(completed),
            bytes(total),
        ),
        None => phase.into(),
    }
}

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
                RegistryUpdate::ValidateArtifact => {
                    match self.workspace.execution_allowed() {
                        Ok(()) => self.run_trusted(TrustedAction::PrepareRegistryArtifact),
                        Err(failure) => {
                            self.show_workspace_failure(failure);
                            self.extension.discard_artifact();
                        }
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

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_lsp_status_ui(&mut self) {
        let running = self.language.running_servers();
        let server = self
            .language
            .active()
            .and_then(|server| {
                self.language
                    .sessions()
                    .get(server)
                    .map(|session| (server, session))
            })
            .map(|(server, session)| {
                if session.capabilities.is_some() {
                    session.server_id.clone()
                } else if session.circuit_open {
                    format!("failed {}", session.server_id)
                } else if session.restart_due.is_some() {
                    format!("backoff {}", session.server_id)
                } else if running.contains(server) {
                    format!("starting {}", session.server_id)
                } else {
                    format!("stopped {}", session.server_id)
                }
            });
        self.ui.set(server, |ui| &mut ui.lsp_server);
        let refresh = self.ui.tool_menu.as_ref().is_some_and(|menu| {
            menu.rows.iter().any(|row| {
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.id == row.id && entry.definition.kind == ToolKind::Lsp
                    })
                    .is_some_and(|entry| {
                        !matches!(entry.status, ToolStatus::Installing { .. })
                            && self.lsp_menu_lifecycle(&row.id).0 != row.session_state
                    })
            })
        });
        if refresh {
            let category = self
                .ui
                .tool_menu
                .as_ref()
                .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
            let selected = self.selected_tool_id();
            self.rebuild_tool_menu(&category, selected.as_deref());
        }
    }

    pub(in crate::editor::session) fn reconcile_lsp_autostart(
        &mut self,
        buffer: crate::editor::BufferId,
        document: &LspDocument,
        matching_session: bool,
    ) {
        // A server runs for a workspace root; a file outside every root, such as the
        // tutorial's copy, gets none rather than an error.
        if self.workspace.require_path(&document.path).is_err() {
            return;
        }
        // Every matching server attaches, as in VS Code and Neovim: Tailwind runs
        // beside the HTML or TypeScript server. They start one at a time because
        // project-root discovery takes one launch at a time.
        let attached = self
            .language
            .sessions()
            .values()
            .filter(|state| state.documents.contains_key(&buffer))
            .map(|state| state.server_id.clone())
            .collect::<Vec<_>>();
        let mut launchable = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| {
                entry.definition.kind == ToolKind::Lsp
                    && entry
                        .definition
                        .automatically_supports_language(&document.language_id)
                    && self.selected_lsp_runtime_available(&entry.definition)
            })
            .collect::<Vec<_>>();
        // The most specific server starts first and stays primary for hover and
        // definitions: typescript-language-server before Tailwind.
        launchable.sort_by_key(|entry| entry.definition.languages.len());
        let launchable = launchable
            .into_iter()
            .map(|entry| entry.definition.id.clone())
            .collect::<Vec<_>>();
        if launchable
            .iter()
            .any(|id| self.language.launch_pending(id))
        {
            return;
        }
        let launchable_server = launchable
            .iter()
            .find(|id| !attached.contains(id))
            .cloned();
        // Like mason-lspconfig, point at the server to install when none can start.
        if launchable.is_empty()
            && !matching_session
            && self.configuration.settings().lsp.enabled
        {
            let installable = self
                .tooling
                .entries()
                .iter()
                .filter(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && entry.status == ToolStatus::Available
                })
                // The most specific server: zls for Zig, not a spell checker that covers it.
                .min_by_key(|entry| entry.definition.languages.len())
                .map(|entry| entry.definition.name.clone());
            if let Some(server) = installable {
                if self.language.hint_install_once(&document.language_id) {
                    self.notify_message(
                        self.message(
                            "lsp_install_hint",
                            &[("server", server), ("language", document.language_id.clone())],
                        ),
                        false,
                    );
                }
            }
        }
        let observation = LspAutostartObservation {
            buffer,
            path: document.path.clone(),
            language: document.language_id.clone(),
            root_revision: self.workspace.root_revision(),
            trust: self.workspace.trust(),
            enabled: self.configuration.settings().lsp.enabled,
            auto_start: self.configuration.settings().lsp.auto_start,
            nix_workspace_fallback: self.configuration.settings().lsp.nix_workspace_fallback,
            launchable_server,
        };
        if let Some(server) = self.language.observe(observation, self.workspace.safe_mode()) {
            self.start_lsp(Some(server));
        }
    }

    pub(in crate::editor::session) fn sync_lsp_document(&mut self) {
        let Some(buffer) = self.state.active_buffer_id() else {
            self.language.reset_autostart();
            return;
        };
        // The text is copied only for a server that needs it, not on every pass.
        let Some((_, mut document)) = self.active_lsp_document_without_text() else {
            let stale = self
                .language
                .sessions()
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .documents
                        .get(&buffer)
                        .map(|document| (key.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer, stale);
            self.clear_lsp_ui_projection();
            self.language.clear_active();

            self.language.reset_autostart();
            return;
        };
        let revision = self.state.buffers[buffer].text.source_version();
        let mut sessions = Vec::new();
        let mut detach = Vec::new();
        for (key, state) in self.language.sessions() {
            // A lexical `starts_with` check would let a session for an outer
            // project absorb a nested, independently resolved project. Only
            // the asynchronous resolver may attach a new document to a server.
            let owns = state.documents.contains_key(&buffer);
            let supports = state.definition.supports_language(&document.language_id);
            if owns && supports {
                sessions.push(key.clone());
                if let Some(previous) = state.documents.get(&buffer) {
                    if previous.path != document.path
                        || previous.language_id != document.language_id
                    {
                        detach.push((key.clone(), previous.uri()));
                    }
                }
            } else if let Some(previous) = state.documents.get(&buffer) {
                detach.push((key.clone(), previous.uri()));
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        if !sessions.is_empty()
            && !sessions
                .iter()
                .any(|key| self.language.active() == Some(key))
        {
            // A full language server rather than a linter such as ESLint names the file's server.
            let primary = sessions
                .iter()
                .find(|key| {
                    self.language
                        .sessions()
                        .get(*key)
                        .and_then(|session| session.capabilities.as_ref())
                        .is_some_and(|capabilities| capabilities.definition)
                })
                .unwrap_or(&sessions[0])
                .clone();
            self.select_lsp_session(&primary);
        }
        self.detach_lsp_documents(buffer, detach);
        let completion_open = self.ui.language_completion.is_some()
            && self.language.projection().is_some_and(|owner| {
                owner.buffer == buffer
                    && Some(owner.view) == self.state.active_view
                    && self.language.active() == Some(&owner.server)
                    && owner.uri == document.uri()
            });
        self.refresh_lsp_projection_owner();
        let mut changed = false;
        for server in &sessions {
            if self.language.synced(server, buffer, &revision) {
                continue;
            }
            if document.text.is_empty() {
                document.text = self.state.buffers[buffer].text.to_string();
            }
            if self
                .language
                .sync_owned(server, document.clone(), (buffer, revision.clone()))
                .is_ok()
            {
                changed = true;
            }
        }
        if changed {
            self.request_lsp_colors();
        }
        if changed
            && !(self.configuration.settings().lsp.inlay_hint_level
                == crate::config::InlayHintLevel::Max
                && self.ui.lsp_inlay_hints.is_empty()
                && self.language.inlay_pending())
        {
            self.schedule_lsp_inlay_refresh(false);
        }
        self.reconcile_lsp_autostart(buffer, &document, !sessions.is_empty());
        let typed = self.language.take_typed();
        let active_attached = self
            .language
            .active()
            .is_some_and(|active| sessions.contains(active));
        let trigger = typed.is_some_and(|typed| self.language.is_completion_trigger(&sessions, typed));
        if completion_open {
            if changed && trigger && active_attached {
                self.mark_lsp_completion_start(true);
                self.request_lsp(LspFeature::Completion);
            } else if changed && self.language.completion_incomplete() && active_attached {
                self.request_lsp(LspFeature::Completion);
            }
            self.narrow_lsp_completion();
        } else if changed
            && self.configuration.settings().lsp.auto_completion
            && !sessions.is_empty()
            && !self.ui.large_file_buffers.contains(&buffer)
            && self.lsp_completion_should_open(typed, trigger)
        {
            self.mark_lsp_completion_start(true);
            self.request_lsp(LspFeature::Completion);
        }
    }

    /// Whether a typed character opens the menu: a server's trigger character, or a letter that
    /// starts or continues a word. Numbers don't, and a request already out will show its answer.
    fn lsp_completion_should_open(&self, typed: Option<char>, trigger: bool) -> bool {
        if !self.lsp_completion_in_insert() {
            return false;
        }
        if trigger {
            return true;
        }
        typed.is_some_and(completion_word_char)
            && !self.language.request_pending()
            && self
                .lsp_word_before_cursor()
                .and_then(|word| word.chars().next())
                .is_some_and(|first| !first.is_numeric())
    }

    fn lsp_completion_in_insert(&self) -> bool {
        self.state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .is_some_and(|view| view.mode == EditorMode::Insert)
    }

    /// The word the cursor is at the end of, with where it starts.
    fn lsp_word_before_cursor(&self) -> Option<String> {
        let start = self.lsp_word_start()?;
        let view = &self.state.views[self.state.active_view?];
        let range = ByteRange::new(start, view.cursor).ok()?;
        self.state.buffers.get(view.buffer)?.text.slice(range).ok()
    }

    fn lsp_word_start(&self) -> Option<ByteOffset> {
        let view = &self.state.views[self.state.active_view?];
        let text = &self.state.buffers.get(view.buffer)?.text;
        let line_start = text.line_to_byte(text.byte_to_line(view.cursor).ok()?).ok()?;
        let before = text.slice(ByteRange::new(line_start, view.cursor).ok()?).ok()?;
        let word = before
            .char_indices()
            .rev()
            .take_while(|(_, character)| completion_word_char(*character))
            .last()
            .map_or(before.len(), |(index, _)| index);
        Some(ByteOffset(line_start.get() + word))
    }

    /// Notes where this completion starts, which later typing is measured from.
    pub(in crate::editor::session) fn mark_lsp_completion_start(&mut self, automatic: bool) {
        let start = self.lsp_word_start();
        self.language.begin_completion(start, automatic);
    }

    pub(in crate::editor::session) fn cancel_lsp_completion(&mut self) {
        let generation = self.language.generation();
        if let Some(server) = self.language.active().map(String::as_str) {
            let _ = self.language.cancel(server, generation);
        }
        self.language.clear_completion();
        self.ui.language_completion = None;
    }

    /// Shows the items matching what was typed since the request, best first. The menu closes
    /// when the cursor leaves the word or nothing matches.
    pub(in crate::editor::session) fn narrow_lsp_completion(&mut self) {
        let Some(view) = self.state.active_view.and_then(|view| self.state.views.get(view)) else {
            return;
        };
        let Some(buffer) = self.state.buffers.get(view.buffer) else {
            return;
        };
        let cursor = view.cursor;
        let revision = buffer.text.revision();
        let automatic_left_insert = self.language.completion_automatic() && view.mode != EditorMode::Insert;
        let start = self.language.completion_start();
        let same_line = |offset: ByteOffset| {
            buffer.text.byte_to_line(offset).ok() == buffer.text.byte_to_line(cursor).ok()
        };
        if automatic_left_insert
            || start.is_some_and(|start| start > cursor || !same_line(start))
        {
            self.cancel_lsp_completion();
            return;
        }
        if !self.language.completion_needs_narrowing(cursor, revision) {
            return;
        }
        let codec = PositionCodec::new(&buffer.text);
        let typed = |from: ByteOffset| {
            ByteRange::new(from, cursor)
                .ok()
                .and_then(|range| buffer.text.slice(range).ok())
                .unwrap_or_default()
        };
        let from_start = start.map(typed).unwrap_or_default();
        let encodings = self.language.completion_encodings();
        let matches = self.language.score_completions(|server, item| {
            // A server's range says where its item begins; other items start at the word.
            let edit_start = item.text_edit.as_ref().and_then(|edit| {
                let encoding = encodings.get(server).copied()?;
                codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .ok()
                    .filter(|offset| *offset <= cursor && same_line(*offset))
            });
            edit_start.map_or_else(|| from_start.clone(), typed)
        });
        self.language.narrow_completions(matches, cursor, revision);
        self.project_lsp_completion();
    }

    /// Mirrors the current items into the menu the renderer draws.
    pub(in crate::editor::session) fn project_lsp_completion(&mut self) {
        let items = self
            .language
            .completion_items()
            .iter()
            .enumerate()
            .map(|(index, item)| CompletionItem {
                label: item.label.clone(),
                detail: Self::lsp_completion_detail(
                    item,
                    &self.lsp_server_label(self.language.completion_server(index).unwrap_or_default()),
                ),
                color: item.color(),
            })
            .collect::<Vec<_>>();
        let (selected, scroll) = self.language.completion_position();
        self.ui.language_completion = (!items.is_empty()).then_some(LanguageCompletionState {
            items,
            selected,
            scroll,
        });
        self.refresh_lsp_completion_documentation();
    }

    /// `<leader>cr` and F2: the command line, ready with the name under the cursor.
    fn word_at_cursor(&self) -> Option<String> {
        self.state.active_view.and_then(|view| self.state.views.get(view)).and_then(|view| {
            let source = self.state.buffers.get(view.buffer)?.text.to_string();
            let cursor = view.cursor.get();
            let start = source[..cursor]
                .char_indices()
                .rev()
                .take_while(|(_, character)| completion_word_char(*character))
                .last()
                .map_or(cursor, |(index, _)| index);
            let end = source[cursor..]
                .char_indices()
                .find(|(_, character)| !completion_word_char(*character))
                .map_or(source.len(), |(relative, _)| cursor + relative);
            Some(source[start..end].to_owned())
        })
    }

    fn open_rename_prompt(&mut self) {
        let word = self.word_at_cursor();
        self.open_prompt(PromptKind::Command, format!("rename {}", word.unwrap_or_default()));
    }

    /// Servers list nothing for an empty query, so `<leader>sS` asks for one.
    pub(in crate::editor::session) fn open_workspace_symbols_prompt(&mut self) {
        let word = self.word_at_cursor().unwrap_or_default();
        self.open_prompt(PromptKind::Command, format!("lsp.workspace_symbols {word}"));
    }
}

/// Where a conflicted file's first marker is, so opening it lands there.
fn first_conflict_line(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().position(|line| line.starts_with("<<<<<<<")))
        .unwrap_or(0)
}

/// A character of an identifier, for the word completion replaces and filters on.
/// Where a search from `cursor` lands, as a byte offset.
fn search_target(
    text: &TextBuffer,
    matches: &[std::ops::Range<usize>],
    cursor: ByteOffset,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    let offset = cursor.get().min(text.len_bytes());
    if forward {
        let start = if offset < text.len_bytes() {
            crate::editor::text::next_grapheme_boundary(text, cursor).get()
        } else {
            offset
        };
        matches
            .iter()
            .find(|matched| matched.start >= start)
            .or_else(|| wrap.then(|| matches.first()).flatten())
            .map(|matched| matched.start)
    } else {
        matches
            .iter()
            .rev()
            .find(|matched| matched.start < offset)
            .or_else(|| wrap.then(|| matches.last()).flatten())
            .map(|matched| matched.start)
    }
}

fn completion_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn active_lsp_document(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let (buffer_id, mut document) = self.active_lsp_document_without_text()?;
        document.text = self.state.buffers[buffer_id].text.to_string();
        Some((buffer_id, document))
    }

    /// The active document's path, language and version, with empty text.
    pub(in crate::editor::session) fn active_lsp_document_without_text(&self) -> Option<(crate::editor::BufferId, LspDocument)> {
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.clone()?;
        let language_id = language_id_for_path(&path)?.to_owned();
        Some((
            buffer_id,
            LspDocument {
                path,
                language_id,
                version: buffer.text.revision().0,
                text: String::new(),
            },
        ))
    }

    pub(in crate::editor::session) fn start_lsp(&mut self, requested: Option<String>) {
        self.launch_lsp(requested, false);
    }

    /// `asked` is a person starting this server, who is told when the project can't have it.
    fn launch_lsp(&mut self, requested: Option<String>, asked: bool) {
        if !self.configuration.settings().lsp.enabled {
            self.show_error(self.message("lsp_disabled", &[]));
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let entry = if let Some(id) = requested.as_ref() {
            let entry =
                self.tooling.entries().iter().find(|entry| {
                    entry.definition.id == *id && entry.definition.kind == ToolKind::Lsp
                });
            if !entry.is_some_and(|entry| entry.definition.supports_language(&document.language_id))
            {
                self.show_error(format!(
                    "language server `{id}` does not support {}",
                    document.language_id
                ));
                return;
            }
            entry
        } else {
            self.tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
                .or_else(|| {
                    self.tooling.entries().iter().find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry
                                .definition
                                .automatically_supports_language(&document.language_id)
                    })
                })
        };
        let Some(entry) = entry else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        let base_definition = entry.definition.clone();
        let source = self.selected_lsp_runtime_source(&base_definition);
        let definition = if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition.clone()
        };
        let launch_source = match source {
            LspRuntimeSource::Path => {
                let Some(executable) = discover_executable(&definition.executable) else {
                    self.show_error(format!(
                        "{} is not available on PATH; press p to choose another runtime",
                        definition.name
                    ));
                    return;
                };
                if definition.id == "rust-analyzer"
                    && Self::rust_analyzer_is_rustup_proxy(&executable)
                {
                    let key = Self::tool_version_key(
                        &definition,
                        LspRuntimeSource::Path,
                        Some(&executable),
                    );
                    const CHECKING: &str = "checking rust-analyzer in the active rustup toolchain…";
                    match self.tooling.version(&key) {
                        Some(Ok(_)) => {
                            if self.notices.current().is_some_and(|(_, notice)| notice == CHECKING) {
                                self.clear_notice();
                            }
                        }
                        Some(Err(_)) => {
                            self.show_error(Self::unavailable_rust_analyzer_message());
                            return;
                        }
                        None => {
                            self.queue_tool_version_probe(
                                &definition,
                                LspRuntimeSource::Path,
                                executable,
                            );
                            self.notify_message(CHECKING.into(), false);
                            return;
                        }
                    }
                }
                LspLaunchSource::Direct {
                    executable,
                    managed: false,
                }
            }
            LspRuntimeSource::Managed => {
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust-download version {} is not installed; press i to install it",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                LspLaunchSource::Direct {
                    executable,
                    managed: true,
                }
            }
            LspRuntimeSource::Nix => {
                let Some(shell) = nix_workspace_shell_for_host(
                    &definition,
                    self.configuration.settings().lsp.nix_workspace_fallback,
                    is_nixos_host(),
                ) else {
                    self.show_error("Dust via Nix is unavailable on this host");
                    return;
                };
                if !nix_lsp_profile_installed(&definition) {
                    self.show_error("Dust via Nix is not installed; press i to install it");
                    return;
                }
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust via Nix version {} is not installed; press i",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                if let Err(error) = nix_lsp_launch_environment(&definition) {
                    self.show_error(error);
                    return;
                }
                LspLaunchSource::NixWorkspace {
                    nix: executable,
                    shell: shell.into(),
                }
            }
        };
        self.language.clear_error(&definition.id);
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if let Err(error) = self.workspace.authorize_execution(
            if matches!(launch_source, LspLaunchSource::NixWorkspace { .. }) {
                ExecutionSource::ProjectLocal
            } else {
                ExecutionSource::SystemTool
            },
        ) {
            self.show_error(error);
            return;
        }
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.discover_launch(
            PendingLspProjectRoot {
                asked,
                definition,
                launch_source,
                root_id,
                workspace_root: root_path,
                buffer,
                document,
            },
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_lsp_project_root(&mut self) {
        let Some(result) = self
            .workspace
            .project_result(crate::editor::session::workspace::DiscoveryKind::Language)
        else {
            return;
        };
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.accept_project_root(
            result,
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn process_nix_lsp_workspace(&mut self) {
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.poll_nix_launch(
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn queue_lsp_preflight(&mut self, pending: LspLaunchRequest) {
        let context = crate::editor::session::lsp::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.queue_launch(
            pending,
            context,
            self.workspace
                .execution_lane(crate::editor::session::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }

    pub(in crate::editor::session) fn restart_lsp_session(&mut self, key: String) {
        let Some(state) = self.language.sessions().get(&key) else {
            return;
        };
        if state.circuit_open || !state.desired_running {
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        let Some((&buffer, document)) = state.documents.iter().next() else {
            return;
        };
        let (definition, launch, root, root_path, document) = (
            state.definition.clone(),
            state.launch.clone(),
            state.root,
            state.root_path.clone(),
            document.clone(),
        );
        self.queue_lsp_preflight(LspLaunchRequest {
            session_key: key,
            definition,
            launch,
            root_id: root,
            project_root: root_path,
            buffer,
            document,
            restart: true,
        });
    }

    pub(in crate::editor::session) fn select_lsp_session(&mut self, key: &str) {
        if self.language.select(key) {
            self.ui.lsp_inlay_hints.clear();
            self.schedule_lsp_inlay_refresh(true);
        }
    }

    pub(in crate::editor::session) fn show_lsp_sessions(&mut self) {
        let active = self.language.active().map(String::as_str);
        let items = self
            .language
            .sessions()
            .iter()
            .map(|(key, session)| {
                (
                    PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("lsp-session:{key}")),
                        label: session.server_id.clone(),
                        detail: format!(
                            "root #{}{} · {} · restarts {}",
                            session.root.0,
                            if active == Some(key) {
                                " · active"
                            } else {
                                ""
                            },
                            if session.circuit_open {
                                "circuit-open"
                            } else if session.restart_due.is_some() {
                                "backoff"
                            } else if session.desired_running {
                                "running"
                            } else {
                                "stopped"
                            },
                            session.restart_count
                        ),
                        source: key.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspSession(key.clone()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_lsp_sessions_title", &[]), items);
    }

    pub(in crate::editor::session) fn clear_lsp_server_ownership(&mut self, server: &str) {
        self.language.forget_synced(server);
        self.language.retire_projection_server(server);
    }

    pub(in crate::editor::session) fn detach_lsp_documents(
        &mut self,
        buffer: crate::editor::BufferId,
        documents: Vec<(String, String)>,
    ) {
        let detached = !documents.is_empty();
        for (server, uri) in documents {
            let _ = self.language.close(&server, uri.clone());
            self.language.invalidate_document(&server, buffer);
            self.language.retire_projection_document(&server, &uri);
            for prefix in ["lsp.", "lsp-pull:"] {
                if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                    self.diagnostics.clear_owner_uri(&owner, &uri);
                }
            }
            self.language.detach(&server, buffer);
        }
        if detached {
            self.sync_ui_diagnostics();
            if self.state.active_buffer_id() == Some(buffer) {
                self.clear_lsp_ui_projection();
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_core_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Quit { force } => {
                if !force && self.state.buffers.iter().any(|(_, buffer)| buffer.dirty) {
                    self.show_error(self.message("unsaved_quit", &[]));
                } else {
                    if force {
                        self.queue_dirty_recovery_before_exit();
                    }
                    self.should_quit = true;
                }
            }
            AppEffect::Save {
                quit_after,
                path,
                force,
            } => {
                if path.is_some() || force || !self.begin_format_on_save(quit_after) {
                    self.start_save(quit_after, path, force);
                }
            }
            AppEffect::SaveAll { quit_after } => self.save_all(quit_after),
            AppEffect::OpenPath(path) => self.start_open(path),
            // Settings files are optional, so the first open creates an empty one to edit.
            AppEffect::OpenUserFile(path) => {
                let created = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                    std::fs::OpenOptions::new().append(true).create(true).open(&path).map(drop)
                });
                // A new file starts with a few commented examples rather than nothing.
                let starter = if path.file_name().is_some_and(|name| name == "keymaps.toml") {
                    include_str!("../../../../assets/defaults/keymaps-starter.toml")
                } else {
                    include_str!("../../../../assets/defaults/settings-starter.toml")
                };
                let created = created.and_then(|()| {
                    if std::fs::metadata(&path)?.len() == 0 {
                        std::fs::write(&path, starter)?;
                    }
                    Ok(())
                });
                match created {
                    Ok(()) => self.start_open(path),
                    Err(error) => self.show_error(format!("{}: {error}", path.display())),
                }
            }
            AppEffect::Reload { force } => self.start_reload(force),
            AppEffect::CloseBuffer { force } => self.close_active_buffer(force),
            AppEffect::CycleBuffer(offset) => match self.state.cycle_buffer(
                offset,
                self.configuration.settings().editor.buffer_navigation_wrap,
            ) {
                Ok(_) => {
                    self.set_dashboard_visibility(false);
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ToggleBufferPin => self.toggle_active_buffer_pin(),
            AppEffect::MoveBufferLine(direction) => self.move_active_buffer_line(direction),
            AppEffect::NewBuffer => {
                self.state.open_text(TextBuffer::from_text(""), None);
                self.set_dashboard_visibility(false);
                self.notify_message(self.message("new_scratch", &[]), false);
            }
            AppEffect::OpenWorkspaceFilePicker => self.start_workspace_file_picker(),
            AppEffect::NewTab => {
                // An untouched [No Name] tab is reused, so ctrl-t never piles up empty tabs;
                // the file picked from the finder replaces it.
                let unused = self.state.buffers.iter().find(|(_, buffer)| {
                    buffer.path.is_none()
                        && !buffer.dirty
                        && buffer.text.len_bytes() == 0
                        && buffer.origin == crate::editor::BufferOrigin::FileOrScratch
                });
                match unused.map(|(id, _)| id) {
                    Some(buffer) => {
                        if let Err(error) = self.state.show_buffer(buffer) {
                            self.show_error(error);
                        }
                    }
                    None => {
                        self.state.open_text(TextBuffer::from_text(""), None);
                    }
                }
                self.set_dashboard_visibility(false);
                self.start_workspace_file_picker();
            }
            AppEffect::GitFileStage => {
                self.input.set_pending_stage(AppEffect::GitFileStage);
                // The plan itself refuses a path whose buffer has unsaved edits and reports
                // `UnsavedDocument`, which the mutation handler turns into the explicit
                // save-and-stage choice. The surface only has to name the right file.
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::workspace::git::GitMutationKind::StageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.show_error(error);
                }
            }
            AppEffect::GitFileUnstage => {
                self.input.set_pending_stage(AppEffect::GitFileUnstage);
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::workspace::git::GitMutationKind::UnstageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.show_error(error);
                }
            }
            AppEffect::GitLinesStage { start, end } => {
                self.input.set_pending_stage(AppEffect::GitLinesStage { start, end });
                let range = u32::try_from(start)
                    .ok()
                    .zip(u32::try_from(end).ok())
                    .filter(|(start, end)| end >= start);
                match range {
                    Some((start, end)) => {
                        if let Err(error) = self.git.start_plan(
                            &self.state,
                            crate::workspace::git::GitMutationKind::StageLines,
                            self.workspace.root_revision(),
                            self.workspace.trust() == WorkspaceTrust::Trusted,
                            (start..=end).collect(),
                        ) {
                            self.show_error(error);
                        }
                    }
                    None => self.show_error(
                        "line range must be two non-negative integers with end >= start",
                    ),
                }
            }
            AppEffect::GitWorktreeCreate { branch, path } => {
                self.create_git_worktree(branch, path);
            }
            AppEffect::HelpForForm => self.open_form_help(),
            AppEffect::GitConflictsOpen => self.open_git_conflicts(),
            AppEffect::GitConflictAccept { side } => self.accept_conflict_block(&side),
            AppEffect::GitConflictChoose { side } => self.choose_git_conflict_side(&side),
            AppEffect::DebuggerBreakpointStates => self.open_breakpoint_states(),
            AppEffect::DebuggerBreakpointDisable => self.toggle_breakpoint_disabled(),
            AppEffect::DebuggerTerminate => self.terminate_debug_session(),
            AppEffect::DebuggerDetach => self.detach_debug_session(),
            AppEffect::GitConflictStage => self.stage_git_conflict(),
            AppEffect::GitConflictContinue => self.continue_git_conflict(),
            AppEffect::GitConflictAbort => self.abort_git_conflict(),
            AppEffect::GitConflictReplace { side, files } => {
                self.preview_git_replacement(&side, &files);
            }
            AppEffect::GitConflictReplaceSubmit => {}
            AppEffect::Tutor => self.open_tutor(),
            AppEffect::TutorNext => self.tutor_next(),
            AppEffect::WalkJumps(back) => self.walk_jumps(back),
            AppEffect::TutorBack => self.tutor_back(),
            AppEffect::TutorQuit => self.tutor_quit(),
            AppEffect::HelpContextual => self.open_contextual_help(),
            AppEffect::HelpCommandReference => self.open_command_reference(),
            AppEffect::GitWorktreeOpen => self.open_git_worktree(),
            AppEffect::GitWorktreeRemove => self.request_git_worktree_remove(),
            AppEffect::GitWorktreesOpen => self.start_git_worktrees(),
            AppEffect::GitHunksOpenStaged => {
                self.start_git_hunks_with_layer(
                    None,
                    crate::workspace::git::GitDiffLayer::Staged,
                );
            }
            AppEffect::GitHunkUnstage => self.unstage_selected_git_hunk(),
            AppEffect::GitStatusRefresh => self.start_git_status_refresh(),
            AppEffect::GitStatusOpen => self.open_git_status_picker(),
            AppEffect::GitBrowse { copy } => self.browse_git_line(copy),
            AppEffect::FlashBegin => self.begin_flash(),
            AppEffect::TodoSearch => self.start_todo_search(),
            AppEffect::AgentAsk => self.begin_agent_question(),
            AppEffect::AgentToggle => self.toggle_agent_panel(),
            AppEffect::AgentAddFile => self.attach_agent_file(),
            AppEffect::AgentAddDiagnostics => self.attach_agent_diagnostics(),
            AppEffect::AgentNew => self.open_agent_picker(),
            AppEffect::AgentSessions => self.open_agent_sessions_picker(),
            AppEffect::AgentNextSession => self.cycle_agent_session(1),
            AppEffect::AgentPreviousSession => self.cycle_agent_session(-1),
            AppEffect::AgentCloseSession => self.close_agent_session(),
            AppEffect::AgentResume => self.open_agent_resume_picker(),
            AppEffect::AgentCancel => self.cancel_agent_turn(),
            AppEffect::AgentModel => self.open_agent_option_picker(Some("model")),
            AppEffect::AgentMode => self.open_agent_option_picker(Some("mode")),
            AppEffect::AgentThinking => self.open_agent_option_picker(Some("thought_level")),
            AppEffect::AgentOptions => self.open_agent_option_picker(None),
            AppEffect::AgentInput(text) => self.agent_input_insert(&text),
            AppEffect::AgentEdit(action) => self.agent_input_edit(&action),
            AppEffect::AgentScroll(rows) => self.scroll_agent(rows),
            AppEffect::AgentEscape => self.agent_escape(),
            AppEffect::AgentNewTab => self.new_agent_tab(),
            AppEffect::PullRequestList => self.list_pull_requests(),
            AppEffect::PullRequestComments => self.list_review_comments(),
            AppEffect::PullRequestComment => self.begin_review_comment(),
            AppEffect::GitChangesLayerSwitch => {
                // Only the change surface has a layer to switch; elsewhere this does nothing
                // rather than reporting an error for a key that has no meaning there.
                if self.search.switch_changes_layer().is_some() {
                    self.sync_picker_ui();
                }
            }
            AppEffect::GitHunksOpen => self.start_git_hunks(),
            AppEffect::GitConflictNavigate(direction) => self.navigate_git_conflict(direction),
            AppEffect::MarkdownPreview => {
                let Some(buffer) = self.state.active_buffer_id().map(|id| &self.state.buffers[id])
                else {
                    return Ok(());
                };
                let title = buffer
                    .path
                    .as_ref()
                    .map_or_else(|| "Markdown".to_owned(), |path| self.workspace_relative_label(path));
                let value = buffer.text.to_string();
                self.present_documentation_popup(
                    DocumentationPopupKind::Git,
                    title,
                    "Markdown".into(),
                    None,
                    vec![LspMarkupContent { kind: LspMarkupKind::Markdown, value }],
                );
            }
            AppEffect::GitHunkNavigate(direction) => self.navigate_git_hunk(direction),
            AppEffect::GitHunkPreview => self.preview_selected_git_hunk(),
            AppEffect::GitHunkStage => {
                self.input.set_pending_stage(AppEffect::GitHunkStage);
                self.stage_selected_git_hunk();
            }
            AppEffect::GitHunkReset => self.request_reset_selected_git_hunk(),
            AppEffect::OpenFilePrompt => {
                let alias = self
                    .configuration
                    .settings()
                    .input
                    .command_aliases
                    .iter()
                    .find(|(_, command)| command.as_str() == "file.open")
                    .map_or("file.open", |(alias, _)| alias.as_str());
                self.open_prompt(PromptKind::Command, format!("{alias} "));
            }
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::PromptInsert(text) => self.prompt_insert(&text),
            AppEffect::PromptBackspace => self.prompt_backspace(),
            AppEffect::PromptMove(movement) => self.prompt_move(movement),
            AppEffect::PromptHistory(direction) => self.prompt_history(direction),
            AppEffect::PromptComplete(direction) => self.prompt_complete(direction),
            // y and n answer at once, like Vim's own confirm prompts.
            AppEffect::PromptConfirmationSelect(choice) => {
                self.input.select_confirmation(choice);
                self.submit_prompt();
            }
            AppEffect::PromptConfirmationToggle => {
                self.input.toggle_confirmation();
                self.project_input_prompt();
            }
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => {
                let declined_trust = self
                    .input
                    .prompt()
                    .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust);
                self.close_prompt();
                if declined_trust {
                    self.notify_message(self.message("workspace_trust_declined", &[]), false);
                }
            }
            AppEffect::NotificationHistory => {
                let items = self
                    .notices
                    .history()
                    .rev()
                    .enumerate()
                    .map(|(index, (error, message))| {
                        (
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("notification:{index}")),
                                label: message.clone(),
                                detail: if *error {
                                    "error".into()
                                } else {
                                    "info".into()
                                },
                                source: "application".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.clear_notice();
                self.install_picker("Notifications".into(), items);
            }
            AppEffect::NotificationDismiss => {
                self.clear_notice();
            }
            AppEffect::SearchRepeat { reverse } => {
                let forward = self.input.search_direction(reverse);
                self.perform_search(forward);
            }
            AppEffect::SearchWord { forward } => self.search_word_under_cursor(forward),
            AppEffect::Scroll {
                kind,
                count,
                at_mouse,
            } => {
                if at_mouse {
                    self.focus_mouse_pane();
                }
                self.scroll_view(&kind, count);
            }
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::Layout(action) => self.apply_layout_action(action),
            AppEffect::ViewToggle(toggle) => {
                let enabled = self.configuration.toggle_view(toggle);
                let name = match toggle {
                    crate::editor::session::ViewToggle::Wrap => "wrap",
                    crate::editor::session::ViewToggle::LineNumbers => "line numbers",
                    crate::editor::session::ViewToggle::RelativeNumbers => "relative numbers",
                    crate::editor::session::ViewToggle::IndentGuides => "indent guides",
                    crate::editor::session::ViewToggle::GitSigns => "git signs",
                    crate::editor::session::ViewToggle::FormatOnSave => "format on save",
                };
                self.ensure_cursor_visible();
                self.notify_message(
                    format!("{name} {}", if enabled { "enabled" } else { "disabled" }),
                    false,
                );
            }
            AppEffect::Zen => {
                let explorer_open = self.ui.explorer.is_some();
                match self.configuration.toggle_zen(explorer_open) {
                    None => {
                        if explorer_open {
                            let _ = self.process_project_effect(AppEffect::ExplorerClose);
                        }
                        if !self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                    }
                    Some(reopen_explorer) => {
                        if self.state.is_zoomed() {
                            self.apply_layout_action(LayoutAction::Zoom);
                        }
                        if reopen_explorer {
                            let _ = self.process_project_effect(AppEffect::ExplorerOpen);
                        }
                    }
                }
                self.ensure_cursor_visible();
            }
            AppEffect::SurfaceFocus(direction) => self.focus_surface(direction),
            AppEffect::WindowClone { target, axis } => {
                if let Err(error) = self.clone_graph_window(&target, axis) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowList => {
                let graph = self.window_session.graph();
                let items = graph
                    .windows()
                    .filter_map(|window| {
                        let content = graph.content_for_window(window.id)?;
                        Some((
                            PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("window:{}", window.id.0)),
                                label: content.title.clone(),
                                detail: format!(
                                    "{} · {:?} · {:?} · tab {}",
                                    content.content_type.as_str(),
                                    content.lifecycle,
                                    window.placement,
                                    window.tab.0
                                ),
                                source: content.owner.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Window(window.id),
                        ))
                    })
                    .collect();
                self.install_picker("Windows".into(), items);
            }
            AppEffect::RetainedUiFocus(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Focus(delta));
            }
            AppEffect::RetainedUiScroll(delta) => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Scroll(delta));
            }
            AppEffect::RetainedUiActivate => {
                self.apply_retained_ui_interaction(PluginUiInteraction::Activate);
            }
            AppEffect::RetainedUiMouseSelect => self.select_retained_ui_at_mouse(),
            AppEffect::PrivateUiActivate { at_mouse } => {
                self.activate_private_surface_action(at_mouse);
            }
            AppEffect::WindowDock { target, edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("window dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                let result =
                    self.place_graph_window(&target, WindowPlacement::Docked { edge, size });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFloat {
                target,
                x,
                y,
                width,
                height,
            } => {
                let result = self.place_graph_window(
                    &target,
                    WindowPlacement::Floating(WindowRect {
                        x,
                        y,
                        width,
                        height,
                    }),
                );
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFullscreen(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Fullscreen);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowHide(target) => {
                let result = self.place_graph_window(&target, WindowPlacement::Hidden);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowShow(target) => {
                let result = self.show_graph_window(&target);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowClose(target) => {
                if let Err(error) = self.close_graph_window(&target) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResize { target, amount } => {
                if let Err(error) = self.resize_graph_window(&target, amount) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowResizeDirectional(direction) => {
                if let Err(error) = self.resize_focused_window(direction) {
                    self.show_error(error);
                }
            }
            AppEffect::WindowMove { target, x, y } => {
                let result = self.move_graph_window(&target, x, y);
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowSwap { first, second } => {
                if let Err(error) = self.swap_graph_windows(&first, &second) {
                    self.show_error(error);
                }
            }
            AppEffect::RepeatLast => self.repeat_last_change(),
            AppEffect::MacroRecordStart(register) => {
                self.input.start_macro_recording(register);
                self.notify_message(
                    self.message("macro_recording", &[("register", register.to_string())]),
                    false,
                );
            }
            AppEffect::MacroRecordStop => {
                if let Some((register, invocations)) = self.input.finish_macro_recording() {
                    self.state.registers.set_macro(register, invocations);
                    self.state.mark_editor_changed();
                    self.notify_message(
                        self.message("macro_recorded", &[("register", register.to_string())]),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_macro_recording", &[]));
                }
            }
            AppEffect::MacroReplay { register, count } => {
                self.input.begin_macro_batch();
                for _ in 0..count {
                    self.replay_macro(register);
                    if self.ui.notification_error {
                        break;
                    }
                }
            }
            AppEffect::Grammar(input) => self.process_grammar_input(input),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl InteractiveApplication {
    pub(super) fn document_view(
        id: crate::editor::BufferId,
        buffer: &crate::editor::Buffer,
    ) -> crate::editor::session::documents::DocumentView<'_> {
        crate::editor::session::documents::DocumentView {
            id,
            path: buffer.path.as_deref(),
            text: &buffer.text,
            persisted: buffer.persisted_bytes(),
            dirty: buffer.dirty,
            virtual_read_only: buffer.is_virtual_read_only(),
        }
    }
    pub(in crate::editor::session) fn show_document_failure(
        &mut self,
        failure: crate::editor::session::documents::DocumentFailure,
    ) {
        match failure {
            crate::editor::session::documents::DocumentFailure::Policy(key) => {
                self.show_error(self.message(key, &[]))
            }
            crate::editor::session::documents::DocumentFailure::Error(e) => self.show_error(e),
        }
    }
    pub(in crate::editor::session) fn start_open(&mut self, path: PathBuf) {
        self.record_jump();
        match self.documents.request_open(
            path,
            &self.state.workspace_root,
            &self.state.window_version(),
            self.workspace.version(),
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
        ) {
            Ok(()) => self.notify_message(self.message("opening_file", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
    pub(in crate::editor::session) fn start_reload(&mut self, force: bool) {
        let view = self.state.active_buffer_id().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        match self.documents.request_reload(
            view,
            force,
            &self.state.window_version(),
            self.workspace.version(),
        ) {
            Ok(()) => self.notify_message(self.message("reloading", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
    /// The formatter from the tool manager for the active file, the way conform.nvim comes
    /// before the language server in LazyVim.
    pub(super) fn active_formatter(&self) -> Option<String> {
        let buffer = &self.state.buffers[self.state.active_buffer_id()?];
        let path = buffer.path.as_deref()?;
        let language = language_id_for_path(path)?;
        Some(managed_formatter(|tool| tool.supports_language(language), path)?.id)
    }
    /// Ask the language server to format, and save when it answers. False when there is nothing
    /// to wait for, so the caller saves right away.
    fn begin_format_on_save(&mut self, quit_after: bool) -> bool {
        if !self.configuration.settings().lsp.format_on_save || self.format_on_save.is_some() {
            return false;
        }
        // An installed formatter runs as a process, which takes longer to start than a request.
        let managed = self.workspace.execution_allowed().is_ok() && self.active_formatter().is_some();
        let buffer = if managed {
            self.state.active_buffer_id()
        } else {
            self.active_lsp_document_without_text().and_then(|(buffer, document)| {
                self.language.formatting_server(buffer, &document).map(|_| buffer)
            })
        };
        let Some(buffer) = buffer else {
            return false;
        };
        self.format_on_save = Some(super::FormatOnSave {
            buffer,
            quit_after,
            // VS Code gives formatters 750ms by default; a slow server must not hold a save back.
            deadline: std::time::Instant::now()
                + std::time::Duration::from_millis(if managed { 3000 } else { 1000 }),
        });
        self.request_lsp(LspFeature::Formatting);
        true
    }

    pub(in crate::editor::session) fn finish_format_on_save(&mut self) {
        let Some(pending) = self.format_on_save.take() else {
            return;
        };
        if self.state.active_buffer_id() == Some(pending.buffer) {
            self.start_save(pending.quit_after, None, false);
        } else {
            self.show_error(self.message("format_on_save_skipped", &[]));
        }
    }

    /// Once a second, notice open files that changed outside Dust: lazygit, a hunk reset, a
    /// formatter. A clean buffer takes the new text; a changed one keeps its text and says so.
    pub(super) fn process_disk_changes(&mut self) {
        let now = self.clock.monotonic();
        if self.next_disk_check.is_some_and(|next| now < next) || self.documents.file_work_pending() {
            return;
        }
        self.next_disk_check = Some(now + std::time::Duration::from_secs(1));
        let open: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.virtual_owner().is_none())
            .filter_map(|(id, buffer)| Some((id, buffer.path.clone()?)))
            .collect();
        self.disk_stamps.retain(|id, _| open.iter().any(|(open, _)| open == id));
        for (id, path) in open {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let stamp = (metadata.modified().ok(), metadata.len());
            let previous = self.disk_stamps.insert(id, stamp);
            if previous.is_none_or(|previous| previous == stamp) {
                continue;
            }
            let view = Self::document_view(id, &self.state.buffers[id]);
            // Our own save moves the stamp too, and then the bytes on disk are what we wrote.
            if metadata.len() == view.persisted.len() as u64
                && std::fs::read(&path).is_ok_and(|disk| disk == view.persisted)
            {
                continue;
            }
            if view.dirty {
                let shown = self.workspace_relative_label(&path);
                self.notify_warning(self.message("file_changed_on_disk", &[("path", shown)]));
                continue;
            }
            let editor = self.state.window_version();
            let workspace = self.workspace.version();
            let view = Self::document_view(id, &self.state.buffers[id]);
            if let Err(error) = self.documents.request_reload(Some(view), false, &editor, workspace) {
                self.show_document_failure(error);
            }
        }
    }

    pub(super) fn process_format_on_save_deadline(&mut self) {
        if self
            .format_on_save
            .as_ref()
            .is_some_and(|pending| std::time::Instant::now() >= pending.deadline)
        {
            self.finish_format_on_save();
        }
    }

    pub(in crate::editor::session) fn start_save(
        &mut self,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
    ) {
        let Some(buffer) = self.state.active_buffer_id() else {
            self.show_document_failure(crate::editor::session::documents::DocumentFailure::Policy(
                "no_active_save",
            ));
            return;
        };
        match self.request_save_buffer(buffer, quit_after, requested_path, force, None) {
            Ok(()) => self.notify_message(self.message("saving", &[]), false),
            Err(error) => self.show_document_failure(error),
        }
    }
    /// Every changed file, without waiting on formatters. Quitting happens when the last save
    /// lands, and only if nothing is left unsaved by then.
    fn save_all(&mut self, quit_after: bool) {
        let changed: Vec<_> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| {
                buffer.dirty && buffer.path.is_some() && buffer.virtual_owner().is_none()
            })
            .map(|(id, _)| id)
            .collect();
        let Some(last) = changed.last().copied() else {
            if quit_after {
                self.process_effect(AppEffect::Quit { force: false });
            } else {
                self.notify_message(self.message("save_all_nothing", &[]), false);
            }
            return;
        };
        self.notify_message(self.message("saving", &[]), false);
        for buffer in changed {
            let quit = quit_after && buffer == last;
            if let Err(error) = self.request_save_buffer(buffer, quit, None, false, None) {
                self.show_document_failure(error);
            }
        }
    }
    fn request_save_buffer(
        &mut self,
        buffer: crate::editor::BufferId,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
        close: Option<EditorCloseSave>,
    ) -> Result<(), crate::editor::session::documents::DocumentFailure> {
        let view = self
            .state
            .buffers
            .get(buffer)
            .map(|candidate| Self::document_view(buffer, candidate));
        let request = crate::editor::session::documents::SaveRequest {
            editor: &self.state.window_version(),
            workspace: self.workspace.version(),
            base: &self.state.workspace_root,
            path: requested_path,
            policy: &self.configuration.settings().editor,
            force,
            quit: quit_after,
            close,
            other_open_paths: self
                .state
                .buffers
                .iter()
                .filter(|(id, _)| *id != buffer)
                .filter_map(|(_, candidate)| candidate.path.clone())
                .collect(),
        };
        self.documents.request_save(view, request)
    }
    pub(in crate::editor::session) fn close_active_buffer(&mut self, force: bool) {
        let Some(view) = self.state.active_view else {
            self.set_dashboard_visibility(true);
            return;
        };
        let buffer = self.state.views[view].buffer;
        if !force {
            self.reconcile_editor_windows();
            match self
                .window_session
                .begin_buffer_close(&(&*self.state).into(), view, buffer)
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
            return;
        }
        self.close_buffer_target(view, buffer);
    }
    fn retire_closed_buffer(&mut self, buffer_id: crate::editor::BufferId, path: Option<&Path>) {
        self.documents.retire_buffer(buffer_id, path);
        if let Some(path) = path {
            if let Some(id) = self.explorer.retire_buffer_path(path) {
                self.documents.cancel_request(id);
            }
        }
        self.extension.close_virtual_buffer(buffer_id);
        let documents = self
            .language
            .sessions()
            .iter()
            .filter_map(|(server, state)| {
                state
                    .documents
                    .get(&buffer_id)
                    .map(|document| (server.clone(), document.uri()))
            })
            .collect::<Vec<_>>();
        self.detach_lsp_documents(buffer_id, documents);
    }
    fn close_buffer_target(
        &mut self,
        view_id: crate::editor::ViewId,
        buffer_id: crate::editor::BufferId,
    ) {
        let closed_path = self
            .state
            .buffers
            .get(buffer_id)
            .and_then(|buffer| buffer.path.clone());
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&*self.state).into())
        {
            self.show_error(error);
            return;
        }
        let mut candidate = self.state.draft();
        let removed = match candidate.close_document(view_id, buffer_id) {
            Ok(removed) => removed,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.commit_editor_draft(candidate, None, false) {
            self.show_error(error);
            return;
        }
        if removed {
            self.retire_closed_buffer(buffer_id, closed_path.as_deref());
        }
        self.set_dashboard_visibility(self.state.active_view.is_none());
        if !self.window_session.dashboard_visible() {
            self.sync_lsp_document();
        } else {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        self.notify_message(
            if self.window_session.dashboard_visible() {
                self.message("returned_dashboard", &[])
            } else {
                self.message("buffer_closed", &[])
            },
            false,
        );
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_file_reply(
        &mut self,
        reply: crate::editor::session::documents::FileReply,
    ) {
        use crate::editor::session::documents::DocumentChange;
        let close_save = reply.close_save().cloned();
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&*self.state).into())
        {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_error(error);
            return;
        }
        if let Err(error) = reply.check_save_destination(
            self.state
                .buffers
                .iter()
                .filter_map(|(id, b)| b.path.as_deref().map(|path| (id, path))),
        ) {
            if let Some(token) = &close_save {
                self.window_session.fail_editor_close_save(token);
            }
            self.show_document_failure(error);
            return;
        }
        let current = self.state.version();
        let view = reply.target().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        let change = match self
            .documents
            .prepare_file(reply, &current, &self.workspace.version(), view)
            .and_then(|plan| plan.into_change(&current))
        {
            Ok(change) => change,
            Err(e) => {
                if let Some(token) = &close_save {
                    self.window_session.fail_editor_close_save(token);
                }
                self.show_document_failure(e);
                return;
            }
        };
        let mut candidate = self.state.draft();
        match change {
            DocumentChange::Open {
                text,
                path,
                navigation,
                activate,
            } => {
                let activate = activate
                    && self.window_session.navigation_current(
                        &navigation,
                        &self.state.window_version(),
                        self.state.active_view,
                    );
                if activate {
                    candidate.open_document(text, path.clone());
                } else {
                    candidate.load_document(text, path.clone());
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, activate) {
                    self.show_error(e);
                    return;
                }
                self.record_recent_file(path.clone());
                self.notify_message(
                    self.message("opened", &[("path", self.workspace_relative_label(&path))]),
                    false,
                );
            }
            DocumentChange::Reload { buffer, text, path } => {
                if !candidate.reload_document(buffer, text, path.clone()) {
                    self.show_error(self.message("reload_closed", &[]));
                    return;
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(e);
                    return;
                }
                self.git.forget_hunks();
                self.notify_message(
                    self.message("reloaded_file", &[("path", self.workspace_relative_label(&path))]),
                    false,
                );
            }
            DocumentChange::Saved {
                buffer,
                text,
                revision,
                bytes,
                target: path,
                quit: quit_after,
                close,
            } => {
                if !candidate.save_document(buffer, &text, revision, bytes, path.clone()) {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    return;
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.input.clear_pending_stage();
                    if let Some(token) = &close {
                        self.window_session.fail_editor_close_save(token);
                    }
                    self.show_error(e);
                    return;
                }
                // A staged save that was waiting on this completion replays now, against the
                // saved bytes, using the exact effect that was refused.
                if let Some(effect) = self.input.take_pending_stage() {
                    self.process_effect(effect);
                }
                let buffer_state = &self.state.buffers[buffer];
                let still_dirty = buffer_state.dirty;
                if let (Some(directory), false) = (&self.state.undo_directory, still_dirty) {
                    crate::editor::save_undo_history(
                        directory.clone(),
                        path.clone(),
                        buffer_state.text.clone(),
                        buffer_state.undo.clone(),
                        self.state.undo_maximum_bytes,
                    );
                }
                let lsp_document = buffer_state.path.clone().and_then(|path| {
                    Some(LspDocument {
                        language_id: language_id_for_path(&path)?.into(),
                        path,
                        version: buffer_state.text.revision().0,
                        text: buffer_state.text.to_string(),
                    })
                });
                self.git.forget_hunks();
                self.notify_message(
                    if still_dirty {
                        self.message(
                            "saved_dirty",
                            &[
                                ("revision", revision.0.to_string()),
                                ("path", self.workspace_relative_label(&path)),
                            ],
                        )
                    } else {
                        self.message("saved", &[("path", self.workspace_relative_label(&path))])
                    },
                    false,
                );
                if let Some(server) = self.language.active().map(String::as_str) {
                    if let Some(document) = lsp_document {
                        let _ = self.language.save(server, document);
                        // An older save snapshot must not leave the worker's document
                        // identity behind the current editor incarnation/content.
                        self.language.invalidate_active_document(buffer);
                    }
                }
                if self.ui.explorer.is_some() {
                    self.start_git_status_refresh_with_feedback(false);
                }
                if let Some(token) = close {
                    if still_dirty {
                        self.window_session.fail_editor_close_save(&token);
                        self.show_error(
                            "document changed while its close save was pending; close was cancelled",
                        );
                    } else {
                        match self
                            .window_session
                            .finish_editor_close_save(&token, &(&*self.state).into())
                        {
                            Ok(step) => self.handle_editor_close_step(step),
                            Err(error) => self.show_error(error),
                        }
                    }
                }
                if quit_after && !still_dirty {
                    if self
                        .state
                        .buffers
                        .iter()
                        .any(|(_, candidate)| candidate.dirty)
                    {
                        self.show_error(self.message("saved_others_dirty", &[]));
                    } else {
                        self.should_quit = true;
                    }
                }
            }
        }
    }
}

use crate::editor::session::workspace::{WorkspaceFailure, WorkspaceUpdate};
impl InteractiveApplication {
    pub(in crate::editor::session) fn show_workspace_failure(&mut self, failure: WorkspaceFailure) {
        let message = match failure {
            WorkspaceFailure::Message(key) => self.message(key, &[]),
            WorkspaceFailure::Diagnostic(key, diagnostic) => {
                self.message(key, &[("diagnostic", diagnostic)])
            }
            WorkspaceFailure::Plain(message) => message,
        };
        self.show_error(message);
    }
    /// Run a project-defined command if the workspace is trusted.
    pub(in crate::editor::session) fn run_trusted(&mut self, action: TrustedAction) {
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        match action {
            TrustedAction::StartDebug(intent) => self.authorize_debug_launch(intent),
            TrustedAction::InstallRegistry {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                true,
            ),
            TrustedAction::PrepareRegistryArtifact => {
                if let Err(error) = self.extension.prepare_artifact(self.plugin_limits()) {
                    self.show_error(error);
                }
            }
            TrustedAction::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => self.install_package_after_validation(
                package_manifest,
                plugin_manifest,
                component,
                update,
            ),
            TrustedAction::RunTask(name) => self.start_task_plan(&name, AdapterContext::Plain),
            TrustedAction::OpenTerminal(intent) => {
                self.open_terminal_with_response(intent, None)
            }
            TrustedAction::OpenDapTerminal { intent, reply } => {
                self.open_terminal_with_response(intent, Some(reply))
            }
            TrustedAction::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            TrustedAction::RunLinter { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Linter { matcher })
            }
            TrustedAction::RunFormatter { name, source } => {
                self.start_task_plan(&name, AdapterContext::Formatter { source })
            }
        }
    }

    #[cfg(test)]
    pub(in crate::editor::session) fn reconcile_workspace_trust(&mut self) {
        self.workspace.reconcile_trust();
    }
    pub(in crate::editor::session) fn process_trust_events(&mut self) {
        let before = self.workspace.version();
        let updates = self.workspace.poll();
        if before != self.workspace.version() {
            self.documents.retire_workspace(&self.workspace.version());
        }
        for event in updates {
            match event {
                WorkspaceUpdate::Failure(e) => self.show_workspace_failure(e),
                WorkspaceUpdate::Message(key) => self.notify_message(self.message(key, &[]), false),
                WorkspaceUpdate::RootAdded(path) => {
                    self.invalidate_root_scoped_operations();
                    self.notify_message(
                        self.message(
                            "workspace_root_added",
                            &[("path", path.display().to_string())],
                        ),
                        false,
                    );
                }
                WorkspaceUpdate::RootSwitched { path, old } => self.finish_root_switch(path, &old),
            }
        }
    }
    pub(in crate::editor::session) fn maybe_prompt_for_unseen_workspace(&mut self) {
        // Never open under the user's fingers: keys typed a moment ago would answer it.
        let typing = self
            .last_key_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(800));
        if self.workspace.should_prompt(
            typing
                || self.input.prompt().is_some()
                || self.ui.picker.is_some()
                || self.ui.tool_menu.is_some(),
        ) {
            self.request_workspace_trust();
        }
    }
    pub(in crate::editor::session) fn request_workspace_trust(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        match self.workspace.request_confirmation() {
            Ok(()) => {
                self.clear_notice();
                self.open_confirmation_actions(
                    PromptKind::WorkspaceTrust,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .workspace_trust_title
                        .clone(),
                    self.message("workspace_trust_confirmation", &[]),
                    crate::ui::ConfirmationChoice::Yes,
                    "Trust",
                    "Not now",
                );
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
    pub(in crate::editor::session) fn confirm_workspace_trust(&mut self) {
        match self.workspace.confirm() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
    pub(in crate::editor::session) fn revoke_workspace_trust(&mut self) {
        match self.workspace.revoke() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}
