//! Command registration and execution for debugger launches, breakpoints and views.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerPayload;
use crate::debugger::DapControl;
use crate::debugger::DapSourceBreakpoint;
use crate::ui::PromptKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

use crate::app::TrustedAction;

use crate::app::commands::optional_integer_arg;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;
use std::path::PathBuf;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "dap.breakpoints.states",
        "Show what is true about each breakpoint rather than what was asked for",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerBreakpointStates),
    )?;
    register(
        registry,
        "dap.breakpoint.disable",
        "Enable or disable the breakpoint on the cursor line",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerBreakpointDisable),
    )?;
    register(
        registry,
        "dap.terminate",
        "End the debug session and ask the debuggee to end with it",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerTerminate),
    )?;
    register(
        registry,
        "dap.detach",
        "Leave the debug session without asking the debuggee to stop",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::DebuggerDetach),
    )?;
    register(
        registry,
        "dap.watch.add",
        "Add debug watch",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapWatchAdd(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.watch.remove",
        "Remove debug watch",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapWatchRemove(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.watch.list",
        "List debug watches",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapWatchList,
    )?;
    register(
        registry,
        "dap.configuration.start",
        "Start configured debug session",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapConfigurationStart(
                string_arg(invocation, "name")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.threads",
        "Show debug threads",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapThreads,
    )?;
    register(
        registry,
        "dap.stack",
        "Show debug stack",
        ContextRequirement::Application,
        ArgumentSchema::default().field("thread", ArgumentType::Integer, false),
        |invocation| {
            Ok(AppEffect::DapStack(optional_integer_arg(
                invocation, "thread",
            )))
        },
    )?;
    register(
        registry,
        "dap.scopes",
        "Show debug variables by scope",
        ContextRequirement::Application,
        ArgumentSchema::default().field("frame", ArgumentType::Integer, false),
        |invocation| {
            Ok(AppEffect::DapScopes(optional_integer_arg(
                invocation, "frame",
            )))
        },
    )?;
    register(
        registry,
        "dap.variables",
        "Show debug variables",
        ContextRequirement::Application,
        ArgumentSchema::default().field("reference", ArgumentType::Integer, false),
        |invocation| {
            Ok(AppEffect::DapVariables(optional_integer_arg(
                invocation,
                "reference",
            )))
        },
    )?;
    register(
        registry,
        "dap.evaluate",
        "Evaluate debug expression",
        ContextRequirement::Application,
        ArgumentSchema::default().field("expression", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::DapEvaluate(
                string_arg(invocation, "expression")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "dap.breakpoint.configure",
        "Configure source breakpoint",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("condition", ArgumentType::String, false)
            .field("hit_condition", ArgumentType::String, false)
            .field("log_message", ArgumentType::String, false),
        |invocation| AppEffect::DapBreakpointConfigure {
            condition: optional_string_arg(invocation, "condition").map(str::to_owned),
            hit_condition: optional_string_arg(invocation, "hit_condition").map(str::to_owned),
            log_message: optional_string_arg(invocation, "log_message").map(str::to_owned),
        },
    )?;
    register(
        registry,
        "dap.session.restart",
        "Restart debug session",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::DapRestart,
    )?;
    register(
        registry,
        "dap.breakpoint.toggle",
        "Toggle source breakpoint",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::DapBreakpointToggle,
    )?;
    register(
        registry,
        "dap.session.start",
        "Start debug session",
        ContextRequirement::Application,
        ArgumentSchema::default().field("program", ArgumentType::String, false),
        |invocation| {
            AppEffect::DapStart(optional_string_arg(invocation, "program").map(PathBuf::from))
        },
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_debugger_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::DebuggerBreakpointStates => self.open_breakpoint_states(),
            AppEffect::DebuggerBreakpointDisable => self.toggle_breakpoint_disabled(),
            AppEffect::DebuggerTerminate => self.terminate_debug_session(),
            AppEffect::DebuggerDetach => self.detach_debug_session(),
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
                        // The value from the last stop shows beside it, as in the Watches pane.
                        let value = self
                            .debugger_session
                            .watch_value(&expression)
                            .map_or_else(String::new, |value| format!("= {value}"));
                        (
                            expression.clone(),
                            value,
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
                                // The origin and the source are both shown, so a REPL result is
                                // never read as program output.
                                detail: entry.source.clone(),
                                source: entry.origin.label().to_owned(),
                                action: PickerAction::None,
                            },
                            // Enter shows the whole line, such as a long compiler error.
                            PickerPayload::Notice {
                                title: self.message("dap_console_title", &[]),
                                text: entry.text.clone(),
                            },
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
                if let Err(error) = self
                    .debugger_session
                    .control(DapControl::Scopes { frame_id })
                {
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
                match self.debugger_session.request_restart(
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
            AppEffect::DapBreakpointConfigure {
                condition: None,
                hit_condition: None,
                log_message: None,
            } => {
                let Some((path, line)) = self.cursor_breakpoint_place() else {
                    return Ok(());
                };
                let row = self
                    .debugger_session
                    .breakpoint_at(&path, line)
                    .cloned()
                    .unwrap_or(DapSourceBreakpoint {
                        path,
                        line,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    });
                self.open_prompt(
                    PromptKind::BreakpointCondition,
                    row.condition.clone().unwrap_or_default(),
                );
                self.input.set_pending_breakpoint(row);
            }
            AppEffect::DapBreakpointConfigure {
                condition,
                hit_condition,
                log_message,
            } => self.configure_breakpoint(condition, hit_condition, log_message),
            AppEffect::DapBreakpointToggle => {
                let Some(view) = self.state.active_view() else {
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
                let attached =
                    self.debugger_session
                        .target()
                        .and_then(|target| match target.request {
                            crate::config::DebugRequestKind::Attach => target.target.clone(),
                            crate::config::DebugRequestKind::Launch => None,
                        });
                match attached {
                    Some(target) => self.notify_message(
                        self.message(
                            "dap_status_attached",
                            &[("adapter", adapter), ("target", target)],
                        ),
                        false,
                    ),
                    None => self
                        .notify_message(self.message("dap_status", &[("adapter", adapter)]), false),
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
