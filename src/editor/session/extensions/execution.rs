use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn apply_plugin_ui_request(
        &mut self,
        index: &crate::extensions::PluginContributionIndex,
        request: crate::extensions::PluginUiRequest,
    ) -> crate::extensions::PluginUiEvent {
        let Some(index) = self
            .plugin_contributions
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
        let catalog = self.plugin_contributions.version();
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
                &self.plugin_contributions.version(),
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
                .plugin_contributions
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
                let mut broker = self.plugin_virtual_documents.clone();
                match broker.apply(&plugin, load, proposal) {
                    Ok(snapshot) => {
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
                                        self.show_error(format!(
                                            "virtual document graph publication rejected: {error}"
                                        ));
                                        continue;
                                    }
                                };
                                self.plugin_virtual_documents = broker;
                                self.state = state;
                                self.plugin_virtual_editor = bridge;
                                self.set_dashboard_visibility(false);
                                self.reconcile_editor_windows();
                            }
                            Err(error) => self.show_error(format!(
                                "virtual document publication rejected: {error:?}"
                            )),
                        }
                    }
                    Err(error) => self.show_error(error.detail),
                }
            }
            for chunk in std::mem::take(&mut execution.provider_chunks) {
                let request = chunk.request;
                match self.plugin_providers.apply_chunk(
                    &plugin,
                    load,
                    workspace_revision,
                    buffer_revision,
                    chunk,
                ) {
                    Ok(status) => {
                        if self.active_plugin_picker == Some(request) {
                            self.present_plugin_picker(request);
                        }
                        if status == PluginProviderSessionStatus::Active
                            && let Some(projection) = self.plugin_providers.projection(request)
                        {
                            let prefix = format!("plugin.{plugin}.");
                            if let Some(command) = projection.action_command.strip_prefix(&prefix) {
                                provider_continuations.push(command.to_owned());
                            }
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
                .plugin_contributions
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
