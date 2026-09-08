use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn install_picker(
        &mut self,
        title: String,
        items: Vec<(PickerItem, PickerPayload)>,
    ) {
        self.install_surface_picker(PickerSurfaceKind::Generic, title, items);
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
        let retired_preview = self
            .search
            .workspace_picker()
            .filter(|id| !matches!(origin, PickerOrigin::WorkspaceEdit(current) if current == *id));
        let (rows, payloads): (Vec<_>, Vec<_>) = items
            .into_iter()
            .map(|(item, payload)| {
                let id = item.id.clone();
                (item, (id, payload))
            })
            .unzip();
        let epoch = match self.search.install_picker(
            PickerContent {
                surface,
                origin,
                title,
                items: rows,
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
        self.input.capture_changed();
        // A refused replacement must not destroy the still-valid old preview.
        if let Some(id) = retired_preview {
            self.retire_workspace_picker(id);
        }
        self.lsp_edit_sources
            .retain_actions(payloads.iter().filter_map(|(_, payload)| match payload {
                PickerPayload::CodeAction(key) => Some(key),
                _ => None,
            }));
        self.input.cancel_prompt_completions();
        self.clear_picker_preview();
        self.picker_payloads = payloads
            .into_iter()
            .map(|(id, payload)| (id, (epoch.clone(), payload)))
            .collect();
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
        let item = self.search.picker()?.selected_item()?;
        let (epoch, payload) = self.picker_payloads.get(&item.id)?;
        self.search.owns_picker_epoch(epoch).then_some(payload)
    }

    pub(in crate::editor::session) fn open_lsp_locations_picker(
        &mut self,
        server: &str,
        locations: Vec<crate::language::lsp::LspLocation>,
    ) {
        let server = self.lsp_server_label(server);
        let items = locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| {
                let path = path_from_file_uri(&location.uri)
                    .map_or_else(|| location.uri.clone(), |path| path.display().to_string());
                let id = PickerItemId(format!("location:{index}:{}", location.uri));
                (
                    PickerItem {
                        id,
                        label: path,
                        detail: format!(
                            "{}:{}",
                            location.range.start.line + 1,
                            location.range.start.character + 1
                        ),
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspLocation(location),
                )
            })
            .collect();
        self.install_picker(self.message("picker_references_title", &[]), items);
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
                        id,
                        label: symbol.name,
                        detail: symbol.kind.map_or_else(String::new, |kind| {
                            self.message("picker_symbol_kind", &[("kind", kind.to_string())])
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
        let choices = match self.lsp_edit_sources.install_actions(
            actions,
            context,
            &self.lsp,
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
                                    |path| path.display().to_string(),
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
                let active = self.workspace.active_root();
                let trusted = self.workspace.trust() == WorkspaceTrust::Trusted;
                let items = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| {
                        let state = if active == Some(root.id) {
                            self.message("workspace_root_active", &[])
                        } else {
                            self.message("workspace_root_inactive", &[])
                        };
                        let trust = if trusted {
                            self.message("workspace_trust_trusted", &[])
                        } else {
                            self.message("workspace_trust_untrusted", &[])
                        };
                        (
                            PickerItem {
                                id: PickerItemId(format!("workspace-root:{}", root.id.0)),
                                label: self.message(
                                    "workspace_root_row",
                                    &[
                                        ("id", root.id.0.to_string()),
                                        ("name", root.name.clone()),
                                        ("state", state),
                                        ("trust", trust),
                                    ],
                                ),
                                detail: root.identity.canonical_path.display().to_string(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::WorkspaceRoot(root.id),
                        )
                    })
                    .collect();
                self.install_picker(self.message("workspace_root_picker_title", &[]), items);
            }
            PickerKind::Recents => {
                let items = self
                    .recent_files
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
                                id: PickerItemId(format!("recent:{index}")),
                                label: path.display().to_string(),
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
                    .marks
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, mark)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("mark:{index}:{}", mark.name)),
                                label: mark.name,
                                detail: format!(
                                    "{}:{}",
                                    mark.location.path.display(),
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
                        self.jump_list.clone(),
                    )
                } else {
                    (
                        self.message("navigation_change_source", &[]),
                        self.message("navigation_changes_title", &[]),
                        self.change_list.clone(),
                    )
                };
                let items = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, location)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("navigation:{index}")),
                                label: location.path.display().to_string(),
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
            PickerKind::Commands => {
                let items = self
                    .commands
                    .specs()
                    .enumerate()
                    .map(|(index, spec)| {
                        let command = spec.id.as_str().to_owned();
                        let id = PickerItemId(format!("command:{index}:{command}"));
                        (
                            PickerItem {
                                id,
                                label: command.clone(),
                                detail: self
                                    .configuration
                                    .settings()
                                    .command_labels
                                    .get(&command)
                                    .cloned()
                                    .unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(command),
                        )
                    })
                    .collect();
                match compose_command_picker_items(items) {
                    Ok(items) => {
                        self.install_picker(self.message("picker_commands_title", &[]), items)
                    }
                    Err(error) => self.show_error(error.to_string()),
                }
            }
        }
    }

    pub(in crate::editor::session) fn sync_picker_ui(&mut self) {
        self.retire_stale_picker();
        self.ui.picker = self.search.picker_projection();
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
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
        }
    }

    pub(in crate::editor::session) fn picker_backspace(&mut self) {
        self.retire_stale_picker();
        let query = self.search.picker_backspace();
        self.sync_picker_ui();
        if self.search.live()
            && let Some(query) = query
        {
            self.restart_live_workspace_search(query);
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

    fn clear_picker_preview(&mut self) {
        if let Some(id) = self.search.clear_preview() {
            self.documents.cancel_request(id);
        }
    }
    fn sync_picker_preview(&mut self) {
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
        let Some(plugin) = plugin_owner_from_id(&provider).map(str::to_owned) else {
            self.show_error("plugin provider ID is malformed");
            return;
        };
        let Some(load) = self.plugins.load_id(&plugin) else {
            self.show_error(crate::extensions::PluginError::NotActive);
            return;
        };
        if !self.plugins.is_active_generation(&plugin, load) {
            self.show_error(crate::extensions::PluginError::NotActive);
            return;
        }
        let Some(declaration) = self
            .plugin_contributions
            .get(&plugin)
            .filter(|index| index.generation == load.0)
            .and_then(|index| index.providers.iter().find(|item| item.id == provider))
            .cloned()
        else {
            self.show_error("plugin picker provider is not statically declared");
            return;
        };
        self.close_picker();
        let buffer_revision = self
            .state
            .active_buffer_id()
            .and_then(|buffer| self.state.buffers.get(buffer))
            .map(|buffer| buffer.text.revision().0);
        let request = match self
            .plugin_providers
            .start(crate::extensions::PluginProviderStart {
                plugin: plugin.clone(),
                load,
                provider: provider.clone(),
                query,
                context: "editor".into(),
                workspace_revision: Some(self.workspace.root_revision()),
                buffer_revision,
            }) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error.detail);
                return;
            }
        };
        self.active_plugin_picker = Some(request);
        self.install_picker(format!("Plugin · {provider}"), Vec::new());
        let prefix = format!("plugin.{plugin}.");
        let Some(command) = declaration.command.strip_prefix(&prefix).map(str::to_owned) else {
            self.show_error("plugin picker command is malformed");
            return;
        };
        self.process_effect(AppEffect::PluginExecute { plugin, command });
    }

    pub(in crate::editor::session) fn present_plugin_picker(&mut self, request: u64) {
        let Some(projection) = self.plugin_providers.projection(request) else {
            return;
        };
        let items = match compose_plugin_picker_items(&projection) {
            Ok(items) => items,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        self.install_picker(format!("Plugin · {}", projection.provider), items);
        self.active_plugin_picker = Some(request);
    }

    pub(in crate::editor::session) fn close_picker(&mut self) {
        self.debugger_session.dismiss_choices();
        if self.ui.picker.is_some() {
            self.input.capture_changed();
        }
        if let Some(id) = self.search.workspace_picker() {
            self.retire_workspace_picker(id);
        }
        self.lsp_edit_sources.clear_actions();
        if let Some(id) = self.search.close_picker() {
            self.documents.cancel_request(id);
        }
        if let Some(request) = self.active_plugin_picker.take() {
            self.plugin_providers.cancel(request);
        }
        self.tasks.select_output(None);
        self.picker_payloads.clear();
        self.ui.picker = None;
        self.refresh_window_content();
    }

    pub(in crate::editor::session) fn accept_picker(&mut self) {
        self.retire_stale_picker();
        let selected = self
            .search
            .picker()
            .and_then(PickerState::selected_item)
            .map(|item| item.id.clone());
        let payload = selected
            .as_ref()
            .and_then(|id| self.picker_payloads.remove(id))
            .and_then(|(epoch, payload)| self.search.owns_picker_epoch(&epoch).then_some(payload));
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
            Some(PickerPayload::CodeAction(key)) => {
                Some(self.lsp_edit_sources.take_action(key, &self.lsp))
            }
            _ => None,
        };
        let debug_selection = match &payload {
            Some(PickerPayload::Debug(key)) => Some(self.debugger_session.take_selection(key)),
            _ => None,
        };
        self.close_picker();
        match payload {
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
                }
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::WorkspaceRoot(root)) => self.activate_workspace_root(root),
            Some(PickerPayload::Terminal(key)) => {
                if let Some(id) = self.terminal_session.resolve(&key) {
                    self.terminal_session.select(id);
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
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
                match debug_selection
                    .expect("debug selection routed")
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
                if let Ok(invocation) = CommandInvocation::new(&command, InvocationSource::Picker) {
                    self.invoke(invocation);
                }
            }
            Some(PickerPayload::PluginProviderItem {
                plugin,
                load,
                request,
                item,
                action_command,
            }) => {
                let valid = self.plugins.load_id(&plugin) == Some(load)
                    && self.plugins.is_active_generation(&plugin, load)
                    && self
                        .plugin_providers
                        .projection(request)
                        .is_some_and(|projection| {
                            projection.action_command == action_command
                                && projection
                                    .items
                                    .iter()
                                    .any(|candidate| candidate.id == item)
                        });
                let prefix = format!("plugin.{plugin}.");
                if valid && let Some(command) = action_command.strip_prefix(&prefix) {
                    self.process_effect(AppEffect::PluginExecute {
                        plugin,
                        command: command.into(),
                    });
                }
            }
            _ => {}
        }
    }
}
