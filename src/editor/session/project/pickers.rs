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
        self.completion_generation = self.completion_generation.saturating_add(1);
        let generation = self.completion_generation;
        let mut picker = PickerState::new(
            generation,
            title,
            self.configuration.settings.workspace.maximum_picker_items,
        );
        self.picker_payloads.clear();
        let rows = items
            .into_iter()
            .map(|(item, payload)| {
                self.picker_payloads.insert(item.id.clone(), payload);
                item
            })
            .collect::<Vec<_>>();
        picker.append(generation, rows, true);
        self.picker_surface = surface;
        self.picker = Some(picker);
        self.sync_picker_ui();
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
    ) {
        let server = self.lsp_server_label(server);
        let items = actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| {
                let id = PickerItemId(format!("code-action:{index}:{}", action.title));
                (
                    PickerItem {
                        id,
                        label: action.title.clone(),
                        detail: action.kind.clone().unwrap_or_default(),
                        source: server.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::CodeAction(action),
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
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (label, detail, control))| {
                (
                    PickerItem {
                        id: PickerItemId(format!("dap:{kind}:{index}")),
                        label,
                        detail,
                        source: "dap".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DapControl(control),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Debug,
            self.message("dap_picker_title", &[("kind", kind.into())]),
            items,
        );
    }

    pub(in crate::editor::session) fn open_dap_frames_picker(&mut self, rows: Vec<DapStackFrame>) {
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("dap:frame:{index}:{}", row.id)),
                        label: row.name.clone(),
                        detail: row
                            .path
                            .as_ref()
                            .map(|path| format!("{}:{}:{}", path.display(), row.line, row.column))
                            .unwrap_or_default(),
                        source: "dap".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DapFrame(row),
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Debug,
            self.message("dap_picker_title", &[("kind", "frames".into())]),
            items,
        );
    }

    pub(in crate::editor::session) fn open_builtin_picker(&mut self, kind: PickerKind) {
        match kind {
            PickerKind::Diagnostics => {
                let items = self
                    .diagnostics
                    .problems(None, None)
                    .into_iter()
                    .enumerate()
                    .map(|(index, problem)| {
                        let id = PickerItemId(format!("diagnostic:{index}:{}", problem.uri));
                        let path = path_from_file_uri(&problem.uri)
                            .map_or_else(|| problem.uri.clone(), |path| path.display().to_string());
                        (
                            PickerItem {
                                id,
                                label: problem.message.clone(),
                                detail: path,
                                source: problem.source,
                                action: PickerAction::None,
                            },
                            PickerPayload::Diagnostic {
                                uri: problem.uri,
                                range: problem.range,
                            },
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
                                            .settings
                                            .ui
                                            .status_line
                                            .scratch_label
                                            .clone()
                                    },
                                    |path| path.display().to_string(),
                                ),
                                detail: if state.dirty {
                                    self.configuration
                                        .settings
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
                                    .settings
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
        self.ui.picker = self.picker.as_ref().map(|picker| {
            let presentation = picker.presentation();
            PickerUiState {
                surface: self.picker_surface,
                title: presentation.title,
                query: presentation.query,
                rows: presentation
                    .rows
                    .into_iter()
                    .map(|(id, label, detail, source)| PickerRow {
                        id: id.0,
                        label,
                        detail,
                        source,
                    })
                    .collect(),
                selected: presentation.selected,
                scroll: presentation.scroll,
                loading: presentation.loading,
            }
        });
    }

    pub(in crate::editor::session) fn picker_insert(&mut self, text: &str) {
        if let Some(picker) = &mut self.picker {
            picker.insert_query(text);
            self.sync_picker_ui();
        }
    }

    pub(in crate::editor::session) fn picker_backspace(&mut self) {
        if let Some(picker) = &mut self.picker {
            picker.backspace_query();
            self.sync_picker_ui();
        }
    }

    pub(in crate::editor::session) fn picker_move(&mut self, direction: i8) {
        if let Some(picker) = &mut self.picker {
            picker.move_selection(direction, self.configuration.settings.workspace.picker_wrap);
            let rows = usize::from(
                self.terminal_height
                    .saturating_mul(u16::from(
                        self.configuration.settings.ui.picker.height_percent,
                    ))
                    .saturating_div(100)
                    .saturating_sub(4),
            );
            picker.ensure_selected_visible(rows);
            self.sync_picker_ui();
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
        if self.workspace_file_generation.take().is_some() {
            self.workspace_files.cancel();
            self.workspace_file_root_revision = None;
            self.workspace_file_entries.clear();
        }
        if let Some(request) = self.active_plugin_picker.take() {
            self.plugin_providers.cancel(request);
        }
        self.picker = None;
        self.picker_payloads.clear();
        self.ui.picker = None;
    }

    pub(in crate::editor::session) fn accept_picker(&mut self) {
        let selected = self
            .picker
            .as_ref()
            .and_then(PickerState::selected_item)
            .map(|item| item.id.clone());
        let payload = selected
            .as_ref()
            .and_then(|id| self.picker_payloads.remove(id));
        self.close_picker();
        match payload {
            Some(PickerPayload::WorkspaceSearch(found)) => {
                self.navigate_workspace_search_match(found)
            }
            Some(PickerPayload::LspLocation(location)) => self.navigate_lsp_location(&location),
            Some(PickerPayload::Diagnostic { uri, range }) => {
                self.navigate_byte_location(&uri, range)
            }
            Some(PickerPayload::CodeAction(action)) => {
                if let Some(edit) = action.edit {
                    self.begin_workspace_proposal(
                        action.title,
                        edit,
                        None,
                        self.lsp_position_encoding(),
                    );
                }
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::WorkspaceRoot(root)) => self.activate_workspace_root(root),
            Some(PickerPayload::LspSession(key)) => self.select_lsp_session(&key),
            Some(PickerPayload::DapFrame(frame)) => {
                let _ = self.dap.control(DapControl::Scopes { frame_id: frame.id });
                if let Some(reference) = frame.source_reference {
                    let _ = self.dap.control(DapControl::Source { reference });
                } else if let Some(path) = frame.path {
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: frame.line.saturating_sub(1) as usize,
                        column: frame.column.saturating_sub(1) as usize,
                    });
                }
            }
            Some(PickerPayload::GitHunk {
                path,
                status_revision,
                repository,
                hunk,
                new_line,
            }) => {
                if self.git_status_snapshot.as_ref().is_some_and(|s| {
                    s.revision == status_revision
                        && s.repository_id == repository
                        && s.root_revision == self.workspace.root_revision()
                }) {
                    if let Some(snapshot) = self.git_hunk_snapshot.as_ref()
                        && let Some(selected) =
                            snapshot.hunks.iter().find(|candidate| candidate.id == hunk)
                    {
                        self.selected_git_hunk =
                            Some((snapshot.clone(), selected.clone(), path.clone()));
                    }
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: new_line,
                        column: 0,
                    })
                } else {
                    self.show_error("Git hunk is stale")
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
            Some(PickerPayload::DapControl(control)) => {
                if let Err(error) = self.dap.control(control) {
                    self.show_error(error)
                }
            }
            Some(PickerPayload::ProposalFile(index)) => {
                if let Some(proposal) = &mut self.workspace_proposal {
                    let _ = proposal.toggle_file(index);
                    self.show_workspace_proposal();
                }
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
