use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn apply_plugin_execution(
        &mut self,
        mut execution: PluginExecution,
    ) {
        let plugin = execution.plugin.clone();
        let mut provider_continuations = Vec::new();
        if let Some(load) = self.plugins.load_id(&plugin)
            && self.plugins.is_active_generation(&plugin, load)
        {
            self.plugin_host_events.acknowledge(
                &plugin,
                load,
                &execution.host_event_acknowledgements,
            );
            if let Some(policy) = self
                .plugin_contributions
                .get(&plugin)
                .filter(|index| index.generation == load.0)
                .and_then(|index| index.storage.clone())
            {
                for request in std::mem::take(&mut execution.storage_requests) {
                    let token = request.token;
                    if let Err(error) =
                        self.plugin_storage
                            .request(plugin.clone(), load, policy.clone(), request)
                    {
                        self.queue_plugin_storage_result(
                            crate::extensions::PluginStorageResult::rejected(
                                plugin.clone(),
                                load,
                                token,
                                error,
                            ),
                        );
                    }
                }
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
                                    self.show_error(format!(
                                        "virtual document graph publication rejected: {error}"
                                    ));
                                    continue;
                                }
                                self.plugin_virtual_documents = broker;
                                self.state = state;
                                self.plugin_virtual_editor = bridge;
                                self.windows = graph;
                                self.editor_windows = editor;
                                self.ui.dashboard = false;
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
            && let Some(load) = self.plugins.load_id(&plugin)
            && self.plugins.is_active_generation(&plugin, load)
            && let Some(index) = self
                .plugin_contributions
                .get(&plugin)
                .filter(|index| index.generation == load.0)
                .cloned()
        {
            let requests = std::mem::take(&mut execution.ui_requests);
            let events = requests
                .into_iter()
                .map(|request| {
                    self.plugin_windows
                        .process(&mut self.windows, &index, request)
                })
                .collect::<Vec<_>>();
            let pending = self
                .pending_plugin_ui_events
                .entry((plugin.clone(), load))
                .or_default();
            pending.extend(events);
            if pending.len() > 64 {
                pending.drain(..pending.len().saturating_sub(64));
            }
            self.ui.plugin_surfaces = self.plugin_windows.snapshots();
            self.surface_windows.invalidate();
            self.reconcile_editor_windows();
        }
        for message in execution.messages {
            self.ui.notification = Some(message);
            self.ui.notification_error = false;
        }
        let mut documents = Vec::new();
        for proposal in execution.proposals {
            let Some((buffer, state)) = self
                .state
                .buffers
                .iter()
                .find(|(id, _)| id.data().as_ffi() == proposal.buffer)
            else {
                continue;
            };
            let revision = BufferRevision(proposal.revision);
            if state.text.revision() != revision {
                continue;
            }
            let Some(path) = state.path.as_ref() else {
                continue;
            };
            let codec = PositionCodec::new(&state.text);
            let mut edits = Vec::new();
            for edit in proposal.edits {
                let (Ok(start), Ok(end)) = (usize::try_from(edit.start), usize::try_from(edit.end))
                else {
                    continue;
                };
                let (Ok((start_line, start_character)), Ok((end_line, end_character))) = (
                    codec.byte_to_lsp(ByteOffset(start), PositionEncoding::Utf8),
                    codec.byte_to_lsp(ByteOffset(end), PositionEncoding::Utf8),
                ) else {
                    continue;
                };
                edits.push(LspTextEdit {
                    range: LspRange {
                        start: LspPosition {
                            line: start_line,
                            character: start_character,
                        },
                        end: LspPosition {
                            line: end_line,
                            character: end_character,
                        },
                    },
                    new_text: edit.replacement,
                });
            }
            if !edits.is_empty() {
                documents.push(LspDocumentEdit {
                    uri: file_uri(path),
                    version: Some(revision.0),
                    edits,
                });
            }
            let _ = buffer;
        }
        if !documents.is_empty() {
            self.begin_workspace_proposal(
                format!("plugin {}: {}", execution.plugin, execution.command),
                LspWorkspaceEdit {
                    resources: Vec::new(),
                    documents,
                },
                None,
                PositionEncoding::Utf8,
            );
        }
        self.logs.record(
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
}
