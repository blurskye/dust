//! Command registration and execution for language-server requests and editor results.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::language::lsp::LspFeature;
use crate::workspace::DiagnosticOwner;

use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    crate::app::language::incoming::register_commands(registry)?;
    for (id, title, category) in [
        (
            "tools.menu.open",
            "Tool Manager: install language servers (Mason)",
            "all",
        ),
        ("lsp.menu.open", "Open language-server manager", "lsp"),
        ("dap.menu.open", "Open debug-adapter manager", "dap"),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenToolMenu(category.into()),
        )?;
    }
    for (id, title, effect) in [
        (
            "tools.menu.close",
            "Close tool manager",
            AppEffect::CloseToolMenu,
        ),
        (
            "tools.menu.next",
            "Select next tool",
            AppEffect::MoveToolMenu(1),
        ),
        (
            "tools.search",
            "Search the tool manager",
            AppEffect::ToolSearch,
        ),
        (
            "tools.search.delete",
            "Delete the last character of the tool search",
            AppEffect::ToolSearchDelete,
        ),
        (
            "tools.search.done",
            "Stop typing and keep the tools filtered",
            AppEffect::ToolSearchStop { keep: true },
        ),
        (
            "tools.search.cancel",
            "Clear the tool search",
            AppEffect::ToolSearchStop { keep: false },
        ),
        (
            "tools.menu.previous",
            "Select previous tool",
            AppEffect::MoveToolMenu(-1),
        ),
        (
            "tools.category.next",
            "Select next tool category",
            AppEffect::CycleToolCategory(1),
        ),
        (
            "tools.category.previous",
            "Select previous tool category",
            AppEffect::CycleToolCategory(-1),
        ),
        (
            "tools.refresh",
            "Refresh installed tools",
            AppEffect::RefreshTools,
        ),
        (
            "tools.activate",
            "Start selected tool or accept selected runtime",
            AppEffect::ActivateTool,
        ),
        (
            "tools.install",
            "Install selected managed runtime",
            AppEffect::InstallSelectedTool,
        ),
        (
            "tools.uninstall",
            "Uninstall selected Dust-owned runtime",
            AppEffect::UninstallSelectedTool,
        ),
        (
            "tools.lsp.stop",
            "Stop all sessions for selected language server",
            AppEffect::StopSelectedLsp,
        ),
        (
            "tools.lsp.restart",
            "Restart selected language server",
            AppEffect::RestartSelectedLsp,
        ),
        (
            "tools.runtime.toggle",
            "Choose a language-server build and installation",
            AppEffect::ToggleToolRuntime,
        ),
        (
            "lsp.server.start",
            "Start language server",
            AppEffect::LspStart,
        ),
        (
            "lsp.server.stop",
            "Stop language server",
            AppEffect::LspStop,
        ),
        (
            "lsp.server.restart",
            "Restart language server",
            AppEffect::LspRestart,
        ),
        (
            "lsp.server.status",
            "Show language-server status",
            AppEffect::LspStatus,
        ),
        (
            "lsp.server.logs",
            "Show language-server logs",
            AppEffect::LspLogs,
        ),
        (
            "lsp.buffer.attach",
            "Attach active buffer",
            AppEffect::LspAttach,
        ),
        (
            "lsp.buffer.detach",
            "Detach active buffer",
            AppEffect::LspDetach,
        ),
        (
            "lsp.completion.trigger",
            "Trigger language completion",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Completion),
        ),
        (
            "lsp.hover",
            "Show language hover",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Hover),
        ),
        (
            "lsp.signature_help",
            "Show signature help",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::SignatureHelp),
        ),
        (
            "lsp.documentation.scroll_up",
            "Scroll documentation up",
            AppEffect::LspDocumentationScroll(-1),
        ),
        (
            "lsp.documentation.scroll_down",
            "Scroll documentation down",
            AppEffect::LspDocumentationScroll(1),
        ),
        (
            "lsp.documentation.close",
            "Close documentation",
            AppEffect::LspDocumentationClose,
        ),
        (
            "lsp.completion.documentation.open",
            "Open completion documentation",
            AppEffect::LspCompletionDocumentationOpen,
        ),
        (
            "lsp.definition",
            "Go to definition",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Definition),
        ),
        (
            "lsp.declaration",
            "Go to declaration",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Declaration),
        ),
        (
            "lsp.type_definition",
            "Go to type definition",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::TypeDefinition),
        ),
        (
            "lsp.implementation",
            "Go to implementation",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Implementation),
        ),
        (
            "lsp.references",
            "Find references",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::References),
        ),
        (
            "lsp.format",
            "Format file",
            // The application fills in the active buffer's indentation settings.
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Formatting {
                tab_size: 0,
                insert_spaces: false,
            }),
        ),
        (
            "lsp.code_action",
            "Show language code actions",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::CodeAction {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
                diagnostics: Vec::new(),
            }),
        ),
        (
            "lsp.document_symbols",
            "Show document symbols (breadcrumbs)",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::DocumentSymbols),
        ),
        (
            "lsp.pull_diagnostics",
            "Pull document diagnostics",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::PullDiagnostics {
                previous_result_id: None,
            }),
        ),
        (
            "lsp.semantic_tokens",
            "Refresh semantic tokens",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::SemanticTokensFull),
        ),
        (
            "lsp.inlay_hints.toggle",
            "Toggle inline inlay hints",
            AppEffect::LspInlayHintsToggle,
        ),
        (
            "lsp.inlay_hints",
            "Refresh inline inlay hints",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::InlayHints {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            }),
        ),
        (
            "lsp.code_lens",
            "Refresh code lens",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::CodeLens),
        ),
        (
            "lsp.document_highlights",
            "Refresh document highlights",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::DocumentHighlights),
        ),
        (
            "lsp.folding_ranges",
            "Refresh folding ranges",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::FoldingRanges),
        ),
        (
            "lsp.selection_ranges",
            "Refresh selection ranges",
            AppEffect::LspRequest(crate::language::lsp::LspFeature::SelectionRanges),
        ),
        ("dap.session.stop", "Stop debug session", AppEffect::DapStop),
        (
            "dap.session.continue",
            "Continue debug session",
            AppEffect::DapContinue,
        ),
        (
            "dap.session.pause",
            "Pause debug session",
            AppEffect::DapPause,
        ),
        ("dap.session.next", "Step over", AppEffect::DapNext),
        ("dap.session.step_in", "Step into", AppEffect::DapStepIn),
        ("dap.session.step_out", "Step out", AppEffect::DapStepOut),
        ("dap.console", "Show debug console", AppEffect::DapConsole),
        (
            "dap.sessions",
            "Show debug sessions",
            AppEffect::DapSessions,
        ),
        (
            "dap.session.status",
            "Show debug-session status",
            AppEffect::DapStatus,
        ),
        (
            "dap.inspector.next",
            "Show the next debug inspector",
            AppEffect::DapInspectorNext,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "tools.search.insert",
        "Type into the tool search",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ToolSearchInsert(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    for (id, title, effect) in [
        (
            "completion.select_next",
            "Select next completion",
            AppEffect::LspCompletionMove(1),
        ),
        (
            "completion.select_previous",
            "Select previous completion",
            AppEffect::LspCompletionMove(-1),
        ),
        (
            "completion.accept",
            "Accept completion",
            AppEffect::LspCompletionAccept,
        ),
        (
            "completion.inline.accept",
            "Accept inline completion",
            AppEffect::LspCompletionInlineAccept,
        ),
        (
            "completion.cancel",
            "Cancel completion",
            AppEffect::LspCompletionCancel,
        ),
        (
            "completion.leave",
            "Close completion and leave Insert",
            AppEffect::LspCompletionLeave,
        ),
        (
            "lsp.rename.prompt",
            "Rename symbol",
            AppEffect::LspRenamePrompt,
        ),
        (
            "snippet.next",
            "Next snippet placeholder",
            AppEffect::SnippetNavigate(1),
        ),
        (
            "snippet.leave",
            "Finish the snippet and leave Insert",
            AppEffect::SnippetLeave,
        ),
        (
            "snippet.previous",
            "Previous snippet placeholder",
            AppEffect::SnippetNavigate(-1),
        ),
        (
            "snippet.cancel",
            "Cancel snippet session",
            AppEffect::SnippetCancel,
        ),
        (
            "lsp.fold.toggle",
            "Toggle language fold",
            AppEffect::LspFoldToggle,
        ),
        (
            "lsp.selection.expand",
            "Expand language selection",
            AppEffect::LspSelectionExpand,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register(
        registry,
        "lsp.rename",
        "Rename symbol",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LspRequest(
                crate::language::lsp::LspFeature::Rename {
                    new_name: string_arg(invocation, "name")?.into(),
                },
            ))
        },
    )?;
    register(
        registry,
        "lsp.workspace_symbols",
        "Show workspace symbols",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LspRequest(
                crate::language::lsp::LspFeature::WorkspaceSymbols {
                    query: string_arg(invocation, "query")?.into(),
                },
            ))
        },
    )?;
    register(
        registry,
        "lsp.sessions",
        "List language-server sessions",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::LspSessions,
    )?;
    register(
        registry,
        "lsp.session.select",
        "Select language-server session",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LspSessionSelect(
                string_arg(invocation, "key")?.to_owned(),
            ))
        },
    )?;
    Ok(())
}

impl App {
    pub(in crate::app) fn process_language_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::LspStart => {
                self.language.clear_all_stopped();
                if let Some(key) = self.language.active().cloned() {
                    self.language.request_start(&key, None);
                }
                self.start_lsp(None);
            }
            AppEffect::LspStop => {
                if let Some(buffer) = self.state.active_buffer_id() {
                    self.language.stop_document_launches(buffer);
                }
                if let Some(server) = self.language.active().cloned() {
                    if let Some(id) = self
                        .language
                        .sessions()
                        .get(&server)
                        .map(|state| state.server_id.clone())
                    {
                        self.language.mark_stopped(&id);
                    }
                    self.language.request_stop(&server);
                    self.clear_lsp_server_ownership(&server);
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
                    self.restart_lsp(&server);
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
                if let Some(buffer) = self.state.active_buffer_id() {
                    self.language.clear_detached(buffer);
                }
                if let Some(server) = self.language.active().cloned() {
                    if let Some((buffer, document)) = self.active_lsp_document() {
                        if let Some(id) = self
                            .language
                            .sessions()
                            .get(&server)
                            .map(|session| session.server_id.clone())
                        {
                            self.language.set_detached(id, buffer, false);
                        }
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
                    if let Some(id) = self
                        .language
                        .sessions()
                        .get(&server)
                        .map(|session| session.server_id.clone())
                    {
                        self.language.set_detached(id, buffer, true);
                    }
                    self.language.detach(&server, buffer);
                    for name in [format!("lsp.{server}"), format!("lsp-pull:{server}")] {
                        if let Ok(owner) = DiagnosticOwner::new(name) {
                            self.diagnostics.clear_owner_uri(&owner, &uri);
                        }
                    }
                    self.clear_lsp_ui_projection();
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
            AppEffect::LspRequest(feature) => {
                if matches!(feature, LspFeature::Completion) {
                    self.mark_lsp_completion_start(false);
                }
                self.request_lsp(feature);
            }
            AppEffect::LspCompletionDocumentationOpen => {
                self.open_lsp_completion_documentation(true)
            }
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspCompletionInlineAccept => {
                if self.language.inline_completion(&self.state).is_some() {
                    self.accept_lsp_completion();
                }
            }
            AppEffect::LspIncoming(id) => self.handle_incoming_lsp_command(id),
            AppEffect::LspRenamePrompt => self.open_rename_prompt(),
            AppEffect::LspCompletionLeave => {
                self.cancel_lsp_completion();
                self.invoke_mapped_command("input.cancel");
            }
            AppEffect::LspCompletionCancel => self.cancel_lsp_completion(),
            AppEffect::SnippetNavigate(direction) => self.navigate_snippet(direction),
            AppEffect::SnippetLeave => {
                self.input.clear_snippet();
                self.invoke_mapped_command("input.cancel");
            }
            AppEffect::SnippetCancel => {
                self.input.clear_snippet();
                if let Some(view) = self.state.active_view() {
                    self.state.views[view].selection = None;
                    self.state.mark_editor_changed();
                }
            }
            AppEffect::LspFoldToggle => {
                if let Some(view) = self.state.active_view() {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    if let Some(fold) = self
                        .ui
                        .lsp_folds
                        .iter_mut()
                        .filter(|fold| {
                            fold.view == view
                                && line.is_some_and(|line| {
                                    line >= fold.start_line && line <= fold.end_line
                                })
                        })
                        .min_by_key(|fold| fold.end_line.saturating_sub(fold.start_line))
                    {
                        fold.collapsed = !fold.collapsed;
                        if fold.collapsed && line.is_some_and(|line| line > fold.start_line) {
                            let buffer = self.state.views[view].buffer;
                            if let Ok(cursor) = self.state.buffers[buffer]
                                .text
                                .line_to_byte(fold.start_line)
                            {
                                self.state.views[view].cursor = cursor;
                                self.state.views[view].selection = None;
                                self.state.mark_editor_changed();
                            }
                        }
                    }
                }
            }
            AppEffect::LspSelectionExpand => {
                if self.active_lsp_document_without_text().is_some() {
                    self.refresh_lsp_projection_owner();
                }
                if let Some(view) = self.state.active_view() {
                    let state = &self.state.views[view];
                    let text = &self.state.buffers[state.buffer].text;
                    let selection = state.selection.map(|selection| {
                        let start = selection.anchor.min(selection.head);
                        let end = crate::editor::text::next_grapheme_boundary(
                            text,
                            selection.anchor.max(selection.head),
                        );
                        (start, end)
                    });
                    let range = self
                        .language
                        .selection_ranges()
                        .iter()
                        .filter(|_| {
                            selection.is_some()
                                || self.language.selection_ranges_current(state.cursor)
                        })
                        .filter(|range| {
                            if let Some((start, end)) = selection {
                                range.start <= start
                                    && range.end >= end
                                    && (range.start < start || range.end > end)
                            } else {
                                range.contains(state.cursor)
                            }
                        })
                        .min_by_key(|range| range.len())
                        .copied();
                    if let Some(range) = range {
                        self.select_text(view, range.start.get(), range.end.get());
                    } else if selection.is_none() || self.language.selection_ranges().is_empty() {
                        self.request_lsp(LspFeature::SelectionRanges);
                        self.language.expand_requested_selection();
                    }
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
