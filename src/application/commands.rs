use std::path::PathBuf;

use crate::editing::{
    ArgumentSchema, ArgumentType, CommandError, CommandOutcome, CommandRegistry, CommandSpec,
    CommandValue, ContextRequirement, FocusDirection,
};
use crate::{input::GrammarInput, terminal_ui::PromptKind};

#[derive(Clone, Debug)]
pub enum AppEffect {
    Quit {
        force: bool,
    },
    Save {
        quit_after: bool,
        path: Option<PathBuf>,
        force: bool,
    },
    OpenPath(PathBuf),
    Reload {
        force: bool,
    },
    CloseBuffer {
        force: bool,
    },
    CycleBuffer(isize),
    ToggleBufferPin,
    MoveBufferLine(isize),
    NewBuffer,
    OpenFilePrompt,
    OpenWorkspaceFilePicker,
    GitStatusRefresh,
    GitStatusOpen,
    GitHunksOpen,
    GitHunkStage,
    GitHunkReset,
    OpenPrompt {
        kind: PromptKind,
        initial: String,
    },
    PromptInsert(String),
    PromptBackspace,
    PromptMove(PromptMove),
    PromptHistory(i8),
    PromptComplete(i8),
    PromptSubmit,
    PromptCancel,
    SearchRepeat {
        reverse: bool,
    },
    SearchWord {
        forward: bool,
    },
    Scroll {
        kind: String,
        count: u64,
        at_mouse: bool,
    },
    PlaceMouseCursor,
    ExtendMouseSelection,
    Layout(LayoutAction),
    SurfaceFocus(FocusDirection),
    RetainedUiFocus(isize),
    RetainedUiScroll(isize),
    RetainedUiActivate,
    RetainedUiMouseSelect,
    PrivateUiActivate {
        at_mouse: bool,
    },
    WindowDock {
        target: String,
        edge: String,
        size: u16,
    },
    WindowFloat {
        target: String,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    WindowFullscreen(String),
    WindowHide(String),
    WindowShow(String),
    WindowClose(String),
    WindowResize {
        target: String,
        amount: i16,
    },
    WindowResizeDirectional(FocusDirection),
    WindowMove {
        target: String,
        x: u16,
        y: u16,
    },
    WindowSwap {
        first: String,
        second: String,
    },
    RepeatLast,
    MacroRecordStart(char),
    MacroRecordStop,
    MacroReplay {
        register: char,
        count: u64,
    },
    Grammar(GrammarInput),
    CancelPendingMapping,
    ReloadConfiguration,
    ExplainConfig(String),
    InspectKeymap {
        id: Option<String>,
        context: Option<String>,
        key: Option<String>,
        command: Option<String>,
    },
    ShowKeymapConflicts,
    RecoveryInspect,
    RecoveryRecoverLatest,
    RecoveryDiscardLatest,
    SessionSave,
    SessionLoad,
    HistoryClear,
    LargeFileSet(String),
    LargeFileStatus,
    SyntaxStatus,
    SyntaxFoldToggle,
    SyntaxIndentLine,
    TaskRun(String),
    TaskStop(String),
    TaskOutput(String),
    PluginValidate,
    PluginList,
    PluginDisable(String),
    PluginExecute {
        plugin: String,
        command: String,
    },
    PluginPickerOpen {
        provider: String,
        query: String,
    },
    VirtualDocumentOpen {
        source: String,
        key: String,
    },
    VirtualDocumentRefresh,
    HealthVirtualOpen,
    HealthVirtualRefresh,
    AiChatOpen,
    AiGhostRequest,
    AiComposerRequest,
    PackageInstall {
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    },
    PackageRegistrySearch(String),
    PackageRegistryShow {
        publisher: String,
        name: String,
        version: String,
    },
    PackageRegistryInstall {
        publisher: String,
        name: String,
        version: String,
    },
    PackageList,
    PackageVerify,
    PackageRollback,
    PackageRemove(String),
    PackageGarbageCollect,
    CrashInspect,
    CrashClear,
    TerminalOpen(Option<String>),
    TerminalToggle,
    TerminalList,
    TerminalFocus(u64),
    TerminalFocusActive,
    TerminalCycle(i8),
    TerminalResize(i16),
    TerminalDock {
        edge: String,
        size: u16,
    },
    TerminalFloat {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    TerminalFullscreen,
    TerminalHide,
    TerminalSend(String),
    TerminalModeNormal,
    TerminalModeInput,
    TerminalNormalMove(String),
    TerminalVisualStart(bool),
    TerminalYank {
        line: bool,
    },
    TerminalUnfocus,
    TerminalClose,
    FormatRun(String),
    LintRun(String),
    LintClear(String),
    TestRun(String),
    TestStop(String),
    TestList(String),
    TaskList,
    TaskRestart(String),
    ShowHealth,
    SafeModeStatus,
    MarkSet(String),
    WorkspaceTrust,
    WorkspaceTrustConfirm(String),
    WorkspaceUntrust,
    WorkspaceStatus,
    WorkspaceRootAdd(PathBuf),
    WorkspaceRootActivate(u64),
    WorkspaceRootRemove(u64),
    WorkspaceRootStatus,
    ExplorerOpen,
    ExplorerClose,
    ExplorerMove(i8),
    ExplorerPage(isize),
    ExplorerParent,
    ExplorerChild,
    ExplorerToggle,
    ExplorerRefresh,
    ExplorerFollow,
    ExplorerToggleHidden,
    ExplorerToggleIgnored,
    ExplorerFileOpen {
        split: bool,
    },
    ExplorerMouseSelect,
    WorkspaceSearch(String),
    WorkspaceSearchCancel,
    WorkspaceReplace {
        query: String,
        replacement: String,
    },
    OpenPicker(PickerKind),
    PickerInsert(String),
    PickerBackspace,
    PickerMove(i8),
    PickerAccept,
    PickerCancel,
    WorkspaceEditToggle,
    WorkspaceEditAccept,
    WorkspaceEditReject,
    WorkspaceResourceApply,
    OpenToolMenu(String),
    CloseToolMenu,
    MoveToolMenu(i8),
    CycleToolCategory(i8),
    RefreshTools,
    ActivateTool,
    LspStart,
    LspStop,
    LspRestart,
    LspStatus,
    LspLogs,
    LspSessions,
    LspSessionSelect(String),
    LspAttach,
    LspDetach,
    LspRequest(crate::languages::LspFeature),
    LspDocumentationScroll(isize),
    LspDocumentationClose,
    LspCompletionDocumentationOpen,
    LspCompletionMove(i8),
    LspCompletionAccept,
    LspCompletionCancel,
    SnippetNavigate(i8),
    SnippetCancel,
    LspFoldToggle,
    LspSelectionExpand,
    DiagnosticNavigate(i8),
    DapStart(Option<PathBuf>),
    DapStop,
    DapContinue,
    DapPause,
    DapNext,
    DapStepIn,
    DapStepOut,
    DapStatus,
    DapConsole,
    DapSessions,
    DapThreads,
    DapStack(i64),
    DapScopes(i64),
    DapVariables(i64),
    DapEvaluate(String),
    DapBreakpointToggle,
    DapBreakpointConfigure {
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
    },
    DapRestart,
    DapConfigurationStart(String),
    DapWatchAdd(String),
    DapWatchRemove(String),
    DapWatchList,
}

#[derive(Clone, Copy, Debug)]
pub enum PickerKind {
    Diagnostics,
    Buffers,
    Commands,
    Roots,
    Recents,
    Marks,
    Jumps,
    Changes,
}

#[derive(Clone, Copy, Debug)]
pub enum LayoutAction {
    Split(crate::editing::Axis),
    ClosePane,
    ResizePane(i8),
    FocusPane(isize),
    NewTab,
    CloseTab,
    FocusTab(isize),
}

#[derive(Clone, Copy, Debug)]
pub enum PromptMove {
    Left,
    Right,
    Home,
    End,
}

pub fn register_app_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "editor.quit",
        "Quit Dust",
        ContextRequirement::Application,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Quit {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    register(
        registry,
        "file.save",
        "Save file",
        ContextRequirement::Buffer,
        ArgumentSchema::default()
            .field("path", ArgumentType::String, false)
            .field("force", ArgumentType::Bool, false),
        |invocation| AppEffect::Save {
            quit_after: false,
            path: optional_string_arg(invocation, "path").map(PathBuf::from),
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    register(
        registry,
        "file.save_quit",
        "Save and quit",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| AppEffect::Save {
            quit_after: true,
            path: None,
            force: optional_bool_arg(invocation, "force").unwrap_or(false),
        },
    )?;
    register(
        registry,
        "file.open",
        "Open file",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::OpenPath(PathBuf::from(string_arg(
                invocation, "path",
            )?)))
        },
    )?;
    register(
        registry,
        "file.reload",
        "Reload file",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Reload {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    register(
        registry,
        "buffer.close",
        "Close buffer",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("force", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::CloseBuffer {
                force: bool_arg(invocation, "force")?,
            })
        },
    )?;
    for (id, title, offset) in [
        ("buffer.next", "Show next buffer", 1),
        ("buffer.previous", "Show previous buffer", -1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::CycleBuffer(offset),
        )?;
    }
    register(
        registry,
        "buffer.pin.toggle",
        "Toggle active buffer pin",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::ToggleBufferPin,
    )?;
    for (id, title, direction) in [
        ("buffer.move_left", "Move buffer left", -1),
        ("buffer.move_right", "Move buffer right", 1),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Buffer,
            ArgumentSchema::default(),
            move |_| AppEffect::MoveBufferLine(direction),
        )?;
    }
    register(
        registry,
        "file.new",
        "New buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::NewBuffer,
    )?;
    for (id, title, category) in [
        ("tools.menu.open", "Open tool manager", "all"),
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
            "Install or start selected tool",
            AppEffect::ActivateTool,
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
            AppEffect::LspRequest(crate::languages::LspFeature::Completion),
        ),
        (
            "lsp.hover",
            "Show language hover",
            AppEffect::LspRequest(crate::languages::LspFeature::Hover),
        ),
        (
            "lsp.signature_help",
            "Show signature help",
            AppEffect::LspRequest(crate::languages::LspFeature::SignatureHelp),
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
            AppEffect::LspRequest(crate::languages::LspFeature::Definition),
        ),
        (
            "lsp.declaration",
            "Go to declaration",
            AppEffect::LspRequest(crate::languages::LspFeature::Declaration),
        ),
        (
            "lsp.type_definition",
            "Go to type definition",
            AppEffect::LspRequest(crate::languages::LspFeature::TypeDefinition),
        ),
        (
            "lsp.implementation",
            "Go to implementation",
            AppEffect::LspRequest(crate::languages::LspFeature::Implementation),
        ),
        (
            "lsp.references",
            "Find references",
            AppEffect::LspRequest(crate::languages::LspFeature::References),
        ),
        (
            "lsp.format",
            "Format with language server",
            AppEffect::LspRequest(crate::languages::LspFeature::Formatting),
        ),
        (
            "lsp.code_action",
            "Show language code actions",
            AppEffect::LspRequest(crate::languages::LspFeature::CodeAction {
                range: crate::languages::LspRange {
                    start: crate::languages::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::languages::LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            }),
        ),
        (
            "lsp.document_symbols",
            "Show document symbols",
            AppEffect::LspRequest(crate::languages::LspFeature::DocumentSymbols),
        ),
        (
            "lsp.pull_diagnostics",
            "Pull document diagnostics",
            AppEffect::LspRequest(crate::languages::LspFeature::PullDiagnostics {
                previous_result_id: None,
            }),
        ),
        (
            "lsp.semantic_tokens",
            "Refresh semantic tokens",
            AppEffect::LspRequest(crate::languages::LspFeature::SemanticTokensFull),
        ),
        (
            "lsp.code_lens",
            "Refresh code lens",
            AppEffect::LspRequest(crate::languages::LspFeature::CodeLens),
        ),
        (
            "lsp.document_highlights",
            "Refresh document highlights",
            AppEffect::LspRequest(crate::languages::LspFeature::DocumentHighlights),
        ),
        (
            "lsp.folding_ranges",
            "Refresh folding ranges",
            AppEffect::LspRequest(crate::languages::LspFeature::FoldingRanges),
        ),
        (
            "lsp.selection_ranges",
            "Refresh selection ranges",
            AppEffect::LspRequest(crate::languages::LspFeature::SelectionRanges),
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
    for (id, title, effect) in [
        (
            "diagnostic.next",
            "Go to next diagnostic",
            AppEffect::DiagnosticNavigate(1),
        ),
        (
            "diagnostic.previous",
            "Go to previous diagnostic",
            AppEffect::DiagnosticNavigate(-1),
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
            "snippet.next",
            "Next snippet placeholder",
            AppEffect::SnippetNavigate(1),
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
                crate::languages::LspFeature::Rename {
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
                crate::languages::LspFeature::WorkspaceSymbols {
                    query: string_arg(invocation, "query")?.into(),
                },
            ))
        },
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
        ArgumentSchema::default().field("thread", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::DapStack(integer_arg(invocation, "thread")?)),
    )?;
    register(
        registry,
        "dap.scopes",
        "Show debug scopes",
        ContextRequirement::Application,
        ArgumentSchema::default().field("frame", ArgumentType::Integer, true),
        |invocation| Ok(AppEffect::DapScopes(integer_arg(invocation, "frame")?)),
    )?;
    register(
        registry,
        "dap.variables",
        "Show debug variables",
        ContextRequirement::Application,
        ArgumentSchema::default().field("reference", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::DapVariables(integer_arg(
                invocation,
                "reference",
            )?))
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
        "Restart configured debug session",
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
    register(
        registry,
        "workspace.trust.confirm",
        "Confirm workspace trust",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("phrase", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceTrustConfirm(
                string_arg(invocation, "phrase")?.into(),
            ))
        },
    )?;
    for (id, title, effect) in [
        (
            "workspace.trust",
            "Trust workspace",
            AppEffect::WorkspaceTrust,
        ),
        (
            "workspace.untrust",
            "Revoke workspace trust",
            AppEffect::WorkspaceUntrust,
        ),
        (
            "workspace.trust.status",
            "Show workspace trust",
            AppEffect::WorkspaceStatus,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    for (id, title, effect) in [
        (
            "explorer.open",
            "Open file explorer",
            AppEffect::ExplorerOpen,
        ),
        (
            "explorer.close",
            "Close file explorer",
            AppEffect::ExplorerClose,
        ),
        (
            "explorer.next",
            "Select next explorer row",
            AppEffect::ExplorerMove(1),
        ),
        (
            "explorer.previous",
            "Select previous explorer row",
            AppEffect::ExplorerMove(-1),
        ),
        (
            "explorer.page_up",
            "Page explorer selection up",
            AppEffect::ExplorerPage(-1),
        ),
        (
            "explorer.page_down",
            "Page explorer selection down",
            AppEffect::ExplorerPage(1),
        ),
        (
            "explorer.parent",
            "Select explorer parent",
            AppEffect::ExplorerParent,
        ),
        (
            "explorer.child",
            "Select first explorer child",
            AppEffect::ExplorerChild,
        ),
        (
            "explorer.toggle",
            "Expand or collapse explorer node",
            AppEffect::ExplorerToggle,
        ),
        (
            "explorer.refresh",
            "Refresh explorer",
            AppEffect::ExplorerRefresh,
        ),
        (
            "explorer.follow",
            "Follow active file in explorer",
            AppEffect::ExplorerFollow,
        ),
        (
            "explorer.hidden.toggle",
            "Toggle hidden explorer entries",
            AppEffect::ExplorerToggleHidden,
        ),
        (
            "explorer.ignored.toggle",
            "Toggle ignored explorer entries",
            AppEffect::ExplorerToggleIgnored,
        ),
        (
            "explorer.file.open",
            "Open explorer file",
            AppEffect::ExplorerFileOpen { split: false },
        ),
        (
            "explorer.file.split_open",
            "Split-open explorer file",
            AppEffect::ExplorerFileOpen { split: true },
        ),
        (
            "explorer.mouse.select",
            "Select explorer row at mouse",
            AppEffect::ExplorerMouseSelect,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Workspace,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
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
    register(
        registry,
        "session.save",
        "Save session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionSave,
    )?;
    register(
        registry,
        "session.load",
        "Load session state",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::SessionLoad,
    )?;
    register(
        registry,
        "history.clear",
        "Clear command and search history",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HistoryClear,
    )?;
    register(
        registry,
        "buffer.large_file.set",
        "Set large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("mode", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LargeFileSet(
                string_arg(invocation, "mode")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "buffer.large_file.status",
        "Show large-file mode",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::LargeFileStatus,
    )?;
    register(
        registry,
        "task.run",
        "Run configured task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.stop",
        "Stop running task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "task.output",
        "Show task output",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskOutput(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "crash.inspect",
        "Inspect previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashInspect,
    )?;
    register(
        registry,
        "crash.clear",
        "Clear previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashClear,
    )?;
    register(
        registry,
        "plugin.validate",
        "Validate configured plugins",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginValidate,
    )?;
    register(
        registry,
        "plugin.list",
        "List plugin status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PluginList,
    )?;
    register(
        registry,
        "picker.plugin.open",
        "Open a plugin picker source",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("provider", ArgumentType::String, true)
            .field("query", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::PluginPickerOpen {
                provider: string_arg(invocation, "provider")?.into(),
                query: invocation
                    .arguments
                    .get("query")
                    .map(CommandValue::as_str)
                    .transpose()?
                    .unwrap_or("")
                    .into(),
            })
        },
    )?;
    register(
        registry,
        "buffer.virtual.open",
        "Open a read-only plugin virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("source", ArgumentType::String, true)
            .field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::VirtualDocumentOpen {
                source: string_arg(invocation, "source")?.into(),
                key: string_arg(invocation, "key")?.into(),
            })
        },
    )?;
    register(
        registry,
        "health.virtual.open",
        "Open read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualOpen,
    )?;
    register(
        registry,
        "health.virtual.refresh",
        "Refresh read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualRefresh,
    )?;
    for (id, title, effect) in [
        (
            "ai.chat.open",
            "Open fake-provider AI chat",
            AppEffect::AiChatOpen,
        ),
        (
            "ai.ghost.request",
            "Request fake-provider ghost text",
            AppEffect::AiGhostRequest,
        ),
        (
            "ai.composer.request",
            "Request fake-provider edit proposal",
            AppEffect::AiComposerRequest,
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
        "buffer.virtual.refresh",
        "Refresh focused read-only virtual document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::VirtualDocumentRefresh,
    )?;
    for (id, title, update) in [
        ("package.install", "Install verified package", false),
        ("package.update", "Update verified package", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("package_manifest", ArgumentType::String, true)
                .field("plugin_manifest", ArgumentType::String, true)
                .field("component", ArgumentType::String, true),
            move |invocation| {
                Ok(AppEffect::PackageInstall {
                    package_manifest: PathBuf::from(string_arg(invocation, "package_manifest")?),
                    plugin_manifest: PathBuf::from(string_arg(invocation, "plugin_manifest")?),
                    component: PathBuf::from(string_arg(invocation, "component")?),
                    update,
                })
            },
        )?;
    }
    register(
        registry,
        "package.registry.search",
        "Search official package registry",
        ContextRequirement::Application,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRegistrySearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    for (id, title, install) in [
        (
            "package.registry.show",
            "Show exact registry package",
            false,
        ),
        (
            "package.registry.install",
            "Install exact registry package",
            true,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("publisher", ArgumentType::String, true)
                .field("name", ArgumentType::String, true)
                .field("version", ArgumentType::String, true),
            move |invocation| {
                let publisher = string_arg(invocation, "publisher")?.into();
                let name = string_arg(invocation, "name")?.into();
                let version = string_arg(invocation, "version")?.into();
                Ok(if install {
                    AppEffect::PackageRegistryInstall {
                        publisher,
                        name,
                        version,
                    }
                } else {
                    AppEffect::PackageRegistryShow {
                        publisher,
                        name,
                        version,
                    }
                })
            },
        )?;
    }
    register(
        registry,
        "package.list",
        "List installed packages",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageList,
    )?;
    register(
        registry,
        "package.verify",
        "Verify package store",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageVerify,
    )?;
    register(
        registry,
        "package.rollback",
        "Rollback package lock",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageRollback,
    )?;
    register(
        registry,
        "package.remove",
        "Remove installed package",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRemove(
                string_arg(invocation, "name")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "package.gc",
        "Collect unreferenced package objects",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageGarbageCollect,
    )?;
    register(
        registry,
        "plugin.disable",
        "Disable plugin",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PluginDisable(
                string_arg(invocation, "id")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "window.dock",
        "Dock an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("edge", ArgumentType::String, true)
            .field("size", ArgumentType::Integer, true),
        |invocation| {
            let size = u16::try_from(integer_arg(invocation, "size")?).map_err(|_| {
                CommandError::InvalidArguments("window dock size is outside u16 bounds".into())
            })?;
            Ok(AppEffect::WindowDock {
                target: string_arg(invocation, "target")?.to_owned(),
                edge: string_arg(invocation, "edge")?.to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "window.float",
        "Float an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
        |invocation| {
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "window float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::WindowFloat {
                target: string_arg(invocation, "target")?.to_owned(),
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })
        },
    )?;
    for (id, title, build) in [
        (
            "window.fullscreen",
            "Fullscreen an ordinary native window",
            AppEffect::WindowFullscreen as fn(String) -> AppEffect,
        ),
        (
            "window.hide",
            "Hide an ordinary native window",
            AppEffect::WindowHide,
        ),
        (
            "window.show",
            "Show an ordinary native window",
            AppEffect::WindowShow,
        ),
        (
            "window.close",
            "Close an ordinary native window",
            AppEffect::WindowClose,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default().field("target", ArgumentType::String, true),
            move |invocation| Ok(build(string_arg(invocation, "target")?.to_owned())),
        )?;
    }
    register(
        registry,
        "window.resize",
        "Resize an ordinary native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("amount", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowResize {
                target: string_arg(invocation, "target")?.to_owned(),
                amount: i16::try_from(integer_arg(invocation, "amount")?).map_err(|_| {
                    CommandError::InvalidArguments("window resize is outside i16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.resize.directional",
        "Resize the focused window in one direction",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::String, true),
        |invocation| {
            let direction = match string_arg(invocation, "direction")? {
                "left" => FocusDirection::Left,
                "down" => FocusDirection::Down,
                "up" => FocusDirection::Up,
                "right" => FocusDirection::Right,
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "window resize direction must be left, down, up, or right".into(),
                    ));
                }
            };
            Ok(AppEffect::WindowResizeDirectional(direction))
        },
    )?;
    register(
        registry,
        "window.move",
        "Move an ordinary floating native window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("target", ArgumentType::String, true)
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::WindowMove {
                target: string_arg(invocation, "target")?.to_owned(),
                x: u16::try_from(integer_arg(invocation, "x")?).map_err(|_| {
                    CommandError::InvalidArguments("window x is outside u16 bounds".into())
                })?,
                y: u16::try_from(integer_arg(invocation, "y")?).map_err(|_| {
                    CommandError::InvalidArguments("window y is outside u16 bounds".into())
                })?,
            })
        },
    )?;
    register(
        registry,
        "window.swap",
        "Swap two ordinary native window placements",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("first", ArgumentType::String, true)
            .field("second", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WindowSwap {
                first: string_arg(invocation, "first")?.to_owned(),
                second: string_arg(invocation, "second")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "terminal.open",
        "Open terminal profile",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("profile", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::TerminalOpen(
                optional_string_arg(invocation, "profile").map(str::to_owned),
            ))
        },
    )?;
    register(
        registry,
        "terminal.toggle",
        "Open or toggle focus for the bottom terminal",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalToggle,
    )?;
    register(
        registry,
        "terminal.list",
        "List terminals",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalList,
    )?;
    register(
        registry,
        "terminal.focus",
        "Focus terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            Ok(AppEffect::TerminalFocus(
                u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                    CommandError::InvalidArguments("terminal id must be non-negative".into())
                })?,
            ))
        },
    )?;
    register(
        registry,
        "terminal.focus_active",
        "Focus the visible terminal panel",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFocusActive,
    )?;
    register(
        registry,
        "terminal.cycle",
        "Cycle the selected terminal",
        ContextRequirement::Application,
        ArgumentSchema::default().field("direction", ArgumentType::Integer, true),
        |invocation| {
            let direction = i8::try_from(integer_arg(invocation, "direction")?).map_err(|_| {
                CommandError::InvalidArguments("terminal direction is outside i8 bounds".into())
            })?;
            Ok(AppEffect::TerminalCycle(direction))
        },
    )?;
    register(
        registry,
        "terminal.resize",
        "Resize the bottom terminal panel by rows",
        ContextRequirement::Application,
        ArgumentSchema::default().field("rows", ArgumentType::Integer, true),
        |invocation| {
            let rows = i16::try_from(integer_arg(invocation, "rows")?).map_err(|_| {
                CommandError::InvalidArguments("terminal resize is outside i16 bounds".into())
            })?;
            Ok(AppEffect::TerminalResize(rows))
        },
    )?;
    register(
        registry,
        "terminal.dock",
        "Dock the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("edge", ArgumentType::String, true)
            .field("size", ArgumentType::Integer, true),
        |invocation| {
            let size = u16::try_from(integer_arg(invocation, "size")?).map_err(|_| {
                CommandError::InvalidArguments("terminal dock size is outside u16 bounds".into())
            })?;
            Ok(AppEffect::TerminalDock {
                edge: string_arg(invocation, "edge")?.to_owned(),
                size,
            })
        },
    )?;
    register(
        registry,
        "terminal.float",
        "Float the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("x", ArgumentType::Integer, true)
            .field("y", ArgumentType::Integer, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
        |invocation| {
            let bounded = |name| {
                u16::try_from(integer_arg(invocation, name)?).map_err(|_| {
                    CommandError::InvalidArguments(format!(
                        "terminal float {name} is outside u16 bounds"
                    ))
                })
            };
            Ok(AppEffect::TerminalFloat {
                x: bounded("x")?,
                y: bounded("y")?,
                width: bounded("width")?,
                height: bounded("height")?,
            })
        },
    )?;
    register(
        registry,
        "terminal.fullscreen",
        "Fullscreen the selected terminal window",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalFullscreen,
    )?;
    register(
        registry,
        "terminal.hide",
        "Hide the selected terminal window without closing its process",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalHide,
    )?;
    register(
        registry,
        "terminal.send",
        "Send terminal text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalSend(
                string_arg(invocation, "text")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.mode.normal",
        "Enter terminal normal mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeNormal,
    )?;
    register(
        registry,
        "terminal.mode.input",
        "Return to terminal input mode",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalModeInput,
    )?;
    register(
        registry,
        "terminal.normal.move",
        "Move the terminal normal-mode cursor",
        ContextRequirement::Application,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TerminalNormalMove(
                string_arg(invocation, "motion")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "terminal.visual.start",
        "Start a terminal visual selection",
        ContextRequirement::Application,
        ArgumentSchema::default().field("linewise", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalVisualStart(
                optional_bool_arg(invocation, "linewise").unwrap_or(false),
            ))
        },
    )?;
    register(
        registry,
        "terminal.yank",
        "Yank terminal text into Dust registers",
        ContextRequirement::Application,
        ArgumentSchema::default().field("line", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::TerminalYank {
                line: optional_bool_arg(invocation, "line").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "terminal.unfocus",
        "Return focus from terminal to editor",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalUnfocus,
    )?;
    register(
        registry,
        "terminal.close",
        "Close focused terminal",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::TerminalClose,
    )?;
    register(
        registry,
        "format.run",
        "Run configured formatter",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::FormatRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.run",
        "Run configured linter",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "lint.clear",
        "Clear linter diagnostics",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::LintClear(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.run",
        "Run configured test",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestRun(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.stop",
        "Stop test run",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestStop(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "test.list",
        "List test results",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TestList(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "syntax.status",
        "Show built-in syntax language status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxStatus,
    )?;
    register(
        registry,
        "syntax.fold.toggle",
        "Toggle built-in syntax fold",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxFoldToggle,
    )?;
    register(
        registry,
        "syntax.indent.line",
        "Indent current line from built-in syntax structure",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::SyntaxIndentLine,
    )?;
    register(
        registry,
        "task.list",
        "List task status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::TaskList,
    )?;
    register(
        registry,
        "task.restart",
        "Restart task",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::TaskRestart(
                string_arg(invocation, "name")?.to_owned(),
            ))
        },
    )?;
    register(
        registry,
        "workspace.root.add",
        "Add workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("path", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceRootAdd(PathBuf::from(string_arg(
                invocation, "path",
            )?)))
        },
    )?;
    register(
        registry,
        "workspace.root.activate",
        "Activate workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootActivate(id))
        },
    )?;
    register(
        registry,
        "workspace.root.remove",
        "Remove workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("id", ArgumentType::Integer, true),
        |invocation| {
            let id = u64::try_from(integer_arg(invocation, "id")?).map_err(|_| {
                CommandError::InvalidArguments("root id must be non-negative".into())
            })?;
            Ok(AppEffect::WorkspaceRootRemove(id))
        },
    )?;
    register(
        registry,
        "workspace.root.status",
        "Show active workspace root",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceRootStatus,
    )?;
    register(
        registry,
        "workspace.search.prompt",
        "Open workspace search prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenPrompt {
            kind: PromptKind::Command,
            initial: "workspace.search ".into(),
        },
    )?;
    register(
        registry,
        "workspace.replace.prompt",
        "Open workspace replacement prompt",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenPrompt {
            kind: PromptKind::Command,
            initial: "workspace.replace ".into(),
        },
    )?;
    register(
        registry,
        "workspace.search",
        "Search workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceSearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "workspace.search.cancel",
        "Cancel workspace search",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::WorkspaceSearchCancel,
    )?;
    register(
        registry,
        "workspace.replace",
        "Replace across workspace",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("query", ArgumentType::String, true)
            .field("replacement", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::WorkspaceReplace {
                query: string_arg(invocation, "query")?.into(),
                replacement: string_arg(invocation, "replacement")?.into(),
            })
        },
    )?;
    for (id, title, kind) in [
        (
            "diagnostic.open",
            "Open workspace problems",
            PickerKind::Diagnostics,
        ),
        (
            "buffer.picker.open",
            "Open buffer picker",
            PickerKind::Buffers,
        ),
        (
            "command_palette.open",
            "Open command picker",
            PickerKind::Commands,
        ),
        (
            "workspace.root.list",
            "List workspace roots",
            PickerKind::Roots,
        ),
        (
            "navigation.recents",
            "Open recent files",
            PickerKind::Recents,
        ),
        ("navigation.marks", "Open marks", PickerKind::Marks),
        ("navigation.jumps", "Open jump list", PickerKind::Jumps),
        (
            "navigation.changes",
            "Open change list",
            PickerKind::Changes,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::OpenPicker(kind),
        )?;
    }
    for (id, title, effect) in [
        (
            "picker.backspace",
            "Delete picker query character",
            AppEffect::PickerBackspace,
        ),
        (
            "picker.next",
            "Select next picker item",
            AppEffect::PickerMove(1),
        ),
        (
            "picker.previous",
            "Select previous picker item",
            AppEffect::PickerMove(-1),
        ),
        (
            "picker.accept",
            "Accept picker item",
            AppEffect::PickerAccept,
        ),
        ("picker.cancel", "Close picker", AppEffect::PickerCancel),
        (
            "workspace.edit.toggle",
            "Toggle workspace edit file",
            AppEffect::WorkspaceEditToggle,
        ),
        (
            "workspace.edit.accept",
            "Accept workspace edit",
            AppEffect::WorkspaceEditAccept,
        ),
        (
            "workspace.resource.apply",
            "Apply staged resource operations",
            AppEffect::WorkspaceResourceApply,
        ),
        (
            "workspace.edit.reject",
            "Reject workspace edit",
            AppEffect::WorkspaceEditReject,
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
        "picker.insert",
        "Insert picker query text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PickerInsert(
                string_arg(invocation, "text")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "config.reload",
        "Reload configuration transactionally",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ReloadConfiguration,
    )?;
    register(
        registry,
        "config.explain",
        "Explain a resolved configuration value",
        ContextRequirement::Application,
        ArgumentSchema::default().field("key", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::ExplainConfig(
                string_arg(invocation, "key")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "keymap.inspect",
        "Inspect a resolved keymap binding",
        ContextRequirement::Application,
        ArgumentSchema::default()
            .field("id", ArgumentType::String, false)
            .field("context", ArgumentType::String, false)
            .field("key", ArgumentType::String, false)
            .field("command", ArgumentType::String, false),
        |invocation| AppEffect::InspectKeymap {
            id: optional_string_arg(invocation, "id").map(str::to_owned),
            context: optional_string_arg(invocation, "context").map(str::to_owned),
            key: optional_string_arg(invocation, "key").map(str::to_owned),
            command: optional_string_arg(invocation, "command").map(str::to_owned),
        },
    )?;
    register(
        registry,
        "keymap.conflicts",
        "Show keymap conflicts",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ShowKeymapConflicts,
    )?;
    for (id, title, effect) in [
        (
            "recovery.inspect",
            "Inspect recovery journals",
            AppEffect::RecoveryInspect,
        ),
        (
            "recovery.recover_latest",
            "Recover latest journal as scratch buffer",
            AppEffect::RecoveryRecoverLatest,
        ),
        (
            "recovery.discard_latest",
            "Discard latest recovery journal",
            AppEffect::RecoveryDiscardLatest,
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
        "navigation.mark.set",
        "Set navigation mark",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| Ok(AppEffect::MarkSet(string_arg(invocation, "name")?.into())),
    )?;
    register(
        registry,
        "application.safe_mode.status",
        "Show safe-mode status",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::SafeModeStatus,
    )?;
    register(
        registry,
        "workspace.git.status.refresh",
        "Refresh Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusRefresh,
    )?;
    register(
        registry,
        "workspace.git.hunk.reset",
        "Reset selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkReset,
    )?;
    register(
        registry,
        "workspace.git.hunk.stage",
        "Stage selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkStage,
    )?;
    register(
        registry,
        "workspace.git.hunks.open",
        "Open Git hunks",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunksOpen,
    )?;
    register(
        registry,
        "workspace.git.status.open",
        "Open Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusOpen,
    )?;
    register(
        registry,
        "health.open",
        "Show health",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ShowHealth,
    )?;
    register(
        registry,
        "workspace.file.find",
        "Open file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::OpenWorkspaceFilePicker,
    )?;
    register(
        registry,
        "command_line.open",
        "Open command line",
        ContextRequirement::Application,
        ArgumentSchema::default().field("kind", ArgumentType::String, true),
        |invocation| {
            let kind = match string_arg(invocation, "kind")? {
                "command" => PromptKind::Command,
                "search_forward" => PromptKind::SearchForward,
                "search_backward" => PromptKind::SearchBackward,
                value => {
                    return Err(CommandError::InvalidArguments(format!(
                        "unknown prompt kind `{value}`"
                    )));
                }
            };
            Ok(AppEffect::OpenPrompt {
                kind,
                initial: String::new(),
            })
        },
    )?;
    register(
        registry,
        "command_line.insert",
        "Insert prompt text",
        ContextRequirement::Application,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PromptInsert(
                string_arg(invocation, "text")?.into(),
            ))
        },
    )?;
    for (id, title, effect) in [
        (
            "command_line.backspace",
            "Prompt backspace",
            AppEffect::PromptBackspace,
        ),
        (
            "command_line.move_left",
            "Prompt left",
            AppEffect::PromptMove(PromptMove::Left),
        ),
        (
            "command_line.move_right",
            "Prompt right",
            AppEffect::PromptMove(PromptMove::Right),
        ),
        (
            "command_line.move_home",
            "Prompt home",
            AppEffect::PromptMove(PromptMove::Home),
        ),
        (
            "command_line.move_end",
            "Prompt end",
            AppEffect::PromptMove(PromptMove::End),
        ),
        (
            "command_line.history.previous",
            "Previous prompt history",
            AppEffect::PromptHistory(-1),
        ),
        (
            "command_line.history.next",
            "Next prompt history",
            AppEffect::PromptHistory(1),
        ),
        (
            "command_line.complete.next",
            "Next prompt completion",
            AppEffect::PromptComplete(1),
        ),
        (
            "command_line.complete.previous",
            "Previous prompt completion",
            AppEffect::PromptComplete(-1),
        ),
        (
            "command_line.submit",
            "Submit prompt",
            AppEffect::PromptSubmit,
        ),
        (
            "command_line.cancel",
            "Cancel prompt",
            AppEffect::PromptCancel,
        ),
    ] {
        let effect = effect.clone();
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| effect.clone(),
        )?;
    }
    register_grammar_commands(registry)?;
    register(
        registry,
        "input.mapping.cancel",
        "Cancel pending mapping",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CancelPendingMapping,
    )?;
    register(
        registry,
        "edit.repeat_last",
        "Repeat last change",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::RepeatLast,
    )?;
    register(
        registry,
        "macro.record.start",
        "Start macro recording",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroRecordStart(name))
        },
    )?;
    register(
        registry,
        "macro.record.stop",
        "Stop macro recording",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::MacroRecordStop,
    )?;
    register(
        registry,
        "macro.replay",
        "Replay macro",
        ContextRequirement::View,
        ArgumentSchema::default().field("register", ArgumentType::String, true),
        |invocation| {
            let name = one_character_arg(invocation, "register")?;
            Ok(AppEffect::MacroReplay {
                register: name,
                count: invocation.count.map_or(1, |count| count.get()),
            })
        },
    )?;
    register(
        registry,
        "view.scroll",
        "Scroll view",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("kind", ArgumentType::String, true)
            .field("at_mouse", ArgumentType::Bool, false),
        |invocation| {
            Ok(AppEffect::Scroll {
                kind: string_arg(invocation, "kind")?.into(),
                count: invocation.count.map_or(1, |count| count.get()),
                at_mouse: optional_bool_arg(invocation, "at_mouse").unwrap_or(false),
            })
        },
    )?;
    register(
        registry,
        "view.cursor.place_mouse",
        "Place cursor at mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::PlaceMouseCursor,
    )?;
    register(
        registry,
        "view.selection.extend_mouse",
        "Extend selection to mouse position",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::ExtendMouseSelection,
    )?;
    for (id, title, direction) in [
        (
            "surface.focus.left",
            "Focus surface to the left",
            FocusDirection::Left,
        ),
        (
            "surface.focus.down",
            "Focus surface below",
            FocusDirection::Down,
        ),
        (
            "surface.focus.up",
            "Focus surface above",
            FocusDirection::Up,
        ),
        (
            "surface.focus.right",
            "Focus surface to the right",
            FocusDirection::Right,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::SurfaceFocus(direction),
        )?;
    }
    for (id, title, effect) in [
        (
            "surface.retained.focus.next",
            "Focus next retained UI node",
            AppEffect::RetainedUiFocus(1),
        ),
        (
            "surface.retained.focus.previous",
            "Focus previous retained UI node",
            AppEffect::RetainedUiFocus(-1),
        ),
        (
            "surface.retained.scroll.down",
            "Scroll retained UI down",
            AppEffect::RetainedUiScroll(1),
        ),
        (
            "surface.retained.scroll.up",
            "Scroll retained UI up",
            AppEffect::RetainedUiScroll(-1),
        ),
        (
            "surface.retained.activate",
            "Activate focused retained UI node",
            AppEffect::RetainedUiActivate,
        ),
        (
            "surface.retained.mouse.select",
            "Select retained UI node at mouse position",
            AppEffect::RetainedUiMouseSelect,
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
    for (id, title, at_mouse) in [
        (
            "surface.private.activate",
            "Activate private surface action at cursor",
            false,
        ),
        (
            "surface.private.mouse.activate",
            "Activate private surface action at mouse",
            true,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::PrivateUiActivate { at_mouse },
        )?;
    }
    for (id, title, action) in [
        (
            "pane.split.horizontal",
            "Split pane horizontally",
            LayoutAction::Split(crate::editing::Axis::Horizontal),
        ),
        (
            "pane.split.vertical",
            "Split pane vertically",
            LayoutAction::Split(crate::editing::Axis::Vertical),
        ),
        ("pane.close", "Close pane", LayoutAction::ClosePane),
        (
            "pane.resize.grow",
            "Grow focused pane",
            LayoutAction::ResizePane(1),
        ),
        (
            "pane.resize.shrink",
            "Shrink focused pane",
            LayoutAction::ResizePane(-1),
        ),
        (
            "pane.focus.next",
            "Focus next pane",
            LayoutAction::FocusPane(1),
        ),
        (
            "pane.focus.previous",
            "Focus previous pane",
            LayoutAction::FocusPane(-1),
        ),
        ("tab.new", "Open new tab", LayoutAction::NewTab),
        ("tab.close", "Close tab", LayoutAction::CloseTab),
        ("tab.next", "Focus next tab", LayoutAction::FocusTab(1)),
        (
            "tab.previous",
            "Focus previous tab",
            LayoutAction::FocusTab(-1),
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::View,
            ArgumentSchema::default(),
            move |_| AppEffect::Layout(action),
        )?;
    }
    register(
        registry,
        "search.word",
        "Search word under cursor",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("forward", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchWord {
                forward: bool_arg(invocation, "forward")?,
            })
        },
    )?;
    register(
        registry,
        "search.repeat",
        "Repeat search",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("reverse", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::SearchRepeat {
                reverse: bool_arg(invocation, "reverse")?,
            })
        },
    )?;
    Ok(())
}

fn register_grammar_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, input) in [
        ("input.cancel", "Cancel focused input", GrammarInput::Cancel),
        ("input.zero", "Count zero or line start", GrammarInput::Zero),
        (
            "input.register.begin",
            "Select register",
            GrammarInput::RegisterBegin,
        ),
        (
            "input.prefix.begin",
            "Begin command prefix",
            GrammarInput::PrefixBegin,
        ),
        (
            "input.replace.begin",
            "Replace character",
            GrammarInput::ReplaceBegin,
        ),
        (
            "input.macro.record.toggle",
            "Toggle macro recording",
            GrammarInput::MacroRecordToggle,
        ),
        (
            "input.macro.replay.begin",
            "Select macro for replay",
            GrammarInput::MacroReplayBegin,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default(),
            move |_| AppEffect::Grammar(input.clone()),
        )?;
    }
    register(
        registry,
        "input.count",
        "Append count digit",
        ContextRequirement::Application,
        ArgumentSchema::default().field("digit", ArgumentType::Integer, true),
        |invocation| {
            let digit = u8::try_from(integer_arg(invocation, "digit")?).map_err(|_| {
                CommandError::InvalidArguments("digit must be between 0 and 9".into())
            })?;
            Ok(AppEffect::Grammar(GrammarInput::Count(digit)))
        },
    )?;
    for (id, title, constructor) in [
        (
            "input.register.select",
            "Use register",
            GrammarInput::RegisterSelect as fn(String) -> GrammarInput,
        ),
        (
            "input.replace.text",
            "Replace with text",
            GrammarInput::ReplaceText,
        ),
        (
            "input.macro.record.select",
            "Record macro register",
            GrammarInput::MacroRecordSelect,
        ),
        (
            "input.macro.replay.select",
            "Replay macro register",
            GrammarInput::MacroReplaySelect,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default().field("text", ArgumentType::String, true),
            move |invocation| {
                Ok(AppEffect::Grammar(constructor(
                    string_arg(invocation, "text")?.into(),
                )))
            },
        )?;
    }
    register(
        registry,
        "input.operator.begin",
        "Begin operator",
        ContextRequirement::View,
        ArgumentSchema::default().field("operator", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::Grammar(GrammarInput::OperatorBegin(
                string_arg(invocation, "operator")?.into(),
            )))
        },
    )?;
    register(
        registry,
        "input.operator.repeat",
        "Repeat pending operator linewise",
        ContextRequirement::View,
        ArgumentSchema::default().field("operator", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::Grammar(GrammarInput::OperatorRepeat(
                string_arg(invocation, "operator")?.into(),
            )))
        },
    )?;
    register(
        registry,
        "input.motion",
        "Resolve configured motion",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("motion", ArgumentType::String, true)
            .field("linewise", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Grammar(GrammarInput::Motion {
                motion: string_arg(invocation, "motion")?.into(),
                linewise: bool_arg(invocation, "linewise")?,
            }))
        },
    )?;
    register(
        registry,
        "input.visual.operator",
        "Apply configured Visual operator",
        ContextRequirement::Selection,
        ArgumentSchema::default()
            .field("operator", ArgumentType::String, true)
            .field("linewise", ArgumentType::Bool, true),
        |invocation| {
            Ok(AppEffect::Grammar(GrammarInput::VisualOperator {
                operator: string_arg(invocation, "operator")?.into(),
                linewise: bool_arg(invocation, "linewise")?,
            }))
        },
    )?;
    Ok(())
}

fn register<F, R>(
    registry: &mut CommandRegistry<AppEffect>,
    id: &str,
    title: &str,
    context: ContextRequirement,
    arguments: ArgumentSchema,
    effect: F,
) -> Result<(), CommandError>
where
    F: Fn(&crate::editing::CommandInvocation) -> R + Send + Sync + 'static,
    R: IntoEffectResult,
{
    let mut spec = CommandSpec::builtin(id, title)?;
    spec.context = context;
    spec.arguments = arguments;
    registry.register(spec, move |_, invocation| {
        let effect = effect(invocation).into_effect_result()?;
        let mut outcome = CommandOutcome::empty();
        outcome.effects.push(effect);
        Ok(outcome)
    })
}

trait IntoEffectResult {
    fn into_effect_result(self) -> Result<AppEffect, CommandError>;
}

impl IntoEffectResult for AppEffect {
    fn into_effect_result(self) -> Result<AppEffect, CommandError> {
        Ok(self)
    }
}

impl IntoEffectResult for Result<AppEffect, CommandError> {
    fn into_effect_result(self) -> Result<AppEffect, CommandError> {
        self
    }
}

fn string_arg<'a>(
    invocation: &'a crate::editing::CommandInvocation,
    name: &str,
) -> Result<&'a str, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_str()
}

fn optional_string_arg<'a>(
    invocation: &'a crate::editing::CommandInvocation,
    name: &str,
) -> Option<&'a str> {
    invocation
        .arguments
        .get(name)
        .and_then(|value| match value {
            CommandValue::String(value) => Some(value.as_str()),
            _ => None,
        })
}

fn optional_bool_arg(invocation: &crate::editing::CommandInvocation, name: &str) -> Option<bool> {
    invocation
        .arguments
        .get(name)
        .and_then(|value| match value {
            CommandValue::Bool(value) => Some(*value),
            _ => None,
        })
}

fn one_character_arg(
    invocation: &crate::editing::CommandInvocation,
    name: &str,
) -> Result<char, CommandError> {
    let value = string_arg(invocation, name)?;
    let mut characters = value.chars();
    let character = characters
        .next()
        .ok_or_else(|| CommandError::InvalidArguments(format!("`{name}` must be one character")))?;
    if characters.next().is_some() {
        return Err(CommandError::InvalidArguments(format!(
            "`{name}` must be one character"
        )));
    }
    Ok(character)
}

fn integer_arg(
    invocation: &crate::editing::CommandInvocation,
    name: &str,
) -> Result<i64, CommandError> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Integer(value)) => Ok(*value),
        _ => Err(CommandError::InvalidArguments(format!(
            "missing integer `{name}`"
        ))),
    }
}

fn bool_arg(
    invocation: &crate::editing::CommandInvocation,
    name: &str,
) -> Result<bool, CommandError> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Bool(value)) => Ok(*value),
        _ => Err(CommandError::InvalidArguments(format!(
            "missing boolean `{name}`"
        ))),
    }
}
