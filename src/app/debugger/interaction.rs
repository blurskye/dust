use crate::debugger::DapControl;
use crate::debugger::DapEvent;
use crate::debugger::DapResponse;
use crate::debugger::DapSourceBreakpoint;
use crate::app::App;
use std::path::PathBuf;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::app::picker::PickerPayload;
use crate::ui::PickerSurfaceKind;
use crate::editor::text::PositionCodec;
use crate::editor::text::PositionEncoding;
use crate::editor::text::TextBuffer;
use crate::app::TrustedAction;
use crate::language::lsp::file_uri;
use crate::language::lsp::language_id_for_path;
use crate::app::command;






impl App {

    pub(in crate::app) fn start_dap_configuration(&mut self, name: &str) {
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
}

impl App {

    pub(in crate::app) fn start_dap(&mut self, requested: Option<String>, program: Option<PathBuf>) {
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
}

impl App {

    pub(in crate::app) fn authorize_debug_launch(
        &mut self,
        intent: crate::app::debugger::DebugIntent,
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
}

impl App {

    pub(in crate::app) fn advance_debug_launch(&mut self) {
        match self.debugger_session.advance_debug(
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Debugger),
            &self.configuration.settings().debug,
            self.configuration.binding_revision(),
            self.tooling.entries(),
        ) {
            Ok(Some(name)) => {
                self.notify_message(self.message("dap_started", &[("adapter", name)]), false);
                self.notices.mark_current_passing();
            }
            Ok(None) => {}
            // A launch retired by revoked trust says how to get trust back, as tasks do.
            Err(error) => match self.workspace.execution_allowed() {
                Err(failure) => self.show_workspace_failure(failure),
                Ok(()) => self.show_error(error),
            },
        }
    }
}

impl App {

    pub(in crate::app) fn process_dap_project_root(&mut self) {
        if let Some(result) = self
            .workspace
            .project_result(crate::app::workspace::DiscoveryKind::Debugger)
            && let Err(error) = self.debugger_session.debug_root_result(result)
        {
            let _ = self.debugger_session.stop();
            self.show_error(error);
        }
        self.advance_debug_launch();
    }
}

impl App {

    pub(in crate::app) fn process_dap_events(&mut self) {
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
                DapEvent::RequestFailed {
                    command, message, ..
                } => {
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
                DapEvent::Response {
                    response: DapResponse::Disconnected { .. },
                    ..
                } => {
                    self.notify_message(
                        if self.debugger_session.end_state()
                            == Some(crate::debugger::DapSessionEnd::Terminated)
                        {
                            "debug session terminated"
                        } else {
                            "debug session detached; program left running"
                        }
                        .into(),
                        false,
                    );
                }
                DapEvent::Response { response, .. } => {
                    match self.debugger_session.response(response) {
                        crate::app::debugger::DebugResponse::Retired
                        | crate::app::debugger::DebugResponse::Absorbed => {}
                        crate::app::debugger::DebugResponse::StoppedAt(location) => {
                            // A library frame outside the workspace stays in the stack pane.
                            if self.workspace.roots().iter().any(|root| {
                                location.path.starts_with(&root.identity.canonical_path)
                            }) {
                                self.navigate_debug_location(location);
                            }
                        }
                        crate::app::debugger::DebugResponse::StoppedSource(frame) => {
                            let target = crate::app::debugger::SourceTarget {
                                line: frame.line,
                                column: frame.column,
                                navigation: self.window_session.navigation_stamp(
                                    self.state.window_version(),
                                    self.state.active_view,
                                ),
                                capture: self.input.capture_epoch(),
                            };
                            if let Err(error) = self.debugger_session.request_source(
                                frame.source_reference.expect("stopped source reference"),
                                target,
                            ) {
                                self.show_error(error);
                            }
                        }
                        crate::app::debugger::DebugResponse::Picker { kind, rows } => {
                            self.install_debug_rows(kind, rows)
                        }
                        crate::app::debugger::DebugResponse::Result(response) => {
                            match response {
                                DapResponse::Disconnected { .. }
                                | DapResponse::BreakpointChanged { .. } => {}
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
                                    if let Err(error) = draft.open_virtual_read_only(
                                        TextBuffer::from_text(&content),
                                        "dap".into(),
                                        0,
                                        0,
                                        0,
                                    ) {
                                        self.show_error(error);
                                        continue;
                                    }
                                    if let Err(error) = self.commit_editor_draft(draft, None, false)
                                    {
                                        self.show_error(error);
                                        continue;
                                    }
                                    if let Some(view) = self.state.active_view {
                                        let buffer = self.state.views[view].buffer;
                                        let text = &self.state.buffers[buffer].text;
                                        let line = target.line.saturating_sub(1) as usize;
                                        let line = line.min(text.len_lines().saturating_sub(1));
                                        let position = match PositionCodec::new(text).lsp_to_byte(
                                            line,
                                            target.column.saturating_sub(1) as usize,
                                            PositionEncoding::Utf16,
                                        ) {
                                            Ok(position) => position,
                                            Err(error) => {
                                                self.show_error(error);
                                                continue;
                                            }
                                        };
                                        self.state.views[view].cursor = position;
                                        self.state.mark_editor_changed();
                                        self.ensure_cursor_visible();
                                    }
                                    self.notify_message(
                                        self.message(
                                            "dap_source_loaded",
                                            &[
                                                ("name", name),
                                                ("mime", mime_type.unwrap_or_default()),
                                            ],
                                        ),
                                        false,
                                    );
                                }
                            }
                        }
                    }
                }
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
                        self.debugger_session
                            .adapter_terminal(&cwd, &arguments, &environment)
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
                    // A rejected launch leaves an empty terminal whose "exit 0" reads as a finished run.
                    self.close_debug_terminal();
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

impl App {


    /// The program's terminal from the last run. It stays after a run ends so its output can be
    /// read, and goes when the user stops the program or runs it again.
    pub(in crate::app) fn close_debug_terminal(&mut self) {
        let Some(id) = self.debug_terminal.take() else {
            return;
        };
        if self.terminal_session.request_stop(id).is_ok() {
            self.sync_terminal_panel();
        }
    }
}

impl App {


    fn open_adapter_terminal(
        &mut self,
        program: crate::config::TerminalProfileConfig,
        reply: crate::debugger::DapTerminalReply,
    ) {
        match self.terminal_session.open_program(
            crate::app::terminal::DEBUG_TERMINAL_PROFILE,
            program,
            &self.workspace,
            self.window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view),
            self.input.capture_epoch(),
            false,
        ) {
            Ok(intent) => self.run_trusted(TrustedAction::OpenDapTerminal { intent, reply }),
            Err(error) => {
                let _ = reply.respond(false, Some(error.clone()));
                self.show_error(error);
            }
        }
    }
}

impl App {


    pub(in crate::app) fn navigate_debug_location(
        &mut self,
        location: crate::app::debugger::DebugLocation,
    ) {
        let position = crate::language::lsp::LspPosition {
            line: usize::try_from(location.line.saturating_sub(1)).unwrap_or(usize::MAX),
            character: usize::try_from(location.column.saturating_sub(1)).unwrap_or(usize::MAX),
        };
        self.navigate_location_encoded(
            &crate::language::lsp::LspLocation {
                uri: crate::language::lsp::file_uri(&location.path),
                range: crate::language::lsp::LspRange {
                    start: position.clone(),
                    end: position,
                },
            },
            PositionEncoding::Utf16,
            true,
        );
    }
}

impl App {


    pub(in crate::app) fn open_dap_picker(
        &mut self,
        kind: &str,
        rows: Vec<(String, String, DapControl)>,
    ) {
        let saved = rows
            .iter()
            .map(|(label, detail, _)| (label.clone(), detail.clone()))
            .collect::<Vec<_>>();
        let rows = self.debugger_session.controls(rows);
        if kind == "watches" && rows.is_empty() {
            let items = saved
                .into_iter()
                .enumerate()
                .map(|(index, (label, detail))| {
                    (
                        PickerItem {
                            badge: String::new(),
                            kind: crate::workspace::PickerItemKind::Entry,
                            id: PickerItemId(format!("dap:watches:{index}")),
                            label,
                            detail,
                            source: "dap".into(),
                            action: PickerAction::None,
                        },
                        PickerPayload::Command(String::new()),
                    )
                })
                .collect();
            self.install_surface_picker(
                PickerSurfaceKind::Debug,
                self.message("dap_picker_title", &[("kind", kind.into())]),
                items,
            );
        } else {
            self.install_debug_rows(kind, rows);
        }
    }
}

impl App {

    pub(in crate::app) fn install_debug_rows(
        &mut self,
        kind: &str,
        rows: Vec<crate::app::debugger::DebugRow>,
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
}

impl App {

    /// Show what is TRUE about each breakpoint. The rows carry the adapter's verdict, so a
    /// request is never presented as a fact.
    pub(in crate::app) fn cursor_breakpoint_place(&self) -> Option<(PathBuf, u64)> {
        let view = &self.state.views[self.state.active_view?];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).ok()?;
        Some((buffer.path.clone()?, line as u64 + 1))
    }
}

impl App {

    pub(in crate::app) fn configure_breakpoint(
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
}

impl App {

    pub(in crate::app) fn open_breakpoint_states(&mut self) {
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
                        label: format!("{}:{}", self.workspace_relative_label(&row.path), row.line),
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
        let title = format!(
            "Breakpoints · {} (what is true, not what was asked)",
            items.len()
        );
        self.install_picker(title, items);
    }
}

impl App {

    /// Enable or disable the breakpoint on the cursor line. Disabling is SESSION state, so it
    /// survives whatever the adapter later says about that breakpoint.
    pub(in crate::app) fn toggle_breakpoint_disabled(&mut self) {
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
        let Some(disabled) = self.debugger_session.breakpoint_disabled(&path, line) else {
            self.show_error("no breakpoint on this line");
            return;
        };
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
}

impl App {

    /// TERMINATE: ask for the debuggee to end with the session.
    pub(in crate::app) fn terminate_debug_session(&mut self) {
        if let Err(error) = self.debugger_session.terminate_debuggee() {
            self.show_error(error);
        }
    }
}

impl App {

    /// Leave the debuggee running after the adapter accepts disconnect.
    pub(in crate::app) fn detach_debug_session(&mut self) {
        if let Err(error) = self.debugger_session.detach_debuggee() {
            self.show_error(error);
        }
    }
}

impl App {

    /// Project the operation bar from the repository's REAL state. It is projected whenever the
    /// status is refreshed rather than per frame, and it is cleared when nothing is paused, so
    /// its presence always means an operation is genuinely in progress.
    /// Project the debug panes from the session's OWN state: what it is attached to or running,
    /// the breakpoints it holds, its watches and its console. Absent when there is no session, so
    /// the whole arrangement appears and disappears as ONE decision.
    pub(in crate::app) fn project_debug_panes(&mut self) {
        self.project_debug_signs();
        let was_open = self.ui.debug_panes.is_some();
        let target = self
            .debugger_session
            .target()
            .filter(|_| self.debugger_session.is_running());
        let Some(target) = target else {
            self.ui.set(None, |ui| &mut ui.debug_panes);
            if was_open {
                self.window_session.set_debugging(false);
                self.sync_terminal_panel();
                // The windows make room for the panes, or take it back.
                self.window_session.invalidate();
                self.reconcile_editor_windows();
            }
            return;
        };
        let selected = self
            .ui
            .debug_panes
            .as_ref()
            .map_or(crate::ui::DebugPane::Sessions, |panes| panes.selected);
        let what = match target.request {
            crate::config::DebugRequestKind::Attach => match &target.target {
                Some(name) => format!("attached to {name}"),
                None => "attached".to_owned(),
            },
            crate::config::DebugRequestKind::Launch => match &target.program {
                Some(program) => format!("launched {}", self.workspace_relative_label(program)),
                None => "launched".to_owned(),
            },
        };
        let adapter = target.adapter.clone();
        let breakpoints = self
            .debugger_session
            .persisted_breakpoints()
            .iter()
            .map(|b| {
                let place = format!("{}:{}", self.workspace_relative_label(&b.path), b.line);
                match &b.condition {
                    Some(condition) => format!("{place} if {condition}"),
                    None => place,
                }
            })
            .collect();
        let threads = self.debugger_session.threads().to_vec();
        let stack = self.debugger_session.stack().to_vec();
        let watches = self
            .debugger_session
            .watches()
            .iter()
            .map(|watch| match self.debugger_session.watch_value(watch) {
                Some(value) => format!("{watch} = {value}"),
                None => watch.clone(),
            })
            .collect();
        let status = match self.debugger_session.stop_reason() {
            Some(reason) => format!("paused: {reason}"),
            None => "running".to_owned(),
        };
        let console = self
            .debugger_session
            .console()
            .map(|entry| entry.text.clone())
            .collect();
        self.ui.debug_panes = Some(crate::ui::DebugPanesUiState {
            selected,
            target: format!("{adapter}: {what}"),
            status,
            threads,
            stack,
            breakpoints,
            variables: self.debugger_session.variables().to_vec(),
            watches,
            console,
        });
        if !was_open {
            self.window_session.set_debugging(true);
            self.sync_terminal_panel();
            // The windows make room for the panes, or take it back.
            self.window_session.invalidate();
            self.reconcile_editor_windows();
        }
    }
}

impl App {

    fn project_debug_signs(&mut self) {
        let mut signs: Vec<crate::ui::DebugSign> = self
            .debugger_session
            .persisted_breakpoints()
            .into_iter()
            .map(|breakpoint| {
                let state = self
                    .debugger_session
                    .breakpoint_state(&breakpoint.path, breakpoint.line);
                let line = match state {
                    Some(crate::debugger::DapBreakpointState::Verified { line }) => line,
                    _ => breakpoint.line,
                };
                let kind = if state == Some(crate::debugger::DapBreakpointState::Disabled) {
                    crate::ui::DebugSignKind::DisabledBreakpoint
                } else if breakpoint.condition.is_some()
                    || breakpoint.hit_condition.is_some()
                    || breakpoint.log_message.is_some()
                {
                    crate::ui::DebugSignKind::ConditionalBreakpoint
                } else {
                    crate::ui::DebugSignKind::Breakpoint
                };
                crate::ui::DebugSign {
                    path: breakpoint.path,
                    line: line.saturating_sub(1) as usize,
                    kind,
                }
            })
            .collect();
        if let Some((path, line)) = self.debugger_session.execution() {
            // First, so it wins over a breakpoint on the same line.
            signs.insert(
                0,
                crate::ui::DebugSign {
                    path: path.clone(),
                    line: line.saturating_sub(1) as usize,
                    kind: crate::ui::DebugSignKind::Execution,
                },
            );
        }
        self.ui.debug_signs = signs;
    }
}
