use super::{
    AppEffect, ArgumentSchema, ArgumentType, CommandError, CommandRegistry, ContextRequirement,
    DiagnosticFilter, register, string_arg,
};

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, category) in [
        ("tools.menu.open", "Tool Manager: install language servers (Mason)", "all"),
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
        ("tools.search", "Search the tool manager", AppEffect::ToolSearch),
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
            AppEffect::LspRequest(crate::language::lsp::LspFeature::Formatting),
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
        |invocation| Ok(AppEffect::ToolSearchInsert(string_arg(invocation, "text")?.to_owned())),
    )?;
    for (id, title, effect) in [
        (
            "diagnostic.next",
            "Go to next error or warning",
            AppEffect::DiagnosticNavigate {
                direction: 1,
                filter: DiagnosticFilter::ErrorOrWarning,
            },
        ),
        (
            "diagnostic.previous",
            "Go to previous error or warning",
            AppEffect::DiagnosticNavigate {
                direction: -1,
                filter: DiagnosticFilter::ErrorOrWarning,
            },
        ),
        (
            "diagnostic.error.next",
            "Go to next error",
            AppEffect::DiagnosticNavigate {
                direction: 1,
                filter: DiagnosticFilter::Error,
            },
        ),
        (
            "diagnostic.error.previous",
            "Go to previous error",
            AppEffect::DiagnosticNavigate {
                direction: -1,
                filter: DiagnosticFilter::Error,
            },
        ),
        (
            "diagnostic.warning.next",
            "Go to next warning",
            AppEffect::DiagnosticNavigate {
                direction: 1,
                filter: DiagnosticFilter::Warning,
            },
        ),
        (
            "diagnostic.warning.previous",
            "Go to previous warning",
            AppEffect::DiagnosticNavigate {
                direction: -1,
                filter: DiagnosticFilter::Warning,
            },
        ),
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
    Ok(())
}
