use super::*;

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
            } => self.start_save(quit_after, path, force),
            AppEffect::OpenPath(path) => self.start_open(path),
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
            AppEffect::GitStatusRefresh => self.start_git_status_refresh(),
            AppEffect::GitStatusOpen => self.open_git_status_picker(),
            AppEffect::GitHunksOpen => self.start_git_hunks(),
            AppEffect::GitHunkNavigate(direction) => self.navigate_git_hunk(direction),
            AppEffect::GitHunkPreview => self.preview_selected_git_hunk(),
            AppEffect::GitHunkStage => self.stage_selected_git_hunk(),
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
            AppEffect::PromptConfirmationSelect(choice) => {
                self.input.select_confirmation(choice);
                self.project_input_prompt();
            }
            AppEffect::PromptConfirmationToggle => {
                self.input.toggle_confirmation();
                self.project_input_prompt();
            }
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => self.close_prompt(),
            AppEffect::NotificationHistory => {
                let items = self
                    .notices
                    .history()
                    .rev()
                    .enumerate()
                    .map(|(index, (error, message))| {
                        (
                            PickerItem {
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
            AppEffect::SurfaceFocus(direction) => self.focus_surface(direction),
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
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(
                            target,
                            WindowPlacement::Docked { edge, size },
                        )
                    });
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
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(
                            target,
                            WindowPlacement::Floating(WindowRect {
                                x,
                                y,
                                width,
                                height,
                            }),
                        )
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowFullscreen(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(target, WindowPlacement::Fullscreen)
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowHide(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| {
                        self.apply_native_window_placement(target, WindowPlacement::Hidden)
                    });
                if let Err(error) = result {
                    self.show_error(error);
                }
            }
            AppEffect::WindowShow(target) => {
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| self.show_native_window(target));
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
                let result = self
                    .resolve_native_window_target(&target)
                    .and_then(|target| self.move_native_window(target, x, y));
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
    fn document_view(
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
    pub(in crate::editor::session) fn start_save(
        &mut self,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
    ) {
        let view = self.state.active_buffer_id().and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|b| Self::document_view(id, b))
        });
        let request = crate::editor::session::documents::SaveRequest {
            editor: &self.state.window_version(),
            workspace: self.workspace.version(),
            base: &self.state.workspace_root,
            path: requested_path,
            policy: &self.configuration.settings().editor,
            force,
            quit: quit_after,
            other_open_paths: self
                .state
                .buffers
                .iter()
                .filter(|(id, _)| Some(*id) != self.state.active_buffer_id())
                .filter_map(|(_, b)| b.path.clone())
                .collect(),
        };
        match self.documents.request_save(view, request) {
            Ok(()) => self.notify_message(self.message("saving", &[]), false),
            Err(e) => self.show_document_failure(e),
        }
    }
    pub(in crate::editor::session) fn close_active_buffer(&mut self, force: bool) {
        let Some(view_id) = self.state.active_view else {
            self.set_dashboard_visibility(true);
            return;
        };
        let buffer_id = self.state.views[view_id].buffer;
        let closed_path = self.state.buffers[buffer_id].path.clone();
        if self.state.buffers[buffer_id].dirty && !force {
            self.show_error(self.message("dirty_close", &[]));
            return;
        }
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&self.state).into())
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
            self.documents
                .retire_buffer(buffer_id, closed_path.as_deref());
            if let Some(path) = &closed_path
                && let Some(id) = self.explorer.retire_buffer_path(path)
            {
                self.documents.cancel_request(id);
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
        if let Err(error) = self
            .window_session
            .check_editor_admission(&(&self.state).into())
        {
            self.show_error(error);
            return;
        }
        if let Err(error) = reply.check_save_destination(
            self.state
                .buffers
                .iter()
                .filter_map(|(id, b)| b.path.as_deref().map(|path| (id, path))),
        ) {
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
                    self.message("opened", &[("path", path.display().to_string())]),
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
                self.notify_message(
                    self.message("reloaded_file", &[("path", path.display().to_string())]),
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
            } => {
                if !candidate.save_document(buffer, &text, revision, bytes, path.clone()) {
                    return;
                }
                if let Err(e) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(e);
                    return;
                }
                let buffer_state = &self.state.buffers[buffer];
                let still_dirty = buffer_state.dirty;
                let lsp_document = buffer_state.path.clone().and_then(|path| {
                    Some(LspDocument {
                        language_id: language_id_for_path(&path)?.into(),
                        path,
                        version: buffer_state.text.revision().0,
                        text: buffer_state.text.to_string(),
                    })
                });
                self.notify_message(
                    if still_dirty {
                        self.message(
                            "saved_dirty",
                            &[
                                ("revision", revision.0.to_string()),
                                ("path", path.display().to_string()),
                            ],
                        )
                    } else {
                        self.message("saved", &[("path", path.display().to_string())])
                    },
                    false,
                );
                if let Some(server) = self.language.active().map(String::as_str)
                    && let Some(document) = lsp_document
                {
                    let _ = self.language.save(server, document);
                    // An older save snapshot must not leave the worker's document
                    // identity behind the current editor incarnation/content.
                    self.language.invalidate_active_document(buffer);
                }
                if self.ui.explorer.is_some() {
                    self.start_git_status_refresh_with_feedback(false);
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

#[derive(Eq, PartialEq)]
pub(in crate::editor::session) enum ExecutionAdmission {
    Tool(String, Option<LspRuntimeSource>),
    Lsp(String),
    Restart(String),
    Debug(crate::editor::session::debugger::DebugIntent),
    Task(String, crate::config::TaskKind),
    Terminal(crate::editor::session::terminal::TerminalIntent),
}
impl ExecutionAdmission {
    fn for_action(action: &DeferredExecution) -> Option<Self> {
        Some(match action {
            DeferredExecution::InstallTool { id, source } => Self::Tool(id.clone(), *source),
            DeferredExecution::StartLsp(Some(id)) => Self::Lsp(id.clone()),
            DeferredExecution::RestartLsp(id) => Self::Restart(id.clone()),
            DeferredExecution::StartDebug(intent) => Self::Debug(intent.clone()),
            DeferredExecution::OpenTerminal(intent)
            | DeferredExecution::OpenDapTerminal { intent, .. } => Self::Terminal(intent.clone()),
            DeferredExecution::RunTask(name) => {
                Self::Task(name.clone(), crate::config::TaskKind::Task)
            }
            DeferredExecution::RunTest { name, .. } => {
                Self::Task(name.clone(), crate::config::TaskKind::Test)
            }
            DeferredExecution::RunLinter { name, .. } => {
                Self::Task(name.clone(), crate::config::TaskKind::Linter)
            }
            DeferredExecution::RunFormatter { name, .. } => {
                Self::Task(name.clone(), crate::config::TaskKind::Formatter)
            }
            _ => return None,
        })
    }
}
pub(in crate::editor::session) struct ExecutionAuthorization {
    admission: ExecutionAdmission,
    id: crate::editor::session::workspace::ValidationId,
    permit: crate::editor::session::workspace::ExecutionPermit,
}
impl ExecutionAuthorization {
    pub(in crate::editor::session) fn debug_permit(
        self,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        intent: &crate::editor::session::debugger::DebugIntent,
    ) -> Option<crate::editor::session::workspace::DebugPermit> {
        if self.admission != ExecutionAdmission::Debug(intent.clone()) {
            return None;
        }
        workspace.debug_permit(self.permit, self.id, intent.nonce())
    }
    pub(in crate::editor::session) fn terminal_permit(
        self,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        intent: &crate::editor::session::terminal::TerminalIntent,
    ) -> Option<crate::editor::session::workspace::TerminalPermit> {
        if self.admission != ExecutionAdmission::Terminal(intent.clone()) {
            return None;
        }
        workspace.terminal_permit(
            self.permit,
            self.id,
            intent.profile().into(),
            intent.nonce(),
        )
    }

    pub(in crate::editor::session) fn task_permit(
        self,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        name: &str,
        kind: crate::config::TaskKind,
    ) -> Option<crate::editor::session::workspace::TaskPermit> {
        if self.admission != ExecutionAdmission::Task(name.into(), kind) {
            return None;
        }
        workspace.task_permit(self.permit, self.id, name.into(), kind)
    }

    pub(in crate::editor::session) fn consume(
        self,
        workspace: &crate::editor::session::workspace::WorkspaceSession,
        requested: ExecutionAdmission,
    ) -> bool {
        self.admission == requested && workspace.consume_permit(self.permit, self.id)
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
    pub(in crate::editor::session) fn defer_trusted_execution(
        &mut self,
        action: DeferredExecution,
    ) {
        match self.workspace.begin_validation() {
            Ok(id) => {
                match action {
                    DeferredExecution::InstallTool { id: tool, source } => {
                        self.tooling.defer_validation(id, tool, source)
                    }
                    DeferredExecution::StartLsp(server) => {
                        self.language.defer_start_validation(id, server)
                    }
                    DeferredExecution::RestartLsp(server) => {
                        self.language.defer_restart_validation(id, server)
                    }
                    DeferredExecution::StartDebug(intent) => {
                        self.debugger_session.defer_validation(id, intent)
                    }
                    DeferredExecution::InstallRegistry {
                        publisher,
                        name,
                        version,
                    } => self
                        .extension
                        .defer_registry_validation(id, publisher, name, version),
                    DeferredExecution::PrepareRegistryArtifact => {
                        self.extension.defer_artifact_validation(id)
                    }
                    DeferredExecution::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    } => self.extension.defer_package_validation(
                        id,
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    ),
                    DeferredExecution::RunTask(name) => {
                        self.tasks.defer_validation(id, name, AdapterContext::Plain)
                    }
                    DeferredExecution::OpenTerminal(intent) => {
                        self.terminal_session.defer_validation(id, intent, None)
                    }
                    DeferredExecution::OpenDapTerminal { intent, reply } => self
                        .terminal_session
                        .defer_validation(id, intent, Some(reply)),
                    DeferredExecution::RunTest { name, matcher } => {
                        self.tasks
                            .defer_validation(id, name, AdapterContext::Test { matcher })
                    }
                    DeferredExecution::RunLinter { name, matcher } => {
                        self.tasks
                            .defer_validation(id, name, AdapterContext::Linter { matcher })
                    }
                    DeferredExecution::RunFormatter { name, source } => self
                        .tasks
                        .defer_validation(id, name, AdapterContext::Formatter { source }),
                }
                self.notify_message(self.message("workspace_execution_validating", &[]), false);
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
    fn finish_execution_validation(
        &mut self,
        id: crate::editor::session::workspace::ValidationId,
        permit: Option<crate::editor::session::workspace::ExecutionPermit>,
    ) {
        let Some(action) = self
            .tooling
            .take_validation(id)
            .or_else(|| self.language.take_validation(id))
            .or_else(|| self.debugger_session.take_validation(id))
            .or_else(|| self.extension.take_validation(id))
            .or_else(|| self.tasks.take_validation(id))
            .or_else(|| self.terminal_session.take_validation(id))
        else {
            return;
        };
        let Some(permit) = permit else {
            self.show_error(self.message(
                "workspace_execution_validation_failed",
                &[("diagnostic", "workspace root identity changed".into())],
            ));
            return;
        };
        if !self.workspace.permit_current(&permit, id) {
            self.show_error("workspace execution permit is stale");
            return;
        }
        self.clear_notice();
        let authorization = if let Some(admission) = ExecutionAdmission::for_action(&action) {
            Some(ExecutionAuthorization {
                admission,
                id,
                permit,
            })
        } else {
            if !self.workspace.consume_permit(permit, id) {
                self.show_error("workspace execution permit is stale");
                return;
            }
            None
        };
        match action {
            DeferredExecution::InstallTool { id, source } => {
                self.install_tool_authorized(&id, source, authorization)
            }
            DeferredExecution::StartLsp(requested) => {
                self.start_lsp_authorized(requested, authorization)
            }
            DeferredExecution::RestartLsp(key) => self.restart_lsp_authorized(key, authorization),
            DeferredExecution::StartDebug(intent) => {
                self.authorize_debug_launch(intent, authorization)
            }
            DeferredExecution::InstallRegistry {
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
            DeferredExecution::PrepareRegistryArtifact => {
                if let Err(error) = self.extension.prepare_artifact(self.plugin_limits()) {
                    self.show_error(error);
                }
            }

            DeferredExecution::InstallPackage {
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
            DeferredExecution::RunTask(name) => {
                self.start_task_after_validation(&name, authorization)
            }
            DeferredExecution::OpenTerminal(intent) => {
                self.open_terminal_after_validation_with_response(intent, None, authorization)
            }
            DeferredExecution::OpenDapTerminal { intent, reply } => self
                .open_terminal_after_validation_with_response(intent, Some(reply), authorization),
            DeferredExecution::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher }, authorization)
            }
            DeferredExecution::RunLinter { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Linter { matcher }, authorization)
            }
            DeferredExecution::RunFormatter { name, source } => {
                self.start_task_plan(&name, AdapterContext::Formatter { source }, authorization)
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
                WorkspaceUpdate::ScopeInvalidated => self.documents.retire_startup(),
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
                WorkspaceUpdate::Validated { id, permit } => {
                    self.finish_execution_validation(id, permit)
                }
            }
        }
    }
    pub(in crate::editor::session) fn maybe_prompt_for_unseen_workspace(&mut self) {
        if self.workspace.should_prompt(
            self.input.prompt().is_some()
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
                self.open_prompt(PromptKind::WorkspaceTrust, String::new());
                self.clear_notice();
                self.prompt_help(
                    self.message(
                        "workspace_trust_confirmation",
                        &[(
                            "phrase",
                            self.configuration
                                .settings()
                                .workspace
                                .trust_confirmation_phrase
                                .clone(),
                        )],
                    ),
                );
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
    pub(in crate::editor::session) fn confirm_workspace_trust(&mut self, phrase: String) {
        match self.workspace.confirm(
            &phrase,
            &self
                .configuration
                .settings()
                .workspace
                .trust_confirmation_phrase,
        ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::session::workspace::{ExecutionPermit, ValidationId, WorkspaceSession};
    fn permit(owner: &mut WorkspaceSession) -> (ValidationId, ExecutionPermit) {
        let id = owner
            .begin_validation()
            .unwrap_or_else(|_| panic!("validation"));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            for event in owner.poll() {
                if let WorkspaceUpdate::Validated {
                    id: actual,
                    permit: Some(permit),
                } = event
                {
                    assert_eq!(id, actual);
                    return (id, permit);
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn explicit_authorizations_are_consumed_by_exact_arguments_and_current_workspace_only() {
        let root = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(root.path()).unwrap();
        let state =
            WorkspaceState::new(WorkspaceId(1), [("root".into(), identity.clone())]).unwrap();
        let mut owner =
            WorkspaceSession::new(state, root.path().join("trust.toml"), 2, false, false, 2);
        owner
            .fixture_identity(crate::workspace::RootId(1), identity.clone())
            .unwrap();
        owner.fixture_trust_identity(identity);
        owner.reconcile_trust();
        let (id, permit) = permit(&mut owner);
        let rejected = ExecutionAuthorization {
            admission: ExecutionAdmission::Lsp("one".into()),
            id,
            permit,
        };
        assert!(!rejected.consume(&owner, ExecutionAdmission::Lsp("two".into())));
        let (id, token) = self::permit(&mut owner);
        let valid = ExecutionAuthorization {
            admission: ExecutionAdmission::Lsp("one".into()),
            id,
            permit: token,
        };
        assert!(valid.consume(&owner, ExecutionAdmission::Lsp("one".into())));
        let (id, token) = self::permit(&mut owner);
        let stale = ExecutionAuthorization {
            admission: ExecutionAdmission::Restart("one".into()),
            id,
            permit: token,
        };
        owner.fixture_safe_mode(true);
        assert!(!stale.consume(&owner, ExecutionAdmission::Restart("one".into())));
    }
}
