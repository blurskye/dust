use super::*;

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
        match self.terminal_session.request_open(
            profile,
            &self.workspace,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
            self.input.capture_epoch(),
        ) {
            Ok(intent) => self.defer_trusted_execution(if let Some(reply) = reply {
                DeferredExecution::OpenDapTerminal { intent, reply }
            } else {
                DeferredExecution::OpenTerminal(intent)
            }),
            Err(error) => {
                if let Some(reply) = reply {
                    let _ = reply.respond(false, Some(error.clone()));
                }
                self.show_error(error);
            }
        }
    }

    pub(super) fn open_terminal_after_validation_with_response(
        &mut self,
        intent: super::super::terminal::TerminalIntent,
        reply: Option<crate::debugger::DapTerminalReply>,
        authorization: Option<ExecutionAuthorization>,
    ) {
        if reply.as_ref().is_some_and(|r| !r.current()) {
            self.show_error("requesting debugger retired before terminal admission");
            return;
        }
        let Some(permit) = authorization.and_then(|a| a.terminal_permit(&self.workspace, &intent))
        else {
            self.show_error("terminal launch requires its original authorization");
            return;
        };
        match self
            .terminal_session
            .prepare(intent, permit, &self.workspace)
        {
            Ok(open) => {
                let id = self
                    .terminal_session
                    .resolve(&open.key)
                    .expect("new terminal");
                match self.workspace.request_preflight(open.request) {
                    Ok(request) => {
                        self.pending_execution_preflight.insert(
                            request,
                            PendingExecutionPreflight::Terminal { key: open.key },
                        );
                        if let Some(reply) = reply {
                            self.debugger_session.register_terminal(id, reply);
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
                            self.set_terminal_focus(true);
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
                    let result = ready.result();
                    let Some(action) = self.pending_execution_preflight.remove(&result.id) else {
                        continue;
                    };
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        if let PendingExecutionPreflight::Task { step } = &action {
                            self.tasks.reject_step(step);
                        }
                        if let PendingExecutionPreflight::Terminal { key } = &action {
                            if let Some(id) = self.terminal_session.resolve(key) {
                                if let Some(reply) = self.debugger_session.take_terminal(&id) {
                                    let _ = reply
                                        .respond(false, Some("terminal workspace retired".into()));
                                }
                            }
                            self.terminal_session.reject(key);
                            self.sync_terminal_panel();
                        }
                        continue;
                    }
                    if let PendingExecutionPreflight::Task { step } = action {
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
                    if let PendingExecutionPreflight::Terminal { key } = action {
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
                    let result = ready.into_result();
                    match action {
                        PendingExecutionPreflight::Lsp {
                            session_key,
                            definition,
                            launch,
                            root_id,
                            project_root,
                            buffer,
                            document,
                            restart,
                        } => self.launch_preflighted_lsp(
                            super::lsp::LspLaunchRequest {
                                session_key,
                                definition,
                                launch,
                                root_id,
                                project_root,
                                buffer,
                                document,
                                restart,
                            },
                            result,
                        ),
                        PendingExecutionPreflight::Task { .. } => continue,
                        PendingExecutionPreflight::Terminal { .. } => unreachable!("routed above"),
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
                    let action = self.pending_execution_preflight.remove(&id);
                    if let Some(PendingExecutionPreflight::Task { step }) = &action {
                        self.tasks.reject_step(step);
                    }
                    if let Some(PendingExecutionPreflight::Terminal { key }) = &action {
                        if let Some(id) = self.terminal_session.resolve(key) {
                            if let Some(reply) = self.debugger_session.take_terminal(&id) {
                                let _ = reply.respond(false, Some(message.clone()));
                            }
                        }
                        self.terminal_session.reject(key);
                        self.sync_terminal_panel();
                    }
                    if root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }

    pub(super) fn process_pty_events(&mut self) {
        let updates = self.terminal_session.drain(&self.workspace);
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
                TerminalUpdate::Failed(id, message) => {
                    if let Some(reply) = self.debugger_session.take_terminal(&id) {
                        let _ = reply.respond(false, Some(message.clone()));
                    }
                    self.show_error(message);
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
