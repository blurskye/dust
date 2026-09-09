use super::*;

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
            AppEffect::LspRequest(feature) => self.request_lsp(feature),
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
            AppEffect::LspCompletionDocumentationOpen => self.open_lsp_completion_documentation(),
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::DiagnosticNavigate { direction, filter } => {
                self.navigate_diagnostic(direction, filter)
            }
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspCompletionCancel => {
                let generation = self.language.generation();
                if let Some(server) = self.language.active().map(String::as_str) {
                    let _ = self.language.cancel(server, generation);
                }
                self.language.clear_completion();
                self.ui.language_completion = None;
            }
            AppEffect::SnippetNavigate(direction) => self.navigate_snippet(direction),
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
