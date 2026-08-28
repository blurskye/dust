use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_language_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::OpenToolMenu(category) => self.open_tool_menu(category),
            AppEffect::CloseToolMenu => self.close_tool_menu(),
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
                if let Some(key) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&key) {
                        state.restart_count = 0;
                        state.restart_due = None;
                        state.circuit_open = false;
                        state.desired_running = true;
                    }
                }
                self.start_lsp(None);
            }
            AppEffect::LspStop => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&server) {
                        state.desired_running = false;
                        state.restart_due = None;
                    }
                    if let Err(error) = self.lsp.stop(&server) {
                        self.show_error(error)
                    }
                    self.clear_lsp_progress(&server);
                    self.lsp_sessions.remove(&server);
                    self.clear_lsp_session_state();
                }
            }
            AppEffect::LspRestart => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&server) {
                        state.restart_count = 0;
                        state.restart_due = Some(Instant::now());
                        state.circuit_open = false;
                        state.desired_running = true;
                    }
                    let _ = self.lsp.stop(&server);
                    self.clear_lsp_progress(&server);
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspStatus => {
                let mut servers = self
                    .lsp
                    .running_servers()
                    .iter()
                    .map(|server| self.lsp_server_label(server))
                    .collect::<Vec<_>>();
                servers.sort();
                servers.dedup();
                self.ui.notification = Some(self.message(
                    "lsp_status",
                    &[(
                        "servers",
                        if servers.is_empty() {
                            "none".into()
                        } else {
                            servers.join(", ")
                        },
                    )],
                ));
                self.ui.notification_error = false;
                if let Some(server) = self.active_lsp_server.as_deref() {
                    let _ = self.lsp.health(server);
                }
            }
            AppEffect::LspAttach => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some((buffer, document)) = self.active_lsp_document() {
                        let revision = BufferRevision(document.version);
                        if let Err(error) = self.lsp.sync(&server, document) {
                            self.show_error(error);
                        } else {
                            self.lsp_synced_revisions.insert(buffer, revision);
                        }
                    }
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspDetach => {
                if let (Some(server), Some((buffer, document))) = (
                    self.active_lsp_server.as_deref(),
                    self.active_lsp_document(),
                ) {
                    let uri = document.uri();
                    let _ = self.lsp.close(server, uri.clone());
                    self.lsp_synced_revisions.remove(&buffer);
                    if let Ok(owner) = DiagnosticOwner::new(format!("lsp.{server}")) {
                        self.diagnostics.clear_owner_uri(&owner, &uri);
                    }
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::LspSessions => self.show_lsp_sessions(),
            AppEffect::LspSessionSelect(key) => self.select_lsp_session(&key),
            AppEffect::LspLogs => {
                let items = self
                    .lsp_logs
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        (
                            PickerItem {
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
            }
            AppEffect::LspInlayHintsToggle => {
                self.configuration.settings.lsp.inlay_hints =
                    !self.configuration.settings.lsp.inlay_hints;
                let enabled = self.configuration.settings.lsp.inlay_hints;
                self.schedule_lsp_inlay_refresh(true);
                self.ui.notification = Some(format!(
                    "inlay hints {}",
                    if enabled { "enabled" } else { "disabled" }
                ));
                self.ui.notification_error = false;
            }
            AppEffect::LspRequest(feature) => self.request_lsp(feature),
            AppEffect::LspDocumentationScroll(direction) => {
                if let Some(popup) = self.ui.documentation_popup.as_mut() {
                    let page = usize::from(
                        self.configuration
                            .settings
                            .ui
                            .documentation_popup
                            .max_height
                            .saturating_sub(2),
                    )
                    .max(1);
                    let maximum = popup.line_count().saturating_sub(page);
                    let step = self
                        .configuration
                        .settings
                        .ui
                        .documentation_popup
                        .scroll_step;
                    popup.scroll = if direction < 0 {
                        popup.scroll.saturating_sub(step)
                    } else {
                        popup.scroll.saturating_add(step).min(maximum)
                    };
                }
            }
            AppEffect::LspDocumentationClose => self.ui.documentation_popup = None,
            AppEffect::LspCompletionDocumentationOpen => self.open_lsp_completion_documentation(),
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::DiagnosticNavigate(direction) => self.navigate_diagnostic(direction),
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspCompletionCancel => {
                let generation = self.lsp_request_generation;
                if let Some(server) = self.active_lsp_server.as_deref() {
                    let _ = self.lsp.cancel(server, generation);
                }
                self.lsp_completion_items.clear();
                self.lsp_completion_owner = None;
                self.ui.language_completion = None;
            }
            AppEffect::SnippetNavigate(direction) => self.navigate_snippet(direction),
            AppEffect::SnippetCancel => {
                self.snippet_session = None;
                if let Some(view) = self.state.active_view {
                    self.state.views[view].selection = None;
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
                    self.lsp_selection_ranges
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
